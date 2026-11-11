//! `backups/manifest.json`: the list of verified copies, kept outside the
//! database on purpose (`recovery.md` section 2), so a damaged `state.db`
//! cannot take the record of its own backups with it.

use std::fs;
use std::io;

use serde::{Deserialize, Serialize};

use super::{ManifestLock, Paths, create_private_dir, io_error, write_synced};
use crate::error::{Error, Result};
use crate::time::Timestamp;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub entries: Vec<Entry>,
    /// Failed nightly attempts since the last one that verified, oldest
    /// first. A success clears them.
    #[serde(default)]
    pub failures: Vec<Failure>,
    /// When the next night's backup is due; `None` before the first one.
    #[serde(default)]
    pub next_night: Option<Timestamp>,
    /// Every JSONL export taken from this home, oldest first. An export
    /// lives in a directory the owner names, so this is the home's only
    /// record of it: `host.edit` keeps the boundary an export records
    /// (`memory.md` section 8).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports: Vec<ExportRecord>,
}

/// One `tiller export --jsonl`, as the backups manifest records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportRecord {
    pub host_id: Option<String>,
    pub boundary: Option<String>,
    pub max_seq: i64,
    pub at: Timestamp,
}

/// One verified copy (`recovery.md` R10: file, size, sha256, schema version,
/// highest event `seq`, time).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the Tiller home, for example `backups/20261102T030000000Z.db`.
    pub file: String,
    pub kind: EntryKind,
    pub size: u64,
    pub sha256: String,
    pub schema_version: usize,
    pub max_seq: i64,
    pub host_id: Option<String>,
    pub boundary: Option<String>,
    pub at: Timestamp,
    /// Set by retention just before it deletes the file; never unset.
    #[serde(default)]
    pub superseded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A nightly backup in `backups/`.
    Nightly,
    /// `tiller backup`, in `backups/`; retained like a nightly one.
    Manual,
    /// `state.db.pre-<from>-<to>`, taken before a migration (`recovery.md` R11).
    PreMigration,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub at: Timestamp,
    pub error: String,
    /// The hour-later retry of a failed night, rather than its first attempt.
    pub retry: bool,
}

impl Manifest {
    /// Reads the manifest; a missing one is empty. An unreadable one is an
    /// error, never silently replaced, since retention trusts it.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.manifest();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| {
                Error::Internal(format!(
                    "{} does not parse ({error}); fix or move it before the next backup",
                    path.display()
                ))
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(io_error("read", &path)(error)),
        }
    }

    /// Replaces the manifest atomically: a temporary file, synced, renamed.
    pub(crate) fn save(&self, paths: &Paths, held: &ManifestLock) -> Result<()> {
        create_private_dir(&paths.backups)?;
        let path = paths.manifest();
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        write_synced(&path, text.as_bytes(), held)
    }

    /// Whether a backup, a pre-migration copy or an export ever recorded
    /// this home's boundary, so a restore or an import would bring it back.
    pub fn records_boundary(&self) -> Option<&'static str> {
        if !self.entries.is_empty() {
            Some("a backup")
        } else if !self.exports.is_empty() {
            Some("an export")
        } else {
            None
        }
    }

    /// Entries retention has not superseded, newest first.
    pub fn live(&self) -> Vec<&Entry> {
        let mut live: Vec<&Entry> = self.entries.iter().filter(|e| !e.superseded).collect();
        live.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.file.cmp(&a.file)));
        live
    }
}
