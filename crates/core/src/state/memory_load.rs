//! What a launch loads and the record of it (`memory.md` sections 2 and 7):
//! the approved Tiller documents a session of a role in a project gets, and
//! `session_loads`, the rows `mem.load` writes for each launch and copies on
//! a resume, `/clear` or `/compact`.
//!
//! Nothing here reads a file. The launcher reads each blob from the memory
//! repo by its SHA and renders the prompt; this module only picks the
//! revisions and records which ones a session got.

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::get;
use super::memory::host_boundary;
use crate::domain::{Run, Session};
use crate::error::{Error, Result};
use crate::ids::FLEET_PROJECT;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// One approved revision a launch renders: the current approved revision
/// of one live Tiller document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loadable {
    pub source_id: String,
    pub revision_id: String,
    pub project_id: String,
    /// The repo as `memory_sources` records it (`~/.tiller/memory`).
    pub canonical_repo: String,
    /// The document's path in that repo, such as `projects/alpha.md`.
    pub canonical_path: String,
    pub blob_sha: String,
}

/// What `mem.load` recorded for one session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loaded {
    pub session_id: String,
    pub trigger: String,
    pub loaded_at: Timestamp,
    /// The revisions, in the order they were rendered or copied.
    pub revision_ids: Vec<String>,
}

/// What one [`Store::mem_load_owed`] pass did.
#[derive(Debug)]
pub struct CopyPass {
    /// The copies made, in observation order.
    pub made: Vec<Loaded>,
    /// The copies that failed, in observation order. The pass went on past
    /// each one.
    pub failed: Vec<FailedCopy>,
    /// The `since` of the next pass: the pass's time (`ctx.now`), or the
    /// `recorded_at` of the earliest observation whose copy failed, when
    /// that is earlier. Every observation committed after the pass read is
    /// stamped later than the pass's time, less [`MARK_SLACK_MINUTES`], and
    /// a failed copy is read, and tried, again on the next pass.
    pub mark: Timestamp,
}

/// One copy a pass could not write.
#[derive(Debug)]
pub struct FailedCopy {
    pub session_id: String,
    pub trigger: String,
    pub observed_at: Timestamp,
    pub error: Error,
}

/// The `session_loads.trigger` values a `SessionStart` observation copies
/// the newest set under. The launch writes `startup` itself.
const COPY_TRIGGERS: &[&str] = &["resume", "clear", "compact"];

/// How long after its session ended a `SessionStart` observation still
/// gets its copy: the daemon may have been down when it came in.
const COPY_WINDOW_MINUTES: i64 = 60;

/// How far before the last pass's mark a pass still reads. An observation
/// is stamped (`recorded_at`, its ctx time) before its transaction takes
/// the write lock, so one stamped before the mark can commit after the
/// pass read.
const MARK_SLACK_MINUTES: i64 = 5;

/// The floor of a pass with no mark: before every observation.
const EPOCH: Timestamp = Timestamp::UNIX_EPOCH;

impl Store {
    /// The approved revisions a launch of `role` in `project` renders, in
    /// prompt order: the `fleet` document first, then the project's. A
    /// source loads when it is a live Tiller document of this host's
    /// boundary, its `roles` name the role, and it has an approved
    /// revision; the current one is the approved revision recorded last (an
    /// undo records its earlier blob again). A source with no approved
    /// revision yet loads nothing: its document has no approved text. A
    /// state.db with no live Tiller document loads nothing either, so only
    /// a launch with memory to load needs `meta.boundary`.
    pub fn mem_loadable(&self, project: &str, role: &str) -> Result<Vec<Loadable>> {
        let any: bool = self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM memory_sources
                            WHERE kind = 'tiller_doc' AND retired_at IS NULL)",
            [],
            |row| row.get(0),
        )?;
        if !any {
            return Ok(Vec::new());
        }
        let boundary = host_boundary(self.conn())?;
        let mut stmt = self.conn().prepare(
            "SELECT s.id, r.id, s.project_id, s.canonical_repo, s.canonical_path, r.blob_sha
             FROM memory_sources s
             JOIN memory_revisions r ON r.id = (
                 SELECT id FROM memory_revisions
                 WHERE source_id = s.id AND review = 'approved'
                 ORDER BY recorded_at DESC, id DESC LIMIT 1)
             WHERE s.kind = 'tiller_doc' AND s.retired_at IS NULL
               AND s.boundary = ?3
               AND s.project_id IN (?1, ?2)
               AND EXISTS (SELECT 1 FROM json_each(s.roles) WHERE value = ?4)
               AND s.canonical_repo IS NOT NULL AND s.canonical_path IS NOT NULL
             ORDER BY s.project_id != ?1, s.canonical_path, s.id",
        )?;
        let rows = stmt.query_map(params![FLEET_PROJECT, project, boundary, role], |row| {
            Ok(Loadable {
                source_id: row.get(0)?,
                revision_id: row.get(1)?,
                project_id: row.get(2)?,
                canonical_repo: row.get(3)?,
                canonical_path: row.get(4)?,
                blob_sha: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `mem.load` (the launcher, once a launch's line went in): one
    /// `startup` row per revision it rendered for the session, at this
    /// moment. A launch that rendered nothing writes nothing and opens no
    /// transaction.
    pub fn mem_load(
        &mut self,
        ctx: &Ctx,
        session: &str,
        revisions: &[String],
    ) -> Result<Applied<Loaded>> {
        if revisions.is_empty() {
            return Ok(nothing(session, "startup", ctx.now));
        }
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let at = tx.now();
            insert(tx, &session, "startup", at, revisions)
        })
    }

    /// `mem.load` on a resume, `/clear` or `/compact` (the daemon, on the
    /// `SessionStart` observation, `protocol.md` 4.2): copies the session's
    /// newest set with the new trigger at `at`. It renders and hashes
    /// nothing, since the appended prompt is the one the session launched
    /// with. A session that loaded nothing writes nothing and opens no
    /// transaction; a copy already recorded at `at` writes nothing either.
    pub fn mem_load_again(
        &mut self,
        ctx: &Ctx,
        session: &str,
        trigger: &str,
        at: Timestamp,
    ) -> Result<Applied<Loaded>> {
        if !COPY_TRIGGERS.contains(&trigger) {
            return Err(Error::Usage(format!(
                "a copied load is for resume, clear or compact, not {trigger}"
            )));
        }
        if newest_set_in(self.conn(), session)?.is_empty() {
            return Ok(nothing(session, trigger, at));
        }
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let revisions = newest_set_in(tx.conn(), &session.id)?;
            insert(tx, &session, trigger, at, &revisions)
        })
    }

    /// The daemon's copy pass: makes each copy a `SessionStart` observation
    /// for resume, clear or compact owes. One is owed when the hook
    /// recorded it for a session that loaded memory, live or ended less
    /// than [`COPY_WINDOW_MINUTES`] ago, the hook bound the session (no
    /// `bind_refused` in its payload), and no row of its trigger is
    /// recorded at its time.
    ///
    /// `since` is the [`CopyPass::mark`] of the caller's last pass: only
    /// observations recorded after it, less [`MARK_SLACK_MINUTES`], are
    /// read, so a pass does not grow with a long session's history. `None`
    /// reads every observation, as the first pass after a start does. A
    /// copy that fails is returned in [`CopyPass::failed`], the pass goes
    /// on to the next one, and the mark stays at or before it, so every
    /// later pass tries it again until it is made or its session's window
    /// closes.
    pub fn mem_load_owed(&mut self, ctx: &Ctx, since: Option<Timestamp>) -> Result<CopyPass> {
        // With no mark every observation is read, so the planner, not the
        // `recorded_at` index, picks the scan.
        let (index, floor) = match since {
            Some(mark) => (
                "INDEXED BY observations_recorded",
                mark.plus_minutes(-MARK_SLACK_MINUTES),
            ),
            None => ("", EPOCH),
        };
        let events = COPY_TRIGGERS
            .iter()
            .map(|trigger| format!("'SessionStart:{trigger}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let owed: Vec<(String, String, Timestamp, Timestamp)> = {
            let mut stmt = self.conn().prepare_cached(&format!(
                "SELECT o.session_id, substr(o.event, length('SessionStart:') + 1), o.observed_at,
                        o.recorded_at
                 FROM observations o {index}
                 JOIN sessions s ON s.id = o.session_id
                 WHERE o.recorded_at > ?2
                   AND (s.ended_at IS NULL OR s.ended_at > ?1)
                   AND EXISTS (SELECT 1 FROM session_loads l WHERE l.session_id = s.id)
                   AND o.source = 'hook'
                   AND o.event IN ({events})
                   AND json_extract(o.payload, '$.bind_refused') IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM session_loads l
                       WHERE l.session_id = o.session_id AND l.loaded_at = o.observed_at
                         AND l.trigger = substr(o.event, length('SessionStart:') + 1))
                 ORDER BY o.observed_at, o.id"
            ))?;
            let rows = stmt.query_map(
                params![ctx.now.plus_minutes(-COPY_WINDOW_MINUTES), floor],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut pass = CopyPass {
            made: Vec::new(),
            failed: Vec::new(),
            mark: ctx.now,
        };
        for (session, trigger, at, recorded_at) in owed {
            match self.mem_load_again(ctx, &session, &trigger, at) {
                Ok(copied) if copied.value.revision_ids.is_empty() => {}
                Ok(copied) => pass.made.push(copied.value),
                Err(error) => {
                    // The next floor is the mark less the slack, so this
                    // observation is read again.
                    pass.mark = pass.mark.min(recorded_at);
                    pass.failed.push(FailedCopy {
                        session_id: session,
                        trigger,
                        observed_at: at,
                        error,
                    });
                }
            }
        }
        Ok(pass)
    }

    /// The session's newest load set: the revisions of its latest
    /// `session_loads` moment.
    pub fn mem_loaded(&self, session: &str) -> Result<Vec<String>> {
        newest_set_in(self.conn(), session)
    }
}

/// A load that wrote nothing, with no transaction opened.
fn nothing(session: &str, trigger: &str, at: Timestamp) -> Applied<Loaded> {
    Applied {
        value: Loaded {
            session_id: session.to_owned(),
            trigger: trigger.to_owned(),
            loaded_at: at,
            revision_ids: Vec::new(),
        },
        events: Vec::new(),
        replayed: false,
    }
}

/// The revisions of the session's latest `session_loads` moment, each once
/// in the order first written: two triggers can record the same set in
/// one millisecond.
fn newest_set_in(conn: &rusqlite::Connection, session: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT revision_id FROM session_loads
         WHERE session_id = ?1
           AND loaded_at = (SELECT max(loaded_at) FROM session_loads WHERE session_id = ?1)
         GROUP BY revision_id
         ORDER BY min(rowid)",
    )?;
    let rows = stmt.query_map([session], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn insert(
    tx: &mut Tx<'_>,
    session: &Session,
    trigger: &str,
    at: Timestamp,
    revisions: &[String],
) -> Result<Loaded> {
    let mut written = Vec::new();
    for revision in revisions {
        let rows = tx.conn().execute(
            "INSERT OR IGNORE INTO session_loads (session_id, revision_id, trigger, loaded_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session.id, revision, trigger, at],
        )?;
        if rows > 0 {
            written.push(revision.clone());
        }
    }
    if !written.is_empty() {
        let card = match &session.run_id {
            Some(run) => Some(get::<Run>(tx.conn(), "run", run)?.card_id),
            None => None,
        };
        tx.event(
            NewEvent::new("session", &session.id, "session.memory_loaded")
                .card_opt(card)
                .payload(json!({"trigger": trigger, "revision_ids": written})),
        )?;
    }
    Ok(Loaded {
        session_id: session.id.clone(),
        trigger: trigger.to_owned(),
        loaded_at: at,
        revision_ids: written,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::domain::ObservationSource;
    use crate::state::runs::NewObservation;
    use crate::state::tests::memory_store;
    use crate::store::Actor;

    /// One approved alpha document, revision `R-alpha`.
    fn document(store: &Store) {
        store
            .conn()
            .execute_batch(
                r#"INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path,
                       canonical_repo, canonical_path, apply_method, load_mode, roles, added_at)
                   VALUES ('S-alpha', 'alpha', 'personal', 'tiller_doc',
                       '~/.tiller/memory/projects/alpha.md', '~/.tiller/memory',
                       'projects/alpha.md', 'tiller_commit', 'rendered', '["orchestrator"]',
                       '2026-11-02T09:00:00.000Z');
                   INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance,
                       scrub, recorded_at, recorded_by)
                   VALUES ('R-alpha', 'S-alpha', 'abc', 'approved', 'baseline', 'passed',
                       '2026-11-02T09:00:00.000Z', 'daemon');"#,
            )
            .unwrap();
    }

    fn observe(store: &mut Store, session: &str, event: &str, at: Timestamp) {
        observe_with(store, session, event, at, json!({}));
    }

    fn observe_with(store: &mut Store, session: &str, event: &str, at: Timestamp, payload: Value) {
        store
            .observation_record(
                &Ctx::new(Actor::Hook),
                NewObservation {
                    session_id: Some(session.into()),
                    run_id: None,
                    source: ObservationSource::Hook,
                    event: event.into(),
                    verdict: None,
                    observed_at: at,
                    payload,
                },
            )
            .unwrap();
    }

    /// The copies one pass with no mark makes.
    fn owed(store: &mut Store, ctx: &Ctx) -> Vec<Loaded> {
        let pass = store.mem_load_owed(ctx, None).unwrap();
        assert!(pass.failed.is_empty(), "{:?}", pass.failed);
        pass.made
    }

    fn triggers(store: &Store, session: &str) -> Vec<String> {
        store
            .conn()
            .prepare(
                "SELECT trigger FROM session_loads WHERE session_id = ?1 ORDER BY loaded_at, rowid",
            )
            .unwrap()
            .query_map([session], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// A home with no `meta.boundary` launches while it has no memory to
    /// load; a Tiller document makes the boundary needed again.
    #[test]
    fn a_home_with_no_boundary_launches_while_it_has_no_memory() {
        let (store, _) = memory_store();
        store
            .conn()
            .execute("DELETE FROM meta WHERE key = 'boundary'", [])
            .unwrap();
        assert_eq!(store.mem_loadable("alpha", "builder").unwrap(), Vec::new());
        document(&store);
        let error = store.mem_loadable("alpha", "orchestrator").unwrap_err();
        assert!(error.to_string().contains("no boundary"), "{error}");
    }

    /// The daemon's pass copies the set once per owed observation, in
    /// order, and a second pass owes nothing. Startup, fork and a session
    /// that loaded nothing owe no copy.
    #[test]
    fn the_copy_pass_makes_each_owed_copy_once() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()])
            .unwrap();
        let start = Timestamp::now();
        for (n, event) in [
            "SessionStart:startup",
            "SessionStart:clear",
            "SessionStart:fork",
            "SessionStart:compact",
            "SessionStart:resume",
        ]
        .into_iter()
        .enumerate()
        {
            let minutes = i64::try_from(n).unwrap() + 1;
            observe(&mut store, &session, event, start.plus_minutes(minutes));
        }
        let made = owed(&mut store, &daemon);
        assert_eq!(
            made.iter().map(|l| l.trigger.as_str()).collect::<Vec<_>>(),
            ["clear", "compact", "resume"]
        );
        assert_eq!(
            triggers(&store, &session),
            ["startup", "clear", "compact", "resume"]
        );
        assert!(owed(&mut store, &daemon).is_empty());
        assert_eq!(triggers(&store, &session).len(), 4);
    }

    /// A `SessionStart` whose bind the hook refused names a Claude process
    /// Tiller did not bind, so it owes no copy (`memory.md` section 7).
    #[test]
    fn a_refused_bind_owes_no_copy() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()])
            .unwrap();
        let at = Timestamp::now().plus_minutes(1);
        observe_with(
            &mut store,
            &session,
            "SessionStart:clear",
            at,
            json!({"bind_refused": "the session has ended"}),
        );
        assert!(owed(&mut store, &daemon).is_empty());
        assert_eq!(triggers(&store, &session), ["startup"]);
    }

    /// A `/clear` in the same millisecond as the launch's record still gets
    /// its own `clear` rows, and is not owed again.
    #[test]
    fn a_copy_in_the_launch_millisecond_is_recorded_once() {
        let (mut store, session) = memory_store();
        document(&store);
        let at = Timestamp::now();
        let daemon = Ctx::at(Actor::Daemon, at);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()])
            .unwrap();
        observe(&mut store, &session, "SessionStart:clear", at);
        let made = owed(&mut store, &daemon);
        assert_eq!(made.len(), 1);
        assert_eq!(made[0].revision_ids, ["R-alpha"]);
        assert_eq!(triggers(&store, &session), ["startup", "clear"]);
        assert_eq!(store.mem_loaded(&session).unwrap(), ["R-alpha"]);
        assert!(owed(&mut store, &daemon).is_empty());
    }

    /// A copy that fails is reported, and the copies after it are still
    /// made.
    #[test]
    fn a_failing_copy_does_not_block_the_others() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()])
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_loads
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        let start = Timestamp::now();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            start.plus_minutes(1),
        );
        observe(
            &mut store,
            &session,
            "SessionStart:compact",
            start.plus_minutes(2),
        );
        let pass = store.mem_load_owed(&daemon, None).unwrap();
        assert_eq!(
            pass.made
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["compact"]
        );
        assert_eq!(pass.failed.len(), 1);
        assert_eq!(pass.failed[0].trigger, "clear");
        assert!(
            pass.failed[0].error.to_string().contains("no clear copies"),
            "{}",
            pass.failed[0].error
        );
        assert_eq!(triggers(&store, &session), ["startup", "compact"]);
    }

    /// A pass given the last pass's mark reads only the observations
    /// recorded since, less the slack; one with no mark reads them all.
    #[test]
    fn a_marked_pass_reads_only_new_observations() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()])
            .unwrap();
        let mark = store.mem_load_owed(&daemon, None).unwrap().mark;
        assert_eq!(mark, daemon.now);
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            mark.plus_minutes(1),
        );
        observe(
            &mut store,
            &session,
            "SessionStart:compact",
            mark.plus_minutes(2),
        );
        let old = mark.plus_minutes(-MARK_SLACK_MINUTES - 1);
        store
            .conn()
            .execute(
                "UPDATE observations SET recorded_at = ?1 WHERE event = 'SessionStart:clear'",
                [old],
            )
            .unwrap();
        let later = Ctx::at(Actor::Daemon, mark.plus_minutes(3));
        let pass = store.mem_load_owed(&later, Some(mark)).unwrap();
        assert_eq!(
            pass.made
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["compact"]
        );
        assert_eq!(pass.mark, later.now);
        assert_eq!(
            owed(&mut store, &later)
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["clear"]
        );
    }

    /// A failed copy holds the mark at its observation, so a pass long
    /// after it still reads it and makes the copy once the write goes in.
    #[test]
    fn a_failed_copy_is_tried_again_on_every_later_pass() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()])
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_loads
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let first = store.mem_load_owed(&daemon, None).unwrap();
        assert_eq!(first.failed.len(), 1);
        let later = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(10));
        let second = store.mem_load_owed(&later, Some(first.mark)).unwrap();
        assert_eq!(second.failed.len(), 1, "the failed copy is read again");
        assert!(second.mark < later.now, "{:?}", second.mark);
        store
            .conn()
            .execute_batch("DROP TRIGGER no_clear;")
            .unwrap();
        let last = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(20));
        let third = store.mem_load_owed(&last, Some(second.mark)).unwrap();
        assert!(third.failed.is_empty(), "{:?}", third.failed);
        assert_eq!(
            third
                .made
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["clear"]
        );
        assert_eq!(third.mark, last.now);
    }
}
