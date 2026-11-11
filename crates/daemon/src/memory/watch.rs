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
//! Tiller's own commit is not an outside change. When `HEAD` is the commit
//! of a revision Tiller recorded since the last look, and every changed
//! document matches `HEAD`, the watcher takes the new state as known and
//! writes nothing: that write's own events already refreshed the board.
//!
//! A missing repo is not an error. It has no `HEAD` and no documents, and a
//! repo made or replaced later is a change like any other. An edit made
//! while the daemon is down is not one: the first look is the baseline.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path};
use std::time::Duration;

use sha2::{Digest as _, Sha256};
use tiller_agents::land::scrub::ScrubList;
use tiller_core::state::memory_repo::TillerCommit;
use tiller_core::{Actor, Ctx, Error, Result, Store, Timestamp};
use tiller_herdr::poller::{PollError, Source};

use super::apply::{Repo, TryLock};

const INTERVAL: Duration = Duration::from_secs(1);

/// The most polls a repo that never goes still waits before it is read.
const MOST_WAIT: u32 = 10;

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
            .filter(|name| {
                Path::new(name)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
            })
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

/// The repo as of the last look.
#[derive(Clone, Debug)]
struct Known {
    stamp: Stamp,
    head: Option<String>,
    /// The SHA-256 of each document's working-tree bytes.
    texts: BTreeMap<String, [u8; 32]>,
    tiller: Option<TillerCommit>,
}

/// The `memory` poller: one `memory_repo.changed` event per change made to
/// the memory repo outside Tiller.
pub struct Watcher {
    repo: Repo,
    store: Store,
    known: Option<Known>,
    /// A moved stamp, and the polls it has waited to go still.
    waiting: Option<(Stamp, u32)>,
}

impl Watcher {
    pub fn new(home: &Path, store: Store) -> Self {
        Self {
            repo: Repo::at(home),
            store,
            known: None,
            waiting: None,
        }
    }

    /// One look. Returns the documents of the change it wrote, if it wrote
    /// one (an empty list when only `HEAD` moved).
    fn look(&mut self, now: Timestamp) -> Result<Option<Vec<String>>> {
        let stamp = Stamp::take(&self.repo);
        if let Some(known) = &self.known {
            if known.stamp == stamp {
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
        // A writer's commit is read once it is whole.
        let _lock = match self.repo.try_lock()? {
            TryLock::Held(lock) => Some(lock),
            TryLock::NoRepo => None,
            TryLock::Busy => return Ok(None),
        };
        let stamp = Stamp::take(&self.repo);
        let head = self.repo.head()?;
        let texts = self.texts(&stamp)?;
        let tiller = self.store.mem_last_commit()?;
        let next = Known {
            stamp,
            head,
            texts,
            tiller,
        };
        let Some(known) = &self.known else {
            self.known = Some(next);
            return Ok(None);
        };
        let paths: BTreeSet<&String> = known.texts.keys().chain(next.texts.keys()).collect();
        let changed: Vec<String> = paths
            .into_iter()
            .filter(|path| known.texts.get(*path) != next.texts.get(*path))
            .cloned()
            .collect();
        let moved = next.head != known.head || !changed.is_empty();
        let wrote = if !moved || self.own_write(known, &next, &changed)? {
            None
        } else {
            self.store.mem_outside_edit(
                &Ctx::at(Actor::Daemon, now),
                next.head.as_deref(),
                &changed,
            )?;
            tracing::info!(files = ?changed, "the memory repo changed outside Tiller");
            Some(changed)
        };
        self.known = Some(next);
        self.waiting = None;
        Ok(wrote)
    }

    /// Each document's digest, reading only those whose stat moved.
    fn texts(&self, stamp: &Stamp) -> Result<BTreeMap<String, [u8; 32]>> {
        let mut texts = BTreeMap::new();
        for (path, stat) in &stamp.docs {
            let same = self.known.as_ref().and_then(|known| {
                (known.stamp.docs.get(path) == Some(stat))
                    .then(|| known.texts.get(path))
                    .flatten()
            });
            if let Some(digest) = same {
                texts.insert(path.clone(), *digest);
                continue;
            }
            match fs::read(self.repo.root().join(path)) {
                Ok(bytes) => {
                    texts.insert(path.clone(), Sha256::digest(&bytes).into());
                }
                // Removed since the stat: it is not there.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(Error::Internal(format!(
                        "memory repo: read {path}: {error}"
                    )));
                }
            }
        }
        Ok(texts)
    }

    /// Tiller's own commit: `HEAD` is a commit Tiller recorded since the
    /// last look, and each changed document is the file at `HEAD`.
    fn own_write(&self, known: &Known, next: &Known, changed: &[String]) -> Result<bool> {
        let recorded = next.tiller != known.tiller
            && next.head.is_some()
            && next.tiller.as_ref().map(|t| &t.commit) == next.head.as_ref();
        if !recorded {
            return Ok(false);
        }
        for path in changed {
            let file = self.repo.state(path, &ScrubList::default())?;
            if file.work_blob != file.head_blob {
                return Ok(false);
            }
        }
        Ok(true)
    }
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
