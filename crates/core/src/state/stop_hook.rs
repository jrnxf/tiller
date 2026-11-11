//! The Stop hook's measurement and the rule for switching it to `block-once`
//! (`protocol.md` 2.5; CP2 C14). `tiller intake stats`, `tiller config set`
//! and the board's Settings page all judge the switch with these numbers.

use jiff::{Timestamp as Jiff, ToSpan};
use serde::Serialize;
use serde_json::Value;

use super::{find, list};
use crate::Store;
use crate::domain::{Ask, Card, Observation};
use crate::error::Result;
use crate::time::Timestamp;

/// Orchestrator stops the last window must hold before `block-once`.
pub const STOP_HOOK_MIN_STOPS: i64 = 200;
/// The false-block rate the window must stay under before `block-once`.
pub const STOP_HOOK_MAX_RATE: f64 = 0.02;
/// The window the rule looks at, in days.
pub const STOP_HOOK_DAYS: i64 = 7;
/// A `would_block` stop is a false block when the session settles it this fast.
const FALSE_BLOCK_WINDOW_MINUTES: i64 = 2;

#[derive(Clone, Debug, Serialize)]
pub struct IntakeStats {
    pub days: i64,
    pub stops: i64,
    pub would_block: i64,
    pub false_blocks: i64,
    /// Null until there is a would-block stop to judge.
    pub false_block_rate: Option<f64>,
    pub min_stops: i64,
    pub max_false_block_rate: f64,
    /// The owner may switch to `block-once` (`protocol.md` 2.5).
    pub rule_met: bool,
}

impl IntakeStats {
    /// Why `block-once` is refused now, in plain words, or `None` when the
    /// rule is met.
    pub fn refusal(&self) -> Option<String> {
        (!self.rule_met).then(|| {
            format!(
                "block-once needs {STOP_HOOK_MIN_STOPS} orchestrator stops in {} days with a false-block rate under {:.0}%; there are {} stops and a rate of {}",
                self.days,
                STOP_HOOK_MAX_RATE * 100.0,
                self.stops,
                self.false_block_rate
                    .map_or("unknown".to_owned(), |rate| format!("{:.1}%", rate * 100.0)),
            )
        })
    }
}

/// The Stop hook's measurement over a window (`protocol.md` 2.5).
///
/// A `would_block` stop is a false block when, within two minutes and without
/// another owner prompt, the same session resolves every listed ask and every
/// unknown tag turns out to be a card created in that window.
pub fn intake_stats(store: &Store, days: i64) -> Result<IntakeStats> {
    intake_stats_at(store, days, Jiff::now())
}

/// [`intake_stats`] for a window that ends at `now`.
pub fn intake_stats_at(store: &Store, days: i64, now: Jiff) -> Result<IntakeStats> {
    let since = Timestamp::from_jiff(plus_minutes(now, -days * 24 * 60));
    let stops: Vec<Observation> = list(
        store.conn(),
        "event = 'Stop' AND observed_at >= ?1
           AND session_id IN (SELECT id FROM sessions WHERE role = 'orchestrator')
         ORDER BY observed_at, id",
        &[&since],
    )?;
    let mut would_block = 0;
    let mut false_blocks = 0;
    for stop in &stops {
        if stop.payload.get("would_block").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        would_block += 1;
        if is_false_block(store, stop)? {
            false_blocks += 1;
        }
    }
    let total = stops.len() as i64;
    let rate = (would_block > 0).then(|| false_blocks as f64 / would_block as f64);
    // The rate is over would-block stops; no would-block stop is a rate of 0.
    let judged = rate.unwrap_or(0.0);
    Ok(IntakeStats {
        days,
        stops: total,
        would_block,
        false_blocks,
        false_block_rate: rate,
        min_stops: STOP_HOOK_MIN_STOPS,
        max_false_block_rate: STOP_HOOK_MAX_RATE,
        rule_met: total >= STOP_HOOK_MIN_STOPS && judged < STOP_HOOK_MAX_RATE,
    })
}

fn plus_minutes(ts: Jiff, minutes: i64) -> Jiff {
    ts.checked_add(minutes.minutes()).unwrap_or(ts)
}

fn is_false_block(store: &Store, stop: &Observation) -> Result<bool> {
    let Some(session) = &stop.session_id else {
        return Ok(false);
    };
    let until = Timestamp::from_jiff(plus_minutes(
        stop.observed_at.as_jiff(),
        FALSE_BLOCK_WINDOW_MINUTES,
    ));
    // Another owner prompt in the window means the owner moved on: not false.
    let prompted: i64 = store.conn().query_row(
        "SELECT count(*) FROM observations
         WHERE session_id = ?1 AND event = 'UserPromptSubmit' AND observed_at > ?2 AND observed_at <= ?3",
        rusqlite::params![session, stop.observed_at, until],
        |row| row.get(0),
    )?;
    if prompted > 0 {
        return Ok(false);
    }
    let strings = |key: &str| -> Vec<String> {
        stop.payload
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|text| text.trim_matches(['[', ']']).to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    for ask_id in strings("unresolved_ask_ids") {
        let resolved: Option<Ask> = find(
            store.conn(),
            "id = ?1 AND disposition != 'unresolved' AND resolved_by_session_id = ?2
             AND resolved_at > ?3 AND resolved_at <= ?4",
            &[&ask_id, session, &stop.observed_at, &until],
        )?;
        if resolved.is_none() {
            return Ok(false);
        }
    }
    for display_id in strings("unknown_tags") {
        let created: Option<Card> = find(
            store.conn(),
            "display_id = ?1 AND created_at > ?2 AND created_at <= ?3",
            &[&display_id, &stop.observed_at, &until],
        )?;
        if created.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(stops: i64, would_block: i64, false_blocks: i64) -> IntakeStats {
        let rate = (would_block > 0).then(|| false_blocks as f64 / would_block as f64);
        IntakeStats {
            days: STOP_HOOK_DAYS,
            stops,
            would_block,
            false_blocks,
            false_block_rate: rate,
            min_stops: STOP_HOOK_MIN_STOPS,
            max_false_block_rate: STOP_HOOK_MAX_RATE,
            rule_met: stops >= STOP_HOOK_MIN_STOPS && rate.unwrap_or(0.0) < STOP_HOOK_MAX_RATE,
        }
    }

    #[test]
    fn a_refusal_names_the_numbers_and_a_met_rule_has_none() {
        let refused = stats(146, 9, 3).refusal().unwrap();
        assert!(
            refused.contains("200 orchestrator stops in 7 days"),
            "{refused}"
        );
        assert!(refused.contains("under 2%"), "{refused}");
        assert!(
            refused.contains("146 stops and a rate of 33.3%"),
            "{refused}"
        );
        assert!(stats(10, 0, 0).refusal().unwrap().contains("10 stops"));
        assert!(stats(200, 0, 0).refusal().is_none());
        assert!(stats(300, 100, 1).refusal().is_none());
        assert!(stats(300, 100, 2).refusal().is_some());
    }
}
