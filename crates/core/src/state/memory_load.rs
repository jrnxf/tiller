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
use crate::import::EMPTY_BLOB;
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

/// A Tiller document a launch went on without (`memory.md` section 9, step
/// 4): it would load for the session's role and project, but has no
/// approved text to render.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub source_id: String,
    pub project_id: String,
    /// The document's path in the memory repo, such as `projects/alpha.md`.
    pub path: String,
    pub reason: String,
}

/// Why a launch skips a document: the one reason a launch skips one today.
pub const NO_APPROVED_REVISION: &str = "it has no approved revision";

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
    /// The `since` of the next pass.
    pub mark: CopyMark,
}

/// Where the next [`Store::mem_load_owed`] pass reads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyMark {
    /// The pass's time (`ctx.now`), or the `recorded_at` of the earliest
    /// observation whose copy failed and is not [`FailedCopy::given_up`],
    /// when that is earlier. Every observation committed after the pass
    /// read is stamped later than the pass's time, less
    /// [`MARK_SLACK_MINUTES`], and a failed copy is read, and tried, again
    /// on each pass until it is given up.
    pub read_from: Timestamp,
    /// The pass's time. Every copy the pass read past its window was made
    /// or given up then, so the next pass does not read it again.
    pub passed_at: Timestamp,
}

/// One copy a pass could not write.
#[derive(Debug)]
pub struct FailedCopy {
    pub session_id: String,
    pub trigger: String,
    pub observed_at: Timestamp,
    pub error: Error,
    /// This was the copy's last try: its observation was recorded, or its
    /// session ended, [`COPY_WINDOW_MINUTES`] or more before the pass, so
    /// no later pass reads it again, and `session_loads` keeps no row for
    /// it.
    pub given_up: bool,
}

/// The `session_loads.trigger` values a `SessionStart` observation copies
/// the newest set under. The launch writes `startup` itself.
const COPY_TRIGGERS: &[&str] = &["resume", "clear", "compact"];

/// How long after its session ended a `SessionStart` observation still
/// gets its copy: the daemon may have been down when it came in. A failed
/// copy is tried again for as long after its observation was recorded.
const COPY_WINDOW_MINUTES: i64 = 60;

/// How far before the last pass's mark a pass still reads. An observation
/// is stamped (`recorded_at`, its ctx time) before its transaction takes
/// the write lock, so one stamped before the mark can commit after the
/// pass read.
const MARK_SLACK_MINUTES: i64 = 5;

/// One copy [`Store::owed_copies`] found: session, trigger, `observed_at`,
/// `recorded_at` and the session's `ended_at`.
type OwedCopy = (String, String, Timestamp, Timestamp, Option<Timestamp>);

/// The floor of a pass with no mark: before every observation.
const EPOCH: Timestamp = Timestamp::UNIX_EPOCH;

impl Store {
    /// The approved revisions a launch of `role` in `project` renders, in
    /// prompt order: the `fleet` document first, then the project's. A
    /// source loads when it is a live Tiller document of this host's
    /// boundary, its `roles` name the role, and it has an approved
    /// revision; the current one is the approved revision recorded last (an
    /// undo records its earlier blob again). A source with no approved
    /// revision yet loads nothing: the launch goes on without it, and
    /// [`Self::mem_skipped`] names it. A state.db with no live Tiller
    /// document loads nothing either, so only a launch with memory to load
    /// needs `meta.boundary`.
    pub fn mem_loadable(&self, project: &str, role: &str) -> Result<Vec<Loadable>> {
        loadable(self.conn(), project, role)
    }

    /// The documents a launch of `role` in `project` skips, in prompt
    /// order: each live Tiller document that would load (as
    /// [`Self::mem_loadable`] picks them) but has no approved revision. One
    /// whose newest revision is the empty file is not skipped, since it has
    /// no text to load.
    pub fn mem_skipped(&self, project: &str, role: &str) -> Result<Vec<Skipped>> {
        skipped(self.conn(), Some((project, role)))
    }

    /// Every live Tiller document of this host's boundary that launches
    /// skip, for any role or project, fleet first: the Health warning.
    pub fn mem_skipped_all(&self) -> Result<Vec<Skipped>> {
        skipped(self.conn(), None)
    }

    /// `mem.load` (the launcher, once a launch's line went in): one
    /// `startup` row per revision it rendered for the session, at this
    /// moment, and the `session.memory_loaded` event naming them and the
    /// documents the launch `skipped`. A launch that rendered and skipped
    /// nothing writes nothing and opens no transaction.
    pub fn mem_load(
        &mut self,
        ctx: &Ctx,
        session: &str,
        revisions: &[String],
        skipped: &[Skipped],
    ) -> Result<Applied<Loaded>> {
        if revisions.is_empty() && skipped.is_empty() {
            return Ok(nothing(session, "startup", ctx.now));
        }
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let at = tx.now();
            insert(tx, &session, "startup", at, revisions, Some(skipped))
        })
    }

    /// `mem.load` on a resume, `/clear` or `/compact` (the daemon, on the
    /// `SessionStart` observation, `protocol.md` 4.2): copies the session's
    /// newest set at `at` with the new trigger at `at`, so a copy made late
    /// copies the set the session had then, not one recorded after. It
    /// renders and hashes nothing, since the appended prompt is the one the
    /// session launched with. A session that loaded nothing by `at` writes
    /// nothing and opens no transaction; a copy already recorded at `at`
    /// writes nothing either.
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
        if set_as_of(self.conn(), session, Some(at))?.is_empty() {
            return Ok(nothing(session, trigger, at));
        }
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let revisions = set_as_of(tx.conn(), &session.id, Some(at))?;
            insert(tx, &session, trigger, at, &revisions, None)
        })
    }

    /// The daemon's copy pass: makes each copy a `SessionStart` observation
    /// for resume, clear or compact owes. One is owed when the hook
    /// recorded it for a session that had loaded memory by its time, live
    /// or ended less than [`COPY_WINDOW_MINUTES`] ago, the hook bound the
    /// session (no `bind_refused` in its payload), and no row of its
    /// trigger is recorded at its time.
    ///
    /// `since` is the [`CopyPass::mark`] of the caller's last pass: only
    /// observations recorded after its `read_from`, less
    /// [`MARK_SLACK_MINUTES`], are read, so a pass does not grow with a long
    /// session's history. `None` reads every observation, as the first pass
    /// after a start does. A copy that fails is returned in
    /// [`CopyPass::failed`], the pass goes on to the next one, and the mark
    /// stays at or before it, so every later pass tries it again until it
    /// is made, or until [`COPY_WINDOW_MINUTES`] after its observation was
    /// recorded or its session ended. The first pass at or past that time
    /// gives it up ([`FailedCopy::given_up`]): it tries it a last time when
    /// the observation's hour passed, and not at all when the session's
    /// did, since a copy is owed only within an hour of the session's end.
    /// A copy given up holds no mark, so one that never goes in does not
    /// make every pass read a growing history, and the next pass does not
    /// read it again.
    pub fn mem_load_owed(&mut self, ctx: &Ctx, since: Option<CopyMark>) -> Result<CopyPass> {
        let owed = self.owed_copies(ctx.now, since)?;
        let window = ctx.now.plus_minutes(-COPY_WINDOW_MINUTES);
        let mut pass = CopyPass {
            made: Vec::new(),
            failed: Vec::new(),
            mark: CopyMark {
                read_from: ctx.now,
                passed_at: ctx.now,
            },
        };
        for (session, trigger, at, recorded_at, ended_at) in owed {
            if ended_at.is_some_and(|ended| ended <= window) {
                // Its session's hour passed since the last pass read it,
                // so it is not made late, and its drop is reported.
                pass.failed.push(FailedCopy {
                    session_id: session,
                    trigger,
                    observed_at: at,
                    error: Error::Internal(
                        "its session ended an hour ago, before the copy went in".to_owned(),
                    ),
                    given_up: true,
                });
                continue;
            }
            match self.mem_load_again(ctx, &session, &trigger, at) {
                Ok(copied) if copied.value.revision_ids.is_empty() => {}
                Ok(copied) => pass.made.push(copied.value),
                Err(error) => {
                    // The next floor is the mark less the slack, so this
                    // observation is read again, until it is given up.
                    let given_up = recorded_at <= window;
                    if !given_up {
                        pass.mark.read_from = pass.mark.read_from.min(recorded_at);
                    }
                    pass.failed.push(FailedCopy {
                        session_id: session,
                        trigger,
                        observed_at: at,
                        error,
                        given_up,
                    });
                }
            }
        }
        Ok(pass)
    }

    /// The observations [`Self::mem_load_owed`] owes a copy at `now`, read
    /// from `since` less the slack: session, trigger, `observed_at`,
    /// `recorded_at` and the session's `ended_at`, in observation order.
    /// With a mark, a copy the last pass read past its window (made or
    /// given up then) is not read again; one that crossed its window since
    /// is read once more, so its drop is reported.
    fn owed_copies(&self, now: Timestamp, since: Option<CopyMark>) -> Result<Vec<OwedCopy>> {
        // With no mark every observation is read, so the planner, not the
        // `recorded_at` index, picks the scan.
        let (index, floor, cut) = match since {
            Some(mark) => {
                let cut = mark.passed_at.plus_minutes(-COPY_WINDOW_MINUTES);
                (
                    "INDEXED BY observations_recorded",
                    mark.read_from.plus_minutes(-MARK_SLACK_MINUTES).max(cut),
                    cut,
                )
            }
            None => ("", EPOCH, now.plus_minutes(-COPY_WINDOW_MINUTES)),
        };
        let events = COPY_TRIGGERS
            .iter()
            .map(|trigger| format!("'SessionStart:{trigger}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = self.conn().prepare_cached(&format!(
            "SELECT o.session_id, substr(o.event, length('SessionStart:') + 1), o.observed_at,
                        o.recorded_at, s.ended_at
                 FROM observations o {index}
                 JOIN sessions s ON s.id = o.session_id
                 WHERE o.recorded_at > ?2
                   AND (s.ended_at IS NULL OR s.ended_at > ?1)
                   AND EXISTS (SELECT 1 FROM session_loads l
                               WHERE l.session_id = s.id AND l.loaded_at <= o.observed_at)
                   AND o.source = 'hook'
                   AND o.event IN ({events})
                   AND json_extract(o.payload, '$.bind_refused') IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM session_loads l
                       WHERE l.session_id = o.session_id AND l.loaded_at = o.observed_at
                         AND l.trigger = substr(o.event, length('SessionStart:') + 1))
                 ORDER BY o.observed_at, o.id"
        ))?;
        let rows = stmt.query_map(params![cut, floor], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The session's newest load set: the revisions of its latest
    /// `session_loads` moment.
    pub fn mem_loaded(&self, session: &str) -> Result<Vec<String>> {
        newest_set_in(self.conn(), session)
    }
}

/// Whether state.db has a live Tiller document, so a host with none needs
/// no `meta.boundary` to load nothing.
fn any_document(conn: &rusqlite::Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM memory_sources
                        WHERE kind = 'tiller_doc' AND retired_at IS NULL)",
        [],
        |row| row.get(0),
    )?)
}

/// [`Store::mem_loadable`] on `conn`, for a read inside a transaction.
pub(super) fn loadable(
    conn: &rusqlite::Connection,
    project: &str,
    role: &str,
) -> Result<Vec<Loadable>> {
    if !any_document(conn)? {
        return Ok(Vec::new());
    }
    let boundary = host_boundary(conn)?;
    let mut stmt = conn.prepare(
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

/// The documents with no approved revision whose newest revision has text:
/// those a launch of the role in the project would load (`Some`), or every
/// one on the host (`None`).
fn skipped(conn: &rusqlite::Connection, scope: Option<(&str, &str)>) -> Result<Vec<Skipped>> {
    if !any_document(conn)? {
        return Ok(Vec::new());
    }
    let boundary = host_boundary(conn)?;
    let (project, role) = scope.unzip();
    let mut stmt = conn.prepare(
        "SELECT s.id, s.project_id, s.canonical_path
         FROM memory_sources s
         WHERE s.kind = 'tiller_doc' AND s.retired_at IS NULL
           AND s.boundary = ?2
           AND s.canonical_repo IS NOT NULL AND s.canonical_path IS NOT NULL
           AND (?3 IS NULL OR s.project_id IN (?1, ?3))
           AND (?4 IS NULL OR EXISTS (SELECT 1 FROM json_each(s.roles) WHERE value = ?4))
           AND NOT EXISTS (SELECT 1 FROM memory_revisions
                           WHERE source_id = s.id AND review = 'approved')
           AND (SELECT blob_sha FROM memory_revisions WHERE source_id = s.id
                ORDER BY recorded_at DESC, id DESC LIMIT 1) != ?5
         ORDER BY s.project_id != ?1, s.project_id, s.canonical_path, s.id",
    )?;
    let rows = stmt.query_map(
        params![FLEET_PROJECT, boundary, project, role, EMPTY_BLOB],
        |row| {
            Ok(Skipped {
                source_id: row.get(0)?,
                project_id: row.get(1)?,
                path: row.get(2)?,
                reason: NO_APPROVED_REVISION.to_owned(),
            })
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
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
    set_as_of(conn, session, None)
}

/// The revisions of the session's latest `session_loads` moment at or
/// before `at` (`None`: its latest), as [`newest_set_in`] orders them.
fn set_as_of(
    conn: &rusqlite::Connection,
    session: &str,
    at: Option<Timestamp>,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT revision_id FROM session_loads
         WHERE session_id = ?1
           AND loaded_at = (SELECT max(loaded_at) FROM session_loads
                            WHERE session_id = ?1 AND (?2 IS NULL OR loaded_at <= ?2))
         GROUP BY revision_id
         ORDER BY min(rowid)",
    )?;
    let rows = stmt.query_map(params![session, at], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The rows and event of one load. `skipped` is the launch's skipped
/// documents, on a `startup` load only: a copy renders nothing, so it
/// skips nothing either.
fn insert(
    tx: &mut Tx<'_>,
    session: &Session,
    trigger: &str,
    at: Timestamp,
    revisions: &[String],
    skipped: Option<&[Skipped]>,
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
    let skipped = skipped.unwrap_or_default();
    if !written.is_empty() || !skipped.is_empty() {
        let card = match &session.run_id {
            Some(run) => Some(get::<Run>(tx.conn(), "run", run)?.card_id),
            None => None,
        };
        let mut payload = json!({"trigger": trigger, "revision_ids": written});
        if trigger == "startup" {
            payload["skipped"] = json!(skipped);
        }
        tx.event(
            NewEvent::new("session", &session.id, "session.memory_loaded")
                .card_opt(card)
                .payload(payload),
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
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
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
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
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
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
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
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
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
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
            .unwrap();
        let mark = store.mem_load_owed(&daemon, None).unwrap().mark;
        assert_eq!(mark.read_from, daemon.now);
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            mark.read_from.plus_minutes(1),
        );
        observe(
            &mut store,
            &session,
            "SessionStart:compact",
            mark.read_from.plus_minutes(2),
        );
        let old = mark.read_from.plus_minutes(-MARK_SLACK_MINUTES - 1);
        store
            .conn()
            .execute(
                "UPDATE observations SET recorded_at = ?1 WHERE event = 'SessionStart:clear'",
                [old],
            )
            .unwrap();
        let later = Ctx::at(Actor::Daemon, mark.read_from.plus_minutes(3));
        let pass = store.mem_load_owed(&later, Some(mark)).unwrap();
        assert_eq!(
            pass.made
                .iter()
                .map(|l| l.trigger.as_str())
                .collect::<Vec<_>>(),
            ["compact"]
        );
        assert_eq!(pass.mark.read_from, later.now);
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
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
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
        assert!(second.mark.read_from < later.now, "{:?}", second.mark);
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
        assert_eq!(third.mark.read_from, last.now);
    }

    /// A failed copy of an observation recorded longer than the copy window
    /// ago, as the first pass after a start meets one, is given up at once.
    /// It holds the mark back not at all, so the next pass does not read the
    /// window's history again for a copy it would not try.
    #[test]
    fn a_failed_copy_past_the_window_is_given_up_at_once() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
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
        let later = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(120));
        let pass = store.mem_load_owed(&later, None).unwrap();
        assert_eq!(pass.failed.len(), 1);
        assert!(pass.failed[0].given_up, "{:?}", pass.failed);
        assert_eq!(pass.mark.read_from, later.now);
    }

    /// A copy that never goes in is tried again until an hour after its
    /// observation was recorded. The pass that tries it last gives it up,
    /// and the pass after it does not read it.
    #[test]
    fn a_failed_copy_is_given_up_an_hour_after_its_observation() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
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
        let recorded: Timestamp = store
            .conn()
            .query_row(
                "SELECT recorded_at FROM observations WHERE event = 'SessionStart:clear'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let within = Ctx::at(
            Actor::Daemon,
            recorded.plus_minutes(COPY_WINDOW_MINUTES - 1),
        );
        let first = store.mem_load_owed(&within, None).unwrap();
        assert_eq!(first.failed.len(), 1);
        assert!(!first.failed[0].given_up, "{:?}", first.failed);
        assert_eq!(first.mark.read_from, recorded);
        let past = Ctx::at(Actor::Daemon, recorded.plus_minutes(COPY_WINDOW_MINUTES));
        let second = store.mem_load_owed(&past, Some(first.mark)).unwrap();
        assert_eq!(second.failed.len(), 1, "the last try");
        assert!(second.failed[0].given_up, "{:?}", second.failed);
        assert_eq!(
            second.mark.read_from, past.now,
            "a copy given up holds no mark"
        );
        let next = Ctx::at(Actor::Daemon, past.now.plus_minutes(1));
        let third = store.mem_load_owed(&next, Some(second.mark)).unwrap();
        assert!(third.failed.is_empty(), "{:?}", third.failed);
    }

    /// A copy given up while an earlier-held mark still covers it is not
    /// read, or reported, again: the last pass read it past its window.
    #[test]
    fn a_copy_given_up_is_not_reported_again() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_loads
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        let start = daemon.now;
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            start.plus_minutes(1),
        );
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            start.plus_minutes(2),
        );
        let recorded = start.plus_minutes(10);
        store
            .conn()
            .execute(
                "UPDATE observations SET recorded_at = CASE
                     WHEN observed_at = ?1 THEN ?2 ELSE ?3 END
                 WHERE event = 'SessionStart:clear'",
                params![start.plus_minutes(1), recorded, recorded.plus_minutes(3)],
            )
            .unwrap();
        let first = Ctx::at(Actor::Daemon, recorded.plus_minutes(COPY_WINDOW_MINUTES));
        let pass = store.mem_load_owed(&first, None).unwrap();
        assert_eq!(
            pass.failed.iter().map(|f| f.given_up).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(pass.mark.read_from, recorded.plus_minutes(3));
        let next = Ctx::at(Actor::Daemon, first.now.plus_minutes(1));
        let pass = store.mem_load_owed(&next, Some(pass.mark)).unwrap();
        assert_eq!(
            pass.failed
                .iter()
                .map(|f| f.observed_at)
                .collect::<Vec<_>>(),
            [start.plus_minutes(2)],
            "only the copy still in its window"
        );
    }

    /// A copy whose session ended is given up an hour after the end, and
    /// that last try is reported: the session's window does not drop it
    /// without a word.
    #[test]
    fn a_failed_copy_of_an_ended_session_is_given_up_when_reported() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
            .unwrap();
        store
            .conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_clear BEFORE INSERT ON session_loads
                 WHEN NEW.trigger = 'clear'
                 BEGIN SELECT RAISE(ABORT, 'no clear copies'); END;",
            )
            .unwrap();
        let ended = daemon.now.plus_minutes(2);
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        store
            .conn()
            .execute(
                "UPDATE observations SET recorded_at = ?1 WHERE event = 'SessionStart:clear'",
                [ended.plus_minutes(10)],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
                params![ended, session],
            )
            .unwrap();
        let within = Ctx::at(Actor::Daemon, ended.plus_minutes(30));
        let first = store.mem_load_owed(&within, None).unwrap();
        assert_eq!(first.failed.len(), 1);
        assert!(!first.failed[0].given_up, "{:?}", first.failed);
        let past = Ctx::at(Actor::Daemon, ended.plus_minutes(COPY_WINDOW_MINUTES + 1));
        let second = store.mem_load_owed(&past, Some(first.mark)).unwrap();
        assert_eq!(second.failed.len(), 1, "the last try is reported");
        assert!(second.failed[0].given_up, "{:?}", second.failed);
        assert_eq!(second.mark.read_from, past.now);
        let next = Ctx::at(Actor::Daemon, past.now.plus_minutes(1));
        let third = store.mem_load_owed(&next, Some(second.mark)).unwrap();
        assert!(third.failed.is_empty(), "{:?}", third.failed);
    }

    /// A poller that did not run for longer than the copy window does not
    /// make a copy for a session that ended over an hour ago: the drop is
    /// reported as given up.
    #[test]
    fn a_stalled_pass_gives_up_a_copy_whose_session_hour_passed() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
            .unwrap();
        let mark = store.mem_load_owed(&daemon, None).unwrap().mark;
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let ended = daemon.now.plus_minutes(2);
        store
            .conn()
            .execute(
                "UPDATE sessions SET ended_at = ?1 WHERE id = ?2",
                params![ended, session],
            )
            .unwrap();
        let late = Ctx::at(Actor::Daemon, ended.plus_minutes(COPY_WINDOW_MINUTES + 1));
        let pass = store.mem_load_owed(&late, Some(mark)).unwrap();
        assert!(pass.made.is_empty(), "{:?}", pass.made);
        assert_eq!(pass.failed.len(), 1);
        assert!(pass.failed[0].given_up, "{:?}", pass.failed);
        assert_eq!(triggers(&store, &session), ["startup"]);
    }

    /// An observation from before the session's first load owes no copy:
    /// the session had no set then. The pass does not select it, so it is
    /// not read on every pass with no mark.
    #[test]
    fn an_observation_before_the_first_load_owes_no_copy() {
        let (mut store, session) = memory_store();
        document(&store);
        let daemon = Ctx::new(Actor::Daemon);
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(-1),
        );
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
            .unwrap();
        assert_eq!(store.owed_copies(daemon.now, None).unwrap(), Vec::new());
        assert!(owed(&mut store, &daemon).is_empty());
        assert_eq!(triggers(&store, &session), ["startup"]);
    }

    /// A copy made late copies the set the session had at the observation,
    /// not a set recorded after it.
    #[test]
    fn a_late_copy_copies_the_set_of_its_moment() {
        let (mut store, session) = memory_store();
        document(&store);
        store
            .conn()
            .execute_batch(
                "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance,
                     scrub, recorded_at, recorded_by)
                 VALUES ('R-beta', 'S-alpha', 'def', 'approved', 'baseline', 'passed',
                     '2026-11-02T10:00:00.000Z', 'daemon');",
            )
            .unwrap();
        let daemon = Ctx::new(Actor::Daemon);
        store
            .mem_load(&daemon, &session, &["R-alpha".to_owned()], &[])
            .unwrap();
        observe(
            &mut store,
            &session,
            "SessionStart:clear",
            daemon.now.plus_minutes(1),
        );
        let relaunch = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(3));
        store
            .mem_load(&relaunch, &session, &["R-beta".to_owned()], &[])
            .unwrap();
        let later = Ctx::at(Actor::Daemon, daemon.now.plus_minutes(5));
        assert_eq!(owed(&mut store, &later).len(), 1);
        let copied: Vec<String> = store
            .conn()
            .prepare(
                "SELECT revision_id FROM session_loads WHERE session_id = ?1 AND trigger = 'clear'",
            )
            .unwrap()
            .query_map([&session], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(copied, ["R-alpha"]);
        assert_eq!(store.mem_loaded(&session).unwrap(), ["R-beta"]);
    }
}
