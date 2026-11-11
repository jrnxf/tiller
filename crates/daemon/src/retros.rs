//! The board's retros (`learning-loop.md` section 4): the list Memory and
//! Health show, and Run again on a retro that did not finish. Run again is
//! `tiller retro run` started by the board: it claims the retro by the
//! same rules and runs the curator on a thread of the daemon's. The daemon
//! stops a running curator, and everything it started, when it stops; the
//! retro then ends `unfinished` and can run again.
//!
//! Run again is refused while another retro of the same card runs, so one
//! card has one curator at a time. It is never forwarded: a retro is run
//! on the host that keeps it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use tiller_core::domain::{Retro, RetroOutcome, RetroState};
use tiller_core::state::learning::{self, Learning};
use tiller_core::state::{get, list};
use tiller_core::{Error, Store, Timestamp};
use utoipa::{IntoParams, ToSchema};

use crate::api::{
    ApiError, ApiResult, AppState, board_ctx, body, guard, key, not_forwarded, read, request_args,
    start_refusal, uninitialized, writable,
};
use crate::dto::{ErrorBody, MutationResponse, Receipt};

/// How far back the list reaches. An unfinished retro shows however old it
/// is: the sweep never runs it again.
const WINDOW_DAYS: i64 = 30;

/// The most rows one read returns besides the unfinished retros, which
/// are all listed.
const MOST: i64 = 200;

/// `POST /api/v1/retros/{id}/run`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetroRunStart {
    pub idempotency_key: String,
    /// Refused unless absent or this host: a retro run is never forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RetrosQuery {
    /// Only this project's retros (its id).
    pub project: Option<String>,
}

/// Where a full retro is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RetroStatus {
    /// Its curator started less than the timeout ago and has not finished.
    Running,
    /// Its curator ended without finishing it; the sweep does not run it
    /// again, the owner does.
    Unfinished,
    /// Finished: nothing, a learning, or a proposal.
    Done,
    /// Scheduled, and no curator holds it.
    Pending,
}

/// The proposal a retro filed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RetroProposal {
    pub id: String,
    /// `proposed` (waiting for review), `accepted`, `applied`, `discarded`,
    /// `stale` or `failed`.
    pub state: String,
    /// The document section it changes, when it names one.
    pub section: Option<String>,
}

/// One full retro, as Memory's Retros list and Health's Learning panel
/// show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RetroView {
    pub id: String,
    pub card_id: String,
    /// The card's display id, `alpha-14`.
    pub card: String,
    pub title: String,
    pub project_id: String,
    pub status: RetroStatus,
    /// `nothing`, `evidence`, `proposed` or `unfinished`, once done.
    pub outcome: Option<String>,
    /// The learning it kept, or for an unfinished retro why it did not
    /// finish.
    pub learning: Option<String>,
    pub created_at: String,
    /// When its newest curator run started.
    pub curator_started_at: Option<String>,
    /// Who started that run: `owner`, `board` or `daemon`.
    pub started_by: Option<String>,
    pub finished_at: Option<String>,
    pub proposal: Option<RetroProposal>,
    /// Why Run again is refused now, in plain words; `None` when it would
    /// start.
    pub run_refusal: Option<String>,
}

/// `GET /api/v1/retros`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RetrosResponse {
    /// Full retros of the last 30 days and every unfinished one, newest
    /// first.
    pub retros: Vec<RetroView>,
    /// `learning.curator_timeout_minutes`: a run is cut at this limit.
    pub curator_timeout_minutes: u64,
}

#[derive(Default)]
struct Shared {
    /// Held while a Run again claims, so two at once cannot both pass the
    /// same-card check.
    claims: Mutex<()>,
    /// The threads of the curator runs not yet seen to end. A retry of a
    /// Run again is answered from the key its claim stored, not from here.
    runs: Mutex<Vec<JoinHandle<()>>>,
    /// Set when the daemon stops: every running curator is stopped.
    stop: Arc<AtomicBool>,
}

/// The curator runs the board started.
#[derive(Clone, Default)]
pub struct Retros(Arc<Shared>);

impl Retros {
    fn runs(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.0.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn claiming(&self) -> MutexGuard<'_, ()> {
        self.0.claims.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `run` on a thread of its own, to be stopped with the daemon.
    fn start(&self, retro: &str, run: crate::api::CuratorRun) -> std::io::Result<()> {
        let stop = Arc::clone(&self.0.stop);
        let id = retro.to_owned();
        let handle = std::thread::Builder::new()
            .name("retro-curator".into())
            .spawn(move || match (run.run)(stop) {
                Ok(retro) if retro.outcome == Some(RetroOutcome::Unfinished) => {
                    tracing::warn!(retro = %id, learning = ?retro.learning, "the curator did not finish the retro");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(retro = %id, %error, "the curator run failed"),
            })?;
        let mut runs = self.runs();
        // A finished run is dropped; a running one is always kept, so the
        // daemon can stop it.
        runs.retain(|run| !run.is_finished());
        runs.push(handle);
        Ok(())
    }

    /// Whether the daemon is stopping: a claim then starts nothing.
    fn stopping(&self) -> bool {
        self.0.stop.load(Ordering::SeqCst)
    }

    /// Stops every running curator and waits up to `wait` for each run to
    /// record how it ended.
    #[cfg(test)]
    pub fn stop(&self, wait: Duration) {
        self.signal();
        self.wait(Instant::now() + wait);
    }

    /// Raises the stop flag, which stops every running curator. It is set
    /// under the claims lock, so a Run again still claiming has started its
    /// run before [`Retros::wait`] takes the runs, and one that claims later
    /// is refused.
    pub fn signal(&self) {
        let _claiming = self.claiming();
        self.0.stop.store(true, Ordering::SeqCst);
    }

    /// Waits until `deadline` for each run [`Retros::signal`] stopped to
    /// record how it ended.
    pub fn wait(&self, deadline: Instant) {
        let handles: Vec<JoinHandle<()>> = self.runs().drain(..).collect();
        for handle in handles {
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

/// `[learning]` from `config.toml`, read on every call so an edit needs no
/// restart, through the loader the claim uses. A file that cannot be read
/// gives the defaults and why: the claim refuses it with the same error.
fn learning_of(home: &std::path::Path) -> (Learning, Option<String>) {
    let read = tiller_core::config_file::load(&home.join("config.toml"))
        .map_err(|error| error.to_string())
        .and_then(|table| Learning::from_table(&table).map_err(|error| error.to_string()));
    match read {
        Ok(learning) => (learning, None),
        Err(error) => (
            Learning::default(),
            Some(format!("config.toml cannot be used: {error}")),
        ),
    }
}

fn status(retro: &Retro, now: Timestamp, timeout: i64) -> RetroStatus {
    match (retro.state, retro.outcome) {
        (RetroState::Done, Some(RetroOutcome::Unfinished)) => RetroStatus::Unfinished,
        (RetroState::Done, _) => RetroStatus::Done,
        (RetroState::Pending, _) => match retro.curator_started_at {
            Some(started) if started.plus_minutes(timeout) > now => RetroStatus::Running,
            _ => RetroStatus::Pending,
        },
    }
}

/// The full retros of `card` whose curator runs now, other than `except`.
fn running_on_card(
    store: &Store,
    card: &str,
    except: &str,
    now: Timestamp,
    timeout: i64,
) -> tiller_core::Result<Option<Retro>> {
    let retros: Vec<Retro> = list(
        store.conn(),
        "card_id = ?1 AND id <> ?2 AND state = 'pending' AND curator_started_at IS NOT NULL",
        &[&card, &except],
    )?;
    Ok(retros
        .into_iter()
        .find(|retro| status(retro, now, timeout) == RetroStatus::Running))
}

/// Why Run again would be refused on `retro` now, by the rules the claim
/// applies.
fn refusal(
    store: &Store,
    retro: &Retro,
    card: &str,
    now: Timestamp,
    timeout: i64,
) -> tiller_core::Result<Option<String>> {
    if let Err(error) = learning::check_start(retro, now, timeout) {
        return Ok(Some(match error {
            Error::Refused { fact, .. } => fact,
            other => other.to_string(),
        }));
    }
    Ok(
        running_on_card(store, &retro.card_id, &retro.id, now, timeout)?
            .map(|other| busy(card, &other)),
    )
}

fn busy(card: &str, other: &Retro) -> String {
    format!(
        "another retro of {card} is running (retro {}); one card has one curator at a time",
        tiller_core::ids::short(&other.id)
    )
}

fn proposal_of(store: &Store, id: &str) -> tiller_core::Result<Option<RetroProposal>> {
    let row = store.conn().query_row(
        "SELECT state, json_extract(memory_fields, '$.section') FROM proposals WHERE id = ?1",
        [id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    );
    match row {
        Ok((state, section)) => Ok(Some(RetroProposal {
            id: id.to_owned(),
            state,
            section,
        })),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn started_by(store: &Store, retro: &str) -> tiller_core::Result<Option<String>> {
    let actor = store.conn().query_row(
        "SELECT actor FROM events WHERE entity = 'retro' AND entity_id = ?1 AND kind = 'retro.started'
         ORDER BY seq DESC LIMIT 1",
        [retro],
        |row| row.get::<_, String>(0),
    );
    match actor {
        Ok(actor) => Ok(Some(actor)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn view(
    store: &Store,
    retro: Retro,
    now: Timestamp,
    timeout: i64,
) -> tiller_core::Result<RetroView> {
    let card: tiller_core::domain::Card = get(store.conn(), "card", &retro.card_id)?;
    let run_refusal = refusal(store, &retro, &card.display_id, now, timeout)?;
    let proposal = match &retro.proposal_id {
        Some(id) => proposal_of(store, id)?,
        None => None,
    };
    Ok(RetroView {
        status: status(&retro, now, timeout),
        started_by: started_by(store, &retro.id)?,
        card: card.display_id,
        title: card.title,
        outcome: retro.outcome.map(|outcome| outcome.as_str().to_owned()),
        learning: retro.learning,
        created_at: retro.created_at.to_string(),
        curator_started_at: retro.curator_started_at.map(|at| at.to_string()),
        finished_at: retro.finished_at.map(|at| at.to_string()),
        proposal,
        run_refusal,
        id: retro.id,
        card_id: retro.card_id,
        project_id: retro.project_id,
    })
}

fn views(
    store: &Store,
    project: Option<&str>,
    now: Timestamp,
    timeout: i64,
) -> tiller_core::Result<Vec<RetroView>> {
    let since = now.plus_minutes(-WINDOW_DAYS * 24 * 60);
    // Every unfinished retro is listed; only the others are capped, so an
    // old unfinished one is never cut for sorting last.
    let newest = "max(created_at, coalesce(finished_at, ''), coalesce(curator_started_at, '')) DESC, id DESC";
    let retros: Vec<Retro> = list(
        store.conn(),
        &format!(
            "depth = 'full' AND (?1 IS NULL OR project_id = ?1)
             AND (outcome = 'unfinished' OR id IN (
                 SELECT id FROM retros WHERE depth = 'full' AND (?1 IS NULL OR project_id = ?1)
                     AND outcome IS NOT 'unfinished'
                     AND (created_at >= ?2 OR finished_at >= ?2 OR curator_started_at >= ?2)
                 ORDER BY {newest} LIMIT {MOST}))
             ORDER BY {newest}"
        ),
        &[&project, &since],
    )?;
    retros
        .into_iter()
        .map(|retro| view(store, retro, now, timeout))
        .collect()
}

#[utoipa::path(get, path = "/api/v1/retros", tag = "reads", params(RetrosQuery),
    responses((status = 200, body = RetrosResponse, description = "Full retros of the last 30 days and every unfinished one, newest first")))]
pub(crate) async fn list_retros(
    State(state): State<AppState>,
    Query(query): Query<RetrosQuery>,
) -> ApiResult<RetrosResponse> {
    let home = state.0.home.clone();
    let (retros, minutes) = read(&state, move |store| {
        let (learning, config_error) = learning_of(&home);
        let minutes = learning.curator_timeout_minutes;
        let timeout = i64::try_from(minutes).unwrap_or(i64::MAX);
        let mut retros = views(store, query.project.as_deref(), Timestamp::now(), timeout)?;
        if let Some(error) = config_error {
            // Run again would be refused for it, so it is off with why.
            for retro in &mut retros {
                retro.run_refusal.get_or_insert_with(|| error.clone());
            }
        }
        Ok((retros, minutes))
    })
    .await?;
    Ok(Json(RetrosResponse {
        retros,
        curator_timeout_minutes: minutes,
    }))
}

#[utoipa::path(post, path = "/api/v1/retros/{id}/run", tag = "mutations", request_body = RetroRunStart,
    params(("id" = String, Path, description = "A retro id, or a card reference for its newest retro")),
    responses((status = 200, body = MutationResponse, description = "The retro is claimed and its curator runs; `data` is the claimed retro, and `GET /api/v1/retros` shows how the run ends"),
              (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "done with an outcome other than unfinished, its curator still runs, or another retro of the same card runs"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "this daemon cannot run retros, or it is stopping")))]
pub(crate) async fn run_retro(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: RetroRunStart = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a retro run").await?;
    let key = key(&request.idempotency_key)?;
    writable(&state)?;
    let Some(starter) = state.0.orchestrators.clone() else {
        return Err(Error::Unavailable("this daemon cannot run retros".into()).into());
    };
    let home = state.0.home.clone();
    let retros = state.0.retros.clone();
    let db = state.db();
    let damage = state.0.damage.clone();
    let args = request_args();
    let joined = tokio::task::spawn_blocking(move || -> Result<MutationResponse, ApiError> {
        if !db.exists() {
            return Err(uninitialized());
        }
        // A config that cannot be used is refused by the claim below. It is
        // read before the claims lock, so a stop does not wait on it.
        let timeout =
            i64::try_from(learning_of(&home).0.curator_timeout_minutes).unwrap_or(i64::MAX);
        let _claiming = retros.claiming();
        let mut store = Store::open_current(&db).map_err(ApiError::from)?;
        let ctx = board_ctx(&key, args.as_ref());
        // A retry of a Run again answers with the retro its key claimed and
        // starts nothing, even after a restart or while the daemon stops:
        // the claim stored the key. A key stored for another write is
        // refused by the same call.
        if store.key_recorded(&ctx)? {
            let claimed = store.retro_start(&ctx, &id, None, timeout)?;
            let retro: Retro = get(store.conn(), "retro", &claimed.value.id)?;
            return Ok(response(&retro, Vec::new(), true)?);
        }
        if retros.stopping() {
            return Err(Error::Unavailable("the daemon is stopping".into()).into());
        }
        let retro = learning::resolve_retro(store.conn(), &id)?;
        let card: tiller_core::domain::Card = get(store.conn(), "card", &retro.card_id)?;
        if let Some(other) =
            running_on_card(&store, &retro.card_id, &retro.id, Timestamp::now(), timeout)?
        {
            return Err(Error::Refused {
                entity: "retro",
                id: retro.id.clone(),
                fact: busy(&card.display_id, &other),
            }
            .into());
        }
        let run = starter
            .retro_run(&mut store, &ctx, &retro.id)
            .map_err(|error| start_refusal(error, |error| damage.suspect(&db, error)))?;
        let claimed = run.claimed.clone();
        if let Err(error) = retros.start(&claimed.value.id, run) {
            // The claim stands with no curator behind it: end it so it can
            // run again at once.
            let note = format!("the curator could not be started: {error}");
            let _ = store.retro_finish(
                &tiller_core::Ctx::new(tiller_core::Actor::Daemon),
                None,
                &claimed.value.id,
                learning::Finish {
                    outcome: RetroOutcome::Unfinished,
                    learning: Some(note.clone()),
                    evidence: None,
                    proposal: None,
                },
            );
            return Err(Error::Internal(note).into());
        }
        Ok(response(&claimed.value, claimed.events, false)?)
    })
    .await;
    match joined {
        Ok(result) => Ok(Json(result?)),
        Err(error) => Err(Error::Internal(error.to_string()).into()),
    }
}

fn response(
    retro: &Retro,
    events: Vec<i64>,
    replayed: bool,
) -> tiller_core::Result<MutationResponse> {
    Ok(MutationResponse {
        ok: true,
        replayed,
        events,
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: None,
            approval_id: None,
        },
        data: serde_json::to_value(retro)?,
        host: None,
        warning: None,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;

    use axum::http::StatusCode;
    use serde_json::{Value, json};
    use tiller_core::{Actor, Applied, Ctx};
    use tiller_herdr::testing::Fleet;

    use super::*;
    use crate::api::tests::{
        Fixture, app_restarting, app_with, get as get_at, mutation, owner_cookie, send,
    };
    use crate::api::{CuratorRun, OrchestratorStart, StartError};
    use crate::assets::Assets;
    use crate::readonly::Damage;

    const R1: &str = "01JC000000000000000000RT01";
    const R2: &str = "01JC000000000000000000RT02";

    /// How a stand-in curator run ends.
    enum Ending {
        /// At once, `unfinished` with this note, as a curator that exits.
        Exits(&'static str),
        /// When the daemon stops it.
        UntilStopped,
        /// Only once the test sets this flag, stop or not, as a curator
        /// slow to end.
        Holds(Arc<AtomicBool>),
    }

    /// Claims as `tiller retro run` does and plays the next ending; no
    /// curator is launched.
    struct FakeCurator {
        db: PathBuf,
        endings: Mutex<VecDeque<Ending>>,
    }

    impl OrchestratorStart for FakeCurator {
        fn start(
            &self,
            _store: &mut Store,
            _ctx: &Ctx,
            _project: &str,
            _restart: bool,
        ) -> Result<Applied<tiller_agents::spawn::orchestrator::Orchestrator>, StartError> {
            unreachable!("a retro run starts no orchestrator")
        }

        fn retro_run(
            &self,
            store: &mut Store,
            ctx: &Ctx,
            retro: &str,
        ) -> Result<CuratorRun, StartError> {
            let claimed = store
                .retro_start(ctx, retro, Some("curator-session"), 20)
                .map_err(|error| StartError::State {
                    error: Box::new(error),
                    notes: None,
                })?;
            let ending = self.endings.lock().unwrap().pop_front().unwrap();
            let db = self.db.clone();
            let id = claimed.value.id.clone();
            Ok(CuratorRun {
                claimed,
                run: Box::new(move |stop| {
                    let note = match ending {
                        Ending::Exits(note) => note.to_owned(),
                        Ending::UntilStopped => {
                            while !stop.load(Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            "curator stopped: the daemon stopped before the retro was finished"
                                .to_owned()
                        }
                        Ending::Holds(released) => {
                            while !released.load(Ordering::SeqCst) {
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            "curator released".to_owned()
                        }
                    };
                    let mut store = Store::open_current(&db).map_err(|e| e.to_string())?;
                    store
                        .retro_finish(
                            &Ctx::new(Actor::Daemon),
                            None,
                            &id,
                            learning::Finish {
                                outcome: RetroOutcome::Unfinished,
                                learning: Some(note),
                                evidence: None,
                                proposal: None,
                            },
                        )
                        .map(|applied| applied.value)
                        .map_err(|e| e.to_string())
                }),
            })
        }
    }

    /// A fleet with closed card alpha-44 and two full retros of it: R1
    /// ended unfinished, R2 is pending.
    fn fleet() -> Fleet {
        let fleet = Fleet::new();
        let now = Timestamp::now();
        fleet
            .store
            .conn()
            .execute_batch(&format!(
                "INSERT INTO cards (id, display_id, project_id, kind, title, state, size, outcome, created_at, closed_at)
                 VALUES ('c44', 'alpha-44', 'alpha', 'ship', 'Undo strokes', 'closed', 'M', 'failed', '{now}', '{now}');
                 INSERT INTO retros (id, card_id, project_id, closed_at, depth, reason, state, outcome, learning,
                                     curator_session, curator_started_at, finished_at, finished_by, created_at)
                 VALUES ('{R1}', 'c44', 'alpha', '2026-11-01T11:00:00.000Z', 'full', 'incident', 'done', 'unfinished',
                         'curator cut after 20 minutes without finishing the retro', 's0', '{now}', '{now}', 'daemon', '{now}');
                 INSERT INTO retros (id, card_id, project_id, closed_at, depth, reason, state, created_at)
                 VALUES ('{R2}', 'c44', 'alpha', '2026-11-02T11:00:00.000Z', 'full', 'incident', 'pending', '{now}');"
            ))
            .unwrap();
        fleet
    }

    fn app(fleet: &Fleet, endings: Vec<Ending>) -> axum::Router {
        app_restarting(
            fleet.dir.path(),
            Arc::new(FakeCurator {
                db: fleet.db.clone(),
                endings: Mutex::new(endings.into()),
            }),
        )
    }

    async fn run(app: &axum::Router, cookie: &str, retro: &str, key: &str) -> (StatusCode, Value) {
        let answer = send(
            app,
            mutation(
                "POST",
                &format!("/api/v1/retros/{retro}/run"),
                json!({"idempotency_key": key}),
                &[("cookie", cookie)],
            ),
        )
        .await;
        (answer.status, serde_json::from_str(&answer.body).unwrap())
    }

    async fn retros(app: &axum::Router) -> RetrosResponse {
        let answer = get_at(app, "/api/v1/retros?project=alpha").await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        serde_json::from_str(&answer.body).unwrap()
    }

    async fn one(app: &axum::Router, id: &str) -> RetroView {
        retros(app)
            .await
            .retros
            .into_iter()
            .find(|retro| retro.id == id)
            .unwrap()
    }

    /// Waits for the run of `id` to end.
    async fn settled(app: &axum::Router, id: &str) -> RetroView {
        for _ in 0..250 {
            let retro = one(app, id).await;
            if retro.status != RetroStatus::Running {
                return retro;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the run of {id} never ended");
    }

    #[tokio::test]
    async fn the_list_says_what_each_retro_is_and_whether_it_can_run_again() {
        let fleet = fleet();
        let app = app(&fleet, Vec::new());
        let list = retros(&app).await;
        assert_eq!(list.curator_timeout_minutes, 20);
        let r1 = list.retros.iter().find(|r| r.id == R1).unwrap();
        assert_eq!(r1.status, RetroStatus::Unfinished);
        assert_eq!(r1.card, "alpha-44");
        assert_eq!(r1.title, "Undo strokes");
        assert_eq!(
            r1.learning.as_deref(),
            Some("curator cut after 20 minutes without finishing the retro")
        );
        assert_eq!(r1.run_refusal, None);
        let r2 = list.retros.iter().find(|r| r.id == R2).unwrap();
        assert_eq!(r2.status, RetroStatus::Pending);
        // Another project's filter leaves both out.
        let other = get_at(&app, "/api/v1/retros?project=beta").await;
        assert_eq!(other.status, StatusCode::OK);
        assert!(other.body.contains("\"retros\":[]"), "{}", other.body);
    }

    /// A config.toml that cannot be used turns Run again off, with why: the
    /// claim would refuse it.
    #[tokio::test]
    async fn a_config_that_cannot_be_used_turns_run_again_off() {
        let fleet = fleet();
        std::fs::write(
            fleet.dir.path().join("config.toml"),
            "[learning]\ncurator_timeout_minutes = \"soon\"\n",
        )
        .unwrap();
        let r1 = one(&app(&fleet, Vec::new()), R1).await;
        assert_eq!(r1.status, RetroStatus::Unfinished);
        let refusal = r1.run_refusal.unwrap_or_default();
        assert!(
            refusal.starts_with("config.toml cannot be used: "),
            "{refusal}"
        );
    }

    /// The list has every unfinished retro, however many newer ones there
    /// are (`protocol.md`, `GET /api/v1/retros`).
    #[tokio::test]
    async fn an_old_unfinished_retro_is_listed_past_the_cap() {
        let fleet = fleet();
        let now = Timestamp::now();
        let old = now.plus_minutes(-60 * 24 * 60);
        fleet
            .store
            .conn()
            .execute(
                &format!("UPDATE retros SET created_at = '{old}', finished_at = '{old}', curator_started_at = '{old}' WHERE id = '{R1}'"),
                [],
            )
            .unwrap();
        for n in 0..MOST {
            // One retro per card close.
            let closed = now.plus_minutes(-(n + 1));
            fleet
                .store
                .conn()
                .execute(
                    &format!(
                        "INSERT INTO retros (id, card_id, project_id, closed_at, depth, reason, state, outcome,
                                             finished_at, finished_by, created_at)
                         VALUES ('01JC0000000000000000D{n:05}', 'c44', 'alpha', '{closed}', 'full', 'incident',
                                 'done', 'nothing', '{now}', 'curator', '{now}')"
                    ),
                    [],
                )
                .unwrap();
        }
        let list = retros(&app(&fleet, Vec::new())).await;
        // The cap still holds for the others: R2 and the newer done ones.
        assert_eq!(list.retros.len(), usize::try_from(MOST).unwrap() + 1);
        assert_eq!(list.retros.last().map(|retro| retro.id.as_str()), Some(R1));
    }

    #[tokio::test]
    async fn run_again_shows_the_new_reason_and_does_not_loop() {
        let fleet = fleet();
        let cookie = owner_cookie(fleet.dir.path());
        let app = app(
            &fleet,
            vec![Ending::Exits(
                "curator exited 3 without finishing the retro",
            )],
        );
        let (status, body) = run(&app, &cookie, R1, "again-1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["replayed"], false);
        assert_eq!(body["data"]["state"], "pending");
        assert!(!body["events"].as_array().unwrap().is_empty());
        let after = settled(&app, R1).await;
        assert_eq!(after.status, RetroStatus::Unfinished);
        assert_eq!(
            after.learning.as_deref(),
            Some("curator exited 3 without finishing the retro")
        );
        assert_eq!(after.started_by.as_deref(), Some("board"));
        // It stays unfinished: nothing runs it again by itself.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let later = one(&app, R1).await;
        assert_eq!(later.status, RetroStatus::Unfinished);
        assert_eq!(later.curator_started_at, after.curator_started_at);
        // A retry of the same request is a replay, not a second run: the
        // stand-in has no second ending to play.
        let (status, body) = run(&app, &cookie, R1, "again-1").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["replayed"], true);
        assert_eq!(body["events"], json!([]));
    }

    #[tokio::test]
    async fn run_again_is_refused_while_a_retro_of_the_same_card_runs() {
        let fleet = fleet();
        let cookie = owner_cookie(fleet.dir.path());
        let app = app(&fleet, vec![Ending::UntilStopped]);
        let (status, body) = run(&app, &cookie, R2, "start-r2").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let r1 = one(&app, R1).await;
        let refusal = r1.run_refusal.unwrap();
        assert!(
            refusal.contains("another retro of alpha-44 is running"),
            "{refusal}"
        );
        let (status, body) = run(&app, &cookie, R1, "again-r1").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("another retro of alpha-44 is running"),
            "{body}"
        );
        // The running one itself is refused by the claim's own rule.
        let r2 = one(&app, R2).await;
        assert_eq!(r2.status, RetroStatus::Running);
        assert!(r2.run_refusal.is_some());
        let (status, _) = run(&app, &cookie, R2, "again-r2").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn run_again_needs_the_owner_and_a_daemon_that_runs_retros() {
        let fleet = fleet();
        let app = app(&fleet, Vec::new());
        let (status, _) = run(&app, "tiller_owner=wrong", R1, "k").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let cookie = owner_cookie(fleet.dir.path());
        let plain = app_with(fleet.dir.path(), Assets::of::<Fixture>(), Damage::default());
        let (status, body) = run(&plain, &cookie, R1, "k").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["error"]["code"], "unavailable");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .ends_with("this daemon cannot run retros")
        );
        // Unknown fields are refused, as on every mutation.
        let answer = send(
            &app,
            mutation(
                "POST",
                &format!("/api/v1/retros/{R1}/run"),
                json!({"idempotency_key": "k", "loop": true}),
                &[("cookie", &cookie)],
            ),
        )
        .await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
    }

    #[test]
    fn stopping_the_daemon_stops_its_curator_runs() {
        let fleet = fleet();
        let mut store = fleet.open();
        let curator = FakeCurator {
            db: fleet.db.clone(),
            endings: Mutex::new(vec![Ending::UntilStopped].into()),
        };
        let Ok(run) = curator.retro_run(&mut store, &Ctx::new(Actor::Board), R1) else {
            panic!("the claim is refused");
        };
        let retros = Retros::default();
        retros.start(R1, run).unwrap();
        retros.stop(Duration::from_secs(5));
        let retro: Retro = get(fleet.open().conn(), "retro", R1).unwrap();
        assert_eq!(retro.outcome, Some(RetroOutcome::Unfinished));
        assert_eq!(
            retro.learning.as_deref(),
            Some("curator stopped: the daemon stopped before the retro was finished")
        );
    }

    /// A Run again that arrives once the daemon is stopping claims nothing,
    /// so no curator starts after the runs were taken to be stopped.
    #[tokio::test]
    async fn a_stopping_daemon_starts_no_new_curator() {
        let fleet = fleet();
        let cookie = owner_cookie(fleet.dir.path());
        let retros = Retros::default();
        let app = crate::api::tests::app_retros(
            fleet.dir.path(),
            Arc::new(FakeCurator {
                db: fleet.db.clone(),
                endings: Mutex::new(VecDeque::from([Ending::UntilStopped])),
            }),
            retros.clone(),
        );
        retros.stop(Duration::from_secs(1));
        let (status, body) = run(&app, &cookie, R1, "late").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .ends_with("the daemon is stopping"),
            "{body}"
        );
        let r1 = one(&app, R1).await;
        assert_eq!(r1.status, RetroStatus::Unfinished);
        assert!(retros.runs().is_empty());
    }

    /// A retry of a Run again that arrives once the daemon is stopping
    /// answers with the retro its key claimed, as any replay does.
    #[tokio::test]
    async fn a_stopping_daemon_still_replays_a_run_again() {
        let fleet = fleet();
        let cookie = owner_cookie(fleet.dir.path());
        let retros = Retros::default();
        let app = crate::api::tests::app_retros(
            fleet.dir.path(),
            Arc::new(FakeCurator {
                db: fleet.db.clone(),
                endings: Mutex::new(VecDeque::from([Ending::UntilStopped])),
            }),
            retros.clone(),
        );
        let (status, body) = run(&app, &cookie, R1, "k").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        retros.stop(Duration::from_secs(5));
        let (status, body) = run(&app, &cookie, R1, "k").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["replayed"], true);
        assert_eq!(body["data"]["id"], R1);
        assert_eq!(body["data"]["outcome"], "unfinished");
        assert!(retros.runs().is_empty());
    }

    /// A running curator that ends only once `released` is set, and a
    /// running holdout eval that runs `eval`.
    fn slow_runs(
        released: &Arc<AtomicBool>,
        eval: impl FnOnce(&tiller_herdr::process::Interrupt) + Send + 'static,
    ) -> (Fleet, Retros, crate::memory::eval::Evals) {
        let fleet = fleet();
        let mut store = fleet.open();
        let curator = FakeCurator {
            db: fleet.db.clone(),
            endings: Mutex::new(vec![Ending::Holds(Arc::clone(released))].into()),
        };
        let Ok(run) = curator.retro_run(&mut store, &Ctx::new(Actor::Board), R1) else {
            panic!("the claim is refused");
        };
        let retros = Retros::default();
        retros.start(R1, run).unwrap();
        let evals = crate::memory::eval::Evals::default();
        evals.spawn(eval).unwrap();
        (fleet, retros, evals)
    }

    /// The daemon's stop raises the eval's flag at once, not after the
    /// curators' wait, so a running eval spends no quota meanwhile.
    #[test]
    fn a_slow_curator_does_not_delay_the_eval_stop() {
        let released = Arc::new(AtomicBool::new(false));
        let caught = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&caught);
        let (_fleet, retros, evals) = slow_runs(&released, move |interrupt| {
            while !interrupt.caught() {
                std::thread::sleep(Duration::from_millis(10));
            }
            seen.store(true, Ordering::SeqCst);
        });
        let stopper = {
            let (retros, evals) = (retros.clone(), evals.clone());
            std::thread::spawn(move || {
                crate::server::stop_board_runs(&retros, &evals, Duration::from_secs(60));
            })
        };
        let stopped = tiller_herdr::testing::eventually(|| caught.load(Ordering::SeqCst));
        released.store(true, Ordering::SeqCst);
        stopper.join().unwrap();
        assert!(
            stopped,
            "the eval was not stopped while a curator still ran"
        );
    }

    /// Both waits share one deadline: a stop of runs that never end takes
    /// the wait once, not once per kind.
    #[test]
    fn the_stop_waits_once_for_curators_and_evals() {
        let released = Arc::new(AtomicBool::new(false));
        let held = Arc::clone(&released);
        let (_fleet, retros, evals) = slow_runs(&released, move |_| {
            while !held.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let wait = Duration::from_secs(2);
        let started = std::time::Instant::now();
        crate::server::stop_board_runs(&retros, &evals, wait);
        let took = started.elapsed();
        released.store(true, Ordering::SeqCst);
        assert!(took < wait * 2, "the stop took {took:?}");
    }

    /// Stopping waits for a Run again that is still claiming, so its run is
    /// among the ones stopped.
    #[test]
    fn stopping_waits_for_a_claim_in_progress() {
        let retros = Retros::default();
        let claiming = retros.claiming();
        let stopper = {
            let retros = retros.clone();
            std::thread::spawn(move || retros.stop(Duration::from_secs(1)))
        };
        std::thread::sleep(Duration::from_millis(100));
        assert!(!retros.stopping(), "stop did not wait for the claim");
        drop(claiming);
        stopper.join().unwrap();
        assert!(retros.stopping());
    }
}
