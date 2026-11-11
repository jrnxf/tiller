//! The recap: what happened since the owner last looked (`tiller recap` on
//! the command line, the Inbox's "Since you left" card on the board). Both
//! read the same events, through [`events_since`], so the two never count
//! differently. Where the owner last looked lives in the log too: each
//! `recap.mark_seen` writes one `recap.seen` event, and the newest is the
//! mark. The mark is the daemon's, so it holds across every device.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::list;
use super::projects::require_owner;
use crate::domain::Event;
use crate::error::Result;
use crate::store::{Applied, Ctx, NewEvent, Store};
use crate::time::Timestamp;

/// The window a recap covers when the owner has never marked one seen.
pub const DEFAULT_WINDOW_MINUTES: i64 = 24 * 60;

/// The events worth a recap line: what the owner would want to hear happened.
/// The notify kinds (`attention.notified`, `attention.notify_sent`,
/// `attention.notify_failed`) stay out: the item's own `attention.opened` line
/// is the news, and a failed ping shows on the card timeline.
const RECAP_KINDS: &str = "'card.created', 'card.closed', 'run.started', 'run.ended', \
    'attention.opened', 'attention.answered', 'attention.withdrawn', 'approval.requested', \
    'approval.decided', 'approval.finished', 'pr.opened', 'pr.merged', 'deploy.finished'";

/// The recap events after `since`, newest first, at most `limit`. With a
/// `project`, only its cards' events: the filter runs before the limit, so a
/// busy fleet's other projects cannot crowd this one out. Card-less events
/// are fleet-wide and only a recap of the whole fleet lists them.
pub fn events_since(
    conn: &Connection,
    since: Timestamp,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<Event>> {
    match project {
        Some(project) => list(
            conn,
            &format!(
                "at > ?1 AND kind IN ({RECAP_KINDS}) AND card_id IN \
                 (SELECT id FROM cards WHERE project_id = ?3) ORDER BY seq DESC LIMIT ?2"
            ),
            &[&since, &limit, &project],
        ),
        None => list(
            conn,
            &format!("at > ?1 AND kind IN ({RECAP_KINDS}) ORDER BY seq DESC LIMIT ?2"),
            &[&since, &limit],
        ),
    }
}

/// What the recap counts for the board: four numbers the owner scans.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tally {
    /// Cards that closed landed.
    pub landed: i64,
    /// Decisions opened for the owner.
    pub decisions: i64,
    /// Runs that ended failed.
    pub failed_runs: i64,
    /// Memory proposals made.
    pub suggestions: i64,
}

impl Tally {
    pub fn is_quiet(&self) -> bool {
        *self == Self::default()
    }
}

/// The recap the board shows: the window and what happened in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recap {
    /// When the owner last marked a recap seen; `None` before the first.
    pub seen_at: Option<Timestamp>,
    /// The start of the window counted: `seen_at`, or a day back.
    pub since: Timestamp,
    pub tally: Tally,
}

/// When the owner last marked a recap seen.
pub fn seen_at(conn: &Connection) -> Result<Option<Timestamp>> {
    Ok(conn
        .query_row(
            "SELECT at FROM events WHERE kind = 'recap.seen' ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?)
}

/// Counts what happened after `since`, from the same event log the recap
/// lines come from.
pub fn tally(conn: &Connection, since: Timestamp) -> Result<Tally> {
    let count =
        |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, params![since], |r| r.get(0))?) };
    Ok(Tally {
        landed: count(
            "SELECT count(*) FROM events WHERE at > ?1 AND kind = 'card.closed'
             AND json_extract(payload, '$.outcome') = 'landed'",
        )?,
        decisions: count(
            "SELECT count(*) FROM events WHERE at > ?1 AND kind = 'attention.opened'
             AND json_extract(payload, '$.kind') = 'decision'",
        )?,
        failed_runs: count(
            "SELECT count(*) FROM events WHERE at > ?1 AND kind = 'run.ended'
             AND json_extract(payload, '$.outcome') = 'failed'",
        )?,
        suggestions: count(
            "SELECT count(*) FROM events WHERE at > ?1 AND kind = 'proposal.created'",
        )?,
    })
}

/// The recap as of `now`.
pub fn recap(conn: &Connection, now: Timestamp) -> Result<Recap> {
    let seen_at = seen_at(conn)?;
    let since = seen_at.unwrap_or_else(|| now.plus_minutes(-DEFAULT_WINDOW_MINUTES));
    Ok(Recap {
        seen_at,
        since,
        tally: tally(conn, since)?,
    })
}

impl Store {
    /// `recap.mark_seen` (the board's Mark seen): moves the owner's mark to
    /// now, so the next recap counts from here. Owner only. Each call writes
    /// one event, so the mark is the newest `recap.seen`.
    pub fn recap_mark_seen(&mut self, ctx: &Ctx) -> Result<Applied<Recap>> {
        require_owner(&ctx.actor, "recap.mark_seen")?;
        self.write(ctx, "recap.mark_seen", None, |tx| {
            let now = tx.now();
            tx.event(
                NewEvent::new("recap", "owner", "recap.seen").payload(json!({"seen_at": now})),
            )?;
            recap(tx.conn(), now)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::memory_store;
    use crate::store::Actor;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn event(store: &Store, id: &str, when: &str, kind: &str, payload: &str) {
        store
            .conn()
            .execute(
                "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, payload)
                 VALUES (?1, ?2, 'test', 'daemon', 'thing', ?1, ?3, ?4)",
                params![id, when, kind, payload],
            )
            .unwrap();
    }

    fn fill(store: &Store) {
        event(
            store,
            "e1",
            "2026-11-02T10:00:00.000Z",
            "card.closed",
            r#"{"outcome":"landed"}"#,
        );
        event(
            store,
            "e2",
            "2026-11-02T10:01:00.000Z",
            "card.closed",
            r#"{"outcome":"dropped"}"#,
        );
        event(
            store,
            "e3",
            "2026-11-02T10:02:00.000Z",
            "attention.opened",
            r#"{"kind":"decision"}"#,
        );
        event(
            store,
            "e4",
            "2026-11-02T10:03:00.000Z",
            "attention.opened",
            r#"{"kind":"capacity"}"#,
        );
        event(
            store,
            "e5",
            "2026-11-02T10:04:00.000Z",
            "run.ended",
            r#"{"outcome":"failed"}"#,
        );
        event(
            store,
            "e6",
            "2026-11-02T10:05:00.000Z",
            "run.ended",
            r#"{"outcome":"done"}"#,
        );
        event(
            store,
            "e7",
            "2026-11-02T10:06:00.000Z",
            "proposal.created",
            "{}",
        );
        event(
            store,
            "e8",
            "2026-11-01T10:00:00.000Z",
            "proposal.created",
            "{}",
        );
    }

    #[test]
    fn the_tally_counts_each_kind_after_the_window_start() {
        let (store, _) = memory_store();
        fill(&store);
        let tally = tally(store.conn(), at("2026-11-02T00:00:00Z")).unwrap();
        assert_eq!(
            tally,
            Tally {
                landed: 1,
                decisions: 1,
                failed_runs: 1,
                suggestions: 1
            }
        );
        assert!(!tally.is_quiet());
        assert!(
            super::tally(store.conn(), at("2026-11-03T00:00:00Z"))
                .unwrap()
                .is_quiet()
        );
    }

    #[test]
    fn without_a_mark_the_window_is_a_day_back() {
        let (store, _) = memory_store();
        let now = at("2026-11-02T12:00:00Z");
        let recap = recap(store.conn(), now).unwrap();
        assert_eq!(recap.seen_at, None);
        assert_eq!(recap.since, now.plus_minutes(-DEFAULT_WINDOW_MINUTES));
    }

    #[test]
    fn marking_seen_moves_the_window_to_now_and_the_newest_mark_wins() {
        let (mut store, _) = memory_store();
        fill(&store);
        let first = store
            .recap_mark_seen(&Ctx::new(Actor::Board))
            .unwrap()
            .value;
        assert_eq!(first.seen_at, Some(first.since));
        assert!(first.tally.is_quiet());
        let second = store
            .recap_mark_seen(&Ctx::new(Actor::Owner))
            .unwrap()
            .value;
        assert!(second.since >= first.since);
        assert_eq!(seen_at(store.conn()).unwrap(), second.seen_at);
    }

    #[test]
    fn only_the_owner_marks_a_recap_seen() {
        let (mut store, session) = memory_store();
        for actor in [Actor::Session(session), Actor::Daemon, Actor::Hook] {
            let refused = store.recap_mark_seen(&Ctx::new(actor)).unwrap_err();
            assert!(
                matches!(refused, crate::error::Error::Forbidden(_)),
                "{refused}"
            );
        }
        assert_eq!(seen_at(store.conn()).unwrap(), None);
    }

    #[test]
    fn events_since_lists_newest_first_and_honours_the_limit() {
        let (store, _) = memory_store();
        fill(&store);
        let all = events_since(store.conn(), at("2026-11-02T00:00:00Z"), None, 50).unwrap();
        // The store's own setup events are newer; only the filled ones are checked.
        let kinds: Vec<_> = all
            .iter()
            .filter(|e| e.id.starts_with('e') && e.id.len() == 2)
            .map(|e| e.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                "run.ended",
                "run.ended",
                "attention.opened",
                "attention.opened",
                "card.closed",
                "card.closed"
            ]
        );
        let two = events_since(store.conn(), at("2026-11-02T00:00:00Z"), None, 2).unwrap();
        assert_eq!(two.len(), 2);
        let none =
            events_since(store.conn(), at("2026-11-02T00:00:00Z"), Some("nope"), 50).unwrap();
        assert!(none.is_empty());
    }
}
