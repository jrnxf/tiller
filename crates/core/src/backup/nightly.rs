//! The nightly backup, its retry and alarm, and retention (`recovery.md` R10
//! and section 6).

use std::collections::BTreeSet;
use std::path::Path;

use jiff::tz::TimeZone;
use serde::Serialize;

use super::{
    Entry, EntryKind, Failure, Manifest, ManifestLock, Paths, copy_verified, create_private_dir,
    remove_if_exists, stamp, verify,
};
use crate::domain::{Attention, AttentionKind};
use crate::error::Result;
use crate::ids::HOST_HEALTH_CARD;
use crate::state::attention::NewAttention;
use crate::store::{Actor, Ctx, Store};
use crate::time::Timestamp;

/// A failed night is retried once, this long after its first attempt.
pub const RETRY_AFTER_MINUTES: i64 = 60;
/// Retention keeps the newest verified backup of each of this many days...
pub const KEEP_DAILY: usize = 14;
/// ...and of each of this many ISO weeks...
pub const KEEP_WEEKLY: usize = 8;
/// ...and never fewer than this many that verify.
pub const MIN_VERIFIED: usize = 2;

const NIGHT_MINUTES: i64 = 24 * 60;

/// What one nightly pass did.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Nightly {
    NotDue {
        next: Timestamp,
    },
    Done {
        entry: Entry,
        retention: Retention,
    },
    Failed {
        error: String,
        next: Timestamp,
        /// The `recovery` item on `fleet-1`, opened from the second failed
        /// night in a row.
        alarm: Option<Box<Attention>>,
    },
}

/// What retention kept and deleted, as manifest file names.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Retention {
    pub kept: Vec<String>,
    pub deleted: Vec<String>,
    /// Kept entries found not to verify while counting [`MIN_VERIFIED`]
    /// (newest first, so the list stops at the second one that verifies).
    pub unverified: Vec<String>,
}

/// When the next nightly attempt is due, or `None` when one is due now
/// because there has never been one.
pub fn next_due(manifest: &Manifest) -> Option<Timestamp> {
    match manifest.failures.last() {
        Some(failure) if !failure.retry => Some(failure.at.plus_minutes(RETRY_AFTER_MINUTES)),
        _ => manifest.next_night,
    }
}

/// The night after the one that just ended. Slots advance by whole days
/// from the first night, so a late timer or a manual backup never moves the
/// schedule.
fn advance_night(manifest: &mut Manifest, now: Timestamp) {
    let mut next = manifest.next_night.unwrap_or(now);
    while next <= now {
        next = next.plus_minutes(NIGHT_MINUTES);
    }
    manifest.next_night = Some(next);
}

/// The daemon's nightly timer: when a backup is due, takes it, verifies it
/// and runs retention; on failure records it for the hour-later retry, and
/// from the second failed night in a row opens a `recovery` item on
/// `fleet-1` with what failed and which backups verify. A manifest that
/// cannot be read is an alarm at once, since no failure can be recorded in
/// it.
pub fn nightly(store: &mut Store, paths: &Paths, now: Timestamp) -> Result<Nightly> {
    let lock = ManifestLock::acquire(paths)?;
    let mut manifest = match Manifest::load(paths) {
        Ok(manifest) => manifest,
        Err(error) => {
            let alarm = open_alarm(
                store,
                now,
                "backup.manifest-unreadable".into(),
                "The backup manifest cannot be read, so nightly backups have stopped",
                format!("{error}"),
            )?;
            return Ok(Nightly::Failed {
                error: error.to_string(),
                next: now.plus_minutes(RETRY_AFTER_MINUTES),
                alarm: Some(Box::new(alarm)),
            });
        }
    };
    if let Some(next) = next_due(&manifest)
        && next > now
    {
        return Ok(Nightly::NotDue { next });
    }
    let mut after = manifest.clone();
    advance_night(&mut after, now);
    match take(store.conn(), paths, &lock, &after, EntryKind::Nightly, now) {
        Ok((entry, mut manifest)) => {
            let retention = retain(paths, &lock, &mut manifest, Some(&entry.file))?;
            Ok(Nightly::Done { entry, retention })
        }
        Err(error) => {
            let retry = matches!(manifest.failures.last(), Some(last) if !last.retry);
            if retry {
                // The night is over: the next attempt is the next night's.
                manifest = after;
            }
            manifest.failures.push(Failure {
                at: now,
                error: error.to_string(),
                retry,
            });
            manifest.save(paths, &lock)?;
            let failed_nights = manifest.failures.iter().filter(|f| f.retry).count();
            let alarm = if retry && failed_nights >= 2 {
                Some(Box::new(failed_nights_alarm(store, paths, &manifest, now)?))
            } else {
                None
            };
            let next = next_due(&manifest).unwrap_or(now);
            Ok(Nightly::Failed {
                error: error.to_string(),
                next,
                alarm,
            })
        }
    }
}

/// `tiller backup`: takes a verified backup now and runs retention. It does
/// not move the nightly schedule.
pub fn backup(conn: &rusqlite::Connection, paths: &Paths, now: Timestamp) -> Result<Nightly> {
    let lock = ManifestLock::acquire(paths)?;
    let manifest = Manifest::load(paths)?;
    let (entry, mut manifest) = take(conn, paths, &lock, &manifest, EntryKind::Manual, now)?;
    let retention = retain(paths, &lock, &mut manifest, Some(&entry.file))?;
    Ok(Nightly::Done { entry, retention })
}

/// Takes one verified copy into `backups/` and records it in a copy of
/// `manifest`, returned only once saved. A copy that does not verify is
/// never renamed into place or listed.
fn take(
    conn: &rusqlite::Connection,
    paths: &Paths,
    lock: &ManifestLock,
    manifest: &Manifest,
    kind: EntryKind,
    now: Timestamp,
) -> Result<(Entry, Manifest)> {
    create_private_dir(&paths.backups)?;
    let base = stamp(now);
    let mut dest = paths.backups.join(format!("{base}.db"));
    let mut n = 1;
    while dest.exists() {
        dest = paths.backups.join(format!("{base}-{n}.db"));
        n += 1;
    }
    let copied = copy_verified(conn, &dest, lock)?;
    let entry = Entry {
        file: paths.relative(&dest)?,
        kind,
        size: copied.size,
        sha256: copied.sha256,
        schema_version: copied.facts.schema_version,
        max_seq: copied.facts.max_seq,
        host_id: copied.facts.host_id,
        boundary: copied.facts.boundary,
        at: now,
        superseded: false,
    };
    let mut manifest = manifest.clone();
    manifest.entries.push(entry.clone());
    manifest.failures.clear();
    if let Err(error) = manifest.save(paths, lock) {
        // Unlisted, the copy could never be verified or retained.
        let _ = remove_if_exists(&dest);
        return Err(error);
    }
    Ok((entry, manifest))
}

/// Keeps the newest verified backup of each of the last [`KEEP_DAILY`] days
/// and [`KEEP_WEEKLY`] ISO weeks (UTC), and never fewer than [`MIN_VERIFIED`]
/// that verify. A pre-migration copy goes once a newer nightly backup has
/// verified. Retention marks a file superseded in the manifest before it
/// deletes it, deletes only files its manifest lists, and never deletes a
/// `state.db.pre-restore-*` file. `fresh` names the copy just taken and
/// verified, which is not checked again.
fn retain(
    paths: &Paths,
    lock: &ManifestLock,
    manifest: &mut Manifest,
    fresh: Option<&str>,
) -> Result<Retention> {
    let live = manifest.live();
    let nightly: Vec<&Entry> = live
        .iter()
        .copied()
        .filter(|entry| entry.kind != EntryKind::PreMigration)
        .collect();

    let mut keep = BTreeSet::new();
    let mut days = Vec::new();
    let mut weeks = Vec::new();
    for entry in &nightly {
        let date = entry.at.as_jiff().to_zoned(TimeZone::UTC).date();
        if days.len() < KEEP_DAILY && !days.contains(&date) {
            days.push(date);
            keep.insert(entry.file.clone());
        }
        let week = date.iso_week_date();
        let week = (week.year(), week.week());
        if weeks.len() < KEEP_WEEKLY && !weeks.contains(&week) {
            weeks.push(week);
            keep.insert(entry.file.clone());
        }
    }

    // Newest first, until enough verify: the new copy and the one before
    // it normally settle this in two checks.
    let mut retention = Retention::default();
    let mut verified = 0;
    for entry in nightly.iter().filter(|entry| keep.contains(&entry.file)) {
        if verified >= MIN_VERIFIED {
            break;
        }
        if fresh == Some(entry.file.as_str()) {
            verified += 1;
            continue;
        }
        match verify(paths, entry) {
            Ok(_) => verified += 1,
            Err(_) => retention.unverified.push(entry.file.clone()),
        }
    }
    // Too few kept backups verify: hold on to older ones that still do.
    let older: Vec<&Entry> = nightly
        .iter()
        .copied()
        .filter(|entry| !keep.contains(&entry.file))
        .collect();
    for entry in older {
        if verified >= MIN_VERIFIED {
            break;
        }
        if verify(paths, entry).is_ok() {
            keep.insert(entry.file.clone());
            verified += 1;
        }
    }

    let newest_nightly = nightly.first().map(|entry| entry.at);
    // Entries already superseded are a cleanup an earlier pass did not
    // finish: their files go now.
    let doomed: BTreeSet<String> = manifest
        .entries
        .iter()
        .filter(|entry| {
            entry.superseded
                || match entry.kind {
                    EntryKind::Nightly | EntryKind::Manual => !keep.contains(&entry.file),
                    EntryKind::PreMigration => {
                        newest_nightly.is_some_and(|newest| newest > entry.at)
                    }
                }
        })
        .filter(|entry| deletable(paths, entry))
        .map(|entry| entry.file.clone())
        .collect();
    retention.kept = nightly
        .iter()
        .filter(|entry| keep.contains(&entry.file))
        .map(|entry| entry.file.clone())
        .collect();

    if doomed.is_empty() {
        return Ok(retention);
    }
    for entry in &mut manifest.entries {
        if doomed.contains(&entry.file) {
            entry.superseded = true;
        }
    }
    manifest.save(paths, lock)?;
    for file in &doomed {
        remove_if_exists(&paths.resolve(file))?;
        retention.deleted.push(file.clone());
    }
    manifest
        .entries
        .retain(|entry| !doomed.contains(&entry.file));
    manifest.save(paths, lock)?;
    Ok(retention)
}

/// Whether retention may delete the file an entry names: a plain relative
/// path inside the home with the shape of its kind, and never a
/// pre-restore file, which may hold the only copy of newer records.
fn deletable(paths: &Paths, entry: &Entry) -> bool {
    let path = Path::new(&entry.file);
    let plain = path
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !plain || name.contains(".pre-restore") {
        return false;
    }
    let db_name = paths
        .db
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state.db");
    match entry.kind {
        EntryKind::Nightly | EntryKind::Manual => {
            let backups = paths.relative(&paths.backups).ok();
            path.parent().and_then(Path::to_str) == backups.as_deref() && name.ends_with(".db")
        }
        EntryKind::PreMigration => name.starts_with(&format!("{db_name}.pre-")),
    }
}

fn failed_nights_alarm(
    store: &mut Store,
    paths: &Paths,
    manifest: &Manifest,
    now: Timestamp,
) -> Result<Attention> {
    let failed: Vec<String> = manifest
        .failures
        .iter()
        .map(|failure| format!("{}: {}", failure.at, failure.error))
        .collect();
    let verified: Vec<String> = manifest
        .live()
        .into_iter()
        .filter(|entry| verify(paths, entry).is_ok())
        .map(|entry| entry.file.clone())
        .collect();
    let verified = if verified.is_empty() {
        "none".to_owned()
    } else {
        verified.join(", ")
    };
    // One item per streak: the key names the streak's first failure.
    let streak = manifest
        .failures
        .first()
        .map_or_else(|| now.to_string(), |failure| failure.at.to_string());
    open_alarm(
        store,
        now,
        format!("backup.failed:{streak}"),
        "Nightly backups failed two nights in a row",
        format!(
            "Failed attempts: {}. Backups that verify: {verified}.",
            failed.join("; ")
        ),
    )
}

fn open_alarm(
    store: &mut Store,
    now: Timestamp,
    key: String,
    question: &str,
    evidence: String,
) -> Result<Attention> {
    let applied = store.attention_open(
        &Ctx::at(Actor::Daemon, now),
        Some(&key),
        NewAttention {
            card: HOST_HEALTH_CARD.into(),
            kind: AttentionKind::Recovery,
            question: question.into(),
            evidence: Some(evidence),
            recommendation: Some(
                "Find the cause in the evidence. To restore: stop the daemon, run \
                 `tiller restore <file> --dry-run`, then run it without the flag."
                    .into(),
            ),
            consequence: Some(
                "Until a backup verifies, a damaged state.db can only be restored to the \
                 newest backup that still verifies."
                    .into(),
            ),
            ..NewAttention::default()
        },
    )?;
    Ok(applied.value)
}
