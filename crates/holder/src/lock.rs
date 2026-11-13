//! One holder per home (`terminal-holder.md` 2.2).
//!
//! The holder takes an exclusive `flock` on `holder.lock` in its run
//! directory before it binds, and writes its pid there. A second holder
//! finds the lock taken and exits, naming the pid. The lock lives as long as
//! its file handle, so it is never free while the holder lives; the live
//! upgrade (H05) keeps that fd across its re-exec.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often, and how far apart, a second holder reads the first one's pid.
/// The first one writes it just after it takes the lock, so a read in
/// between finds an old pid or none.
const PID_READS: u32 = 20;
const PID_READ_GAP: Duration = Duration::from_millis(25);

/// The lock file in a run directory.
pub fn lock_path(run_dir: &Path) -> PathBuf {
    run_dir.join("holder.lock")
}

/// The held lock. Dropping it frees the lock.
#[derive(Debug)]
pub struct HolderLock {
    file: File,
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
                return Ok(Acquired::Taken {
                    pid: holder_pid(&mut file)?,
                });
            }
            Err(TryLockError::Error(err)) => return Err(err),
        }
        // Written over the old pid and then cut to length, so the file is
        // never empty on the way.
        let pid = format!("{}\n", std::process::id());
        file.seek(SeekFrom::Start(0))?;
        file.write_all(pid.as_bytes())?;
        file.set_len(pid.len() as u64)?;
        file.flush()?;
        Ok(Acquired::Held(HolderLock { file }))
    }

    /// The lock the old program image held, handed over by the live
    /// upgrade: the lock is on the open file, so it was never free.
    pub fn adopt(file: File) -> Self {
        HolderLock { file }
    }
}

impl AsFd for HolderLock {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

/// The pid the lock file at `path` names, when it is a live process. One
/// read, no wait, and the lock is not taken: a read only probe of whether a
/// holder runs.
pub fn live_pid(path: &Path) -> Option<u32> {
    let pid = std::fs::read_to_string(path).ok()?.trim().parse().ok()?;
    is_alive(pid).then_some(pid)
}

/// The binary the holder of `run_dir` runs now, when one runs: what the
/// live upgrade compares with the stable profile path (`recovery.md` R22).
pub fn running_binary(run_dir: &Path) -> Option<PathBuf> {
    binary_of(live_pid(&lock_path(run_dir))?)
}

/// The file process `pid` runs. A file deleted since it started reads with
/// a ` (deleted)` suffix on Linux, so it never matches a live path.
#[cfg(target_os = "linux")]
pub fn binary_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// The file process `pid` runs.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn binary_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for the length passed, and `proc_pidpath`
    // writes at most that many bytes into it.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len).ok().filter(|len| *len > 0)?;
    buf.truncate(len);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf)))
}

/// The pid in a taken lock's file, once it names a live process.
fn holder_pid(file: &mut File) -> std::io::Result<Option<u32>> {
    for read in 0..PID_READS {
        if read > 0 {
            std::thread::sleep(PID_READ_GAP);
        }
        let mut text = String::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_string(&mut text)?;
        let Ok(pid) = text.trim().parse::<u32>() else {
            continue;
        };
        if is_alive(pid) {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

/// Whether `pid` is a process. One of another user's counts.
fn is_alive(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| {
            matches!(
                rustix::process::test_kill_process(pid),
                Ok(()) | Err(rustix::io::Errno::PERM)
            )
        })
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

    /// The holder's binary reads from its pid in the lock, and nothing
    /// reads when no holder holds it.
    #[test]
    fn the_running_binary_is_the_lock_holders_executable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(running_binary(dir.path()), None);
        let Acquired::Held(_lock) = HolderLock::acquire(&lock_path(dir.path())).unwrap() else {
            panic!("the take failed");
        };
        let exe = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert_eq!(running_binary(dir.path()), Some(exe));
    }
}
