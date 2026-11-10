//! Nightly backup, `tiller restore`, JSONL export and import, and the schema
//! migration tool (T2.3; `recovery.md` R10 and R11, `state.md` section 10).
//!
//! Everything here works on files in the host's Tiller home (`~/.tiller`):
//! `state.db`, `backups/` with its `manifest.json`, the pre-migration and
//! pre-restore copies beside `state.db`, and the daemon's lock file. Backups
//! never leave that home, and every copy records the `meta.host_id` and
//! `meta.boundary` it was taken from, so a host's records stay inside its
//! boundary (`memory.md` section 8): a restore refuses another host's backup
//! and an import refuses another boundary's export.
//!
//! Copies are taken through SQLite's online backup API, not the `sqlite3`
//! CLI, which one host does not have (`host-capabilities.md`).

mod export;
mod manifest;
mod nightly;
mod restore;
mod schema;

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::store::BUSY_TIMEOUT;
use crate::time::Timestamp;

pub use export::{
    EXPORT_FORMAT, ExportManifest, ExportedTable, Imported, export_jsonl, import_jsonl,
};
pub use manifest::{Entry, EntryKind, Failure, Manifest};
pub use nightly::{
    KEEP_DAILY, KEEP_WEEKLY, MIN_VERIFIED, Nightly, RETRY_AFTER_MINUTES, Retention, backup,
    next_due, nightly,
};
pub use restore::{Restored, Skipped, restore};
pub(crate) use schema::migrate_on_open;
pub use schema::{Migrated, migrate};

/// Where one host's Tiller keeps its files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The Tiller home, `~/.tiller`. Manifest entries are relative to it.
    pub home: PathBuf,
    pub db: PathBuf,
    pub backups: PathBuf,
    /// The daemon's single-instance lock (`recovery.md` R01).
    pub daemon_lock: PathBuf,
}

impl Paths {
    pub fn in_home(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        Self {
            db: home.join("state.db"),
            backups: home.join("backups"),
            daemon_lock: home.join("daemon.lock"),
            home,
        }
    }

    /// The paths around a `state.db`: its directory is the Tiller home.
    pub fn for_db(db: impl AsRef<Path>) -> Self {
        let db = db.as_ref();
        let home = db.parent().map(Path::to_path_buf).unwrap_or_default();
        Self {
            db: db.to_path_buf(),
            ..Self::in_home(home)
        }
    }

    pub fn manifest(&self) -> PathBuf {
        self.backups.join("manifest.json")
    }

    /// A path a manifest entry names, relative to the home.
    fn resolve(&self, file: &str) -> PathBuf {
        self.home.join(file)
    }

    /// `name` with `suffix` appended, beside `state.db`.
    fn beside_db(&self, suffix: &str) -> PathBuf {
        let mut name = self.db.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        self.db.with_file_name(name)
    }

    /// The lock every migration of `state.db` holds, from its version check
    /// to its commit ([`MigrationLock`]).
    fn migration_lock(&self) -> PathBuf {
        self.beside_db(".migrate.lock")
    }

    /// `path` relative to the home, as a manifest stores it.
    fn relative(&self, path: &Path) -> Result<String> {
        path.strip_prefix(&self.home)
            .ok()
            .and_then(Path::to_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::Internal(format!(
                    "{} is outside the Tiller home {}",
                    path.display(),
                    self.home.display()
                ))
            })
    }
}

/// The daemon's single-instance lock, held for the daemon's lifetime.
/// `tiller restore`, `tiller migrate` and the JSONL import take it too and
/// hold it to the end, so they refuse while the daemon runs and the daemon
/// cannot start halfway through them.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
}

impl DaemonLock {
    /// Takes the lock, or refuses when another process holds it.
    pub fn acquire(paths: &Paths) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&paths.daemon_lock)
            .map_err(io_error("open", &paths.daemon_lock))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(daemon_running_error()),
            Err(TryLockError::Error(error)) => Err(io_error("lock", &paths.daemon_lock)(error)),
        }
    }
}

/// The lock that makes a schema migration one process's work (`recovery.md`
/// R11). `Store::open` and `tiller migrate` hold it from the moment they read
/// the database's version, through the pre-migration copy, to the
/// migration's commit. A process that waited for it reads the version again,
/// so when the daemon and a CLI command open an old database at once, one
/// migrates and the other finds nothing to do. The lock is the operating
/// system's, so it ends with the process that held it, even on a crash.
#[derive(Debug)]
pub(crate) struct MigrationLock {
    _file: File,
}

impl MigrationLock {
    /// Waits for the lock. A migration and its copy are bounded by the size
    /// of the database, so the wait is too.
    pub(crate) fn acquire(paths: &Paths) -> Result<Self> {
        let path = paths.migration_lock();
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(io_error("open", &path))?;
        file.lock().map_err(io_error("lock", &path))?;
        Ok(Self { _file: file })
    }
}

/// Whether a daemon holds its lock right now.
pub fn daemon_running(paths: &Paths) -> Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(&paths.daemon_lock)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error("open", &paths.daemon_lock)(error)),
    };
    match file.try_lock() {
        // Dropping the file releases the probe's lock.
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(io_error("lock", &paths.daemon_lock)(error)),
    }
}

fn daemon_running_error() -> Error {
    Error::refused(
        "daemon",
        "local",
        "the daemon is running; stop it first (restore, migrate and an export's import refuse while it runs)",
    )
}

/// The facts a copy records about the database it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Facts {
    schema_version: usize,
    max_seq: i64,
    host_id: Option<String>,
    boundary: Option<String>,
}

/// A verified copy on disk.
struct Copied {
    size: u64,
    sha256: String,
    facts: Facts,
}

/// Copies the database behind `src` to `dest` through the online backup API
/// and verifies the copy: `integrity_check`, the page count against the
/// source's, then its size and sha256. The copy is written to a temporary
/// file of its own ([`unique_tmp`]) and renamed into place only once it
/// verifies, so `dest` never holds a partial file.
fn copy_verified(src: &Connection, dest: &Path) -> Result<Copied> {
    let tmp = unique_tmp(dest);
    #[cfg(test)]
    if FAIL_COPIES.get() {
        return Err(Error::Internal(format!(
            "copy to {}: failure injected by the test",
            tmp.display()
        )));
    }
    let result = copy_to(src, &tmp);
    match result {
        Ok(copied) => {
            fs::rename(&tmp, dest).map_err(io_error("rename", &tmp))?;
            sync_dir(dest)?;
            Ok(copied)
        }
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(error)
        }
    }
}

fn copy_to(src: &Connection, tmp: &Path) -> Result<Copied> {
    let mut dest = Connection::open(tmp)?;
    let pages = {
        let backup = Backup::new(src, &mut dest)?;
        // One step copies every page under one read lock, so a writer on
        // another connection cannot restart the copy halfway.
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            match backup.step(-1)? {
                StepResult::Done => break,
                StepResult::More => {}
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    return Err(Error::Unavailable(
                        "database busy for the whole backup timeout".into(),
                    ));
                }
            }
        }
        i64::from(backup.progress().pagecount)
    };
    // A self-contained file: no WAL beside it.
    let _: String = dest.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    check_integrity(&dest, "the backup copy")?;
    let copied_pages: i64 = dest.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    if copied_pages != pages {
        return Err(Error::Internal(format!(
            "the backup copy has {copied_pages} pages, the source {pages}"
        )));
    }
    let facts = facts(&dest)?;
    dest.close().map_err(|(_, error)| Error::from(error))?;
    let file = File::open(tmp).map_err(io_error("open", tmp))?;
    file.sync_all().map_err(io_error("sync", tmp))?;
    let (size, sha256) = hash_file(tmp)?;
    Ok(Copied {
        size,
        sha256,
        facts,
    })
}

/// Reads the facts a manifest line records from a database.
fn facts(conn: &Connection) -> Result<Facts> {
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let has = |table: &str| -> Result<bool> {
        Ok(conn
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    let max_seq = if has("events")? {
        conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
            row.get(0)
        })?
    } else {
        0
    };
    let meta = |key: &str| -> Result<Option<String>> {
        if !has("meta")? {
            return Ok(None);
        }
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    };
    Ok(Facts {
        schema_version: usize::try_from(schema_version).unwrap_or(0),
        max_seq,
        host_id: meta("host_id")?,
        boundary: meta("boundary")?,
    })
}

fn check_integrity(conn: &Connection, what: &str) -> Result<()> {
    let result: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(Error::Internal(format!(
            "{what} failed integrity_check: {result}"
        )))
    }
}

/// Checks a manifest entry against its file: size, sha256, then
/// `integrity_check`. `Err` carries the reason it does not verify.
fn verify(paths: &Paths, entry: &Entry) -> std::result::Result<Facts, String> {
    let path = paths.resolve(&entry.file);
    let (size, sha256) = hash_file(&path).map_err(|error| error.to_string())?;
    if size != entry.size {
        return Err(format!(
            "size {size} differs from the manifest's {}",
            entry.size
        ));
    }
    if sha256 != entry.sha256 {
        return Err("sha256 differs from the manifest's".into());
    }
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| error.to_string())?;
    check_integrity(&conn, "the backup").map_err(|error| error.to_string())?;
    facts(&conn).map_err(|error| error.to_string())
}

fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut file = File::open(path).map_err(io_error("open", path))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(io_error("read", path))?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }
    Ok((size, hex(&hasher.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A file-name stamp for `now`, for example `20261102T030000123Z`.
fn stamp(now: Timestamp) -> String {
    let ts = now.as_jiff();
    format!(
        "{}{:03}Z",
        ts.strftime("%Y%m%dT%H%M%S"),
        now.as_millis().rem_euclid(1000)
    )
}

/// A temporary name beside `path` that no other writer uses,
/// `<path>.<ulid>.tmp`: two processes that write the same file at once each
/// write their own temporary file, so neither removes or overwrites the
/// other's.
fn unique_tmp(path: &Path) -> PathBuf {
    with_suffix(path, &format!(".{}.tmp", crate::ids::new_ulid()))
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("remove", path)(error)),
    }
}

/// Writes `bytes` to `path` atomically: a temporary file of its own
/// ([`unique_tmp`]), synced, renamed, and the rename synced.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = unique_tmp(path);
    let written = File::create(&tmp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(io_error("write", &tmp));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written?;
    sync_dir(path)
}

/// Makes a rename in `path`'s directory durable.
fn sync_dir(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error("sync", dir))?;
    }
    Ok(())
}

/// Creates a directory only its owner can read (`recovery.md` R01, directory
/// modes).
fn create_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(io_error("create", dir))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(io_error("chmod", dir))?;
    }
    Ok(())
}

fn io_error<'a>(action: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> Error + 'a {
    move |error| Error::Internal(format!("{action} {}: {error}", path.display()))
}

#[cfg(test)]
thread_local! {
    /// While set, every [`copy_verified`] on this thread fails, as a full
    /// disk would. The temporary name is unique, so a test cannot block it.
    static FAIL_COPIES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests;
