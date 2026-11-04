//! The learning loop (`docs/design/learning-loop.md`, T7.3): one retro per
//! closed card, decided from the record with no model call; change sets,
//! which are one change or a named bundle measured as one; the holdout eval
//! runs that gate an approval; and the manifest behind every run's
//! `config_hash`.
//!
//! Nothing in these tables is rendered into a fleet role. The curator, which
//! finishes a `full` retro, is not a fleet role (`Actor::Curator`).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::projects::meta;
use super::{find, get, list, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, HOST_HEALTH_CARD};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// The `[learning]` config keys the loop reads, with their defaults
/// (learning-loop.md, section 10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Learning {
    /// One routine card in this many gets a full retro.
    pub sample_every: u64,
    /// Cards closed `landed` or `reported` that a change set is measured over.
    pub window_cards: i64,
    pub curator_model: String,
    pub curator_timeout_minutes: u64,
    /// The checkout that holds `evals/run.sh`, for `tiller learn eval`.
    pub evals_dir: Option<String>,
}

impl Default for Learning {
    fn default() -> Self {
        Self {
            sample_every: 5,
            window_cards: 10,
            curator_model: "sonnet".into(),
            curator_timeout_minutes: 20,
            evals_dir: None,
        }
    }
}

impl Learning {
    /// The `[learning]` table of a parsed config file; absent means defaults.
    pub fn from_table(table: &toml::Table) -> Result<Self> {
        match table.get("learning") {
            None => Ok(Self::default()),
            Some(value) => value
                .clone()
                .try_into()
                .map_err(|e: toml::de::Error| Error::Usage(format!("config.toml [learning]: {e}"))),
        }
    }
}

/// What one sweep did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Swept {
    pub scheduled: Vec<Retro>,
    /// Change sets moved to `measuring`.
    pub measuring: Vec<String>,
    /// Change sets closed `window_complete`.
    pub closed: Vec<String>,
}

/// How a retro ends (`tiller retro finish`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finish {
    pub outcome: RetroOutcome,
    /// The one learning kept, for `evidence`; a note for `unfinished`.
    pub learning: Option<String>,
    /// JSON pointers into the record that back the learning.
    pub evidence: Option<Value>,
    /// The proposal filed, for `proposed`.
    pub proposal: Option<String>,
}

/// The result of one holdout run, as `tiller learn eval` records it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    /// The members hash the candidate was rendered from. When set, the
    /// record is refused if the set's members changed since.
    pub members_sha256: Option<String>,
    pub candidate_sha256: Option<String>,
    pub cases_total: i64,
    pub cases_passed: i64,
    pub overall_score: Option<f64>,
    pub cost_usd: Option<f64>,
    pub partial: bool,
    pub claude_version: Option<String>,
    pub judge_model: Option<String>,
    pub result_path: Option<String>,
}

/// The gate's verdict on one proposal (learning-loop.md, section 7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gate {
    pub proposal: String,
    pub ok: bool,
    /// The failing step, when not ok.
    pub reason: Option<String>,
    /// The command that clears it.
    pub hint: Option<String>,
    pub change_set: Option<String>,
    pub eval_run: Option<String>,
    /// The set that is measuring and blocks every other, with cards to go.
    pub measuring: Option<Measuring>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measuring {
    pub change_set: String,
    pub name: String,
    pub cards_closed: i64,
    pub window_cards: i64,
}

/// Guardrails over a window of accepted cards (learning-loop.md, section 8).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Guardrails {
    pub cards: i64,
    pub attention_per_card: Option<f64>,
    pub rework_attempts: i64,
    pub escaped: i64,
    pub findings_per_review: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowReport {
    pub change_set: ChangeSet,
    pub window: Guardrails,
    pub before: Guardrails,
    pub config_hashes: Vec<String>,
    pub revert_suggested: bool,
    pub worse: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// Retros of the last 30 days: depth, reason and outcome counts.
    pub retros: BTreeMap<String, i64>,
    pub pending: Vec<Retro>,
    pub open_sets: Vec<ChangeSet>,
    pub measuring: Option<WindowReport>,
    /// Sessions started in the last 30 days with no config hash.
    pub sessions_without_hash: i64,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether the card id falls in the sample: the first 8 bytes of its SHA-256
/// as a big-endian integer, modulo `sample_every`, is 0.
pub fn sampled(card_id: &str, sample_every: u64) -> bool {
    if sample_every <= 1 {
        return true;
    }
    let digest = Sha256::digest(card_id.as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head) % sample_every == 0
}

fn count(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<i64> {
    Ok(conn.query_row(sql, params, |row| row.get(0))?)
}

fn elapsed_minutes(from: Timestamp, to: Timestamp) -> i64 {
    (to.as_millis() - from.as_millis()) / 60_000
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        (values[mid - 1] + values[mid]) / 2.0
    })
}

/// The counts scripts compute for a card (learning-loop.md, section 3).
pub(crate) fn counts(conn: &Connection, card: &Card) -> Result<Value> {
    let closed_at = card.closed_at.unwrap_or(card.created_at);
    let mut attempts = serde_json::Map::new();
    let mut stmt =
        conn.prepare("SELECT role, MAX(attempt) FROM runs WHERE card_id = ?1 GROUP BY role")?;
    for row in stmt.query_map([&card.id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (role, max) = row?;
        attempts.insert(role, json!(max));
    }
    let mut attention = serde_json::Map::new();
    let mut stmt = conn.prepare(
        "SELECT kind, COUNT(*) FROM attention WHERE card_id = ?1 AND withdrawn_at IS NULL GROUP BY kind",
    )?;
    for row in stmt.query_map([&card.id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (kind, n) = row?;
        attention.insert(kind, json!(n));
    }
    let (high, medium, low): (i64, i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(findings_high), 0), COALESCE(SUM(findings_medium), 0), COALESCE(SUM(findings_low), 0)
         FROM reviews WHERE card_id = ?1",
        [&card.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let mut run_outcomes = serde_json::Map::new();
    let mut models = BTreeSet::new();
    let mut runs = 0;
    let mut stmt = conn.prepare("SELECT model, outcome FROM runs WHERE card_id = ?1")?;
    for row in stmt.query_map([&card.id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })? {
        let (model, outcome) = row?;
        runs += 1;
        models.insert(model);
        let key = outcome.unwrap_or_else(|| "active".into());
        let n = run_outcomes.get(&key).and_then(Value::as_i64).unwrap_or(0);
        run_outcomes.insert(key, json!(n + 1));
    }
    Ok(json!({
        "elapsed_minutes": elapsed_minutes(card.created_at, closed_at),
        "attempts": attempts,
        "attention": attention,
        "findings": {"high": high, "medium": medium, "low": low},
        "guard_breaches": count(
            conn,
            "SELECT COUNT(*) FROM events WHERE kind = 'guard.breached' AND card_id = ?1",
            params![card.id],
        )?,
        "fix_cards": count(
            conn,
            "SELECT COUNT(*) FROM cards WHERE fixes_card_id = ?1",
            params![card.id],
        )?,
        "runs": runs,
        "run_outcomes": run_outcomes,
        "models": models,
        "status_lines": count(
            conn,
            "SELECT COUNT(*) FROM observations WHERE source = 'status_line'
             AND run_id IN (SELECT id FROM runs WHERE card_id = ?1)",
            params![card.id],
        )?,
    }))
}

fn config_hashes(conn: &Connection, card_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT h FROM (
            SELECT config_hash AS h FROM runs WHERE card_id = ?1
            UNION SELECT s.config_hash FROM sessions s JOIN runs r ON r.id = s.run_id WHERE r.card_id = ?1
         ) WHERE h IS NOT NULL ORDER BY h",
    )?;
    let rows = stmt.query_map([card_id], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Whether the record shows an incident on the card (learning-loop.md,
/// section 2).
fn is_incident(conn: &Connection, card: &Card) -> Result<bool> {
    if matches!(
        card.outcome,
        Some(CardOutcome::Failed | CardOutcome::Abandoned | CardOutcome::Superseded)
    ) || card.category.as_deref() == Some("revert")
        || card.fixes_card_id.is_some()
    {
        return Ok(true);
    }
    let breached = count(
        conn,
        "SELECT COUNT(*) FROM events WHERE kind = 'guard.breached' AND card_id = ?1",
        params![card.id],
    )?;
    if breached > 0 {
        return Ok(true);
    }
    let bad_runs = count(
        conn,
        "SELECT COUNT(*) FROM runs WHERE card_id = ?1 AND outcome IN ('failed', 'abandoned', 'cut')",
        params![card.id],
    )?;
    Ok(bad_runs > 0)
}

/// A landed or reported card on the first attempt of every role, with no
/// attention and no high or medium finding, at or under half the median
/// elapsed time of the last 20 such cards of its size (at least 5 of them).
fn is_surprising_win(conn: &Connection, card: &Card) -> Result<bool> {
    let Some(closed_at) = card.closed_at else {
        return Ok(false);
    };
    if !matches!(
        card.outcome,
        Some(CardOutcome::Landed | CardOutcome::Reported)
    ) {
        return Ok(false);
    }
    let retries = count(
        conn,
        "SELECT COUNT(*) FROM runs WHERE card_id = ?1 AND attempt > 1",
        params![card.id],
    )?;
    let attention = count(
        conn,
        "SELECT COUNT(*) FROM attention WHERE card_id = ?1 AND withdrawn_at IS NULL",
        params![card.id],
    )?;
    let findings = count(
        conn,
        "SELECT COALESCE(SUM(findings_high + findings_medium), 0) FROM reviews WHERE card_id = ?1",
        params![card.id],
    )?;
    if retries > 0 || attention > 0 || findings > 0 {
        return Ok(false);
    }
    let mut stmt = conn.prepare(
        "SELECT (julianday(closed_at) - julianday(created_at)) * 1440.0 FROM cards
         WHERE state = 'closed' AND outcome IN ('landed', 'reported') AND id <> ?1
           AND COALESCE(size, '') = COALESCE(?2, '') AND closed_at <= ?3
         ORDER BY closed_at DESC LIMIT 20",
    )?;
    let minutes: Vec<f64> = stmt
        .query_map(
            params![card.id, card.size.map(|s| s.as_str()), closed_at],
            |row| row.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    if minutes.len() < 5 {
        return Ok(false);
    }
    let Some(median) = median(minutes) else {
        return Ok(false);
    };
    let elapsed = elapsed_minutes(card.created_at, closed_at) as f64;
    Ok(elapsed <= median / 2.0)
}

/// The depth and reason a card's retro gets, from the record alone.
pub(crate) fn decide(
    conn: &Connection,
    card: &Card,
    sample_every: u64,
) -> Result<(RetroDepth, RetroReason)> {
    if is_incident(conn, card)? {
        return Ok((RetroDepth::Full, RetroReason::Incident));
    }
    if card.kind == CardKind::Question {
        return Ok((RetroDepth::Light, RetroReason::Routine));
    }
    if is_surprising_win(conn, card)? {
        return Ok((RetroDepth::Full, RetroReason::SurprisingWin));
    }
    if sampled(&card.id, sample_every) {
        return Ok((RetroDepth::Full, RetroReason::Sample));
    }
    Ok((RetroDepth::Light, RetroReason::Routine))
}

fn insert_retro(
    tx: &mut Tx<'_>,
    card: &Card,
    trigger: Option<&str>,
    depth: RetroDepth,
    reason: RetroReason,
) -> Result<Retro> {
    let closed_at = card
        .closed_at
        .ok_or_else(|| Error::refused("card", &card.display_id, "card is open"))?;
    let id = ids::new_ulid();
    let now = tx.now();
    let by = tx.actor().to_string();
    let counts = counts(tx.conn(), card)?;
    let hashes = json!(config_hashes(tx.conn(), &card.id)?);
    let (state, outcome, finished_at, finished_by) = match depth {
        RetroDepth::Light => (
            RetroState::Done,
            Some(RetroOutcome::Nothing),
            Some(now),
            Some(by.clone()),
        ),
        RetroDepth::Full => (RetroState::Pending, None, None, None),
    };
    tx.conn().execute(
        "INSERT INTO retros (id, card_id, project_id, closed_at, trigger, depth, reason, counts, config_hashes,
                             state, outcome, finished_at, finished_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            id,
            card.id,
            card.project_id,
            closed_at,
            trigger,
            depth,
            reason,
            counts,
            hashes,
            state,
            outcome,
            finished_at,
            finished_by,
            now,
        ],
    )?;
    tx.event(
        NewEvent::new("retro", &id, "retro.scheduled")
            .rev(1)
            .card(&card.id)
            .payload(json!({"depth": depth, "reason": reason, "trigger": trigger})),
    )?;
    get(tx.conn(), "retro", &id)
}

fn members_sha256(conn: &Connection, set: &str) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT p.diff_sha256 FROM change_set_members m JOIN proposals p ON p.id = m.proposal_id
         WHERE m.change_set_id = ?1 ORDER BY p.diff_sha256",
    )?;
    let hashes: Vec<String> = stmt
        .query_map([set], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(sha256_hex(hashes.join("\n").as_bytes()))
}

/// Cards closed `landed` or `reported` since `from`.
fn accepted_since(conn: &Connection, from: Timestamp) -> Result<i64> {
    count(
        conn,
        "SELECT COUNT(*) FROM cards WHERE state = 'closed' AND outcome IN ('landed', 'reported') AND closed_at >= ?1",
        params![from],
    )
}

fn resolve_set(conn: &Connection, input: &str) -> Result<ChangeSet> {
    if let Some(found) = find::<ChangeSet>(
        conn,
        "name = ?1 AND state <> 'closed' ORDER BY opened_at DESC",
        &[&input],
    )? {
        return Ok(found);
    }
    let id = resolve_id(conn, "change_sets", "change set", input)?;
    get(conn, "change set", &id)
}

/// A proposal's id, state, card and diff hash, without a record struct of
/// its own: the proposal tables are memory's (`memory.md` section 11).
struct ProposalRow {
    id: String,
    state: String,
    diff_sha256: String,
}

fn proposal_row(conn: &Connection, input: &str) -> Result<ProposalRow> {
    let id = resolve_id(conn, "proposals", "proposal", input)?;
    Ok(conn.query_row(
        "SELECT id, state, diff_sha256 FROM proposals WHERE id = ?1",
        [&id],
        |row| {
            Ok(ProposalRow {
                id: row.get(0)?,
                state: row.get(1)?,
                diff_sha256: row.get(2)?,
            })
        },
    )?)
}

/// The proposal's membership in a set that is not closed, if any. A closed
/// set keeps its members as history and binds nothing.
fn open_membership(conn: &Connection, proposal_id: &str) -> Result<Option<ChangeSetMember>> {
    find(
        conn,
        "proposal_id = ?1 AND change_set_id IN (SELECT id FROM change_sets WHERE state <> 'closed')",
        &[&proposal_id],
    )
}

fn add_member(tx: &mut Tx<'_>, set: &ChangeSet, proposal: &str) -> Result<ChangeSetMember> {
    let row = proposal_row(tx.conn(), proposal)?;
    if row.state != "proposed" {
        return Err(Error::refused(
            "proposal",
            &row.id,
            format!("proposal is {}, not proposed", row.state),
        ));
    }
    if let Some(member) = open_membership(tx.conn(), &row.id)? {
        if member.change_set_id == set.id {
            return Ok(member);
        }
        return Err(Error::refused(
            "proposal",
            &row.id,
            format!("proposal is already in change set {}", member.change_set_id),
        ));
    }
    let now = tx.now();
    tx.conn().execute(
        "INSERT INTO change_set_members (change_set_id, proposal_id, added_at) VALUES (?1, ?2, ?3)",
        params![set.id, row.id, now],
    )?;
    tx.event(
        NewEvent::new("change_set", &set.id, "change_set.member_added")
            .payload(json!({"proposal": row.id, "diff_sha256": row.diff_sha256})),
    )?;
    Ok(ChangeSetMember {
        change_set_id: set.id.clone(),
        proposal_id: row.id,
        added_at: now,
    })
}

fn require_owner_or_daemon(actor: &Actor, what: &str) -> Result<()> {
    if actor.is_owner() || matches!(actor, Actor::Daemon) {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "{what} is for the owner or the daemon; {actor} may not"
        )))
    }
}

fn guardrails(conn: &Connection, cards: &[String]) -> Result<Guardrails> {
    if cards.is_empty() {
        return Ok(Guardrails::default());
    }
    let list = json!(cards).to_string();
    let in_cards = "IN (SELECT value FROM json_each(?1))";
    let attention = count(
        conn,
        &format!(
            "SELECT COUNT(*) FROM attention WHERE withdrawn_at IS NULL AND card_id {in_cards}"
        ),
        params![list],
    )?;
    let rework = count(
        conn,
        &format!(
            "SELECT COUNT(*) FROM runs WHERE role = 'builder' AND attempt > 1 AND card_id {in_cards}"
        ),
        params![list],
    )?;
    let escaped = count(
        conn,
        &format!(
            "SELECT COUNT(*) FROM cards WHERE COALESCE(category, '') <> 'revert' AND fixes_card_id {in_cards}"
        ),
        params![list],
    )?;
    let (reviews, findings): (i64, i64) = conn.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(findings_high + findings_medium), 0) FROM reviews WHERE card_id {in_cards}"
        ),
        params![list],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let n = cards.len() as i64;
    Ok(Guardrails {
        cards: n,
        attention_per_card: Some(attention as f64 / n as f64),
        rework_attempts: rework,
        escaped,
        findings_per_review: (reviews > 0).then(|| findings as f64 / reviews as f64),
    })
}

fn accepted_cards(
    conn: &Connection,
    from: Option<Timestamp>,
    before: Option<Timestamp>,
    limit: i64,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM cards WHERE state = 'closed' AND outcome IN ('landed', 'reported')
           AND (?1 IS NULL OR closed_at >= ?1) AND (?2 IS NULL OR closed_at < ?2)
         ORDER BY closed_at DESC, id DESC LIMIT ?3",
    )?;
    let rows = stmt.query_map(params![from, before, limit], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn worse(window: &Guardrails, before: &Guardrails) -> Vec<String> {
    let mut worse = Vec::new();
    if before.cards == 0 || window.cards == 0 {
        return worse;
    }
    let ratio = |rate: Option<f64>| rate.unwrap_or(0.0);
    if ratio(window.attention_per_card) > ratio(before.attention_per_card) {
        worse.push("attention_per_card".into());
    }
    if (window.rework_attempts as f64 / window.cards as f64)
        > (before.rework_attempts as f64 / before.cards as f64)
    {
        worse.push("rework_attempts".into());
    }
    if (window.escaped as f64 / window.cards as f64) > (before.escaped as f64 / before.cards as f64)
    {
        worse.push("escaped".into());
    }
    if ratio(window.findings_per_review) > ratio(before.findings_per_review) {
        worse.push("findings_per_review".into());
    }
    worse
}

/// The parts in which two manifests differ: part name to (a, b).
pub fn manifest_diff(a: &Value, b: &Value) -> BTreeMap<String, (Value, Value)> {
    let keys: BTreeSet<&String> = a
        .as_object()
        .into_iter()
        .chain(b.as_object())
        .flat_map(|map| map.keys())
        .collect();
    keys.into_iter()
        .filter_map(|key| {
            let (x, y) = (a.get(key), b.get(key));
            (x != y).then(|| {
                (
                    key.clone(),
                    (
                        x.cloned().unwrap_or(Value::Null),
                        y.cloned().unwrap_or(Value::Null),
                    ),
                )
            })
        })
        .collect()
}

impl Store {
    /// `retro.schedule` for every closed card without one, plus the change
    /// set transitions the record now supports (learning-loop.md, sections 2
    /// and 6). Owner, daemon.
    pub fn learning_sweep(&mut self, ctx: &Ctx, config: &Learning) -> Result<Applied<Swept>> {
        self.write(ctx, "learning.sweep", None, |tx| {
            require_owner_or_daemon(tx.actor(), "the learning sweep")?;
            let mut swept = Swept::default();

            // Closed cards with no retro for their close.
            let unretroed: Vec<Card> = list(
                tx.conn(),
                "state = 'closed' AND outcome <> 'cancelled' AND display_id <> ?1
                   AND NOT EXISTS (SELECT 1 FROM retros r WHERE r.card_id = cards.id
                                   AND r.closed_at = cards.closed_at AND r.trigger IS NULL)
                 ORDER BY closed_at, id",
                &[&HOST_HEALTH_CARD],
            )?;
            for card in unretroed {
                let (depth, reason) = decide(tx.conn(), &card, config.sample_every)?;
                swept.scheduled.push(insert_retro(tx, &card, None, depth, reason)?);
            }

            // A fix card created after its origin closed re-opens the origin's
            // retro, once per fix card.
            let fixes: Vec<Card> = list(
                tx.conn(),
                "fixes_card_id IS NOT NULL
                   AND EXISTS (SELECT 1 FROM cards o WHERE o.id = cards.fixes_card_id
                               AND o.state = 'closed' AND o.outcome <> 'cancelled' AND o.closed_at < cards.created_at)
                   AND NOT EXISTS (SELECT 1 FROM retros r WHERE r.trigger = cards.id)
                 ORDER BY created_at, id",
                &[],
            )?;
            for fix in fixes {
                let Some(origin_id) = &fix.fixes_card_id else {
                    continue;
                };
                let origin: Card = get(tx.conn(), "card", origin_id)?;
                swept.scheduled.push(insert_retro(
                    tx,
                    &origin,
                    Some(&fix.id),
                    RetroDepth::Full,
                    RetroReason::Incident,
                )?);
            }

            // An open set with an applied member starts measuring, from the
            // time the applied revision was recorded.
            let open: Vec<ChangeSet> = list(tx.conn(), "state = 'open' ORDER BY opened_at", &[])?;
            for set in open {
                let applied: Option<Timestamp> = tx
                    .conn()
                    .query_row(
                        "SELECT MIN(COALESCE(r.recorded_at, p.decided_at, p.created_at)) FROM change_set_members m
                         JOIN proposals p ON p.id = m.proposal_id
                         LEFT JOIN memory_revisions r ON r.id = p.applied_revision_id
                         WHERE m.change_set_id = ?1 AND p.state = 'applied'",
                        [&set.id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(from) = applied {
                    tx.conn().execute(
                        "UPDATE change_sets SET state = 'measuring', measuring_from = ?2, rev = rev + 1 WHERE id = ?1",
                        params![set.id, from],
                    )?;
                    tx.event(
                        NewEvent::new("change_set", &set.id, "change_set.measuring")
                            .rev(set.rev + 1)
                            .payload(json!({"measuring_from": from})),
                    )?;
                    swept.measuring.push(set.id);
                }
            }

            // A measuring set whose window is complete closes.
            let measuring: Vec<ChangeSet> =
                list(tx.conn(), "state = 'measuring' ORDER BY opened_at", &[])?;
            for set in measuring {
                let Some(from) = set.measuring_from else {
                    continue;
                };
                if accepted_since(tx.conn(), from)? >= set.window_cards {
                    close_set(tx, &set, ChangeSetClose::WindowComplete)?;
                    swept.closed.push(set.id);
                }
            }
            Ok(swept)
        })
    }

    /// `retro.start`: records the curator session on a pending retro. A
    /// retro whose curator started less than `timeout_minutes` ago is still
    /// running and is refused; an older start was a run that died.
    pub fn retro_start(
        &mut self,
        ctx: &Ctx,
        retro: &str,
        curator_session: Option<&str>,
        timeout_minutes: i64,
    ) -> Result<Applied<Retro>> {
        self.write(ctx, "retro.start", None, |tx| {
            require_owner_or_daemon(tx.actor(), "tiller retro run")?;
            let retro = resolve_retro(tx.conn(), retro)?;
            if retro.state != RetroState::Pending {
                return Err(Error::refused("retro", &retro.id, "retro is done"));
            }
            if let Some(started) = retro.curator_started_at
                && started.plus_minutes(timeout_minutes) > tx.now()
            {
                return Err(Error::refused(
                    "retro",
                    &retro.id,
                    format!(
                        "a curator started at {started} is still running; it is cut after {timeout_minutes} minutes"
                    ),
                ));
            }
            let now = tx.now();
            tx.conn().execute(
                "UPDATE retros SET curator_session = ?2, curator_started_at = ?3, rev = rev + 1 WHERE id = ?1",
                params![retro.id, curator_session, now],
            )?;
            tx.event(
                NewEvent::new("retro", &retro.id, "retro.started")
                    .rev(retro.rev + 1)
                    .card(&retro.card_id)
                    .payload(json!({"curator_session": curator_session})),
            )?;
            get(tx.conn(), "retro", &retro.id)
        })
    }

    /// `retro.finish` (`tiller retro finish`, keyed): the curator's or the
    /// owner's verdict on a full retro. A `light` retro cannot be finished; a
    /// done retro finished again with the same outcome is a replay.
    pub fn retro_finish(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        retro: &str,
        finish: Finish,
    ) -> Result<Applied<Retro>> {
        self.write(ctx, "retro.finish", key, |tx| {
            let retro = resolve_retro(tx.conn(), retro)?;
            match tx.actor() {
                Actor::Owner | Actor::Board | Actor::Daemon => {}
                // A curator finishes the one retro it was launched for.
                Actor::Curator(id) if *id == retro.id => {}
                other => {
                    return Err(Error::Forbidden(format!(
                        "tiller retro finish {} is for its curator or the owner; {other} may not",
                        retro.id
                    )));
                }
            }
            if retro.depth != RetroDepth::Full {
                return Err(Error::refused(
                    "retro",
                    &retro.id,
                    "a light retro is counts only and is already done",
                ));
            }
            if retro.state == RetroState::Done {
                if retro.outcome == Some(finish.outcome) {
                    return Ok(retro);
                }
                return Err(Error::refused(
                    "retro",
                    &retro.id,
                    format!(
                        "retro is done with outcome {}",
                        retro.outcome.map_or("", RetroOutcome::as_str)
                    ),
                ));
            }
            let mut evidence_id = None;
            let mut proposal_id = None;
            match finish.outcome {
                RetroOutcome::Nothing => {}
                RetroOutcome::Unfinished => {}
                RetroOutcome::Evidence => {
                    let learning = finish.learning.as_deref().unwrap_or("");
                    required("learning", learning)?;
                    evidence_id = Some(capture_learning(tx, &retro, learning, finish.evidence.as_ref())?);
                }
                RetroOutcome::Proposed => {
                    let input = finish
                        .proposal
                        .as_deref()
                        .ok_or_else(|| Error::Usage("--proposed names the proposal".into()))?;
                    proposal_id = Some(proposal_row(tx.conn(), input)?.id);
                }
            }
            let now = tx.now();
            let by = tx.actor().to_string();
            tx.conn().execute(
                "UPDATE retros SET state = 'done', outcome = ?2, learning = ?3, evidence_id = ?4, proposal_id = ?5,
                        finished_at = ?6, finished_by = ?7, rev = rev + 1 WHERE id = ?1",
                params![
                    retro.id,
                    finish.outcome,
                    finish.learning,
                    evidence_id,
                    proposal_id,
                    now,
                    by
                ],
            )?;
            tx.event(
                NewEvent::new("retro", &retro.id, "retro.finished")
                    .rev(retro.rev + 1)
                    .card(&retro.card_id)
                    .payload(json!({
                        "outcome": finish.outcome,
                        "evidence": evidence_id,
                        "proposal": proposal_id,
                    })),
            )?;
            get(tx.conn(), "retro", &retro.id)
        })
    }

    /// `change_set.create` (`tiller learn bundle`): one change, or a named
    /// bundle of proposals. Owner.
    pub fn change_set_create(
        &mut self,
        ctx: &Ctx,
        name: &str,
        proposals: &[String],
        window_cards: i64,
    ) -> Result<Applied<ChangeSet>> {
        self.write(ctx, "change_set.create", None, |tx| {
            require_owner_or_daemon(tx.actor(), "tiller learn bundle")?;
            required("name", name)?;
            let name = name.trim();
            if window_cards <= 0 {
                return Err(Error::Usage("window_cards must be positive".into()));
            }
            if find::<ChangeSet>(tx.conn(), "name = ?1 AND state <> 'closed'", &[&name])?.is_some()
            {
                return Err(Error::refused(
                    "change set",
                    name,
                    "an open change set already has this name",
                ));
            }
            let id = ids::new_ulid();
            let now = tx.now();
            tx.conn().execute(
                "INSERT INTO change_sets (id, name, window_cards, opened_at, opened_by) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, name, window_cards, now, tx.actor().to_string()],
            )?;
            tx.event(
                NewEvent::new("change_set", &id, "change_set.created")
                    .rev(1)
                    .payload(json!({"name": name, "window_cards": window_cards})),
            )?;
            let set: ChangeSet = get(tx.conn(), "change set", &id)?;
            for proposal in proposals {
                add_member(tx, &set, proposal)?;
            }
            Ok(set)
        })
    }

    /// `change_set.add` (`tiller learn add`). Owner.
    pub fn change_set_add(
        &mut self,
        ctx: &Ctx,
        set: &str,
        proposal: &str,
    ) -> Result<Applied<ChangeSetMember>> {
        self.write(ctx, "change_set.add", None, |tx| {
            require_owner_or_daemon(tx.actor(), "tiller learn add")?;
            let set = resolve_set(tx.conn(), set)?;
            if set.state != ChangeSetState::Open {
                return Err(Error::refused(
                    "change set",
                    &set.id,
                    format!(
                        "change set is {}; members are added while it is open",
                        set.state
                    ),
                ));
            }
            add_member(tx, &set, proposal)
        })
    }

    /// `change_set.close` with `abandoned` (`tiller learn abandon`). Owner.
    pub fn change_set_abandon(&mut self, ctx: &Ctx, set: &str) -> Result<Applied<ChangeSet>> {
        self.write(ctx, "change_set.close", None, |tx| {
            require_owner_or_daemon(tx.actor(), "tiller learn abandon")?;
            let set = resolve_set(tx.conn(), set)?;
            if set.state == ChangeSetState::Closed {
                return Ok(set);
            }
            close_set(tx, &set, ChangeSetClose::Abandoned)
        })
    }

    /// `eval_run.record` (`tiller learn eval`): one holdout run for a set,
    /// bound to the members it ran with. Owner.
    pub fn eval_run_record(
        &mut self,
        ctx: &Ctx,
        set: &str,
        result: EvalResult,
    ) -> Result<Applied<EvalRun>> {
        self.write(ctx, "eval_run.record", None, |tx| {
            require_owner_or_daemon(tx.actor(), "tiller learn eval")?;
            let set = resolve_set(tx.conn(), set)?;
            if set.state == ChangeSetState::Closed {
                return Err(Error::refused(
                    "change set",
                    &set.id,
                    "change set is closed; nothing more is measured for it",
                ));
            }
            if result.cases_total < 0 || result.cases_passed < 0 || result.cases_passed > result.cases_total {
                return Err(Error::Usage(
                    "cases_passed must be between 0 and cases_total".into(),
                ));
            }
            let passed =
                result.cases_total > 0 && result.cases_passed == result.cases_total && !result.partial;
            let members = members_sha256(tx.conn(), &set.id)?;
            if let Some(rendered) = &result.members_sha256
                && *rendered != members
            {
                return Err(Error::refused(
                    "change set",
                    &set.id,
                    "the members changed while the holdout ran; the run covers an older set, run it again",
                ));
            }
            let id = ids::new_ulid();
            let now = tx.now();
            tx.conn().execute(
                "INSERT INTO eval_runs (id, change_set_id, split, members_sha256, candidate_sha256, cases_total,
                                        cases_passed, overall_score, cost_usd, partial, passed, claude_version,
                                        judge_model, result_path, ran_at, recorded_by)
                 VALUES (?1, ?2, 'holdout', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    id,
                    set.id,
                    members,
                    result.candidate_sha256,
                    result.cases_total,
                    result.cases_passed,
                    result.overall_score,
                    result.cost_usd,
                    i64::from(result.partial),
                    i64::from(passed),
                    result.claude_version,
                    result.judge_model,
                    result.result_path,
                    now,
                    tx.actor().to_string(),
                ],
            )?;
            tx.event(
                NewEvent::new("eval_run", &id, "eval_run.recorded").payload(json!({
                    "change_set": set.id,
                    "passed": passed,
                    "cases_passed": result.cases_passed,
                    "cases_total": result.cases_total,
                    "partial": result.partial,
                })),
            )?;
            get(tx.conn(), "eval run", &id)
        })
    }

    /// `config.manifest_record` (`tiller spawn`): the parts behind a
    /// `config_hash`, written on first sight. Returns whether it was new.
    pub fn config_manifest_record(
        &mut self,
        ctx: &Ctx,
        config_hash: &str,
        parts: &Value,
    ) -> Result<Applied<bool>> {
        self.write(ctx, "config.manifest_record", None, |tx| {
            required("config_hash", config_hash)?;
            if find::<ConfigManifest>(tx.conn(), "config_hash = ?1", &[&config_hash])?.is_some() {
                return Ok(false);
            }
            tx.conn().execute(
                "INSERT INTO config_manifests (config_hash, parts, first_seen_at) VALUES (?1, ?2, ?3)",
                params![config_hash, parts, tx.now()],
            )?;
            tx.event(
                NewEvent::new("config_manifest", config_hash, "config.manifest_recorded")
                    .payload(json!({"parts": parts.as_object().map(|o| o.keys().collect::<Vec<_>>())})),
            )?;
            Ok(true)
        })
    }

    /// The gate `mem.decide` runs before it accepts a proposal
    /// (learning-loop.md, section 7). A read.
    pub fn learning_gate(&self, proposal: &str) -> Result<Gate> {
        gate(self.conn(), proposal)
    }

    /// `tiller learn status` (learning-loop.md, section 8). A read.
    pub fn learning_status(&self, now: Timestamp) -> Result<Status> {
        let conn = self.conn();
        let since = now.plus_minutes(-30 * 24 * 60);
        let mut retros = BTreeMap::new();
        let mut stmt = conn.prepare(
            "SELECT depth, reason, COALESCE(outcome, 'pending'), COUNT(*) FROM retros
             WHERE created_at >= ?1 GROUP BY depth, reason, outcome",
        )?;
        for row in stmt.query_map([since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })? {
            let (depth, reason, outcome, n) = row?;
            *retros
                .entry(format!("{depth}:{reason}:{outcome}"))
                .or_insert(0) += n;
        }
        let pending = list(conn, "state = 'pending' ORDER BY created_at", &[])?;
        let open_sets = list(conn, "state = 'open' ORDER BY opened_at", &[])?;
        let measuring =
            match find::<ChangeSet>(conn, "state = 'measuring' ORDER BY opened_at", &[])? {
                Some(set) => Some(window_report(conn, set)?),
                None => None,
            };
        let sessions_without_hash = count(
            conn,
            "SELECT COUNT(*) FROM sessions WHERE config_hash IS NULL AND started_at >= ?1",
            params![since],
        )?;
        Ok(Status {
            retros,
            pending,
            open_sets,
            measuring,
            sessions_without_hash,
        })
    }
}

fn close_set(tx: &mut Tx<'_>, set: &ChangeSet, reason: ChangeSetClose) -> Result<ChangeSet> {
    let now = tx.now();
    tx.conn().execute(
        "UPDATE change_sets SET state = 'closed', closed_at = ?2, close_reason = ?3, rev = rev + 1 WHERE id = ?1",
        params![set.id, now, reason],
    )?;
    tx.event(
        NewEvent::new("change_set", &set.id, "change_set.closed")
            .rev(set.rev + 1)
            .payload(json!({"reason": reason})),
    )?;
    get(tx.conn(), "change set", &set.id)
}

/// The learning as an evidence row: the retro is its pointer, the card its
/// independence key (memory.md section 6).
fn capture_learning(
    tx: &mut Tx<'_>,
    retro: &Retro,
    learning: &str,
    pointers: Option<&Value>,
) -> Result<String> {
    let boundary = meta(tx.conn(), "boundary")?.ok_or_else(|| {
        Error::refused("host", "local", "state.db has no boundary; run tiller init")
    })?;
    let kind = if retro.reason == RetroReason::Incident {
        "incident"
    } else {
        "card"
    };
    let id = ids::new_ulid();
    let reference = json!({"retro": retro.id, "card": retro.card_id, "pointers": pointers});
    tx.conn().execute(
        "INSERT INTO evidence (id, project_id, boundary, kind, ref, excerpt, card_id, independence_key, captured_at, captured_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            id,
            retro.project_id,
            boundary,
            kind,
            reference,
            learning.trim(),
            retro.card_id,
            retro.card_id,
            tx.now(),
            tx.actor().to_string(),
        ],
    )?;
    tx.event(
        NewEvent::new("evidence", &id, "evidence.captured")
            .card(&retro.card_id)
            .payload(json!({"kind": kind, "retro": retro.id})),
    )?;
    Ok(id)
}

/// A retro by id or short form, or the newest retro of a card reference.
pub fn resolve_retro(conn: &Connection, input: &str) -> Result<Retro> {
    if let Ok(id) = resolve_id(conn, "retros", "retro", input) {
        return get(conn, "retro", &id);
    }
    let card = resolve_card(conn, input)?;
    find(
        conn,
        "card_id = ?1 ORDER BY created_at DESC, id DESC",
        &[&card.id],
    )?
    .ok_or_else(|| Error::not_found("retro", input))
}

/// The hash over the sorted `diff_sha256` of a set's members, as
/// `eval_runs.members_sha256` stores it.
pub fn members_sha256_of(conn: &Connection, set_id: &str) -> Result<String> {
    members_sha256(conn, set_id)
}

/// The set a proposal belongs to that is not closed, for the CLI.
pub fn open_set_of(conn: &Connection, proposal_id: &str) -> Result<Option<ChangeSet>> {
    match open_membership(conn, proposal_id)? {
        Some(member) => Ok(Some(get(conn, "change set", &member.change_set_id)?)),
        None => Ok(None),
    }
}

/// The gate as a plain read over the record.
pub fn gate(conn: &Connection, input: &str) -> Result<Gate> {
    let row = proposal_row(conn, input)?;
    let mut gate = Gate {
        proposal: row.id.clone(),
        ok: false,
        reason: None,
        hint: None,
        change_set: None,
        eval_run: None,
        measuring: None,
    };
    if row.state != "proposed" {
        gate.reason = Some(format!("proposal is {}, not proposed", row.state));
        return Ok(gate);
    }
    let Some(member) = open_membership(conn, &row.id)? else {
        gate.reason = Some("the proposal is in no change set".into());
        gate.hint = Some(format!("tiller learn eval --proposal {}", row.id));
        return Ok(gate);
    };
    let set: ChangeSet = get(conn, "change set", &member.change_set_id)?;
    gate.change_set = Some(set.id.clone());
    let members = members_sha256(conn, &set.id)?;
    let newest: Option<EvalRun> = find(
        conn,
        "change_set_id = ?1 ORDER BY ran_at DESC, id DESC",
        &[&set.id],
    )?;
    match newest {
        None => {
            gate.reason = Some(format!("change set {} has no holdout run", set.name));
            gate.hint = Some(format!("tiller learn eval {}", set.name));
            return Ok(gate);
        }
        Some(run) if run.members_sha256 != members => {
            gate.eval_run = Some(run.id);
            gate.reason = Some(format!(
                "the newest holdout run of {} was made with other members; run it again",
                set.name
            ));
            gate.hint = Some(format!("tiller learn eval {}", set.name));
            return Ok(gate);
        }
        Some(run) if run.passed == 0 => {
            gate.eval_run = Some(run.id.clone());
            gate.reason = Some(format!(
                "the newest holdout run of {} did not pass ({} of {} cases{})",
                set.name,
                run.cases_passed,
                run.cases_total,
                if run.partial == 1 { ", partial" } else { "" }
            ));
            gate.hint = Some("write more tuning cases, then tiller learn eval again".into());
            return Ok(gate);
        }
        Some(run) => gate.eval_run = Some(run.id),
    }
    let others: Vec<ChangeSet> = list(
        conn,
        "state = 'measuring' AND id <> ?1 ORDER BY opened_at",
        &[&set.id],
    )?;
    for other in others {
        let Some(from) = other.measuring_from else {
            continue;
        };
        let closed = accepted_since(conn, from)?;
        if closed < other.window_cards {
            gate.measuring = Some(Measuring {
                change_set: other.id.clone(),
                name: other.name.clone(),
                cards_closed: closed,
                window_cards: other.window_cards,
            });
            gate.reason = Some(format!(
                "change set {} is measuring: {closed} of {} cards closed; one change at a time",
                other.name, other.window_cards
            ));
            gate.hint = Some(format!(
                "wait for the window, or tiller learn abandon {}",
                other.name
            ));
            return Ok(gate);
        }
    }
    gate.ok = true;
    Ok(gate)
}

fn window_report(conn: &Connection, set: ChangeSet) -> Result<WindowReport> {
    let from = set.measuring_from;
    let window_cards = accepted_cards(conn, from, None, set.window_cards)?;
    let before_cards = accepted_cards(conn, None, from, set.window_cards)?;
    let window = guardrails(conn, &window_cards)?;
    let before = guardrails(conn, &before_cards)?;
    let worse = worse(&window, &before);
    let mut hashes = BTreeSet::new();
    for card in &window_cards {
        hashes.extend(config_hashes(conn, card)?);
    }
    Ok(WindowReport {
        revert_suggested: !worse.is_empty(),
        worse,
        change_set: set,
        window,
        before,
        config_hashes: hashes.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests;
