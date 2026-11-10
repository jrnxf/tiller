//! Attention items: `attention.open`, `attention.answer`, `attention.withdraw`
//! (`state.md`, Attention).

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::cards::{REOPEN_WORK, close_card, insert_hold, release_hold};
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::projects::{ProjectEdit, edit_project, positive};
use super::records::cancel_obligation;
use super::{find, get, list, refuse, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
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
/// message that carries it to the owning session, the card it closed and the
/// owner action it applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Answered {
    pub attention: Attention,
    pub still_current: bool,
    pub message: Option<OutboxMessage>,
    pub closed_card: Option<Card>,
    pub applied: Option<OwnerAction>,
}

/// An owner-only change an option carries (`state.md`, Attention): picking
/// that option on this host's board or in an owner terminal applies it in
/// the answer's transaction. The orchestrator offers it; only the owner
/// applies it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnerAction {
    /// `project.edit` of one project's capacity, bound to the capacity the
    /// question saw.
    ProjectCapacity { project: String, from: i64, to: i64 },
    /// Start a `role` run on the card past the quota floor, and release the
    /// card's quota hold so the orchestrator may spawn it.
    QuotaStart { role: RunRole },
}

impl OwnerAction {
    /// What the action changes, in the words the answer message uses.
    pub fn describe(&self) -> String {
        match self {
            Self::ProjectCapacity { project, from, to } => {
                format!("project {project} capacity {from} to {to}")
            }
            Self::QuotaStart { role } => format!("start the {role} run past the quota floor"),
        }
    }

    /// The action an option carries. Lenient, because `attention.open`
    /// refused a malformed one and an item opened before actions existed may
    /// hold any `action` value: that option applies nothing.
    pub fn of_option(option: &Value) -> Option<Self> {
        option
            .get("action")
            .and_then(|action| serde_json::from_value(action.clone()).ok())
    }

    /// Refuses an action that cannot apply to the record as it is now, or
    /// that reaches past the card's own project. Returns the project.
    fn check(&self, conn: &rusqlite::Connection, card: &Card) -> Result<Project> {
        match self {
            Self::ProjectCapacity { project, from, to } => {
                if *project != card.project_id {
                    return Err(Error::Usage(format!(
                        "the action names project {project}; {} is in {}",
                        card.display_id, card.project_id
                    )));
                }
                let found: Project = get(conn, "project", project)?;
                if found.retired_at.is_some() {
                    return Err(Error::refused("project", project, "project is retired"));
                }
                if found.capacity != *from {
                    return Err(Error::refused(
                        "project",
                        project,
                        format!(
                            "capacity is {} now, not {from} as the question says",
                            found.capacity
                        ),
                    ));
                }
                positive("capacity", *to)?;
                if to == from {
                    return Err(Error::Usage(format!(
                        "project {project} capacity is already {to}"
                    )));
                }
                Ok(found)
            }
            Self::QuotaStart { .. } => {
                if card.state != CardState::Open {
                    return Err(Error::refused("card", &card.display_id, "card is closed"));
                }
                get(conn, "project", &card.project_id)
            }
        }
    }

    fn apply(&self, tx: &mut Tx<'_>, card: &Card) -> Result<()> {
        let project = self.check(tx.conn(), card)?;
        match self {
            Self::ProjectCapacity { to, .. } => {
                let edit = ProjectEdit {
                    capacity: Some(*to),
                    ..ProjectEdit::default()
                };
                edit_project(tx, &project.id, project.rev, &edit)?;
            }
            Self::QuotaStart { .. } => {
                if let Some(hold) = find::<Hold>(
                    tx.conn(),
                    "card_id = ?1 AND released_at IS NULL AND reason = ?2",
                    &[&card.id, &QUOTA_HOLD_REASON],
                )? {
                    release_hold(tx, &hold, HoldReleaseReason::Removed)?;
                }
            }
        }
        Ok(())
    }
}

/// An option's label: a bare string or the `label` of `{label, consequence,
/// recommended, action}`.
fn option_label(option: &Value) -> &str {
    option
        .as_str()
        .or_else(|| option.get("label").and_then(Value::as_str))
        .unwrap_or_default()
}

/// Whether the answer picks the option labelled `label`: its first line is
/// the label, ignoring case and surrounding space. The board sends the label,
/// then the owner's note after a blank line.
fn picks(answer: &str, label: &str) -> bool {
    let label = label.trim();
    let first = answer.lines().next().unwrap_or_default().trim();
    !label.is_empty() && first.to_lowercase() == label.to_lowercase()
}

/// Checks the options' owner actions at open: each is well formed, names a
/// label no other option shares, and applies to the record as it is.
fn check_options(conn: &rusqlite::Connection, card: &Card, options: Option<&Value>) -> Result<()> {
    let Some(Value::Array(options)) = options else {
        return Ok(());
    };
    for (index, option) in options.iter().enumerate() {
        let Some(action) = option.get("action").filter(|action| !action.is_null()) else {
            continue;
        };
        let action: OwnerAction = serde_json::from_value(action.clone())
            .map_err(|error| Error::Usage(format!("option {} action: {error}", index + 1)))?;
        let label = option_label(option);
        if label.trim().is_empty() {
            return Err(Error::Usage(format!(
                "option {} carries an action and needs a label",
                index + 1
            )));
        }
        let twins = options
            .iter()
            .filter(|other| picks(label, option_label(other)))
            .count();
        if twins > 1 {
            return Err(Error::Usage(format!(
                "option {label:?} carries an action and is offered twice"
            )));
        }
        action.check(conn, card)?;
    }
    Ok(())
}

/// The owner action of the option the answer picked. `None` when the item
/// carries no actions; `Some(None)` when it does and the answer picked none.
fn picked_action(item: &Attention, answer: &str) -> Option<Option<OwnerAction>> {
    let Some(Value::Array(options)) = item.options.as_ref() else {
        return None;
    };
    let actions: Vec<_> = options
        .iter()
        .map(|option| (option_label(option), OwnerAction::of_option(option)))
        .collect();
    if actions.iter().all(|(_, action)| action.is_none()) {
        return None;
    }
    Some(
        actions
            .into_iter()
            .find(|(label, _)| picks(answer, label))
            .and_then(|(_, action)| action),
    )
}

/// The label that starts the run past the quota floor.
pub const QUOTA_START_LABEL: &str = "Start now anyway";
/// The label that keeps the hold until the reset.
pub const QUOTA_WAIT_LABEL: &str = "Wait for the reset";

/// The reason of the hold a quota refusal sets.
pub const QUOTA_HOLD_REASON: &str = "the weekly quota is under the floor";

/// The two options of a quota hold's decision.
fn quota_options(role: RunRole) -> Value {
    json!([
        {
            "label": QUOTA_START_LABEL,
            "consequence": "The run starts now, below the quota floor, and the hold ends.",
            "action": OwnerAction::QuotaStart { role },
        },
        {
            "label": QUOTA_WAIT_LABEL,
            "consequence": "The card stays held and comes back for a decision after the reset.",
            "recommended": true,
        },
    ])
}

/// The card's open items that carry a quota start option.
fn open_quota_items(conn: &rusqlite::Connection, card_id: &str) -> Result<Vec<Attention>> {
    let open: Vec<Attention> = list(
        conn,
        "card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL ORDER BY opened_at, id",
        &[&card_id],
    )?;
    Ok(open
        .into_iter()
        .filter(|item| quota_role(item, None).is_some())
        .collect())
}

/// The role a quota start option of `item` names. With `answer`, only when
/// that answer picks the option.
fn quota_role(item: &Attention, answer: Option<&str>) -> Option<RunRole> {
    let Some(Value::Array(options)) = item.options.as_ref() else {
        return None;
    };
    options.iter().find_map(|option| {
        let Some(OwnerAction::QuotaStart { role }) = OwnerAction::of_option(option) else {
            return None;
        };
        answer
            .is_none_or(|answer| picks(answer, option_label(option)))
            .then_some(role)
    })
}

/// Withdraws the card's open quota items.
pub(crate) fn withdraw_quota_items(tx: &mut Tx<'_>, card_id: &str, reason: &str) -> Result<()> {
    for item in open_quota_items(tx.conn(), card_id)? {
        withdraw_attention(tx, &item, reason)?;
    }
    Ok(())
}

/// The card's newest answered quota item for a `role` run.
fn newest_quota_answer(
    conn: &rusqlite::Connection,
    card_id: &str,
    role: RunRole,
) -> Result<Option<Attention>> {
    let answered: Vec<Attention> = list(
        conn,
        "card_id = ?1 AND answered_at IS NOT NULL AND withdrawn_at IS NULL ORDER BY answered_at DESC, id DESC",
        &[&card_id],
    )?;
    Ok(answered
        .into_iter()
        .find(|item| quota_role(item, None) == Some(role)))
}

/// Whether the newest quota answer picks "Start now anyway".
fn starts(item: &Attention, role: RunRole) -> bool {
    item.answer
        .as_deref()
        .is_some_and(|answer| quota_role(item, Some(answer)) == Some(role))
}

/// The owner's answered decision to start a `role` run on the card past the
/// quota floor: the newest quota answer for that role, when it is "Start now
/// anyway" and no `role` run has started on the card since. Only the owner
/// applies that option, so an answered item is the owner's decision.
pub fn quota_start_decision(
    conn: &rusqlite::Connection,
    card_id: &str,
    role: RunRole,
) -> Result<Option<Attention>> {
    let Some(item) = newest_quota_answer(conn, card_id, role)? else {
        return Ok(None);
    };
    if !starts(&item, role) {
        return Ok(None);
    }
    let used: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'run.started' AND card_id = ?1
                        AND json_extract(payload, '$.role') = ?2
                        AND seq > (SELECT MAX(seq) FROM events WHERE entity = 'attention'
                                   AND entity_id = ?3 AND kind = 'attention.answered'))",
        params![card_id, role, item.id],
        |row| row.get(0),
    )?;
    Ok((!used).then_some(item))
}

impl Store {
    /// A start refused by the quota floor: holds the card until `until` (the
    /// quota reset) and opens one decision on it, "Start now anyway" or
    /// "Wait for the reset". Natural: a card that already has an open quota
    /// decision gets no second one, and while the quota hold the owner chose
    /// to wait on is active, the newest answer stands and none opens. A card
    /// held for another reason keeps that hold and still gets the decision.
    pub fn quota_hold(
        &mut self,
        ctx: &Ctx,
        card: &str,
        role: RunRole,
        until: Timestamp,
        why: &str,
    ) -> Result<Applied<Attention>> {
        self.write(ctx, "quota.hold", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            match tx.actor() {
                Actor::Owner | Actor::Board => {}
                Actor::Session(id)
                    if live_orchestrator(tx.conn(), &card.project_id)?
                        .is_some_and(|live| live.id == *id) => {}
                other => {
                    return Err(Error::Forbidden(format!(
                        "{other} may not hold {} for the quota",
                        card.display_id
                    )));
                }
            }
            if let Some(item) = open_quota_items(tx.conn(), &card.id)?.into_iter().next() {
                return Ok(item);
            }
            let held = find::<Hold>(
                tx.conn(),
                "card_id = ?1 AND released_at IS NULL",
                &[&card.id],
            )?;
            if let Some(hold) = held.as_ref().filter(|hold| hold.reason == QUOTA_HOLD_REASON)
                && let Some(item) = newest_quota_answer(tx.conn(), &card.id, role)?
                && item.answered_at.is_some_and(|at| at >= hold.set_at)
                && !starts(&item, role)
            {
                return Ok(item);
            }
            if held.is_none() {
                if until <= tx.now() {
                    return Err(Error::refused(
                        "card",
                        &card.display_id,
                        format!("{until} is not in the future"),
                    ));
                }
                insert_hold(tx, &card, until, QUOTA_HOLD_REASON)?;
            }
            // A due quota hold's re-ask is the same question; this decision
            // replaces it, so the card keeps one open item.
            let reasks: Vec<Attention> = list(
                tx.conn(),
                "card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL AND options IS NULL AND question = ?2",
                &[&card.id, &QUOTA_HOLD_REASON],
            )?;
            for item in reasks {
                withdraw_attention(tx, &item, "the quota floor refused the run again")?;
            }
            open_attention(
                tx,
                &NewAttention {
                    card: card.id.clone(),
                    question: format!(
                        "The weekly quota is under the floor, so the {role} run for {} did not start. Start it now anyway, or wait for the reset?",
                        card.display_id
                    ),
                    evidence: Some(why.to_owned()),
                    recommendation: Some(QUOTA_WAIT_LABEL.to_owned()),
                    options: Some(quota_options(role)),
                    ..NewAttention::default()
                },
            )
        })
    }
}

/// `attention.open` inside a mutation.
pub(crate) fn open_attention(tx: &mut Tx<'_>, new: &NewAttention) -> Result<Attention> {
    required("question", &new.question)?;
    let card = resolve_card(tx.conn(), &new.card)?;
    check_options(tx.conn(), &card, new.options.as_ref())?;
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

/// "Reopen work" on a deploy `recovery` item cancels the watch the item
/// names, so a later deploy run can no longer close the card (`state.md` 7).
fn reopen_work(tx: &mut Tx<'_>, item: &Attention) -> Result<()> {
    let watch = item
        .evidence
        .as_deref()
        .and_then(|evidence| serde_json::from_str::<Value>(evidence).ok())
        .and_then(|evidence| evidence.get("obligation_id")?.as_str().map(str::to_owned));
    let Some(watch) = watch else {
        return Ok(());
    };
    if let Some(obligation) =
        find::<Obligation>(tx.conn(), "id = ?1 AND state = 'open'", &[&watch])?
    {
        cancel_obligation(tx, &obligation, REOPEN_WORK)?;
    }
    Ok(())
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
                applied: None,
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
    let picked = picked_action(item, answer);
    let applied = picked.clone().flatten();
    if let Some(action) = &applied {
        // The forwarded owner has no project commands (`authority.md`
        // section 3), and the orchestrator only records the owner's words.
        if !matches!(tx.actor(), Actor::Owner | Actor::Board) {
            return Err(Error::Forbidden(format!(
                "{answer:?} applies {}, an owner-only change: the owner picks it on this host's board or in an owner terminal; {} may not",
                action.describe(),
                tx.actor()
            )));
        }
        if !current {
            return Err(Error::refused(
                "attention",
                &item.id,
                format!(
                    "the question is no longer current, so {} does not apply: pick another option or withdraw it",
                    action.describe()
                ),
            ));
        }
    }
    tx.conn().execute(
        "UPDATE attention SET answered_at = ?2, answer = ?3, answered_via = ?4, answer_idempotency_key = ?5,
                rev = rev + 1
         WHERE id = ?1",
        params![item.id, tx.now(), answer, via, tx.key()],
    )?;
    let mut payload = json!({"answer": answer, "via": via, "still_current": current});
    if let Some(action) = &applied {
        payload["applied"] = json!(action);
    }
    tx.event(
        NewEvent::new("attention", &item.id, "attention.answered")
            .rev(item.rev + 1)
            .card(&item.card_id)
            .payload(payload),
    )?;

    let card: Card = get(tx.conn(), "card", &item.card_id)?;
    // After the answer's event, so the answer's key belongs to the item.
    if let Some(action) = &applied {
        action.apply(tx, &card)?;
    }
    if item.kind == AttentionKind::Recovery
        && answer
            .trim()
            .to_lowercase()
            .starts_with(&REOPEN_WORK.to_lowercase())
    {
        reopen_work(tx, item)?;
    }
    let target = owning_session(tx.conn(), item, &card.project_id)?;
    let message = match target {
        Some(session) => Some(enqueue(
            tx,
            &NewMessage {
                target_session_id: session,
                kind: OutboxKind::Answer,
                body: match &picked {
                    None => format!("[{}] {}\nanswer: {answer}", card.display_id, item.question),
                    Some(applied) => format!(
                        "[{}] {}\nanswer: {answer}\napplied: {}",
                        card.display_id,
                        item.question,
                        applied
                            .as_ref()
                            .map_or_else(|| "nothing".into(), OwnerAction::describe)
                    ),
                },
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
        applied,
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

#[cfg(test)]
mod tests;
