//! The delivery pass: store, then a wake-up line naming the message id, then
//! delivery states (T2.5; `protocol.md` 3.3, `recovery.md` R09).
//!
//! `outbox.enqueue` has already committed every message this pass sees, so a
//! crash anywhere in here leaves rows the next pass picks up. The body is
//! never typed: the only line that reaches a pane is
//! `tiller inbox <id> # <kind> <card>`, and only after the pane's herdr
//! metadata names the target session and its composer reads as empty. A
//! message is `delivered` only once herdr took the keystrokes. A line typed
//! but not submitted is finished with Enter, never typed again.

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

/// How long a remote lane's messages are left alone between looks.
const QUIET_REMOTE_MINUTES: i64 = 5;
/// The deferral reason for a composer holding someone's text.
const HELD: &str = "composer holds text";

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

/// The message id a held composer's text names, when the text is a wake-up
/// line this pass may finish with Enter.
fn wake_line_id(text: &str) -> Option<&str> {
    let id = text
        .trim()
        .strip_prefix("tiller inbox ")?
        .split(' ')
        .next()?;
    ids::is_ulid(id).then_some(id)
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
    /// `(session, error)` for each session the pass could not serve.
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
}

impl Defer {
    fn new(secs: i64, reason: impl Into<String>) -> Self {
        Self {
            secs,
            reason: reason.into(),
        }
    }
}

/// The daemon's delivery loop for the local host.
pub struct Deliverer<H: Herdr> {
    store: Store,
    herdr: H,
    host_id: String,
    /// When each message's composer was first seen holding someone's text.
    held_since: HashMap<String, Timestamp>,
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
            held_since: HashMap::new(),
            reported: HashSet::new(),
            quiet_until: HashMap::new(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// One pass over every due message, one session at a time.
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
        for session in sessions {
            if let Err(error) = self.serve(&ctx, &session, &mut report) {
                tracing::warn!(%session, %error, "delivery failed for a session");
                report.errors.push((session, error.to_string()));
            }
        }
        Ok(report)
    }

    fn serve(&mut self, ctx: &Ctx, session_id: &str, report: &mut Report) -> Result<()> {
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
            return Ok(());
        }
        if session.host_id != self.host_id {
            // Remote lanes are read only in v1: the peer's own Tiller
            // delivers, retargets and fails its messages.
            self.quiet(&due, now.plus_minutes(QUIET_REMOTE_MINUTES));
            return Ok(());
        }
        if session.ended_at.is_some() {
            return self.rehome(ctx, &session, &due, report);
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
                return self.defer(ctx, &due, Defer::new(BUSY_DEFER_SECS, "not idle"), report);
            }
        }

        let rung = self.ring(&session, &unacked, now)?;
        if !matches!(&rung, Err(defer) if defer.reason == HELD) {
            // The composer is no longer known to hold someone's text.
            for message in &unacked {
                self.held_since.remove(&message.id);
            }
        }
        match rung {
            Ok(pane_id) => {
                for message in &unacked {
                    let (rings, first) = doorbells.get(&message.id).copied().unwrap_or((0, None));
                    let next = next_ring(first.unwrap_or(now), rings + 1, now);
                    match self.store.outbox_deliver(ctx, &message.id, &pane_id, next) {
                        Ok(_) => {}
                        // The target's hook acked the batch the moment the
                        // line was submitted, before this record.
                        Err(_)
                            if get::<OutboxMessage>(
                                self.store.conn(),
                                "outbox message",
                                &message.id,
                            )?
                            .state
                                == OutboxState::Acked => {}
                        Err(error) => return Err(error.into()),
                    }
                    report.rung.push(message.id.clone());
                }
                Ok(())
            }
            Err(defer) => self.defer(ctx, &due, defer, report),
        }
    }

    fn quiet(&mut self, messages: &[&OutboxMessage], until: Timestamp) {
        for message in messages {
            self.quiet_until.insert(message.id.clone(), until);
        }
    }

    /// Checks the pane and its composer, then types the line (or Enter
    /// alone for a line already typed). Returns the pane rung, or why not.
    fn ring(
        &mut self,
        session: &Session,
        unacked: &[OutboxMessage],
        now: Timestamp,
    ) -> Result<std::result::Result<String, Defer>> {
        let Some(oldest) = unacked.first() else {
            return Ok(Err(Defer::new(BUSY_DEFER_SECS, "nothing to ring")));
        };
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
        let typed = match classify(&screen) {
            Composer::Empty => {
                let card = match &oldest.card_id {
                    Some(id) => find::<Card>(self.store.conn(), "id = ?1", &[id])?,
                    None => None,
                };
                let line = wake_line(oldest, card.as_ref());
                self.herdr
                    .send_text(&pane_id, &line)
                    .and_then(|()| self.herdr.send_enter(&pane_id))
            }
            Composer::Holds(text)
                if wake_line_id(&text).is_some_and(|id| unacked.iter().any(|m| m.id == id)) =>
            {
                self.herdr.send_enter(&pane_id)
            }
            Composer::Holds(_) => {
                for message in unacked {
                    self.held_since.entry(message.id.clone()).or_insert(now);
                }
                return Ok(Err(Defer::new(BUSY_DEFER_SECS, HELD)));
            }
            Composer::Busy => return Ok(Err(Defer::new(BUSY_DEFER_SECS, "busy"))),
            Composer::Dialog => return Ok(Err(Defer::new(DIALOG_DEFER_SECS, "dialog showing"))),
            Composer::Unknown => {
                return Ok(Err(Defer::new(BUSY_DEFER_SECS, "composer unreadable")));
            }
        };
        Ok(match typed {
            Ok(()) => Ok(pane_id),
            Err(error) => Err(Defer::new(BUSY_DEFER_SECS, error.to_string())),
        })
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
            format!("its composer has held other text for {HELD_MINUTES} minutes")
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
