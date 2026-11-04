//! Versioned schema migrations, each with an up and a down (`state.md`
//! section 10; `recovery.md` R11). `rusqlite_migration` keeps the version in
//! `PRAGMA user_version`; each migration also writes `meta.schema_version` so
//! the version is visible in the data and the JSONL export.
//!
//! The verified pre-migration copy (`state.db.pre-<from>-<to>`) and the
//! `tiller migrate` command belong to the schema migration tool
//! ([`crate::backup::migrate`], T2.3), which calls [`to_version`] after
//! taking that copy; [`crate::Store::open`] takes the same copy before it
//! migrates up.

use rusqlite::{Connection, Transaction};
use rusqlite_migration::{HookError, HookResult, M, Migrations, SchemaVersion};

use crate::error::{Error, Result};

/// The `projects.id` check of migrations 1 to 6. It meant a hyphen-free id of
/// at most 16 characters, but in GLOB `*` matches anything, so only the first
/// two characters and the length were checked.
const PROJECT_ID_CHECK_V1: &str =
    "CHECK (id GLOB '[a-z][a-z0-9]*' AND length(id) BETWEEN 2 AND 16)";

/// The `projects.id` check from migration 7 on, [`crate::ids::validate_project_id`]
/// in GLOB form: words of lowercase letters and digits joined by single
/// hyphens, each word starting with a letter, 2 to 32 characters.
const PROJECT_ID_CHECK_V7: &str = "CHECK (id GLOB '[a-z]*' AND id NOT GLOB '*[^a-z0-9-]*' \
    AND id NOT GLOB '*-' AND id NOT GLOB '*--*' AND id NOT GLOB '*-[0-9]*' \
    AND length(id) BETWEEN 2 AND 32)";

/// The schema version this binary writes: the length of [`list`].
pub const LATEST: usize = 8;

/// Every migration in version order. Add a new one at the end and bump
/// [`LATEST`].
fn list() -> Vec<M<'static>> {
    vec![
        M::up(include_str!("../migrations/0001_initial/up.sql"))
            .down(include_str!("../migrations/0001_initial/down.sql"))
            .foreign_key_check(),
        M::up(include_str!("../migrations/0002_project_stage/up.sql"))
            .down(include_str!("../migrations/0002_project_stage/down.sql"))
            .foreign_key_check(),
        M::up(include_str!("../migrations/0003_done_window_3d/up.sql"))
            .down(include_str!("../migrations/0003_done_window_3d/down.sql"))
            .foreign_key_check(),
        M::up(include_str!("../migrations/0004_peer_pairing/up.sql"))
            .down(include_str!("../migrations/0004_peer_pairing/down.sql"))
            .foreign_key_check(),
        M::up(include_str!(
            "../migrations/0005_intervention_labels/up.sql"
        ))
        .down(include_str!(
            "../migrations/0005_intervention_labels/down.sql"
        ))
        .foreign_key_check(),
        M::up(include_str!("../migrations/0006_learning_loop/up.sql"))
            .down(include_str!("../migrations/0006_learning_loop/down.sql"))
            .foreign_key_check(),
        M::up_with_hook(
            include_str!("../migrations/0007_hyphen_project_ids/up.sql"),
            |tx: &Transaction| {
                rewrite_project_id_check(tx, PROJECT_ID_CHECK_V1, PROJECT_ID_CHECK_V7)
            },
        )
        .down_with_hook(
            include_str!("../migrations/0007_hyphen_project_ids/down.sql"),
            narrow_project_ids,
        )
        .foreign_key_check(),
        M::up(include_str!("../migrations/0008_attention_subject/up.sql"))
            .down(include_str!(
                "../migrations/0008_attention_subject/down.sql"
            ))
            .foreign_key_check(),
    ]
}

fn migrations() -> Migrations<'static> {
    Migrations::new(list())
}

/// Starts a hook error that refuses a step over the rows it meets, as
/// opposed to an internal failure; [`Error`] maps the two apart.
pub(crate) const HOOK_REFUSAL: &str = "refused: ";

/// Swaps the `projects.id` check's text in `sqlite_schema` and bumps the
/// schema cookie so every open connection reloads it. SQLite does not
/// recheck stored rows against the new text, so this does, and refuses,
/// naming them, when a row fails it.
fn rewrite_project_id_check(tx: &Transaction, from: &str, to: &str) -> HookResult {
    let sql: String = tx.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'projects'",
        [],
        |row| row.get(0),
    )?;
    if sql.matches(from).count() != 1 {
        return Err(HookError::Hook(format!(
            "the projects table does not hold the check {from:?} exactly once"
        )));
    }
    let Some(expr) = to.strip_prefix("CHECK ") else {
        return Err(HookError::Hook(format!("{to:?} is not a CHECK")));
    };
    let cookie: i64 = tx.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    tx.pragma_update(None, "writable_schema", true)?;
    tx.execute(
        "UPDATE sqlite_schema SET sql = ?1 WHERE type = 'table' AND name = 'projects'",
        [sql.replacen(from, to, 1)],
    )?;
    tx.pragma_update(None, "schema_version", cookie + 1)?;
    // RESET also turns writable_schema off and makes this connection reparse
    // the schema, so its own next statement sees the new check
    tx.execute_batch("PRAGMA writable_schema = RESET")?;
    let failing: Vec<String> = tx
        .prepare(&format!(
            "SELECT id FROM projects WHERE NOT {expr} ORDER BY id"
        ))?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !failing.is_empty() {
        return Err(HookError::Hook(format!(
            "{HOOK_REFUSAL}these project ids fail the new check {to:?}: {}",
            failing.join(", ")
        )));
    }
    Ok(())
}

/// Migration 7 down: refuses while a project id needs the wider rule, since
/// an older tiller would not parse its card ids.
fn narrow_project_ids(tx: &Transaction) -> HookResult {
    let wide: Vec<String> = tx
        .prepare("SELECT id FROM projects WHERE id GLOB '*-*' OR length(id) > 16 ORDER BY id")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !wide.is_empty() {
        return Err(HookError::Hook(format!(
            "{HOOK_REFUSAL}schema 6 allows only hyphen-free project ids of at most 16 characters; \
             these projects need schema 7: {}",
            wide.join(", ")
        )));
    }
    rewrite_project_id_check(tx, PROJECT_ID_CHECK_V7, PROJECT_ID_CHECK_V1)
}

/// The schema version of the database, or 0 for an empty one. Fails when the
/// database is newer than this binary.
pub fn current_version(conn: &Connection) -> Result<usize> {
    match migrations().current_version(conn)? {
        SchemaVersion::NoneSet => Ok(0),
        SchemaVersion::Inside(version) => Ok(version.get()),
        SchemaVersion::Outside(version) => Err(too_new(version.get())),
    }
}

/// Migrates up to [`LATEST`]. A database newer than this binary is refused
/// before anything is written.
pub fn up(conn: &mut Connection) -> Result<()> {
    to_version(conn, LATEST)
}

/// Migrates up or down to `target`, every step in one transaction.
pub fn to_version(conn: &mut Connection, target: usize) -> Result<()> {
    if target > LATEST {
        return Err(Error::Usage(format!(
            "schema {target} does not exist; this tiller knows up to {LATEST}"
        )));
    }
    let current = current_version(conn)?;
    if current != target {
        migrations().to_version(conn, target)?;
    }
    Ok(())
}

fn too_new(found: usize) -> Error {
    Error::SchemaTooNew {
        found,
        binary: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

#[cfg(test)]
mod tests;
