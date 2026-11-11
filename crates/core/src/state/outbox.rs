//! The outbox: `outbox.enqueue`, `outbox.deliver`, `outbox.ack`,
//! `outbox.fail`, `outbox.retarget` (`state.md`, Outbox).

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::attention::{NewAttention, open_attention};
use super::projects::require_daemon;
use super::{find, get, list, refuse, required, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, HOST_HEALTH_CARD};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// A message to store before anything is typed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewMessage {
    pub target_session_id: String,
    pub kind: OutboxKind,
    pub body: String,
    pub card_id: Option<String>,
    /// An ask or attention id.
    pub reply_to: Option<String>,
}

/// The project's live orchestrator session, if any.
pub fn live_orchestrator(conn: &Connection, project_id: &str) -> Result<Option<Session>> {
    find(
        conn,
        "project_id = ?1 AND role = 'orchestrator' AND ended_at IS NULL",
        &[&project_id],
    )
}

/// `outbox.enqueue` inside a mutation, with no key of its own.
pub(crate) fn enqueue(tx: &mut Tx<'_>, message: &NewMessage) -> Result<OutboxMessage> {
    insert(tx, message, None)
}

fn insert(tx: &mut Tx<'_>, message: &NewMessage, key: Option<&str>) -> Result<OutboxMessage> {
    required("body", &message.body)?;
    let session: Session = get(tx.conn(), "session", &message.target_session_id)?;
    if session.ended_at.is_some() {
        return Err(Error::refused(
            "session",
            &session.id,
            "target session has ended",
        ));
    }
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO outbox (id, target_session_id, target_pane_id, kind, body, card_id, reply_to,
                             idempotency_key, next_attempt_at, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10)",
        params![
            id,
            session.id,
            session.pane_id,
            message.kind,
            message.body,
            message.card_id,
            message.reply_to,
            key,
            tx.now(),
            tx.actor().to_string(),
        ],
    )?;
    tx.event(
        NewEvent::new("outbox", &id, "outbox.enqueued")
            .rev(1)
            .card_opt(message.card_id.clone())
            .payload(json!({
                "target_session_id": session.id,
                "kind": message.kind,
                "reply_to": message.reply_to,
            })),
    )?;
    get(tx.conn(), "outbox message", &id)
}

fn load(tx: &Tx<'_>, id: &str) -> Result<OutboxMessage> {
    let id = resolve_id(tx.conn(), "outbox", "outbox message", id)?;
    get(tx.conn(), "outbox message", &id)
}

/// Refuses an ack from any session but the target.
fn require_target(actor: &Actor, message: &OutboxMessage) -> Result<()> {
    match actor.session_id() {
        Some(session) if session != message.target_session_id => Err(Error::Forbidden(format!(
            "message {} is for session {}, not {session}",
            message.id, message.target_session_id
        ))),
        _ => Ok(()),
    }
}

fn ack(tx: &mut Tx<'_>, message: &OutboxMessage) -> Result<OutboxMessage> {
    message
        .state
        .ack()
        .map_err(refuse("outbox message", &message.id))?;
    tx.conn().execute(
        "UPDATE outbox SET state = 'acked', acked_at = ?2, next_attempt_at = NULL, rev = rev + 1 WHERE id = ?1",
        params![message.id, tx.now()],
    )?;
    tx.event(
        NewEvent::new("outbox", &message.id, "outbox.acked")
            .rev(message.rev + 1)
            .card_opt(message.card_id.clone())
            .payload(
                json!({"was": message.state, "polled": message.state == OutboxState::Pending}),
            ),
    )?;
    get(tx.conn(), "outbox message", &message.id)
}

impl Store {
    /// `outbox.enqueue`. Keyed: an existing `idempotency_key` returns the
    /// existing row.
    pub fn outbox_enqueue(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        message: NewMessage,
    ) -> Result<Applied<OutboxMessage>> {
        self.write(ctx, "outbox.enqueue", key, |tx| {
            if let Some(key) = key
                && let Some(existing) =
                    find::<OutboxMessage>(tx.conn(), "idempotency_key = ?1", &[&key])?
            {
                return Ok(existing);
            }
            insert(tx, &message, key)
        })
    }

    /// `tiller send` and the board's message bar (`protocol.md` 6.4, 7.4):
    /// a steer or a note for `session_id`, filed on its run's card when it
    /// has a run. Keyed like [`Store::outbox_enqueue`].
    pub fn outbox_send(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        session_id: &str,
        kind: OutboxKind,
        body: String,
    ) -> Result<Applied<OutboxMessage>> {
        if !matches!(kind, OutboxKind::Steer | OutboxKind::Note) {
            return Err(Error::Usage(format!(
                "a send is a steer or a note, not {kind}"
            )));
        }
        let session: Session = get(self.conn(), "session", session_id)?;
        let card_id = match &session.run_id {
            Some(run_id) => Some(get::<Run>(self.conn(), "run", run_id)?.card_id),
            None => None,
        };
        self.outbox_enqueue(
            ctx,
            key,
            NewMessage {
                target_session_id: session.id,
                kind,
                body,
                card_id,
                reply_to: None,
            },
        )
    }

    /// `outbox.deliver` (daemon): the wake-up line reached `pane_id`, which
    /// hosts the target session. A delivered message rings again.
    pub fn outbox_deliver(
        &mut self,
        ctx: &Ctx,
        id: &str,
        pane_id: &str,
        next_attempt_at: Timestamp,
    ) -> Result<Applied<OutboxMessage>> {
        require_daemon(&ctx.actor, "outbox.deliver")?;
        self.write(ctx, "outbox.deliver", None, |tx| {
            let message = load(tx, id)?;
            message.state.deliver().map_err(refuse("outbox message", &message.id))?;
            let session: Session = get(tx.conn(), "session", &message.target_session_id)?;
            if session.ended_at.is_some() || session.pane_id.as_deref() != Some(pane_id) {
                return Err(Error::refused(
                    "outbox message",
                    &message.id,
                    format!("pane {pane_id} does not host session {}", session.id),
                ));
            }
            tx.conn().execute(
                "UPDATE outbox SET state = 'delivered', attempts = attempts + 1, target_pane_id = ?2,
                        delivered_at = coalesce(delivered_at, ?3), next_attempt_at = ?4, rev = rev + 1
                 WHERE id = ?1",
                params![message.id, pane_id, tx.now(), next_attempt_at],
            )?;
            tx.event(
                NewEvent::new("outbox", &message.id, "outbox.delivered")
                    .rev(message.rev + 1)
                    .card_opt(message.card_id.clone())
                    .payload(json!({"pane_id": pane_id, "attempt": message.attempts + 1})),
            )?;
            get(tx.conn(), "outbox message", &message.id)
        })
    }

    /// The daemon defers delivery to a busy pane or one showing a permission
    /// dialog; deferring never fails a message.
    pub fn outbox_defer(
        &mut self,
        ctx: &Ctx,
        id: &str,
        until: Timestamp,
        reason: &str,
    ) -> Result<Applied<OutboxMessage>> {
        require_daemon(&ctx.actor, "outbox.defer")?;
        self.write(ctx, "outbox.defer", None, |tx| {
            let message = load(tx, id)?;
            if message.next_attempt_at == Some(until) {
                return Ok(message);
            }
            message
                .state
                .deliver()
                .map_err(refuse("outbox message", &message.id))?;
            tx.conn().execute(
                "UPDATE outbox SET next_attempt_at = ?2, rev = rev + 1 WHERE id = ?1",
                params![message.id, until],
            )?;
            tx.event(
                NewEvent::new("outbox", &message.id, "outbox.deferred")
                    .rev(message.rev + 1)
                    .card_opt(message.card_id.clone())
                    .payload(json!({"until": until, "reason": reason})),
            )?;
            get(tx.conn(), "outbox message", &message.id)
        })
    }

    /// `outbox.ack` (`tiller inbox <id>`): reads the body and acks in one
    /// transaction. Natural: an acked message is returned.
    pub fn outbox_ack(&mut self, ctx: &Ctx, id: &str) -> Result<Applied<OutboxMessage>> {
        self.write(ctx, "outbox.ack", None, |tx| {
            let message = load(tx, id)?;
            require_target(tx.actor(), &message)?;
            if message.state == OutboxState::Acked {
                return Ok(message);
            }
            ack(tx, &message)
        })
    }

    /// `tiller inbox` with no id: acks and returns every unacked message for
    /// `session`, oldest first.
    pub fn outbox_drain(
        &mut self,
        ctx: &Ctx,
        session: &str,
    ) -> Result<Applied<Vec<OutboxMessage>>> {
        self.write(ctx, "outbox.ack", None, |tx| {
            let pending: Vec<OutboxMessage> = list(
                tx.conn(),
                "target_session_id = ?1 AND state IN ('pending', 'delivered') ORDER BY created_at, id",
                &[&session],
            )?;
            let mut acked = Vec::with_capacity(pending.len());
            for message in pending {
                require_target(tx.actor(), &message)?;
                acked.push(ack(tx, &message)?);
            }
            Ok(acked)
        })
    }

    /// `outbox.fail` (daemon). A failed answer opens a `recovery` item on its
    /// card, or on `fleet-1` when it has none.
    pub fn outbox_fail(
        &mut self,
        ctx: &Ctx,
        id: &str,
        reason: &str,
    ) -> Result<Applied<OutboxMessage>> {
        require_daemon(&ctx.actor, "outbox.fail")?;
        required("reason", reason)?;
        self.write(ctx, "outbox.fail", None, |tx| {
            let message = load(tx, id)?;
            if message.state == OutboxState::Failed {
                return Ok(message);
            }
            message.state.fail().map_err(refuse("outbox message", &message.id))?;
            tx.conn().execute(
                "UPDATE outbox SET state = 'failed', failed_at = ?2, failure_reason = ?3, next_attempt_at = NULL,
                        rev = rev + 1
                 WHERE id = ?1",
                params![message.id, tx.now(), reason],
            )?;
            tx.event(
                NewEvent::new("outbox", &message.id, "outbox.failed")
                    .rev(message.rev + 1)
                    .card_opt(message.card_id.clone())
                    .payload(json!({"reason": reason, "attempts": message.attempts})),
            )?;
            if message.kind == OutboxKind::Answer {
                let card = message.card_id.clone().unwrap_or_else(|| HOST_HEALTH_CARD.to_owned());
                open_attention(
                    tx,
                    &NewAttention {
                        card,
                        kind: AttentionKind::Recovery,
                        question: format!("An answer could not be delivered: {reason}. Resend it or answer again?"),
                        evidence: Some(json!({"outbox_id": message.id, "reply_to": message.reply_to}).to_string()),
                        ..NewAttention::default()
                    },
                )?;
            }
            get(tx.conn(), "outbox message", &message.id)
        })
    }

    /// `outbox.retarget` (daemon): the target ended and `successor` is the
    /// live session of the same run, or the project's live orchestrator.
    pub fn outbox_retarget(
        &mut self,
        ctx: &Ctx,
        id: &str,
        successor: &str,
    ) -> Result<Applied<OutboxMessage>> {
        require_daemon(&ctx.actor, "outbox.retarget")?;
        self.write(ctx, "outbox.retarget", None, |tx| {
            let message = load(tx, id)?;
            if message.target_session_id == successor {
                return Ok(message);
            }
            message
                .state
                .retarget()
                .map_err(refuse("outbox message", &message.id))?;
            let old: Session = get(tx.conn(), "session", &message.target_session_id)?;
            if old.ended_at.is_none() {
                return Err(Error::refused(
                    "session",
                    &old.id,
                    "target session is still live",
                ));
            }
            let new: Session = get(tx.conn(), "session", successor)?;
            let same_run = old.run_id.is_some() && old.run_id == new.run_id;
            let same_project = new.role == Role::Orchestrator && new.project_id == old.project_id;
            if new.ended_at.is_some() || !(same_run || same_project) {
                return Err(Error::refused(
                    "session",
                    &new.id,
                    "not a live successor of the same run or project",
                ));
            }
            tx.conn().execute(
                "UPDATE outbox SET target_session_id = ?2, target_pane_id = ?3, state = 'pending',
                        next_attempt_at = ?4, rev = rev + 1
                 WHERE id = ?1",
                params![message.id, new.id, new.pane_id, tx.now()],
            )?;
            tx.event(
                NewEvent::new("outbox", &message.id, "outbox.retargeted")
                    .rev(message.rev + 1)
                    .card_opt(message.card_id.clone())
                    .payload(json!({"from": old.id, "to": new.id})),
            )?;
            get(tx.conn(), "outbox message", &message.id)
        })
    }
}
