//! Cards, holds and dependencies: `card.*`, `hold.release`, `dep.*`
//! (`state.md` sections 3, 7 and 8).

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::approvals::mark_stale;
use super::attention::{NewAttention, open_attention, withdraw_attention};
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::runs::{EndRun, end_run};
use super::{check_rev, find, get, list, refuse, required, resolve_card};
use crate::domain::lifecycle::{allowed_outcomes, outcome_satisfies_dependants};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, HOST_HEALTH_CARD};
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// A card to create.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewCard {
    pub project_id: String,
    pub kind: CardKind,
    pub title: String,
    pub body: Option<String>,
    pub priority: Option<i64>,
    pub category: Option<String>,
    pub size: Option<Size>,
    pub acceptance: Option<String>,
    /// A card reference (display id, ULID or short form).
    pub fixes_card: Option<String>,
    /// The Firstmate task id, on import.
    pub legacy_id: Option<String>,
}

impl Default for NewCard {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            kind: CardKind::Ship,
            title: String::new(),
            body: None,
            priority: None,
            category: None,
            size: None,
            acceptance: None,
            fixes_card: None,
            legacy_id: None,
        }
    }
}

/// The owner's per-card override of the dispatch rules (CP2, C04).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchOverride {
    pub model: String,
    pub effort: Effort,
}

/// The editable card fields; `None` leaves a field as it is, and
/// `Some(None)` clears an optional one.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CardEdit {
    pub title: Option<String>,
    pub body: Option<Option<String>>,
    pub priority: Option<i64>,
    pub category: Option<Option<String>>,
    pub size: Option<Option<Size>>,
    pub acceptance: Option<Option<String>>,
    /// Owner only.
    pub dispatch_override: Option<Option<DispatchOverride>>,
}

/// A card's close: the card, and the holds and dependencies it released.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Closed {
    pub card: Card,
    pub released_holds: Vec<String>,
    pub released_dependants: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cancelled {
    pub closed: Closed,
    pub ended_runs: Vec<String>,
    pub withdrawn_attention: Vec<String>,
}

fn check_priority(priority: i64) -> Result<()> {
    if (0..=4).contains(&priority) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "priority must be 0 (highest) to 4, got {priority}"
        )))
    }
}

/// `card.create` inside a mutation: mints the display id from the project's
/// counter in this transaction, and links the ask that created it.
pub(crate) fn create_card(tx: &mut Tx<'_>, new: &NewCard, ask_id: Option<&str>) -> Result<Card> {
    required("title", &new.title)?;
    let priority = new.priority.unwrap_or(2);
    check_priority(priority)?;
    let project: Project = get(tx.conn(), "project", &new.project_id)?;
    if project.retired_at.is_some() {
        return Err(Error::refused("project", &project.id, "project is retired"));
    }
    let fixes = match &new.fixes_card {
        Some(reference) => Some(resolve_card(tx.conn(), reference)?.id),
        None => None,
    };
    let seq = project.next_card_seq;
    tx.conn().execute(
        "UPDATE projects SET next_card_seq = next_card_seq + 1, rev = rev + 1 WHERE id = ?1",
        [&project.id],
    )?;
    let id = ids::new_ulid();
    let display_id = ids::display_id(&project.id, seq);
    tx.conn().execute(
        "INSERT INTO cards (id, display_id, legacy_id, project_id, kind, title, body, priority, category,
                            size, acceptance, fixes_card_id, created_at, created_by_ask_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            id,
            display_id,
            new.legacy_id,
            project.id,
            new.kind,
            new.title,
            new.body,
            priority,
            new.category,
            new.size,
            new.acceptance,
            fixes,
            tx.now(),
            ask_id,
        ],
    )?;
    tx.event(
        NewEvent::new("card", &id, "card.created")
            .rev(1)
            .card(&id)
            .payload(json!({
                "display_id": display_id,
                "project_id": project.id,
                "kind": new.kind,
                "title": new.title,
                "ask_id": ask_id,
                "legacy_id": new.legacy_id,
            })),
    )?;
    if let Some(ask_id) = ask_id {
        link_ask(tx, ask_id, &id, LinkRelation::Created)?;
    }
    get(tx.conn(), "card", &id)
}

/// Writes one `ask_cards` row and its `card.linked` event.
pub(crate) fn link_ask(
    tx: &mut Tx<'_>,
    ask_id: &str,
    card_id: &str,
    relation: LinkRelation,
) -> Result<()> {
    let session = tx.actor().session_id().map(str::to_owned);
    tx.conn().execute(
        "INSERT INTO ask_cards (ask_id, card_id, relation, linked_at, linked_by_session_id)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ask_id, card_id, relation, tx.now(), session],
    )?;
    tx.event(
        NewEvent::new("card", card_id, "card.linked")
            .card(card_id)
            .payload(json!({"ask_id": ask_id, "relation": relation})),
    )?;
    Ok(())
}

/// The id of the oldest open attention item on a card.
pub(crate) fn open_attention_id(
    conn: &rusqlite::Connection,
    card_id: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT id FROM attention WHERE card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL
             ORDER BY opened_at, id LIMIT 1",
            [card_id],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn active_runs(conn: &rusqlite::Connection, card_id: &str) -> Result<Vec<Run>> {
    list(
        conn,
        "card_id = ?1 AND ended_at IS NULL ORDER BY started_at, id",
        &[&card_id],
    )
}

fn require_open(card: &Card) -> Result<()> {
    match card.state {
        CardState::Open => Ok(()),
        CardState::Closed => Err(Error::refused(
            "card",
            &card.display_id,
            format!(
                "card is closed {}",
                card.outcome.map_or("", CardOutcome::as_str)
            ),
        )),
    }
}

fn require_not_host_health(card: &Card) -> Result<()> {
    if card.display_id == HOST_HEALTH_CARD {
        Err(Error::refused(
            "card",
            &card.display_id,
            "the host health card never closes",
        ))
    } else {
        Ok(())
    }
}

/// `card.close` inside a mutation, after the caller checked any evidence its
/// outcome needs: no open attention, no active run, an outcome the kind
/// allows. Releases the open hold and, for a satisfying outcome, every
/// dependency on this card.
pub(crate) fn close_card(
    tx: &mut Tx<'_>,
    card: &Card,
    outcome: CardOutcome,
    payload: Value,
) -> Result<Closed> {
    require_not_host_health(card)?;
    if let Some(item) = open_attention_id(tx.conn(), &card.id)? {
        return Err(Error::refused(
            "card",
            &card.display_id,
            format!("attention item {item} is open"),
        ));
    }
    if let Some(run) = active_runs(tx.conn(), &card.id)?.first() {
        return Err(Error::refused(
            "card",
            &card.display_id,
            format!("run {} ({}) is active", run.id, run.role),
        ));
    }
    let status = CardStatus::from_columns(card.state, card.outcome)
        .map_err(refuse("card", &card.display_id))?;
    status
        .close(card.kind, outcome)
        .map_err(refuse("card", &card.display_id))?;

    let now = tx.now();
    let by = tx.actor().to_string();
    tx.conn().execute(
        "UPDATE cards SET state = 'closed', outcome = ?2, closed_at = ?3, closed_by = ?4, rev = rev + 1 WHERE id = ?1",
        params![card.id, outcome, now, by],
    )?;
    tx.event(
        NewEvent::new("card", &card.id, "card.closed")
            .rev(card.rev + 1)
            .card(&card.id)
            .payload(merge(json!({"outcome": outcome}), payload)),
    )?;

    // An approval the closed card was waiting on can no longer execute.
    // Linked memory approvals go stale with the one they ride on.
    let waiting: Vec<Approval> = list(
        tx.conn(),
        "card_id = ?1 AND kind IN ('merge', 'land_direct') AND state IN ('requested', 'accepted')
         AND linked_approval_id IS NULL ORDER BY id",
        &[&card.id],
    )?;
    for approval in waiting {
        mark_stale(tx, &approval, &format!("card closed {outcome}"))?;
    }

    let mut released_holds = Vec::new();
    if let Some(hold) = find::<Hold>(
        tx.conn(),
        "card_id = ?1 AND released_at IS NULL",
        &[&card.id],
    )? {
        release_hold(tx, &hold, HoldReleaseReason::Removed)?;
        released_holds.push(hold.id);
    }

    let mut released_dependants = Vec::new();
    let deps: Vec<CardDep> = list(
        tx.conn(),
        "depends_on_card_id = ?1 AND released_at IS NULL ORDER BY added_at",
        &[&card.id],
    )?;
    if outcome_satisfies_dependants(outcome) {
        let reason: DepReleaseReason = outcome.as_str().parse()?;
        for dep in deps {
            release_dep(tx, &dep, reason)?;
            released_dependants.push(dep.card_id);
        }
    } else {
        // The work a dependant waited on was not delivered: the dependency
        // stays, and the owner decides (`state.md`, Dependency).
        for dep in deps {
            let waiting: Card = get(tx.conn(), "card", &dep.card_id)?;
            if waiting.state == CardState::Open {
                open_attention(
                    tx,
                    &NewAttention {
                        card: waiting.id.clone(),
                        kind: AttentionKind::Decision,
                        question: format!(
                            "{} waits on {}, which closed {outcome}. Drop the dependency, or cancel {}?",
                            waiting.display_id, card.display_id, waiting.display_id
                        ),
                        ..NewAttention::default()
                    },
                )?;
            }
        }
    }
    Ok(Closed {
        card: get(tx.conn(), "card", &card.id)?,
        released_holds,
        released_dependants,
    })
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

pub(crate) fn release_hold(tx: &mut Tx<'_>, hold: &Hold, reason: HoldReleaseReason) -> Result<()> {
    tx.conn().execute(
        "UPDATE holds SET released_at = ?2, release_reason = ?3 WHERE id = ?1",
        params![hold.id, tx.now(), reason],
    )?;
    tx.event(
        NewEvent::new("hold", &hold.id, "hold.released")
            .card(&hold.card_id)
            .payload(json!({"reason": reason, "until_at": hold.until_at})),
    )?;
    Ok(())
}

fn release_dep(tx: &mut Tx<'_>, dep: &CardDep, reason: DepReleaseReason) -> Result<()> {
    tx.conn().execute(
        "UPDATE card_deps SET released_at = ?3, release_reason = ?4 WHERE card_id = ?1 AND depends_on_card_id = ?2",
        params![dep.card_id, dep.depends_on_card_id, tx.now(), reason],
    )?;
    tx.event(
        NewEvent::new(
            "dep",
            format!("{}:{}", dep.card_id, dep.depends_on_card_id),
            "dep.released",
        )
        .card(&dep.card_id)
        .payload(json!({"depends_on": dep.depends_on_card_id, "reason": reason})),
    )?;
    Ok(())
}

/// The option on a `recovery` item that confirms a change landed outside an
/// approval: an outside merge, or a deploy fixed by hand (`state.md` 7).
pub const MARK_LANDED: &str = "Mark landed";

/// Whether the card has the evidence a `landed` outcome needs.
pub(crate) fn landed_evidence(conn: &rusqlite::Connection, card: &Card) -> Result<bool> {
    // An approval reached `succeeded`, or the owner confirmed an outside merge
    // by answering a `recovery` item on the card with "Mark landed", alone or
    // qualified ("Mark landed, not me"; `recovery.md` R07). Any other answer
    // is not evidence.
    let approved: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM approvals WHERE card_id = ?1 AND kind IN ('merge', 'land_direct') AND state = 'succeeded')",
        [&card.id],
        |row| row.get(0),
    )?;
    if approved {
        return Ok(true);
    }
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM attention WHERE card_id = ?1 AND kind = 'recovery'
                        AND lower(trim(answer)) LIKE lower(?2) || '%')",
        params![card.id, MARK_LANDED],
        |row| row.get(0),
    )?)
}

impl Store {
    /// `card.create` (`tiller card create`). Keyed. Without an ask the card
    /// has no `created_by_ask_id` and no `ask_cards` row.
    pub fn card_create(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        new: NewCard,
        ask: Option<&str>,
    ) -> Result<Applied<Card>> {
        self.write(ctx, "card.create", key, |tx| {
            let ask_id = match ask {
                Some(reference) => {
                    let id = super::resolve_id(tx.conn(), "asks", "ask", reference)?;
                    let ask: Ask = get(tx.conn(), "ask", &id)?;
                    if ask.project_id != new.project_id {
                        return Err(Error::refused(
                            "ask",
                            &ask.id,
                            format!("ask belongs to project {}", ask.project_id),
                        ));
                    }
                    Some(id)
                }
                None => None,
            };
            create_card(tx, &new, ask_id.as_deref())
        })
    }

    /// `card.edit` against `expected_rev`. A stale `rev` is a conflict even
    /// when the fields already hold the sent values; only a known `key`
    /// replays. An edit at the current `rev` that changes nothing is a no-op.
    /// The dispatch override is owner only.
    pub fn card_edit(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        card: &str,
        expected_rev: i64,
        edit: CardEdit,
    ) -> Result<Applied<Card>> {
        if edit.dispatch_override.is_some() && !ctx.actor.is_owner() {
            return Err(Error::Forbidden(format!(
                "the dispatch override is owner only; {} may not set it",
                ctx.actor
            )));
        }
        if let Some(title) = &edit.title {
            required("title", title)?;
        }
        if let Some(priority) = edit.priority {
            check_priority(priority)?;
        }
        self.write(ctx, "card.edit", key, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            let dispatch = match &edit.dispatch_override {
                Some(value) => Some(value.as_ref().map(serde_json::to_value).transpose()?),
                None => None,
            };
            let mut next = card.clone();
            let mut changed = serde_json::Map::new();
            let mut named = None;
            macro_rules! apply {
                ($field:ident, $value:expr) => {
                    if let Some(value) = $value {
                        named.get_or_insert(stringify!($field));
                        if next.$field != value {
                            changed.insert(stringify!($field).into(), json!(value));
                            next.$field = value;
                        }
                    }
                };
            }
            apply!(title, edit.title.clone());
            apply!(body, edit.body.clone());
            apply!(priority, edit.priority);
            apply!(category, edit.category.clone());
            apply!(size, edit.size);
            apply!(acceptance, edit.acceptance.clone());
            apply!(dispatch_override, dispatch);
            let Some(named) = named else {
                return Ok(card);
            };
            // Two writers that compute the same value from one stale read must
            // not both succeed, so the rev is checked before the no-op.
            let first = changed.keys().next().map_or(named, String::as_str);
            let current = serde_json::to_value(&card)?;
            check_rev(
                "card",
                &card.id,
                card.rev,
                expected_rev,
                Some((first, current[first].clone())),
            )?;
            if changed.is_empty() {
                return Ok(card);
            }
            require_open(&card)?;
            tx.conn().execute(
                "UPDATE cards SET title = ?2, body = ?3, priority = ?4, category = ?5, size = ?6,
                        acceptance = ?7, dispatch_override = ?8, rev = rev + 1
                 WHERE id = ?1",
                params![
                    card.id,
                    next.title,
                    next.body,
                    next.priority,
                    next.category,
                    next.size,
                    next.acceptance,
                    next.dispatch_override,
                ],
            )?;
            tx.event(
                NewEvent::new("card", &card.id, "card.edited")
                    .rev(card.rev + 1)
                    .card(&card.id)
                    .payload(json!({"changed": changed, "expected_rev": expected_rev})),
            )?;
            get(tx.conn(), "card", &card.id)
        })
    }

    /// `card.hold`: a dated park. Natural: an open hold with the same date and
    /// reason is returned.
    pub fn card_hold(
        &mut self,
        ctx: &Ctx,
        card: &str,
        until: Timestamp,
        reason: &str,
    ) -> Result<Applied<Hold>> {
        required("reason", reason)?;
        self.write(ctx, "card.hold", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            if let Some(hold) = find::<Hold>(tx.conn(), "card_id = ?1 AND released_at IS NULL", &[&card.id])? {
                if hold.until_at == until && hold.reason == reason {
                    return Ok(hold);
                }
                return Err(Error::refused(
                    "card",
                    &card.display_id,
                    format!("hold {} is open until {}", hold.id, hold.until_at),
                ));
            }
            require_open(&card)?;
            if until <= tx.now() {
                return Err(Error::refused("card", &card.display_id, format!("{until} is not in the future")));
            }
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO holds (id, card_id, until_at, reason, set_at, set_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, card.id, until, reason, tx.now(), tx.actor().to_string()],
            )?;
            tx.event(
                NewEvent::new("hold", &id, "hold.set")
                    .card(&card.id)
                    .payload(json!({"until_at": until, "reason": reason})),
            )?;
            get(tx.conn(), "hold", &id)
        })
    }

    /// `hold.release`. Natural: a card with no open hold returns `None`. A
    /// `due` release (reconcile, when `until_at` passed) re-asks: it opens a
    /// `decision` item with the hold's reason as the question.
    pub fn hold_release(
        &mut self,
        ctx: &Ctx,
        card: &str,
        reason: HoldReleaseReason,
    ) -> Result<Applied<Option<Hold>>> {
        self.write(ctx, "hold.release", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            let Some(hold) = find::<Hold>(
                tx.conn(),
                "card_id = ?1 AND released_at IS NULL",
                &[&card.id],
            )?
            else {
                return Ok(None);
            };
            if reason == HoldReleaseReason::Due && hold.until_at > tx.now() {
                return Err(Error::refused(
                    "hold",
                    &hold.id,
                    format!("hold is not due until {}", hold.until_at),
                ));
            }
            release_hold(tx, &hold, reason)?;
            if reason == HoldReleaseReason::Due && card.state == CardState::Open {
                open_attention(
                    tx,
                    &NewAttention {
                        card: card.id.clone(),
                        kind: AttentionKind::Decision,
                        question: hold.reason.clone(),
                        evidence: Some(format!("parked until {}", hold.until_at)),
                        ..NewAttention::default()
                    },
                )?;
            }
            Ok(Some(get(tx.conn(), "hold", &hold.id)?))
        })
    }

    /// `dep.add`: `card` waits on `depends_on`. Natural: an unreleased
    /// dependency is returned. Refuses a cycle and a closed dependency.
    pub fn dep_add(&mut self, ctx: &Ctx, card: &str, depends_on: &str) -> Result<Applied<CardDep>> {
        self.write(ctx, "dep.add", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            let target = resolve_card(tx.conn(), depends_on)?;
            let existing: Option<CardDep> = find(
                tx.conn(),
                "card_id = ?1 AND depends_on_card_id = ?2",
                &[&card.id, &target.id],
            )?;
            if let Some(dep) = &existing
                && dep.released_at.is_none() {
                    return Ok(dep.clone());
                }
            if card.id == target.id {
                return Err(Error::refused("card", &card.display_id, "a card cannot wait on itself"));
            }
            require_open(&card)?;
            if target.state != CardState::Open {
                return Err(Error::refused("card", &target.display_id, "the dependency is already closed"));
            }
            let cycle: bool = tx.conn().query_row(
                "WITH RECURSIVE upstream(id) AS (
                     SELECT ?1
                     UNION
                     SELECT d.depends_on_card_id FROM card_deps d JOIN upstream u ON d.card_id = u.id
                     WHERE d.released_at IS NULL
                 )
                 SELECT EXISTS (SELECT 1 FROM upstream WHERE id = ?2)",
                params![target.id, card.id],
                |row| row.get(0),
            )?;
            if cycle {
                return Err(Error::refused(
                    "card",
                    &card.display_id,
                    format!("{} already waits on {}", target.display_id, card.display_id),
                ));
            }
            let by = tx.actor().to_string();
            if existing.is_some() {
                tx.conn().execute(
                    "UPDATE card_deps SET added_at = ?3, added_by = ?4, released_at = NULL, release_reason = NULL
                     WHERE card_id = ?1 AND depends_on_card_id = ?2",
                    params![card.id, target.id, tx.now(), by],
                )?;
            } else {
                tx.conn().execute(
                    "INSERT INTO card_deps (card_id, depends_on_card_id, added_at, added_by) VALUES (?1, ?2, ?3, ?4)",
                    params![card.id, target.id, tx.now(), by],
                )?;
            }
            tx.event(
                NewEvent::new("dep", format!("{}:{}", card.id, target.id), "dep.added")
                    .card(&card.id)
                    .payload(json!({"depends_on": target.id, "depends_on_display_id": target.display_id})),
            )?;
            find(tx.conn(), "card_id = ?1 AND depends_on_card_id = ?2", &[&card.id, &target.id])?
                .ok_or_else(|| Error::Internal("dependency vanished".into()))
        })
    }

    /// `dep.release` by hand (`removed`). Natural: no unreleased dependency
    /// returns `None`.
    pub fn dep_release(
        &mut self,
        ctx: &Ctx,
        card: &str,
        depends_on: &str,
    ) -> Result<Applied<Option<CardDep>>> {
        self.write(ctx, "dep.release", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            let target = resolve_card(tx.conn(), depends_on)?;
            let Some(dep) = find::<CardDep>(
                tx.conn(),
                "card_id = ?1 AND depends_on_card_id = ?2 AND released_at IS NULL",
                &[&card.id, &target.id],
            )?
            else {
                return Ok(None);
            };
            release_dep(tx, &dep, DepReleaseReason::Removed)?;
            find(
                tx.conn(),
                "card_id = ?1 AND depends_on_card_id = ?2",
                &[&card.id, &target.id],
            )
        })
    }

    /// `card.close` with an outcome (`tiller card close`, `card complete`).
    ///
    /// Natural: a card already closed with the same outcome is returned.
    /// `landed` needs a succeeded approval or a confirmed outside merge;
    /// `reported` needs a report artifact; `completed` needs evidence.
    pub fn card_close(
        &mut self,
        ctx: &Ctx,
        card: &str,
        outcome: CardOutcome,
        reason: Option<&str>,
        evidence: Option<&str>,
    ) -> Result<Applied<Closed>> {
        self.write(ctx, "card.close", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            if card.outcome == Some(outcome) {
                return Ok(Closed {
                    card,
                    released_holds: Vec::new(),
                    released_dependants: Vec::new(),
                });
            }
            require_open(&card)?;
            if !allowed_outcomes(card.kind).contains(&outcome) {
                // Let the lifecycle name the allowed outcomes.
                CardStatus::Open.close(card.kind, outcome).map_err(refuse("card", &card.display_id))?;
            }
            match outcome {
                CardOutcome::Landed if !landed_evidence(tx.conn(), &card)? => {
                    return Err(Error::refused(
                        "card",
                        &card.display_id,
                        "landed needs a succeeded merge or land approval, or a recovery item answered \"Mark landed\"",
                    ));
                }
                CardOutcome::Reported => {
                    let report: bool = tx.conn().query_row(
                        "SELECT EXISTS (SELECT 1 FROM artifacts WHERE card_id = ?1 AND kind = 'report' AND retired_at IS NULL)",
                        [&card.id],
                        |row| row.get(0),
                    )?;
                    if !report {
                        return Err(Error::refused("card", &card.display_id, "no report artifact is recorded on the card"));
                    }
                }
                CardOutcome::Completed => required("evidence", evidence.unwrap_or_default())?,
                CardOutcome::Cancelled
                | CardOutcome::Superseded
                | CardOutcome::Failed
                | CardOutcome::Abandoned => required("reason", reason.unwrap_or_default())?,
                _ => {}
            }
            close_card(tx, &card, outcome, json!({"reason": reason, "evidence": evidence}))
        })
    }

    /// `card.cancel`: ends the card's active runs `cancelled`, releases their
    /// reservations, withdraws its open attention and closes it `cancelled`.
    /// Natural: a card already cancelled is returned.
    pub fn card_cancel(
        &mut self,
        ctx: &Ctx,
        card: &str,
        reason: &str,
    ) -> Result<Applied<Cancelled>> {
        required("reason", reason)?;
        self.write(ctx, "card.cancel", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            if card.outcome == Some(CardOutcome::Cancelled) {
                return Ok(Cancelled {
                    closed: Closed {
                        card,
                        released_holds: Vec::new(),
                        released_dependants: Vec::new(),
                    },
                    ended_runs: Vec::new(),
                    withdrawn_attention: Vec::new(),
                });
            }
            require_open(&card)?;
            require_not_host_health(&card)?;
            let mut ended_runs = Vec::new();
            for run in active_runs(tx.conn(), &card.id)? {
                end_run(
                    tx,
                    &run,
                    EndRun {
                        outcome: RunOutcome::Cancelled,
                        note: Some(reason.to_owned()),
                    },
                )?;
                ended_runs.push(run.id);
            }
            let mut withdrawn_attention = Vec::new();
            while let Some(item) = open_attention_id(tx.conn(), &card.id)? {
                let item: Attention = get(tx.conn(), "attention", &item)?;
                withdraw_attention(tx, &item, &format!("card cancelled: {reason}"))?;
                withdrawn_attention.push(item.id);
            }
            let card: Card = get(tx.conn(), "card", &card.id)?;
            let closed = close_card(tx, &card, CardOutcome::Cancelled, json!({"reason": reason}))?;
            Ok(Cancelled {
                closed,
                ended_runs,
                withdrawn_attention,
            })
        })
    }

    /// `card.reopen`. Natural: an open card is returned.
    pub fn card_reopen(&mut self, ctx: &Ctx, card: &str, reason: &str) -> Result<Applied<Card>> {
        required("reason", reason)?;
        self.write(ctx, "card.reopen", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            let status = CardStatus::from_columns(card.state, card.outcome).map_err(refuse("card", &card.display_id))?;
            if status == CardStatus::Open {
                return Ok(card);
            }
            status.reopen().map_err(refuse("card", &card.display_id))?;
            tx.conn().execute(
                "UPDATE cards SET state = 'open', outcome = NULL, closed_at = NULL, closed_by = NULL, rev = rev + 1
                 WHERE id = ?1",
                [&card.id],
            )?;
            tx.event(
                NewEvent::new("card", &card.id, "card.reopened")
                    .rev(card.rev + 1)
                    .card(&card.id)
                    .payload(json!({"reason": reason, "was": card.outcome})),
            )?;
            get(tx.conn(), "card", &card.id)
        })
    }

    /// `tiller card comment`: an `outbox.enqueue` (`note`) to the project's
    /// live orchestrator, recorded as an event on the card. Keyed.
    pub fn card_comment(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        card: &str,
        text: &str,
    ) -> Result<Applied<OutboxMessage>> {
        required("comment", text)?;
        self.write(ctx, "outbox.enqueue", key, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            let Some(orchestrator) = live_orchestrator(tx.conn(), &card.project_id)? else {
                return Err(Error::refused(
                    "project",
                    &card.project_id,
                    "no live orchestrator to read the comment",
                ));
            };
            enqueue(
                tx,
                &NewMessage {
                    target_session_id: orchestrator.id,
                    kind: OutboxKind::Note,
                    body: format!("[{}] {text}", card.display_id),
                    card_id: Some(card.id.clone()),
                    reply_to: None,
                },
            )
        })
    }
}
