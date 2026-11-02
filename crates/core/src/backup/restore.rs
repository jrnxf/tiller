//! `tiller restore [<file>] [--dry-run]` (`recovery.md` R10). Always the
//! owner's command, and refused while the daemon runs.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use super::{
    DaemonLock, Entry, Manifest, Paths, facts, hash_file, io_error, remove_if_exists, stamp,
    sync_dir, verify, with_suffix,
};
use crate::error::{Error, Result};
use crate::time::Timestamp;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Restored {
    /// The backup restored, or that a dry run would restore.
    pub entry: Entry,
    /// Newer backups passed over because they do not verify, with the reason.
    pub skipped: Vec<Skipped>,
    /// Where the live database was moved (`state.db.pre-restore-<stamp>`);
    /// `None` when there was none. A dry run names the path it would use.
    pub moved_to: Option<PathBuf>,
    pub dry_run: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Skipped {
    pub file: String,
    pub reason: String,
}

/// Restores `file`, or with `None` the newest backup that verifies. A
/// backup that fails verification is never used. The live database is never
/// overwritten: it moves to `state.db.pre-restore-<stamp>` (its WAL and
/// shared-memory files with it, so it opens with every committed record),
/// and that file is never deleted automatically.
pub fn restore(
    paths: &Paths,
    file: Option<&Path>,
    dry_run: bool,
    now: Timestamp,
) -> Result<Restored> {
    let _lock = DaemonLock::acquire(paths)?;
    let manifest = Manifest::load(paths)?;
    let mut skipped = Vec::new();
    let entry = match file {
        Some(file) => {
            let entry = find(paths, &manifest, file)?;
            verify(paths, entry).map_err(|reason| {
                Error::refused(
                    "backup",
                    &entry.file,
                    format!(
                        "does not verify: {reason}; {}",
                        verified_list(paths, &manifest)
                    ),
                )
            })?;
            entry.clone()
        }
        None => {
            let mut chosen = None;
            for entry in manifest.live() {
                match verify(paths, entry) {
                    Ok(_) => {
                        chosen = Some(entry.clone());
                        break;
                    }
                    Err(reason) => skipped.push(Skipped {
                        file: entry.file.clone(),
                        reason,
                    }),
                }
            }
            chosen.ok_or_else(|| {
                Error::refused("backup", "newest", "no backup in the manifest verifies")
            })?
        }
    };
    check_host(paths, &entry)?;

    // The live file and every sidecar SQLite may have left: a stale WAL
    // next to the restored file would be replayed into it.
    let present: Vec<&str> = ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .filter(|suffix| with_suffix(&paths.db, suffix).exists())
        .collect();
    let moved_to =
        (!present.is_empty()).then(|| paths.beside_db(&format!(".pre-restore-{}", stamp(now))));
    if dry_run {
        return Ok(Restored {
            entry,
            skipped,
            moved_to,
            dry_run,
        });
    }

    // Stage the copy next to state.db and check it again, so the rename
    // below swaps in exactly the bytes that verified.
    let source = paths.resolve(&entry.file);
    let staged = paths.beside_db(".restore.tmp");
    remove_if_exists(&staged)?;
    fs::copy(&source, &staged).map_err(io_error("copy", &source))?;
    fs::File::open(&staged)
        .and_then(|file| file.sync_all())
        .map_err(io_error("sync", &staged))?;
    let (size, sha256) = hash_file(&staged)?;
    if size != entry.size || sha256 != entry.sha256 {
        let _ = fs::remove_file(&staged);
        return Err(Error::refused(
            "backup",
            &entry.file,
            "the copy changed while it was staged",
        ));
    }

    if let Some(moved_to) = &moved_to
        && let Err(error) = move_aside(&paths.db, moved_to, &present)
    {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    fs::rename(&staged, &paths.db).map_err(io_error("rename", &staged))?;
    sync_dir(&paths.db)?;
    Ok(Restored {
        entry,
        skipped,
        moved_to,
        dry_run,
    })
}

/// Moves the live file and its sidecars to `moved_to` together, or, when
/// one move fails, puts back the ones already moved, so the live database
/// is never left without its WAL.
fn move_aside(db: &Path, moved_to: &Path, suffixes: &[&str]) -> Result<()> {
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for suffix in suffixes {
        let from = with_suffix(db, suffix);
        let to = with_suffix(moved_to, suffix);
        if let Err(error) = fs::rename(&from, &to) {
            for (from, to) in moved.iter().rev() {
                let _ = fs::rename(to, from);
            }
            return Err(io_error("move", &from)(error));
        }
        moved.push((from, to));
    }
    Ok(())
}

/// The live entry naming `file`, by path or by the name the manifest uses.
fn find<'m>(paths: &Paths, manifest: &'m Manifest, file: &Path) -> Result<&'m Entry> {
    let wanted = fs::canonicalize(file).ok();
    manifest
        .live()
        .into_iter()
        .find(|entry| {
            let path = paths.resolve(&entry.file);
            Path::new(&entry.file) == file
                || path == file
                || (wanted.is_some() && fs::canonicalize(&path).ok() == wanted)
        })
        .ok_or_else(|| {
            Error::refused(
                "backup",
                file.display().to_string(),
                format!(
                    "not a backup this host's manifest lists, so it cannot be verified; {}",
                    verified_list(paths, manifest)
                ),
            )
        })
}

fn verified_list(paths: &Paths, manifest: &Manifest) -> String {
    let verified: Vec<&str> = manifest
        .live()
        .into_iter()
        .filter(|entry| verify(paths, entry).is_ok())
        .map(|entry| entry.file.as_str())
        .collect();
    if verified.is_empty() {
        "no backup verifies".into()
    } else {
        format!("backups that verify: {}", verified.join(", "))
    }
}

/// A backup restores only onto the host it was taken from. A live database
/// too damaged to read names no host, so it does not block the restore.
fn check_host(paths: &Paths, entry: &Entry) -> Result<()> {
    let Ok(conn) = Connection::open_with_flags(&paths.db, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return Ok(());
    };
    let Ok(live) = facts(&conn) else {
        return Ok(());
    };
    if let (Some(live_host), Some(backup_host)) = (&live.host_id, &entry.host_id)
        && live_host != backup_host
    {
        return Err(Error::refused(
            "backup",
            &entry.file,
            format!("taken on host {backup_host}, but state.db belongs to host {live_host}"),
        ));
    }
    if let (Some(live_boundary), Some(backup_boundary)) = (&live.boundary, &entry.boundary)
        && live_boundary != backup_boundary
    {
        return Err(Error::refused(
            "backup",
            &entry.file,
            format!("its boundary is {backup_boundary}, this host's is {live_boundary}"),
        ));
    }
    Ok(())
}
