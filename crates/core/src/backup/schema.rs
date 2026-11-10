//! The schema migration tool, `tiller migrate [up | down <version>]`
//! (`recovery.md` R11): a verified copy `state.db.pre-<from>-<to>` through
//! the online backup API, then the migration in one transaction. The copy
//! is listed in the manifest and kept until the next nightly backup
//! verifies.

use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use super::{DaemonLock, Entry, EntryKind, Manifest, MigrationLock, Paths, copy_verified};
use crate::error::{Error, Result};
use crate::migrate;
use crate::store::BUSY_TIMEOUT;
use crate::time::Timestamp;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Migrated {
    pub from: usize,
    pub to: usize,
    /// The copy taken before the migration, relative to the Tiller home;
    /// `None` when there was nothing to migrate.
    pub copy: Option<String>,
}

/// Migrates `state.db` to `target`, refusing while the daemon runs. An
/// older binary refuses a newer database before it writes anything,
/// including the copy.
pub fn migrate(paths: &Paths, target: usize, now: Timestamp) -> Result<Migrated> {
    let _lock = DaemonLock::acquire(paths)?;
    if !paths.db.exists() {
        return Err(Error::refused(
            "state.db",
            paths.db.display().to_string(),
            "does not exist; run tiller init",
        ));
    }
    migrate_with(paths, target, now, migrate::to_version)
}

/// `Store::open`'s migration: take the verified copy of an existing database,
/// then migrate it up. An empty database (version 0) has nothing to keep.
///
/// The work runs under the [`MigrationLock`], and the version is read again
/// once the lock is held: a process that waited while another migrated finds
/// the database current and does nothing. A current database takes no lock.
pub(crate) fn migrate_on_open(conn: &mut Connection, db: &Path) -> Result<()> {
    if migrate::current_version(conn)? == migrate::LATEST {
        return Ok(());
    }
    let paths = Paths::for_db(db);
    let _lock = MigrationLock::acquire(&paths)?;
    let from = migrate::current_version(conn)?;
    if from == migrate::LATEST {
        return Ok(());
    }
    if from != 0 {
        take_copy(conn, &paths, from, migrate::LATEST, Timestamp::now())?;
    }
    migrate::up(conn)
}

pub(super) fn migrate_with(
    paths: &Paths,
    target: usize,
    now: Timestamp,
    step: impl FnOnce(&mut Connection, usize) -> Result<()>,
) -> Result<Migrated> {
    let mut conn = Connection::open(&paths.db)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // Held to the end, so a `Store::open` from the CLI cannot migrate up
    // between the version read below and this migration's commit.
    let _lock = MigrationLock::acquire(paths)?;
    let from = migrate::current_version(&conn)?;
    if target > migrate::LATEST {
        return Err(Error::Usage(format!(
            "schema {target} does not exist; this tiller knows up to {}",
            migrate::LATEST
        )));
    }
    if from == target {
        return Ok(Migrated {
            from,
            to: target,
            copy: None,
        });
    }
    // An empty database (version 0) has nothing to keep.
    let copy = if from == 0 {
        None
    } else {
        Some(take_copy(&conn, paths, from, target, now)?)
    };
    step(&mut conn, target).map_err(|error| {
        let kept = copy.as_deref().map_or_else(
            || "the database was empty".to_owned(),
            |copy| format!("the copy taken before it is {copy}"),
        );
        Error::Internal(format!(
            "migration {from} to {target} failed and rolled back: {error}; {kept}"
        ))
    })?;
    Ok(Migrated {
        from,
        to: target,
        copy,
    })
}

fn take_copy(
    conn: &Connection,
    paths: &Paths,
    from: usize,
    to: usize,
    now: Timestamp,
) -> Result<String> {
    let mut manifest = Manifest::load(paths)?;
    let dest = paths.beside_db(&format!(".pre-{from}-{to}"));
    let copied = copy_verified(conn, &dest)?;
    let file = paths.relative(&dest)?;
    // A second migration between the same versions replaces the first copy.
    manifest.entries.retain(|entry| entry.file != file);
    manifest.entries.push(Entry {
        file: file.clone(),
        kind: EntryKind::PreMigration,
        size: copied.size,
        sha256: copied.sha256,
        schema_version: copied.facts.schema_version,
        max_seq: copied.facts.max_seq,
        host_id: copied.facts.host_id,
        boundary: copied.facts.boundary,
        at: now,
        superseded: false,
    });
    manifest.save(paths)?;
    Ok(file)
}
