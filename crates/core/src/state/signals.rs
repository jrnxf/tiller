//! The Health page's counts (`protocol.md` section 7.3, `GET /api/v1/signals`):
//! plain queries over `state.db`, one row of counts per week. Reads never
//! write. The board turns the counts into a verdict and statuses. Each time
//! the developer was needed is also classified (`interventions.rs`), and the
//! week carries those counts beside the plain ones.
//!
//! The caller picks the weeks (Monday 00:00 to Monday 00:00 Lisbon time), so
//! the zone rule stays in one place, and every window is half open: a time on
//! the boundary belongs to the week that starts there.

use std::collections::BTreeMap;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::interventions::{Intervention, InterventionCounts};
use crate::error::Result;
use crate::store::Store;
use crate::time::Timestamp;

/// A host counts as live when it was seen within this long (`protocol.md`
/// section 9).
const HOST_LIVE_MINUTES: i64 = 2;

/// The runs and sessions that overlap in time, and so would be a duplicate
/// dispatch, counted in the week the later one began.
const OVERLAPPING_RUNS: &str = "SELECT COUNT(*) FROM runs a JOIN runs b
        ON a.card_id = b.card_id AND a.role = b.role AND a.id < b.id
       AND a.started_at < COALESCE(b.ended_at, ?3)
       AND b.started_at < COALESCE(a.ended_at, ?3)
     WHERE MAX(a.started_at, b.started_at) >= ?1 AND MAX(a.started_at, b.started_at) < ?2";

const SHARED_PANES: &str = "SELECT COUNT(*) FROM sessions a JOIN sessions b
        ON a.pane_id = b.pane_id AND a.host_id = b.host_id AND a.id < b.id
       AND a.started_at < COALESCE(b.ended_at, ?3)
       AND b.started_at < COALESCE(a.ended_at, ?3)
     WHERE MAX(a.started_at, b.started_at) >= ?1 AND MAX(a.started_at, b.started_at) < ?2";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekWindow {
    pub start: Timestamp,
    pub end: Timestamp,
}

/// How many interventions the page lists to relabel, newest first.
const LISTED_INTERVENTIONS: usize = 40;

/// Everything the page shows for one week.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeekCounts {
    pub start: Timestamp,
    pub end: Timestamp,
    /// Shipping: cards closed `landed`, scout cards closed `reported`, and the
    /// PRs merged in the week.
    pub landed: i64,
    pub reported: i64,
    pub merged_prs: i64,
    /// Your time: attention items opened that were not withdrawn, and the
    /// median minutes from opening to answer over items answered in the week.
    pub needed: i64,
    /// How many items were answered in the week: the sample behind the median.
    pub answered: i64,
    pub median_answer_minutes: Option<f64>,
    /// Why the developer was needed, after the developer's own relabels.
    pub interventions: InterventionCounts,
    /// Quality: reverts and escaped bugs are cards created in the week;
    /// rework is builder attempts beyond the first.
    pub reverts: i64,
    pub escaped: i64,
    pub rework: i64,
    /// Safety: lost work and unauthorized actions are `guard.breach` events;
    /// duplicate dispatches are overlapping runs or sessions sharing a pane.
    pub lost_work: i64,
    pub duplicate_dispatches: i64,
    pub unauthorized: i64,
}

/// One project's counts for the newest week.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectCounts {
    pub project_id: String,
    pub accepted: i64,
    pub needed: i64,
    /// Avoidable interventions: corrections, unplanned escalations and rework.
    pub avoidable: i64,
    pub escaped: i64,
    pub answered: i64,
    pub median_answer_minutes: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    /// Oldest first; the last is the week in progress.
    pub weeks: Vec<WeekCounts>,
    pub by_project: Vec<ProjectCounts>,
    /// The latest interventions across the weeks shown, newest first, for the
    /// developer to relabel.
    pub interventions: Vec<Intervention>,
    pub machines_live: i64,
    pub machines_total: i64,
}

/// The last `count` Lisbon weeks (Monday 00:00 to Monday 00:00),
/// oldest first; the last holds `now`.
pub fn lisbon_weeks(now: Timestamp, count: usize) -> Vec<WeekWindow> {
    // Europe/Lisbon's own POSIX rule, so no timezone database is needed
    // (the CLI's Lisbon times work the same way).
    let Ok(zone) = jiff::tz::TimeZone::posix("WET0WEST,M3.5.0/1,M10.5.0") else {
        return Vec::new();
    };
    let local = now.as_jiff().to_zoned(zone);
    let days_from_monday = i64::from(local.weekday().to_monday_zero_offset());
    let Ok(monday) = local
        .date()
        .checked_sub(jiff::Span::new().days(days_from_monday))
    else {
        return Vec::new();
    };
    let mut weeks = Vec::new();
    for back in (0..count).rev() {
        let back = i64::try_from(back).unwrap_or(0);
        let start = monday.checked_sub(jiff::Span::new().weeks(back));
        let end = monday.checked_sub(jiff::Span::new().weeks(back - 1));
        let (Ok(start), Ok(end)) = (start, end) else {
            continue;
        };
        let zoned = |date: jiff::civil::Date| {
            date.to_zoned(local.time_zone().clone())
                .map(|z| Timestamp::from_jiff(z.timestamp()))
        };
        if let (Ok(start), Ok(end)) = (zoned(start), zoned(end)) {
            weeks.push(WeekWindow { start, end });
        }
    }
    weeks
}
fn count(store: &Store, sql: &str, params: &[&dyn rusqlite::ToSql]) -> Result<i64> {
    Ok(store.conn().query_row(sql, params, |row| row.get(0))?)
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

/// Rounded to a tenth of a minute, as the page shows it.
fn tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

impl Store {
    pub fn signals(&self, windows: &[WeekWindow], now: Timestamp) -> Result<Signals> {
        let weeks = windows
            .iter()
            .map(|window| self.week_counts(*window, now))
            .collect::<Result<Vec<_>>>()?;
        let by_project = match windows.last() {
            Some(window) => self.project_counts(*window)?,
            None => Vec::new(),
        };
        let (machines_live, machines_total) = self.machines_live(now)?;
        let mut interventions = match (windows.first(), windows.last()) {
            (Some(first), Some(last)) => self.interventions(first.start, last.end)?,
            _ => Vec::new(),
        };
        interventions.reverse();
        interventions.truncate(LISTED_INTERVENTIONS);
        Ok(Signals {
            weeks,
            by_project,
            interventions,
            machines_live,
            machines_total,
        })
    }

    fn week_counts(&self, window: WeekWindow, now: Timestamp) -> Result<WeekCounts> {
        let (s, e) = (window.start, window.end);
        let closed = |outcome: &str| {
            count(
                self,
                "SELECT COUNT(*) FROM cards
                 WHERE state = 'closed' AND outcome = ?3 AND closed_at >= ?1 AND closed_at < ?2",
                params![s, e, outcome],
            )
        };
        let breaches = |op: &str| {
            count(
                self,
                &format!(
                    "SELECT COUNT(*) FROM events
                     WHERE kind = 'guard.breached' AND entity_id {op} 'G19' AND at >= ?1 AND at < ?2"
                ),
                params![s, e],
            )
        };
        Ok(WeekCounts {
            start: s,
            end: e,
            landed: closed("landed")?,
            reported: closed("reported")?,
            merged_prs: count(
                self,
                "SELECT COUNT(*) FROM prs
                 WHERE state = 'merged' AND merged_at >= ?1 AND merged_at < ?2",
                params![s, e],
            )?,
            needed: count(
                self,
                "SELECT COUNT(*) FROM attention
                 WHERE withdrawn_at IS NULL AND opened_at >= ?1 AND opened_at < ?2",
                params![s, e],
            )?,
            answered: count(
                self,
                "SELECT COUNT(*) FROM attention WHERE answered_at >= ?1 AND answered_at < ?2",
                params![s, e],
            )?,
            median_answer_minutes: self.median_answer(window, None)?,
            interventions: self.intervention_counts(s, e)?.0,
            reverts: count(
                self,
                "SELECT COUNT(*) FROM cards
                 WHERE category = 'revert' AND created_at >= ?1 AND created_at < ?2",
                params![s, e],
            )?,
            escaped: self.escaped(window, None)?,
            rework: count(
                self,
                "SELECT COUNT(*) FROM runs
                 WHERE role = 'builder' AND attempt > 1 AND started_at >= ?1 AND started_at < ?2",
                params![s, e],
            )?,
            lost_work: breaches("=")?,
            duplicate_dispatches: count(self, OVERLAPPING_RUNS, params![s, e, now])?
                + count(self, SHARED_PANES, params![s, e, now])?,
            unauthorized: breaches("<>")?,
        })
    }

    /// Cards created in the window that point back at a card already landed
    /// before them. A revert is its own count, not an escaped bug.
    fn escaped(&self, window: WeekWindow, project: Option<&str>) -> Result<i64> {
        count(
            self,
            "SELECT COUNT(*) FROM cards c JOIN cards origin ON origin.id = c.fixes_card_id
             WHERE COALESCE(c.category, '') <> 'revert'
               AND origin.outcome = 'landed' AND origin.closed_at < c.created_at
               AND c.created_at >= ?1 AND c.created_at < ?2
               AND (?3 IS NULL OR c.project_id = ?3)",
            params![window.start, window.end, project],
        )
    }

    fn median_answer(&self, window: WeekWindow, project: Option<&str>) -> Result<Option<f64>> {
        let mut stmt = self.conn().prepare(
            "SELECT (julianday(a.answered_at) - julianday(a.opened_at)) * 1440.0
             FROM attention a JOIN cards c ON c.id = a.card_id
             WHERE a.answered_at >= ?1 AND a.answered_at < ?2 AND (?3 IS NULL OR c.project_id = ?3)",
        )?;
        let minutes = stmt
            .query_map(params![window.start, window.end, project], |row| {
                row.get::<_, f64>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(median(minutes).map(tenth))
    }

    fn project_counts(&self, window: WeekWindow) -> Result<Vec<ProjectCounts>> {
        let mut rows: BTreeMap<String, ProjectCounts> = self
            .projects()?
            .into_iter()
            .map(|project| {
                let counts = ProjectCounts {
                    project_id: project.id.clone(),
                    accepted: 0,
                    needed: 0,
                    avoidable: 0,
                    escaped: 0,
                    answered: 0,
                    median_answer_minutes: None,
                };
                (project.id, counts)
            })
            .collect();
        let (_, interventions) = self.intervention_counts(window.start, window.end)?;
        for row in rows.values_mut() {
            let id = row.project_id.clone();
            row.accepted = count(
                self,
                "SELECT COUNT(*) FROM cards
                 WHERE state = 'closed' AND outcome IN ('landed', 'reported')
                   AND closed_at >= ?1 AND closed_at < ?2 AND project_id = ?3",
                params![window.start, window.end, id],
            )?;
            row.needed = count(
                self,
                "SELECT COUNT(*) FROM attention a JOIN cards c ON c.id = a.card_id
                 WHERE a.withdrawn_at IS NULL AND a.opened_at >= ?1 AND a.opened_at < ?2
                   AND c.project_id = ?3",
                params![window.start, window.end, id],
            )?;
            row.avoidable = interventions
                .get(&id)
                .map_or(0, InterventionCounts::avoidable);
            row.answered = count(
                self,
                "SELECT COUNT(*) FROM attention a JOIN cards c ON c.id = a.card_id
                 WHERE a.answered_at >= ?1 AND a.answered_at < ?2 AND c.project_id = ?3",
                params![window.start, window.end, id],
            )?;
            row.escaped = self.escaped(window, Some(&id))?;
            row.median_answer_minutes = self.median_answer(window, Some(&id))?;
        }
        Ok(rows.into_values().collect())
    }

    /// The local host is the daemon itself, so it is live; a remote host is
    /// live when it was seen in the last two minutes.
    fn machines_live(&self, now: Timestamp) -> Result<(i64, i64)> {
        let cutoff = now.plus_minutes(-HOST_LIVE_MINUTES);
        let live = count(
            self,
            "SELECT COUNT(*) FROM hosts WHERE kind = 'local' OR last_seen_at >= ?1",
            params![cutoff],
        )?;
        let total = count(self, "SELECT COUNT(*) FROM hosts", params![])?;
        Ok((live, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::memory_store;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn week() -> WeekWindow {
        WeekWindow {
            start: at("2026-10-26T00:00:00Z"),
            end: at("2026-11-02T00:00:00Z"),
        }
    }

    fn earlier() -> WeekWindow {
        WeekWindow {
            start: at("2026-10-19T00:00:00Z"),
            end: at("2026-10-26T00:00:00Z"),
        }
    }

    fn sql(store: &Store, text: &str) {
        store.conn().execute_batch(text).unwrap();
    }

    fn card(store: &Store, id: &str, extra: &str) {
        // `extra` is `state, outcome, closed_at, category, fixes_card_id, created_at`.
        sql(
            store,
            &format!(
                "INSERT INTO cards (id, display_id, project_id, kind, title, state, outcome, closed_at, category, fixes_card_id, created_at)
                 VALUES ('{id}', 'alpha-{id}', 'alpha', 'ship', 't', {extra})"
            ),
        );
    }

    fn signals(store: &Store) -> Signals {
        store
            .signals(&[earlier(), week()], at("2026-11-01T12:00:00Z"))
            .unwrap()
    }

    #[test]
    fn lisbon_weeks_start_on_monday_midnight_lisbon_time() {
        // Monday 2026-11-02 15:00 UTC is 15:00 Lisbon (WET, UTC+0).
        let now: Timestamp = "2026-11-02T15:00:00.000Z".parse().unwrap();
        let weeks = lisbon_weeks(now, 8);
        assert_eq!(weeks.len(), 8);
        let last = weeks.last().unwrap();
        assert_eq!(last.start.to_string(), "2026-11-02T00:00:00.000Z");
        assert_eq!(last.end.to_string(), "2026-11-09T00:00:00.000Z");
        // Seven weeks back is in summer time (WEST): that Monday's
        // midnight is 23:00 UTC the day before.
        let first = weeks.first().unwrap();
        assert_eq!(first.start.to_string(), "2026-09-13T23:00:00.000Z");
        for pair in weeks.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
    }

    #[test]
    fn shipping_counts_landed_cards_by_the_week_they_closed() {
        let (store, _) = memory_store();
        card(
            &store,
            "c1",
            "'closed', 'landed', '2026-10-27T09:00:00.000Z', NULL, NULL, '2026-10-20T09:00:00.000Z'",
        );
        card(
            &store,
            "c2",
            "'closed', 'landed', '2026-10-20T09:00:00.000Z', NULL, NULL, '2026-10-19T09:00:00.000Z'",
        );
        card(
            &store,
            "c3",
            "'closed', 'reported', '2026-11-01T23:59:59.999Z', NULL, NULL, '2026-10-30T09:00:00.000Z'",
        );
        // On the boundary: belongs to the week that starts there.
        card(
            &store,
            "c4",
            "'closed', 'landed', '2026-11-02T00:00:00.000Z', NULL, NULL, '2026-10-30T09:00:00.000Z'",
        );
        card(
            &store,
            "c5",
            "'closed', 'cancelled', '2026-10-28T09:00:00.000Z', NULL, NULL, '2026-10-20T09:00:00.000Z'",
        );
        card(
            &store,
            "c6",
            "'open', NULL, NULL, NULL, NULL, '2026-10-20T09:00:00.000Z'",
        );
        sql(
            &store,
            "INSERT INTO prs (id, project_id, card_id, number, url, linked_at, linked_by, state, merged_at)
             VALUES ('p1', 'alpha', 'c1', 1, 'https://example.com/pr/1', '2026-10-26T10:00:00.000Z', 'x', 'merged', '2026-10-27T09:00:00.000Z'),
                    ('p2', 'alpha', 'c6', 2, 'https://example.com/pr/2', '2026-10-26T10:00:00.000Z', 'x', 'open', NULL)",
        );
        let signals = signals(&store);
        let [before, now] = signals.weeks.as_slice() else {
            panic!("two weeks expected");
        };
        assert_eq!((before.landed, before.reported), (1, 0));
        assert_eq!((now.landed, now.reported, now.merged_prs), (1, 1, 1));
        assert_eq!(signals.by_project[0].accepted, 2);
    }

    #[test]
    fn your_time_counts_items_and_takes_the_median_answer_time() {
        let (store, _) = memory_store();
        card(
            &store,
            "c1",
            "'open', NULL, NULL, NULL, NULL, '2026-10-20T09:00:00.000Z'",
        );
        for (id, opened, answered, withdrawn) in [
            (
                "a1",
                "2026-10-27T10:00:00.000Z",
                Some("2026-10-27T10:04:00.000Z"),
                None,
            ),
            (
                "a2",
                "2026-10-27T11:00:00.000Z",
                Some("2026-10-27T11:10:00.000Z"),
                None,
            ),
            (
                "a3",
                "2026-10-28T11:00:00.000Z",
                Some("2026-10-28T11:30:00.000Z"),
                None,
            ),
            (
                "a4",
                "2026-10-29T11:00:00.000Z",
                None,
                Some("2026-10-29T12:00:00.000Z"),
            ),
            ("a5", "2026-10-30T11:00:00.000Z", None, None),
        ] {
            let answer = answered.map_or("NULL, NULL, NULL".to_owned(), |t| {
                format!("'{t}', 'yes', 'board'")
            });
            let withdrawn = withdrawn.map_or("NULL".to_owned(), |t| format!("'{t}'"));
            sql(
                &store,
                &format!(
                    "INSERT INTO attention (id, card_id, kind, question, opened_at, answered_at, answer, answered_via, withdrawn_at)
                     VALUES ('{id}', 'c1', 'decision', 'q', '{opened}', {answer}, {withdrawn})"
                ),
            );
        }
        let now = signals(&store).weeks.pop().unwrap();
        // a4 was withdrawn, so it did not need the developer.
        assert_eq!(now.needed, 4);
        assert_eq!(now.median_answer_minutes, Some(10.0));
    }

    #[test]
    fn quality_separates_reverts_from_bugs_that_escaped_after_landing() {
        let (store, session) = memory_store();
        card(
            &store,
            "c1",
            "'closed', 'landed', '2026-10-20T09:00:00.000Z', NULL, NULL, '2026-10-19T09:00:00.000Z'",
        );
        card(
            &store,
            "bug",
            "'open', NULL, NULL, 'bug', 'c1', '2026-10-28T09:00:00.000Z'",
        );
        card(
            &store,
            "rev",
            "'open', NULL, NULL, 'revert', 'c1', '2026-10-29T09:00:00.000Z'",
        );
        // A fix for a card that had not landed yet is rework inside the card, not an escape.
        card(
            &store,
            "open",
            "'open', NULL, NULL, NULL, NULL, '2026-10-21T09:00:00.000Z'",
        );
        card(
            &store,
            "early",
            "'open', NULL, NULL, 'bug', 'open', '2026-10-28T09:00:00.000Z'",
        );
        let host: String = store
            .conn()
            .query_row("SELECT id FROM hosts", [], |row| row.get(0))
            .unwrap();
        for (id, attempt, started) in [
            ("r1", 1, "2026-10-27T09:00:00.000Z"),
            ("r2", 2, "2026-10-28T09:00:00.000Z"),
            ("r3", 3, "2026-10-19T09:00:00.000Z"),
        ] {
            sql(
                &store,
                &format!(
                    "INSERT INTO runs (id, card_id, role, attempt, model, effort, host_id, supervisor_session_id, started_at, ended_at, outcome)
                     VALUES ('{id}', 'open', 'builder', {attempt}, 'm', 'high', '{host}', '{session}', '{started}', '{started}', 'failed')"
                ),
            );
        }
        let now = signals(&store).weeks.pop().unwrap();
        assert_eq!((now.escaped, now.reverts, now.rework), (1, 1, 1));
    }

    #[test]
    fn safety_counts_breaches_overlapping_runs_and_live_machines() {
        let (store, session) = memory_store();
        card(
            &store,
            "c1",
            "'open', NULL, NULL, NULL, NULL, '2026-10-20T09:00:00.000Z'",
        );
        let host: String = store
            .conn()
            .query_row("SELECT id FROM hosts", [], |row| row.get(0))
            .unwrap();
        // The unique indexes refuse this by construction; drop them to prove the
        // count would show it if a bug ever let it through.
        sql(
            &store,
            "DROP INDEX runs_active_role; DROP INDEX runs_attempt;",
        );
        for (id, started) in [
            ("r1", "2026-10-27T09:00:00.000Z"),
            ("r2", "2026-10-27T10:00:00.000Z"),
        ] {
            sql(
                &store,
                &format!(
                    "INSERT INTO runs (id, card_id, role, attempt, model, effort, host_id, supervisor_session_id, started_at)
                     VALUES ('{id}', 'c1', 'builder', 1, 'm', 'high', '{host}', '{session}', '{started}')"
                ),
            );
        }
        for (id, entity_id, when) in [
            ("e1", "G19", "2026-10-28T09:00:00.000Z"),
            ("e2", "G11", "2026-10-29T09:00:00.000Z"),
            ("e3", "R19", "2026-10-20T09:00:00.000Z"),
        ] {
            sql(
                &store,
                &format!(
                    "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind)
                     VALUES ('{id}', '{when}', 'guard.breach', 'daemon', 'guard', '{entity_id}', 'guard.breached')"
                ),
            );
        }
        sql(
            &store,
            "INSERT INTO hosts (id, label, kind, last_seen_at)
             VALUES ('h2', 'host-b', 'remote', '2026-11-01T11:59:00.000Z'),
                    ('h3', 'host-c', 'remote', '2026-11-01T11:00:00.000Z')",
        );
        let signals = signals(&store);
        let [before, now] = signals.weeks.as_slice() else {
            panic!("two weeks expected");
        };
        assert_eq!(
            (now.lost_work, now.unauthorized, now.duplicate_dispatches),
            (1, 1, 1)
        );
        assert_eq!((before.lost_work, before.unauthorized), (0, 1));
        // The local host, and the remote seen a minute ago; not the one seen an hour ago.
        assert_eq!((signals.machines_live, signals.machines_total), (2, 3));
    }

    #[test]
    fn an_empty_database_gives_zero_counts_and_no_median() {
        let (store, _) = memory_store();
        let now = signals(&store).weeks.pop().unwrap();
        assert_eq!(
            (
                now.landed,
                now.needed,
                now.escaped,
                now.duplicate_dispatches
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(now.median_answer_minutes, None);
    }

    /// The board reads these field names (`board/src/lib/api-types.ts`,
    /// `SignalsWeek`); a rename here must break this test, not the page.
    #[test]
    fn a_week_serializes_to_the_fields_the_board_reads() {
        let (store, _) = memory_store();
        let value = serde_json::to_value(signals(&store)).unwrap();
        let mut keys: Vec<&str> = value["weeks"][1]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "answered",
                "duplicate_dispatches",
                "end",
                "escaped",
                "interventions",
                "landed",
                "lost_work",
                "median_answer_minutes",
                "merged_prs",
                "needed",
                "reported",
                "reverts",
                "rework",
                "start",
                "unauthorized"
            ]
        );
        assert!(value["weeks"][1]["median_answer_minutes"].is_null());
        assert_eq!(value["weeks"][1]["start"], "2026-10-26T00:00:00.000Z");
    }
}
