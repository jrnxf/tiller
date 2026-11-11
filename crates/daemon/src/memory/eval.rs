//! The board's "Run holdout eval..." (`memory.md` section 7,
//! `learning-loop.md` section 7): the plan the confirmation shows, and the
//! run of `evals/run.sh` it starts, recorded as `tiller learn eval` records
//! one.
//!
//! A run takes model quota, so a start is refused under this host's quota
//! floor for new work, and the plan says how many model runs and how much
//! of the week it will probably take. One eval runs at a time. A run that
//! records nothing keeps its reason here and in an `eval_run.failed` event,
//! which the memory view still shows after the daemon restarts. A run the
//! daemon stops is stopped with everything its runner started, and only
//! the newest runs' directories stay under `<home>/evals`.

use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tiller_agents::holdout::{self, HoldoutError, Plan};
use tiller_agents::spawn::config::SpawnConfig;
use tiller_agents::spawn::quota_floor;
use tiller_core::domain::{ChangeSet, QuotaWindow, RunRole};
use tiller_core::memory::document_path;
use tiller_core::state::approvals::active_away;
use tiller_core::state::learning::{self, EvalFailure, Learning};
use tiller_core::state::list;
use tiller_core::{Actor, Ctx, Error, Store, Timestamp, ids};
use tiller_herdr::process::Interrupt;
use utoipa::ToSchema;

use super::dto::{MemoryEvalJob, MemoryEvalPlan, MemoryEvalQuota};
use super::{Writer, refused, scrub, write};
use crate::api::{ApiResult, AppState, board_ctx, body, guard, key, not_forwarded, read};
use crate::dto::{ErrorBody, MutationResponse};

/// The most finished jobs kept for the board, and the most run
/// directories kept under `<home>/evals`.
const KEPT: usize = 20;

/// Why a run the daemon stopped recorded nothing.
const STOPPED: &str = "the daemon stopped before the holdout eval finished; nothing recorded";

/// `POST /api/v1/memory/proposals/{id}/eval`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryEvalStart {
    pub idempotency_key: String,
    /// Refused unless absent or this host: memory is never forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

#[derive(Clone, Debug)]
struct Job {
    key: String,
    view: MemoryEvalJob,
}

#[derive(Default)]
struct Shared {
    jobs: Mutex<Vec<Job>>,
    /// The threads that run an eval; the daemon waits for them when it
    /// stops.
    threads: Mutex<Vec<JoinHandle<()>>>,
    /// Set when the daemon stops: every running eval is stopped with
    /// everything it started.
    stop: Arc<AtomicBool>,
}

/// The evals the board started: the running one and the failed ones.
#[derive(Clone, Default)]
pub struct Evals(Arc<Shared>);

impl Evals {
    fn lock(&self) -> MutexGuard<'_, Vec<Job>> {
        self.0.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn threads(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.0
            .threads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// What the memory view shows, newest first: this daemon's jobs, then
    /// the failures its `eval_run.failed` events keep for every other
    /// proposal, so a failure outlives a restart.
    pub fn jobs(&self, conn: &rusqlite::Connection) -> tiller_core::Result<Vec<MemoryEvalJob>> {
        let mut jobs: Vec<MemoryEvalJob> = self
            .lock()
            .iter()
            .rev()
            .map(|job| job.view.clone())
            .collect();
        for failed in learning::failed_evals(conn, KEPT)? {
            if jobs.len() >= KEPT {
                break;
            }
            if jobs
                .iter()
                .any(|job| job.proposal_id == failed.failure.proposal_id)
            {
                continue;
            }
            jobs.push(MemoryEvalJob {
                proposal_id: failed.failure.proposal_id,
                change_set: failed.change_set,
                cases: failed.failure.cases,
                started_at: failed.failure.started_at,
                minutes: failed.failure.minutes,
                error: Some(failed.failure.error),
                finished_at: Some(failed.finished_at),
            });
        }
        Ok(jobs)
    }

    fn running(&self) -> Option<MemoryEvalJob> {
        self.lock()
            .iter()
            .find(|job| job.view.finished_at.is_none())
            .map(|job| job.view.clone())
    }

    fn by_key(&self, key: &str) -> Option<MemoryEvalJob> {
        self.lock()
            .iter()
            .find(|job| job.key == key)
            .map(|job| job.view.clone())
    }

    fn start(&self, key: &str, view: MemoryEvalJob) {
        let mut jobs = self.lock();
        jobs.retain(|job| job.view.proposal_id != view.proposal_id);
        jobs.push(Job {
            key: key.to_owned(),
            view,
        });
        let over = jobs.len().saturating_sub(KEPT);
        jobs.drain(..over);
    }

    /// Runs `run` on a thread of its own, which the daemon stops with it.
    /// `run` gets the interrupt the stop raises.
    fn spawn(&self, run: impl FnOnce(&Interrupt) + Send + 'static) -> std::io::Result<()> {
        let interrupt = Interrupt::flag(Arc::clone(&self.0.stop));
        let handle = std::thread::Builder::new()
            .name("holdout-eval".into())
            .spawn(move || run(&interrupt))?;
        let mut threads = self.threads();
        threads.retain(|thread| !thread.is_finished());
        threads.push(handle);
        Ok(())
    }

    /// Whether the daemon is stopping.
    fn stopping(&self) -> bool {
        self.0.stop.load(Ordering::SeqCst)
    }

    /// Stops every running eval, with everything its runner started, and
    /// waits up to `wait` for each to say how it ended.
    pub fn stop(&self, wait: Duration) {
        self.0.stop.store(true, Ordering::SeqCst);
        let threads: Vec<JoinHandle<()>> = self.threads().drain(..).collect();
        let deadline = Instant::now() + wait;
        for thread in threads {
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }

    /// A run that recorded: the suggestion's holdout shows it from now on.
    fn recorded(&self, key: &str) {
        self.lock().retain(|job| job.key != key);
    }

    /// A run that recorded nothing: the job keeps why, and so does an
    /// `eval_run.failed` event on `set`, which the view reads after a
    /// restart.
    fn failed(&self, store: Option<&mut Store>, set: &str, key: &str, error: String) {
        let mut jobs = self.lock();
        let Some(job) = jobs.iter_mut().find(|job| job.key == key) else {
            return;
        };
        job.view.error = Some(error.clone());
        job.view.finished_at = Some(Timestamp::now().to_string());
        let failure = EvalFailure {
            proposal_id: job.view.proposal_id.clone(),
            cases: job.view.cases,
            started_at: job.view.started_at.clone(),
            minutes: job.view.minutes,
            error,
        };
        drop(jobs);
        let Some(store) = store else {
            tracing::warn!(change_set = %set, "the failed holdout eval is kept until the daemon stops only: state.db did not open");
            return;
        };
        if let Err(error) = store.eval_run_fail(&Ctx::new(Actor::Daemon), set, failure) {
            tracing::warn!(%error, change_set = %set, "the failed holdout eval is kept until the daemon stops only");
        }
    }
}

/// Removes all but the newest `keep` run directories under `evals` (each
/// named by a ULID, so the newest sort last), so `<home>/evals` does not
/// grow with every run. Anything else there is left alone.
fn prune(evals: &FsPath, keep: usize) {
    let Ok(entries) = std::fs::read_dir(evals) else {
        return;
    };
    let mut runs: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name().to_str().is_some_and(ids::is_ulid))
        .map(|entry| entry.path())
        .collect();
    runs.sort();
    let over = runs.len().saturating_sub(keep);
    for old in runs.drain(..over) {
        if let Err(error) = std::fs::remove_dir_all(&old) {
            tracing::warn!(%error, run = ?old.file_name(), "could not remove an old holdout eval's directory");
        }
    }
}

/// `config.toml`'s `[learning]` and spawn settings, read on every call so
/// an edit needs no restart.
fn config(home: &FsPath) -> Result<(Learning, SpawnConfig), String> {
    let table = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map_err(|error| format!("config.toml does not parse: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(error) => return Err(format!("read config.toml: {error}")),
    };
    let learning = Learning::from_table(&table).map_err(|error| error.to_string())?;
    let spawn = SpawnConfig::from_table(&table).map_err(|error| format!("config.toml: {error}"))?;
    Ok((learning, spawn))
}

fn tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The weekly window with the least left, checked against this host's floor
/// for new work and an away contract's floor, the way a builder's start is.
fn quota(
    store: &Store,
    spawn: &SpawnConfig,
    percent: f64,
    now: Timestamp,
) -> tiller_core::Result<(MemoryEvalQuota, Option<String>)> {
    let conn = store.conn();
    let away = active_away(conn, now)?.and_then(|away| away.quota_floor_percent);
    let floor = match (quota_floor(spawn, RunRole::Builder), away) {
        (Some(standing), Some(away)) => Some(standing.max(away)),
        (standing, away) => standing.or(away),
    };
    let windows: Vec<QuotaWindow> = list(conn, "kind = 'weekly' ORDER BY label", &[])?;
    let least = windows
        .iter()
        .min_by(|a, b| {
            let left = |w: &QuotaWindow| w.percent_remaining.unwrap_or(f64::NEG_INFINITY);
            left(a).total_cmp(&left(b))
        })
        .cloned();
    let left = least.as_ref().and_then(|w| w.percent_remaining);
    let view = MemoryEvalQuota {
        label: least.as_ref().map(|w| w.label.clone()),
        left,
        floor,
        // A run cannot leave less than nothing.
        left_after: left.map(|left| tenth(left - percent).max(0.0)),
        resets_at: least
            .as_ref()
            .and_then(|w| w.resets_at)
            .map(|at| at.to_string()),
        fetched_at: least.as_ref().map(|w| w.fetched_at.to_string()),
    };
    let Some(floor) = floor else {
        return Ok((view, None));
    };
    if windows.is_empty() {
        return Ok((
            view,
            Some("there is no weekly quota reading, and the quota floor fails closed".into()),
        ));
    }
    let oldest_allowed = now.plus_minutes(-spawn.quota.max_age_minutes);
    if windows
        .iter()
        .any(|w| w.fetch_error.is_some() || w.fetched_at < oldest_allowed)
    {
        return Ok((
            view,
            Some("a weekly quota reading is stale, and the quota floor fails closed".into()),
        ));
    }
    let refusal = match view.left_after {
        Some(after) if after > floor => None,
        Some(after) => Some(format!(
            "it would leave {after} percent of the week, at or under the {floor} percent floor"
        )),
        None => {
            Some("a weekly quota window has no reading, and the quota floor fails closed".into())
        }
    };
    Ok((view, refusal))
}

/// What a start would do, and why it would be refused.
struct Planned {
    view: MemoryEvalPlan,
    evals_dir: Option<PathBuf>,
    set: Option<ChangeSet>,
    window_cards: i64,
}

fn planned(
    store: &Store,
    home: &FsPath,
    proposal: &str,
    head: &str,
    evals: &Evals,
    now: Timestamp,
) -> tiller_core::Result<Planned> {
    let suggestion = store.memory_suggestion(proposal, head)?;
    let id = suggestion.proposal_id.clone();
    let set = learning::open_set_of(store.conn(), &id)?;
    let mut refusals: Vec<String> = Vec::new();
    let (learning, spawn) = match config(home) {
        Ok(config) => config,
        Err(error) => {
            refusals.push(error);
            let spawn = SpawnConfig::from_table(&toml::Table::new())
                .map_err(|error| Error::Internal(error.to_string()))?;
            (Learning::default(), spawn)
        }
    };
    let state: String =
        store
            .conn()
            .query_row("SELECT state FROM proposals WHERE id = ?1", [&id], |row| {
                row.get(0)
            })?;
    if state != "proposed" {
        refusals.push(format!("the proposal is {state}, not proposed"));
    }
    if let Some(job) = evals.running() {
        refusals.push(format!(
            "a holdout eval is already running, on change set {}",
            job.change_set
        ));
    }
    if suggestion.moved {
        refusals.push(
            "the document changed after this was proposed; re-propose it on the current text first"
                .into(),
        );
    } else if let Some(measuring) = &suggestion.gate.measuring {
        refusals.push(format!(
            "change set {} is measuring, {} of {} cards closed, and the next change waits for it",
            measuring.name, measuring.cards_closed, measuring.window_cards
        ));
    } else if state == "proposed" && !suggestion.eval_clears {
        refusals.push(match &suggestion.holdout {
            Some(run) if run.current => format!(
                "the holdout already ran on these members: {} of {} cases",
                run.cases_passed, run.cases_total
            ),
            _ => suggestion
                .gate
                .reason
                .clone()
                .unwrap_or_else(|| "the gate already passes".into()),
        });
    }
    let evals_dir = learning.evals_dir.as_deref().map(PathBuf::from);
    let plan = match &evals_dir {
        None => {
            refusals.push(
                "learning.evals_dir is not set; it names the checkout that holds evals/run.sh"
                    .into(),
            );
            Plan::default()
        }
        Some(dir) => match holdout::plan(dir) {
            Ok(plan) => plan,
            Err(error) => {
                refusals.push(error.to_string());
                Plan::default()
            }
        },
    };
    let model_runs = plan.model_runs();
    let percent = tenth(model_runs as f64 * learning.eval_percent_per_run);
    let (quota, short) = quota(store, &spawn, percent, now)?;
    refusals.extend(short);
    Ok(Planned {
        view: MemoryEvalPlan {
            proposal_id: id.clone(),
            change_set: set
                .as_ref()
                .map_or_else(|| ids::short(&id).to_owned(), |set| set.name.clone()),
            cases: plan.cases,
            case_runs: plan.case_runs,
            judge_runs: plan.judge_runs,
            model_runs,
            percent,
            minutes: tenth(plan.case_runs as f64 * learning.eval_minutes_per_run),
            quota,
            refusal: refusals.into_iter().next(),
        },
        evals_dir,
        set,
        window_cards: learning.window_cards,
    })
}

fn head(home: &FsPath, store: &Store, proposal: &str) -> tiller_core::Result<String> {
    let path = document_path(&store.memory_change(proposal)?.project_id);
    Ok(super::apply::Repo::at(home)
        .state(&path, &scrub::terms(home))?
        .head_blob)
}

fn holdout_error(proposal: &str, error: HoldoutError) -> Error {
    match error {
        HoldoutError::Core(error) => error,
        other => refused("proposal", proposal, other.to_string()),
    }
}

#[utoipa::path(get, path = "/api/v1/memory/proposals/{id}/eval", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = MemoryEvalPlan, description = "What the holdout eval would cost, and why it would not start"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn eval_plan(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<MemoryEvalPlan> {
    let home = state.0.home.clone();
    let evals = state.0.evals.clone();
    let plan = read(&state, move |store| {
        let head = head(&home, store, &id)?;
        Ok(planned(store, &home, &id, &head, &evals, Timestamp::now())?.view)
    })
    .await?;
    Ok(Json(plan))
}

#[utoipa::path(post, path = "/api/v1/memory/proposals/{id}/eval", tag = "mutations", request_body = MemoryEvalStart,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "The eval started; the memory view's `evals` shows it until it records"),
              (status = 422, body = ErrorBody, description = "the plan's refusal: not proposed, the text moved, a set is measuring, the gate needs no run, no evals checkout, under the quota floor, or an eval is running"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
pub(crate) async fn eval_start(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: MemoryEvalStart = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a holdout eval").await?;
    let key = key(&request.idempotency_key)?;
    let home = state.0.home.clone();
    let evals = state.0.evals.clone();
    let db = state.db();
    write(
        &state,
        move |w: &mut Writer, key, args| {
            // A retry of a start answers with the job it started.
            if let Some(job) = evals.by_key(key) {
                return Ok(serde_json::to_value(job)?);
            }
            let path = document_path(&w.store.memory_change(&id)?.project_id);
            let head = w.repo.state(&path, &w.list)?.head_blob;
            let now = Timestamp::now();
            let planned = planned(&w.store, &home, &id, &head, &evals, now)?;
            let proposal = planned.view.proposal_id.clone();
            if let Some(refusal) = planned.view.refusal {
                return Err(refused("proposal", &proposal, refusal));
            }
            let Some(evals_dir) = planned.evals_dir else {
                return Err(refused("proposal", &proposal, "no evals checkout"));
            };
            let set = match planned.set {
                Some(set) => set,
                None => {
                    let created = w.store.change_set_create(
                        &board_ctx(key, args),
                        ids::short(&proposal),
                        std::slice::from_ref(&proposal),
                        planned.window_cards,
                    )?;
                    w.track(created)
                }
            };
            // The members the candidate stands for, taken before the run: a
            // member added meanwhile makes the record refuse.
            let members = learning::members_sha256_of(w.store.conn(), &set.id)?;
            let candidate = holdout::render_candidate(w.store.conn(), &set, &home)
                .map_err(|error| holdout_error(&proposal, error))?;
            // The newest runs' results stay, this one's among them.
            let runs = home.join("evals");
            prune(&runs, KEPT.saturating_sub(1));
            let out = runs.join(ids::new_ulid());
            let job = MemoryEvalJob {
                proposal_id: proposal,
                change_set: set.name.clone(),
                cases: planned.view.cases,
                started_at: now.to_string(),
                minutes: planned.view.minutes,
                error: None,
                finished_at: None,
            };
            evals.start(key, job.clone());
            let registry = evals.clone();
            let job_key = key.to_owned();
            let set_id = set.id.clone();
            let key = key.to_owned();
            let started = registry.spawn(move |interrupt| {
                let recorded = holdout::run(&evals_dir, &out, &candidate, members, interrupt)
                    .map_err(|error| {
                        if evals.stopping() {
                            STOPPED.to_owned()
                        } else {
                            error.to_string()
                        }
                    })
                    .and_then(|result| {
                        let mut store =
                            Store::open_current(&db).map_err(|error| error.to_string())?;
                        store
                            .eval_run_record(&Ctx::new(Actor::Daemon), &set.id, result)
                            .map_err(|error| error.to_string())
                    });
                match recorded {
                    Ok(_) => evals.recorded(&key),
                    Err(error) => {
                        tracing::warn!(%error, change_set = %set.name, "the holdout eval recorded nothing");
                        let mut store = Store::open_current(&db).ok();
                        evals.failed(store.as_mut(), &set.id, &key, error);
                    }
                }
            });
            if let Err(error) = started {
                let error = format!("start the holdout eval: {error}");
                registry.failed(Some(&mut w.store), &set_id, &job_key, error.clone());
                return Err(Error::Internal(error));
            }
            Ok(json!(job))
        },
        key,
    )
    .await
}
