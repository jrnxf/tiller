//! One holder per home (`terminal-holder.md` 2.2).
//!
//! The holder takes an exclusive `flock` on `holder.lock` in its run
//! directory before it binds, and writes its pid there. A second holder
//! finds the lock taken and exits, naming the pid. The lock lives as long as
//! its file handle, so it is never free while the holder lives; the live
//! upgrade (H05) keeps that fd across its re-exec.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The lock file in a run directory.
pub fn lock_path(run_dir: &Path) -> PathBuf {
    run_dir.join("holder.lock")
}

/// The held lock. Dropping it frees the lock.
#[derive(Debug)]
pub struct HolderLock {
    _file: File,
}

/// What taking the lock found.
#[derive(Debug)]
pub enum Acquired {
    /// This process holds the lock now.
    Held(HolderLock),
    /// Another holder holds it. Its pid, when it has written it yet.
    Taken { pid: Option<u32> },
}

impl HolderLock {
    /// Takes the lock at `path` without waiting, and writes this process's
    /// pid into it.
    pub fn acquire(path: &Path) -> std::io::Result<Acquired> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                let mut text = String::new();
                file.read_to_string(&mut text)?;
                return Ok(Acquired::Taken {
                    pid: text.trim().parse().ok(),
                });
            }
            Err(TryLockError::Error(err)) => return Err(err),
        }
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        writeln!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Acquired::Held(HolderLock { _file: file }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_taker_finds_the_first_ones_pid_until_it_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(dir.path());
        let Acquired::Held(lock) = HolderLock::acquire(&path).unwrap() else {
            panic!("the first take failed");
        };
        let Acquired::Taken { pid } = HolderLock::acquire(&path).unwrap() else {
            panic!("the second take got the lock");
        };
        assert_eq!(pid, Some(std::process::id()));
        drop(lock);
        assert!(matches!(
            HolderLock::acquire(&path).unwrap(),
            Acquired::Held(_)
        ));
    }
}
