//! `mem.outside_edit`: the record that the memory repo changed outside
//! Tiller (`memory.md` section 12, U16). An editor's save, a `git pull` or a
//! commit made by hand moves no row, so without it an open board would show
//! the old text until a reload. The daemon's watcher writes it; the event is
//! the board's cue to read the memory documents again.

use rusqlite::OptionalExtension;
use serde_json::json;

use super::projects::require_daemon;
use crate::error::Result;
use crate::store::{Applied, Ctx, NewEvent, Store};

/// The newest commit Tiller made to the memory repo: the `seq` of its
/// `memory_revision.recorded` event and the commit it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TillerCommit {
    pub seq: i64,
    pub commit: String,
}

impl Store {
    /// `mem.outside_edit` (daemon): writes `memory_repo.changed` with the
    /// repo's `HEAD` (null when it has none) and the documents whose text
    /// changed. Each call is one change; the watcher coalesces a burst.
    pub fn mem_outside_edit(
        &mut self,
        ctx: &Ctx,
        head: Option<&str>,
        files: &[String],
    ) -> Result<Applied<()>> {
        require_daemon(&ctx.actor, "mem.outside_edit")?;
        self.write(ctx, "mem.outside_edit", None, |tx| {
            tx.event(
                NewEvent::new("memory_repo", "memory", "memory_repo.changed")
                    .payload(json!({"head": head, "files": files})),
            )?;
            Ok(())
        })
    }

    /// The newest commit Tiller made to the memory repo, so the watcher can
    /// tell Tiller's own write from an outside one.
    pub fn mem_last_commit(&self) -> Result<Option<TillerCommit>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT seq, json_extract(payload, '$.commit_sha') FROM events
                 WHERE kind = 'memory_revision.recorded'
                   AND json_extract(payload, '$.commit_sha') IS NOT NULL
                 ORDER BY seq DESC LIMIT 1",
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
            .mem_outside_edit(&Ctx::new(Actor::Daemon), Some("abc123"), &files)
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

        let refused = store.mem_outside_edit(&Ctx::new(Actor::Owner), None, &[]);
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
        record(
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
        assert_eq!(
            store.mem_last_commit().unwrap(),
            Some(TillerCommit {
                seq: second,
                commit: "c2".into()
            })
        );
    }
}
