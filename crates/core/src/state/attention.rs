//! Attention items: `attention.open`, `attention.answer`, `attention.withdraw`
//! (`state.md`, Attention).

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::cards::close_card;
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::{get, refuse, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// An attention item to open.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewAttention {
    /// A card reference (display id, ULID or short form).
    pub card: String,
    pub kind: AttentionKind,
    pub question: String,
    pub evidence: Option<String>,
    pub recommendation: Option<String>,
    pub consequence: Option<String>,
    pub options: Option<Value>,
    pub run_id: Option<String>,
    /// A live session the question asks the owner to act on: the item stops
    /// being current when it ends. Leave empty when the question is about a
    /// session that already ended.
    pub subject_session_id: Option<String>,
    /// The project the question is about, when the card is not its own
    /// (`fleet-1` items). Taken from `subject_session_id` when empty.
    pub subject_project_id: Option<String>,
}

impl Default for NewAttention {
    fn default() -> Self {
        Self {
            card: String::new(),
            kind: AttentionKind::Decision,
            question: String::new(),
            evidence: None,
            recommendation: None,
            consequence: None,
            options: None,
            run_id: None,
            subject_session_id: None,
            subject_project_id: None,
        }
    }
}

/// An answer: the item, whether it was still current when answered, the
/// message that carries it to the owning session, and the card it closed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Answered {
    pub attention: Attention,
    pub still_current: bool,
    pub message: Option<OutboxMessage>,
    pub closed_card: Option<Card>,
}

/// `attention.open` inside a mutation.
pub(crate) fn open_attention(tx: &mut Tx<'_>, new: &NewAttention) -> Result<Attention> {
    required("question", &new.question)?;
    let card = resolve_card(tx.conn(), &new.card)?;
    if card.state != CardState::Open {
        return Err(Error::refused("card", &card.display_id, "card is closed"));
    }
    let id = ids::new_ulid();
    let session = tx.actor().session_id().map(str::to_owned);
    let subject_project = match (&new.subject_project_id, &new.subject_session_id) {
        (Some(project), _) => Some(project.clone()),
        (None, Some(session)) => tx
            .conn()
            .query_row(
                "SELECT project_id FROM sessions WHERE id = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()?,
        (None, None) => None,
    };
    tx.conn().execute(
        "INSERT INTO attention (id, card_id, kind, question, evidence, recommendation, consequence, options,
                                opened_at, opened_by_session_id, opened_by_run_id,
                                subject_session_id, subject_project_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            card.id,
            new.kind,
            new.question,
            new.evidence,
            new.recommendation,
            new.consequence,
            new.options,
            tx.now(),
            session,
            new.run_id,
            new.subject_session_id,
            subject_project,
        ],
    )?;
    tx.event(
        NewEvent::new("attention", &id, "attention.opened")
            .rev(1)
            .card(&card.id)
            .payload(json!({"kind": new.kind, "question": new.question, "run_id": new.run_id})),
    )?;
    get(tx.conn(), "attention", &id)
}

/// `attention.withdraw` inside a mutation.
pub(crate) fn withdraw_attention(
    tx: &mut Tx<'_>,
    item: &Attention,
    reason: &str,
) -> Result<Attention> {
    required("reason", reason)?;
    status(item)
        .withdraw()
        .map_err(refuse("attention", &item.id))?;
    tx.conn().execute(
        "UPDATE attention SET withdrawn_at = ?2, withdraw_reason = ?3, rev = rev + 1 WHERE id = ?1",
        params![item.id, tx.now(), reason],
    )?;
    tx.event(
        NewEvent::new("attention", &item.id, "attention.withdrawn")
            .rev(item.rev + 1)
            .card(&item.card_id)
            .payload(json!({"reason": reason})),
    )?;
    get(tx.conn(), "attention", &item.id)
}

/// `attention.answer` inside a mutation, after the caller resolved the item.
/// Natural: the same words again return the first answer; different words
/// are refused, since one row holds one answer.
pub(crate) fn answer_attention(
    tx: &mut Tx<'_>,
    item: &Attention,
    answer: &str,
    via: AnsweredVia,
) -> Result<Answered> {
    required("answer", answer)?;
    if item.answered_at.is_some() {
        if item.answer.as_deref() == Some(answer) {
            return Ok(Answered {
                attention: item.clone(),
                still_current: false,
                message: None,
                closed_card: None,
            });
        }
        return Err(Error::refused(
            "attention",
            &item.id,
            format!(
                "item is already answered: {:?}",
                item.answer.as_deref().unwrap_or_default()
            ),
        ));
    }
    status(item)
        .answer()
        .map_err(refuse("attention", &item.id))?;
    let current = still_current(tx.conn(), item)?;
    tx.conn().execute(
        "UPDATE attention SET answered_at = ?2, answer = ?3, answered_via = ?4, answer_idempotency_key = ?5,
                rev = rev + 1
         WHERE id = ?1",
        params![item.id, tx.now(), answer, via, tx.key()],
    )?;
    tx.event(
        NewEvent::new("attention", &item.id, "attention.answered")
            .rev(item.rev + 1)
            .card(&item.card_id)
            .payload(json!({"answer": answer, "via": via, "still_current": current})),
    )?;

    let card: Card = get(tx.conn(), "card", &item.card_id)?;
    let target = owning_session(tx.conn(), item, &card.project_id)?;
    let message = match target {
        Some(session) => Some(enqueue(
            tx,
            &NewMessage {
                target_session_id: session,
                kind: OutboxKind::Answer,
                body: format!("[{}] {}\nanswer: {answer}", card.display_id, item.question),
                card_id: Some(card.id.clone()),
                reply_to: Some(item.id.clone()),
            },
        )?),
        None => None,
    };

    // A question card closes answered; a finished review pass card closes
    // reported once its review item is read.
    let done = match (card.kind, item.kind) {
        (CardKind::Question, _) => Some(CardOutcome::Answered),
        (CardKind::Scout, AttentionKind::Review)
            if tx.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM review_passes WHERE card_id = ?1 AND state = 'finished')",
                [&card.id],
                |row| row.get::<_, bool>(0),
            )? =>
        {
            Some(CardOutcome::Reported)
        }
        _ => None,
    };
    let closed_card = if let Some(outcome) = done
        && card.state == CardState::Open
    {
        let open_left = super::cards::open_attention_id(tx.conn(), &card.id)?;
        let active = super::cards::active_runs(tx.conn(), &card.id)?;
        if open_left.is_none() && active.is_empty() {
            Some(close_card(tx, &card, outcome, json!({"attention_id": item.id}))?.card)
        } else {
            None
        }
    } else {
        None
    };
    Ok(Answered {
        attention: get(tx.conn(), "attention", &item.id)?,
        still_current: current,
        message,
        closed_card,
    })
}

fn status(item: &Attention) -> AttentionStatus {
    AttentionStatus::from_columns(item.answered_at.is_some(), item.withdrawn_at.is_some())
}

/// Why an open item no longer applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stale {
    pub reason: StaleReason,
    /// The card, run, session or project the reason is about.
    pub subject: Option<String>,
    /// When it happened, when the state records it.
    pub at: Option<Timestamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StaleReason {
    CardClosed,
    RunEnded,
    SessionEnded,
    ProjectDeleted,
    ProjectRetired,
}

impl StaleReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CardClosed => "card_closed",
            Self::RunEnded => "run_ended",
            Self::SessionEnded => "session_ended",
            Self::ProjectDeleted => "project_deleted",
            Self::ProjectRetired => "project_retired",
        }
    }
}

/// `still_current`, derived: open, and not [`stale`].
pub fn still_current(conn: &rusqlite::Connection, item: &Attention) -> Result<bool> {
    if item.answered_at.is_some() || item.withdrawn_at.is_some() {
        return Ok(false);
    }
    Ok(stale(conn, item)?.is_none())
}

/// Why an open item no longer applies, or `None` while it does: its card is
/// closed, the run that opened it ended, the live session it is about ended,
/// or the project it is about was deleted or retired.
pub fn stale(conn: &rusqlite::Connection, item: &Attention) -> Result<Option<Stale>> {
    let (card, state): (String, CardState) = conn.query_row(
        "SELECT display_id, state FROM cards WHERE id = ?1",
        [&item.card_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if state != CardState::Open {
        return Ok(Some(Stale {
            reason: StaleReason::CardClosed,
            subject: Some(card),
            at: None,
        }));
    }
    if let Some(run) = &item.opened_by_run_id {
        let ended: Option<Timestamp> =
            conn.query_row("SELECT ended_at FROM runs WHERE id = ?1", [run], |row| {
                row.get(0)
            })?;
        if ended.is_some() {
            return Ok(Some(Stale {
                reason: StaleReason::RunEnded,
                subject: Some(run.clone()),
                at: ended,
            }));
        }
    }
    if let Some(project) = &item.subject_project_id {
        let row: Option<Option<Timestamp>> = conn
            .query_row(
                "SELECT retired_at FROM projects WHERE id = ?1",
                [project],
                |row| row.get(0),
            )
            .optional()?;
        match row {
            None => {
                let at = conn.query_row(
                    "SELECT max(at) FROM events WHERE kind = 'project.deleted' AND entity_id = ?1",
                    [project],
                    |row| row.get(0),
                )?;
                return Ok(Some(Stale {
                    reason: StaleReason::ProjectDeleted,
                    subject: Some(project.clone()),
                    at,
                }));
            }
            Some(Some(at)) => {
                return Ok(Some(Stale {
                    reason: StaleReason::ProjectRetired,
                    subject: Some(project.clone()),
                    at: Some(at),
                }));
            }
            Some(None) => {}
        }
    }
    if let Some(session) = &item.subject_session_id {
        let row: Option<Option<Timestamp>> = conn
            .query_row(
                "SELECT ended_at FROM sessions WHERE id = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()?;
        // A session that is gone went with its project, which the check
        // above names when the item says which project that was.
        if !matches!(row, Some(None)) {
            return Ok(Some(Stale {
                reason: StaleReason::SessionEnded,
                subject: Some(session.clone()),
                at: row.flatten(),
            }));
        }
    }
    Ok(None)
}

/// The session an answer goes to: the live session that opened the item,
/// else the project's live orchestrator.
fn owning_session(
    conn: &rusqlite::Connection,
    item: &Attention,
    project_id: &str,
) -> Result<Option<String>> {
    if let Some(session) = &item.opened_by_session_id {
        let live: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = ?1 AND ended_at IS NULL)",
            [session],
            |row| row.get(0),
        )?;
        if live {
            return Ok(Some(session.clone()));
        }
    }
    Ok(live_orchestrator(conn, project_id)?.map(|session| session.id))
}

impl Store {
    /// `attention.open`. Keyed.
    pub fn attention_open(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        new: NewAttention,
    ) -> Result<Applied<Attention>> {
        self.write(ctx, "attention.open", key, |tx| open_attention(tx, &new))
    }

    /// `attention.answer`. Keyed: the key is also stored on the row.
    pub fn attention_answer(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        item: &str,
        answer: &str,
        via: AnsweredVia,
    ) -> Result<Applied<Answered>> {
        self.write(ctx, "attention.answer", key, |tx| {
            let id = resolve_id(tx.conn(), "attention", "attention", item)?;
            let item: Attention = get(tx.conn(), "attention", &id)?;
            if item.kind == AttentionKind::Merge {
                return Err(Error::refused(
                    "attention",
                    &item.id,
                    "a merge item is answered by deciding its approval (tiller approve)",
                ));
            }
            answer_attention(tx, &item, answer, via)
        })
    }

    /// `attention.withdraw`. Natural: an item already withdrawn is returned.
    pub fn attention_withdraw(
        &mut self,
        ctx: &Ctx,
        item: &str,
        reason: &str,
    ) -> Result<Applied<Attention>> {
        self.withdraw(ctx, item, reason, false)
    }

    /// The owner's withdraw: refused while the question still applies, since
    /// a withdrawn item is never answered and the orchestrator would wait on
    /// nothing.
    pub fn attention_withdraw_stale(
        &mut self,
        ctx: &Ctx,
        item: &str,
        reason: &str,
    ) -> Result<Applied<Attention>> {
        self.withdraw(ctx, item, reason, true)
    }

    fn withdraw(
        &mut self,
        ctx: &Ctx,
        item: &str,
        reason: &str,
        only_stale: bool,
    ) -> Result<Applied<Attention>> {
        self.write(ctx, "attention.withdraw", None, |tx| {
            let id = resolve_id(tx.conn(), "attention", "attention", item)?;
            let item: Attention = get(tx.conn(), "attention", &id)?;
            if item.withdrawn_at.is_some() {
                return Ok(item);
            }
            if only_stale && still_current(tx.conn(), &item)? {
                return Err(Error::refused(
                    "attention",
                    &id,
                    "the question still applies; answer it instead",
                ));
            }
            withdraw_attention(tx, &item, reason)
        })
    }
}
