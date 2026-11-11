//! `tiller agent interrupt`, `resume`, `relaunch` and `stop` (T2.5, T4.5;
//! `protocol.md` 6.4), and the daemon's stop loop for finished runs: the
//! herdr half of agent control, over the same herdr adapter and launch
//! files spawn uses.
//!
//! Each command says what it saw, never what it hoped: an interrupt is
//! `confirmed` only when the transcript shows the interrupt marker or a hook
//! reports the turn ended, and a launch only when the new session's
//! `SessionStart` hook arrives. Busy state is never rewritten as proof
//! (Firstmate's `agent-control`).
//!
//! None of them deletes work. A relaunch keeps the worktree, its
//! commits and uncommitted work, the run row and its history: the old
//! session's row is ended, a new one starts in the same worktree, and a new
//! attempt ends the old run `relaunched` beside the new one. A stop returns
//! a worktree only through `run.clean`, which keeps one that holds anything.

mod project;
#[cfg(test)]
mod tests;

pub use project::{
    AgentsStopped, ProjectDeleted, ProjectRetired, StoppedAgent, delete_project, retire_project,
};

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use tiller_core::domain::{
    Activity, Card, Delivery, Effort, Host, ObservationSource, Project, Run, RunRole, Session,
};
use tiller_core::state::outbox::live_orchestrator;
use tiller_core::state::runs::{
    AttemptModel, Bind, NewObservation, Placement, Relaunch, SpawnChecks, floor_refusal,
    live_session,
};
use tiller_core::state::{get, list};
use tiller_core::{Actor, Applied, Ctx, Store, Timestamp};
use tiller_herdr::client::{Herdr, PaneInfo};
use tiller_herdr::composer::{self, Composer};
use tiller_herdr::layout;
use tiller_herdr::submit::{self, Pacing, Submit};
use tiller_observe::tailer;
use tiller_reconcile::recovery::sessions::{LAUNCH_BIND_SOURCE, MANUAL_RESUME_REASON};

use crate::spawn::launch::{self, AgainSpec, LaunchSpec, Start};
use crate::spawn::memory;
use crate::spawn::{
    Cleaned, Result, SpawnError, World, cleanup, delivery_line, dispatch, local_host, project_of,
    quota_floor, relayout, require_local, role_of, scan_and_record,
};

/// How long each command waits for its proof, and how often it looks.
#[derive(Clone, Copy, Debug)]
pub struct Waits {
    /// For an interrupt's marker or turn end (`protocol.md` 6.4: 10 s).
    pub interrupt: Duration,
    /// For a session to leave after `/exit`.
    pub exit: Duration,
    /// For a launched session's `SessionStart` hook.
    pub bind: Duration,
    pub poll: Duration,
    /// The pauses around the Enter that submits a line typed into Claude.
    pub pacing: Pacing,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            interrupt: Duration::from_secs(10),
            exit: Duration::from_secs(15),
            bind: Duration::from_secs(30),
            poll: Duration::from_millis(250),
            pacing: Pacing::default(),
        }
    }
}

/// Whether a command saw its effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Proof {
    Confirmed,
    Unconfirmed,
}

/// What `tiller agent interrupt` did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Interrupted {
    pub run: String,
    pub session: String,
    pub pane_id: String,
    pub cancel: Proof,
    /// What the proof is, or what was seen instead.
    pub evidence: String,
}

/// What `tiller agent resume` did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Resumed {
    pub run: Run,
    pub session: Session,
    pub pane_id: Option<String>,
    /// The session was already running in its pane, so nothing was done.
    pub left_alone: bool,
    /// `None` when the session was left alone.
    pub started: Option<Proof>,
    pub evidence: String,
    pub warning: Option<String>,
}

/// What a relaunch is asked for.
#[derive(Clone, Debug, Default)]
pub struct RelaunchRequest {
    /// The progress note the new session starts from (P24).
    pub note: String,
    pub new_attempt: bool,
    /// A new attempt's brief.
    pub brief: Option<String>,
    /// A new attempt's own `--model --effort`, which may only raise the rule.
    pub asked: Option<(String, Effort)>,
}

/// What `tiller agent relaunch` did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Relaunched {
    pub run: Run,
    pub session: Session,
    pub ended_session: Option<String>,
    /// How the old session was proven stopped; `None` when none was live.
    pub stop_proof: Option<String>,
    pub pane_id: String,
    pub started: Proof,
    pub evidence: String,
    pub warning: Option<String>,
}

/// A relaunch, or the replay of a key that already relaunched.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RelaunchOutcome {
    Relaunched(Box<Relaunched>),
    Replayed {
        run: Box<Run>,
        session: Box<Session>,
    },
}

/// The run's supervisor and the owner control its agent; resume also lets
/// the daemon (`authority.md` role matrix). The store checks again.
fn require_controller(ctx: &Ctx, run: &Run, what: &str, daemon: bool) -> Result<()> {
    match &ctx.actor {
        Actor::Owner | Actor::Board => Ok(()),
        Actor::Daemon if daemon => Ok(()),
        Actor::Session(id) if *id == run.supervisor_session_id => Ok(()),
        other => Err(tiller_core::Error::Forbidden(format!(
            "{other} may not {what} run {}; its supervisor or the owner does",
            run.id
        ))
        .into()),
    }
}

fn require_active(run: &Run) -> Result<()> {
    match run.ended_at {
        None => Ok(()),
        Some(_) => Err(SpawnError::Refused(format!("run {} has ended", run.id))),
    }
}

/// The run's worktree, which must still be on disk (R15).
fn worktree_of(run: &Run) -> Result<PathBuf> {
    let path = run
        .worktree_path
        .as_deref()
        .map(PathBuf::from)
        .ok_or_else(|| SpawnError::Refused(format!("run {} has no worktree", run.id)))?;
    if !path.is_dir() {
        return Err(SpawnError::Refused(format!(
            "run {}'s worktree {} is gone; nothing was changed",
            run.id,
            path.display()
        )));
    }
    Ok(path)
}

/// Sends Escape to a working session and reports whether its turn ended.
/// Refuses unless the session is working: a second Escape would reach
/// whatever the agent does next. For the same reason a key that already
/// sent one (the board's, kept by the `interrupt_sent` observation) is
/// refused, and nothing is sent again.
pub fn interrupt(
    store: &mut Store,
    ctx: &Ctx,
    herdr: &dyn Herdr,
    waits: &Waits,
    run: &Run,
) -> Result<Applied<Interrupted>> {
    require_controller(ctx, run, "interrupt", false)?;
    if store.key_recorded(ctx)? {
        return Err(SpawnError::Refused(format!(
            "this interrupt of run {} was sent already; nothing was sent again",
            run.id
        )));
    }
    require_local(store, run)?;
    require_active(run)?;
    let session = live_session(store.conn(), &run.id)?.ok_or_else(|| {
        SpawnError::Refused(format!(
            "run {} has no live session to interrupt; tiller agent relaunch starts one",
            run.id
        ))
    })?;
    let pane_id = session
        .pane_id
        .clone()
        .ok_or_else(|| SpawnError::Refused(format!("session {} has no pane bound", session.id)))?;
    let pane = herdr
        .pane(&pane_id)?
        .ok_or_else(|| SpawnError::Refused(format!("pane {pane_id} is gone; nothing was sent")))?;
    if pane.agent.is_none() {
        return Err(SpawnError::Refused(format!(
            "pane {pane_id} shows no agent; nothing was sent"
        )));
    }
    // herdr's word wins; the recorded activity counts only without one.
    let working = match pane.agent_status.as_deref() {
        Some(status) => status == "working",
        None => session.activity == Activity::Working,
    };
    if !working {
        return Err(SpawnError::Refused(format!(
            "session {} is not working (herdr: {}, recorded: {}); a second Escape would reach whatever it does next, so nothing was sent",
            session.id,
            pane.agent_status.as_deref().unwrap_or("no status"),
            session.activity
        )));
    }

    let transcript = session.transcript_path.as_deref().map(PathBuf::from);
    let offset = transcript
        .as_deref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map_or(0, |meta| meta.len());
    let since = Timestamp::now();
    herdr.send_escape(&pane_id)?;
    let recorded = store.observation_record(
        ctx,
        NewObservation {
            session_id: Some(session.id.clone()),
            run_id: Some(run.id.clone()),
            source: ObservationSource::Herdr,
            event: "interrupt_sent".into(),
            verdict: None,
            observed_at: since,
            payload: json!({"pane_id": pane_id}),
        },
    )?;

    let seen = wait(waits, waits.interrupt, || {
        if let Some(event) = events_since(store, &session.id, since)?
            .into_iter()
            .find(|e| e == "interrupt" || e == "Stop" || e.starts_with("StopFailure:"))
        {
            return Ok(Some(format!("observation {event} after the Escape")));
        }
        Ok(transcript
            .as_deref()
            .filter(|path| interrupt_in_transcript(path, offset))
            .map(|_| "the transcript shows the interrupt marker".to_owned()))
    })?;
    let (cancel, evidence) = match seen {
        Some(evidence) => (Proof::Confirmed, evidence),
        None => (
            Proof::Unconfirmed,
            format!(
                "no interrupt marker or turn end within {} s; herdr now shows {}",
                waits.interrupt.as_secs(),
                herdr_view(herdr, &pane_id)
            ),
        ),
    };
    Ok(Applied {
        value: Interrupted {
            run: run.id.clone(),
            session: session.id,
            pane_id,
            cancel,
            evidence,
        },
        events: recorded.events,
        replayed: false,
    })
}

/// Resumes the run's live session with `claude --resume <id>` in its
/// worktree, in its own pane when that pane is still open. A session herdr
/// shows running is left alone.
pub fn resume(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    waits: &Waits,
    run: &Run,
) -> Result<Applied<Resumed>> {
    require_controller(ctx, run, "resume", true)?;
    require_local(store, run)?;
    require_active(run)?;
    let worktree = worktree_of(run)?;
    let session = live_session(store.conn(), &run.id)?.ok_or_else(|| {
        SpawnError::Refused(format!(
            "run {} has no live session to resume: the last one ended, so start a new one with tiller agent relaunch {} --note <progress>",
            run.id, run.id
        ))
    })?;
    let claude_id = session.claude_session_id.clone().ok_or_else(|| {
        SpawnError::Refused(format!(
            "session {} never bound a Claude session, so there is nothing to resume; tiller agent relaunch {} --note <progress> starts one",
            session.id, run.id
        ))
    })?;
    let pane = match &session.pane_id {
        Some(id) => world.herdr.pane(id)?,
        None => None,
    };
    // A manual resume clears the run's pause (`run.unpause`). The key is
    // `session.resume`'s alone, so the unpause goes unkeyed.
    let unkeyed = Ctx {
        key: None,
        ..ctx.clone()
    };
    if let Some(pane) = pane.as_ref().filter(|p| p.agent.is_some()) {
        let unpaused = store.run_unpause(&unkeyed, &run.id)?;
        return Ok(Applied {
            replayed: unpaused.events.is_empty(),
            events: unpaused.events,
            value: Resumed {
                run: unpaused.value,
                session: session.clone(),
                pane_id: Some(pane.pane_id.clone()),
                left_alone: true,
                started: None,
                evidence: format!("herdr shows {}", describe(pane)),
                warning: None,
            },
        });
    }

    let host: Host = get(store.conn(), "host", &run.host_id)?;
    if let Some(why) = floor_refusal("RAM", host.ram_floor_mb, world.ram_free_mb)
        .or_else(|| floor_refusal("disk", host.disk_floor_mb, world.disk_free_mb))
    {
        return Err(SpawnError::Refused(format!("{why}; nothing was launched")));
    }
    let line = launch::write_again(
        &AgainSpec {
            role: role_of(run.role)?,
            session_id: &session.id,
            run_id: &run.id,
            worktree: &worktree,
            scratch: &world.scratch_root.join(&run.id),
            tiller_home: world.tiller_home,
            model: &run.model,
            effort: run.effort,
            claude: &world.config.spawn.claude,
            claude_config: world.claude_config,
            review_hosts: &world.config.spawn.review_allowed_hosts,
        },
        Start::Resume {
            claude_session_id: &claude_id,
        },
    )?;
    let mut events = store
        .session_resume_attempted(
            ctx,
            &session.id,
            json!({
                "ram_free_mb": world.ram_free_mb,
                "disk_free_mb": world.disk_free_mb,
                "cwd": worktree.display().to_string(),
                "resume": claude_id,
                "reason": MANUAL_RESUME_REASON,
                "skipped": null,
            }),
        )?
        .events;

    let card: Card = get(store.conn(), "card", &run.card_id)?;
    let pane_id = match pane {
        Some(pane) => pane.pane_id,
        None => {
            let created = open_pane(world, &card, run, &worktree)?;
            // The key was `session.resume`'s alone, not the bind's.
            events.extend(
                store
                    .run_bind_session(
                        &unkeyed,
                        Bind {
                            session_id: session.id.clone(),
                            claude_session_id: claude_id.clone(),
                            pane_id: Some(created.clone()),
                            transcript_path: None,
                            source: LAUNCH_BIND_SOURCE.to_owned(),
                        },
                    )?
                    .events,
            );
            created
        }
    };
    let since = Timestamp::now();
    let warning = type_line(store, world.herdr, run, &session.id, &pane_id, &line)?;
    let (started, evidence) =
        wait_started(store, world.herdr, waits, &session.id, &pane_id, since)?;
    // The pause clears only once the session is seen running: a resume that
    // failed to start leaves the run paused and its alarms quiet.
    events.extend(store.run_unpause(&unkeyed, &run.id)?.events);
    Ok(Applied {
        value: Resumed {
            run: get(store.conn(), "run", &run.id)?,
            session: get(store.conn(), "session", &session.id)?,
            pane_id: Some(pane_id),
            left_alone: false,
            started: Some(started),
            evidence,
            warning,
        },
        events,
        replayed: false,
    })
}

/// Stops the run's live session and starts a new one in the same worktree,
/// on the same run or as a new attempt (`run.relaunch`). The stop comes
/// first and must be proven; nothing is recorded when it cannot be.
/// `key` is the relaunch's idempotency key: a key that already relaunched
/// replays without touching herdr.
pub fn relaunch(
    store: &mut Store,
    ctx: &Ctx,
    key: Option<&str>,
    world: &World<'_>,
    waits: &Waits,
    run: &Run,
    request: RelaunchRequest,
) -> Result<Applied<RelaunchOutcome>> {
    // The key is `run.relaunch`'s alone, not the scan's or the placement's.
    let keyed = Ctx {
        key: key.map(str::to_owned),
        ..ctx.clone()
    };
    let ctx = &Ctx {
        key: None,
        ..ctx.clone()
    };
    require_controller(ctx, run, "relaunch", false)?;
    if store.key_recorded(&keyed)? {
        let applied =
            store.run_relaunch(&keyed, &run.id, relaunch_of(&request, None, None, None))?;
        return Ok(Applied {
            value: RelaunchOutcome::Replayed {
                run: Box::new(applied.value.run),
                session: Box::new(applied.value.session),
            },
            events: applied.events,
            replayed: true,
        });
    }
    if request.asked.is_some() && !request.new_attempt {
        return Err(SpawnError::Usage(
            "--model and --effort go with --new-attempt; a relaunch on the same run keeps its model".into(),
        ));
    }
    let brief = match (request.new_attempt, &request.brief) {
        (true, None) => {
            return Err(SpawnError::Usage("a new attempt needs its brief".into()));
        }
        (true, Some(brief)) => Some(brief.clone()),
        (false, _) => None,
    };
    require_local(store, run)?;
    let worktree = worktree_of(run)?;
    let project = project_of(store, run)?;
    let card: Card = get(store.conn(), "card", &run.card_id)?;
    let (scan, mut events) = scan_and_record(store, ctx, world.scanner, run, &project, &worktree)?;
    if scan.branch.is_none() || scan.branch != run.branch {
        return Err(SpawnError::Refused(format!(
            "worktree {} has {} checked out, not the run's branch {}; nothing was changed (R15)",
            worktree.display(),
            scan.branch.as_deref().unwrap_or("no branch"),
            run.branch.as_deref().unwrap_or("(none)")
        )));
    }
    let role = role_of(run.role)?;
    let scratch = world.scratch_root.join(&run.id);
    if !request.new_attempt {
        // The same run keeps its brief and renders the rest again: check
        // the brief is there before the stop.
        if launch::brief_in(&scratch).is_none() {
            return Err(SpawnError::Refused(format!(
                "{} is missing or holds no brief, so run {} cannot be launched again; relaunch with --new-attempt --brief-file <f>; nothing was changed",
                scratch.display(),
                run.id
            )));
        }
    }
    // Memory that cannot be read refuses the launch: read it before the
    // stop, so the refusal leaves the old session running.
    let memory = memory::render(store, world.tiller_home, &project.id, role)?;
    let model = match &request.asked {
        Some((model, effort)) => {
            let resolved = dispatch::resolve(
                &world.config.dispatch,
                &card,
                Some((model, *effort)),
                ctx.actor.is_owner(),
            )?;
            Some(AttemptModel {
                model: resolved.model,
                effort: resolved.effort,
                dispatch: resolved.payload,
            })
        }
        None => None,
    };
    // A new attempt is a new run, held to the floors a spawn is.
    let checks = request.new_attempt.then(|| SpawnChecks {
        ram_free_mb: world.ram_free_mb,
        disk_free_mb: world.disk_free_mb,
        quota_floor_percent: quota_floor(world.config, run.role),
        quota_max_age_minutes: world.config.quota.max_age_minutes,
        past_quota_floor: false,
        dispatch: model.as_ref().map_or(Value::Null, |m| m.dispatch.clone()),
    });
    // Every refusal the record would give comes before the stop, so none
    // leaves the agent stopped and nothing recorded.
    store.run_relaunch_rehearse(
        ctx,
        &run.id,
        &relaunch_of(&request, None, model.clone(), checks.clone()),
    )?;

    let old = live_session(store.conn(), &run.id)?;
    let (stop_proof, old_pane) = match &old {
        Some(session) => {
            let (proof, pane) = stop(store, world.herdr, waits, session)?;
            (Some(proof), pane)
        }
        None => (None, None),
    };
    let relaunched = store.run_relaunch(
        &keyed,
        &run.id,
        relaunch_of(&request, stop_proof.clone(), model, checks),
    )?;
    events.extend(relaunched.events);
    let relaunched = relaunched.value;
    let launched = start_again(
        store,
        ctx,
        world,
        Again {
            card: &card,
            project: &project,
            role,
            worktree: &worktree,
            run: &relaunched.run,
            session: &relaunched.session,
            note: &request.note,
            brief: brief.as_deref(),
            old_scratch: &scratch,
            old_pane,
            memory: &memory,
        },
        &mut events,
    )
    .map_err(|error| SpawnError::Undone {
        error: Box::new(error),
        notes: format!(
            "the relaunch is recorded (run {}, session {}) and nothing was deleted; start its session with tiller agent relaunch {} --note <progress> --key <a new key>, since this key replays",
            relaunched.run.id, relaunched.session.id, relaunched.run.id
        ),
    })?;
    let (since, pane_id, warning) = launched;
    let (started, evidence) = wait_started(
        store,
        world.herdr,
        waits,
        &relaunched.session.id,
        &pane_id,
        since,
    )?;
    Ok(Applied {
        value: RelaunchOutcome::Relaunched(Box::new(Relaunched {
            run: get(store.conn(), "run", &relaunched.run.id)?,
            session: get(store.conn(), "session", &relaunched.session.id)?,
            // The `SessionEnd` hook may have ended the row before the
            // relaunch did; it is still the session this relaunch stopped.
            ended_session: relaunched.ended_session.or(old.map(|s| s.id)),
            stop_proof,
            pane_id,
            started,
            evidence,
            warning,
        })),
        events,
        replayed: false,
    })
}

fn relaunch_of(
    request: &RelaunchRequest,
    stop_proof: Option<String>,
    model: Option<AttemptModel>,
    checks: Option<SpawnChecks>,
) -> Relaunch {
    Relaunch {
        note: request.note.clone(),
        new_attempt: request.new_attempt,
        stop_proof,
        model,
        checks,
    }
}

/// Stops a live session: `/exit` into its pane, after an Escape when it is
/// mid-turn, then waits for its `SessionEnd` hook or for herdr to show no
/// agent. Returns the proof and the pane, when it is still open, for the
/// next session. Refuses, having changed nothing recorded, when the stop
/// cannot be proven or would lose a draft in the composer.
pub(crate) fn stop(
    store: &Store,
    herdr: &dyn Herdr,
    waits: &Waits,
    session: &Session,
) -> Result<(String, Option<String>)> {
    match ready_to_stop(store, herdr, waits, session, true)? {
        Ready::Gone(proof) => Ok(proof),
        Ready::Exit(pane_id) => exit_and_prove(store, herdr, waits, session, &pane_id),
    }
}

/// Where a stop stands before its `/exit`.
enum Ready {
    /// No agent runs for the session: the proof, and the pane when open.
    Gone((String, Option<String>)),
    /// The agent's pane, at an empty composer that takes `/exit`.
    Exit(String),
}

/// The half of [`stop`] before `/exit`, which types nothing but Escape.
/// With `interrupt` false not even that: a session still working is
/// refused, so its turn is never cut short.
fn ready_to_stop(
    store: &Store,
    herdr: &dyn Herdr,
    waits: &Waits,
    session: &Session,
    interrupt: bool,
) -> Result<Ready> {
    let Some(pane_id) = session.pane_id.clone() else {
        return Ok(Ready::Gone((
            format!("session {} never had a pane", session.id),
            None,
        )));
    };
    let Some(pane) = herdr.pane(&pane_id)? else {
        return Ok(Ready::Gone((
            format!("herdr shows pane {pane_id} gone"),
            None,
        )));
    };
    // The pane may have gone to another session (a relaunch reuses it, and
    // herdr gives a closed pane's id to another workspace): what runs there
    // is not this session's to stop, and the pane not its to close.
    if let Some(holder) = pane_holder(store, &pane, &session.id)? {
        return Ok(Ready::Gone((
            format!("pane {pane_id} now holds session {holder}"),
            None,
        )));
    }
    if pane.agent.is_none() {
        return Ok(Ready::Gone((
            format!("herdr shows no agent in pane {pane_id}"),
            tagged(&pane, &session.id).then_some(pane_id),
        )));
    }
    if interrupt {
        if pane.agent_status.as_deref() == Some("working") {
            herdr.send_escape(&pane_id)?;
        }
    } else {
        // herdr's word wins; the recorded activity counts only without one.
        let working = match pane.agent_status.as_deref() {
            Some(status) => status == "working",
            None => session.activity == Activity::Working,
        };
        if working {
            return Err(SpawnError::Refused(format!(
                "session {} is still working (herdr: {}, recorded: {}); it is stopped once its turn ends",
                session.id,
                pane.agent_status.as_deref().unwrap_or("no status"),
                session.activity
            )));
        }
    }
    // `/exit` goes in only at an empty composer: a turn still running would
    // take it as a message, a dialog as an answer, and a draft would be lost.
    let ready = wait(waits, waits.interrupt, || {
        Ok(match composer::classify(&herdr.read_visible(&pane_id)?) {
            Composer::Empty => Some(Ok(())),
            Composer::Holds(draft) => Some(Err(format!(
                "the composer of pane {pane_id} holds a draft ({draft:?}); send or clear it first"
            ))),
            Composer::Dialog => Some(Err(format!(
                "pane {pane_id} shows a dialog; answer it first"
            ))),
            Composer::Busy | Composer::Unknown => None,
        })
    })?;
    match ready {
        Some(Ok(())) => {}
        Some(Err(why)) => {
            return Err(SpawnError::Refused(format!(
                "{why}; the session was not stopped and nothing was changed"
            )));
        }
        None => {
            return Err(SpawnError::Refused(format!(
                "pane {pane_id} did not come to an empty composer within {} s ({}); the session was not stopped and nothing was changed",
                waits.interrupt.as_secs(),
                herdr_view(herdr, &pane_id)
            )));
        }
    }
    Ok(Ready::Exit(pane_id))
}

/// Another session that holds `pane`: a live row bound to it, or the
/// session its tokens name.
fn pane_holder(store: &Store, pane: &PaneInfo, session: &str) -> Result<Option<String>> {
    let bound: Vec<Session> = list(
        store.conn(),
        "ended_at IS NULL AND pane_id = ?1 AND id <> ?2",
        &[&pane.pane_id, &session],
    )?;
    Ok(bound.into_iter().map(|s| s.id).next().or_else(|| {
        pane.tokens
            .get("session")
            .filter(|named| *named != session)
            .cloned()
    }))
}

/// Whether the pane's tokens name `session`: only such a pane is handed back
/// to be closed, as `run.clean` closes only those.
fn tagged(pane: &PaneInfo, session: &str) -> bool {
    pane.tokens
        .get("session")
        .is_some_and(|named| named == session)
}

/// The half of [`stop`] from `/exit` on: types it and waits for the proof.
fn exit_and_prove(
    store: &Store,
    herdr: &dyn Herdr,
    waits: &Waits,
    session: &Session,
    pane_id: &str,
) -> Result<(String, Option<String>)> {
    let pane_id = pane_id.to_owned();
    // The pane is reused only once herdr shows Claude gone from it: a
    // `SessionEnd` hook can fire while the process is still exiting, and a
    // line typed then would reach the dying composer.
    let gone = |herdr: &dyn Herdr| -> Result<Option<(String, Option<String>)>> {
        Ok(match herdr.pane(&pane_id)? {
            None => Some((format!("pane {pane_id} closed after /exit"), None)),
            Some(p) if p.agent.is_none() => Some((
                format!("herdr shows no agent in pane {pane_id} after /exit"),
                tagged(&p, &session.id).then(|| pane_id.clone()),
            )),
            Some(_) => None,
        })
    };
    herdr.send_text(&pane_id, "/exit")?;
    thread::sleep(waits.pacing.settle);
    // Claude can read the Enter as a newline, leaving `/exit` typed and the
    // session running: press again while the line stays, as delivery does.
    // Whatever the verdict, the proof below decides the stop.
    if let Submit::Failed(error) = submit::submit(
        herdr,
        &pane_id,
        waits.pacing,
        |composer| matches!(composer, Composer::Holds(text) if text == "/exit"),
        || Ok::<_, SpawnError>(gone(herdr)?.is_some()),
    )? {
        return Err(error.into());
    }
    let proof = wait(waits, waits.exit, || gone(herdr))?;
    let ended: Session = get(store.conn(), "session", &session.id)?;
    match (proof, ended.ended_at) {
        (Some(proof), _) => Ok(proof),
        // Proven by the hook, but Claude still holds the pane: the next
        // session gets a pane of its own.
        (None, Some(_)) => Ok((
            format!(
                "SessionEnd after /exit ({}); herdr still shows {}, so the pane is not reused",
                ended.end_reason.as_deref().unwrap_or("no reason"),
                herdr_view(herdr, &pane_id)
            ),
            None,
        )),
        (None, None) => Err(SpawnError::Refused(format!(
            "after /exit, no SessionEnd hook arrived and herdr still shows {} within {} s; the session is not proven stopped, so nothing was changed",
            herdr_view(herdr, &pane_id),
            waits.exit.as_secs()
        ))),
    }
}

/// The end reason of a session `tiller agent stop` stopped.
pub const STOPPED: &str = "stopped";

/// The end reason of a session the daemon stopped once its run finished.
pub const RUN_ENDED: &str = "run_ended";

/// An orchestrator stopped by [`stop_orchestrator`].
pub(crate) struct OrchestratorStop {
    pub proof: String,
    pub closed_pane: Option<String>,
    /// A pane left open, and why.
    pub warning: Option<String>,
    pub events: Vec<i64>,
}

/// Stops a live orchestrator with [`stop`], ends its row with `reason`, and
/// closes its pane rather than reusing it: herdr cannot tell a bare shell
/// from a program running in it. Only a pane herdr shows without an agent,
/// and whose tokens still name the session, is closed; one Claude still
/// holds after its `SessionEnd` hook is left open with a warning, since a
/// live pane is never closed.
pub(crate) fn stop_orchestrator(
    store: &mut Store,
    ctx: &Ctx,
    herdr: &dyn Herdr,
    waits: &Waits,
    session: &Session,
    reason: &str,
) -> Result<OrchestratorStop> {
    let (proof, pane) = stop(store, herdr, waits, session)?;
    let events = store.session_end(ctx, &session.id, reason)?.events;
    let (closed_pane, warning) = match (pane, &session.pane_id) {
        (Some(pane), _) => match herdr.close_pane(&pane) {
            Ok(()) => (Some(pane), None),
            Err(e) => (
                None,
                Some(format!(
                    "the stopped orchestrator ({proof}) left pane {pane} open: {e}"
                )),
            ),
        },
        (None, Some(pane)) => match herdr.pane(pane) {
            Ok(Some(open))
                if open.agent.is_some()
                    && matches!(pane_holder(store, &open, &session.id), Ok(None)) =>
            {
                (
                    None,
                    Some(format!(
                        "the stopped orchestrator ({proof}) left pane {pane} open; close it in herdr once {} exits",
                        open.agent.as_deref().unwrap_or("its agent")
                    )),
                )
            }
            _ => (None, None),
        },
        (None, None) => (None, None),
    };
    Ok(OrchestratorStop {
        proof,
        closed_pane,
        warning,
        events,
    })
}

/// What `tiller agent stop` did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stopped {
    pub session: Session,
    /// How the agent was proven stopped.
    pub stop_proof: String,
    /// The cleanup of a worker's ended run; `None` for an orchestrator, or
    /// when cleanup refused.
    pub cleaned: Option<Cleaned>,
    /// An orchestrator's pane, closed once its agent stopped.
    pub closed_pane: Option<String>,
    /// What the stop left undone: a cleanup that refused, or a pane that
    /// would not close.
    pub warning: Option<String>,
}

/// `tiller agent stop <run|session>`: stops the session's agent with the
/// proven stop a relaunch uses, ends its row, then cleans up after it. A
/// worker's run must have ended first, since the stop is how its pane and
/// worktree come back, never how a run ends; its supervisor or the owner
/// stops it, and cleanup returns its worktree when that holds nothing. Only
/// the owner stops an orchestrator, whose pane is closed as a restart's is.
pub fn stop_agent(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    waits: &Waits,
    session: &Session,
) -> Result<Applied<Stopped>> {
    // Judged as the session is now, not as the caller last read it.
    let session: Session = get(store.conn(), "session", &session.id)?;
    let run = match &session.run_id {
        Some(id) => {
            let run: Run = get(store.conn(), "run", id)?;
            if let Err(refused) = require_controller(ctx, &run, "stop the agent of", false) {
                // An ended run is never reowned, so the project's live
                // orchestrator stops its agent too.
                let project = project_of(store, &run)?;
                let live = live_orchestrator(store.conn(), &project.id)?;
                let current = matches!(&ctx.actor, Actor::Session(id)
                    if live.is_some_and(|live| live.id == *id));
                if run.ended_at.is_none() || !current {
                    return Err(refused);
                }
            }
            require_local(store, &run)?;
            if run.ended_at.is_none() {
                return Err(SpawnError::Refused(format!(
                    "run {} is active; end it first with tiller done {} --outcome <outcome>, then stop its agent; nothing was sent",
                    run.id, run.id
                )));
            }
            Some(run)
        }
        None => {
            if !matches!(ctx.actor, Actor::Owner | Actor::Board) {
                return Err(tiller_core::Error::Forbidden(format!(
                    "{} may not stop orchestrator session {}; the owner does",
                    ctx.actor, session.id
                ))
                .into());
            }
            if session.host_id != local_host(store)? {
                return Err(SpawnError::Refused(format!(
                    "session {} is on another host; stop it there",
                    session.id
                )));
            }
            None
        }
    };
    let mut cleaned = None;
    let mut closed_pane = None;
    let mut warnings = Vec::new();
    let (stop_proof, events) = match &run {
        Some(run) => {
            let (proof, _) = stop(store, world.herdr, waits, &session)?;
            let mut events = store.session_end(ctx, &session.id, STOPPED)?.events;
            // The key was `session.end`'s alone, not the cleanup's.
            let unkeyed = Ctx {
                key: None,
                ..ctx.clone()
            };
            match cleanup(store, &unkeyed, world, run) {
                Ok(applied) => {
                    events.extend(applied.events);
                    warnings.extend(applied.value.warnings.iter().cloned());
                    cleaned = Some(applied.value);
                }
                Err(error) => warnings.push(format!(
                    "the agent is stopped, but cleanup did not run: {error}"
                )),
            }
            (proof, events)
        }
        None => {
            let stopped = stop_orchestrator(store, ctx, world.herdr, waits, &session, STOPPED)?;
            closed_pane = stopped.closed_pane;
            warnings.extend(stopped.warning);
            // The row is ended: what is left undone is a warning, not a
            // failure that would hide the stop.
            match layout::bindings(store.conn(), &session.host_id) {
                Ok(bindings) => {
                    if let Err(e) = relayout(world.herdr, &bindings) {
                        warnings.push(e);
                    }
                }
                Err(e) => warnings.push(format!("the panes were not relabelled: {e}")),
            }
            (stopped.proof, stopped.events)
        }
    };
    Ok(Applied {
        value: Stopped {
            session: get(store.conn(), "session", &session.id)?,
            stop_proof,
            cleaned,
            closed_pane,
            warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        },
        events,
        replayed: false,
    })
}

/// The observation the stop loop records before its one `/exit` into a
/// session.
pub const EXIT_SENT: &str = "exit_sent";

/// What one pass of the daemon's stop loop did with one finished agent.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Swept {
    pub session: String,
    pub run: String,
    pub stopped: bool,
    /// `/exit` went in this pass: a session it did not stop is never sent
    /// another, and is left to R15.
    pub exit_sent: bool,
    /// An error, not a refusal, left the agent as it was.
    pub failed: bool,
    /// The stop proof, or why the agent was left for a later pass.
    pub evidence: String,
}

/// The daemon's stop loop: a run on this host that ended `landed` or
/// `reported` (a builder whose PR landed or holds its head, a scout, a
/// reviewer) leaves its agent nothing to do, so the agent is stopped with
/// the same proven stop as `tiller agent stop` and its row ended. Its pane
/// is then a bare shell, which reconcile closes before returning the
/// worktree (R15). Only an agent idle at an empty composer is stopped: a
/// turn still running, a draft, a dialog or a message not yet acknowledged
/// leaves it for a later pass, and nothing is ever interrupted. `/exit` goes
/// into a session at most once, recorded as an [`EXIT_SENT`] observation
/// first, so an agent that ignores it is left to R15's note. A run that
/// ended any other way keeps its agent for the orchestrator to decide about.
pub fn stop_finished(
    store: &mut Store,
    ctx: &Ctx,
    herdr: &dyn Herdr,
    waits: &Waits,
) -> Result<Vec<Swept>> {
    let host = local_host(store)?;
    let sessions: Vec<Session> = list(
        store.conn(),
        "ended_at IS NULL AND host_id = ?1 \
         AND run_id IN (SELECT id FROM runs WHERE ended_at IS NOT NULL AND outcome IN ('landed', 'reported')) \
         AND id NOT IN (SELECT session_id FROM observations WHERE event = ?2 AND session_id IS NOT NULL) \
         ORDER BY started_at, id",
        &[&host, &EXIT_SENT],
    )?;
    let mut swept = Vec::new();
    // One session that errors is reported and the rest still go.
    for session in sessions {
        let mut exit_sent = false;
        let stop =
            ready_to_stop(store, herdr, waits, &session, false).and_then(|ready| match ready {
                Ready::Gone(proof) => Ok(proof),
                Ready::Exit(pane_id) => {
                    // Read just before typing, since a pass waits on each
                    // agent in turn: `/exit` never races a message for it.
                    if waiting_message(store, &session.id)? {
                        return Err(SpawnError::Refused(format!(
                            "a message waits to be typed into session {}",
                            session.id
                        )));
                    }
                    store.observation_record(
                        ctx,
                        NewObservation {
                            session_id: Some(session.id.clone()),
                            run_id: session.run_id.clone(),
                            source: ObservationSource::Herdr,
                            event: EXIT_SENT.into(),
                            verdict: None,
                            observed_at: Timestamp::now(),
                            payload: json!({"pane_id": pane_id}),
                        },
                    )?;
                    exit_sent = true;
                    exit_and_prove(store, herdr, waits, &session, &pane_id)
                }
            });
        let (stopped, failed, evidence) = match stop
            .and_then(|(proof, _)| Ok((store.session_end(ctx, &session.id, RUN_ENDED)?, proof)))
        {
            Ok((_, proof)) => (true, false, proof),
            Err(SpawnError::Refused(why)) => (false, false, why),
            Err(error) => (false, true, error.to_string()),
        };
        swept.push(Swept {
            run: session.run_id.clone().unwrap_or_default(),
            session: session.id,
            stopped,
            exit_sent,
            failed,
            evidence,
        });
    }
    Ok(swept)
}

/// Whether the outbox holds a message for `session` not yet typed or not
/// yet acknowledged.
fn waiting_message(store: &Store, session: &str) -> Result<bool> {
    Ok(store
        .conn()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM outbox WHERE target_session_id = ?1 AND state IN ('pending', 'delivered'))",
            [session],
            |row| row.get(0),
        )
        .map_err(tiller_core::Error::from)?)
}

/// The session a relaunch starts, and where.
struct Again<'a> {
    card: &'a Card,
    project: &'a Project,
    role: crate::roles::Role,
    worktree: &'a Path,
    run: &'a Run,
    session: &'a Session,
    note: &'a str,
    /// A new attempt's brief; `None` keeps the run's own, from its prompt file.
    brief: Option<&'a str>,
    old_scratch: &'a Path,
    /// The stopped session's pane, when it is still open and free.
    old_pane: Option<String>,
    /// The memory, rendered before the stop.
    memory: &'a memory::Memory,
}

/// Writes the new session's launch, binds it to a pane (the old one when it
/// is still open) and types the line. Returns when the line went in, the
/// pane, and a layout repair that failed.
fn start_again(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    again: Again<'_>,
    events: &mut Vec<i64>,
) -> Result<(Timestamp, String, Option<String>)> {
    let Again {
        card,
        project,
        role,
        worktree,
        run,
        session,
        note,
        brief,
        old_scratch,
        old_pane,
        memory,
    } = again;
    let attempt = brief.is_some();
    let brief = match brief {
        Some(brief) => {
            let mut brief = brief.to_owned();
            if run.role == RunRole::Builder {
                brief.push_str(&delivery_line(project));
            }
            brief
        }
        // The same run keeps the brief it was launched with, but its
        // delivery line is the project's now, since the sandbox's gh login
        // rules follow the delivery the launch files are rendered for.
        None => {
            let kept = launch::brief_in(old_scratch).ok_or_else(|| {
                SpawnError::Refused(format!(
                    "{} holds no brief, so run {} cannot be launched again; relaunch with --new-attempt --brief-file <f>",
                    old_scratch.display(),
                    run.id
                ))
            })?;
            let kept = kept.as_str();
            let mut brief = match kept.rfind("\nDelivery: ") {
                Some(at) if !kept[at + 1..].contains('\n') => kept[..at].to_owned(),
                _ => kept.to_owned(),
            };
            if run.role == RunRole::Builder {
                brief.push_str(&delivery_line(project));
            }
            brief
        }
    };
    let claude_version = launch::claude_version(&world.config.spawn.claude);
    let spec = LaunchSpec {
        role,
        session_id: &session.id,
        run_id: &run.id,
        worktree,
        scratch: if attempt {
            &world.scratch_root.join(&run.id)
        } else {
            old_scratch
        },
        tiller_home: world.tiller_home,
        model: &run.model,
        effort: run.effort,
        brief: Some(&brief),
        roots: None,
        tiller: world.tiller,
        default_branch: &project.default_branch,
        network_allow: &world.config.spawn.network_allow,
        review_hosts: &world.config.spawn.review_allowed_hosts,
        no_mistakes_socket: world.config.spawn.no_mistakes_socket.as_deref(),
        gh_login_readable: project.effective_delivery() == Delivery::NoMistakes,
        claude: &world.config.spawn.claude,
        claude_config: world.claude_config,
        claude_version: claude_version.as_deref(),
        memory,
        note: Some(note),
    };
    let line = if attempt {
        // A new attempt is a new run: its own launch files and hash.
        let launched = launch::write(&spec)?;
        events.extend(
            store
                .config_manifest_record(ctx, &launched.config_hash, &launched.manifest)?
                .events,
        );
        events.extend(
            store
                .run_place(
                    ctx,
                    &run.id,
                    Placement {
                        config_hash: Some(launched.config_hash.clone()),
                        ..Placement::default()
                    },
                )?
                .events,
        );
        launched.line
    } else {
        // The same run keeps its brief, branch and worktree, but its launch
        // files are rendered again, so a tiller or role prompt upgraded
        // since the first launch reaches the new session.
        // The hash follows the files, so the run and the new session are
        // attributed to the config they actually run.
        let launched = launch::write_refreshed(&spec)?;
        events.extend(
            store
                .config_manifest_record(ctx, &launched.config_hash, &launched.manifest)?
                .events,
        );
        events.extend(
            store
                .run_reconfigure(ctx, &run.id, &launched.config_hash)?
                .events,
        );
        launched.line
    };
    // What the new session was launched with, for the stale-session check.
    events.extend(crate::stale::record(store, ctx, &session.id, role)?.events);
    let pane_id = match old_pane {
        Some(pane) => pane,
        None => open_pane(world, card, run, worktree)?,
    };
    events.extend(
        store
            .run_place(
                ctx,
                &run.id,
                Placement {
                    pane_id: Some(pane_id.clone()),
                    ..Placement::default()
                },
            )?
            .events,
    );
    let since = Timestamp::now();
    let warning = type_line(store, world.herdr, run, &session.id, &pane_id, &line)?;
    let (loads, loaded) = memory::record_load(store, ctx, &session.id, memory);
    events.extend(loads);
    let warning = [warning, loaded]
        .into_iter()
        .flatten()
        .reduce(|all, next| format!("{all}; {next}"));
    Ok((since, pane_id, warning))
}

/// Opens a worker workspace for the run in its worktree; returns its pane.
fn open_pane(world: &World<'_>, card: &Card, run: &Run, worktree: &Path) -> Result<String> {
    let label = layout::worker_label(&card.display_id, run.role, true);
    Ok(world
        .herdr
        .create_workspace(&label, Some(worktree))?
        .pane_id)
}

/// Reports the session's row tokens on its pane, runs the layout pass and
/// types the launch line. Returns a layout repair that failed.
fn type_line(
    store: &Store,
    herdr: &dyn Herdr,
    run: &Run,
    session: &str,
    pane_id: &str,
    line: &str,
) -> Result<Option<String>> {
    let bindings = layout::bindings(store.conn(), &run.host_id)?;
    if let Some(binding) = bindings.iter().find(|b| b.session_id == session) {
        herdr.report_tokens(pane_id, &binding.tokens())?;
    }
    let warning = relayout(herdr, &bindings).err();
    herdr.send_text(pane_id, line)?;
    herdr.send_enter(pane_id)?;
    Ok(warning)
}

/// Waits for the session's `SessionStart` hook after a launch.
fn wait_started(
    store: &Store,
    herdr: &dyn Herdr,
    waits: &Waits,
    session: &str,
    pane_id: &str,
    since: Timestamp,
) -> Result<(Proof, String)> {
    let seen = wait(waits, waits.bind, || {
        Ok(events_since(store, session, since)?
            .into_iter()
            .find(|e| e.starts_with("SessionStart:")))
    })?;
    Ok(match seen {
        Some(event) => (Proof::Confirmed, format!("hook {event} in pane {pane_id}")),
        None => (
            Proof::Unconfirmed,
            format!(
                "no SessionStart hook within {} s; herdr shows {}",
                waits.bind.as_secs(),
                herdr_view(herdr, pane_id)
            ),
        ),
    })
}

/// Polls `probe` every `waits.poll` until it finds something or `within`
/// runs out.
fn wait<T>(
    waits: &Waits,
    within: Duration,
    mut probe: impl FnMut() -> Result<Option<T>>,
) -> Result<Option<T>> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(found) = probe()? {
            return Ok(Some(found));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(waits.poll);
    }
}

/// The events recorded for a session at or after `since`, oldest first.
fn events_since(store: &Store, session: &str, since: Timestamp) -> Result<Vec<String>> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT event FROM observations WHERE session_id = ?1 AND recorded_at >= ?2
             ORDER BY recorded_at, id",
        )
        .map_err(tiller_core::Error::from)?;
    let rows = stmt
        .query_map(rusqlite::params![session, since], |row| row.get(0))
        .map_err(tiller_core::Error::from)?;
    Ok(rows
        .collect::<rusqlite::Result<_>>()
        .map_err(tiller_core::Error::from)?)
}

/// Whether the transcript, past `offset`, has the interrupt marker.
fn interrupt_in_transcript(path: &Path, offset: u64) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return false;
    }
    BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .any(|entry| {
            tailer::classify(&entry, false).is_some_and(|detected| detected.event == "interrupt")
        })
}

fn describe(pane: &PaneInfo) -> String {
    match &pane.agent {
        Some(agent) => format!(
            "{agent} ({}) in pane {}",
            pane.agent_status.as_deref().unwrap_or("no status"),
            pane.pane_id
        ),
        None => format!("no agent in pane {}", pane.pane_id),
    }
}

/// What herdr shows in a pane now, for a report.
fn herdr_view(herdr: &dyn Herdr, pane_id: &str) -> String {
    match herdr.pane(pane_id) {
        Ok(Some(pane)) => describe(&pane),
        Ok(None) => format!("pane {pane_id} gone"),
        Err(error) => format!("an unreadable pane {pane_id} ({error})"),
    }
}
