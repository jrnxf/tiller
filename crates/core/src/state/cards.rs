//! Cards, holds and dependencies: `card.*`, `hold.release`, `dep.*`
//! (`state.md` sections 3, 7 and 8).

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::approvals::mark_stale;
use super::attention::{NewAttention, open_attention, withdraw_attention, withdraw_quota_items};
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::projects::local_host;
use super::records::cancel_obligation;
use super::runs::{EndRun, end_run};
use super::{check_rev, find, get, list, refuse, required, resolve_card};
use crate::authority::{CardAction, require_card_scope};
use crate::domain::lifecycle::{allowed_outcomes, outcome_satisfies_dependants};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, HOST_HEALTH_CARD};
use crate::limits::{self, COMMENT_MAX_CHARS};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
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
    limits::card_text(Some(&new.title), new.body.as_deref())?;
    insert_card(tx, new, ask_id)
}

/// `create_card` for a caller that checks the caps on its own terms:
/// `bug.file` caps what the filer wrote, not the evidence block it adds.
fn insert_card(tx: &mut Tx<'_>, new: &NewCard, ask_id: Option<&str>) -> Result<Card> {
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

/// The category of a card `tiller bug file` files (`protocol.md`, 6.4).
pub const TILLER_BUG: &str = "tiller-bug";

/// A Tiller bug to file: `tiller bug file`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewBug {
    pub title: String,
    pub body: String,
    /// What the filer saw, as JSON; quoted at the end of the card's body.
    pub evidence: Option<Value>,
}

/// Why a Tiller bug went where it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugRoute {
    /// The host's bug project took it.
    BugProject,
    /// The host has no bug project, so it waits in `fleet`.
    NoBugProject,
    /// The host's bug project is retired, so it waits in `fleet`.
    BugProjectRetired,
}

/// A filed Tiller bug: its card and the route it took.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FiledBug {
    pub card: Card,
    pub route: BugRoute,
    /// The host's bug project when it was filed, open or not.
    pub bug_project_id: Option<String>,
}

/// The card body of a Tiller bug: the filer's text, then the evidence.
fn bug_body(bug: &NewBug) -> Result<String> {
    let Some(evidence) = &bug.evidence else {
        return Ok(bug.body.clone());
    };
    let quoted = serde_json::to_string_pretty(evidence)?;
    Ok(format!(
        "{}\n\nEvidence:\n\n```json\n{quoted}\n```",
        bug.body.trim_end()
    ))
}

/// The note that tells a project's orchestrator about a card somebody else
/// filed. Also the text `session.start` sends for such a card.
pub(crate) fn new_card_note(card: &Card) -> String {
    format!("new card {}: {}", card.display_id, card.title)
}

/// A card created by a principal other than its project's own orchestrator
/// notes the project's live orchestrator, in the transaction of the create
/// (`protocol.md`, section 2.2). With no live orchestrator nothing is
/// enqueued; `session.start` tells the next one.
fn announce_card(tx: &mut Tx<'_>, card: &Card) -> Result<()> {
    if let Some(session) = tx.actor().session_id() {
        let own: bool = tx.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = ?1 AND role = 'orchestrator' AND project_id = ?2)",
            params![session, card.project_id],
            |row| row.get(0),
        )?;
        if own {
            return Ok(());
        }
    }
    let Some(orchestrator) = live_orchestrator(tx.conn(), &card.project_id)? else {
        return Ok(());
    };
    enqueue(
        tx,
        &NewMessage {
            target_session_id: orchestrator.id,
            kind: OutboxKind::Note,
            body: new_card_note(card),
            card_id: Some(card.id.clone()),
            reply_to: None,
        },
    )?;
    Ok(())
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

    // A promise about the closed card, such as a deploy watch, is moot; left
    // open it would act on the card again after a reopen.
    let promises: Vec<Obligation> = list(
        tx.conn(),
        "card_id = ?1 AND state = 'open' ORDER BY id",
        &[&card.id],
    )?;
    for obligation in promises {
        cancel_obligation(tx, &obligation, &format!("card closed {outcome}"))?;
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

/// Writes a hold on `card` and its `hold.set` event.
pub(crate) fn insert_hold(
    tx: &mut Tx<'_>,
    card: &Card,
    until: Timestamp,
    reason: &str,
) -> Result<Hold> {
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

/// Marks a dependency its card's close already released as `removed`,
/// keeping when it was released. The event names the reason it had.
fn remove_released_dep(tx: &mut Tx<'_>, dep: &CardDep, was: DepReleaseReason) -> Result<()> {
    tx.conn().execute(
        "UPDATE card_deps SET release_reason = ?3 WHERE card_id = ?1 AND depends_on_card_id = ?2",
        params![
            dep.card_id,
            dep.depends_on_card_id,
            DepReleaseReason::Removed
        ],
    )?;
    tx.event(
        NewEvent::new(
            "dep",
            format!("{}:{}", dep.card_id, dep.depends_on_card_id),
            "dep.released",
        )
        .card(&dep.card_id)
        .payload(json!({
            "depends_on": dep.depends_on_card_id,
            "reason": DepReleaseReason::Removed,
            "was": was,
        })),
    )?;
    Ok(())
}

/// The option on a `recovery` item that confirms a change landed outside an
/// approval: an outside merge, or a deploy fixed by hand (`state.md` 7).
pub const MARK_LANDED: &str = "Mark landed";

/// The option on R07's outside-merge `recovery` item that closes the card
/// `superseded`: the only close other than `landed` the daemon makes, and
/// only on the owner's answer (`state.md` 7, `recovery.md` R07).
pub const SUPERSEDED: &str = "Superseded";

/// The option on a deploy `recovery` item that gives up the deploy watch and
/// leaves the card open for a new run (`state.md` 7).
pub const REOPEN_WORK: &str = "Reopen work";

/// The ids of every card that waits on `card_id`, directly or through other
/// cards, over unreleased dependencies. Making `card_id` wait on any of them
/// would close a loop, so `dep.add` refuses it and the card page disables it.
pub fn waiting_on(conn: &rusqlite::Connection, card_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE downstream(id) AS (
             SELECT card_id FROM card_deps WHERE depends_on_card_id = ?1 AND released_at IS NULL
             UNION
             SELECT d.card_id FROM card_deps d JOIN downstream w ON d.depends_on_card_id = w.id
             WHERE d.released_at IS NULL
         )
         SELECT id FROM downstream ORDER BY id",
    )?;
    let ids = stmt
        .query_map([card_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

/// The `seq` of the card's last `card.reopened` event, or 0 for a card never
/// reopened. A reopen starts the card over (`state.md` 6), so evidence for an
/// outcome counts only when its event is newer than this.
pub fn reopened_seq(conn: &rusqlite::Connection, card: &Card) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT coalesce(max(seq), 0) FROM events
         WHERE entity = 'card' AND entity_id = ?1 AND kind = 'card.reopened'",
        [&card.id],
        |row| row.get(0),
    )?)
}

/// The tail of an evidence refusal: names the reopen when the card had one,
/// so the caller knows why older evidence on the card does not count.
fn since_reopen(conn: &rusqlite::Connection, card: &Card) -> Result<&'static str> {
    Ok(if reopened_seq(conn, card)? == 0 {
        ""
    } else {
        " since the card was last reopened"
    })
}

/// Whether the card has the evidence a `landed` outcome needs, recorded
/// since the card was created or last reopened.
pub fn landed_evidence(conn: &rusqlite::Connection, card: &Card) -> Result<bool> {
    // An approval reached `succeeded`, or the owner confirmed an outside merge
    // by answering a `recovery` item on the card with "Mark landed", alone or
    // qualified ("Mark landed, not me"; `recovery.md` R07). Any other answer
    // is not evidence. A row with no event (imported history) counts only on
    // a card never reopened.
    let since = reopened_seq(conn, card)?;
    let approved: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM approvals a
                        WHERE a.card_id = ?1 AND a.kind IN ('merge', 'land_direct') AND a.state = 'succeeded'
                          AND (?2 = 0 OR EXISTS (SELECT 1 FROM events e
                                                 WHERE e.entity = 'approval' AND e.entity_id = a.id
                                                   AND e.kind = 'approval.finished' AND e.seq > ?2)))",
        params![card.id, since],
        |row| row.get(0),
    )?;
    if approved {
        return Ok(true);
    }
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM attention a
                        WHERE a.card_id = ?1 AND a.kind = 'recovery'
                          AND lower(trim(a.answer)) LIKE lower(?2) || '%'
                          AND (?3 = 0 OR EXISTS (SELECT 1 FROM events e
                                                 WHERE e.entity = 'attention' AND e.entity_id = a.id
                                                   AND e.kind = 'attention.answered' AND e.seq > ?3)))",
        params![card.id, MARK_LANDED, since],
        |row| row.get(0),
    )?)
}

/// Whether the owner answered "Superseded" to an R07 outside-merge
/// `recovery` item on the card since it was created or last reopened: the
/// evidence the daemon needs to close the card `superseded`.
fn superseded_answer(conn: &rusqlite::Connection, card: &Card) -> Result<bool> {
    let since = reopened_seq(conn, card)?;
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM attention a
                        WHERE a.card_id = ?1 AND a.kind = 'recovery'
                          AND lower(trim(a.answer)) = lower(?2)
                          AND json_valid(a.evidence)
                          AND (json_extract(a.evidence, '$.r07_pr') IS NOT NULL
                               OR json_extract(a.evidence, '$.r07_direct_run') IS NOT NULL)
                          AND (?3 = 0 OR EXISTS (SELECT 1 FROM events e
                                                 WHERE e.entity = 'attention' AND e.entity_id = a.id
                                                   AND e.kind = 'attention.answered' AND e.seq > ?3)))",
        params![card.id, SUPERSEDED, since],
        |row| row.get(0),
    )?)
}

/// Whether the card has the report artifact a `reported` outcome needs,
/// recorded since the card was created or last reopened.
fn report_evidence(conn: &rusqlite::Connection, card: &Card) -> Result<bool> {
    let since = reopened_seq(conn, card)?;
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM artifacts a
                        WHERE a.card_id = ?1 AND a.kind = 'report' AND a.retired_at IS NULL
                          AND (?2 = 0 OR EXISTS (SELECT 1 FROM events e
                                                 WHERE e.entity = 'artifact' AND e.entity_id = a.id
                                                   AND e.kind = 'artifact.added' AND e.seq > ?2)))",
        params![card.id, since],
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
            // Citing an ask links it, which only its own project may do:
            // the cross-project create allows no citation.
            let action = if ask.is_some() {
                CardAction::ResolveAsk
            } else {
                CardAction::Create
            };
            require_card_scope(tx.conn(), tx.actor(), &new.project_id, action)?;
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
            let card = create_card(tx, &new, ask_id.as_deref())?;
            announce_card(tx, &card)?;
            Ok(card)
        })
    }

    /// `bug.file`: a Tiller bug, filed in this host's bug project while it
    /// names an open project, else in `fleet`, where it waits for the owner
    /// (`state.md`, Host). Never on `fleet-1`: a bug is always a new card. The
    /// `card.routed` event records the route and who filed it, with the
    /// filer's project.
    pub fn bug_file(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        bug: NewBug,
    ) -> Result<Applied<FiledBug>> {
        required("title", &bug.title)?;
        required("body", &bug.body)?;
        // The cap is on what the filer wrote; the evidence block is ours.
        limits::card_text(Some(&bug.title), Some(&bug.body))?;
        let body = bug_body(&bug)?;
        self.write(ctx, "bug.file", key, |tx| {
            let host = local_host(tx.conn())?;
            let configured = match &host.bug_project_id {
                Some(id) => Some(get::<Project>(tx.conn(), "project", id)?),
                None => None,
            };
            let (project_id, route) = match &configured {
                Some(project) if project.retired_at.is_some() => {
                    (ids::FLEET_PROJECT.to_owned(), BugRoute::BugProjectRetired)
                }
                Some(project) => (project.id.clone(), BugRoute::BugProject),
                None => (ids::FLEET_PROJECT.to_owned(), BugRoute::NoBugProject),
            };
            require_card_scope(tx.conn(), tx.actor(), &project_id, CardAction::Create)?;
            let filer = match tx.actor().session_id() {
                Some(id) => find::<Session>(tx.conn(), "id = ?1", &[&id])?,
                None => None,
            };
            let card = insert_card(
                tx,
                &NewCard {
                    project_id: project_id.clone(),
                    kind: CardKind::Ship,
                    title: bug.title.clone(),
                    body: Some(body.clone()),
                    category: Some(TILLER_BUG.into()),
                    ..NewCard::default()
                },
                None,
            )?;
            tx.event(
                NewEvent::new("card", &card.id, "card.routed")
                    .card(&card.id)
                    .payload(json!({
                        "project_id": project_id,
                        "route": route,
                        "bug_project_id": host.bug_project_id,
                        "filed_by_session_id": filer.as_ref().map(|session| &session.id),
                        "filed_from_project_id": filer.as_ref().map(|session| &session.project_id),
                        "evidence": bug.evidence,
                    })),
            )?;
            announce_card(tx, &card)?;
            Ok(FiledBug {
                card,
                route,
                bug_project_id: host.bug_project_id,
            })
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
        mut edit: CardEdit,
    ) -> Result<Applied<Card>> {
        // An empty body clears it, from every writer alike: the CLI's
        // `--body ""` and the API's `"body": ""` (`protocol.md` 7.4).
        if edit
            .body
            .as_ref()
            .is_some_and(|body| body.as_deref().is_some_and(str::is_empty))
        {
            edit.body = Some(None);
        }
        limits::card_text(
            edit.title.as_deref(),
            edit.body.as_ref().and_then(|body| body.as_deref()),
        )?;
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
            require_card_scope(tx.conn(), tx.actor(), &card.project_id, CardAction::Edit)?;
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
            require_card_scope(tx.conn(), tx.actor(), &card.project_id, CardAction::Hold)?;
            if let Some(hold) = find::<Hold>(
                tx.conn(),
                "card_id = ?1 AND released_at IS NULL",
                &[&card.id],
            )? {
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
                return Err(Error::refused(
                    "card",
                    &card.display_id,
                    format!("{until} is not in the future"),
                ));
            }
            insert_hold(tx, &card, until, reason)
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
            let action = if reason == HoldReleaseReason::Due {
                CardAction::ReleaseDueHold
            } else {
                CardAction::Hold
            };
            require_card_scope(tx.conn(), tx.actor(), &card.project_id, action)?;
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
            if reason == HoldReleaseReason::Due {
                // The reset passed, so the quota question is moot; the
                // re-ask below is the card's one open item.
                withdraw_quota_items(tx, &card.id, "the hold's date passed")?;
            }
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
            require_card_scope(tx.conn(), tx.actor(), &card.project_id, CardAction::Depend)?;
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
            if waiting_on(tx.conn(), &card.id)?.contains(&target.id) {
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

    /// `dep.release` by hand (`removed`). A dependency its card's close
    /// already released is marked `removed` too, keeping its release time, so
    /// the card stops listing it. Natural: none, or one already removed,
    /// returns `None`.
    pub fn dep_release(
        &mut self,
        ctx: &Ctx,
        card: &str,
        depends_on: &str,
    ) -> Result<Applied<Option<CardDep>>> {
        self.write(ctx, "dep.release", None, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            require_card_scope(tx.conn(), tx.actor(), &card.project_id, CardAction::Depend)?;
            let target = resolve_card(tx.conn(), depends_on)?;
            let Some(dep) = find::<CardDep>(
                tx.conn(),
                "card_id = ?1 AND depends_on_card_id = ?2
                 AND (release_reason IS NULL OR release_reason != 'removed')",
                &[&card.id, &target.id],
            )?
            else {
                return Ok(None);
            };
            match dep.release_reason {
                None => release_dep(tx, &dep, DepReleaseReason::Removed)?,
                Some(was) => remove_released_dep(tx, &dep, was)?,
            }
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
            require_card_scope(tx.conn(), tx.actor(), &card.project_id, CardAction::Close(outcome))?;
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
                    let since = since_reopen(tx.conn(), &card)?;
                    return Err(Error::refused(
                        "card",
                        &card.display_id,
                        format!(
                            "landed needs a succeeded merge or land approval, or a recovery item answered \"Mark landed\"{since}"
                        ),
                    ));
                }
                CardOutcome::Reported if !report_evidence(tx.conn(), &card)? => {
                    let since = since_reopen(tx.conn(), &card)?;
                    return Err(Error::refused(
                        "card",
                        &card.display_id,
                        format!("no report artifact is recorded on the card{since}"),
                    ));
                }
                // The daemon's one close besides `landed`: carrying out the
                // owner's "Superseded" to R07 (`recovery.md` R07).
                CardOutcome::Superseded
                    if matches!(tx.actor(), Actor::Daemon) && !superseded_answer(tx.conn(), &card)? =>
                {
                    let since = since_reopen(tx.conn(), &card)?;
                    return Err(Error::Forbidden(format!(
                        "the daemon closes a card superseded only on the owner's answer \"{SUPERSEDED}\" to its outside-merge recovery item{since}"
                    )));
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
            require_card_scope(
                tx.conn(),
                tx.actor(),
                &card.project_id,
                CardAction::Close(CardOutcome::Cancelled),
            )?;
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
            require_card_scope(tx.conn(), tx.actor(), &card.project_id, CardAction::Edit)?;
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
        limits::within("a comment", text, COMMENT_MAX_CHARS)?;
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
