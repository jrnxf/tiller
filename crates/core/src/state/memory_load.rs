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

/// The triggers of `session_loads.trigger`.
const TRIGGERS: &[&str] = &["startup", "resume", "clear", "compact"];

impl Store {
    /// The approved revisions a launch of `role` in `project` renders, in
    /// prompt order: the `fleet` document first, then the project's. A
    /// source loads when it is a live Tiller document of this host's
    /// boundary, its `roles` name the role, and it has an approved
    /// revision; the current one is the approved revision recorded last (an
    /// undo records its earlier blob again). A source with no approved
    /// revision yet loads nothing: its document has no approved text.
    pub fn mem_loadable(&self, project: &str, role: &str) -> Result<Vec<Loadable>> {
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

    /// `mem.load` (the launcher, after a launch rendered `revisions`): one
    /// `startup` row per revision for the session, at this moment. A launch
    /// that rendered nothing writes nothing and opens no transaction.
    pub fn mem_load(
        &mut self,
        ctx: &Ctx,
        session: &str,
        revisions: &[String],
    ) -> Result<Applied<Loaded>> {
        if revisions.is_empty() {
            return Ok(nothing(session, "startup", Timestamp::now()));
        }
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let at = tx.now();
            insert(tx, &session, "startup", at, revisions)
        })
    }

    /// `mem.load` on a resume, `/clear` or `/compact` (the `SessionStart`
    /// observation's writer, `protocol.md` 4.2): copies the session's newest
    /// set with the new trigger at `at`. It renders and hashes nothing, since
    /// the appended prompt is the one the session launched with. A session
    /// that loaded nothing writes nothing and opens no transaction, so the
    /// hook stays in its budget; a copy already recorded at `at` (a replayed
    /// observation) writes nothing either.
    pub fn mem_load_again(
        &mut self,
        ctx: &Ctx,
        session: &str,
        trigger: &str,
        at: Timestamp,
    ) -> Result<Applied<Loaded>> {
        if !TRIGGERS.contains(&trigger) || trigger == "startup" {
            return Err(Error::Usage(format!(
                "a copied load is for resume, clear or compact, not {trigger}"
            )));
        }
        if newest_set_in(self.conn(), session)?.is_empty() {
            return Ok(nothing(session, trigger, at));
        }
        self.write(ctx, "mem.load", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            let revisions = newest_set(tx, &session.id)?;
            insert(tx, &session, trigger, at, &revisions)
        })
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

fn newest_set(tx: &Tx<'_>, session: &str) -> Result<Vec<String>> {
    newest_set_in(tx.conn(), session)
}

fn newest_set_in(conn: &rusqlite::Connection, session: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT revision_id FROM session_loads
         WHERE session_id = ?1
           AND loaded_at = (SELECT max(loaded_at) FROM session_loads WHERE session_id = ?1)
         ORDER BY rowid",
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
