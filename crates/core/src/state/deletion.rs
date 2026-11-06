//! `project.delete`: the hard delete of one project and every row that
//! belongs to it (`state.md`, Deleting a project). It is the one mutation that
//! deletes event rows: the project's audit trail goes with it, and a
//! `project.deleted` tombstone records what was removed.
//!
//! The rows to remove are found from the schema, not from a table list, so a
//! new table is covered the day its migration lands: starting from the
//! project row, a row belongs to the project when a `NOT NULL` foreign key of
//! it points at a row that does (or a nullable one in [`OWNED`]). A nullable
//! foreign key on a row that stays and points at a removed row is set to
//! `NULL` instead ("unlinked"), and a row of a [`LINKS`] table that ties a
//! removed row to one that stays is removed and counted as unlinked too. The
//! test `every_reference_is_classified`
//! fails until a new nullable or soft reference is sorted into one of the
//! lists below.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::attention::withdraw_attention;
use super::get;
use super::projects::{local_host_id, require_owner};
use crate::domain::{Attention, Project};
use crate::error::{Error, Result};
use crate::ids::FLEET_PROJECT;
use crate::store::{Actor, Applied, Ctx, NewEvent, Store};

/// Nullable foreign keys that still mean "belongs to": an observation of a
/// removed session or run, and a worktree cached for the project.
pub(crate) const OWNED: &[(&str, &str)] = &[
    ("observations", "run_id"),
    ("observations", "session_id"),
    ("worktrees", "project_id"),
];

/// Nullable foreign keys that only point: a row of another project (or of
/// `fleet`) that stays when its target goes, with the column set to `NULL`.
pub(crate) const UNLINKED: &[(&str, &str)] = &[
    ("approvals", "grant_id"),
    ("approvals", "linked_approval_id"),
    ("approvals", "pr_id"),
    ("approvals", "run_id"),
    ("approvals", "card_id"),
    ("artifacts", "created_by_session_id"),
    ("artifacts", "run_id"),
    ("ask_cards", "linked_by_session_id"),
    ("asks", "resolved_by_session_id"),
    ("asks", "session_id"),
    ("attention", "opened_by_run_id"),
    ("attention", "opened_by_session_id"),
    ("cards", "created_by_ask_id"),
    ("cards", "fixes_card_id"),
    ("evidence", "superseded_by"),
    ("evidence", "session_id"),
    ("evidence", "run_id"),
    ("evidence", "card_id"),
    ("memories", "retired_revision_id"),
    ("memories", "authored_by_session_id"),
    ("memory_revisions", "approval_id"),
    ("memory_revisions", "parent_revision_id"),
    ("obligations", "met_by_event_seq"),
    ("obligations", "card_id"),
    ("outbox", "card_id"),
    ("panes", "session_id"),
    ("proposals", "applied_revision_id"),
    ("proposals", "approval_id"),
    ("proposals", "card_id"),
    ("proposals", "authored_by_session_id"),
    ("proposals", "memory_id"),
    ("prs", "run_id"),
    ("publishes", "pr_id"),
    ("retros", "proposal_id"),
    ("retros", "evidence_id"),
    ("retros", "trigger"),
    ("review_passes", "obligation_id"),
    ("review_passes", "report_artifact_id"),
    ("runs", "predecessor_run_id"),
    ("sessions", "run_id"),
    ("worktrees", "run_id"),
];

/// Id columns with no foreign key. `Unlink` ones are set to `NULL` like
/// [`UNLINKED`]; `Remove` ones remove their row; `Keep` ones stay as they are.
pub(crate) const SOFT: &[(&str, &str, Soft)] = &[
    (
        "evidence",
        "contradicts_memory_id",
        Soft::Unlink("memories"),
    ),
    // The subject of an item that stays names a project or session that may
    // be gone: the page says what became of it.
    ("attention", "subject_project_id", Soft::Keep),
    ("attention", "subject_session_id", Soft::Keep),
    ("events", "entity_id", Soft::Remove),
    ("events", "card_id", Soft::Remove),
    ("idempotency_keys", "entity_id", Soft::Remove),
    ("intervention_labels", "ref_id", Soft::Remove),
    // A memory made from a proposal keeps naming it: its provenance check
    // needs the id, and the memory's own revisions carry the text.
    ("memories", "proposal_id", Soft::Keep),
];

/// Tables whose rows only tie two records together. A link between a removed
/// row and one that stays (another project's card, a fleet memory or session)
/// is removed with the project and reported under `unlinked`; any other row
/// that needs a row of another project refuses the delete.
pub(crate) const LINKS: &[&str] = &[
    "ask_cards",
    "card_deps",
    "change_set_members",
    "memory_evidence",
    "proposal_evidence",
    "session_loads",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Soft {
    Unlink(&'static str),
    Remove,
    Keep,
}

/// What `project.delete` removes, or removed: rows per table, and per
/// `table.column` the references to rows that stay or from them that were
/// cleared: set to `NULL` on a row that stays, or a removed [`LINKS`] row.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Deletion {
    pub project: String,
    pub removed: BTreeMap<String, i64>,
    pub unlinked: BTreeMap<String, i64>,
    /// Open attention items of other projects (the fleet project included)
    /// that are about this project or one of its sessions, withdrawn with it:
    /// nobody is left to act on them.
    #[serde(default)]
    pub withdrawn_attention: i64,
}

/// [`Deletion`] before it happens, with the reason it would be refused.
/// Running agents do not refuse it: the owner's delete stops them first, so
/// they are listed, with the work they leave behind.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeletionPreview {
    #[serde(flatten)]
    pub deletion: Deletion,
    pub refused: Option<String>,
    /// What clears `refused`, when the owner can clear it from here.
    #[serde(default)]
    pub unblock: Option<Unblock>,
    /// The agents the delete stops before it removes anything.
    #[serde(default)]
    pub agents: Vec<LiveAgent>,
    /// Worktrees holding commits not yet landed, uncommitted changes, or
    /// state not yet read. They stay on disk; the delete forgets them.
    #[serde(default)]
    pub unlanded: Vec<UnlandedWork>,
    /// Pull requests still open on GitHub. They stay open; the delete
    /// forgets them.
    #[serde(default)]
    pub open_prs: Vec<OpenPr>,
}

/// A running agent of a project: a live session, or an active run whose
/// session has already ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiveAgent {
    pub role: String,
    /// The live session; `None` for an active run with no live session.
    pub session: Option<String>,
    /// The display id of the worker's card; `None` for an orchestrator.
    pub card: Option<String>,
    pub run: Option<String>,
    /// Whether it runs on this host, the only one that can stop it.
    pub local: bool,
}

impl LiveAgent {
    /// How a refusal or a prompt names it: `builder on alpha-42 (session
    /// ...)`, or `orchestrator (session ...)`.
    pub fn describe(&self) -> String {
        let mut name = self.role.clone();
        if let Some(card) = &self.card {
            name.push_str(&format!(" on {card}"));
        }
        match (&self.session, &self.run) {
            (Some(session), _) => name.push_str(&format!(" (session {session})")),
            (None, Some(run)) => name.push_str(&format!(" (run {run}, no live session)")),
            (None, None) => {}
        }
        name
    }
}

/// A worktree of the project that holds work not yet landed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnlandedWork {
    pub path: String,
    pub branch: Option<String>,
    /// The display id of the card whose run made it, when known.
    pub card: Option<String>,
    /// Commits not on the base branch; `None` when not yet read.
    pub unlanded_commits: Option<i64>,
    /// Uncommitted changes; `None` when not yet read.
    pub dirty: Option<bool>,
}

/// A pull request of the project still open on GitHub.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OpenPr {
    /// The display id of its card.
    pub card: String,
    pub number: i64,
    pub url: String,
}

/// The owner's way past a refused delete: the step, and the command that
/// starts it when there is one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Unblock {
    pub step: String,
    pub command: Option<String>,
}

/// Why the delete is refused while the project has rows in flight.
struct Blocker {
    fact: String,
    unblock: Option<Unblock>,
}

impl Store {
    /// The counts `project.delete` would remove, without removing anything,
    /// and why it would be refused. Read-only.
    pub fn project_delete_preview(&self, project_id: &str) -> Result<DeletionPreview> {
        let tx = self.conn().unchecked_transaction()?;
        let project: Project = get(&tx, "project", project_id)?;
        match check_deletable(&tx, &project) {
            Ok(()) => {}
            // Nothing is counted for `fleet` or another host's project: it
            // would walk rows that can never be deleted here.
            Err(Error::Refused { fact, .. }) => {
                return Ok(DeletionPreview {
                    deletion: Deletion {
                        project: project.id,
                        ..Deletion::default()
                    },
                    refused: Some(fact),
                    ..DeletionPreview::default()
                });
            }
            Err(error) => return Err(error),
        }
        let fks = foreign_keys(&tx)?;
        mark(&tx, &fks, &project.id)?;
        let agents = live_agents(&tx, &project.id)?;
        let blocked = match agents.iter().find(|LiveAgent { local, .. }| !local) {
            // Only the host an agent runs on can stop it.
            Some(remote) => Some(Blocker {
                fact: format!("{} runs on another host", remote.describe()),
                unblock: Some(Unblock {
                    step: "stop it on its host".into(),
                    command: remote
                        .session
                        .as_ref()
                        .map(|session| format!("tiller agent stop {session}")),
                }),
            }),
            None => blocker(&tx, &fks, &project.id, Live::Stopped)?,
        };
        let (refused, unblock) = match blocked {
            Some(Blocker { fact, unblock }) => (Some(fact), unblock),
            None => (None, None),
        };
        let mut deletion = counts(&tx, &fks, &project.id)?;
        deletion.withdrawn_attention = about_project(&tx, &project.id)?.len() as i64;
        let unlanded = unlanded_work(&tx, &project.id)?;
        let open_prs = open_prs(&tx, &project.id)?;
        // Dropping the transaction rolls the scratch table back.
        Ok(DeletionPreview {
            deletion,
            refused,
            unblock,
            agents,
            unlanded,
            open_prs,
        })
    }

    /// The project's running agents: its live sessions, the orchestrator
    /// first, then its active runs with no live session.
    pub fn project_live_agents(&self, project_id: &str) -> Result<Vec<LiveAgent>> {
        live_agents(self.conn(), project_id)
    }

    /// `project.delete`: removes the project and every row that belongs to
    /// it, its events included, on the host that owns it. Refused for
    /// `fleet`, for a project another host owns, and while any of its runs,
    /// sessions or approval executions is live. Never touches a checkout,
    /// a worktree on disk or the remote repository.
    pub fn project_delete(&mut self, ctx: &Ctx, project_id: &str) -> Result<Applied<Deletion>> {
        require_owner(&ctx.actor, "project.delete")?;
        if matches!(ctx.actor, Actor::Peer(_)) {
            return Err(Error::Forbidden(format!(
                "project.delete runs on the owning host only; {} may not",
                ctx.actor
            )));
        }
        self.write(ctx, "project.delete", None, |tx| {
            let project: Project = get(tx.conn(), "project", project_id)?;
            check_deletable(tx.conn(), &project)?;
            let fks = foreign_keys(tx.conn())?;
            mark(tx.conn(), &fks, &project.id)?;
            if let Some(Blocker { fact, .. }) = blocker(tx.conn(), &fks, &project.id, Live::Refuse)?
            {
                return Err(Error::refused("project", &project.id, fact));
            }
            let mut deletion = counts(tx.conn(), &fks, &project.id)?;
            let reason = format!("project {} was deleted", project.id);
            for id in about_project(tx.conn(), &project.id)? {
                let item: Attention = get(tx.conn(), "attention", &id)?;
                withdraw_attention(tx, &item, &reason)?;
                deletion.withdrawn_attention += 1;
            }
            apply(tx.conn(), &fks)?;
            tx.event(
                NewEvent::new("project", &project.id, "project.deleted")
                    .rev(project.rev + 1)
                    .payload(json!({
                        "name": project.name,
                        // A later `project.add` of the same id numbers its
                        // cards on from here, so display ids are never reused.
                        "next_card_seq": project.next_card_seq,
                        "removed": deletion.removed,
                        "unlinked": deletion.unlinked,
                        "withdrawn_attention": deletion.withdrawn_attention,
                    })),
            )?;
            Ok(deletion)
        })
    }
}

/// The open attention items, on cards that stay, about the project or one of
/// its sessions. Items on the project's own cards are removed with them.
fn about_project(conn: &Connection, project_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT a.id FROM attention a JOIN cards c ON c.id = a.card_id
         WHERE a.answered_at IS NULL AND a.withdrawn_at IS NULL
           AND c.project_id <> ?1
           AND (a.subject_project_id = ?1
                OR a.subject_session_id IN (SELECT id FROM sessions WHERE project_id = ?1))
         ORDER BY a.opened_at, a.id",
    )?;
    let ids = stmt.query_map([project_id], |row| row.get(0))?;
    Ok(ids.collect::<rusqlite::Result<_>>()?)
}

/// The checks that need no row walk.
fn check_deletable(conn: &Connection, project: &Project) -> Result<()> {
    if project.id == FLEET_PROJECT {
        return Err(Error::refused(
            "project",
            &project.id,
            "the fleet project is never deleted",
        ));
    }
    if project.owner_host_id != local_host_id(conn)? {
        return Err(Error::refused(
            "project",
            &project.id,
            "another host owns this project; delete it there",
        ));
    }
    Ok(())
}

/// One foreign key: `table.column` references `parent.to`.
struct ForeignKey {
    table: String,
    column: String,
    parent: String,
    to: String,
    nullable: bool,
}

fn foreign_keys(conn: &Connection) -> Result<Vec<ForeignKey>> {
    let mut stmt = conn.prepare(
        "SELECT m.name, f.\"from\", f.\"table\", f.\"to\", c.\"notnull\"
         FROM sqlite_schema m
         JOIN pragma_foreign_key_list(m.name) f
         JOIN pragma_table_info(m.name) c ON c.name = f.\"from\"
         WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%'
         ORDER BY m.name, f.\"from\"",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ForeignKey {
            table: row.get(0)?,
            column: row.get(1)?,
            parent: row.get(2)?,
            to: row.get(3)?,
            nullable: row.get::<_, i64>(4)? == 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Tables whose primary key is one `id` column: the ids events and keys name.
fn id_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT m.name FROM sqlite_schema m
         WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%'
           AND (SELECT count(*) FROM pragma_table_info(m.name) WHERE pk > 0) = 1
           AND EXISTS (SELECT 1 FROM pragma_table_info(m.name) WHERE pk = 1 AND name = 'id')
         ORDER BY m.name",
    )?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn owns(fk: &ForeignKey) -> bool {
    !fk.nullable
        || OWNED
            .iter()
            .any(|(table, column)| fk.table == *table && fk.column == *column)
}

/// The rows of `table` marked for removal, as a subquery of their `column`.
fn doomed(table: &str, column: &str) -> String {
    format!(
        "SELECT p.\"{column}\" FROM \"{table}\" p JOIN temp.project_doomed d
         ON d.tbl = '{table}' AND d.rid = p.rowid"
    )
}

/// Marks, in `temp.project_doomed`, every row that belongs to the project.
fn mark(conn: &Connection, fks: &[ForeignKey], project_id: &str) -> Result<()> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp.project_doomed;
         CREATE TEMP TABLE project_doomed (
           tbl TEXT NOT NULL,
           rid INTEGER NOT NULL,
           PRIMARY KEY (tbl, rid)
         );",
    )?;
    conn.execute(
        "INSERT INTO temp.project_doomed SELECT 'projects', rowid FROM projects WHERE id = ?1",
        [project_id],
    )?;
    spread(conn, fks)?;

    // The project's audit trail: events about it (the project row is marked,
    // so its id is among `ids`), its cards or any row it owns, and the
    // idempotency keys and labels that name those rows. The tombstones of
    // earlier deletes of the same id stay.
    let ids = id_tables(conn)?
        .iter()
        .map(|table| doomed(table, "id"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO temp.project_doomed
             SELECT 'events', seq FROM events
             WHERE kind <> 'project.deleted'
               AND (card_id IN ({cards}) OR entity_id IN ({ids}))",
            cards = doomed("cards", "id"),
        ),
        [],
    )?;
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO temp.project_doomed
             SELECT 'idempotency_keys', rowid FROM idempotency_keys WHERE entity_id IN ({ids})"
        ),
        [],
    )?;
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO temp.project_doomed
             SELECT 'intervention_labels', rowid FROM intervention_labels WHERE ref_id IN ({ids})"
        ),
        [],
    )?;
    // Again, for any row that needs one of those event or key rows.
    spread(conn, fks)
}

/// Marks rows that belong to a marked row until nothing new is marked.
fn spread(conn: &Connection, fks: &[ForeignKey]) -> Result<()> {
    loop {
        let mut added = 0;
        for fk in fks.iter().filter(|fk| owns(fk)) {
            added += conn.execute(
                &format!(
                    "INSERT OR IGNORE INTO temp.project_doomed
                     SELECT '{table}', rowid FROM \"{table}\" WHERE \"{column}\" IN ({parents})",
                    table = fk.table,
                    column = fk.column,
                    parents = doomed(&fk.parent, &fk.to),
                ),
                [],
            )?;
        }
        if added == 0 {
            return Ok(());
        }
    }
}

/// Whether [`blocker`] counts the project's running agents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Live {
    /// They refuse: the delete itself never stops an agent.
    Refuse,
    /// The owner's delete stops them before it starts, so they do not.
    Stopped,
}

/// Why the marked rows may not go: something is live, or the walk reached
/// a row of another project.
fn blocker(
    conn: &Connection,
    fks: &[ForeignKey],
    project_id: &str,
    agents: Live,
) -> Result<Option<Blocker>> {
    let live = |sql: &str| -> Result<Option<(String, String)>> {
        Ok(conn
            .query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?)
    };
    // The owner cannot end a run directly; cancelling its card does.
    if let (Live::Refuse, Some((role, card))) = (
        agents,
        live(&format!(
            "SELECT r.role, c.display_id FROM runs r JOIN cards c ON c.id = r.card_id
             WHERE r.ended_at IS NULL AND r.id IN ({}) ORDER BY r.id LIMIT 1",
            doomed("runs", "id")
        ))?,
    ) {
        return Ok(Some(Blocker {
            fact: format!("{role} run on {card} is live; end it first"),
            unblock: Some(Unblock {
                step: "cancel its card".into(),
                command: Some(format!(
                    "tiller card cancel {card} --reason \"deleting project {project_id}\""
                )),
            }),
        }));
    }
    // A session ends when its agent exits: `tiller agent stop` types the
    // `/exit`, and the `SessionEnd` hook records it.
    if let (Live::Refuse, Some((role, session))) = (
        agents,
        live(&format!(
            "SELECT role, id FROM sessions WHERE ended_at IS NULL AND id IN ({}) ORDER BY id LIMIT 1",
            doomed("sessions", "id")
        ))?,
    ) {
        return Ok(Some(Blocker {
            fact: format!("{role} session {session} is live; end it first"),
            unblock: Some(Unblock {
                step: "stop its agent".into(),
                command: Some(format!("tiller agent stop {session}")),
            }),
        }));
    }
    if let Some((approval, _)) = live(&format!(
        "SELECT id, state FROM approvals WHERE state = 'executing' AND id IN ({}) ORDER BY id LIMIT 1",
        doomed("approvals", "id")
    ))? {
        return Ok(Some(Blocker {
            fact: format!("approval {approval} is executing"),
            unblock: Some(Unblock {
                step: "wait for it to finish".into(),
                command: None,
            }),
        }));
    }
    // A marked row whose required project or card stays belongs to someone
    // else: the walk reached it through a reference, not ownership. A link
    // row is only cut, and `counts` reports it.
    for fk in fks.iter().filter(|fk| {
        !fk.nullable
            && (fk.parent == "projects" || fk.parent == "cards")
            && !LINKS.contains(&fk.table.as_str())
    }) {
        let stray: Option<String> = conn
            .query_row(
                &format!(
                    "SELECT p.\"{column}\" FROM \"{table}\" p JOIN temp.project_doomed d
                     ON d.tbl = '{table}' AND d.rid = p.rowid
                     WHERE p.\"{column}\" NOT IN ({parents}) LIMIT 1",
                    table = fk.table,
                    column = fk.column,
                    parents = doomed(&fk.parent, &fk.to),
                ),
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(other) = stray {
            let owner: String = if fk.parent == "cards" {
                conn.query_row(
                    "SELECT display_id FROM cards WHERE id = ?1",
                    [&other],
                    |row| row.get(0),
                )?
            } else {
                other
            };
            return Ok(Some(Blocker {
                fact: format!(
                    "{} rows of {owner} depend on {project_id}; untangle them first",
                    fk.table
                ),
                unblock: None,
            }));
        }
    }
    Ok(None)
}

fn live_agents(conn: &Connection, project_id: &str) -> Result<Vec<LiveAgent>> {
    let local = local_host_id(conn)?;
    let mut stmt = conn.prepare(
        "SELECT role, session, card, run, host FROM (
           SELECT s.role, s.id AS session, c.display_id AS card, s.run_id AS run,
                  s.host_id AS host
           FROM sessions s
           LEFT JOIN runs r ON r.id = s.run_id
           LEFT JOIN cards c ON c.id = r.card_id
           WHERE s.ended_at IS NULL AND s.project_id = ?1
           UNION ALL
           SELECT r.role, NULL, c.display_id, r.id, r.host_id
           FROM runs r JOIN cards c ON c.id = r.card_id
           WHERE r.ended_at IS NULL AND c.project_id = ?1
             AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.run_id = r.id AND s.ended_at IS NULL)
         )
         ORDER BY role <> 'orchestrator', card, session, run",
    )?;
    let rows = stmt.query_map([project_id], |row| {
        let host: String = row.get(4)?;
        Ok(LiveAgent {
            role: row.get(0)?,
            session: row.get(1)?,
            card: row.get(2)?,
            run: row.get(3)?,
            local: host == local,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The project's worktrees still on disk that hold work not landed, or
/// whose state was never read.
fn unlanded_work(conn: &Connection, project_id: &str) -> Result<Vec<UnlandedWork>> {
    let mut stmt = conn.prepare(
        "SELECT w.path, w.branch, c.display_id, w.unlanded_commits, w.dirty
         FROM worktrees w
         LEFT JOIN runs r ON r.id = w.run_id
         LEFT JOIN cards c ON c.id = r.card_id
         WHERE w.project_id = ?1 AND w.gone_at IS NULL
           AND (w.unlanded_commits IS NULL OR w.unlanded_commits > 0
                OR w.dirty IS NULL OR w.dirty = 1)
         ORDER BY c.display_id, w.path",
    )?;
    let rows = stmt.query_map([project_id], |row| {
        Ok(UnlandedWork {
            path: row.get(0)?,
            branch: row.get(1)?,
            card: row.get(2)?,
            unlanded_commits: row.get(3)?,
            dirty: row.get::<_, Option<i64>>(4)?.map(|dirty| dirty == 1),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn open_prs(conn: &Connection, project_id: &str) -> Result<Vec<OpenPr>> {
    let mut stmt = conn.prepare(
        "SELECT c.display_id, p.number, p.url FROM prs p JOIN cards c ON c.id = p.card_id
         WHERE p.project_id = ?1 AND p.state = 'open'
         ORDER BY p.number",
    )?;
    let rows = stmt.query_map([project_id], |row| {
        Ok(OpenPr {
            card: row.get(0)?,
            number: row.get(1)?,
            url: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The rows to remove per table and the references to unlink.
fn counts(conn: &Connection, fks: &[ForeignKey], project_id: &str) -> Result<Deletion> {
    let mut removed = BTreeMap::new();
    let mut stmt =
        conn.prepare("SELECT tbl, count(*) FROM temp.project_doomed GROUP BY tbl ORDER BY tbl")?;
    for row in stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (table, n): (String, i64) = row?;
        removed.insert(table, n);
    }
    let mut unlinked = BTreeMap::new();
    for fk in fks
        .iter()
        .filter(|fk| !fk.nullable && LINKS.contains(&fk.table.as_str()))
    {
        let n: i64 = conn.query_row(
            &format!(
                "SELECT count(*) FROM \"{table}\" p JOIN temp.project_doomed d
                 ON d.tbl = '{table}' AND d.rid = p.rowid
                 WHERE p.\"{column}\" NOT IN ({parents})",
                table = fk.table,
                column = fk.column,
                parents = doomed(&fk.parent, &fk.to),
            ),
            [],
            |row| row.get(0),
        )?;
        if n > 0 {
            unlinked.insert(format!("{}.{}", fk.table, fk.column), n);
        }
    }
    for (table, column, parent, to) in unlinks(fks)? {
        let n: i64 = conn.query_row(
            &unlink_filter(&table, &column, &parent, &to, "count(*)"),
            [],
            |row| row.get(0),
        )?;
        if n > 0 {
            unlinked.insert(format!("{table}.{column}"), n);
        }
    }
    Ok(Deletion {
        project: project_id.to_owned(),
        removed,
        unlinked,
        withdrawn_attention: 0,
    })
}

/// Every reference that is set to `NULL` on a row that stays.
fn unlinks(fks: &[ForeignKey]) -> Result<Vec<(String, String, String, String)>> {
    let mut all = Vec::new();
    for fk in fks.iter().filter(|fk| !owns(fk)) {
        if !UNLINKED
            .iter()
            .any(|(table, column)| fk.table == *table && fk.column == *column)
        {
            // A new nullable reference must be sorted into OWNED or UNLINKED
            // before a delete may guess which it is.
            return Err(Error::Internal(format!(
                "project.delete does not know whether {}.{} owns or only points",
                fk.table, fk.column
            )));
        }
        all.push((
            fk.table.clone(),
            fk.column.clone(),
            fk.parent.clone(),
            fk.to.clone(),
        ));
    }
    for (table, column, soft) in SOFT {
        if let Soft::Unlink(parent) = soft {
            all.push((
                table.to_string(),
                column.to_string(),
                parent.to_string(),
                "id".into(),
            ));
        }
    }
    Ok(all)
}

fn unlink_filter(table: &str, column: &str, parent: &str, to: &str, what: &str) -> String {
    format!(
        "SELECT {what} FROM \"{table}\"
         WHERE \"{column}\" IN ({parents})
           AND rowid NOT IN (SELECT rid FROM temp.project_doomed WHERE tbl = '{table}')",
        parents = doomed(parent, to),
    )
}

/// Unlinks, then removes, every marked row. Foreign keys are checked at
/// commit, so the order of the deletes does not matter, and a reference the
/// walk missed fails the commit instead of leaving a dangling row.
fn apply(conn: &Connection, fks: &[ForeignKey]) -> Result<()> {
    conn.execute_batch("PRAGMA defer_foreign_keys = ON")?;
    for (table, column, parent, to) in unlinks(fks)? {
        conn.execute(
            &format!(
                "UPDATE \"{table}\" SET \"{column}\" = NULL WHERE rowid IN ({})",
                unlink_filter(&table, &column, &parent, &to, "rowid")
            ),
            [],
        )?;
    }
    let tables: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT tbl FROM temp.project_doomed ORDER BY tbl")?;
        stmt.query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for table in tables {
        conn.execute(
            &format!(
                "DELETE FROM \"{table}\" WHERE rowid IN
                 (SELECT rid FROM temp.project_doomed WHERE tbl = ?1)"
            ),
            params![table],
        )?;
    }
    conn.execute_batch("DROP TABLE temp.project_doomed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::authority::decisions_by_wrong_principal;
    use crate::domain::{CardKind, RunOutcome};
    use crate::state::asks::NewAsk;
    use crate::state::attention::NewAttention;
    use crate::state::cards::NewCard;
    use crate::state::projects::{NewProject, Numbering};
    use crate::state::runs::EndRun;
    use crate::state::tests::{builder_run, card, memory_store, owner};

    /// Id columns that name something outside `state.db`'s projects: a
    /// display id, a legacy or external id, a host, a pane.
    const NOT_PROJECT_REFERENCES: &[(&str, &str)] = &[
        ("cards", "display_id"),
        ("cards", "legacy_id"),
        ("hosts", "peer_host_id"),
        ("memories", "legacy_id"),
        ("memories", "scope_value"),
        ("outbox", "target_pane_id"),
        ("panes", "pane_id"),
        ("peer_keys", "viewer_host_id"),
        ("session_loads", "trigger"),
        ("sessions", "claude_session_id"),
        ("sessions", "pane_id"),
    ];

    fn project(id: &str) -> NewProject {
        NewProject {
            id: id.into(),
            name: id.to_uppercase(),
            repo: Some(format!("example/{id}")),
            remote: None,
            default_branch: None,
            delivery: crate::domain::Delivery::DirectPr,
            checks: None,
            capacity: None,
            deploy_workflow: None,
            deploy_environment: None,
            deploy_timeout_minutes: None,
        }
    }

    fn count(store: &Store, sql: &str) -> i64 {
        store.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// A new migration's references have to be sorted before a delete runs:
    /// every nullable foreign key is owned or unlinked, and every id column
    /// without a foreign key is listed in `SOFT` or here.
    #[test]
    fn every_reference_is_classified() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn();
        let nullable: BTreeSet<(String, String)> = foreign_keys(conn)
            .unwrap()
            .into_iter()
            .filter(|fk| fk.nullable)
            .map(|fk| (fk.table, fk.column))
            .collect();
        let listed: BTreeSet<(String, String)> = OWNED
            .iter()
            .chain(UNLINKED)
            .map(|(table, column)| (table.to_string(), column.to_string()))
            .collect();
        assert_eq!(
            nullable, listed,
            "sort every nullable foreign key into OWNED or UNLINKED"
        );
        assert_eq!(listed.len(), OWNED.len() + UNLINKED.len(), "listed twice");

        let fk_columns: BTreeSet<(String, String)> = foreign_keys(conn)
            .unwrap()
            .into_iter()
            .map(|fk| (fk.table, fk.column))
            .collect();
        let mut stmt = conn
            .prepare(
                "SELECT m.name, c.name FROM sqlite_schema m JOIN pragma_table_info(m.name) c
                 WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%'
                   AND (c.name LIKE '%\\_id' ESCAPE '\\' OR c.name IN ('trigger', 'scope_value'))",
            )
            .unwrap();
        let soft: BTreeSet<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<BTreeSet<(String, String)>>>()
            .unwrap()
            .difference(&fk_columns)
            .cloned()
            .collect();
        let classified: BTreeSet<(String, String)> = SOFT
            .iter()
            .map(|(table, column, _)| (table.to_string(), column.to_string()))
            .chain(
                NOT_PROJECT_REFERENCES
                    .iter()
                    .map(|(table, column)| (table.to_string(), column.to_string())),
            )
            .collect();
        assert_eq!(
            soft, classified,
            "sort every id column without a foreign key into SOFT"
        );

        // A link table ties two records and nothing points at its rows.
        let fks = foreign_keys(conn).unwrap();
        for table in LINKS {
            let required = fks
                .iter()
                .filter(|fk| fk.table == *table && !fk.nullable)
                .count();
            assert_eq!(required, 2, "{table} is not a link table");
            assert!(
                !fks.iter().any(|fk| fk.parent == *table),
                "a row of {table} is pointed at"
            );
        }
    }

    /// The whole lifecycle: a used, retired project is deleted with its
    /// events, nothing of another project changes, the audits still pass, and
    /// the id is free to add again.
    #[test]
    fn delete_removes_the_project_and_frees_its_id() {
        let (mut store, orchestrator) = memory_store();
        store.project_add(&owner(), project("beta")).unwrap();
        let beta = store
            .card_create(
                &owner(),
                None,
                NewCard {
                    project_id: "beta".into(),
                    ..card(CardKind::Ship, "beta's own")
                },
                None,
            )
            .unwrap()
            .value;
        let fleet = store
            .card_create(
                &owner(),
                None,
                NewCard {
                    project_id: FLEET_PROJECT.into(),
                    ..card(CardKind::Ops, "points at alpha")
                },
                None,
            )
            .unwrap()
            .value;

        let as_orchestrator = Ctx::new(Actor::Session(orchestrator.clone()));
        let added = store
            .board_add(
                &Ctx::new(Actor::Board),
                Some("click-1"),
                card(CardKind::Ship, "first"),
            )
            .unwrap()
            .value;
        let alpha = added.card;
        store
            .ask_record(
                &owner(),
                Some("ask-1"),
                NewAsk {
                    project_id: "alpha".into(),
                    session_id: Some(orchestrator.clone()),
                    source: crate::domain::AskSource::Script,
                    text: "a note".into(),
                },
            )
            .unwrap();
        store
            .attention_open(
                &owner(),
                None,
                NewAttention {
                    card: alpha.id.clone(),
                    question: "Which way?".into(),
                    ..NewAttention::default()
                },
            )
            .unwrap();
        let run = store
            .run_start(&as_orchestrator, Some("spawn-1"), builder_run(&alpha.id))
            .unwrap()
            .value;
        store
            .conn()
            .execute(
                "UPDATE cards SET fixes_card_id = ?1 WHERE id = ?2",
                [&alpha.id, &fleet.id],
            )
            .unwrap();

        // The preview lists the live run for the stop that comes first;
        // the delete alone still refuses it.
        let preview = store.project_delete_preview("alpha").unwrap();
        assert_eq!(preview.refused, None);
        let roles: Vec<_> = preview.agents.iter().map(|a| a.role.as_str()).collect();
        assert_eq!(roles, ["orchestrator", "builder"], "{:?}", preview.agents);
        assert_eq!(preview.agents[1].run.as_deref(), Some(run.run.id.as_str()));
        let error = store.project_delete(&owner(), "alpha").unwrap_err();
        assert_eq!(error.code(), "refused", "{error}");

        store
            .run_end(
                &owner(),
                &run.run.id,
                EndRun {
                    outcome: RunOutcome::Abandoned,
                    note: None,
                },
            )
            .unwrap();
        store
            .session_end(&owner(), &run.session.id, "exit")
            .unwrap();
        store.session_end(&owner(), &orchestrator, "exit").unwrap();
        store
            .card_cancel(&owner(), &alpha.id, "not needed")
            .unwrap();
        store.project_retire(&owner(), "alpha").unwrap();

        let beta_events = count(
            &store,
            &format!(
                "SELECT count(*) FROM events WHERE card_id = '{}' OR entity_id = 'beta'",
                beta.id
            ),
        );
        let preview = store.project_delete_preview("alpha").unwrap();
        assert_eq!(preview.refused, None);
        for table in [
            "projects",
            "cards",
            "runs",
            "sessions",
            "asks",
            "attention",
            "events",
        ] {
            assert!(preview.deletion.removed[table] > 0, "{table}: {preview:?}");
        }
        assert_eq!(preview.deletion.unlinked["cards.fixes_card_id"], 1);
        // The preview changed nothing.
        assert_eq!(
            count(&store, "SELECT count(*) FROM projects WHERE id = 'alpha'"),
            1
        );

        let deleted = store.project_delete(&owner(), "alpha").unwrap();
        assert_eq!(deleted.value, preview.deletion);
        assert_eq!(deleted.events.len(), 1);

        assert_eq!(
            count(&store, "SELECT count(*) FROM projects WHERE id = 'alpha'"),
            0
        );
        for table in ["cards", "sessions", "asks"] {
            assert_eq!(
                count(
                    &store,
                    &format!("SELECT count(*) FROM {table} WHERE project_id = 'alpha'")
                ),
                0,
                "{table}"
            );
        }
        assert_eq!(count(&store, "SELECT count(*) FROM runs"), 0);
        assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM events WHERE entity_id = 'alpha'"
            ),
            1,
            "only the tombstone names alpha"
        );
        assert_eq!(
            count(
                &store,
                &format!("SELECT count(*) FROM events WHERE card_id = '{}'", alpha.id)
            ),
            0
        );
        assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM idempotency_keys WHERE key IN ('click-1', 'ask-1', 'spawn-1')"
            ),
            0
        );
        assert_eq!(
            count(
                &store,
                &format!(
                    "SELECT count(*) FROM events WHERE card_id = '{}' OR entity_id = 'beta'",
                    beta.id
                )
            ),
            beta_events,
            "beta's history is untouched"
        );
        let fixes: Option<String> = store
            .conn()
            .query_row(
                "SELECT fixes_card_id FROM cards WHERE id = ?1",
                [&fleet.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fixes, None);
        assert_eq!(
            count(&store, "SELECT count(*) FROM pragma_foreign_key_check"),
            0
        );
        assert!(
            decisions_by_wrong_principal(store.conn())
                .unwrap()
                .is_empty()
        );
        let tombstone: serde_json::Value = store
            .conn()
            .query_row(
                "SELECT payload FROM events WHERE kind = 'project.deleted' AND entity_id = 'alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tombstone["removed"]["projects"], 1);

        // Gone: a second delete finds nothing; the id is free again.
        assert_eq!(
            store.project_delete(&owner(), "alpha").unwrap_err().code(),
            "not_found"
        );
        store.project_add(&owner(), project("alpha")).unwrap();
        let again = store
            .card_create(&owner(), None, card(CardKind::Ship, "again"), None)
            .unwrap()
            .value;
        assert_eq!(
            again.display_id,
            format!("alpha-{}", tombstone["next_card_seq"]),
            "display ids are never reused"
        );
        assert_ne!(again.display_id, alpha.display_id);

        // The tombstone of the first delete survives a second one.
        store.project_delete(&owner(), "alpha").unwrap();
        assert_eq!(
            count(
                &store,
                "SELECT count(*) FROM events WHERE kind = 'project.deleted' AND entity_id = 'alpha'"
            ),
            2
        );

        // A caller that found none of alpha's run branches left starts it at 1.
        assert_eq!(
            store.deleted_card_seq("alpha").unwrap(),
            Some(tombstone["next_card_seq"].as_i64().unwrap() + 1)
        );
        assert_eq!(store.deleted_card_seq("beta").unwrap(), None);
        store
            .project_add_numbered(&owner(), project("alpha"), Numbering::Restart)
            .unwrap();
        let fresh = store
            .card_create(&owner(), None, card(CardKind::Ship, "fresh"), None)
            .unwrap()
            .value;
        assert_eq!(fresh.display_id, "alpha-1");
    }

    /// A dependency between alpha's and beta's cards, either way, is cut with
    /// alpha and reported; beta's card stays.
    #[test]
    fn links_to_another_project_are_cut_not_refused() {
        let (mut store, orchestrator) = memory_store();
        store.project_add(&owner(), project("beta")).unwrap();
        let beta = store
            .card_create(
                &owner(),
                None,
                NewCard {
                    project_id: "beta".into(),
                    ..card(CardKind::Ship, "beta's own")
                },
                None,
            )
            .unwrap()
            .value;
        let alpha = store
            .card_create(&owner(), None, card(CardKind::Ship, "alpha's own"), None)
            .unwrap()
            .value;
        for (card, on) in [(&alpha.id, &beta.id), (&beta.id, &alpha.id)] {
            store
                .conn()
                .execute(
                    "INSERT INTO card_deps (card_id, depends_on_card_id, added_at, added_by)
                     VALUES (?1, ?2, '2026-01-01T00:00:00Z', 'owner')",
                    [card, on],
                )
                .unwrap();
        }
        store.session_end(&owner(), &orchestrator, "exit").unwrap();

        let preview = store.project_delete_preview("alpha").unwrap();
        assert_eq!(preview.refused, None);
        assert_eq!(preview.deletion.removed["card_deps"], 2);
        assert_eq!(preview.deletion.unlinked["card_deps.card_id"], 1);
        assert_eq!(preview.deletion.unlinked["card_deps.depends_on_card_id"], 1);

        store.project_delete(&owner(), "alpha").unwrap();
        assert_eq!(count(&store, "SELECT count(*) FROM card_deps"), 0);
        assert_eq!(
            count(
                &store,
                &format!("SELECT count(*) FROM cards WHERE id = '{}'", beta.id)
            ),
            1
        );
        assert_eq!(
            count(&store, "SELECT count(*) FROM pragma_foreign_key_check"),
            0
        );
    }

    /// An open question on `fleet-1` about the deleted project, or one of its
    /// sessions, is withdrawn with it; one about something else stays.
    #[test]
    fn delete_withdraws_the_open_questions_about_the_project() {
        use crate::ids::HOST_HEALTH_CARD;
        use crate::state::attention::{StaleReason, stale, still_current};

        let (mut store, orchestrator) = memory_store();
        let open = |store: &mut Store, question: &str, subject: Option<String>| {
            store
                .attention_open(
                    &owner(),
                    None,
                    NewAttention {
                        card: HOST_HEALTH_CARD.into(),
                        kind: crate::domain::AttentionKind::Recovery,
                        question: question.into(),
                        subject_session_id: subject,
                        ..NewAttention::default()
                    },
                )
                .unwrap()
                .value
        };
        let about_session = open(&mut store, "silent?", Some(orchestrator.clone()));
        let about_project = store
            .attention_open(
                &owner(),
                None,
                NewAttention {
                    card: HOST_HEALTH_CARD.into(),
                    kind: crate::domain::AttentionKind::Recovery,
                    question: "gate".into(),
                    subject_project_id: Some("alpha".into()),
                    ..NewAttention::default()
                },
            )
            .unwrap()
            .value;
        let unrelated = open(&mut store, "disk?", None);
        assert_eq!(about_session.subject_project_id.as_deref(), Some("alpha"));
        assert!(still_current(store.conn(), &about_session).unwrap());

        store
            .session_end(&owner(), &orchestrator, "stopped")
            .unwrap();
        let why = stale(store.conn(), &about_session).unwrap().unwrap();
        assert_eq!(why.reason, StaleReason::SessionEnded);
        assert!(stale(store.conn(), &unrelated).unwrap().is_none());

        let preview = store.project_delete_preview("alpha").unwrap();
        assert_eq!(preview.deletion.withdrawn_attention, 2);
        let deleted = store.project_delete(&owner(), "alpha").unwrap().value;
        assert_eq!(deleted.withdrawn_attention, 2);

        for id in [&about_session.id, &about_project.id] {
            let item: crate::domain::Attention = get(store.conn(), "attention", id).unwrap();
            assert_eq!(
                item.withdraw_reason.as_deref(),
                Some("project alpha was deleted")
            );
        }
        let kept: crate::domain::Attention = get(store.conn(), "attention", &unrelated.id).unwrap();
        assert!(kept.withdrawn_at.is_none());
        assert!(still_current(store.conn(), &kept).unwrap());
    }

    /// An item about a project that is gone or retired is not current, and
    /// says which and when.
    #[test]
    fn an_item_about_a_gone_project_is_not_current() {
        use crate::ids::HOST_HEALTH_CARD;
        use crate::state::attention::{StaleReason, stale};

        let (mut store, orchestrator) = memory_store();
        store.project_add(&owner(), project("beta")).unwrap();
        let open = |store: &mut Store, project: &str| {
            store
                .attention_open(
                    &owner(),
                    None,
                    NewAttention {
                        card: HOST_HEALTH_CARD.into(),
                        kind: crate::domain::AttentionKind::Recovery,
                        question: format!("about {project}?"),
                        subject_project_id: Some(project.into()),
                        ..NewAttention::default()
                    },
                )
                .unwrap()
                .value
        };
        let retired = open(&mut store, "beta");
        let deleted = open(&mut store, "alpha");
        assert!(stale(store.conn(), &retired).unwrap().is_none());
        // The owner's withdraw is for a question that no longer applies.
        let error = store
            .attention_withdraw_stale(&owner(), &retired.id, "owner")
            .unwrap_err();
        assert!(error.to_string().contains("still applies"), "{error}");

        store.project_retire(&owner(), "beta").unwrap();
        let withdrawn = store
            .attention_withdraw_stale(&owner(), &retired.id, "owner")
            .unwrap()
            .value;
        assert!(withdrawn.withdrawn_at.is_some());
        let why = stale(store.conn(), &retired).unwrap().unwrap();
        assert_eq!(why.reason, StaleReason::ProjectRetired);
        assert!(why.at.is_some());

        // The item is opened but, as a legacy row would be, not withdrawn.
        store
            .session_end(&owner(), &orchestrator, "stopped")
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE attention SET subject_project_id = 'alpha' WHERE id = ?1",
                [&deleted.id],
            )
            .unwrap();
        store.project_delete(&owner(), "alpha").unwrap();
        store
            .conn()
            .execute(
                "UPDATE attention SET withdrawn_at = NULL, withdraw_reason = NULL WHERE id = ?1",
                [&deleted.id],
            )
            .unwrap();
        let item: crate::domain::Attention = get(store.conn(), "attention", &deleted.id).unwrap();
        let why = stale(store.conn(), &item).unwrap().unwrap();
        assert_eq!(why.reason, StaleReason::ProjectDeleted);
        assert_eq!(why.subject.as_deref(), Some("alpha"));
        assert!(why.at.is_some());
    }

    #[test]
    fn delete_is_refused_for_fleet_peers_and_sessions() {
        let (mut store, orchestrator) = memory_store();
        let error = store.project_delete(&owner(), FLEET_PROJECT).unwrap_err();
        assert!(error.to_string().contains("never deleted"), "{error}");
        let preview = store.project_delete_preview(FLEET_PROJECT).unwrap();
        assert!(
            preview
                .refused
                .as_deref()
                .is_some_and(|fact| fact.contains("never deleted"))
        );
        assert!(preview.deletion.removed.is_empty(), "{preview:?}");
        let peer = Ctx::new(Actor::Peer("board-host".into()));
        assert_eq!(
            store.project_delete(&peer, "alpha").unwrap_err().code(),
            "forbidden"
        );
        let session = Ctx::new(Actor::Session(orchestrator));
        assert_eq!(
            store.project_delete(&session, "alpha").unwrap_err().code(),
            "forbidden"
        );
        let error = store.project_delete(&owner(), "alpha").unwrap_err();
        assert!(
            error.to_string().contains("orchestrator session"),
            "{error}"
        );
        assert_eq!(
            store.project_delete(&owner(), "nope").unwrap_err().code(),
            "not_found"
        );
    }
}
