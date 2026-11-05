//! The memory view's writes and reads (`memory.md` sections 6 and 7, T7.2):
//! owner edits, Keep and Discard on agent proposals, Undo, and the two halves
//! of `mem.apply` the daemon runs around its git commit.
//!
//! Nothing here touches the file system. The daemon reads the canonical
//! file (its blob at `HEAD`, its blob in the working tree and its text) into
//! a [`FileState`] under the repo lock and passes it in; every check against
//! the file runs inside the same transaction as the rows it guards.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::approvals::mark_stale;
use super::learning::{self, Gate};
use super::projects::{meta, require_daemon, require_owner};
use super::{find, get, list};
use crate::domain::{Approval, ApprovalKind, ApprovalState, Record};
use crate::error::{Error, Result};
use crate::ids::{self, FLEET_PROJECT};
use crate::memory::{self as doc, EMPTY_BLOB, Entry, STARTUP_BUDGET, document_path};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// The host's memory repo, as `memory_sources` records it (`memory.md` section 3).
pub const MEMORY_REPO: &str = "~/.tiller/memory";

macro_rules! row {
    ($name:ident in $table:literal { $($field:ident: $ty:ty),+ $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            $(pub $field: $ty),+
        }

        impl Record for $name {
            const TABLE: &'static str = $table;
            const FIELDS: &'static [&'static str] = &[$(stringify!($field)),+];

            fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
                Ok(Self { $($field: row.get(stringify!($field))?),+ })
            }
        }
    };
}

row!(Source in "memory_sources" {
    id: String,
    project_id: String,
    boundary: String,
    kind: String,
    canonical_repo: Option<String>,
    canonical_path: Option<String>,
    retired_at: Option<Timestamp>,
});

row!(Revision in "memory_revisions" {
    id: String,
    source_id: String,
    blob_sha: String,
    commit_sha: Option<String>,
    parent_revision_id: Option<String>,
    review: String,
    provenance: String,
    approval_id: Option<String>,
    recorded_at: Timestamp,
    recorded_by: String,
});

row!(Memory in "memories" {
    id: String,
    source_id: String,
    section: Option<String>,
    text_sha256: String,
    class: String,
    review_status: String,
    provenance: String,
    proposal_id: Option<String>,
    created_revision_id: String,
    retired_at: Option<Timestamp>,
    rev: i64,
});

row!(Proposal in "proposals" {
    id: String,
    project_id: String,
    boundary: String,
    target_source_id: String,
    op: String,
    memory_id: Option<String>,
    base_revision_id: String,
    base_blob: String,
    diff: String,
    diff_sha256: String,
    memory_fields: Value,
    rationale: Option<String>,
    expected_effect: Option<String>,
    token_budget_after: Option<Value>,
    authored_by_session_id: Option<String>,
    card_id: Option<String>,
    created_at: Timestamp,
    created_by: String,
    state: String,
    approval_id: Option<String>,
    decided_at: Option<Timestamp>,
    applied_revision_id: Option<String>,
    failure_reason: Option<String>,
    rev: i64,
});

/// The canonical file as the daemon read it under the repo lock.
#[derive(Clone, Debug)]
pub struct FileState {
    /// The file's blob at `HEAD`; [`EMPTY_BLOB`] when absent.
    pub head_blob: String,
    /// The file's blob in the working tree; [`EMPTY_BLOB`] when absent.
    pub work_blob: String,
    /// The text of the `HEAD` blob.
    pub text: String,
    /// Whether the whole text passes the scrub (`memory.md` section 8).
    pub clean: bool,
}

/// One edit in the memory view, against the entry the board showed: entries
/// are named by section and position, and `base_blob` pins the text.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    Add {
        section: String,
        text: String,
    },
    Rewrite {
        section: String,
        index: usize,
        text: String,
    },
    Remove {
        section: String,
        index: usize,
    },
}

/// One observed fact for `mem.note` (`memory.md` sections 6 and 12).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Note {
    /// The project the fact is about. A session or a curator notes for its
    /// own project, which this defaults to; the owner names one.
    pub project: Option<String>,
    /// The fact, kept as the local-only excerpt.
    pub text: String,
    /// The evidence kind; `observation` when not given.
    pub kind: Option<String>,
    /// The JSON pointer to the record; when not given, one naming the
    /// caller's card, run, session or retro.
    pub reference: Option<Value>,
    /// The card it was seen on, when it is not the caller's own.
    pub card: Option<String>,
    /// The memory it contradicts, which comes due for review at once.
    pub contradicts: Option<String>,
}

/// The evidence row `mem.note` wrote. The excerpt stays in `state.db`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Noted {
    pub evidence_id: String,
    pub project_id: String,
    pub kind: String,
    pub card_id: Option<String>,
    pub independence_key: String,
    /// The contradicted memory, now `due`.
    pub due_memory_id: Option<String>,
}

/// One agent proposal for `mem.propose`, on an entry of a Tiller document.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProposeOp {
    /// A new entry at the end of `section`.
    Add { section: String, text: String },
    /// New text for the entry `memory`.
    Rewrite { memory: String, text: String },
    /// The entry `memory` taken out.
    Retire { memory: String },
}

/// `mem.propose`: the change, the evidence rows it stands on and what it
/// should change (`memory.md` section 6).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Propose {
    /// The project whose document it changes, or `fleet`.
    pub project: String,
    pub op: ProposeOp,
    /// Evidence ids or short forms; at least one, except for a retire.
    pub evidence: Vec<String>,
    /// What should change, on which kind of task; required except for a
    /// retire.
    pub expected_effect: Option<String>,
    pub rationale: Option<String>,
}

const EVIDENCE_KINDS: [&str; 9] = [
    "observation",
    "transcript",
    "card",
    "review",
    "test",
    "ci",
    "incident",
    "owner_message",
    "legacy",
];

/// Kinds only the owner (or the importer) records: an agent cannot speak
/// for the owner or for the predecessor system.
const OWNER_KINDS: [&str; 2] = ["owner_message", "legacy"];

/// A change the owner made or approved, waiting for `mem.apply`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub proposal_id: String,
    /// The accepted approval to apply; none for a discard or a re-proposal.
    pub approval_id: Option<String>,
}

/// What `mem.apply` does after its checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "begin", rename_all = "snake_case")]
pub enum Begin {
    /// Commit `diff` to `path` with `message`; the approval is executing.
    Go {
        proposal_id: String,
        path: String,
        diff: String,
        message: String,
    },
    /// The base moved: the approval and the proposal are stale.
    Stale { reason: String },
    /// Already applied.
    Done,
}

/// What the daemon's commit came to.
#[derive(Clone, Debug)]
pub enum Outcome {
    Applied { blob: String, commit: String },
    Failed { reason: String },
}

/// Who may change memory from the memory view: the owner on this host, never
/// a paired board (the memory routes are not on the forwarding allowlist).
fn owner_here(actor: &Actor, what: &str) -> Result<()> {
    require_owner(actor, what)?;
    if let Actor::Peer(label) = actor {
        return Err(Error::Forbidden(format!(
            "{what} is decided on the host that owns the memory; board@{label} may not"
        )));
    }
    Ok(())
}

fn stale_view(path: &str) -> Error {
    Error::refused(
        "memory document",
        path,
        "it changed since the board showed it; reload to see the current text",
    )
}

/// Refuses a change against another text than the one at `HEAD`, or while
/// the working tree holds edits nobody committed: `mem.apply` would only
/// stale it.
fn shown(file: &FileState, base_blob: &str, path: &str) -> Result<()> {
    if base_blob != file.head_blob {
        return Err(stale_view(path));
    }
    committed(file, path)
}

/// The working tree holds what `HEAD` holds, so a change made against
/// `HEAD` is the change that lands.
fn committed(file: &FileState, path: &str) -> Result<()> {
    if file.work_blob != file.head_blob {
        return Err(Error::refused(
            "memory document",
            path,
            "the memory repo has uncommitted edits to it; commit or reset them first",
        ));
    }
    Ok(())
}

fn scope_of(project_id: &str) -> Value {
    if project_id == FLEET_PROJECT {
        json!({"kind": "fleet", "value": Value::Null})
    } else {
        json!({"kind": "project", "value": project_id})
    }
}

fn field<'a>(fields: &'a Value, name: &str) -> Option<&'a str> {
    fields.get(name).and_then(Value::as_str)
}

/// The source row of `project_id`'s Tiller document, if it has one.
fn source_of(conn: &rusqlite::Connection, project_id: &str) -> Result<Option<Source>> {
    find(
        conn,
        "project_id = ?1 AND kind = 'tiller_doc' AND canonical_path = ?2 AND retired_at IS NULL",
        &[&project_id, &document_path(project_id)],
    )
}

/// The source row, created as import does when the project has none.
fn ensure_source(tx: &mut Tx<'_>, project_id: &str) -> Result<Source> {
    if let Some(found) = source_of(tx.conn(), project_id)? {
        return Ok(found);
    }
    let boundary = host_boundary(tx.conn())?;
    let path = document_path(project_id);
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo, canonical_path,
                                     apply_method, load_mode, added_at)
         VALUES (?1, ?2, ?3, 'tiller_doc', ?4, ?5, ?6, 'tiller_commit', 'rendered', ?7)",
        params![id, project_id, boundary, format!("{MEMORY_REPO}/{path}"), MEMORY_REPO, path, tx.now()],
    )?;
    tx.event(
        NewEvent::new("memory_source", &id, "memory_source.added")
            .rev(1)
            .payload(
                json!({"project_id": project_id, "kind": "tiller_doc", "canonical_path": path}),
            ),
    )?;
    get(tx.conn(), "memory source", &id)
}

fn revision_of(
    conn: &rusqlite::Connection,
    source_id: &str,
    blob: &str,
) -> Result<Option<Revision>> {
    find(
        conn,
        "source_id = ?1 AND blob_sha = ?2 AND installed_sha256 IS NULL",
        &[&source_id, &blob],
    )
}

/// The revision row of `blob`, recorded when the file got there outside
/// Tiller: `baseline` for the empty file, `external` otherwise, unreviewed.
fn ensure_revision(tx: &mut Tx<'_>, source_id: &str, file: &FileState) -> Result<Revision> {
    if let Some(found) = revision_of(tx.conn(), source_id, &file.head_blob)? {
        return Ok(found);
    }
    let id = ids::new_ulid();
    let provenance = if file.head_blob == EMPTY_BLOB {
        "baseline"
    } else {
        "external"
    };
    let scrub = if file.clean { "passed" } else { "flagged" };
    tx.conn().execute(
        "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub, recorded_at, recorded_by)
         VALUES (?1, ?2, ?3, 'unreviewed', ?4, ?5, ?6, ?7)",
        params![id, source_id, file.head_blob, provenance, scrub, tx.now(), tx.actor().to_string()],
    )?;
    tx.event(
        NewEvent::new("memory_revision", &id, "memory_revision.recorded").payload(
            json!({"source_id": source_id, "blob_sha": file.head_blob, "provenance": provenance}),
        ),
    )?;
    get(tx.conn(), "memory revision", &id)
}

/// Each entry of the document with the `memories` row it is, matched by
/// section and text hash in row order; `None` for a line written outside
/// Tiller.
fn mapped(
    conn: &rusqlite::Connection,
    source_id: &str,
    sections: &[doc::Section],
) -> Result<Vec<Vec<Option<Memory>>>> {
    let mut rows: Vec<Option<Memory>> = list::<Memory>(
        conn,
        "source_id = ?1 AND retired_at IS NULL ORDER BY id",
        &[&source_id],
    )?
    .into_iter()
    .map(Some)
    .collect();
    Ok(sections
        .iter()
        .map(|section| {
            section
                .entries
                .iter()
                .map(|entry| {
                    let sha = doc::text_sha256(&entry.text);
                    rows.iter_mut()
                        .find(|row| {
                            row.as_ref().is_some_and(|row| {
                                row.text_sha256 == sha
                                    && row.section.as_deref() == Some(section.title.as_str())
                            })
                        })
                        .and_then(Option::take)
                })
                .collect()
        })
        .collect())
}

/// The entry `index` of `section`, with its row. A title the file repeats is
/// one section, numbered across its parts in file order, as the view shows it.
fn entry_at<'a>(
    sections: &'a [doc::Section],
    rows: &[Vec<Option<Memory>>],
    section: &str,
    index: usize,
    path: &str,
) -> Result<(&'a Entry, Option<Memory>)> {
    sections
        .iter()
        .zip(rows)
        .filter(|(s, _)| s.title == section)
        .flat_map(|(s, rows)| s.entries.iter().zip(rows))
        .nth(index)
        .map(|(entry, row)| (entry, row.clone()))
        .ok_or_else(|| stale_view(path))
}

/// The entry a row is, in the current text.
fn entry_of<'a>(
    sections: &'a [doc::Section],
    rows: &[Vec<Option<Memory>>],
    memory_id: &str,
) -> Option<&'a Entry> {
    sections.iter().zip(rows).find_map(|(section, rows)| {
        section
            .entries
            .iter()
            .zip(rows)
            .find(|(_, row)| row.as_ref().is_some_and(|row| row.id == memory_id))
            .map(|(entry, _)| entry)
    })
}

/// A row for a line that was written outside Tiller, so an edit can name it.
fn adopt(
    tx: &mut Tx<'_>,
    source: &Source,
    section: &str,
    entry: &Entry,
    revision: &Revision,
) -> Result<Memory> {
    let id = ids::new_ulid();
    let scope = scope_of(&source.project_id);
    tx.conn().execute(
        "INSERT INTO memories (id, source_id, section, text_sha256, scope_kind, scope_value, class, validity,
                               review_status, provenance, created_revision_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'durable', 'unreviewed', 'external', ?8)",
        params![
            id,
            source.id,
            section,
            doc::text_sha256(&entry.text),
            scope["kind"].as_str(),
            scope["value"].as_str(),
            doc::section_class(section),
            revision.id,
        ],
    )?;
    tx.event(
        NewEvent::new("memory", &id, "memory.recorded")
            .rev(1)
            .payload(json!({"source_id": source.id, "section": section, "provenance": "external"})),
    )?;
    get(tx.conn(), "memory", &id)
}

/// Where an agent's memory write comes from. Empty for the owner.
#[derive(Default)]
struct Origin {
    /// The caller's own project: a session's, or the retro's.
    project: Option<String>,
    card: Option<String>,
    run: Option<String>,
    session: Option<String>,
    retro: Option<String>,
}

/// The caller of `mem.note` or `mem.propose`: a fleet session, a curator
/// on a retro still open, or the owner on this host.
fn origin(conn: &rusqlite::Connection, actor: &Actor, what: &str) -> Result<Origin> {
    match actor {
        Actor::Session(id) => {
            let id = super::resolve_id(conn, "sessions", "session", id)?;
            let session: crate::domain::Session = get(conn, "session", &id)?;
            let card = match &session.run_id {
                Some(run) => Some(get::<crate::domain::Run>(conn, "run", run)?.card_id),
                None => None,
            };
            Ok(Origin {
                project: Some(session.project_id),
                card,
                run: session.run_id,
                session: Some(session.id),
                retro: None,
            })
        }
        Actor::Curator(retro) => {
            let retro: crate::domain::Retro = get(conn, "retro", retro)?;
            if retro.state == crate::domain::RetroState::Done {
                return Err(Error::refused(
                    "retro",
                    &retro.id,
                    "it is done; its curator writes nothing more",
                ));
            }
            Ok(Origin {
                project: Some(retro.project_id),
                card: Some(retro.card_id),
                run: None,
                session: None,
                retro: Some(retro.id),
            })
        }
        Actor::Owner | Actor::Board => Ok(Origin::default()),
        other => Err(Error::Forbidden(format!(
            "{what} is for a fleet role, the curator or the owner; {other} may not"
        ))),
    }
}

pub(super) fn host_boundary(conn: &rusqlite::Connection) -> Result<String> {
    meta(conn, "boundary")?
        .ok_or_else(|| Error::refused("host", "local", "state.db has no boundary; run tiller init"))
}

/// One `evidence` row and its `evidence.captured` event.
pub(super) struct NewEvidence<'a> {
    pub project: &'a str,
    pub boundary: &'a str,
    pub kind: &'a str,
    pub reference: Value,
    pub excerpt: &'a str,
    pub card: Option<&'a str>,
    pub run: Option<&'a str>,
    pub session: Option<&'a str>,
    /// Default: the row's own id, independent of every other row.
    pub independence: Option<&'a str>,
    pub contradicts: Option<&'a str>,
    pub payload: Value,
}

pub(super) fn insert_evidence(tx: &mut Tx<'_>, new: NewEvidence<'_>) -> Result<String> {
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO evidence (id, project_id, boundary, kind, ref, excerpt, card_id, run_id, session_id,
                               independence_key, captured_at, captured_by, contradicts_memory_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            new.project,
            new.boundary,
            new.kind,
            new.reference,
            new.excerpt,
            new.card,
            new.run,
            new.session,
            new.independence.unwrap_or(&id),
            tx.now(),
            tx.actor().to_string(),
            new.contradicts,
        ],
    )?;
    tx.event(
        NewEvent::new("evidence", &id, "evidence.captured")
            .card_opt(new.card.map(str::to_owned))
            .payload(new.payload),
    )?;
    Ok(id)
}

/// The memory a note contradicts: live, on this host's boundary, and in the
/// project's document or the fleet's.
fn contradicted(
    conn: &rusqlite::Connection,
    input: &str,
    project: &str,
    boundary: &str,
) -> Result<Memory> {
    let id = super::resolve_id(conn, "memories", "memory", input)?;
    let memory: Memory = get(conn, "memory", &id)?;
    if memory.retired_at.is_some() {
        return Err(Error::refused("memory", &id, "it is retired"));
    }
    let source: Source = get(conn, "memory source", &memory.source_id)?;
    if source.boundary != boundary {
        return Err(Error::refused(
            "memory",
            &id,
            format!(
                "it is in the {} boundary, not this host's {boundary}",
                source.boundary
            ),
        ));
    }
    if source.project_id != project && source.project_id != FLEET_PROJECT {
        return Err(Error::refused(
            "memory",
            &id,
            format!("it is {}'s memory, not {project}'s", source.project_id),
        ));
    }
    Ok(memory)
}

/// Marks a memory due for review because `evidence` contradicts it
/// (`memory.md` section 5). An expired entry stays expired.
fn mark_due(tx: &mut Tx<'_>, memory: &Memory, evidence: &str) -> Result<bool> {
    let changed = tx.conn().execute(
        "UPDATE memories SET review_status = 'due', due_reason = 'contradicting_evidence',
                review_due_at = ?2, rev = rev + 1
         WHERE id = ?1 AND review_status IN ('approved', 'unreviewed', 'due')",
        params![memory.id, tx.now()],
    )?;
    if changed == 1 {
        tx.event(
            NewEvent::new("memory", &memory.id, "memory.due")
                .rev(memory.rev + 1)
                .payload(json!({"reason": "contradicting_evidence", "evidence_id": evidence})),
        )?;
    }
    Ok(changed == 1)
}

/// The evidence rows a proposal cites, resolved once each: on this host's
/// boundary, not superseded, and about the target project (any project
/// for the fleet document).
fn cited(
    conn: &rusqlite::Connection,
    inputs: &[String],
    project: &str,
    boundary: &str,
) -> Result<Vec<String>> {
    let mut ids: Vec<String> = Vec::new();
    for input in inputs {
        let id = super::resolve_id(conn, "evidence", "evidence", input)?;
        let (of, on, superseded): (String, String, Option<String>) = conn.query_row(
            "SELECT project_id, boundary, superseded_by FROM evidence WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if on != boundary {
            return Err(Error::refused(
                "evidence",
                &id,
                format!("it is in the {on} boundary, not this host's {boundary}"),
            ));
        }
        if let Some(later) = superseded {
            return Err(Error::refused(
                "evidence",
                &id,
                format!("it was superseded by {later}; cite that instead"),
            ));
        }
        if project != FLEET_PROJECT && of != project {
            return Err(Error::refused(
                "evidence",
                &id,
                format!("it is about {of}, not {project}"),
            ));
        }
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// The entry a live memory of `source` is in the current text.
fn in_document<'a>(
    conn: &rusqlite::Connection,
    source: &Source,
    sections: &'a [doc::Section],
    rows: &[Vec<Option<Memory>>],
    input: &str,
    path: &str,
) -> Result<(Memory, &'a Entry)> {
    let id = super::resolve_id(conn, "memories", "memory", input)?;
    let memory: Memory = get(conn, "memory", &id)?;
    if memory.retired_at.is_some() {
        return Err(Error::refused("memory", &id, "it is retired"));
    }
    if memory.source_id != source.id {
        return Err(Error::refused(
            "memory",
            &id,
            format!("it is not an entry of {path}"),
        ));
    }
    let entry = entry_of(sections, rows, &id).ok_or_else(|| {
        Error::refused(
            "memory",
            &id,
            format!("its line is not in the current text of {path}"),
        )
    })?;
    Ok((memory, entry))
}

struct NewProposal<'a> {
    source: &'a Source,
    op: &'a str,
    memory_id: Option<&'a str>,
    base: &'a Revision,
    diff: String,
    fields: Value,
    expected_effect: Option<String>,
    /// The proposal this one copies the author, rationale and evidence of.
    from: Option<&'a Proposal>,
    /// What an agent's new proposal stands on, from `mem.propose`.
    backing: Option<Backing>,
}

/// An agent proposal's reason, author and evidence rows.
struct Backing {
    rationale: Option<String>,
    session: Option<String>,
    card: Option<String>,
    evidence: Vec<String>,
}

fn insert_proposal(tx: &mut Tx<'_>, new: NewProposal<'_>) -> Result<Proposal> {
    let id = ids::new_ulid();
    let actor = tx.actor().to_string();
    let from = new.from;
    let backing = new.backing.unwrap_or(Backing {
        rationale: from.and_then(|p| p.rationale.clone()),
        session: from.and_then(|p| p.authored_by_session_id.clone()),
        card: from.and_then(|p| p.card_id.clone()),
        evidence: Vec::new(),
    });
    tx.conn().execute(
        "INSERT INTO proposals (id, project_id, boundary, target_source_id, op, memory_id, base_revision_id, base_blob,
                                diff, diff_sha256, memory_fields, rationale, expected_effect, token_budget_after,
                                authored_by_session_id, card_id, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
        params![
            id,
            new.source.project_id,
            new.source.boundary,
            new.source.id,
            new.op,
            new.memory_id,
            new.base.id,
            new.base.blob_sha,
            new.diff,
            doc::sha256_hex(new.diff.as_bytes()),
            new.fields,
            backing.rationale,
            new.expected_effect.or_else(|| from.and_then(|p| p.expected_effect.clone())),
            from.and_then(|p| p.token_budget_after.clone()),
            backing.session,
            backing.card,
            tx.now(),
            from.map_or(actor, |p| p.created_by.clone()),
        ],
    )?;
    if let Some(from) = from {
        tx.conn().execute(
            "INSERT INTO proposal_evidence (proposal_id, evidence_id)
             SELECT ?1, evidence_id FROM proposal_evidence WHERE proposal_id = ?2",
            params![id, from.id],
        )?;
    }
    for evidence in &backing.evidence {
        tx.conn().execute(
            "INSERT INTO proposal_evidence (proposal_id, evidence_id) VALUES (?1, ?2)",
            params![id, evidence],
        )?;
    }
    tx.event(
        NewEvent::new("proposal", &id, "proposal.created")
            .rev(1)
            .card_opt(backing.card.clone())
            .payload(json!({
                "op": new.op,
                "memory_id": new.memory_id,
                "from": from.map(|p| &p.id),
                "evidence": backing.evidence,
            })),
    )?;
    get(tx.conn(), "proposal", &id)
}

/// Keep or Discard: the approval row, decided by the owner, and the
/// proposal's state.
fn decide(tx: &mut Tx<'_>, proposal: &Proposal, keep: bool) -> Result<Approval> {
    let id = ids::new_ulid();
    let state = if keep { "accepted" } else { "rejected" };
    let source: Source = get(tx.conn(), "memory source", &proposal.target_source_id)?;
    tx.conn().execute(
        "INSERT INTO approvals (id, kind, project_id, card_id, repo, diff_sha256, base_rev, action, state,
                                requested_at, requested_by, decided_at, decided_by)
         VALUES (?1, 'memory', ?2, ?3, ?4, ?5, ?6, 'apply', ?7, ?8, ?9, ?8, 'owner')",
        params![
            id,
            proposal.project_id,
            proposal.card_id,
            source.canonical_repo,
            proposal.diff_sha256,
            proposal.base_blob,
            state,
            tx.now(),
            tx.actor().to_string(),
        ],
    )?;
    tx.event(
        NewEvent::new("approval", &id, "approval.decided")
            .rev(1)
            .card_opt(proposal.card_id.clone())
            .payload(json!({"kind": "memory", "proposal_id": proposal.id, "accepted": keep, "decided_by": "owner"})),
    )?;
    let to = if keep { "accepted" } else { "discarded" };
    tx.conn().execute(
        "UPDATE proposals SET state = ?2, approval_id = ?3, decided_at = ?4, rev = rev + 1 WHERE id = ?1",
        params![proposal.id, to, id, tx.now()],
    )?;
    tx.event(
        NewEvent::new("proposal", &proposal.id, "proposal.decided")
            .rev(proposal.rev + 1)
            .card_opt(proposal.card_id.clone())
            .payload(json!({"state": to, "approval_id": id})),
    )?;
    get(tx.conn(), "approval", &id)
}

fn set_proposal(
    tx: &mut Tx<'_>,
    proposal: &Proposal,
    state: &str,
    kind: &'static str,
    why: &str,
) -> Result<()> {
    let current: Proposal = get(tx.conn(), "proposal", &proposal.id)?;
    tx.conn().execute(
        "UPDATE proposals SET state = ?2, failure_reason = ?3, rev = rev + 1 WHERE id = ?1",
        params![proposal.id, state, why],
    )?;
    tx.event(
        NewEvent::new("proposal", &proposal.id, kind)
            .rev(current.rev + 1)
            .card_opt(proposal.card_id.clone())
            .payload(json!({"reason": why})),
    )?;
    Ok(())
}

/// The entry text a proposal leaves behind: what it recorded, else the last
/// item its diff adds.
fn intended_text(proposal: &Proposal) -> Option<String> {
    field(&proposal.memory_fields, "text")
        .map(str::to_owned)
        .or_else(|| doc::added_entries(&proposal.diff).pop())
}

fn intended_section(proposal: &Proposal) -> String {
    field(&proposal.memory_fields, "section")
        .unwrap_or(doc::RULE_SECTION)
        .to_owned()
}

/// The inverse of an applied change worked out on the current text, for a
/// commit that does not revert cleanly (a later edit next to its lines):
/// the same line retired, restored or rewritten back, as long as it still
/// reads as the change left it.
#[allow(clippy::too_many_arguments)]
fn inverse_diff(
    conn: &rusqlite::Connection,
    source_id: &str,
    proposal: &Proposal,
    memory: &Memory,
    op: &str,
    fields: &Value,
    path: &str,
    file: &FileState,
) -> Result<String> {
    let conflict = || {
        Error::refused(
            "proposal",
            &proposal.id,
            "undoing it conflicts with a later edit to the same lines; change the line by hand",
        )
    };
    let sections = doc::parse(&file.text);
    let rows = mapped(conn, source_id, &sections)?;
    let intended = intended_text(proposal);
    let current = || {
        entry_of(&sections, &rows, &memory.id)
            .filter(|entry| Some(entry.text.as_str()) == intended.as_deref())
            .ok_or_else(conflict)
    };
    let text = || field(fields, "text").ok_or_else(conflict);
    let new = match op {
        "retire" => doc::remove(&file.text, current()?),
        "rewrite" => doc::rewrite(&file.text, current()?, text()?),
        _ => doc::add(&file.text, &intended_section(proposal), text()?),
    };
    let diff = doc::unified_diff(path, &file.text, &new);
    if diff.is_empty() {
        Err(conflict())
    } else {
        Ok(diff)
    }
}

/// A proposal's intent redone against the current text: the same add,
/// rewrite or removal, as a new diff.
fn rebased_diff(
    proposal: &Proposal,
    path: &str,
    file: &FileState,
    sections: &[doc::Section],
    rows: &[Vec<Option<Memory>>],
) -> Result<(String, Value)> {
    let gone = || {
        Error::refused(
            "proposal",
            &proposal.id,
            "the line it changes is no longer in the document",
        )
    };
    let mut fields = proposal.memory_fields.clone();
    let new = match proposal.op.as_str() {
        "add" if proposal.memory_id.is_none() => {
            let text = intended_text(proposal).ok_or_else(gone)?;
            fields["text"] = json!(text);
            doc::add(&file.text, &intended_section(proposal), &text)
        }
        "rewrite" => {
            let memory_id = proposal.memory_id.as_deref().ok_or_else(gone)?;
            let entry = entry_of(sections, rows, memory_id).ok_or_else(gone)?;
            let text = intended_text(proposal).ok_or_else(gone)?;
            fields["text"] = json!(text);
            doc::rewrite(&file.text, entry, &text)
        }
        "retire" | "remove" => {
            let memory_id = proposal.memory_id.as_deref().ok_or_else(gone)?;
            let entry = entry_of(sections, rows, memory_id).ok_or_else(gone)?;
            doc::remove(&file.text, entry)
        }
        _ => {
            return Err(Error::refused(
                "proposal",
                &proposal.id,
                format!(
                    "a {} proposal is not rebased in the memory view",
                    proposal.op
                ),
            ));
        }
    };
    let diff = doc::unified_diff(path, &file.text, &new);
    if diff.is_empty() {
        return Err(Error::refused(
            "proposal",
            &proposal.id,
            "it would change nothing in the current text",
        ));
    }
    Ok((diff, fields))
}

/// The holdout gate on keeping `proposal` with the text at `head`, as
/// `mem.decide` runs it and the view shows it. A proposal made against
/// another text is stale, since its diff is not the one the holdout ran,
/// unless `after` holds only members of its change set made against the
/// same text: then the run measured it applied after them.
fn keep_gate(
    conn: &rusqlite::Connection,
    proposal: &Proposal,
    head: &str,
    after: &[String],
) -> Result<Gate> {
    let mut gate = learning::gate(conn, &proposal.id)?;
    if proposal.base_blob != head && !measured_after(conn, proposal, &gate, after)? {
        gate.ok = false;
        gate.reason = Some("the text moved; re-base and run the holdout again".into());
        gate.hint = Some(
            "discard it, undo the discard to propose it on the current text, then tiller learn eval --proposal <new>"
                .into(),
        );
    }
    Ok(gate)
}

fn measured_after(
    conn: &rusqlite::Connection,
    proposal: &Proposal,
    gate: &Gate,
    after: &[String],
) -> Result<bool> {
    let Some(set) = gate.change_set.as_deref() else {
        return Ok(false);
    };
    if after.is_empty() {
        return Ok(false);
    }
    for id in after {
        let earlier: Proposal = get(conn, "proposal", id)?;
        let same_set = learning::open_set_of(conn, id)?.is_some_and(|s| s.id == set);
        if !same_set
            || earlier.base_blob != proposal.base_blob
            || earlier.target_source_id != proposal.target_source_id
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Refuses a Keep that would put a role over its startup budget.
fn check_budget(proposal: &Proposal) -> Result<()> {
    let Some(Value::Object(roles)) = &proposal.token_budget_after else {
        return Ok(());
    };
    for (role, tokens) in roles {
        if let Some(tokens) = tokens.as_i64().filter(|t| *t > STARTUP_BUDGET) {
            return Err(Error::refused(
                "proposal",
                &proposal.id,
                format!(
                    "it would put the {role} at {tokens} tokens, over its {STARTUP_BUDGET}-token startup budget"
                ),
            ));
        }
    }
    Ok(())
}

fn undone(conn: &rusqlite::Connection, proposal_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM proposals
                        WHERE (json_extract(memory_fields, '$.undoes') = ?1 AND state IN ('accepted', 'applied'))
                           OR json_extract(memory_fields, '$.reproposed_from') = ?1)",
        [proposal_id],
        |row| row.get(0),
    )?)
}

fn load_approval(tx: &Tx<'_>, id: &str) -> Result<(Approval, Proposal)> {
    let approval: Approval = get(tx.conn(), "approval", id)?;
    if approval.kind != ApprovalKind::Memory {
        return Err(Error::refused(
            "approval",
            id,
            "it is not a memory approval",
        ));
    }
    let proposal: Proposal = find(tx.conn(), "approval_id = ?1", &[&id])?
        .ok_or_else(|| Error::refused("approval", id, "no proposal names it"))?;
    Ok((approval, proposal))
}

impl Store {
    /// An owner edit in the memory view: a proposal and its approval, decided
    /// by the owner in one act (`memory.md` section 7). The daemon applies it.
    pub fn mem_edit(
        &mut self,
        ctx: &Ctx,
        project: &str,
        edit: &EditOp,
        base_blob: &str,
        file: &FileState,
    ) -> Result<Applied<Pending>> {
        owner_here(&ctx.actor, "mem.edit")?;
        self.write(ctx, "mem.edit", None, |tx| {
            let path = document_path(project);
            get::<crate::domain::Project>(tx.conn(), "project", project)?;
            shown(file, base_blob, &path)?;
            let source = ensure_source(tx, project)?;
            let base = ensure_revision(tx, &source.id, file)?;
            let sections = doc::parse(&file.text);
            let rows = mapped(tx.conn(), &source.id, &sections)?;
            let line = |text: &str| {
                let text = doc::one_line(text);
                super::required("text", &text).map(|()| text)
            };
            let (op, memory, new, fields) = match edit {
                EditOp::Add { section, text } => {
                    super::required("section", section)?;
                    let text = line(text)?;
                    let class = doc::section_class(section);
                    let fields = json!({
                        "scope": scope_of(project),
                        "class": class,
                        "validity": "durable",
                        "section": section,
                        "provenance": "owner_edit",
                        "text": text,
                    });
                    ("add", None, doc::add(&file.text, section, &text), fields)
                }
                EditOp::Rewrite {
                    section,
                    index,
                    text,
                } => {
                    let text = line(text)?;
                    let (entry, row) = entry_at(&sections, &rows, section, *index, &path)?;
                    let row = match row {
                        Some(row) => row,
                        None => adopt(tx, &source, section, entry, &base)?,
                    };
                    let fields = json!({
                        "section": section,
                        "provenance": "owner_edit",
                        "text": text,
                        "old_text": entry.text,
                    });
                    (
                        "rewrite",
                        Some(row.id),
                        doc::rewrite(&file.text, entry, &text),
                        fields,
                    )
                }
                EditOp::Remove { section, index } => {
                    let (entry, row) = entry_at(&sections, &rows, section, *index, &path)?;
                    let row = match row {
                        Some(row) => row,
                        None => adopt(tx, &source, section, entry, &base)?,
                    };
                    let fields = json!({
                        "section": section,
                        "provenance": "owner_edit",
                        "old_text": entry.text,
                        "reason": "removed in the memory view",
                    });
                    (
                        "retire",
                        Some(row.id),
                        doc::remove(&file.text, entry),
                        fields,
                    )
                }
            };
            let diff = doc::unified_diff(&path, &file.text, &new);
            if diff.is_empty() {
                return Err(Error::Usage("the line already reads that way".into()));
            }
            let proposal = insert_proposal(
                tx,
                NewProposal {
                    source: &source,
                    op,
                    memory_id: memory.as_deref(),
                    base: &base,
                    diff,
                    fields,
                    expected_effect: Some("Owner edit in the memory view".into()),
                    from: None,
                    backing: None,
                },
            )?;
            let approval = decide(tx, &proposal, true)?;
            Ok(Pending {
                proposal_id: proposal.id,
                approval_id: Some(approval.id),
            })
        })
    }

    /// `mem.note` (`memory.md` section 12): one evidence row. With
    /// `contradicts`, that memory comes due for review in the same act.
    pub fn mem_note(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        note: &Note,
    ) -> Result<Applied<Noted>> {
        self.write(ctx, "mem.note", key, |tx| {
            let from = origin(tx.conn(), tx.actor(), "mem.note")?;
            let text = note.text.trim();
            super::required("text", text)?;
            let kind = note.kind.as_deref().unwrap_or("observation");
            if !EVIDENCE_KINDS.contains(&kind) {
                return Err(Error::Usage(format!(
                    "not an evidence kind: {kind:?} ({})",
                    EVIDENCE_KINDS.join(", ")
                )));
            }
            if !tx.actor().is_owner() && OWNER_KINDS.contains(&kind) {
                return Err(Error::Forbidden(format!(
                    "{kind} evidence is the owner's or the importer's; {} may not record it",
                    tx.actor()
                )));
            }
            let project = match (&from.project, &note.project) {
                (Some(own), Some(asked)) if own != asked => {
                    return Err(Error::Forbidden(format!(
                        "{} notes for {own}, not {asked}",
                        tx.actor()
                    )));
                }
                (Some(own), _) => own.clone(),
                (None, Some(asked)) => asked.clone(),
                (None, None) => {
                    return Err(Error::Usage("name the project the fact is about".into()));
                }
            };
            get::<crate::domain::Project>(tx.conn(), "project", &project)?;
            let card = match &note.card {
                Some(input) => {
                    let card = super::resolve_card(tx.conn(), input)?;
                    if card.project_id != project {
                        return Err(Error::refused(
                            "card",
                            &card.id,
                            format!("it is in {}, not {project}", card.project_id),
                        ));
                    }
                    Some(card.id)
                }
                None => from.card.clone(),
            };
            let boundary = host_boundary(tx.conn())?;
            let due = note
                .contradicts
                .as_deref()
                .map(|input| contradicted(tx.conn(), input, &project, &boundary))
                .transpose()?;
            // The caller's own place in the record, with what it points at
            // under `pointers`: the shape `retro.finish --learning` writes.
            let mut reference = serde_json::Map::new();
            for (name, value) in [
                ("retro", &from.retro),
                ("card", &card),
                ("run", &from.run),
                ("session", &from.session),
            ] {
                if let Some(value) = value {
                    reference.insert(name.into(), json!(value));
                }
            }
            if let Some(pointers) = &note.reference {
                reference.insert("pointers".into(), pointers.clone());
            }
            if reference.is_empty() {
                reference.insert("by".into(), json!(tx.actor().to_string()));
            }
            // An agent's facts are independent by its own card or session,
            // never by a card it names, so one session cannot make one fact
            // look like two.
            let independence = if tx.actor().is_owner() {
                card.clone()
            } else {
                from.card.clone().or_else(|| from.session.clone())
            };
            let id = insert_evidence(
                tx,
                NewEvidence {
                    project: &project,
                    boundary: &boundary,
                    kind,
                    reference: Value::Object(reference),
                    excerpt: text,
                    card: card.as_deref(),
                    run: from.run.as_deref(),
                    session: from.session.as_deref(),
                    independence: independence.as_deref(),
                    contradicts: due.as_ref().map(|m| m.id.as_str()),
                    payload: json!({
                        "kind": kind,
                        "project_id": project,
                        "contradicts": due.as_ref().map(|m| &m.id),
                    }),
                },
            )?;
            let mut due_memory_id = None;
            if let Some(memory) = due
                && mark_due(tx, &memory, &id)?
            {
                due_memory_id = Some(memory.id);
            }
            let independence_key = independence.unwrap_or_else(|| id.clone());
            Ok(Noted {
                evidence_id: id,
                project_id: project,
                kind: kind.to_owned(),
                card_id: card,
                independence_key,
                due_memory_id,
            })
        })
    }

    /// `mem.propose` (`memory.md` section 12): an agent's change to one
    /// entry of a Tiller document, made against the text at `HEAD` and
    /// standing on its evidence rows. It shows in the memory view as a
    /// suggestion; Keep runs the holdout gate (`mem_decide`).
    pub fn mem_propose(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        propose: &Propose,
        file: &FileState,
    ) -> Result<Applied<Proposal>> {
        self.write(ctx, "mem.propose", key, |tx| {
            let from = origin(tx.conn(), tx.actor(), "mem.propose")?;
            let project = propose.project.as_str();
            if let Some(own) = &from.project
                && own != project
                && project != FLEET_PROJECT
            {
                return Err(Error::Forbidden(format!(
                    "{} proposes for {own} or the fleet, not {project}",
                    tx.actor()
                )));
            }
            get::<crate::domain::Project>(tx.conn(), "project", project)?;
            let path = document_path(project);
            committed(file, &path)?;
            let boundary = host_boundary(tx.conn())?;
            let source = ensure_source(tx, project)?;
            if source.boundary != boundary {
                return Err(Error::refused(
                    "memory source",
                    &source.id,
                    format!(
                        "it is in the {} boundary, not this host's {boundary}",
                        source.boundary
                    ),
                ));
            }
            let base = ensure_revision(tx, &source.id, file)?;
            let sections = doc::parse(&file.text);
            let rows = mapped(tx.conn(), &source.id, &sections)?;
            let line = |text: &str| {
                let text = doc::one_line(text);
                super::required("text", &text).map(|()| text)
            };
            let (op, memory, new, fields) = match &propose.op {
                ProposeOp::Add { section, text } => {
                    let section = &doc::one_line(section);
                    super::required("section", section)?;
                    let text = line(text)?;
                    let fields = json!({
                        "scope": scope_of(project),
                        "class": doc::section_class(section),
                        "validity": "durable",
                        "section": section,
                        "provenance": "proposal",
                        "text": text,
                    });
                    ("add", None, doc::add(&file.text, section, &text), fields)
                }
                ProposeOp::Rewrite { memory, text } => {
                    let text = line(text)?;
                    let (row, entry) =
                        in_document(tx.conn(), &source, &sections, &rows, memory, &path)?;
                    let fields = json!({
                        "section": row.section,
                        "provenance": "proposal",
                        "text": text,
                        "old_text": entry.text,
                    });
                    (
                        "rewrite",
                        Some(row.id),
                        doc::rewrite(&file.text, entry, &text),
                        fields,
                    )
                }
                ProposeOp::Retire { memory } => {
                    let (row, entry) =
                        in_document(tx.conn(), &source, &sections, &rows, memory, &path)?;
                    let reason = propose
                        .rationale
                        .as_deref()
                        .map(str::trim)
                        .filter(|r| !r.is_empty())
                        .unwrap_or("retired by proposal");
                    let fields = json!({
                        "section": row.section,
                        "provenance": "proposal",
                        "old_text": entry.text,
                        "reason": reason,
                    });
                    ("retire", Some(row.id), doc::remove(&file.text, entry), fields)
                }
            };
            let evidence = cited(tx.conn(), &propose.evidence, project, &boundary)?;
            if evidence.is_empty() && op != "retire" {
                return Err(Error::Usage(
                    "a proposal cites at least one evidence row (--evidence <id>); record one with tiller mem note"
                        .into(),
                ));
            }
            let expected_effect = propose
                .expected_effect
                .as_deref()
                .map(str::trim)
                .filter(|e| !e.is_empty())
                .map(str::to_owned);
            if expected_effect.is_none() && op != "retire" {
                return Err(Error::Usage(
                    "a proposal states its expected effect (--expected-effect): what should change, on which kind of task"
                        .into(),
                ));
            }
            let diff = doc::unified_diff(&path, &file.text, &new);
            if diff.is_empty() {
                return Err(Error::Usage("the line already reads that way".into()));
            }
            let open: Option<Proposal> = find(
                tx.conn(),
                "target_source_id = ?1 AND state = 'proposed' AND diff_sha256 = ?2",
                &[&source.id, &doc::sha256_hex(diff.as_bytes())],
            )?;
            if let Some(open) = open {
                return Err(Error::refused(
                    "proposal",
                    &open.id,
                    "it already proposes this change",
                ));
            }
            let rationale = propose
                .rationale
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_owned);
            insert_proposal(
                tx,
                NewProposal {
                    source: &source,
                    op,
                    memory_id: memory.as_deref(),
                    base: &base,
                    diff,
                    fields,
                    expected_effect,
                    from: None,
                    backing: Some(Backing {
                        rationale,
                        session: from.session,
                        card: from.card,
                        evidence,
                    }),
                },
            )
        })
    }

    /// `mem.decide` from the memory view: Keep (the approval) or Discard on
    /// one agent proposal. `base_blob` is the blob the board showed it
    /// against. Keep runs the holdout gate first (learning-loop.md, section
    /// 7) and is refused with its reason unless it passes; a proposal made
    /// against an older text is refused as stale. `after` names the
    /// proposals this batch kept earlier on the same document: a member kept
    /// after others of its change set made against the same text is rebased
    /// onto the text they left, the order the holdout run applied them in,
    /// and the old proposal and its approval go `stale`.
    pub fn mem_decide(
        &mut self,
        ctx: &Ctx,
        proposal: &str,
        keep: bool,
        base_blob: &str,
        file: &FileState,
        after: &[String],
    ) -> Result<Applied<Pending>> {
        owner_here(&ctx.actor, "mem.decide")?;
        self.write(ctx, "mem.decide", None, |tx| {
            let proposal: Proposal = get(tx.conn(), "proposal", proposal)?;
            let path = document_path(&proposal.project_id);
            if proposal.state != "proposed" {
                return Err(Error::refused(
                    "proposal",
                    &proposal.id,
                    format!("it is {}, not proposed", proposal.state),
                ));
            }
            shown(file, base_blob, &path)?;
            if !keep {
                decide(tx, &proposal, false)?;
                return Ok(Pending {
                    proposal_id: proposal.id,
                    approval_id: None,
                });
            }
            check_budget(&proposal)?;
            let gate = keep_gate(tx.conn(), &proposal, &file.head_blob, after)?;
            if !gate.ok {
                let reason = gate.reason.unwrap_or_default();
                let fact = match gate.hint {
                    Some(hint) => format!("{reason}; {hint}"),
                    None => reason,
                };
                return Err(Error::refused("proposal", &proposal.id, fact));
            }
            if proposal.base_blob == file.head_blob {
                let approval = decide(tx, &proposal, true)?;
                return Ok(Pending {
                    proposal_id: proposal.id,
                    approval_id: Some(approval.id),
                });
            }
            let source: Source = get(tx.conn(), "memory source", &proposal.target_source_id)?;
            let base = ensure_revision(tx, &source.id, file)?;
            let sections = doc::parse(&file.text);
            let rows = mapped(tx.conn(), &source.id, &sections)?;
            let (diff, mut fields) = rebased_diff(&proposal, &path, file, &sections, &rows)?;
            fields["rebased_from"] = json!(proposal.id);
            let old = decide(tx, &proposal, true)?;
            let rebased = insert_proposal(
                tx,
                NewProposal {
                    source: &source,
                    op: &proposal.op,
                    memory_id: proposal.memory_id.as_deref(),
                    base: &base,
                    diff,
                    fields,
                    expected_effect: None,
                    from: Some(&proposal),
                    backing: None,
                },
            )?;
            let why = format!(
                "rebased onto blob {} as proposal {}",
                file.head_blob, rebased.id
            );
            mark_stale(tx, &old, &why)?;
            set_proposal(tx, &proposal, "stale", "proposal.stale", &why)?;
            let approval = decide(tx, &rebased, true)?;
            Ok(Pending {
                proposal_id: rebased.id,
                approval_id: Some(approval.id),
            })
        })
    }

    /// Undo from the "Recent changes" rail. An applied change gets its
    /// inverse as an owner-approved proposal whose diff is `reverse`, the
    /// commit reverted onto the current text (the daemon builds it with git),
    /// or, when that revert conflicts (`None`), the same line undone on the
    /// current text; a discarded suggestion is proposed again as it was.
    pub fn mem_undo(
        &mut self,
        ctx: &Ctx,
        proposal: &str,
        base_blob: &str,
        file: &FileState,
        reverse: Option<&str>,
    ) -> Result<Applied<Pending>> {
        owner_here(&ctx.actor, "mem.undo")?;
        self.write(ctx, "mem.undo", None, |tx| {
            let proposal: Proposal = get(tx.conn(), "proposal", proposal)?;
            let path = document_path(&proposal.project_id);
            shown(file, base_blob, &path)?;
            if undone(tx.conn(), &proposal.id)? {
                return Err(Error::refused("proposal", &proposal.id, "it was already undone"));
            }
            let source: Source = get(tx.conn(), "memory source", &proposal.target_source_id)?;
            let base = ensure_revision(tx, &source.id, file)?;
            match proposal.state.as_str() {
                "discarded" => {
                    let (diff, mut fields) = if proposal.base_blob == file.head_blob {
                        (proposal.diff.clone(), proposal.memory_fields.clone())
                    } else {
                        let sections = doc::parse(&file.text);
                        let rows = mapped(tx.conn(), &source.id, &sections)?;
                        rebased_diff(&proposal, &path, file, &sections, &rows)?
                    };
                    fields["reproposed_from"] = json!(proposal.id);
                    let again = insert_proposal(
                        tx,
                        NewProposal {
                            source: &source,
                            op: &proposal.op,
                            memory_id: proposal.memory_id.as_deref(),
                            base: &base,
                            diff,
                            fields,
                            expected_effect: None,
                            from: Some(&proposal),
                            backing: None,
                        },
                    )?;
                    Ok(Pending {
                        proposal_id: again.id,
                        approval_id: None,
                    })
                }
                "applied" => {
                    let reverse = reverse.filter(|d| !d.is_empty());
                    let approval: Approval = get(
                        tx.conn(),
                        "approval",
                        proposal.approval_id.as_deref().unwrap_or_default(),
                    )?;
                    let section = intended_section(&proposal);
                    let restored: Option<Memory> = match proposal.memory_id.as_deref() {
                        Some(id) => Some(get(tx.conn(), "memory", id)?),
                        None => find(tx.conn(), "proposal_id = ?1", &[&proposal.id])?,
                    };
                    let memory = restored.ok_or_else(|| {
                        Error::refused("proposal", &proposal.id, "the line it added has no memory row")
                    })?;
                    let (op, fields) = match (proposal.op.as_str(), memory.retired_at.is_some()) {
                        ("add", false) | ("rewrite", true) => (
                            "retire",
                            json!({"section": section, "old_text": intended_text(&proposal),
                                   "reason": "undone in the memory view", "undoes": proposal.id}),
                        ),
                        ("retire" | "remove", true) | ("add", true) => (
                            "add",
                            json!({"section": section, "text": field(&proposal.memory_fields, "old_text"),
                                   "undoes": proposal.id}),
                        ),
                        ("rewrite", false) => (
                            "rewrite",
                            json!({
                                "section": section,
                                "text": field(&proposal.memory_fields, "old_text")
                                    .map(str::to_owned)
                                    .or_else(|| reverse.and_then(|d| doc::added_entries(d).pop())),
                                "text_sha256": approval.result.as_ref()
                                    .and_then(|r| r.get("previous_text_sha256")).cloned(),
                                "old_text": intended_text(&proposal),
                                "undoes": proposal.id,
                            }),
                        ),
                        _ => {
                            return Err(Error::refused(
                                "proposal",
                                &proposal.id,
                                format!("a {} change is not undone in the memory view", proposal.op),
                            ));
                        }
                    };
                    let diff = match reverse {
                        Some(diff) => diff.to_owned(),
                        None => inverse_diff(tx.conn(), &source.id, &proposal, &memory, op, &fields, &path, file)?,
                    };
                    let inverse = insert_proposal(
                        tx,
                        NewProposal {
                            source: &source,
                            op,
                            memory_id: Some(&memory.id),
                            base: &base,
                            diff,
                            fields,
                            expected_effect: Some(format!("Undo of proposal {}", proposal.id)),
                            from: None,
                            backing: None,
                        },
                    )?;
                    let approval = decide(tx, &inverse, true)?;
                    Ok(Pending {
                        proposal_id: inverse.id,
                        approval_id: Some(approval.id),
                    })
                }
                other => Err(Error::refused(
                    "proposal",
                    &proposal.id,
                    format!("it is {other}; only an applied or discarded change is undone"),
                )),
            }
        })
    }

    /// `mem.apply`, first half (daemon only, under the repo lock): the
    /// approval's binding against the file as it is now. Both blobs must be
    /// the approved base and the diff its approved hash, or the approval and
    /// the proposal go `stale`.
    pub fn mem_apply_begin(
        &mut self,
        ctx: &Ctx,
        approval: &str,
        file: &FileState,
    ) -> Result<Applied<Begin>> {
        require_daemon(&ctx.actor, "mem.apply")?;
        self.write(ctx, "mem.apply", None, |tx| {
            let (approval, proposal) = load_approval(tx, approval)?;
            match approval.state {
                ApprovalState::Succeeded => return Ok(Begin::Done),
                ApprovalState::Accepted => {}
                other => {
                    return Err(Error::refused("approval", &approval.id, format!("it is {other}, not accepted")));
                }
            }
            let source: Source = get(tx.conn(), "memory source", &proposal.target_source_id)?;
            let path = document_path(&proposal.project_id);
            if source.kind != "tiller_doc"
                || source.canonical_repo.as_deref() != Some(MEMORY_REPO)
                || source.canonical_path.as_deref() != Some(path.as_str())
            {
                return Err(Error::refused(
                    "memory source",
                    &source.id,
                    format!("the memory view writes only {MEMORY_REPO}/{path}"),
                ));
            }
            let base = approval.base_rev.as_deref().unwrap_or_default();
            let why = if file.head_blob != base {
                Some(format!("{path} at HEAD is blob {}, not the approved {base}", file.head_blob))
            } else if file.work_blob != base {
                Some(format!("{path} in the working tree is blob {}, not the approved {base}", file.work_blob))
            } else if Some(doc::sha256_hex(proposal.diff.as_bytes())).as_deref() != approval.diff_sha256.as_deref() {
                Some("the diff is not the one approved".to_owned())
            } else {
                None
            };
            if let Some(why) = why {
                mark_stale(tx, &approval, &why)?;
                set_proposal(tx, &proposal, "stale", "proposal.stale", &why)?;
                return Ok(Begin::Stale { reason: why });
            }
            tx.conn().execute(
                "UPDATE approvals SET state = 'executing', executed_at = ?2, rev = rev + 1 WHERE id = ?1",
                params![approval.id, tx.now()],
            )?;
            tx.event(
                NewEvent::new("approval", &approval.id, "approval.executing")
                    .rev(approval.rev + 1)
                    .card_opt(approval.card_id.clone())
                    .payload(json!({"proposal_id": proposal.id, "base_rev": base})),
            )?;
            let section = intended_section(&proposal);
            let what = match proposal.op.as_str() {
                "add" if proposal.memory_id.is_some() => format!("restore a line in {section}"),
                "add" => format!("add a line to {section}"),
                "rewrite" => format!("edit a line in {section}"),
                _ => format!("remove a line from {section}"),
            };
            let scope = if proposal.project_id == FLEET_PROJECT { "fleet" } else { proposal.project_id.as_str() };
            Ok(Begin::Go {
                proposal_id: proposal.id.clone(),
                path,
                diff: proposal.diff.clone(),
                message: format!(
                    "memory({scope}): {what}\n\nTiller-Proposal: {}\nTiller-Approval: {}\n",
                    proposal.id, approval.id
                ),
            })
        })
    }

    /// `mem.apply`, second half (daemon only, still under the lock): the new
    /// revision, the `memories` rows, the approval `succeeded` and the
    /// proposal `applied`, all in one transaction; or both `failed`.
    pub fn mem_apply_finish(
        &mut self,
        ctx: &Ctx,
        approval: &str,
        outcome: &Outcome,
    ) -> Result<Applied<Approval>> {
        require_daemon(&ctx.actor, "mem.apply")?;
        self.write(ctx, "mem.apply.finish", None, |tx| {
            let (approval, proposal) = load_approval(tx, approval)?;
            if approval.state == ApprovalState::Succeeded {
                return Ok(approval);
            }
            if approval.state != ApprovalState::Executing {
                return Err(Error::refused("approval", &approval.id, format!("it is {}, not executing", approval.state)));
            }
            let (blob, commit) = match outcome {
                Outcome::Failed { reason } => {
                    let result = json!({"error": reason});
                    finish(tx, &approval, "failed", &result)?;
                    set_proposal(tx, &proposal, "failed", "proposal.failed", reason)?;
                    return get(tx.conn(), "approval", &approval.id);
                }
                Outcome::Applied { blob, commit } => (blob, commit),
            };
            let parent = revision_of(tx.conn(), &proposal.target_source_id, approval.base_rev.as_deref().unwrap_or_default())?;
            let revision = record_revision(tx, &proposal, &approval, blob, commit, parent.map(|p| p.id))?;
            let previous = apply_rows(tx, &proposal, &revision)?;
            let result = json!({"commit": commit, "blob": blob, "revision_id": revision, "previous_text_sha256": previous});
            finish(tx, &approval, "succeeded", &result)?;
            let current: Proposal = get(tx.conn(), "proposal", &proposal.id)?;
            tx.conn().execute(
                "UPDATE proposals SET state = 'applied', applied_revision_id = ?2, rev = rev + 1 WHERE id = ?1",
                params![proposal.id, revision],
            )?;
            tx.event(
                NewEvent::new("proposal", &proposal.id, "memory.applied")
                    .rev(current.rev + 1)
                    .card_opt(proposal.card_id.clone())
                    .payload(json!({"revision_id": revision, "commit": commit, "blob": blob, "op": proposal.op})),
            )?;
            get(tx.conn(), "approval", &approval.id)
        })
    }
}

fn finish(tx: &mut Tx<'_>, approval: &Approval, state: &str, result: &Value) -> Result<()> {
    tx.conn().execute(
        "UPDATE approvals SET state = ?2, finished_at = ?3, result = ?4, rev = rev + 1 WHERE id = ?1",
        params![approval.id, state, tx.now(), result],
    )?;
    tx.event(
        NewEvent::new("approval", &approval.id, "approval.finished")
            .rev(approval.rev + 2)
            .card_opt(approval.card_id.clone())
            .payload(json!({"state": state, "result": result})),
    )?;
    Ok(())
}

/// The approved revision of the new blob. A blob the source had before (an
/// undo back to an earlier text) is recorded again on its row: revisions are
/// keyed by content, and the row now names this commit and approval.
fn record_revision(
    tx: &mut Tx<'_>,
    proposal: &Proposal,
    approval: &Approval,
    blob: &str,
    commit: &str,
    parent: Option<String>,
) -> Result<String> {
    let by = tx.actor().to_string();
    let existing = revision_of(tx.conn(), &proposal.target_source_id, blob)?;
    let (id, again) = match existing {
        Some(found) => {
            tx.conn().execute(
                "UPDATE memory_revisions SET commit_sha = ?2, parent_revision_id = ?3, review = 'approved',
                        provenance = 'tiller', approval_id = ?4, scrub = 'passed', recorded_at = ?5, recorded_by = ?6
                 WHERE id = ?1",
                params![found.id, commit, parent.filter(|p| *p != found.id), approval.id, tx.now(), by],
            )?;
            (found.id, true)
        }
        None => {
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO memory_revisions (id, source_id, blob_sha, commit_sha, parent_revision_id, review, provenance,
                                               approval_id, scrub, recorded_at, recorded_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'approved', 'tiller', ?6, 'passed', ?7, ?8)",
                params![id, proposal.target_source_id, blob, commit, parent, approval.id, tx.now(), by],
            )?;
            (id, false)
        }
    };
    tx.event(
        NewEvent::new("memory_revision", &id, "memory_revision.recorded").payload(json!({
            "source_id": proposal.target_source_id,
            "blob_sha": blob,
            "commit_sha": commit,
            "provenance": "tiller",
            "approval_id": approval.id,
            "recorded_again": again,
        })),
    )?;
    Ok(id)
}

/// The `memories` rows an applied proposal changes. Returns the text hash a
/// rewrite replaced, for its Undo.
fn apply_rows(tx: &mut Tx<'_>, proposal: &Proposal, revision: &str) -> Result<Option<String>> {
    let now = tx.now();
    let fields = &proposal.memory_fields;
    let section = intended_section(proposal);
    let class = field(fields, "class")
        .unwrap_or(doc::section_class(&section))
        .to_owned();
    let due = now.plus_minutes(doc::review_days(&class) * 24 * 60);
    let sha = || {
        field(fields, "text_sha256")
            .map(str::to_owned)
            .or_else(|| intended_text(proposal).map(|t| doc::text_sha256(&t)))
            .ok_or_else(|| Error::refused("proposal", &proposal.id, "it names no entry text"))
    };
    let memory: Option<Memory> = proposal
        .memory_id
        .as_deref()
        .map(|id| get(tx.conn(), "memory", id))
        .transpose()?;
    match (proposal.op.as_str(), memory) {
        ("add", None) => {
            let id = ids::new_ulid();
            let scope = fields
                .get("scope")
                .cloned()
                .unwrap_or_else(|| scope_of(&proposal.project_id));
            let provenance = field(fields, "provenance")
                .filter(|p| matches!(*p, "owner_edit" | "legacy-import" | "external" | "baseline"))
                .unwrap_or("proposal");
            tx.conn().execute(
                "INSERT INTO memories (id, source_id, section, text_sha256, legacy_id, scope_kind, scope_value, class,
                                       validity, review_due_at, review_status, provenance, rationale,
                                       authored_by_session_id, proposal_id, created_revision_id, reviewed_at, reviewed_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'durable', ?9, 'approved', ?10, ?11, ?12, ?13, ?14, ?15, 'owner')",
                params![
                    id,
                    proposal.target_source_id,
                    section,
                    sha()?,
                    field(fields, "legacy_id"),
                    scope.get("kind").and_then(Value::as_str).unwrap_or("project"),
                    scope.get("value").and_then(Value::as_str),
                    class,
                    due,
                    provenance,
                    proposal.rationale,
                    proposal.authored_by_session_id,
                    proposal.id,
                    revision,
                    now,
                ],
            )?;
            tx.conn().execute(
                "INSERT INTO memory_evidence (memory_id, evidence_id)
                 SELECT ?1, evidence_id FROM proposal_evidence WHERE proposal_id = ?2",
                params![id, proposal.id],
            )?;
            tx.event(
                NewEvent::new("memory", &id, "memory.recorded")
                    .rev(1)
                    .card_opt(proposal.card_id.clone())
                    .payload(json!({"proposal_id": proposal.id, "section": section, "provenance": provenance})),
            )?;
            Ok(None)
        }
        ("add", Some(memory)) => {
            tx.conn().execute(
                "UPDATE memories SET retired_at = NULL, retired_revision_id = NULL, retire_reason = NULL,
                        review_status = 'approved', review_due_at = ?2, due_reason = NULL, reviewed_at = ?3,
                        reviewed_by = 'owner', rev = rev + 1
                 WHERE id = ?1",
                params![memory.id, due, now],
            )?;
            tx.event(
                NewEvent::new("memory", &memory.id, "memory.restored")
                    .rev(memory.rev + 1)
                    .payload(json!({"proposal_id": proposal.id, "revision_id": revision})),
            )?;
            Ok(None)
        }
        ("rewrite", Some(memory)) => {
            tx.conn().execute(
                "UPDATE memories SET text_sha256 = ?2, review_status = 'approved', review_due_at = ?3,
                        due_reason = NULL, reviewed_at = ?4, reviewed_by = 'owner', rev = rev + 1
                 WHERE id = ?1",
                params![memory.id, sha()?, due, now],
            )?;
            tx.conn().execute(
                "INSERT OR IGNORE INTO memory_evidence (memory_id, evidence_id)
                 SELECT ?1, evidence_id FROM proposal_evidence WHERE proposal_id = ?2",
                params![memory.id, proposal.id],
            )?;
            tx.event(
                NewEvent::new("memory", &memory.id, "memory.rewritten")
                    .rev(memory.rev + 1)
                    .payload(json!({"proposal_id": proposal.id, "revision_id": revision})),
            )?;
            Ok(Some(memory.text_sha256))
        }
        ("retire" | "remove", Some(memory)) => {
            let reason = field(fields, "reason").unwrap_or("retired in the memory view");
            tx.conn().execute(
                "UPDATE memories SET retired_at = ?2, retired_revision_id = ?3, retire_reason = ?4,
                        review_status = 'retired', due_reason = NULL, rev = rev + 1
                 WHERE id = ?1",
                params![memory.id, now, revision, reason],
            )?;
            tx.event(
                NewEvent::new("memory", &memory.id, "memory.retired")
                    .rev(memory.rev + 1)
                    .payload(json!({"proposal_id": proposal.id, "revision_id": revision, "reason": reason})),
            )?;
            Ok(None)
        }
        (op, _) => Err(Error::refused(
            "proposal",
            &proposal.id,
            format!("a {op} proposal is not applied by the memory view"),
        )),
    }
}

// -- reads ----------------------------------------------------------------------------

/// One document in the memory view's list.
#[derive(Clone, Debug, Serialize)]
pub struct DocSummary {
    pub project_id: String,
    pub name: String,
    pub lines: usize,
    pub suggestions: usize,
}

/// Where a line came from: the revision that last changed it, and the card
/// or note and date the hint shows.
#[derive(Clone, Debug, Serialize)]
pub struct Provenance {
    pub revision_id: String,
    pub card: Option<String>,
    pub note: Option<String>,
    pub at: Timestamp,
}

/// An agent proposal shown inline.
#[derive(Clone, Debug, Serialize)]
pub struct Suggestion {
    pub proposal_id: String,
    pub op: String,
    /// The new text, for an add or a rewrite.
    pub text: Option<String>,
    pub by: String,
    pub card: Option<String>,
    pub at: Timestamp,
    pub why: Option<String>,
    pub token_budget_after: Option<Value>,
    /// The holdout gate's verdict on keeping it; Keep waits until it passes.
    pub gate: Gate,
    /// The current text of the line a rewrite or removal names, while the
    /// file has it.
    pub target: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Line {
    pub index: usize,
    pub text: String,
    pub memory_id: Option<String>,
    pub provenance: Option<Provenance>,
    /// A rewrite or removal of this line.
    pub suggestion: Option<Suggestion>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DocSection {
    pub title: String,
    pub class: String,
    pub lines: Vec<Line>,
    /// New lines agents suggest for this section, and suggestions whose line
    /// is gone.
    pub suggestions: Vec<Suggestion>,
}

/// One entry in "Recent changes".
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub proposal_id: String,
    /// `added`, `kept`, `edit`, `remove` or `discard`.
    pub kind: String,
    pub text: Option<String>,
    pub from: String,
    pub at: Timestamp,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryDoc {
    pub project_id: String,
    pub name: String,
    pub path: String,
    pub blob: String,
    /// The working tree differs from `HEAD`: edits go stale until it is
    /// committed or reset.
    pub dirty: bool,
    pub sections: Vec<DocSection>,
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MemoryView {
    pub docs: Vec<DocSummary>,
    pub doc: MemoryDoc,
}

/// Which sessions loaded a revision, for the provenance hint.
#[derive(Clone, Debug, Serialize)]
pub struct LoadedBy {
    pub session_id: String,
    pub role: String,
    pub project_id: String,
    pub run_id: Option<String>,
    pub card: Option<String>,
    pub trigger: String,
    pub loaded_at: Timestamp,
}

#[derive(Clone, Debug, Serialize)]
pub struct RevisionDetail {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub blob_sha: String,
    pub commit_sha: Option<String>,
    pub review: String,
    pub provenance: String,
    pub recorded_at: Timestamp,
    pub approval: Option<Approval>,
    pub proposal_id: Option<String>,
    pub card: Option<String>,
    pub sessions: Vec<LoadedBy>,
}

/// Which open proposals `tiller mem approve` takes in bulk. A field left
/// `None` matches everything; the command refuses a filter with none set.
#[derive(Clone, Debug, Default)]
pub struct PendingFilter {
    pub class: Option<String>,
    pub boundary: Option<String>,
    pub project: Option<String>,
}

impl PendingFilter {
    pub fn is_empty(&self) -> bool {
        self.class.is_none() && self.boundary.is_none() && self.project.is_none()
    }
}

/// The line a proposal adds or rewrites to, when it names one.
pub fn proposal_text(proposal: &Proposal) -> Option<String> {
    intended_text(proposal)
}

/// The class a proposal's entry has: the one it carries, else the one its
/// section implies, as `apply_rows` decides it.
pub fn proposal_class(proposal: &Proposal) -> String {
    field(&proposal.memory_fields, "class")
        .unwrap_or_else(|| doc::section_class(&intended_section(proposal)))
        .to_owned()
}

/// What the daemon needs to undo a change: its commit.
#[derive(Clone, Debug, Serialize)]
pub struct ChangeRef {
    pub proposal_id: String,
    pub project_id: String,
    pub state: String,
    pub commit: Option<String>,
}

fn display_id(conn: &rusqlite::Connection, card: Option<&str>) -> Result<Option<String>> {
    let Some(card) = card else { return Ok(None) };
    Ok(conn
        .query_row(
            "SELECT display_id FROM cards WHERE id = ?1",
            [card],
            |row| row.get(0),
        )
        .optional()?)
}

/// Who a proposal reads as coming from: "you" for the owner, "the curator",
/// or "the <project> <role>" for a fleet session.
fn author(conn: &rusqlite::Connection, proposal: &Proposal) -> Result<String> {
    match field(&proposal.memory_fields, "provenance") {
        Some("owner_edit") => return Ok("you".to_owned()),
        Some("legacy-import") => return Ok("carried over".to_owned()),
        _ => {}
    }
    if proposal.created_by == "owner" || proposal.created_by == "board" {
        return Ok("you".to_owned());
    }
    if proposal.created_by.starts_with("curator:") {
        return Ok("the curator".to_owned());
    }
    if let Some(id) = &proposal.authored_by_session_id
        && let Some(session) = find::<crate::domain::Session>(conn, "id = ?1", &[id])?
    {
        return Ok(format!(
            "the {} {}",
            session.project_id,
            session.role.as_str()
        ));
    }
    Ok(proposal.created_by.clone())
}

fn suggestion(conn: &rusqlite::Connection, proposal: &Proposal, head: &str) -> Result<Suggestion> {
    Ok(Suggestion {
        proposal_id: proposal.id.clone(),
        op: proposal.op.clone(),
        text: match proposal.op.as_str() {
            "add" | "rewrite" => intended_text(proposal),
            _ => None,
        },
        by: author(conn, proposal)?,
        card: display_id(conn, proposal.card_id.as_deref())?,
        at: proposal.created_at,
        why: proposal
            .rationale
            .clone()
            .or_else(|| proposal.expected_effect.clone()),
        token_budget_after: proposal.token_budget_after.clone(),
        gate: keep_gate(conn, proposal, head, &[])?,
        target: None,
    })
}

fn provenance(conn: &rusqlite::Connection, memory: &Memory) -> Result<Provenance> {
    let last: Option<Proposal> = find(
        conn,
        "state = 'applied' AND (memory_id = ?1 OR id = ?2) ORDER BY decided_at DESC, id DESC",
        &[&memory.id, &memory.proposal_id],
    )?;
    if let Some(last) = last {
        let revision: Revision = get(
            conn,
            "memory revision",
            last.applied_revision_id.as_deref().unwrap_or_default(),
        )?;
        let card = display_id(conn, last.card_id.as_deref())?;
        return Ok(Provenance {
            revision_id: revision.id,
            note: match card {
                Some(_) => None,
                None => Some(author(conn, &last)?),
            },
            card,
            at: last.decided_at.unwrap_or(last.created_at),
        });
    }
    let revision: Revision = get(conn, "memory revision", &memory.created_revision_id)?;
    Ok(Provenance {
        revision_id: revision.id,
        card: None,
        note: Some(match memory.provenance.as_str() {
            "legacy-import" => "carried over".to_owned(),
            "external" | "baseline" => "outside Tiller".to_owned(),
            _ => "you".to_owned(),
        }),
        at: revision.recorded_at,
    })
}

fn changes(conn: &rusqlite::Connection, source_id: &str) -> Result<Vec<Change>> {
    let done: Vec<Proposal> = list(
        conn,
        "target_source_id = ?1 AND state IN ('applied', 'discarded')
           AND json_extract(memory_fields, '$.undoes') IS NULL
         ORDER BY decided_at DESC, id DESC LIMIT 40",
        &[&source_id],
    )?;
    let mut out = Vec::new();
    for proposal in done {
        if out.len() == 5 {
            break;
        }
        if undone(conn, &proposal.id)? {
            continue;
        }
        let by = author(conn, &proposal)?;
        let owner = by == "you";
        let kind = match (proposal.state.as_str(), proposal.op.as_str()) {
            ("discarded", _) => "discard",
            (_, "add") if owner => "added",
            (_, "add") => "kept",
            (_, "rewrite") => "edit",
            _ => "remove",
        };
        let text = match kind {
            "remove" => field(&proposal.memory_fields, "old_text").map(str::to_owned),
            _ => intended_text(&proposal),
        };
        let card = display_id(conn, proposal.card_id.as_deref())?;
        out.push(Change {
            proposal_id: proposal.id.clone(),
            kind: kind.to_owned(),
            text,
            from: card.unwrap_or(by),
            at: proposal.decided_at.unwrap_or(proposal.created_at),
        });
    }
    Ok(out)
}

fn open_proposals(conn: &rusqlite::Connection, source_id: &str) -> Result<Vec<Proposal>> {
    list(
        conn,
        "target_source_id = ?1 AND state = 'proposed' ORDER BY created_at, id",
        &[&source_id],
    )
}

impl Store {
    /// `GET /api/v1/memory`: the document list, fleet first, and the
    /// selected document with its lines, inline suggestions and recent
    /// changes. `read` gives the canonical file of a path, and `texts` the
    /// text at `HEAD` of each path, for the list.
    pub fn memory_view(
        &self,
        project: &str,
        read: impl Fn(&str) -> Result<FileState>,
        texts: impl Fn(&[String]) -> Result<Vec<String>>,
    ) -> Result<MemoryView> {
        let conn = self.conn();
        let projects: Vec<(String, String)> = conn
            .prepare(
                "SELECT id, name FROM projects WHERE retired_at IS NULL
                 ORDER BY id <> ?1, name COLLATE NOCASE, id",
            )?
            .query_map([FLEET_PROJECT], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        if !projects.iter().any(|(id, _)| id == project) {
            return Err(Error::not_found("project", project));
        }
        let paths: Vec<String> = projects.iter().map(|(id, _)| document_path(id)).collect();
        let mut docs = Vec::new();
        for ((id, name), text) in projects.iter().zip(texts(&paths)?) {
            let open = match source_of(conn, id)? {
                Some(source) => open_proposals(conn, &source.id)?.len(),
                None => 0,
            };
            docs.push(DocSummary {
                project_id: id.clone(),
                name: name.clone(),
                lines: doc::parse(&text).iter().map(|s| s.entries.len()).sum(),
                suggestions: open,
            });
        }
        let Some((project_id, name)) = projects.into_iter().find(|(id, _)| id == project) else {
            return Err(Error::not_found("project", project));
        };
        let path = document_path(&project_id);
        let file = read(&path)?;
        let parsed = doc::parse(&file.text);
        let source = source_of(conn, &project_id)?;
        let open = match &source {
            Some(source) => open_proposals(conn, &source.id)?,
            None => Vec::new(),
        };
        let rows = match &source {
            Some(source) => mapped(conn, &source.id, &parsed)?,
            None => parsed.iter().map(|s| vec![None; s.entries.len()]).collect(),
        };
        let mut titles: Vec<String> = doc::SECTIONS.iter().map(|(t, _)| (*t).to_owned()).collect();
        for section in &parsed {
            if !titles.contains(&section.title) {
                titles.push(section.title.clone());
            }
        }
        let mut sections: Vec<DocSection> = titles
            .iter()
            .map(|title| DocSection {
                title: title.clone(),
                class: doc::section_class(title).to_owned(),
                lines: Vec::new(),
                suggestions: Vec::new(),
            })
            .collect();
        for (section, rows) in parsed.iter().zip(&rows) {
            let Some(out) = sections.iter_mut().find(|s| s.title == section.title) else {
                continue;
            };
            for (entry, row) in section.entries.iter().zip(rows) {
                out.lines.push(Line {
                    index: out.lines.len(),
                    text: entry.text.clone(),
                    memory_id: row.as_ref().map(|r| r.id.clone()),
                    provenance: row.as_ref().map(|r| provenance(conn, r)).transpose()?,
                    suggestion: None,
                });
            }
        }
        for proposal in &open {
            let mut shown = suggestion(conn, proposal, &file.head_blob)?;
            let target = proposal
                .memory_id
                .as_deref()
                .filter(|_| proposal.op != "add");
            shown.target = target.and_then(|id| {
                sections
                    .iter()
                    .flat_map(|s| &s.lines)
                    .find(|l| l.memory_id.as_deref() == Some(id))
                    .map(|l| l.text.clone())
            });
            // A second suggestion on a line that has one is listed under its
            // section, still showing the line it names.
            let line = target.and_then(|id| {
                sections
                    .iter_mut()
                    .flat_map(|s| s.lines.iter_mut())
                    .find(|l| l.memory_id.as_deref() == Some(id) && l.suggestion.is_none())
            });
            match line {
                Some(line) => line.suggestion = Some(shown),
                None => {
                    let title = intended_section(proposal);
                    match sections.iter_mut().find(|s| s.title == title) {
                        Some(section) => section.suggestions.push(shown),
                        None => sections.push(DocSection {
                            class: doc::section_class(&title).to_owned(),
                            title,
                            lines: Vec::new(),
                            suggestions: vec![shown],
                        }),
                    }
                }
            }
        }
        let changes = match &source {
            Some(source) => changes(conn, &source.id)?,
            None => Vec::new(),
        };
        Ok(MemoryView {
            docs,
            doc: MemoryDoc {
                project_id,
                name,
                path,
                dirty: file.work_blob != file.head_blob,
                blob: file.head_blob,
                sections,
                changes,
            },
        })
    }

    /// One revision for the provenance hint: its approval, the proposal that
    /// made it, and the sessions that loaded it.
    pub fn memory_revision(&self, id: &str) -> Result<RevisionDetail> {
        let conn = self.conn();
        let revision: Revision = get(conn, "memory revision", id)?;
        let source: Source = get(conn, "memory source", &revision.source_id)?;
        let approval: Option<Approval> = revision
            .approval_id
            .as_deref()
            .map(|id| get(conn, "approval", id))
            .transpose()?;
        let proposal: Option<Proposal> = find(
            conn,
            "applied_revision_id = ?1 ORDER BY decided_at DESC",
            &[&id],
        )?;
        let card = display_id(conn, proposal.as_ref().and_then(|p| p.card_id.as_deref()))?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.role, s.project_id, s.run_id, c.display_id, l.trigger, l.loaded_at
             FROM session_loads l
             JOIN sessions s ON s.id = l.session_id
             LEFT JOIN runs r ON r.id = s.run_id
             LEFT JOIN cards c ON c.id = r.card_id
             WHERE l.revision_id = ?1
             ORDER BY l.loaded_at DESC LIMIT 50",
        )?;
        let sessions = stmt
            .query_map([id], |row| {
                Ok(LoadedBy {
                    session_id: row.get(0)?,
                    role: row.get(1)?,
                    project_id: row.get(2)?,
                    run_id: row.get(3)?,
                    card: row.get(4)?,
                    trigger: row.get(5)?,
                    loaded_at: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(RevisionDetail {
            id: revision.id,
            path: source
                .canonical_path
                .unwrap_or_else(|| document_path(&source.project_id)),
            project_id: source.project_id,
            blob_sha: revision.blob_sha,
            commit_sha: revision.commit_sha,
            review: revision.review,
            provenance: revision.provenance,
            recorded_at: revision.recorded_at,
            approval,
            proposal_id: proposal.map(|p| p.id),
            card,
            sessions,
        })
    }

    /// The proposals still `proposed` that match `filter`, oldest first (the
    /// order a bulk approval decides them in).
    pub fn mem_pending(&self, filter: &PendingFilter) -> Result<Vec<Proposal>> {
        let found: Vec<Proposal> = list(
            self.conn(),
            "state = 'proposed' AND (?1 IS NULL OR boundary = ?1) AND (?2 IS NULL OR project_id = ?2)
             ORDER BY created_at, id",
            &[&filter.boundary, &filter.project],
        )?;
        Ok(found
            .into_iter()
            .filter(|p| {
                filter
                    .class
                    .as_deref()
                    .is_none_or(|c| proposal_class(p) == c)
            })
            .collect())
    }

    /// The proposal behind a "Recent changes" entry, with the commit an
    /// Undo reverts.
    pub fn memory_change(&self, proposal: &str) -> Result<ChangeRef> {
        let conn = self.conn();
        let proposal: Proposal = get(conn, "proposal", proposal)?;
        let commit = match proposal.approval_id.as_deref() {
            Some(id) if proposal.state == "applied" => get::<Approval>(conn, "approval", id)?
                .result
                .and_then(|r| r.get("commit").and_then(Value::as_str).map(str::to_owned)),
            _ => None,
        };
        Ok(ChangeRef {
            proposal_id: proposal.id,
            project_id: proposal.project_id,
            state: proposal.state,
            commit,
        })
    }
}

#[cfg(test)]
#[path = "memory_tests.rs"]
mod tests;
