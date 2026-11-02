//! Attention items: `attention.open`, `attention.answer`, `attention.withdraw`
//! (`state.md`, Attention).

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::cards::close_card;
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::{get, refuse, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};

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
    tx.conn().execute(
        "INSERT INTO attention (id, card_id, kind, question, evidence, recommendation, consequence, options,
                                opened_at, opened_by_session_id, opened_by_run_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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

    let closed_card = if card.kind == CardKind::Question && card.state == CardState::Open {
        let open_left = super::cards::open_attention_id(tx.conn(), &card.id)?;
        let active = super::cards::active_runs(tx.conn(), &card.id)?;
        if open_left.is_none() && active.is_empty() {
            Some(
                close_card(
                    tx,
                    &card,
                    CardOutcome::Answered,
                    json!({"attention_id": item.id}),
                )?
                .card,
            )
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

/// `still_current`, derived: open, card open, and the run that opened it is
/// still active (or it was not opened by a run).
pub fn still_current(conn: &rusqlite::Connection, item: &Attention) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT a.answered_at IS NULL AND a.withdrawn_at IS NULL AND c.state = 'open'
                AND (a.opened_by_run_id IS NULL OR r.ended_at IS NULL)
         FROM attention a JOIN cards c ON c.id = a.card_id LEFT JOIN runs r ON r.id = a.opened_by_run_id
         WHERE a.id = ?1",
        [&item.id],
        |row| row.get(0),
    )?)
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
        self.write(ctx, "attention.withdraw", None, |tx| {
            let id = resolve_id(tx.conn(), "attention", "attention", item)?;
            let item: Attention = get(tx.conn(), "attention", &id)?;
            if item.withdrawn_at.is_some() {
                return Ok(item);
            }
            withdraw_attention(tx, &item, reason)
        })
    }
}
