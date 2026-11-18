//! Migration 23 up (`memory.md` section 6): each approved tiller memory
//! document becomes pending notes, then the old memory tables go, in one
//! transaction. A blob the memory repo cannot give is skipped and named in
//! the `memory.migrated` event; the migration does not stop for it.

use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Transaction, params};
use rusqlite_migration::HookResult;
use serde_json::json;

use crate::ids;
use crate::time::Timestamp;

/// Git's id for the empty blob: a document with no lines.
const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
/// The host's memory repo as `memory_sources` named it, under the Tiller home.
const MEMORY_REPO: &str = "~/.tiller/memory";
/// The section whose lines become gotchas; every other section is a convention.
const GOTCHA_SECTION: &str = "Watch out for";
const TITLE_MAX: usize = 120;
const BODY_MAX: usize = 2000;

struct Document {
    source_id: String,
    project_id: String,
    path: String,
    repo: Option<String>,
    blob_sha: String,
}

/// One pending note a document line gives.
#[derive(Debug, PartialEq)]
pub(super) struct Line {
    pub kind: &'static str,
    pub title: String,
    pub body: String,
}

pub(super) fn up(tx: &Transaction) -> HookResult {
    let home = home(tx)?;
    let documents = documents(tx)?;
    let mut notes = Vec::new();
    let mut skipped = Vec::new();
    for doc in &documents {
        let text = if doc.blob_sha == EMPTY_BLOB {
            Ok(String::new())
        } else {
            blob(home.as_deref(), doc)
        };
        match text {
            Ok(text) => notes.extend(
                lines(&text)
                    .into_iter()
                    .map(|l| (doc.project_id.clone(), l)),
            ),
            Err(reason) => skipped.push(json!({
                "source_id": doc.source_id,
                "path": doc.path,
                "blob_sha": doc.blob_sha,
                "reason": reason,
            })),
        }
    }

    tx.execute_batch(include_str!("../../migrations/0023_kb_memory/up.sql"))?;

    let now = Timestamp::now();
    for (project_id, line) in &notes {
        tx.execute(
            "INSERT INTO memory_pending (id, project_id, kind, title, body, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'migration', ?6)",
            params![
                ids::new_ulid(),
                project_id,
                line.kind,
                line.title,
                line.body,
                now
            ],
        )?;
    }
    // A database that had no memory document (a new one) records nothing.
    if documents.is_empty() {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, payload)
         VALUES (?1, ?2, 'migrate', 'daemon', 'meta', 'schema', 'memory.migrated', ?3)",
        params![
            ids::new_ulid(),
            now,
            json!({"pending": notes.len(), "skipped": skipped}).to_string()
        ],
    )?;
    Ok(())
}

/// The Tiller home: the folder that holds `state.db`. An in-memory
/// database has none.
fn home(tx: &Transaction) -> rusqlite::Result<Option<PathBuf>> {
    let file: String = tx.query_row(
        "SELECT file FROM pragma_database_list WHERE name = 'main'",
        [],
        |row| row.get(0),
    )?;
    Ok(Path::new(&file)
        .parent()
        .filter(|_| !file.is_empty())
        .map(Path::to_path_buf))
}

/// Every live tiller document with an approved revision, and the blob of
/// its newest approved revision.
fn documents(tx: &Transaction) -> rusqlite::Result<Vec<Document>> {
    let mut stmt = tx.prepare(
        "SELECT s.id, s.project_id, s.canonical_path, s.canonical_repo,
                (SELECT r.blob_sha FROM memory_revisions r
                  WHERE r.source_id = s.id AND r.review = 'approved'
                  ORDER BY r.recorded_at DESC, r.id DESC LIMIT 1)
         FROM memory_sources s
         WHERE s.kind = 'tiller_doc' AND s.retired_at IS NULL
         ORDER BY s.project_id, s.canonical_path, s.id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (source_id, project_id, path, repo, blob) = row?;
        if let Some(blob_sha) = blob {
            out.push(Document {
                source_id,
                project_id,
                path: path.unwrap_or_default(),
                repo,
                blob_sha,
            });
        }
    }
    Ok(out)
}

/// The document's approved text from the memory repo, or why it cannot be read.
fn blob(home: Option<&Path>, doc: &Document) -> Result<String, String> {
    let repo = match doc.repo.as_deref() {
        Some(MEMORY_REPO) => home
            .map(|home| home.join("memory"))
            .ok_or_else(|| "the database has no home folder".to_owned())?,
        Some(other) if !other.starts_with('~') => PathBuf::from(other),
        Some(other) => return Err(format!("cannot place the repo {other}")),
        None => return Err("the document names no repo".to_owned()),
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["cat-file", "blob", &doc.blob_sha])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|_| "the blob is not UTF-8".to_owned())
}

/// Each non-empty line under a `## ` section heading, without its list
/// marker. "Watch out for" lines are gotchas; the rest are conventions.
pub(super) fn lines(text: &str) -> Vec<Line> {
    let mut section: Option<&str> = None;
    let mut out = Vec::new();
    for raw in text.lines() {
        if let Some(heading) = raw.strip_prefix("## ") {
            section = Some(heading.trim());
            continue;
        }
        if raw.starts_with("# ") {
            section = None;
            continue;
        }
        if raw.starts_with("###") {
            continue;
        }
        let Some(name) = section else { continue };
        let line = raw.trim();
        let line = line.strip_prefix("- ").unwrap_or(line).trim();
        if line.is_empty() {
            continue;
        }
        out.push(Line {
            kind: if name == GOTCHA_SECTION {
                "gotcha"
            } else {
                "convention"
            },
            title: line.chars().take(TITLE_MAX).collect(),
            body: line.chars().take(BODY_MAX).collect(),
        });
    }
    out
}
