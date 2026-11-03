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

use rusqlite::Connection;
use rusqlite_migration::{M, Migrations, SchemaVersion};

use crate::error::{Error, Result};

static MIGRATIONS: &[M<'static>] = &[
    M::up(include_str!("../migrations/0001_initial/up.sql"))
        .down(include_str!("../migrations/0001_initial/down.sql"))
        .foreign_key_check(),
    M::up(include_str!("../migrations/0002_project_stage/up.sql"))
        .down(include_str!("../migrations/0002_project_stage/down.sql"))
        .foreign_key_check(),
];

fn migrations() -> Migrations<'static> {
    Migrations::from_slice(MIGRATIONS)
}

/// The schema version this binary writes.
pub const LATEST: usize = MIGRATIONS.len();

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
