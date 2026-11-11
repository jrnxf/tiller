//! `mem.outside_edit`: the record that the memory repo changed outside
//! Tiller (`memory.md` section 12, U16). An editor's save, a `git pull` or a
//! commit made by hand moves no row, so without it an open board would show
//! the old text until a reload. The daemon's watcher writes it; the event is
//! the board's cue to read the memory documents again.
//!
//! The watcher's last look is kept in `meta.memory_watch`, so a daemon that
//! starts again compares its first look with the last one before it
//! stopped, and an edit made while it was down is a change too. The value
//! is the watcher's own JSON; the store only keeps it. It is rebuildable:
//! without it the first look is the baseline.

use std::collections::HashSet;

use rusqlite::OptionalExtension;
use serde_json::json;

use super::projects::{meta, require_daemon};
use crate::error::Result;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};

/// The events that record a commit Tiller made to the memory repo, as a
/// literal so each query that reads them stays one `&'static str`.
macro_rules! tiller_commits {
    () => {
        " FROM events
         WHERE kind = 'memory_revision.recorded'
           AND json_extract(payload, '$.provenance') = 'tiller'
           AND json_extract(payload, '$.commit_sha') IS NOT NULL"
    };
}

/// The `meta` key that holds the watcher's last look.
const WATCH: &str = "memory_watch";

/// The newest commit Tiller made to the memory repo: the `seq` of its
/// `memory_revision.recorded` event and the commit it names.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TillerCommit {
    pub seq: i64,
    pub commit: String,
}

impl Store {
    /// `mem.outside_edit` (daemon): writes `memory_repo.changed` with the
    /// repo's `HEAD` (null when it has none) and the documents whose text
    /// changed, and keeps `seen`, the look that found the change, as the
    /// watcher's last look. Each call is one change; the watcher coalesces a
    /// burst.
    pub fn mem_outside_edit(
        &mut self,
        ctx: &Ctx,
        head: Option<&str>,
        files: &[String],
        seen: &str,
    ) -> Result<Applied<()>> {
        require_daemon(&ctx.actor, "mem.outside_edit")?;
        self.write(ctx, "mem.outside_edit", None, |tx| {
            keep_seen(tx, seen)?;
            tx.event(
                NewEvent::new("memory_repo", "memory", "memory_repo.changed")
                    .payload(json!({"head": head, "files": files})),
            )?;
            Ok(())
        })
    }

    /// `mem.watch` (daemon): keeps the watcher's last look when it moved
    /// with no outside change (the first look, Tiller's own commit). It is
    /// rebuildable bookkeeping, so it writes no event.
    pub fn mem_watch(&mut self, ctx: &Ctx, seen: &str) -> Result<Applied<()>> {
        require_daemon(&ctx.actor, "mem.watch")?;
        self.write(ctx, "mem.watch", None, |tx| {
            tx.rebuildable();
            keep_seen(tx, seen)
        })
    }

    /// The watcher's last look, as it was kept; `None` before the first.
    pub fn mem_watch_seen(&self) -> Result<Option<String>> {
        meta(self.conn(), WATCH)
    }

    /// The newest commit Tiller made to the memory repo, so the watcher can
    /// tell Tiller's own write from an outside one.
    pub fn mem_last_commit(&self) -> Result<Option<TillerCommit>> {
        Ok(self
            .conn()
            .query_row(
                concat!(
                    "SELECT seq, json_extract(payload, '$.commit_sha')",
                    tiller_commits!(),
                    " ORDER BY seq DESC LIMIT 1"
                ),
                [],
                |row| {
                    Ok(TillerCommit {
                        seq: row.get(0)?,
                        commit: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// The commits of the Tiller revisions recorded after `seq` (all of them
    /// when `seq` is `None`): the commits the watcher may read as Tiller's
    /// own since its last look.
    pub fn mem_commits_since(&self, seq: Option<i64>) -> Result<HashSet<String>> {
        let mut statement = self.conn().prepare_cached(concat!(
            "SELECT json_extract(payload, '$.commit_sha')",
            tiller_commits!(),
            " AND seq > ?1"
        ))?;
        let commits = statement
            .query_map([seq.unwrap_or(0)], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        Ok(commits)
    }
}

fn keep_seen(tx: &mut Tx<'_>, seen: &str) -> Result<()> {
    tx.conn().execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [WATCH, seen],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error;
    use crate::store::Actor;

    #[test]
    fn an_outside_edit_is_one_daemon_event() {
        let mut store = Store::open_in_memory().unwrap();
        let files = vec!["fleet.md".to_owned()];
        let applied = store
            .mem_outside_edit(&Ctx::new(Actor::Daemon), Some("abc123"), &files, "{}")
            .unwrap();
        assert_eq!(applied.events.len(), 1);
        let (entity, kind, payload): (String, String, String) = store
            .conn()
            .query_row(
                "SELECT entity, kind, payload FROM events WHERE seq = ?1",
                [applied.events[0]],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(entity, "memory_repo");
        assert_eq!(kind, "memory_repo.changed");
        let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload, json!({"head": "abc123", "files": ["fleet.md"]}));

        assert_eq!(store.mem_watch_seen().unwrap().as_deref(), Some("{}"));

        let refused = store.mem_outside_edit(&Ctx::new(Actor::Owner), None, &[], "{}");
        assert!(matches!(refused, Err(Error::Forbidden(_))), "{refused:?}");
    }

    #[test]
    fn the_watchers_last_look_is_kept_without_an_event() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.mem_watch_seen().unwrap(), None);
        let ctx = Ctx::new(Actor::Daemon);
        let applied = store.mem_watch(&ctx, r#"{"head":"c1"}"#).unwrap();
        assert!(applied.events.is_empty());
        store.mem_watch(&ctx, r#"{"head":"c2"}"#).unwrap();
        assert_eq!(
            store.mem_watch_seen().unwrap().as_deref(),
            Some(r#"{"head":"c2"}"#)
        );
        let events: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(events, 0);
        let refused = store.mem_watch(&Ctx::new(Actor::Owner), "{}");
        assert!(matches!(refused, Err(Error::Forbidden(_))), "{refused:?}");
    }

    #[test]
    fn the_last_commit_is_the_newest_tiller_revision() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.mem_last_commit().unwrap(), None);
        let ctx = Ctx::new(Actor::Daemon);
        let record = |store: &mut Store, payload: serde_json::Value| {
            store
                .write(&ctx, "test.record", None, |tx| {
                    tx.event(
                        NewEvent::new("memory_revision", "r", "memory_revision.recorded")
                            .payload(payload),
                    )
                })
                .unwrap()
                .value
        };
        let first = record(
            &mut store,
            json!({"commit_sha": "c1", "provenance": "tiller"}),
        );
        let second = record(
            &mut store,
            json!({"commit_sha": "c2", "provenance": "tiller"}),
        );
        // A revision first seen outside Tiller names no commit.
        record(
            &mut store,
            json!({"blob_sha": "b3", "provenance": "external"}),
        );
        // One that names a commit with another provenance is not Tiller's
        // commit either.
        record(
            &mut store,
            json!({"commit_sha": "c4", "provenance": "external"}),
        );
        assert_eq!(
            store.mem_last_commit().unwrap(),
            Some(TillerCommit {
                seq: second,
                commit: "c2".into()
            })
        );
        let since = |seq| {
            let mut commits: Vec<String> =
                store.mem_commits_since(seq).unwrap().into_iter().collect();
            commits.sort();
            commits
        };
        assert_eq!(since(None), ["c1", "c2"]);
        assert_eq!(since(Some(first)), ["c2"]);
        assert_eq!(since(Some(second)), Vec::<String>::new());
    }
}
