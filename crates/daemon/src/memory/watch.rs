//! The memory repo watcher (U16; `memory.md` section 12, `protocol.md`
//! 7.5).
//!
//! An edit made in the memory repo outside the board (an editor's save, a
//! `git pull`, a commit by hand) moves no row, so the change stream would
//! never carry it and an open board would keep the old text. Once a second
//! the watcher stats a few paths, starts no process and reads nothing but
//! `.git/HEAD`: the repo and its `.git`, the ref `HEAD` names,
//! `packed-refs`, `fleet.md` and every `projects/*.md`. When that stamp
//! moves, it waits until one poll sees no further move, so a pull that
//! rewrites many files is one change. Then, under the repo lock (taken only
//! when free), it reads `HEAD` and the text of each document whose stamp
//! moved, and writes `mem.outside_edit` when either changed.
//!
//! A stat cannot vouch for a document changed in the same tick of the file
//! clock as the read: a same-length rewrite in that tick leaves the stat as
//! it was. Such a document is racy (as git calls it), and each poll reads
//! its bytes again until its stat is older than [`RACY`], so the stat can
//! vouch for it from then on.
//!
//! Tiller's own commit is not an outside change. When `HEAD` is the commit
//! of a revision Tiller recorded since the last look, a changed document
//! that matches `HEAD` is Tiller's write: that write's own events already
//! refreshed the board. The change names only the documents that differ
//! from `HEAD`, and when none does the watcher writes nothing. That check
//! reads `HEAD` with one git process, after the lock is released.
//!
//! A missing repo is not an error. It has no `HEAD` and no documents, and a
//! repo made or replaced later is a change like any other. A document that
//! cannot be read is not an error either: it has a digest of its own, so it
//! is a change, and the other documents are still watched.
//!
//! What a look read (`HEAD`, each document's digest, Tiller's last commit)
//! is kept in the store (`mem.watch`, and with each `mem.outside_edit`). A
//! daemon that starts again compares its first look with the kept one, so
//! an edit made while it was down is a change too. With nothing kept, the
//! first look is the baseline.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tiller_core::memory::sha256_hex;
use tiller_core::state::memory_repo::TillerCommit;
use tiller_core::{Actor, Ctx, Error, Result, Store, Timestamp};
use tiller_herdr::poller::{PollError, Source};

use super::apply::{Repo, TryLock, checked};

const INTERVAL: Duration = Duration::from_secs(1);

/// The most polls a repo that never goes still waits before it is read.
const MOST_WAIT: u32 = 10;

/// How long after a document's last change its stat cannot vouch for its
/// bytes: wider than the coarsest file clock the repo is likely to sit on
/// (one second, two on FAT).
const RACY: Duration = Duration::from_secs(2);

const FLEET: &str = "fleet.md";
const PROJECTS: &str = "projects";

/// What a stat says about one file: enough to see it rewritten, replaced or
/// touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stat {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Stat {
    fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        meta.is_file().then(|| Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        })
    }

    /// Changed too close to `at` for the stat to vouch for the bytes.
    fn racy(&self, at: SystemTime) -> bool {
        let time = |(secs, nanos): (i64, i64)| {
            let secs = u64::try_from(secs).ok()?;
            let nanos = u32::try_from(nanos).ok()?;
            SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
        };
        let last = time(self.mtime).max(time(self.ctime));
        match last.and_then(|last| last.checked_add(RACY)) {
            Some(settled) => settled > at,
            // A time the clock cannot name vouches for nothing.
            None => true,
        }
    }
}

/// A directory by device and inode, so a replaced repo shows.
fn dir_id(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok()?;
    meta.is_dir().then(|| (meta.dev(), meta.ino()))
}

/// The repo as a handful of stats, cheap enough to take every second.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Stamp {
    root: Option<(u64, u64)>,
    git: Option<(u64, u64)>,
    /// `.git/HEAD`'s bytes: the branch it names, or a detached commit.
    head: Option<Vec<u8>>,
    /// The ref file `HEAD` names, and `packed-refs`.
    refs: [Option<Stat>; 2],
    /// Each document, by its canonical path.
    docs: BTreeMap<String, Stat>,
}

impl Stamp {
    fn take(repo: &Repo) -> Self {
        let root = repo.root();
        let git = repo.git_dir();
        let head = fs::read(git.join("HEAD")).ok();
        let named = head
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| text.trim().strip_prefix("ref: "))
            .and_then(|name| checked(name).ok())
            .and_then(|name| Stat::of(&git.join(name)));
        let mut docs = BTreeMap::new();
        if let Some(stat) = Stat::of(&root.join(FLEET)) {
            docs.insert(FLEET.to_owned(), stat);
        }
        if let Ok(entries) = fs::read_dir(root.join(PROJECTS)) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.starts_with('.') || !name.ends_with(".md") {
                    continue;
                }
                if let Some(stat) = Stat::of(&entry.path()) {
                    docs.insert(format!("{PROJECTS}/{name}"), stat);
                }
            }
        }
        Self {
            root: dir_id(root),
            git: dir_id(&git),
            head,
            refs: [named, Stat::of(&git.join("packed-refs"))],
            docs,
        }
    }
}

/// What a look read, and what the store keeps of it across a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    head: Option<String>,
    /// The SHA-256 (hex) of each document's working-tree bytes.
    texts: BTreeMap<String, String>,
    tiller: Option<TillerCommit>,
}

/// The repo as of the last look.
#[derive(Clone, Debug)]
struct Known {
    stamp: Stamp,
    seen: Seen,
    /// The documents whose stat could not vouch for their bytes when they
    /// were read, so each poll reads them again.
    racy: BTreeSet<String>,
}

/// The `memory` poller: one `memory_repo.changed` event per change made to
/// the memory repo outside Tiller.
pub struct Watcher {
    repo: Repo,
    store: Store,
    known: Option<Known>,
    /// What the store keeps of the last look, once it is read or written.
    kept: Option<Option<Seen>>,
    /// A moved stamp, and the polls it has waited to go still.
    waiting: Option<(Stamp, u32)>,
}

impl Watcher {
    pub fn new(home: &Path, store: Store) -> Self {
        Self {
            repo: Repo::at(home),
            store,
            known: None,
            kept: None,
            waiting: None,
        }
    }

    /// One look. Returns the documents of the change it wrote, if it wrote
    /// one (an empty list when only `HEAD` moved).
    fn look(&mut self, now: Timestamp) -> Result<Option<Vec<String>>> {
        let stamp = Stamp::take(&self.repo);
        if let Some(known) = &mut self.known {
            if known.stamp == stamp && !racy_moved(&self.repo, known, &stamp) {
                self.waiting = None;
                return Ok(None);
            }
            // A burst is read once it is over.
            let still = match &mut self.waiting {
                Some((waited, polls)) if *waited != stamp && *polls < MOST_WAIT => {
                    *waited = stamp;
                    *polls += 1;
                    false
                }
                Some(_) => true,
                None => {
                    self.waiting = Some((stamp, 1));
                    false
                }
            };
            if !still {
                return Ok(None);
            }
        }
        let Some(next) = self.read()? else {
            return Ok(None);
        };
        let before = match &self.known {
            Some(known) => Some(known.seen.clone()),
            None => self.kept()?,
        };
        let wrote = match before {
            // Nothing to compare with: this look is the baseline.
            None => None,
            Some(before) => self.compare(&before, &next.seen, now)?,
        };
        if wrote.is_none() {
            self.keep(&next.seen, now)?;
        }
        self.known = Some(next);
        self.waiting = None;
        Ok(wrote)
    }

    /// The repo as it is now, read under the lock so a writer's commit is
    /// read once it is whole; `None` while a writer holds it. The lock is
    /// released when the read ends.
    fn read(&self) -> Result<Option<Known>> {
        let _lock = match self.repo.try_lock()? {
            TryLock::Held(lock) => Some(lock),
            TryLock::NoRepo => None,
            TryLock::Busy => return Ok(None),
        };
        // Taken before the stat, so a document changed while it is read
        // counts as racy.
        let read_at = SystemTime::now();
        let stamp = Stamp::take(&self.repo);
        let head = self.repo.head()?;
        let texts = self.texts(&stamp);
        let tiller = self.store.mem_last_commit()?;
        let racy = stamp
            .docs
            .iter()
            .filter(|(_, stat)| stat.racy(read_at))
            .map(|(path, _)| path.clone())
            .collect();
        Ok(Some(Known {
            stamp,
            seen: Seen {
                head,
                texts,
                tiller,
            },
            racy,
        }))
    }

    /// Writes `mem.outside_edit` when `next` moved from `before` and the
    /// move is not only Tiller's own commit; returns the documents changed
    /// outside Tiller.
    fn compare(
        &mut self,
        before: &Seen,
        next: &Seen,
        now: Timestamp,
    ) -> Result<Option<Vec<String>>> {
        let paths: BTreeSet<&String> = before.texts.keys().chain(next.texts.keys()).collect();
        let changed: Vec<String> = paths
            .into_iter()
            .filter(|path| before.texts.get(*path) != next.texts.get(*path))
            .cloned()
            .collect();
        if next.head == before.head && changed.is_empty() {
            return Ok(None);
        }
        let changed = match self.tiller_commit(before, next) {
            Some(commit) => {
                let outside = self.not_at(commit, next, changed)?;
                if outside.is_empty() {
                    return Ok(None);
                }
                outside
            }
            None => changed,
        };
        self.store.mem_outside_edit(
            &Ctx::at(Actor::Daemon, now),
            next.head.as_deref(),
            &changed,
            &encode(next)?,
        )?;
        self.kept = Some(Some(next.clone()));
        tracing::info!(files = ?changed, "the memory repo changed outside Tiller");
        Ok(Some(changed))
    }

    /// The last look the store keeps, read once. One that does not parse is
    /// dropped: it is rebuildable, and the look is the baseline then.
    fn kept(&mut self) -> Result<Option<Seen>> {
        if let Some(kept) = &self.kept {
            return Ok(kept.clone());
        }
        let kept = match self.store.mem_watch_seen()? {
            None => None,
            Some(text) => match serde_json::from_str(&text) {
                Ok(seen) => Some(seen),
                Err(error) => {
                    tracing::warn!(%error, "the memory watcher's last look does not parse; this look is the baseline");
                    None
                }
            },
        };
        self.kept = Some(kept.clone());
        Ok(kept)
    }

    /// Keeps `seen` as the last look, when the store does not hold it yet.
    fn keep(&mut self, seen: &Seen, now: Timestamp) -> Result<()> {
        if self.kept()?.as_ref() == Some(seen) {
            return Ok(());
        }
        self.store
            .mem_watch(&Ctx::at(Actor::Daemon, now), &encode(seen)?)?;
        self.kept = Some(Some(seen.clone()));
        Ok(())
    }

    /// Each document's digest, reading only those whose stat moved or could
    /// not vouch for them at the last look.
    fn texts(&self, stamp: &Stamp) -> BTreeMap<String, String> {
        let mut texts = BTreeMap::new();
        for (path, stat) in &stamp.docs {
            let same = self.known.as_ref().and_then(|known| {
                (known.stamp.docs.get(path) == Some(stat) && !known.racy.contains(path))
                    .then(|| known.seen.texts.get(path))
                    .flatten()
            });
            if let Some(digest) = same {
                texts.insert(path.clone(), digest.clone());
                continue;
            }
            if let Some(digest) = digest(&self.repo, path) {
                texts.insert(path.clone(), digest);
            }
        }
        texts
    }

    /// The commit at `HEAD` when it is a commit Tiller recorded since the
    /// last look.
    fn tiller_commit<'a>(&self, before: &Seen, next: &'a Seen) -> Option<&'a str> {
        let head = next.head.as_deref()?;
        let recorded = next.tiller != before.tiller
            && next.tiller.as_ref().map(|t| t.commit.as_str()) == Some(head);
        recorded.then_some(head)
    }

    /// The documents of `changed` whose bytes, as `next` read them, are not
    /// the file at `commit`: those Tiller's commit does not explain. One git
    /// process reads them all.
    fn not_at(&self, commit: &str, next: &Seen, changed: Vec<String>) -> Result<Vec<String>> {
        let at = self.repo.bytes_at(commit, &changed)?;
        Ok(changed
            .into_iter()
            .zip(at)
            .filter(|(path, bytes)| {
                bytes.as_deref().map(sha256_hex).as_ref() != next.texts.get(path)
            })
            .map(|(path, _)| path)
            .collect())
    }
}

/// A document's digest; `None` when it is not there. One that cannot be read
/// has a digest of its own, so it is a change and the look goes on.
fn digest(repo: &Repo, path: &str) -> Option<String> {
    match fs::read(repo.root().join(path)) {
        Ok(bytes) => Some(sha256_hex(&bytes)),
        // Removed since the stat: it is not there.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            tracing::warn!(path, %error, "the memory watcher cannot read a document");
            Some(format!("unreadable: {:?}", error.kind()))
        }
    }
}

/// Reads each racy document of a still stamp again. True when one's bytes
/// moved; otherwise drops those whose stat can vouch for them now.
fn racy_moved(repo: &Repo, known: &mut Known, stamp: &Stamp) -> bool {
    for path in &known.racy {
        if digest(repo, path).as_ref() != known.seen.texts.get(path) {
            return true;
        }
    }
    let now = SystemTime::now();
    known
        .racy
        .retain(|path| stamp.docs.get(path).is_some_and(|stat| stat.racy(now)));
    false
}

fn encode(seen: &Seen) -> Result<String> {
    serde_json::to_string(seen)
        .map_err(|error| Error::Internal(format!("memory watcher: encode its last look: {error}")))
}

impl Source for Watcher {
    fn name(&self) -> String {
        "memory".to_owned()
    }

    fn interval(&self) -> Duration {
        INTERVAL
    }

    fn poll(&mut self, now: Timestamp) -> std::result::Result<(), PollError> {
        self.look(now)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
