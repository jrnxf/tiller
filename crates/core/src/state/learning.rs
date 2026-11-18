//! Retros (`memory.md` section 5): one retro per closed card, decided from
//! the record with no model call, and the manifest behind every run's
//! `config_hash`. A full retro runs only for an incident; its curator ends
//! it with `nothing` or one pending note.
//!
//! Nothing in these tables is rendered into a fleet role. The curator, which
//! finishes a `full` retro, is not a fleet role (`Actor::Curator`).

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::memory::{NewNote, insert_note};
use super::{find, get, list, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, HOST_HEALTH_CARD};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// The model a curator runs on.
pub const CURATOR_MODEL: &str = "sonnet";
/// The minutes a curator runs before it is cut.
pub const CURATOR_TIMEOUT_MINUTES: i64 = 20;

/// What one sweep did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Swept {
    pub scheduled: Vec<Retro>,
}

/// How a retro ends (`tiller retro finish`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finish {
    pub outcome: RetroOutcome,
    /// Why, for `unfinished`.
    pub learning: Option<String>,
    /// The one note, for `noted`: it joins the pending queue.
    pub note: Option<NewNote>,
}

fn count(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<i64> {
    Ok(conn.query_row(sql, params, |row| row.get(0))?)
}

fn elapsed_minutes(from: Timestamp, to: Timestamp) -> i64 {
    (to.as_millis() - from.as_millis()) / 60_000
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

/// Whether the record shows an incident on the card (`memory.md` section
/// 5): the card closed failed or abandoned, it reverts a landed change, or
/// one of its runs failed an attempt.
fn is_incident(conn: &Connection, card: &Card) -> Result<bool> {
    if matches!(
        card.outcome,
        Some(CardOutcome::Failed | CardOutcome::Abandoned)
    ) || card.category.as_deref() == Some("revert")
    {
        return Ok(true);
    }
    let failed = count(
        conn,
        "SELECT COUNT(*) FROM runs WHERE card_id = ?1 AND outcome = 'failed'",
        params![card.id],
    )?;
    Ok(failed > 0)
}

/// The depth and reason a card's retro gets, from the record alone.
pub(crate) fn decide(conn: &Connection, card: &Card) -> Result<(RetroDepth, RetroReason)> {
    if is_incident(conn, card)? {
        return Ok((RetroDepth::Full, RetroReason::Incident));
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

fn require_owner_or_daemon(actor: &Actor, what: &str) -> Result<()> {
    if actor.is_owner() || matches!(actor, Actor::Daemon) {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "{what} is for the owner or the daemon; {actor} may not"
        )))
    }
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
    /// `retro.schedule` for every closed card without one (`memory.md`
    /// section 5). Owner, daemon.
    pub fn learning_sweep(&mut self, ctx: &Ctx) -> Result<Applied<Swept>> {
        self.write(ctx, "learning.sweep", None, |tx| {
            require_owner_or_daemon(tx.actor(), "tiller retro sweep")?;
            let mut swept = Swept::default();
            let unretroed: Vec<Card> = list(
                tx.conn(),
                "state = 'closed' AND outcome <> 'cancelled' AND display_id <> ?1
                   AND NOT EXISTS (SELECT 1 FROM retros r WHERE r.card_id = cards.id
                                   AND r.closed_at = cards.closed_at AND r.trigger IS NULL)
                 ORDER BY closed_at, id",
                &[&HOST_HEALTH_CARD],
            )?;
            for card in unretroed {
                let (depth, reason) = decide(tx.conn(), &card)?;
                swept
                    .scheduled
                    .push(insert_retro(tx, &card, None, depth, reason)?);
            }
            Ok(swept)
        })
    }

    /// `retro.start`: records the curator session on a pending retro. A
    /// retro whose curator started less than `timeout_minutes` ago is still
    /// running and is refused; an older start was a run that died. A retro
    /// that ended `unfinished` is reset to `pending` first (a
    /// `retro.reset` event), so the owner can run it again once the cause
    /// is fixed (learning-loop.md, section 4).
    pub fn retro_start(
        &mut self,
        ctx: &Ctx,
        retro: &str,
        curator_session: Option<&str>,
        timeout_minutes: i64,
    ) -> Result<Applied<Retro>> {
        self.write(ctx, "retro.start", None, |tx| {
            require_owner_or_daemon(tx.actor(), "tiller retro run")?;
            let mut retro = resolve_retro(tx.conn(), retro)?;
            if check_start(&retro, tx.now(), timeout_minutes)? {
                tx.conn().execute(
                    "UPDATE retros SET state = 'pending', outcome = NULL, learning = NULL, pending_id = NULL,
                            finished_at = NULL, finished_by = NULL, rev = rev + 1
                     WHERE id = ?1",
                    [&retro.id],
                )?;
                tx.event(
                    NewEvent::new("retro", &retro.id, "retro.reset")
                        .rev(retro.rev + 1)
                        .card(&retro.card_id)
                        .payload(json!({
                            "outcome": retro.outcome,
                            "learning": retro.learning,
                            "curator_session": retro.curator_session,
                            "finished_at": retro.finished_at,
                            "finished_by": retro.finished_by,
                        })),
                )?;
                retro = get(tx.conn(), "retro", &retro.id)?;
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
    /// owner's verdict on a full retro: `nothing`, one note for the pending
    /// queue (`noted`, with `source = curator`, in the same transaction), or
    /// `unfinished`. A `light` retro cannot be finished; a done retro
    /// finished again with the same outcome is a replay.
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
            let pending_id = match (finish.outcome, &finish.note) {
                (RetroOutcome::Noted, Some(note)) => Some(insert_note(tx, note, Some(&retro))?.id),
                (RetroOutcome::Noted, None) => {
                    return Err(Error::Usage("a noted retro names its one note".into()));
                }
                (_, Some(_)) => {
                    return Err(Error::Usage(format!(
                        "a retro that ends {} adds no note",
                        finish.outcome
                    )));
                }
                (_, None) => None,
            };
            let now = tx.now();
            let by = tx.actor().to_string();
            tx.conn().execute(
                "UPDATE retros SET state = 'done', outcome = ?2, learning = ?3, pending_id = ?4,
                        finished_at = ?5, finished_by = ?6, rev = rev + 1 WHERE id = ?1",
                params![
                    retro.id,
                    finish.outcome,
                    finish.learning,
                    pending_id,
                    now,
                    by
                ],
            )?;
            tx.event(
                NewEvent::new("retro", &retro.id, "retro.finished")
                    .rev(retro.rev + 1)
                    .card(&retro.card_id)
                    .payload(json!({"outcome": finish.outcome, "pending_id": pending_id})),
            )?;
            get(tx.conn(), "retro", &retro.id)
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
}

/// Whether a curator may start on `retro` now, the rule `retro.start`
/// applies. Refuses a retro that is done with any outcome but `unfinished`,
/// and a pending retro whose curator started less than `timeout_minutes`
/// before `now` (it is still running). `Ok(true)` means the retro ended
/// `unfinished` and is reset to `pending` before the start; that run is
/// over, so its start time does not count.
pub fn check_start(retro: &Retro, now: Timestamp, timeout_minutes: i64) -> Result<bool> {
    if retro.state == RetroState::Done {
        return match retro.outcome {
            Some(RetroOutcome::Unfinished) => Ok(true),
            outcome => Err(Error::refused(
                "retro",
                &retro.id,
                format!(
                    "retro is done with outcome {}; only an unfinished retro runs again",
                    outcome.map_or("", RetroOutcome::as_str)
                ),
            )),
        };
    }
    if let Some(started) = retro.curator_started_at
        && started.plus_minutes(timeout_minutes) > now
    {
        return Err(Error::refused(
            "retro",
            &retro.id,
            format!(
                "a curator started at {started} is still running; it is cut after {timeout_minutes} minutes"
            ),
        ));
    }
    Ok(false)
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

#[cfg(test)]
mod tests;
