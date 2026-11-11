//! The delivery pass: store, then a wake-up line naming the message id, then
//! delivery states (T2.5; `protocol.md` 3.3, `recovery.md` R09).
//!
//! `outbox.enqueue` has already committed every message this pass sees, so a
//! crash anywhere in here leaves rows the next pass picks up. The body is
//! never typed: the only line that reaches a pane is
//! `tiller inbox <id> # <kind> <card>`, and only after the pane's herdr
//! metadata names the target session and its composer reads as empty. A
//! message is `delivered` only once its line has left the composer: the pass
//! waits a moment between the line and Enter, reads the composer back, and
//! presses Enter again while the line is still there. A line typed but not
//! submitted is finished with Enter, never typed again.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;
use tiller_core::domain::{
    Activity, Attention, AttentionKind, Card, OutboxKind, OutboxMessage, OutboxState, Project,
    Role, Run, Session,
};
use tiller_core::ids::{self, HOST_HEALTH_CARD};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::outbox::{NewMessage, live_orchestrator};
use tiller_core::state::runs::live_session;
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_herdr::client::Herdr;
use tiller_herdr::composer::{Composer, classify};
use tiller_herdr::poller::{PollError, Source};
use tiller_herdr::submit::{Submission, Submit};

/// How long a busy pane, or one whose composer holds text or cannot be read,
/// waits before the next try.
pub const BUSY_DEFER_SECS: i64 = 30;
/// How long a pane showing a dialog waits.
pub const DIALOG_DEFER_SECS: i64 = 60;
/// Minutes after `delivered_at` at which an unread message rings again.
pub const RINGS_MINUTES: [i64; 5] = [2, 5, 15, 30, 60];
/// How long the target must have been idle before a re-ring.
pub const IDLE_MINUTES: i64 = 2;
/// A composer holding text this long is reported like a missed doorbell.
pub const HELD_MINUTES: i64 = 10;
/// An answer unread this long after delivery, or undelivered this long
/// after it was stored with no session to take it, goes to the owner.
pub const ANSWER_MINUTES: i64 = 60;

/// How many times one pass presses Enter on a wake-up line that stays in
/// the composer.
pub use tiller_herdr::submit::{PRESSES as SUBMIT_PRESSES, Pacing};
/// How many passes press Enter on a wake-up line that stays in the
/// composer; after them it is left like someone's text.
pub const SUBMIT_PASSES: u32 = 3;

/// How long a remote lane's messages are left alone between looks.
const QUIET_REMOTE_MINUTES: i64 = 5;
/// The deferral reason for a composer holding someone's text.
const HELD: &str = "composer holds text";
/// The deferral reason for a wake-up line Enter did not submit.
const UNSUBMITTED: &str = "wake line typed but not submitted";

/// How often `message` has been rung at its current target, and when the
/// first of those rings was: `attempts` and `delivered_at` count every
/// target, so a successor is judged by its own doorbell only.
fn doorbell(store: &Store, message: &str) -> Result<(i64, Option<Timestamp>)> {
    Ok(store
        .conn()
        .query_row(
            "SELECT count(*), min(at) FROM events
             WHERE entity = 'outbox' AND entity_id = ?1 AND kind = 'outbox.delivered'
               AND seq > coalesce((SELECT max(seq) FROM events
                                   WHERE entity = 'outbox' AND entity_id = ?1
                                     AND kind = 'outbox.retargeted'), 0)",
            [message],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(tiller_core::Error::from)?)
}

/// The wake-up line for `message` (`protocol.md` 3.1).
pub fn wake_line(message: &OutboxMessage, card: Option<&Card>) -> String {
    match card {
        Some(card) => format!(
            "tiller inbox {} # {} {}",
            message.id, message.kind, card.display_id
        ),
        None => format!("tiller inbox {} # {}", message.id, message.kind),
    }
}

/// Whether the composer holds exactly one of `lines`, however its rows
/// wrapped: only such a line is this pass's to finish with Enter, never a
/// draft that merely starts with one.
fn holds_line(composer: &Composer, lines: &[String]) -> bool {
    let Composer::Holds(text) = composer else {
        return false;
    };
    let squeeze = |s: &str| s.split_whitespace().collect::<String>();
    let text = squeeze(text);
    lines.iter().any(|line| squeeze(line) == text)
}

fn plus_secs(at: Timestamp, secs: i64) -> Timestamp {
    Timestamp::from_millis(at.as_millis().saturating_add(secs.saturating_mul(1000))).unwrap_or(at)
}

/// When a message that has now been rung `attempts` times rings next.
fn next_ring(delivered_at: Timestamp, attempts: i64, now: Timestamp) -> Timestamp {
    let step = usize::try_from(attempts.saturating_sub(1)).unwrap_or(usize::MAX);
    let scheduled = match RINGS_MINUTES.get(step) {
        Some(minutes) => delivered_at.plus_minutes(*minutes),
        // Past the schedule: the orchestrator or the owner has been told;
        // keep the doorbell going hourly.
        None => now.plus_minutes(ANSWER_MINUTES),
    };
    scheduled.max(now.plus_minutes(IDLE_MINUTES))
}

/// What one pass did, by message id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Messages a wake-up line was typed for (or finished with Enter).
    pub rung: Vec<String>,
    /// `(message, reason)` for each deferral.
    pub deferred: Vec<(String, String)>,
    pub retargeted: Vec<String>,
    pub failed: Vec<String>,
    /// Messages reported to the orchestrator or the owner as unread.
    pub reported: Vec<String>,
    /// `(session, error)` for each session the pass could not serve, and
    /// for each message of a rung batch it could not record (the error
    /// then names the message).
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error(transparent)]
    State(#[from] tiller_core::Error),
}

type Result<T, E = DeliveryError> = std::result::Result<T, E>;

/// Why a ring cannot happen now, and how long to wait.
struct Defer {
    secs: i64,
    reason: String,
    /// The composer holds text this pass did not submit.
    held: bool,
}

impl Defer {
    fn new(secs: i64, reason: impl Into<String>) -> Self {
        Self {
            secs,
            reason: reason.into(),
            held: false,
        }
    }

    fn held(reason: &str) -> Self {
        Self {
            held: true,
            ..Self::new(BUSY_DEFER_SECS, reason)
        }
    }
}

/// A pane whose composer holds a session's wake-up line, ready for Enter.
struct Ready {
    pane_id: String,
    /// Every line a pass may have typed for the batch.
    lines: Vec<String>,
    /// This pass typed the line, rather than finding it there.
    typed: bool,
}

/// A session whose wake-up line is typed and waits for Enter.
struct Flight {
    session: Session,
    unacked: Vec<OutboxMessage>,
    doorbells: HashMap<String, (i64, Option<Timestamp>)>,
    /// The batch's oldest message, whose ack shows the line was submitted.
    oldest: String,
    pane_id: String,
    /// Every line a pass may have typed for this batch.
    lines: Vec<String>,
    /// This pass typed the line, rather than finding it there.
    typed: bool,
}

/// The daemon's delivery loop for the local host.
pub struct Deliverer<H: Herdr> {
    store: Store,
    herdr: H,
    host_id: String,
    pacing: Pacing,
    /// When each message's composer was first seen holding someone's text.
    held_since: HashMap<String, Timestamp>,
    /// Passes that left each message's wake-up line in the composer.
    unsubmitted: HashMap<String, u32>,
    /// `(message, target session)` pairs already reported as unread.
    reported: HashSet<(String, String)>,
    /// Messages this daemon cannot act on yet (a remote lane, a successor
    /// not started), skipped until then without writing anything.
    quiet_until: HashMap<String, Timestamp>,
}

impl<H: Herdr> Deliverer<H> {
    pub fn new(store: Store, herdr: H, host_id: impl Into<String>) -> Self {
        Self {
            store,
            herdr,
            host_id: host_id.into(),
            pacing: Pacing::default(),
            held_since: HashMap::new(),
            unsubmitted: HashMap::new(),
            reported: HashSet::new(),
            quiet_until: HashMap::new(),
        }
    }

    /// Replaces the default pauses around Enter.
    #[must_use]
    pub fn pacing(mut self, pacing: Pacing) -> Self {
        self.pacing = pacing;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// One pass over every due message. Each session is checked and its
    /// line typed in turn, but the pauses around Enter are taken once for
    /// all of them, and a session is recorded the moment its own line is
    /// shown to have left, so a pane that is slow to take its line never
    /// holds up the others.
    pub fn pass(&mut self, now: Timestamp) -> Result<Report> {
        let due: Vec<OutboxMessage> = list(
            self.store.conn(),
            "state IN ('pending', 'delivered') AND next_attempt_at <= ?1 ORDER BY created_at, id",
            &[&now],
        )?;
        let mut sessions: Vec<String> = Vec::new();
        for message in &due {
            if !sessions.contains(&message.target_session_id) {
                sessions.push(message.target_session_id.clone());
            }
        }
        self.quiet_until.retain(|_, until| *until > now);
        let ctx = Ctx::at(Actor::Daemon, now);
        let mut report = Report::default();
        let mut flights = Vec::new();
        for session in sessions {
            match self.serve(&ctx, &session, &mut report) {
                Ok(Some(flight)) => flights.push(flight),
                Ok(None) => {}
                Err(error) => Self::failed(&mut report, session, &error),
            }
        }
        self.submit_all(&ctx, flights, &mut report);
        Ok(report)
    }

    fn failed(report: &mut Report, session: String, error: &DeliveryError) {
        tracing::warn!(%session, %error, "delivery failed for a session");
        report.errors.push((session, error.to_string()));
    }

    /// Presses Enter for every flight and reads each composer back, with one
    /// pause for all of them at each step.
    fn submit_all(&mut self, ctx: &Ctx, flights: Vec<Flight>, report: &mut Report) {
        if flights.iter().any(|f| f.typed) {
            std::thread::sleep(self.pacing.settle);
        }
        let mut active = Vec::with_capacity(flights.len());
        for flight in flights {
            match Submission::begin(&self.herdr, &flight.pane_id) {
                Ok(submission) => active.push((flight, submission)),
                Err(error) => self.settle(ctx, flight, Submit::Failed(error), report),
            }
        }
        while !active.is_empty() {
            std::thread::sleep(self.pacing.recheck);
            let mut waiting = Vec::with_capacity(active.len());
            for (flight, mut submission) in active {
                let acked =
                    match get::<OutboxMessage>(self.store.conn(), "outbox message", &flight.oldest)
                    {
                        Ok(message) => message.state == OutboxState::Acked,
                        Err(error) => {
                            Self::failed(report, flight.session.id.clone(), &error.into());
                            continue;
                        }
                    };
                match submission.check(&self.herdr, acked, |c| holds_line(c, &flight.lines)) {
                    Some(done) => self.settle(ctx, flight, done, report),
                    None => waiting.push((flight, submission)),
                }
            }
            active = waiting;
        }
    }

    /// Records how a flight's submit went.
    fn settle(&mut self, ctx: &Ctx, flight: Flight, done: Submit, report: &mut Report) {
        let session = flight.session.id.clone();
        if let Err(error) = self.land(ctx, flight, done, report) {
            Self::failed(report, session, &error);
        }
    }

    fn land(&mut self, ctx: &Ctx, flight: Flight, done: Submit, report: &mut Report) -> Result<()> {
        let Flight {
            unacked,
            doorbells,
            pane_id,
            ..
        } = flight;
        let due = self.due_of(ctx.now, &unacked);
        let defer = match done {
            Submit::Left => {
                self.forget(&unacked);
                self.deliver(ctx, &unacked, &doorbells, &pane_id, report);
                return Ok(());
            }
            // Still pending, so the next pass finishes it with Enter; a line
            // stuck there for long is reported like held text.
            Submit::Held => {
                for message in &unacked {
                    *self.unsubmitted.entry(message.id.clone()).or_default() += 1;
                    self.held_since.entry(message.id.clone()).or_insert(ctx.now);
                }
                Defer::held(UNSUBMITTED)
            }
            Submit::Failed(error) => {
                self.forget(&unacked);
                Defer::new(BUSY_DEFER_SECS, error.to_string())
            }
        };
        let due: Vec<&OutboxMessage> = unacked.iter().filter(|m| due.contains(&m.id)).collect();
        self.defer(ctx, &due, defer, report)
    }

    /// The composer is no longer known to hold text.
    fn forget(&mut self, unacked: &[OutboxMessage]) {
        for message in unacked {
            self.held_since.remove(&message.id);
            self.unsubmitted.remove(&message.id);
        }
    }

    fn due_of(&self, now: Timestamp, unacked: &[OutboxMessage]) -> Vec<String> {
        unacked
            .iter()
            .filter(|m| m.next_attempt_at.is_some_and(|at| at <= now))
            .filter(|m| !self.quiet_until.contains_key(&m.id))
            .map(|m| m.id.clone())
            .collect()
    }

    /// Records every message of a batch as rung at `pane_id`. The line is
    /// already typed and submitted, so one message that cannot be recorded
    /// does not stop the rest: they are recorded now, not left pending for a
    /// second pass to type the line again. Each failure goes in the report.
    fn deliver(
        &mut self,
        ctx: &Ctx,
        unacked: &[OutboxMessage],
        doorbells: &HashMap<String, (i64, Option<Timestamp>)>,
        pane_id: &str,
        report: &mut Report,
    ) {
        let now = ctx.now;
        for message in unacked {
            let (rings, first) = doorbells.get(&message.id).copied().unwrap_or((0, None));
            let next = next_ring(first.unwrap_or(now), rings + 1, now);
            match self.record_ring(ctx, message, pane_id, next) {
                Ok(()) => report.rung.push(message.id.clone()),
                Err(error) => {
                    tracing::warn!(message = %message.id, %error, "recording a ring failed");
                    report.errors.push((
                        message.target_session_id.clone(),
                        format!("message {}: {error}", message.id),
                    ));
                    // Still due, it would have the line typed again on each
                    // pass while the fault lasts: it waits as for a busy
                    // pane, in the store if it can be written, else here.
                    let until = plus_secs(now, BUSY_DEFER_SECS);
                    let reason = format!("recording the ring failed: {error}");
                    match self.store.outbox_defer(ctx, &message.id, until, &reason) {
                        Ok(_) => report.deferred.push((message.id.clone(), reason)),
                        Err(_) => self.quiet(&[message], until),
                    }
                }
            }
        }
    }

    fn record_ring(
        &mut self,
        ctx: &Ctx,
        message: &OutboxMessage,
        pane_id: &str,
        next: Timestamp,
    ) -> Result<()> {
        match self.store.outbox_deliver(ctx, &message.id, pane_id, next) {
            Ok(_) => Ok(()),
            // The target's hook acked the batch the moment the line was
            // submitted, before this record. A read that fails too keeps
            // the record's own error.
            Err(error) => {
                match get::<OutboxMessage>(self.store.conn(), "outbox message", &message.id) {
                    Ok(now) if now.state == OutboxState::Acked => Ok(()),
                    _ => Err(error.into()),
                }
            }
        }
    }

    fn serve(
        &mut self,
        ctx: &Ctx,
        session_id: &str,
        report: &mut Report,
    ) -> Result<Option<Flight>> {
        let now = ctx.now;
        let session: Session = get(self.store.conn(), "session", session_id)?;
        let unacked: Vec<OutboxMessage> = list(
            self.store.conn(),
            "target_session_id = ?1 AND state IN ('pending', 'delivered') ORDER BY created_at, id",
            &[&session_id],
        )?;
        let due: Vec<&OutboxMessage> = unacked
            .iter()
            .filter(|m| m.next_attempt_at.is_some_and(|at| at <= now))
            .filter(|m| !self.quiet_until.contains_key(&m.id))
            .collect();
        if due.is_empty() {
            return Ok(None);
        }
        if session.host_id != self.host_id {
            // Remote lanes are read only in v1: the peer's own Tiller
            // delivers, retargets and fails its messages.
            self.quiet(&due, now.plus_minutes(QUIET_REMOTE_MINUTES));
            return Ok(None);
        }
        if session.ended_at.is_some() {
            self.rehome(ctx, &session, &due, report)?;
            return Ok(None);
        }
        let mut doorbells = HashMap::with_capacity(unacked.len());
        for message in &unacked {
            doorbells.insert(message.id.clone(), doorbell(&self.store, &message.id)?);
        }
        self.report_unread(ctx, &session, &unacked, &doorbells, report)?;

        // Only re-rings are due: they wait for the target to sit idle.
        if due.iter().all(|m| m.state == OutboxState::Delivered) {
            let idle_since = (session.activity == Activity::Idle)
                .then_some(session.activity_observed_at)
                .flatten();
            if idle_since.is_none_or(|at| at > now.plus_minutes(-IDLE_MINUTES)) {
                self.defer(ctx, &due, Defer::new(BUSY_DEFER_SECS, "not idle"), report)?;
                return Ok(None);
            }
        }

        let ring = self.ring(&session, &unacked)?;
        let oldest = unacked[0].id.clone();
        let Ready {
            pane_id,
            lines,
            typed,
        } = match ring {
            Ok(ready) => ready,
            Err(defer) => {
                if defer.held {
                    for message in &unacked {
                        self.held_since.entry(message.id.clone()).or_insert(now);
                    }
                } else {
                    self.forget(&unacked);
                }
                self.defer(ctx, &due, defer, report)?;
                return Ok(None);
            }
        };
        Ok(Some(Flight {
            session,
            unacked,
            doorbells,
            oldest,
            pane_id,
            lines,
            typed,
        }))
    }

    fn quiet(&mut self, messages: &[&OutboxMessage], until: Timestamp) {
        for message in messages {
            self.quiet_until.insert(message.id.clone(), until);
        }
    }

    /// Checks the pane and its composer, then types the line unless it is
    /// already there. Returns the pane, the lines the batch may have typed
    /// and whether this call typed one, or why not; Enter is pressed by
    /// [`Deliverer::submit_all`].
    fn ring(
        &mut self,
        session: &Session,
        unacked: &[OutboxMessage],
    ) -> Result<std::result::Result<Ready, Defer>> {
        if unacked.is_empty() {
            return Ok(Err(Defer::new(BUSY_DEFER_SECS, "nothing to ring")));
        }
        let Some(pane_id) = session.pane_id.clone() else {
            return Ok(Err(Defer::new(BUSY_DEFER_SECS, "session has no pane yet")));
        };
        let pane = match self.herdr.pane(&pane_id) {
            Ok(Some(pane)) => pane,
            Ok(None) => return Ok(Err(Defer::new(BUSY_DEFER_SECS, "pane is gone"))),
            Err(error) => return Ok(Err(Defer::new(BUSY_DEFER_SECS, error.to_string()))),
        };
        // The target check: never type into a pane that does not say it
        // hosts this session.
        match pane.tokens.get("session") {
            Some(named) if *named == session.id => {}
            Some(named) => {
                return Ok(Err(Defer::new(
                    BUSY_DEFER_SECS,
                    format!("pane {pane_id} hosts session {named}"),
                )));
            }
            None => {
                return Ok(Err(Defer::new(
                    BUSY_DEFER_SECS,
                    format!("pane {pane_id} does not name its session"),
                )));
            }
        }
        if session.activity == Activity::WaitingPermission
            || pane.agent_status.as_deref() == Some("blocked")
        {
            return Ok(Err(Defer::new(DIALOG_DEFER_SECS, "waiting on a prompt")));
        }
        if session.activity == Activity::Working || pane.agent_status.as_deref() == Some("working")
        {
            return Ok(Err(Defer::new(BUSY_DEFER_SECS, "busy")));
        }
        let screen = match self.herdr.read_visible(&pane_id) {
            Ok(screen) => screen,
            Err(error) => return Ok(Err(Defer::new(BUSY_DEFER_SECS, error.to_string()))),
        };
        // Every line a pass may have typed for this batch.
        let mut lines = Vec::with_capacity(unacked.len());
        for message in unacked {
            let card = match &message.card_id {
                Some(id) => find::<Card>(self.store.conn(), "id = ?1", &[id])?,
                None => None,
            };
            lines.push(wake_line(message, card.as_ref()));
        }
        let stuck = unacked.iter().any(|m| {
            self.unsubmitted
                .get(&m.id)
                .is_some_and(|n| *n >= SUBMIT_PASSES)
        });
        let typed = match classify(&screen) {
            Composer::Empty => {
                if let Err(error) = self.herdr.send_text(&pane_id, &lines[0]) {
                    return Ok(Err(Defer::new(BUSY_DEFER_SECS, error.to_string())));
                }
                true
            }
            composer @ Composer::Holds(_) if !stuck && holds_line(&composer, &lines) => false,
            Composer::Holds(_) => return Ok(Err(Defer::held(HELD))),
            Composer::Busy => return Ok(Err(Defer::new(BUSY_DEFER_SECS, "busy"))),
            Composer::Dialog => return Ok(Err(Defer::new(DIALOG_DEFER_SECS, "dialog showing"))),
            Composer::Unknown => {
                return Ok(Err(Defer::new(BUSY_DEFER_SECS, "composer unreadable")));
            }
        };
        Ok(Ok(Ready {
            pane_id,
            lines,
            typed,
        }))
    }

    fn defer(
        &mut self,
        ctx: &Ctx,
        due: &[&OutboxMessage],
        defer: Defer,
        report: &mut Report,
    ) -> Result<()> {
        let until = plus_secs(ctx.now, defer.secs);
        for message in due {
            self.store
                .outbox_defer(ctx, &message.id, until, &defer.reason)?;
            report
                .deferred
                .push((message.id.clone(), defer.reason.clone()));
        }
        Ok(())
    }

    /// The target ended. A run's message goes to the run's next session and
    /// waits for one while the run is active; an orchestrator's goes to the
    /// project's live orchestrator. A message with no successor to come
    /// fails, and so does an answer stored more than an hour ago.
    fn rehome(
        &mut self,
        ctx: &Ctx,
        session: &Session,
        due: &[&OutboxMessage],
        report: &mut Report,
    ) -> Result<()> {
        let conn = self.store.conn();
        let (successor, gone) = match &session.run_id {
            Some(run_id) => {
                let run: Run = get(conn, "run", run_id)?;
                (
                    live_session(conn, run_id)?,
                    run.ended_at.map(|_| "the run has ended"),
                )
            }
            None => {
                let project: Project = get(conn, "project", &session.project_id)?;
                (
                    live_orchestrator(conn, &session.project_id)?,
                    project.retired_at.map(|_| "the project is retired"),
                )
            }
        };
        for message in due {
            if let Some(successor) = successor.as_ref().filter(|s| s.id != session.id) {
                self.store
                    .outbox_retarget(ctx, &message.id, &successor.id)?;
                report.retargeted.push(message.id.clone());
                continue;
            }
            let stale_answer = message.kind == OutboxKind::Answer
                && message.created_at.plus_minutes(ANSWER_MINUTES) <= ctx.now;
            let reason = match (gone, stale_answer) {
                (Some(reason), _) => Some(format!("target session ended and {reason}")),
                (None, true) => Some("no session took the answer within an hour".to_owned()),
                (None, false) => None,
            };
            match reason {
                Some(reason) => {
                    self.store.outbox_fail(ctx, &message.id, &reason)?;
                    report.failed.push(message.id.clone());
                }
                // Waiting for the run's next session costs nothing per pass.
                None => self.quiet(&[message], plus_secs(ctx.now, BUSY_DEFER_SECS)),
            }
        }
        Ok(())
    }

    /// A missed doorbell, reported once per message: a worker's message
    /// still unread after the fifth ring, or behind someone's text for ten
    /// minutes, becomes a note to the orchestrator; an answer unread an hour
    /// after delivery, or held that long, becomes a `recovery` item.
    fn report_unread(
        &mut self,
        ctx: &Ctx,
        session: &Session,
        unacked: &[OutboxMessage],
        doorbells: &HashMap<String, (i64, Option<Timestamp>)>,
        report: &mut Report,
    ) -> Result<()> {
        let now = ctx.now;
        for message in unacked {
            let pair = (message.id.clone(), session.id.clone());
            if self.reported.contains(&pair) {
                continue;
            }
            let (rings, first) = doorbells.get(&message.id).copied().unwrap_or((0, None));
            let held = self
                .held_since
                .get(&message.id)
                .is_some_and(|at| at.plus_minutes(HELD_MINUTES) <= now);
            let unread = if message.kind == OutboxKind::Answer {
                held || first.is_some_and(|at| at.plus_minutes(ANSWER_MINUTES) <= now)
            } else {
                session.role != Role::Orchestrator
                    && (held || rings > i64::try_from(RINGS_MINUTES.len()).unwrap_or(5))
            };
            if !unread {
                continue;
            }
            let sent = if message.kind == OutboxKind::Answer {
                self.open_recovery(ctx, session, message)?;
                true
            } else {
                // With no live orchestrator, the next pass tries again.
                self.note_orchestrator(ctx, session, message, held)?
            };
            if sent {
                self.reported.insert(pair);
                report.reported.push(message.id.clone());
            }
        }
        Ok(())
    }

    fn open_recovery(
        &mut self,
        ctx: &Ctx,
        session: &Session,
        message: &OutboxMessage,
    ) -> Result<()> {
        let conn = self.store.conn();
        let question = match &message.reply_to {
            Some(id) => find::<Attention>(conn, "id = ?1", &[id])?.map(|a| a.question),
            None => None,
        }
        .unwrap_or_else(|| "your question".to_owned());
        let card = match &message.card_id {
            Some(id) => get::<Card>(conn, "card", id)?.display_id,
            None => HOST_HEALTH_CARD.to_owned(),
        };
        let key = format!("outbox-unread:{}:{}", message.id, session.id);
        self.store.attention_open(
            ctx,
            Some(&key),
            NewAttention {
                card,
                kind: AttentionKind::Recovery,
                question: format!(
                    "Your answer to \"{question}\" has not reached session {}; the worker is {}. Relaunch it, or retype?",
                    ids::short(&session.id),
                    session.activity,
                ),
                evidence: Some(
                    json!({
                        "outbox_id": message.id,
                        "delivered_at": message.delivered_at,
                        "attempts": message.attempts,
                    })
                    .to_string(),
                ),
                run_id: session.run_id.clone(),
                subject_session_id: Some(session.id.clone()),
                ..NewAttention::default()
            },
        )?;
        Ok(())
    }

    fn note_orchestrator(
        &mut self,
        ctx: &Ctx,
        session: &Session,
        message: &OutboxMessage,
        held: bool,
    ) -> Result<bool> {
        let conn = self.store.conn();
        let Some(orchestrator) = live_orchestrator(conn, &session.project_id)? else {
            return Ok(false);
        };
        let card = match &message.card_id {
            Some(id) => Some(get::<Card>(conn, "card", id)?),
            None => None,
        };
        let why = if held {
            format!("its composer has held text for {HELD_MINUTES} minutes")
        } else {
            format!("after {} rings", RINGS_MINUTES.len())
        };
        let key = format!("outbox-unread:{}:{}", message.id, session.id);
        self.store.outbox_enqueue(
            ctx,
            Some(&key),
            NewMessage {
                target_session_id: orchestrator.id,
                kind: OutboxKind::Note,
                body: format!(
                    "session {} ({}) has not read {} {} {why}",
                    ids::short(&session.id),
                    session.role,
                    message.kind,
                    ids::short(&message.id),
                ),
                card_id: card.map(|c| c.id),
                reply_to: None,
            },
        )?;
        Ok(true)
    }
}

/// The delivery loop as a poller: one pass every two seconds.
impl<H: Herdr + Send + 'static> Source for Deliverer<H> {
    fn name(&self) -> String {
        "delivery".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(2)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        match self.pass(now) {
            Ok(_) => Ok(()),
            Err(DeliveryError::State(error)) => Err(PollError::State(error)),
        }
    }
}
