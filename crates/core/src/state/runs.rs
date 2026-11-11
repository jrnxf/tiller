//! Runs, sessions and observations: `run.*`, `session.*`,
//! `observation.record` (`state.md` sections 3 and 8).

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::approvals::{AwayContract, active_away};
use super::attention::{quota_start_decision, withdraw_attention};
use super::cards::{landed_evidence, new_card_note};
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::projects::{local_host_id, require_daemon};
use super::publishes::published_head;
use super::{find, get, list, refuse, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// The live readings `tiller spawn` takes before `run.start`, checked inside
/// its transaction and written into the `run.started` payload.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpawnChecks {
    pub ram_free_mb: Option<i64>,
    pub disk_free_mb: Option<i64>,
    /// The weekly quota floor; `None` skips the quota check (no quota feed
    /// configured on this host).
    pub quota_floor_percent: Option<f64>,
    /// How old a quota reading may be before the check fails closed.
    pub quota_max_age_minutes: i64,
    /// Start past the quota floor on the owner's answered decision. The
    /// decision is looked up and used inside the `run.start` transaction, so
    /// one answer starts one run; the standing floor is skipped (an away
    /// contract's still binds) and the item is recorded in the `run.started`
    /// payload.
    pub past_quota_floor: bool,
    /// The dispatch rule that resolved the model and effort (T4.2).
    pub dispatch: Value,
}

impl SpawnChecks {
    /// The standing quota floor, skipped past the owner's decision.
    fn standing_floor(&self) -> Option<f64> {
        self.quota_floor_percent.filter(|_| !self.past_quota_floor)
    }
}

/// A run to start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewRun {
    /// A card reference.
    pub card: String,
    pub role: RunRole,
    pub model: String,
    pub effort: Effort,
    /// The host to run on; the local host when `None`.
    pub host_id: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub base_sha: Option<String>,
    pub config_hash: Option<String>,
    pub checks: SpawnChecks,
}

/// A scout's promotion to a builder run on the same card and worktree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Promote {
    /// The ended scout run, as a run reference.
    pub scout_run: String,
    pub model: String,
    pub effort: Effort,
    pub checks: SpawnChecks,
}

/// What `run.start` wrote.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Started {
    pub run: Run,
    pub session: Session,
    pub reservation: Reservation,
}

/// Where a spawned run works, recorded once spawn has made it: `run.start`
/// reserves before the worktree and pane exist (`state.md`, Reservation).
/// `None` leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub base_sha: Option<String>,
    /// The SHA-256 of the run's settings fragment, for the run and its session.
    pub config_hash: Option<String>,
    /// The herdr pane spawn opened for the run's session.
    pub pane_id: Option<String>,
}

/// How a run ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EndRun {
    pub outcome: RunOutcome,
    pub note: Option<String>,
}

/// An orchestrator session to start (`tiller orchestrator start`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewOrchestrator {
    /// The session id, when the caller minted it first: the launch files
    /// name the session before its row exists. A ULID; `None` mints one.
    pub id: Option<String>,
    pub project_id: String,
    pub pane_id: Option<String>,
    pub claude_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub harness: Option<String>,
    pub config_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OrchestratorStarted {
    pub session: Session,
    pub notes: Vec<OutboxMessage>,
}

/// What the `SessionStart` hook binds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bind {
    /// The Tiller session id the spawn handed the pane.
    pub session_id: String,
    pub claude_session_id: String,
    pub pane_id: Option<String>,
    pub transcript_path: Option<String>,
    /// `startup`, `resume`, `clear` or `compact`, as the hook reports it.
    pub source: String,
}

/// A relaunch (`tiller agent relaunch`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Relaunch {
    pub note: String,
    /// A new attempt with a new brief, or a new session on the same run.
    pub new_attempt: bool,
    /// How the old session was proven stopped (the ladder's dead verdict or a
    /// confirmed exit); required when the run has a live session.
    pub stop_proof: Option<String>,
    /// The model a new attempt runs on, as dispatch resolved it; `None`
    /// keeps the old attempt's. A same-run relaunch keeps the run's model.
    pub model: Option<AttemptModel>,
    /// The spawn readings a new attempt is checked against, as `run.start`
    /// checks a spawn; `None` checks capacity alone.
    #[serde(default)]
    pub checks: Option<SpawnChecks>,
}

/// A new attempt's model and effort, and the dispatch rule behind them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttemptModel {
    pub model: String,
    pub effort: Effort,
    pub dispatch: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Relaunched {
    pub run: Run,
    pub session: Session,
    pub ended_session: Option<String>,
}

/// One observation to append.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewObservation {
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub source: ObservationSource,
    pub event: String,
    pub verdict: Option<Activity>,
    pub observed_at: Timestamp,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    pub observation: Observation,
    /// The session's activity moved to the observation's verdict.
    pub verdict_changed: bool,
}

/// How long a hook `working` holds against a `herdr:idle` (`protocol.md`
/// section 4.5): herdr can read idle through a whole turn.
pub const HOOK_WORKING_HOLD_MS: i64 = 30_000;

/// A `herdr:idle` never replaces a hook `working` less than 30 s old.
fn herdr_idle_held(session: &Session, new: &NewObservation) -> bool {
    new.source == ObservationSource::Herdr
        && new.verdict == Some(Activity::Idle)
        && session.activity == Activity::Working
        && session.activity_source == Some(ObservationSource::Hook)
        && session
            .activity_observed_at
            .is_some_and(|at| new.observed_at.as_millis() - at.as_millis() < HOOK_WORKING_HOLD_MS)
}

/// A `panes` filter for the panes of the run bound at `?{param}`. A pane row
/// names its session only while the session is live, so the pane's own `run`
/// and `session` tokens find it once the agent has exited.
pub fn run_panes(param: usize) -> String {
    format!(
        "(session_id IN (SELECT id FROM sessions WHERE run_id = ?{param})
          OR json_extract(metadata, '$.run') = ?{param}
          OR json_extract(metadata, '$.session') IN (SELECT id FROM sessions WHERE run_id = ?{param}))"
    )
}

/// The live session of a run.
pub fn live_session(conn: &Connection, run_id: &str) -> Result<Option<Session>> {
    find(conn, "run_id = ?1 AND ended_at IS NULL", &[&run_id])
}

/// The treehouse lease holder a run takes a worktree from the pool under.
pub fn holder(run: &Run) -> String {
    format!("tiller:{}", run.id)
}

/// The lease holder of the worktree a run works in: the run that took it
/// from the pool. A promotion or a new attempt continues in its
/// predecessor's worktree, so the chain's first run holds the lease.
pub fn lease_holder(conn: &Connection, run: &Run) -> Result<String> {
    let mut root = run.clone();
    while let Some(id) = root.predecessor_run_id.clone() {
        root = get(conn, "run", &id)?;
    }
    Ok(holder(&root))
}

/// Where `herdr agent focus` finds a session's agent (`tiller agent focus`,
/// the board's "Open in herdr"): its pane, from the `panes` cache first and
/// the session's own `pane_id` after, and the saved herdr machine of a
/// remote host. An ended session is refused: its pane may hold another.
pub fn focus_target(conn: &Connection, session: &Session) -> Result<(String, Option<String>)> {
    if session.ended_at.is_some() {
        return Err(Error::refused("session", &session.id, "the session ended"));
    }
    let host: Host = get(conn, "host", &session.host_id)?;
    let pane = find::<Pane>(conn, "session_id = ?1 AND gone_at IS NULL", &[&session.id])?
        .map(|pane| pane.pane_id)
        .or(session.pane_id.clone())
        .ok_or_else(|| {
            Error::refused("session", &session.id, "the session has no pane to focus")
        })?;
    let machine = (host.kind == HostKind::Remote)
        .then_some(host.herdr_machine)
        .flatten();
    Ok((pane, machine))
}

fn load_run(tx: &Tx<'_>, run: &str) -> Result<Run> {
    let id = resolve_id(tx.conn(), "runs", "run", run)?;
    get(tx.conn(), "run", &id)
}

fn load_session(tx: &Tx<'_>, session: &str) -> Result<Session> {
    let id = resolve_id(tx.conn(), "sessions", "session", session)?;
    get(tx.conn(), "session", &id)
}

pub(crate) fn require_active(run: &Run) -> Result<()> {
    match run.outcome {
        None => Ok(()),
        Some(outcome) => Err(Error::refused(
            "run",
            &run.id,
            format!("run ended {outcome}"),
        )),
    }
}

fn session_role(role: RunRole) -> Role {
    match role {
        RunRole::Scout => Role::Scout,
        RunRole::Builder => Role::Builder,
        RunRole::Reviewer => Role::Reviewer,
        RunRole::Verifier => Role::Verifier,
    }
}

/// The orchestrator that will supervise a run `actor` starts: the actor
/// itself when it is the project's live orchestrator, the live orchestrator
/// when the owner spawns. Workers never spawn (G28).
fn supervisor(conn: &Connection, actor: &Actor, project_id: &str) -> Result<Session> {
    let live = live_orchestrator(conn, project_id)?;
    match actor {
        Actor::Session(id) => match live {
            Some(session) if session.id == *id => Ok(session),
            _ => Err(Error::Forbidden(format!(
                "session {id} is not the live orchestrator of {project_id}; only the orchestrator or the owner starts a run"
            ))),
        },
        Actor::Owner | Actor::Board => live.ok_or_else(|| {
            Error::refused(
                "project",
                project_id,
                "no live orchestrator to supervise the run",
            )
        }),
        other => Err(Error::Forbidden(format!("{other} may not start a run"))),
    }
}

/// Frees `claude_id` for `session_id`. Claude ids are unique, and a resume
/// can carry an ended session's id into a new row: the ended row gives the
/// id up, with the taker in its event. A live row keeps it.
fn take_claude_id(tx: &mut Tx<'_>, claude_id: &str, session_id: &str) -> Result<()> {
    let Some(holder) = find::<Session>(
        tx.conn(),
        "claude_session_id = ?1 AND id != ?2",
        &[&claude_id, &session_id],
    )?
    else {
        return Ok(());
    };
    if holder.ended_at.is_none() {
        return Err(Error::refused(
            "session",
            &holder.id,
            format!("Claude session {claude_id} is bound to this live session"),
        ));
    }
    tx.conn().execute(
        "UPDATE sessions SET claude_session_id = NULL, rev = rev + 1 WHERE id = ?1",
        [&holder.id],
    )?;
    tx.event(
        NewEvent::new("session", &holder.id, "session.unbound")
            .rev(holder.rev + 1)
            .payload(json!({"claude_session_id": claude_id, "taken_by": session_id})),
    )?;
    Ok(())
}

/// Inserts a placeholder session for a run; the `SessionStart` hook binds it.
fn insert_run_session(
    tx: &mut Tx<'_>,
    run: &Run,
    project_id: &str,
    config_hash: Option<&str>,
) -> Result<Session> {
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO sessions (id, role, project_id, run_id, host_id, config_hash, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            session_role(run.role),
            project_id,
            run.id,
            run.host_id,
            config_hash,
            tx.now()
        ],
    )?;
    tx.event(
        NewEvent::new("session", &id, "session.started")
            .rev(1)
            .card(&run.card_id)
            .payload(json!({"role": session_role(run.role), "run_id": run.id})),
    )?;
    get(tx.conn(), "session", &id)
}

fn end_session(tx: &mut Tx<'_>, session: &Session, reason: &str) -> Result<Session> {
    tx.conn().execute(
        "UPDATE sessions SET ended_at = ?2, end_reason = ?3, rev = rev + 1 WHERE id = ?1",
        params![session.id, tx.now(), reason],
    )?;
    let card = match &session.run_id {
        Some(run) => Some(get::<Run>(tx.conn(), "run", run)?.card_id),
        None => None,
    };
    tx.event(
        NewEvent::new("session", &session.id, "session.ended")
            .rev(session.rev + 1)
            .card_opt(card)
            .payload(json!({"reason": reason, "run_id": session.run_id})),
    )?;
    get(tx.conn(), "session", &session.id)
}

fn reserve(tx: &mut Tx<'_>, host: &Host, project: &Project, run: &Run) -> Result<Reservation> {
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO reservations (id, host_id, project_id, run_id, reserved_at, reserved_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            host.id,
            project.id,
            run.id,
            tx.now(),
            tx.actor().to_string()
        ],
    )?;
    tx.event(
        NewEvent::new("reservation", &id, "reservation.reserved")
            .card(&run.card_id)
            .payload(json!({"host_id": host.id, "project_id": project.id, "run_id": run.id})),
    )?;
    get(tx.conn(), "reservation", &id)
}

/// Releases a run's open reservation, if it has one.
pub(crate) fn release_reservation(
    tx: &mut Tx<'_>,
    run: &Run,
    reason: ReservationRelease,
) -> Result<Option<String>> {
    let Some(reservation) =
        find::<Reservation>(tx.conn(), "run_id = ?1 AND released_at IS NULL", &[&run.id])?
    else {
        return Ok(None);
    };
    tx.conn().execute(
        "UPDATE reservations SET released_at = ?2, release_reason = ?3 WHERE id = ?1",
        params![reservation.id, tx.now(), reason],
    )?;
    tx.event(
        NewEvent::new("reservation", &reservation.id, "reservation.released")
            .card(&run.card_id)
            .payload(json!({"reason": reason, "run_id": run.id})),
    )?;
    // A failed spawn is the orchestrator's own retry in flight: waking it
    // would retry a spawn that keeps failing, forever.
    if reason != ReservationRelease::SpawnFailed {
        note_slot_open(
            tx,
            &reservation.project_id,
            Some(&run.card_id),
            format!(
                "a run slot opened: run {} ended ({}); retry any queued spawn",
                ids::short(&run.id),
                reason
            ),
        )?;
    }
    Ok(Some(reservation.id))
}

/// Wakes the project's live orchestrator, if any, so it retries spawns that
/// were refused as full.
pub(crate) fn note_slot_open(
    tx: &mut Tx<'_>,
    project_id: &str,
    card_id: Option<&str>,
    body: String,
) -> Result<()> {
    if let Some(orchestrator) = live_orchestrator(tx.conn(), project_id)? {
        enqueue(
            tx,
            &NewMessage {
                target_session_id: orchestrator.id,
                kind: OutboxKind::Note,
                body,
                card_id: card_id.map(str::to_owned),
                reply_to: None,
            },
        )?;
    }
    Ok(())
}

/// Counts the open reservations on a host and a project.
fn reserved(conn: &Connection, column: &str, id: &str) -> Result<i64> {
    Ok(conn.query_row(
        &format!("SELECT count(*) FROM reservations WHERE {column} = ?1 AND released_at IS NULL"),
        [id],
        |row| row.get(0),
    )?)
}

/// Checks capacity, the floors and the quota inside the transaction, and
/// returns what it saw for the event payload.
/// Refuses a new reservation on a full host or project; returns the counts.
fn check_capacity(conn: &Connection, host: &Host, project: &Project) -> Result<(i64, i64)> {
    let host_reserved = reserved(conn, "host_id", &host.id)?;
    if let Some(lanes) = host.lane_capacity
        && host_reserved >= lanes
    {
        return Err(Error::refused(
            "host",
            &host.label,
            format!("host is full: {host_reserved} of {lanes} lanes reserved"),
        ));
    }
    let project_reserved = reserved(conn, "project_id", &project.id)?;
    if project_reserved >= project.capacity {
        return Err(Error::refused(
            "project",
            &project.id,
            format!(
                "project is full: {project_reserved} of {} runs reserved",
                project.capacity
            ),
        ));
    }
    Ok((host_reserved, project_reserved))
}

/// Whether the run's worktree went back to the pool: a run of its relaunch
/// chain on the same worktree was cleaned (`run.clean`). The chain shares one
/// worktree and one lease, so a clean by any of its runs returned it. The
/// worktree reading's `gone_at` is not the test: the pool may have handed the
/// path to another run since, and a later reading clears it.
fn worktree_returned(conn: &Connection, run: &Run, path: &str) -> Result<bool> {
    let mut root = run.clone();
    while let Some(id) = root.predecessor_run_id.clone() {
        root = get(conn, "run", &id)?;
    }
    Ok(conn.query_row(
        "WITH RECURSIVE chain(id) AS (
             SELECT ?1
             UNION SELECT r.id FROM runs r JOIN chain c ON r.predecessor_run_id = c.id
         )
         SELECT EXISTS (SELECT 1 FROM chain c
                        JOIN runs r ON r.id = c.id
                        JOIN events e ON e.entity = 'run' AND e.entity_id = r.id AND e.kind = 'run.cleaned'
                        WHERE r.host_id = ?2 AND r.worktree_path = ?3)",
        params![root.id, run.host_id, path],
        |row| row.get(0),
    )?)
}

/// `run.relaunch`'s transaction body, shared with its rehearsal.
fn relaunch_in(tx: &mut Tx<'_>, run: &str, relaunch: &Relaunch) -> Result<Relaunched> {
    let run = load_run(tx, run)?;
    match tx.actor() {
        Actor::Owner | Actor::Board => {}
        Actor::Session(id) if *id == run.supervisor_session_id => {}
        other => {
            return Err(Error::Forbidden(format!(
                "{other} may not relaunch run {}; its supervisor or the owner does",
                run.id
            )));
        }
    }
    let Some(path) = run.worktree_path.as_deref() else {
        return Err(Error::refused(
            "run",
            &run.id,
            "run has no worktree to relaunch in",
        ));
    };
    if worktree_returned(tx.conn(), &run, path)? {
        return Err(Error::refused(
            "run",
            &run.id,
            "the run's worktree was already returned; spawn a run on a fresh worktree instead",
        ));
    }
    let card: Card = get(tx.conn(), "card", &run.card_id)?;
    let mut ended_session = None;
    if let Some(session) = live_session(tx.conn(), &run.id)? {
        let Some(proof) = &relaunch.stop_proof else {
            return Err(Error::refused(
                "session",
                &session.id,
                "the live session is not proven stopped",
            ));
        };
        end_session(tx, &session, &format!("relaunched ({proof})"))?;
        ended_session = Some(session.id);
    }
    let (target, checks) = if relaunch.new_attempt {
        // A new attempt is a new run: it passes the spawn checks that
        // still apply to it, and the project's live orchestrator
        // supervises it, not the one that started the old attempt.
        if card.state != CardState::Open {
            return Err(Error::refused("card", &card.display_id, "card is closed"));
        }
        let project: Project = get(tx.conn(), "project", &card.project_id)?;
        if project.retired_at.is_some() {
            return Err(Error::refused("project", &project.id, "project is retired"));
        }
        if let Some(active) = find::<Run>(
            tx.conn(),
            "card_id = ?1 AND role = ?2 AND ended_at IS NULL AND id != ?3",
            &[&card.id, &run.role, &run.id],
        )? {
            return Err(Error::refused(
                "card",
                &card.display_id,
                format!("{} run {} is already active", run.role, active.id),
            ));
        }
        let supervisor = live_orchestrator(tx.conn(), &project.id)?.ok_or_else(|| {
            Error::refused(
                "project",
                &project.id,
                "no live orchestrator to supervise the run",
            )
        })?;
        // The new attempt is its supervisor's to place, so only that
        // orchestrator or the owner starts it.
        if let Actor::Session(id) = tx.actor()
            && *id != supervisor.id
        {
            return Err(Error::Forbidden(format!(
                "the new attempt would be supervised by {}, the project's live orchestrator; relaunch it from that session or as the owner",
                supervisor.id
            )));
        }
        if run.outcome.is_none() {
            end_run(
                tx,
                &run,
                EndRun {
                    outcome: RunOutcome::Relaunched,
                    note: Some(relaunch.note.clone()),
                },
            )?;
        }
        let host: Host = get(tx.conn(), "host", &run.host_id)?;
        let checks = match &relaunch.checks {
            Some(checks) => Some(check_spawn(
                tx.conn(),
                &host,
                &project,
                checks,
                None,
                tx.now(),
            )?),
            None => {
                check_capacity(tx.conn(), &host, &project)?;
                check_spawn_rate(tx.conn(), &host, &project, tx.now())?;
                None
            }
        };
        let id = ids::new_ulid();
        let attempt = next_attempt(tx.conn(), &card.id, run.role)?;
        let (model, effort) = match &relaunch.model {
            Some(m) => (m.model.clone(), m.effort),
            None => (run.model.clone(), run.effort),
        };
        // The new brief means new launch files: the caller records
        // their hash with `run.place`.
        tx.conn().execute(
                "INSERT INTO runs (id, card_id, role, attempt, predecessor_run_id, model, effort, host_id,
                                   worktree_path, branch, base_branch, base_sha, supervisor_session_id,
                                   started_at)
                 SELECT ?1, card_id, role, ?2, id, ?6, ?7, host_id, worktree_path, branch, base_branch,
                        base_sha, ?5, ?3
                 FROM runs WHERE id = ?4",
                params![id, attempt, tx.now(), run.id, supervisor.id, model, effort],
            )?;
        let new_run: Run = get(tx.conn(), "run", &id)?;
        reserve(tx, &host, &project, &new_run)?;
        (new_run, checks)
    } else {
        require_active(&run)?;
        // A relaunch in place is the other half of a respawn storm.
        let project: Project = get(tx.conn(), "project", &card.project_id)?;
        let host: Host = get(tx.conn(), "host", &run.host_id)?;
        check_spawn_rate(tx.conn(), &host, &project, tx.now())?;
        (run.clone(), None)
    };
    tx.event(
        NewEvent::new("run", &target.id, "run.relaunched")
            .rev(target.rev)
            .card(&card.id)
            .payload(json!({
                "note": relaunch.note,
                "new_attempt": relaunch.new_attempt,
                "predecessor_run_id": target.predecessor_run_id,
                "stop_proof": relaunch.stop_proof,
                "model": target.model,
                "effort": target.effort,
                "dispatch": relaunch.model.as_ref().map(|m| &m.dispatch),
                "checks": checks,
            })),
    )?;
    let session = insert_run_session(tx, &target, &card.project_id, target.config_hash.as_deref())?;
    if let Some(supervisor) = live_supervisor(tx.conn(), &target)? {
        enqueue(
            tx,
            &NewMessage {
                target_session_id: supervisor,
                kind: OutboxKind::Note,
                body: format!(
                    "[{}] {} relaunched: {}",
                    card.display_id,
                    run.role.short_name(),
                    relaunch.note
                ),
                card_id: Some(card.id.clone()),
                reply_to: None,
            },
        )?;
    }
    Ok(Relaunched {
        run: get(tx.conn(), "run", &target.id)?,
        session,
        ended_session,
    })
}

/// The refusal text of a weekly window at or under the quota floor.
fn quota_floor_fact(left: Option<f64>, floor: f64) -> String {
    let left = left.map_or_else(|| "unknown".to_owned(), |left| left.to_string());
    format!("{left} percent left is at or under the {floor} percent floor")
}

/// The quota floor a start is held to: the standing floor, unless the
/// owner's decision skips it, and an away contract's floor when stricter.
fn effective_quota_floor(standing: Option<f64>, away: Option<&AwayContract>) -> Option<f64> {
    match away.and_then(|away| away.quota_floor_percent) {
        Some(floor) => Some(standing.map_or(floor, |standing| standing.max(floor))),
        None => standing,
    }
}

/// Whether a weekly window is at or under `floor`; an unknown reading is.
fn at_or_under(window: &QuotaWindow, floor: f64) -> bool {
    window.percent_remaining.is_none_or(|left| left <= floor)
}

/// The latest reset among the weekly windows at or under the standing floor
/// `checks` are held to: the reset a refused start waits for. `None` when the
/// standing floor refuses nothing, and when an away contract's floor would
/// refuse the start past it too, since no owner answer lifts that floor.
pub fn quota_floor_reset(
    conn: &Connection,
    checks: &SpawnChecks,
    now: Timestamp,
) -> Result<Option<Timestamp>> {
    let Some(floor) = checks.standing_floor() else {
        return Ok(None);
    };
    let windows: Vec<QuotaWindow> = list(conn, "kind = 'weekly' ORDER BY label", &[])?;
    if let Some(away) = active_away(conn, now)?.and_then(|away| away.quota_floor_percent)
        && windows.iter().any(|window| at_or_under(window, away))
    {
        return Ok(None);
    }
    Ok(windows
        .iter()
        .filter(|window| at_or_under(window, floor))
        .filter_map(|window| window.resets_at)
        .max())
}

/// The owner's unused "Start now anyway" decision a start past the quota
/// floor rests on, when `checks` ask for one. Refuses when there is none.
fn quota_override(
    conn: &Connection,
    card: &Card,
    role: RunRole,
    checks: &SpawnChecks,
) -> Result<Option<String>> {
    if !checks.past_quota_floor {
        return Ok(None);
    }
    let item = quota_start_decision(conn, &card.id, role)?.ok_or_else(|| {
        Error::refused(
            "card",
            &card.display_id,
            format!("no unused owner decision to start a {role} run past the quota floor"),
        )
    })?;
    Ok(Some(item.id))
}

/// Whether `error` is the quota floor refusing a start, as opposed to a
/// missing or stale reading, which fails closed and has no reset to wait for.
pub fn is_quota_floor_refusal(error: &Error) -> bool {
    matches!(error, Error::Refused { entity: "quota", fact, .. } if fact.ends_with("percent floor"))
}

/// Why a RAM or disk reading fails its floor, if it does. An unknown
/// reading under a set floor fails closed.
pub fn floor_refusal(name: &str, floor: Option<i64>, free: Option<i64>) -> Option<String> {
    match (floor, free) {
        (Some(floor), Some(free)) if free < floor => Some(format!(
            "{name} free is {free} MB, under the {floor} MB floor"
        )),
        (Some(floor), None) => Some(format!(
            "{name} free is unknown; the {floor} MB floor fails closed"
        )),
        _ => None,
    }
}

/// The spawn rate limit (`recovery.md` R16, P48): worker sessions started in
/// this many minutes are counted.
pub const SPAWN_RATE_MINUTES: i64 = 10;
/// More than this many spawns for one project in [`SPAWN_RATE_MINUTES`] trips
/// the limit.
pub const SPAWN_RATE_PROJECT: i64 = 5;
/// More than this many spawns on one host in [`SPAWN_RATE_MINUTES`] trips the
/// limit.
pub const SPAWN_RATE_HOST: i64 = 10;

/// Refuses a spawn or a relaunch past the spawn rate, so a respawn storm
/// trips this limit and not the host's RAM, disk or process table. Returns
/// the counts it saw. Every worker session row counts, ended or failed, and
/// so a spawn, a new attempt and a relaunch in place each count once: each one
/// launched (or tried to launch) a worker process.
fn check_spawn_rate(
    conn: &Connection,
    host: &Host,
    project: &Project,
    now: Timestamp,
) -> Result<(i64, i64)> {
    let since = now.plus_minutes(-SPAWN_RATE_MINUTES);
    let project_spawns: i64 = conn.query_row(
        "SELECT count(*) FROM sessions
         WHERE project_id = ?1 AND run_id IS NOT NULL AND started_at >= ?2",
        params![project.id, since],
        |row| row.get(0),
    )?;
    if project_spawns >= SPAWN_RATE_PROJECT {
        return Err(Error::refused(
            "project",
            &project.id,
            format!(
                "spawn rate limit: {project_spawns} spawns in the last {SPAWN_RATE_MINUTES} minutes, limit {SPAWN_RATE_PROJECT}; wait for the window to pass"
            ),
        ));
    }
    let host_spawns: i64 = conn.query_row(
        "SELECT count(*) FROM sessions
         WHERE host_id = ?1 AND run_id IS NOT NULL AND started_at >= ?2",
        params![host.id, since],
        |row| row.get(0),
    )?;
    if host_spawns >= SPAWN_RATE_HOST {
        return Err(Error::refused(
            "host",
            &host.label,
            format!(
                "spawn rate limit: {host_spawns} spawns in the last {SPAWN_RATE_MINUTES} minutes, limit {SPAWN_RATE_HOST}; wait for the window to pass"
            ),
        ));
    }
    Ok((project_spawns, host_spawns))
}

fn check_spawn(
    conn: &Connection,
    host: &Host,
    project: &Project,
    checks: &SpawnChecks,
    quota_override: Option<&str>,
    now: Timestamp,
) -> Result<Value> {
    let (host_reserved, project_reserved) = check_capacity(conn, host, project)?;
    let (project_spawns, host_spawns) = check_spawn_rate(conn, host, project, now)?;
    let floor = |name: &str, floor: Option<i64>, free: Option<i64>| -> Result<()> {
        match floor_refusal(name, floor, free) {
            Some(why) => Err(Error::refused("host", &host.label, why)),
            None => Ok(()),
        }
    };
    floor("RAM", host.ram_floor_mb, checks.ram_free_mb)?;
    floor("disk", host.disk_floor_mb, checks.disk_free_mb)?;

    // While the owner is away, the contract's spend cap narrows the standing
    // limits (`authority.md`, G28): no run beyond `max_new_runs`, and a
    // quota floor when it is stricter.
    let away = active_away(conn, now)?;
    let mut away_spend = Value::Null;
    if let Some(away) = &away {
        let started = away.new_runs(conn)?;
        if let Some(cap) = away.max_new_runs
            && started >= cap
        {
            return Err(Error::refused(
                "approval",
                &away.approval_id,
                format!(
                    "away mode allows {cap} new runs and {started} have started; the owner raises the cap or returns"
                ),
            ));
        }
        away_spend = json!({
            "approval_id": away.approval_id,
            "max_new_runs": away.max_new_runs,
            "started": started,
            "quota_floor_percent": away.quota_floor_percent,
        });
    }

    let quota_floor = effective_quota_floor(checks.standing_floor(), away.as_ref());
    let mut quota = Vec::new();
    if let Some(floor) = quota_floor {
        let windows: Vec<QuotaWindow> = list(conn, "kind = 'weekly' ORDER BY label", &[])?;
        if windows.is_empty() {
            return Err(Error::refused(
                "quota",
                "weekly",
                "no weekly quota reading; the quota floor fails closed",
            ));
        }
        let oldest_allowed = now.plus_minutes(-checks.quota_max_age_minutes);
        for window in windows {
            if window.fetch_error.is_some() || window.fetched_at < oldest_allowed {
                return Err(Error::refused(
                    "quota",
                    &window.label,
                    format!(
                        "reading from {} is stale; the quota floor fails closed",
                        window.fetched_at
                    ),
                ));
            }
            match window.percent_remaining {
                Some(left) if left > floor => {}
                left => {
                    return Err(Error::refused(
                        "quota",
                        &window.label,
                        quota_floor_fact(left, floor),
                    ));
                }
            }
            quota.push(json!({"label": window.label, "percent_remaining": window.percent_remaining, "fetched_at": window.fetched_at}));
        }
    }
    let mut seen = json!({
        "host_reserved": host_reserved,
        "lane_capacity": host.lane_capacity,
        "project_reserved": project_reserved,
        "project_capacity": project.capacity,
        "project_spawns": project_spawns,
        "host_spawns": host_spawns,
        "ram_free_mb": checks.ram_free_mb,
        "ram_floor_mb": host.ram_floor_mb,
        "disk_free_mb": checks.disk_free_mb,
        "disk_floor_mb": host.disk_floor_mb,
        "quota_floor_percent": quota_floor,
        "quota": quota,
    });
    if let Some(item) = quota_override {
        seen["quota_override"] = json!(item);
    }
    if !away_spend.is_null() {
        seen["away"] = away_spend;
    }
    Ok(seen)
}

/// `run.end` inside a mutation: ends the run and releases its reservation.
pub(crate) fn end_run(tx: &mut Tx<'_>, run: &Run, end: EndRun) -> Result<Run> {
    RunStatus::from_outcome(run.outcome)
        .end(end.outcome)
        .map_err(refuse("run", &run.id))?;
    tx.conn().execute(
        "UPDATE runs SET ended_at = ?2, outcome = ?3, outcome_note = ?4, rev = rev + 1 WHERE id = ?1",
        params![run.id, tx.now(), end.outcome, end.note],
    )?;
    tx.event(
        NewEvent::new("run", &run.id, "run.ended")
            .rev(run.rev + 1)
            .card(&run.card_id)
            .payload(json!({"outcome": end.outcome, "note": end.note, "role": run.role})),
    )?;
    release_reservation(tx, run, ReservationRelease::Ended)?;
    get(tx.conn(), "run", &run.id)
}

fn next_attempt(conn: &Connection, card_id: &str, role: RunRole) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT coalesce(max(attempt), 0) + 1 FROM runs WHERE card_id = ?1 AND role = ?2",
        params![card_id, role],
        |row| row.get(0),
    )?)
}

/// The supervisor's live session id, for a wake or a note.
fn live_supervisor(conn: &Connection, run: &Run) -> Result<Option<String>> {
    Ok(find::<Session>(
        conn,
        "id = ?1 AND ended_at IS NULL",
        &[&run.supervisor_session_id],
    )?
    .map(|s| s.id))
}

impl Store {
    /// `run.start` (`tiller spawn`). Keyed. Reserves capacity, inserts the run
    /// and its placeholder session in one `BEGIN IMMEDIATE` transaction, so
    /// two spawns on a full host or project cannot both pass (invariant 6).
    pub fn run_start(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        new: NewRun,
    ) -> Result<Applied<Started>> {
        required("model", &new.model)?;
        self.write(ctx, "run.start", key, |tx| {
            let card = resolve_card(tx.conn(), &new.card)?;
            if card.state != CardState::Open {
                return Err(Error::refused("card", &card.display_id, "card is closed"));
            }
            if let Some(active) = find::<Run>(
                tx.conn(),
                "card_id = ?1 AND role = ?2 AND ended_at IS NULL",
                &[&card.id, &new.role],
            )? {
                return Err(Error::refused(
                    "card",
                    &card.display_id,
                    format!("{} run {} is already active", new.role, active.id),
                ));
            }
            let project: Project = get(tx.conn(), "project", &card.project_id)?;
            if project.retired_at.is_some() {
                return Err(Error::refused("project", &project.id, "project is retired"));
            }
            let supervisor = supervisor(tx.conn(), tx.actor(), &project.id)?;
            let host_id = match &new.host_id {
                Some(id) => id.clone(),
                None => local_host_id(tx.conn())?,
            };
            let host: Host = get(tx.conn(), "host", &host_id)?;
            let quota_override = quota_override(tx.conn(), &card, new.role, &new.checks)?;
            let seen = check_spawn(
                tx.conn(),
                &host,
                &project,
                &new.checks,
                quota_override.as_deref(),
                tx.now(),
            )?;

            let id = ids::new_ulid();
            let attempt = next_attempt(tx.conn(), &card.id, new.role)?;
            tx.conn().execute(
                "INSERT INTO runs (id, card_id, role, attempt, model, effort, host_id, worktree_path, branch,
                                   base_branch, base_sha, supervisor_session_id, started_at, config_hash,
                                   idempotency_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    id,
                    card.id,
                    new.role,
                    attempt,
                    new.model,
                    new.effort,
                    host.id,
                    new.worktree_path,
                    new.branch,
                    new.base_branch,
                    new.base_sha,
                    supervisor.id,
                    tx.now(),
                    new.config_hash,
                    tx.key(),
                ],
            )?;
            let run: Run = get(tx.conn(), "run", &id)?;
            let reservation = reserve(tx, &host, &project, &run)?;
            tx.event(
                NewEvent::new("run", &id, "run.started")
                    .rev(1)
                    .card(&card.id)
                    .payload(json!({
                        "role": new.role,
                        "attempt": attempt,
                        "model": new.model,
                        "effort": new.effort,
                        "host_id": host.id,
                        "supervisor_session_id": supervisor.id,
                        "checks": seen,
                        "dispatch": new.checks.dispatch,
                        "config_hash": new.config_hash,
                    })),
            )?;
            let session = insert_run_session(tx, &run, &project.id, new.config_hash.as_deref())?;
            Ok(Started {
                run,
                session,
                reservation,
            })
        })
    }

    /// `run.start` for a promotion (P07): the developer said go on a scout's
    /// report, so a builder run starts on the same card, in the scout's
    /// worktree and on its branch, and the card becomes a ship card. It is
    /// the execute decision again (`authority.md`, section 4 notes), so the
    /// actor, capacity and floors are `run.start`'s. Keyed.
    ///
    /// The scout run has ended `reported`, its worktree is still held (never
    /// cleaned), and no attention item is open on the card: the report's
    /// choices are answered before the work they ask for starts.
    pub fn run_promote(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        promote: Promote,
    ) -> Result<Applied<Started>> {
        required("model", &promote.model)?;
        self.write(ctx, "run.start", key, |tx| {
            let scout = load_run(tx, &promote.scout_run)?;
            if scout.role != RunRole::Scout {
                return Err(Error::refused(
                    "run",
                    &scout.id,
                    format!("only a scout is promoted; this run is a {}", scout.role),
                ));
            }
            if scout.outcome != Some(RunOutcome::Reported) {
                return Err(Error::refused(
                    "run",
                    &scout.id,
                    "a scout is promoted once it has ended reported; end it with tiller done first",
                ));
            }
            let (Some(path), Some(branch)) = (&scout.worktree_path, &scout.branch) else {
                return Err(Error::refused("run", &scout.id, "the scout has no worktree to promote in"));
            };
            let worktree: Option<Worktree> = find(
                tx.conn(),
                "host_id = ?1 AND path = ?2",
                &[&scout.host_id, path],
            )?;
            let cleaned: bool = tx.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE entity = 'run' AND entity_id = ?1 AND kind = 'run.cleaned')",
                [&scout.id],
                |row| row.get(0),
            )?;
            if cleaned || worktree.as_ref().is_none_or(|w| w.gone_at.is_some()) {
                return Err(Error::refused(
                    "run",
                    &scout.id,
                    "the scout's worktree was already returned; spawn a builder on a fresh worktree instead",
                ));
            }
            let card: Card = get(tx.conn(), "card", &scout.card_id)?;
            if card.state != CardState::Open {
                return Err(Error::refused("card", &card.display_id, "card is closed"));
            }
            // A promotion that failed after it committed left the card a
            // ship card; it may be retried while the scout's worktree is held.
            let retried: bool = tx.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE entity = 'card' AND entity_id = ?1 AND kind = 'card.promoted')",
                [&card.id],
                |row| row.get(0),
            )?;
            if card.kind != CardKind::Scout && !(card.kind == CardKind::Ship && retried) {
                return Err(Error::refused(
                    "card",
                    &card.display_id,
                    format!("only a scout card is promoted; this one is {}", card.kind),
                ));
            }
            // The report's choices are what "go" answers; any other open
            // question is not, and stays the owner's first.
            let open: Vec<Attention> = list(
                tx.conn(),
                "card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL",
                &[&card.id],
            )?;
            if let Some(other) = open.iter().find(|item| item.kind != AttentionKind::Report) {
                return Err(Error::refused(
                    "card",
                    &card.display_id,
                    format!("a {} item is open; answer it before promoting", other.kind),
                ));
            }
            if let Some(active) = find::<Run>(
                tx.conn(),
                "card_id = ?1 AND ended_at IS NULL AND (worktree_path = ?2 OR role = 'builder')",
                &[&card.id, path],
            )? {
                return Err(Error::refused(
                    "card",
                    &card.display_id,
                    format!("{} run {} is already active on the card", active.role, active.id),
                ));
            }
            let project: Project = get(tx.conn(), "project", &card.project_id)?;
            if project.retired_at.is_some() {
                return Err(Error::refused("project", &project.id, "project is retired"));
            }
            let supervisor = supervisor(tx.conn(), tx.actor(), &project.id)?;
            let host: Host = get(tx.conn(), "host", &scout.host_id)?;
            if host.id != local_host_id(tx.conn())? {
                return Err(Error::refused("host", &host.label, "promote on the host that holds the worktree"));
            }
            let quota_override =
                quota_override(tx.conn(), &card, RunRole::Builder, &promote.checks)?;
            let seen = check_spawn(
                tx.conn(),
                &host,
                &project,
                &promote.checks,
                quota_override.as_deref(),
                tx.now(),
            )?;
            for item in &open {
                withdraw_attention(tx, item, "the scout was promoted to a ship run")?;
            }

            let id = ids::new_ulid();
            let attempt = next_attempt(tx.conn(), &card.id, RunRole::Builder)?;
            tx.conn().execute(
                "INSERT INTO runs (id, card_id, role, attempt, predecessor_run_id, model, effort, host_id,
                                   worktree_path, branch, base_branch, base_sha, supervisor_session_id,
                                   started_at, idempotency_key)
                 VALUES (?1, ?2, 'builder', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    id,
                    card.id,
                    attempt,
                    scout.id,
                    promote.model,
                    promote.effort,
                    host.id,
                    path,
                    branch,
                    scout.base_branch,
                    scout.base_sha,
                    supervisor.id,
                    tx.now(),
                    tx.key(),
                ],
            )?;
            let run: Run = get(tx.conn(), "run", &id)?;
            let reservation = reserve(tx, &host, &project, &run)?;
            tx.conn().execute(
                "UPDATE cards SET kind = 'ship', rev = rev + 1 WHERE id = ?1",
                [&card.id],
            )?;
            tx.event(
                NewEvent::new("card", &card.id, "card.promoted")
                    .rev(card.rev + 1)
                    .card(&card.id)
                    .payload(json!({"from": "scout", "to": "ship", "scout_run_id": scout.id, "run_id": id})),
            )?;
            tx.event(
                NewEvent::new("run", &id, "run.started")
                    .rev(1)
                    .card(&card.id)
                    .payload(json!({
                        "role": RunRole::Builder,
                        "attempt": attempt,
                        "model": promote.model,
                        "effort": promote.effort,
                        "host_id": host.id,
                        "supervisor_session_id": supervisor.id,
                        "checks": seen,
                        "dispatch": promote.checks.dispatch,
                        "promoted_from_run_id": scout.id,
                    })),
            )?;
            let session = insert_run_session(tx, &run, &project.id, None)?;
            Ok(Started {
                run,
                session,
                reservation,
            })
        })
    }

    /// A spawn that failed after `run.start` (no worktree or pane): releases
    /// the reservation `spawn_failed`, ends the placeholder session and the
    /// run `failed`. Natural: a run already ended is returned.
    pub fn run_spawn_failed(&mut self, ctx: &Ctx, run: &str, reason: &str) -> Result<Applied<Run>> {
        required("reason", reason)?;
        self.write(ctx, "run.start", None, |tx| {
            let run = load_run(tx, run)?;
            if run.outcome.is_some() {
                return Ok(run);
            }
            release_reservation(tx, &run, ReservationRelease::SpawnFailed)?;
            if let Some(session) = live_session(tx.conn(), &run.id)? {
                end_session(tx, &session, "spawn_failed")?;
            }
            end_run(
                tx,
                &run,
                EndRun {
                    outcome: RunOutcome::Failed,
                    note: Some(format!("spawn failed: {reason}")),
                },
            )
        })
    }

    /// `run.place` (`tiller spawn`): records the worktree, branch, base and
    /// fragment hash on the run and the pane on its session. A value already
    /// recorded never changes: a run's worktree and pane do not move, and a
    /// relaunch is a new session. Natural: the same values write nothing.
    pub fn run_place(&mut self, ctx: &Ctx, run: &str, place: Placement) -> Result<Applied<Run>> {
        self.write(ctx, "run.place", None, |tx| {
            let run = load_run(tx, run)?;
            match tx.actor() {
                Actor::Owner | Actor::Board | Actor::Daemon => {}
                Actor::Session(id) if *id == run.supervisor_session_id => {}
                other => {
                    return Err(Error::Forbidden(format!(
                        "{other} may not place run {}; its supervisor or the owner does",
                        run.id
                    )));
                }
            }
            require_active(&run)?;
            let session = live_session(tx.conn(), &run.id)?;
            let fields = [
                ("worktree_path", &run.worktree_path, &place.worktree_path),
                ("branch", &run.branch, &place.branch),
                ("base_branch", &run.base_branch, &place.base_branch),
                ("base_sha", &run.base_sha, &place.base_sha),
                ("config_hash", &run.config_hash, &place.config_hash),
            ];
            let mut set = serde_json::Map::new();
            for (column, old, new) in fields {
                match (old, new) {
                    (_, None) => {}
                    (Some(old), Some(new)) if old == new => {}
                    (Some(old), Some(new)) => {
                        return Err(Error::refused(
                            "run",
                            &run.id,
                            format!("{column} is already {old}; it cannot become {new}"),
                        ));
                    }
                    (None, Some(new)) => {
                        set.insert(column.to_owned(), json!(new));
                    }
                }
            }
            let mut pane = None;
            if let Some(new) = &place.pane_id {
                let session = session.as_ref().ok_or_else(|| {
                    Error::refused(
                        "run",
                        &run.id,
                        "the run has no live session to bind a pane to",
                    )
                })?;
                match &session.pane_id {
                    Some(old) if old == new => {}
                    Some(old) => {
                        return Err(Error::refused(
                            "session",
                            &session.id,
                            format!("pane is already {old}; it cannot become {new}"),
                        ));
                    }
                    None => pane = Some((session, new)),
                }
            }
            if set.is_empty() && pane.is_none() {
                return Ok(run);
            }
            for (column, value) in &set {
                // The column names come from the fixed list above.
                tx.conn().execute(
                    &format!("UPDATE runs SET {column} = ?2 WHERE id = ?1"),
                    params![run.id, value.as_str()],
                )?;
            }
            if !set.is_empty() {
                tx.conn()
                    .execute("UPDATE runs SET rev = rev + 1 WHERE id = ?1", [&run.id])?;
            }
            if let Some(hash) = set.get("config_hash").and_then(Value::as_str)
                && let Some(session) = &session
            {
                tx.conn().execute(
                    "UPDATE sessions SET config_hash = ?2 WHERE id = ?1 AND config_hash IS NULL",
                    params![session.id, hash],
                )?;
            }
            if let Some((session, pane_id)) = &pane {
                tx.conn().execute(
                    "UPDATE sessions SET pane_id = ?2, rev = rev + 1 WHERE id = ?1",
                    params![session.id, pane_id],
                )?;
                set.insert("pane_id".into(), json!(pane_id));
                set.insert("session_id".into(), json!(session.id));
            }
            let placed: Run = get(tx.conn(), "run", &run.id)?;
            tx.event(
                NewEvent::new("run", &run.id, "run.placed")
                    .rev(placed.rev)
                    .card(&run.card_id)
                    .payload(Value::Object(set)),
            )?;
            Ok(placed)
        })
    }

    /// `run.reconfigure` (a same-run relaunch): moves the run and its live
    /// session onto the hash of launch files rendered again, since
    /// `run.place` never changes a hash it holds. Ended sessions keep the
    /// hash they ran with. Natural: the same hash writes nothing.
    pub fn run_reconfigure(
        &mut self,
        ctx: &Ctx,
        run: &str,
        config_hash: &str,
    ) -> Result<Applied<Run>> {
        required("config_hash", config_hash)?;
        self.write(ctx, "run.reconfigure", None, |tx| {
            let run = load_run(tx, run)?;
            match tx.actor() {
                Actor::Owner | Actor::Board | Actor::Daemon => {}
                Actor::Session(id) if *id == run.supervisor_session_id => {}
                other => {
                    return Err(Error::Forbidden(format!(
                        "{other} may not reconfigure run {}; its supervisor or the owner does",
                        run.id
                    )));
                }
            }
            require_active(&run)?;
            if run.config_hash.as_deref() == Some(config_hash) {
                return Ok(run);
            }
            tx.conn().execute(
                "UPDATE runs SET config_hash = ?2, rev = rev + 1 WHERE id = ?1",
                params![run.id, config_hash],
            )?;
            if let Some(session) = live_session(tx.conn(), &run.id)? {
                tx.conn().execute(
                    "UPDATE sessions SET config_hash = ?2 WHERE id = ?1",
                    params![session.id, config_hash],
                )?;
            }
            let reconfigured: Run = get(tx.conn(), "run", &run.id)?;
            tx.event(
                NewEvent::new("run", &run.id, "run.reconfigured")
                    .rev(reconfigured.rev)
                    .card(&run.card_id)
                    .payload(json!({"from": run.config_hash, "to": config_hash})),
            )?;
            Ok(reconfigured)
        })
    }

    /// `run.end` (`tiller done`, reconcile). `landed` needs a succeeded
    /// approval or a confirmed outside merge; `reported` for a builder on a
    /// ship card needs its committed head on an open PR of the card; a
    /// session ending its own run needs a known `unlanded_commits` for its
    /// worktree (P51).
    pub fn run_end(&mut self, ctx: &Ctx, run: &str, end: EndRun) -> Result<Applied<Run>> {
        self.write(ctx, "run.end", None, |tx| {
            let run = load_run(tx, run)?;
            if run.outcome == Some(end.outcome) {
                return Ok(run);
            }
            require_active(&run)?;
            let card: Card = get(tx.conn(), "card", &run.card_id)?;
            match end.outcome {
                RunOutcome::Landed if !landed_evidence(tx.conn(), &card)? => {
                    return Err(Error::refused(
                        "run",
                        &run.id,
                        "landed comes from a finished merge or an outside merge the owner confirmed",
                    ));
                }
                RunOutcome::Reported
                    if card.kind == CardKind::Ship
                        && run.role == RunRole::Builder
                        && published_head(tx.conn(), &run)?.is_none() =>
                {
                    return Err(Error::refused(
                        "run",
                        &run.id,
                        "a builder on a ship card ends reported only once its committed head is on an open PR; otherwise landed with the merge, or failed or abandoned",
                    ));
                }
                _ => {}
            }
            if let (Actor::Session(_), Some(path)) = (tx.actor(), &run.worktree_path) {
                let known: Option<Option<i64>> = tx
                    .conn()
                    .query_row(
                        "SELECT unlanded_commits FROM worktrees WHERE host_id = ?1 AND path = ?2",
                        params![run.host_id, path],
                        |row| row.get(0),
                    )
                    .map(Some)
                    .or_else(|error| match error {
                        rusqlite::Error::QueryReturnedNoRows => Ok(None),
                        other => Err(other),
                    })?;
                if !matches!(known, Some(Some(_))) {
                    return Err(Error::refused(
                        "run",
                        &run.id,
                        format!("unlanded commits in {path} are unknown; refresh the worktree first"),
                    ));
                }
            }
            end_run(tx, &run, end)
        })
    }

    /// `session.start` for an orchestrator (`tiller orchestrator start`).
    /// Natural: the live orchestrator with the same Claude session id is
    /// returned. Enqueues one note per unresolved ask of the project.
    pub fn session_start(
        &mut self,
        ctx: &Ctx,
        new: NewOrchestrator,
    ) -> Result<Applied<OrchestratorStarted>> {
        self.write(ctx, "session.start", None, |tx| {
            let project: Project = get(tx.conn(), "project", &new.project_id)?;
            if project.retired_at.is_some() {
                return Err(Error::refused("project", &project.id, "project is retired"));
            }
            if let Some(live) = live_orchestrator(tx.conn(), &project.id)? {
                if new.claude_session_id.is_some() && live.claude_session_id == new.claude_session_id {
                    return Ok(OrchestratorStarted {
                        session: live,
                        notes: Vec::new(),
                    });
                }
                return Err(Error::refused(
                    "project",
                    &project.id,
                    format!("orchestrator session {} is live", live.id),
                ));
            }
            let id = match &new.id {
                Some(id) if ids::is_ulid(id) => id.clone(),
                Some(id) => return Err(Error::Usage(format!("session id {id:?} is not a ULID"))),
                None => ids::new_ulid(),
            };
            if let Some(claude_id) = &new.claude_session_id {
                take_claude_id(tx, claude_id, &id)?;
            }
            // Open cards filed since the last orchestrator ended (or ever),
            // by a principal that was not an ask, which nobody was told of.
            let gap_start: Option<String> = tx.conn().query_row(
                "SELECT max(ended_at) FROM sessions WHERE project_id = ?1 AND role = 'orchestrator'",
                [&project.id],
                |row| row.get(0),
            )?;
            let unannounced: Vec<Card> = list(
                tx.conn(),
                "project_id = ?1 AND state = 'open' AND created_by_ask_id IS NULL AND created_at > coalesce(?2, '')
                 AND NOT EXISTS (SELECT 1 FROM outbox WHERE outbox.card_id = cards.id AND outbox.kind = 'note' AND outbox.body LIKE 'new card %')
                 ORDER BY created_at, id",
                &[&project.id, &gap_start],
            )?;
            let host = local_host_id(tx.conn())?;
            tx.conn().execute(
                "INSERT INTO sessions (id, claude_session_id, role, project_id, host_id, pane_id, transcript_path,
                                       harness, config_hash, started_at)
                 VALUES (?1, ?2, 'orchestrator', ?3, ?4, ?5, ?6, coalesce(?7, 'claude'), ?8, ?9)",
                params![
                    id,
                    new.claude_session_id,
                    project.id,
                    host,
                    new.pane_id,
                    new.transcript_path,
                    new.harness,
                    new.config_hash,
                    tx.now(),
                ],
            )?;
            tx.event(
                NewEvent::new("session", &id, "session.started")
                    .rev(1)
                    .payload(json!({"role": "orchestrator", "project_id": project.id})),
            )?;
            let asks: Vec<Ask> = list(
                tx.conn(),
                "project_id = ?1 AND disposition = 'unresolved' ORDER BY received_at, id",
                &[&project.id],
            )?;
            let mut notes = Vec::with_capacity(asks.len());
            for ask in asks {
                notes.push(enqueue(
                    tx,
                    &NewMessage {
                        target_session_id: id.clone(),
                        kind: OutboxKind::Note,
                        body: format!("unresolved ask {}: {}", ids::short(&ask.id), ask.text),
                        card_id: None,
                        reply_to: Some(ask.id),
                    },
                )?);
            }
            for card in unannounced {
                notes.push(enqueue(
                    tx,
                    &NewMessage {
                        target_session_id: id.clone(),
                        kind: OutboxKind::Note,
                        body: new_card_note(&card),
                        card_id: Some(card.id.clone()),
                        reply_to: None,
                    },
                )?);
            }
            Ok(OrchestratorStarted {
                session: get(tx.conn(), "session", &id)?,
                notes,
            })
        })
    }

    /// `run.bind_session` (`SessionStart` hook). Rebinding to a new Claude id
    /// (`clear`, `resume` with a fork) keeps the row and puts the old id in
    /// the event. Natural: the same id and pane write nothing.
    pub fn run_bind_session(&mut self, ctx: &Ctx, bind: Bind) -> Result<Applied<Session>> {
        required("claude session id", &bind.claude_session_id)?;
        self.write(ctx, "run.bind_session", None, |tx| {
            let session = load_session(tx, &bind.session_id)?;
            if session.ended_at.is_some() {
                return Err(Error::refused("session", &session.id, "session has ended"));
            }
            let card = match &session.run_id {
                Some(run) => {
                    let run: Run = get(tx.conn(), "run", run)?;
                    require_active(&run)?;
                    Some(run.card_id)
                }
                None => None,
            };
            let pane = bind.pane_id.clone().or_else(|| session.pane_id.clone());
            let transcript = bind.transcript_path.clone().or_else(|| session.transcript_path.clone());
            if session.claude_session_id.as_deref() == Some(bind.claude_session_id.as_str())
                && session.pane_id == pane
                && session.transcript_path == transcript
            {
                return Ok(session);
            }
            take_claude_id(tx, &bind.claude_session_id, &session.id)?;
            tx.conn().execute(
                "UPDATE sessions SET claude_session_id = ?2, pane_id = ?3, transcript_path = ?4, rev = rev + 1
                 WHERE id = ?1",
                params![session.id, bind.claude_session_id, pane, transcript],
            )?;
            tx.event(
                NewEvent::new("session", &session.id, "session.bound")
                    .rev(session.rev + 1)
                    .card_opt(card)
                    .payload(json!({
                        "claude_session_id": bind.claude_session_id,
                        "previous_claude_session_id": session.claude_session_id,
                        "pane_id": pane,
                        "source": bind.source,
                    })),
            )?;
            get(tx.conn(), "session", &session.id)
        })
    }

    /// `session.end` (`SessionEnd` hook, reconcile). Natural: an ended
    /// session is returned.
    pub fn session_end(
        &mut self,
        ctx: &Ctx,
        session: &str,
        reason: &str,
    ) -> Result<Applied<Session>> {
        required("reason", reason)?;
        self.write(ctx, "session.end", None, |tx| {
            let session = load_session(tx, session)?;
            if session.ended_at.is_some() {
                return Ok(session);
            }
            end_session(tx, &session, reason)
        })
    }

    /// `session.resume`, the attempt half: the daemon (reconcile) or
    /// `tiller agent resume` is about to launch `claude --resume`; the hook's
    /// bind or end completes it. The daemon, the owner and the run's
    /// supervisor may; a session is resumed only while its row is live, and
    /// a worker's only while its run is active with a worktree.
    pub fn session_resume_attempted(
        &mut self,
        ctx: &Ctx,
        session: &str,
        readings: Value,
    ) -> Result<Applied<Session>> {
        self.write(ctx, "session.resume", None, |tx| {
            let session = load_session(tx, session)?;
            if session.ended_at.is_some() {
                return Err(Error::refused("session", &session.id, "session has ended"));
            }
            let card = match &session.run_id {
                Some(run) => {
                    let run: Run = get(tx.conn(), "run", run)?;
                    match tx.actor() {
                        Actor::Owner | Actor::Board | Actor::Daemon => {}
                        Actor::Session(id) if *id == run.supervisor_session_id => {}
                        other => {
                            return Err(Error::Forbidden(format!(
                                "{other} may not resume session {}; its run's supervisor, the owner or the daemon does",
                                session.id
                            )));
                        }
                    }
                    require_active(&run)?;
                    if run.worktree_path.is_none() {
                        return Err(Error::refused("run", &run.id, "run has no worktree to resume in"));
                    }
                    Some(run.card_id)
                }
                None => {
                    require_daemon(tx.actor(), "session.resume of an orchestrator")?;
                    None
                }
            };
            tx.event(
                NewEvent::new("session", &session.id, "session.resume_attempted")
                    .card_opt(card)
                    .payload(
                        json!({"claude_session_id": session.claude_session_id, "checks": readings}),
                    ),
            )?;
            Ok(session)
        })
    }

    /// `run.status` (`tiller status`): copies the line onto the run, appends a
    /// `status_line` observation and wakes the supervisor.
    pub fn run_status(
        &mut self,
        ctx: &Ctx,
        run: &str,
        state: StatusState,
        line: &str,
    ) -> Result<Applied<Run>> {
        required("status line", line)?;
        self.write(ctx, "run.status", None, |tx| {
            let run = load_run(tx, run)?;
            require_active(&run)?;
            let text = format!("{state}: {line}");
            tx.conn().execute(
                "UPDATE runs SET last_status_line = ?2, last_status_at = ?3, rev = rev + 1 WHERE id = ?1",
                params![run.id, text, tx.now()],
            )?;
            let session = live_session(tx.conn(), &run.id)?.map(|s| s.id);
            tx.conn().execute(
                "INSERT INTO observations (id, session_id, run_id, source, event, observed_at, recorded_at, payload)
                 VALUES (?1, ?2, ?3, 'status_line', 'status', ?4, ?4, ?5)",
                params![ids::new_ulid(), session, run.id, tx.now(), json!({"state": state, "line": line})],
            )?;
            tx.event(
                NewEvent::new("run", &run.id, "run.status")
                    .rev(run.rev + 1)
                    .card(&run.card_id)
                    .payload(json!({"state": state, "line": line})),
            )?;
            if let Some(supervisor) = live_supervisor(tx.conn(), &run)? {
                let card: Card = get(tx.conn(), "card", &run.card_id)?;
                enqueue(
                    tx,
                    &NewMessage {
                        target_session_id: supervisor,
                        kind: OutboxKind::Wake,
                        body: format!("[{}] {} {text}", card.display_id, run.role.short_name()),
                        card_id: Some(card.id),
                        reply_to: None,
                    },
                )?;
            }
            get(tx.conn(), "run", &run.id)
        })
    }

    /// `run.pause`: a dated worker pause that silences alarms until then,
    /// with why, when one is given. Natural: the same date and reason write
    /// nothing.
    pub fn run_pause(
        &mut self,
        ctx: &Ctx,
        run: &str,
        until: Timestamp,
        reason: Option<&str>,
    ) -> Result<Applied<Run>> {
        let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
        self.write(ctx, "run.pause", None, |tx| {
            let run = load_run(tx, run)?;
            require_active(&run)?;
            if run.pause_until == Some(until)
                && pause_reason(tx.conn(), &run.id)?.as_deref() == reason
            {
                return Ok(run);
            }
            if until <= tx.now() {
                return Err(Error::refused(
                    "run",
                    &run.id,
                    format!("{until} is not in the future"),
                ));
            }
            tx.conn().execute(
                "UPDATE runs SET pause_until = ?2, rev = rev + 1 WHERE id = ?1",
                params![run.id, until],
            )?;
            tx.event(
                NewEvent::new("run", &run.id, "run.paused")
                    .rev(run.rev + 1)
                    .card(&run.card_id)
                    .payload(json!({"until": until, "reason": reason})),
            )?;
            get(tx.conn(), "run", &run.id)
        })
    }

    /// `run.unpause`: a manual resume (`tiller agent resume`, the board's
    /// Resume) clears the run's dated pause, so its alarms count again.
    /// Natural: a run with no pause writes nothing.
    pub fn run_unpause(&mut self, ctx: &Ctx, run: &str) -> Result<Applied<Run>> {
        self.write(ctx, "run.unpause", None, |tx| {
            let run = load_run(tx, run)?;
            require_active(&run)?;
            let Some(until) = run.pause_until else {
                return Ok(run);
            };
            tx.conn().execute(
                "UPDATE runs SET pause_until = NULL, rev = rev + 1 WHERE id = ?1",
                params![run.id],
            )?;
            tx.event(
                NewEvent::new("run", &run.id, "run.unpaused")
                    .rev(run.rev + 1)
                    .card(&run.card_id)
                    .payload(json!({"until": until})),
            )?;
            get(tx.conn(), "run", &run.id)
        })
    }

    /// `run.reown` (reconcile): hands the run to the project's live
    /// orchestrator. Natural: a run it already supervises is returned.
    pub fn run_reown(&mut self, ctx: &Ctx, run: &str) -> Result<Applied<Run>> {
        self.write(ctx, "run.reown", None, |tx| {
            let run = load_run(tx, run)?;
            require_active(&run)?;
            let card: Card = get(tx.conn(), "card", &run.card_id)?;
            let Some(live) = live_orchestrator(tx.conn(), &card.project_id)? else {
                return Err(Error::refused(
                    "project",
                    &card.project_id,
                    "no live orchestrator",
                ));
            };
            if live.id == run.supervisor_session_id {
                return Ok(run);
            }
            tx.conn().execute(
                "UPDATE runs SET supervisor_session_id = ?2, rev = rev + 1 WHERE id = ?1",
                params![run.id, live.id],
            )?;
            tx.event(
                NewEvent::new("run", &run.id, "run.reowned")
                    .rev(run.rev + 1)
                    .card(&run.card_id)
                    .payload(json!({"from": run.supervisor_session_id, "to": live.id})),
            )?;
            get(tx.conn(), "run", &run.id)
        })
    }

    /// `run.relaunch` (`tiller agent relaunch`): ends the run's live session,
    /// proven stopped, and starts a new session on the same run or a new
    /// attempt that names this run as its predecessor.
    pub fn run_relaunch(
        &mut self,
        ctx: &Ctx,
        run: &str,
        relaunch: Relaunch,
    ) -> Result<Applied<Relaunched>> {
        required("note", &relaunch.note)?;
        if relaunch.model.is_some() && !relaunch.new_attempt {
            return Err(Error::Usage(
                "a model goes with a new attempt; a relaunch on the same run keeps its model"
                    .into(),
            ));
        }
        self.write(ctx, "run.relaunch", None, |tx| {
            relaunch_in(tx, run, &relaunch)
        })
    }

    /// Runs `run.relaunch` with a stop assumed proven, then rolls it back: a
    /// relaunch checks every precondition before it stops the live session,
    /// so a refusal never leaves an agent killed and nothing recorded.
    pub fn run_relaunch_rehearse(
        &mut self,
        ctx: &Ctx,
        run: &str,
        relaunch: &Relaunch,
    ) -> Result<()> {
        let relaunch = Relaunch {
            stop_proof: Some("rehearsal".into()),
            ..relaunch.clone()
        };
        self.rehearse(ctx, "run.relaunch", |tx| {
            relaunch_in(tx, run, &relaunch).map(drop)
        })
    }

    /// `run.clean`: the run ended, its worktree has no unlanded commits and
    /// no uncommitted changes by a reading taken after it ended, and its pane
    /// is closed. Marks the cache
    /// rows gone. Natural: a cleaned worktree is returned unchanged.
    pub fn run_clean(&mut self, ctx: &Ctx, run: &str, pane_closed: bool) -> Result<Applied<Run>> {
        self.write(ctx, "run.clean", None, |tx| {
            let run = load_run(tx, run)?;
            let Some(ended_at) = run.ended_at else {
                return Err(Error::refused("run", &run.id, "run is active"));
            };
            let Some(path) = run.worktree_path.clone() else {
                return Err(Error::refused("run", &run.id, "run has no worktree"));
            };
            let worktree: Worktree = find(
                tx.conn(),
                "host_id = ?1 AND path = ?2",
                &[&run.host_id, &path],
            )?
            .ok_or_else(|| Error::refused("worktree", &path, "no reading of the worktree"))?;
            // A relaunch in place (`--new-attempt`) hands the worktree to a
            // successor. Checked here, in the transaction that marks the
            // worktree gone, so no successor can start between the check and
            // the write.
            if let Some(successor) = find::<Run>(
                tx.conn(),
                "host_id = ?1 AND worktree_path = ?2 AND ended_at IS NULL AND id != ?3",
                &[&run.host_id, &path, &run.id],
            )? {
                return Err(Error::refused(
                    "worktree",
                    &path,
                    format!("the worktree is in use by active run {}", successor.id),
                ));
            }
            if worktree.gone_at.is_some() {
                return Ok(run);
            }
            if worktree.fetched_at < ended_at {
                return Err(Error::refused(
                    "worktree",
                    &path,
                    "the reading predates the run's end",
                ));
            }
            if worktree.unlanded_commits != Some(0) {
                return Err(Error::refused(
                    "worktree",
                    &path,
                    format!("unlanded commits: {:?}", worktree.unlanded_commits),
                ));
            }
            // Uncommitted changes are unlanded work too (`recovery.md` R13).
            if worktree.dirty != Some(false) {
                return Err(Error::refused(
                    "worktree",
                    &path,
                    format!("uncommitted changes: {:?}", worktree.dirty),
                ));
            }
            if !pane_closed {
                return Err(Error::refused(
                    "run",
                    &run.id,
                    "the run's pane is still alive",
                ));
            }
            tx.conn().execute(
                "UPDATE worktrees SET gone_at = ?3 WHERE host_id = ?1 AND path = ?2",
                params![run.host_id, path, tx.now()],
            )?;
            tx.conn().execute(
                &format!(
                    "UPDATE panes SET gone_at = ?2 WHERE gone_at IS NULL AND {}",
                    run_panes(1)
                ),
                params![run.id, tx.now()],
            )?;
            tx.event(
                NewEvent::new("run", &run.id, "run.cleaned")
                    .card(&run.card_id)
                    .payload(json!({"worktree_path": path})),
            )?;
            Ok(run)
        })
    }

    /// `observation.record`: appends the observation and moves the session's
    /// activity when it is newer. Only a verdict change is an event.
    pub fn observation_record(
        &mut self,
        ctx: &Ctx,
        new: NewObservation,
    ) -> Result<Applied<Recorded>> {
        self.observation_record_keyed(ctx, None, new)
    }

    /// [`Store::observation_record`] under an idempotency key, so a hook
    /// delivered twice is written once (`protocol.md` section 4.1).
    pub fn observation_record_keyed(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        new: NewObservation,
    ) -> Result<Applied<Recorded>> {
        required("event", &new.event)?;
        self.write(ctx, "observation.record", key, |tx| {
            tx.rebuildable();
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO observations (id, session_id, run_id, source, event, verdict, observed_at, recorded_at, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    new.session_id,
                    new.run_id,
                    new.source,
                    new.event,
                    new.verdict,
                    new.observed_at,
                    tx.now(),
                    new.payload,
                ],
            )?;
            let mut verdict_changed = false;
            if let (Some(session_id), Some(verdict)) = (&new.session_id, new.verdict) {
                let session: Session = get(tx.conn(), "session", session_id)?;
                let newer = session.activity_observed_at.is_none_or(|at| new.observed_at > at);
                if newer && !herdr_idle_held(&session, &new) {
                    tx.conn().execute(
                        "UPDATE sessions SET activity = ?2, activity_observed_at = ?3, activity_source = ?4 WHERE id = ?1",
                        params![session.id, verdict, new.observed_at, new.source],
                    )?;
                    if session.activity != verdict {
                        verdict_changed = true;
                        let card = match &session.run_id {
                            Some(run) => Some(get::<Run>(tx.conn(), "run", run)?.card_id),
                            None => None,
                        };
                        tx.event(
                            NewEvent::new("session", &session.id, "observation.recorded")
                                .card_opt(card)
                                .payload(json!({
                                    "observation_id": id,
                                    "from": session.activity,
                                    "to": verdict,
                                    "source": new.source,
                                    "event": new.event,
                                })),
                        )?;
                    }
                }
            }
            Ok(Recorded {
                observation: get(tx.conn(), "observation", &id)?,
                verdict_changed,
            })
        })
    }
}

/// Why the run's newest pause was taken, when it said.
pub fn pause_reason(conn: &Connection, run: &str) -> Result<Option<String>> {
    let reason = conn
        .query_row(
            "SELECT json_extract(payload, '$.reason') FROM events
             WHERE entity = 'run' AND entity_id = ?1 AND kind = 'run.paused'
             ORDER BY seq DESC LIMIT 1",
            [run],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(reason.flatten())
}
