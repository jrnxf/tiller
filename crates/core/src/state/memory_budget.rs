//! The startup token budget `mem.decide` holds a Keep to (`memory.md`
//! section 6): the approved memory a launch of each role renders, counted
//! as the launcher renders it, before and after the kept change.
//!
//! Nothing here reads a file: the caller hands in a reader for blob texts.

use std::collections::HashMap;

use rusqlite::params;
use serde_json::{Map, Value, json};

use super::memory::host_boundary;
use super::memory_load::loadable;
use crate::error::{Error, Result};
use crate::ids::FLEET_PROJECT;
use crate::memory::{STARTUP_BUDGET, grouped, render_loaded, tokens};

/// Reads a blob's text from the memory repo by its SHA; `None` when the
/// repo does not have it.
pub type BlobReader<'a> = &'a dyn Fn(&str) -> Result<Option<String>>;

/// The document a Keep changes and the text it leaves.
pub(super) struct Change<'a> {
    pub proposal_id: &'a str,
    pub source_id: &'a str,
    pub project_id: &'a str,
    pub path: &'a str,
    pub after: &'a str,
}

/// One role's startup memory in one project, before and after the change.
struct Count {
    project: String,
    role: String,
    now: i64,
    after: i64,
}

/// The startup tokens each role that loads the changed document would have
/// after it, the most over every project it loads in. Refuses the Keep with
/// the numbers when a role would go over [`STARTUP_BUDGET`] and the change
/// adds to it; a change that leaves a role where it was, or lowers it, keeps.
pub(super) fn check(
    conn: &rusqlite::Connection,
    change: &Change<'_>,
    read: BlobReader<'_>,
) -> Result<Value> {
    let roles: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT value FROM json_each((SELECT roles FROM memory_sources WHERE id = ?1))",
        )?;
        let rows = stmt.query_map([change.source_id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let projects = if change.project_id == FLEET_PROJECT {
        // A launch in a project with no document of its own loads the fleet
        // document alone: `loadable` of the fleet itself is that set.
        let boundary = host_boundary(conn)?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT project_id FROM memory_sources
             WHERE kind = 'tiller_doc' AND retired_at IS NULL AND boundary = ?1
               AND project_id != ?2
             ORDER BY project_id",
        )?;
        let rows = stmt.query_map(params![boundary, FLEET_PROJECT], |row| row.get(0))?;
        let mut projects: Vec<String> = rows.collect::<rusqlite::Result<_>>()?;
        projects.push(FLEET_PROJECT.to_owned());
        projects
    } else {
        vec![change.project_id.to_owned()]
    };
    let mut texts = HashMap::new();
    let mut counts = Vec::new();
    for project in &projects {
        for role in &roles {
            let (now, after) = count(conn, change, project, role, read, &mut texts)?;
            counts.push(Count {
                project: project.clone(),
                role: role.clone(),
                now,
                after,
            });
        }
    }
    let over = |c: &&Count| c.after > STARTUP_BUDGET && c.after > c.now;
    if let Some(worst) = counts
        .iter()
        .filter(over)
        .max_by_key(|c| (c.after, c.after - c.now))
    {
        let same: Vec<&str> = counts
            .iter()
            .filter(over)
            .filter(|c| c.project == worst.project && c.now == worst.now && c.after == worst.after)
            .map(|c| c.role.as_str())
            .collect();
        let whose = if same.len() == roles.len() && roles.len() > 1 {
            "every role's".to_owned()
        } else {
            same.iter()
                .map(|role| format!("the {role}'s"))
                .collect::<Vec<_>>()
                .join(" and ")
        };
        let place = if worst.project == FLEET_PROJECT {
            "a project with no document of its own".to_owned()
        } else {
            worst.project.clone()
        };
        return Err(Error::refused(
            "proposal",
            change.proposal_id,
            format!(
                "it would put {whose} startup memory in {place} over the {}-token budget: {} tokens now, {} from this change, {} after",
                grouped(STARTUP_BUDGET),
                grouped(worst.now),
                grouped(worst.after - worst.now),
                grouped(worst.after),
            ),
        ));
    }
    let mut most = Map::new();
    for c in &counts {
        let entry = most.entry(c.role.clone()).or_insert(json!(c.after));
        if entry.as_i64().is_some_and(|tokens| tokens < c.after) {
            *entry = json!(c.after);
        }
    }
    Ok(Value::Object(most))
}

/// One role's startup tokens in `project` now and with the change: the
/// approved set `loadable` gives, with the changed document's text swapped
/// for the one it leaves. The count does not depend on the documents'
/// order, so a new document is counted at the end.
fn count(
    conn: &rusqlite::Connection,
    change: &Change<'_>,
    project: &str,
    role: &str,
    read: BlobReader<'_>,
    texts: &mut HashMap<String, String>,
) -> Result<(i64, i64)> {
    let docs = loadable(conn, project, role)?;
    let mut now = Vec::new();
    let mut after = Vec::new();
    let mut loads = false;
    for doc in &docs {
        if !texts.contains_key(&doc.blob_sha) {
            let text = read(&doc.blob_sha)?.ok_or_else(|| {
                Error::refused(
                    "proposal",
                    change.proposal_id,
                    format!(
                        "the approved revision {} of {} cannot be read to count the startup budget",
                        doc.revision_id, doc.canonical_path
                    ),
                )
            })?;
            texts.insert(doc.blob_sha.clone(), text);
        }
    }
    let texts = &*texts;
    for doc in &docs {
        let text = texts.get(&doc.blob_sha).map_or("", String::as_str);
        let path = doc.canonical_path.as_str();
        now.push((path, text));
        if doc.source_id == change.source_id {
            loads = true;
            after.push((path, change.after));
        } else {
            after.push((path, text));
        }
    }
    if !loads {
        // The document has no approved text yet: it loads once kept, if
        // the role and project load it at all.
        let loaded_here = change.project_id == FLEET_PROJECT || change.project_id == project;
        if loaded_here {
            after.push((change.path, change.after));
        }
    }
    Ok((
        tokens(&render_loaded(now.iter().copied())),
        tokens(&render_loaded(after.iter().copied())),
    ))
}
