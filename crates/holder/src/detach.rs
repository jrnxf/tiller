//! `tiller holder --detach` (`terminal-holder.md` 2.2): starts the holder so
//! it outlives the terminal that started it. A host with no user service
//! (MDM blocks background items) has the daemon and `tiller attach` run it.
//!
//! The caller is single-threaded here, so it may fork: it forks, the child
//! calls `setsid` and forks again (so the holder is no session leader and
//! can never take a terminal as its own), and the grandchild turns its
//! stdio to `/dev/null` and a log file and goes on as the holder. The caller
//! first tries the lock, so a holder that already runs, or one in the middle
//! of an upgrade, is "already runs" and not a second start. It then waits
//! for the new holder's socket to answer.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::fs::{OFlags, fcntl_setfl};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{WaitOptions, waitpid};
use tiller_panes::paths::{SocketPathTooLong, check_socket_path};
use tiller_panes::{HolderClient, run_dir, socket_path};

use crate::lock::{Acquired, HolderLock, lock_path};

/// How long the caller waits for the new holder's socket to answer.
const READY_WITHIN: Duration = Duration::from_secs(10);
/// The gap between two tries of the socket.
const READY_GAP: Duration = Duration::from_millis(20);

/// The log of a detached holder, in the run directory.
pub fn log_path(home: &Path) -> PathBuf {
    run_dir(home).join("holder.log")
}

/// What [`detach`] made of this process.
#[derive(Debug)]
pub enum Detached {
    /// This process is the detached holder now. It goes on to run.
    Holder,
    /// This process is the caller, and its work is done.
    Caller(Started),
}

/// What the caller found once the holder was started.
#[derive(Debug, PartialEq, Eq)]
pub enum Started {
    /// A new holder answers on the socket. Its pid, when the lock names it.
    Started { pid: Option<u32> },
    /// A holder held the lock already, so none was started.
    AlreadyRuns { pid: Option<u32> },
}

#[derive(Debug, thiserror::Error)]
pub enum DetachError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathTooLong),

    #[error("{what} {path}: {source}")]
    Io {
        what: &'static str,
        path: PathBuf,
        source: io::Error,
    },

    #[error("the holder could not be forked: {0}")]
    Fork(io::Error),

    /// The new holder ended before it answered. Its reason is in the log.
    #[error("the holder ended before it answered; its log is {}", log.display())]
    Died { log: PathBuf },

    #[error("the holder did not answer within {waited:?}; its log is {}", log.display())]
    Slow { waited: Duration, log: PathBuf },
}

fn io_error(what: &'static str, path: &Path) -> impl FnOnce(io::Error) -> DetachError + use<> {
    let path = path.to_owned();
    move |source| DetachError::Io { what, path, source }
}

/// Starts a holder for `home` that outlives the caller's terminal.
///
/// Call it before any thread starts, and before logging is set up: the
/// holder's stderr is the log file from here on.
pub fn detach(home: &Path) -> Result<Detached, DetachError> {
    let socket = socket_path(home);
    check_socket_path(&socket)?;
    let run = run_dir(home);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .map_err(io_error("create", &run))?;
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))
        .map_err(io_error("chmod", &run))?;

    // The lock first: a holder that runs (or is upgrading, and keeps the
    // lock across it) is the answer, and nothing starts.
    let lock_file = lock_path(&run);
    match HolderLock::acquire(&lock_file).map_err(io_error("lock", &lock_file))? {
        Acquired::Taken { pid } => return Ok(Detached::Caller(Started::AlreadyRuns { pid })),
        Acquired::Held(lock) => drop(lock),
    }

    let log = log_path(home);
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log)
        .map_err(io_error("open", &log))?;
    let null = File::open("/dev/null").map_err(io_error("open", Path::new("/dev/null")))?;
    // Its write end stays open in the holder alone (it closes at exec, so
    // no pane's child holds it): the read end sees the end of the file when
    // the holder is gone. macOS has no `pipe2`, so close-on-exec is set
    // after: the caller is single-threaded, so no exec can come between.
    let (alive, hold) = rustix::pipe::pipe().map_err(|err| DetachError::Fork(err.into()))?;
    for end in [&alive, &hold] {
        fcntl_setfd(end, FdFlags::CLOEXEC).map_err(|err| DetachError::Fork(err.into()))?;
    }
    fcntl_setfl(&alive, OFlags::NONBLOCK).map_err(|err| DetachError::Fork(err.into()))?;

    match fork().map_err(DetachError::Fork)? {
        Fork::Child => {
            drop(alive);
            if rustix::process::setsid().is_err() {
                exit_now(1);
            }
            match fork() {
                Ok(Fork::Child) => {
                    // The grandchild: the holder.
                    if redirect_stdio(&null, &log_file).is_err() {
                        exit_now(1);
                    }
                    // Kept open for the life of the process.
                    std::mem::forget(hold);
                    Ok(Detached::Holder)
                }
                // The middle process has done its part.
                Ok(Fork::Parent(_)) => exit_now(0),
                Err(_) => exit_now(1),
            }
        }
        Fork::Parent(middle) => {
            drop(hold);
            // The middle process exits at once; reaping it leaves no zombie.
            let _ = waitpid(Some(middle), WaitOptions::empty());
            wait_ready(&socket, &lock_file, &log, &alive).map(Detached::Caller)
        }
    }
}

enum Fork {
    Child,
    Parent(rustix::process::Pid),
}

#[allow(unsafe_code)]
fn fork() -> io::Result<Fork> {
    // SAFETY: the caller is single-threaded (see `detach`), so the child
    // is a copy of the only thread and may allocate and take locks freely.
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(Fork::Child),
        pid => Ok(Fork::Parent(
            rustix::process::Pid::from_raw(pid).ok_or_else(|| io::Error::other("a zero pid"))?,
        )),
    }
}

/// Ends a forked child without running the parent's exit handlers or
/// flushing the parent's buffers a second time.
#[allow(unsafe_code)]
fn exit_now(code: i32) -> ! {
    // SAFETY: `_exit` ends the process at once and touches no memory.
    unsafe { libc::_exit(code) }
}

/// Turns stdin to `/dev/null`, and stdout and stderr to the log.
fn redirect_stdio(null: &File, log: &File) -> rustix::io::Result<()> {
    rustix::stdio::dup2_stdin(null)?;
    rustix::stdio::dup2_stdout(log)?;
    rustix::stdio::dup2_stderr(log)?;
    Ok(())
}

/// Waits until the new holder answers, or ends, or the wait runs out.
fn wait_ready(
    socket: &Path,
    lock_file: &Path,
    log: &Path,
    alive: &OwnedFd,
) -> Result<Started, DetachError> {
    let deadline = Instant::now() + READY_WITHIN;
    loop {
        // Checked before the connect: a dead holder must not cost the
        // connect's own wait on a socket nothing serves.
        if holder_gone(alive) {
            // A second `--detach` that lost the race to the lock ends at
            // once and exits 0; the winner is the one that answers.
            return match HolderLock::acquire(lock_file) {
                Ok(Acquired::Taken { pid }) => Ok(Started::AlreadyRuns { pid }),
                _ => Err(DetachError::Died {
                    log: log.to_owned(),
                }),
            };
        }
        if HolderClient::connect(socket, "cli").is_ok_and(|client| client.ping().is_ok()) {
            return Ok(Started::Started {
                pid: pid_in(lock_file),
            });
        }
        if Instant::now() >= deadline {
            return Err(DetachError::Slow {
                waited: READY_WITHIN,
                log: log.to_owned(),
            });
        }
        std::thread::sleep(READY_GAP);
    }
}

/// Whether the holder's end of the pipe is closed.
fn holder_gone(alive: &OwnedFd) -> bool {
    let mut byte = [0u8; 1];
    // `File` reads an fd; the borrowed clone is not closed twice.
    match alive.try_clone().map(File::from) {
        Ok(mut file) => matches!(file.read(&mut byte), Ok(0)),
        Err(_) => false,
    }
}

/// The pid the lock file names.
fn pid_in(lock_file: &Path) -> Option<u32> {
    std::fs::read_to_string(lock_file).ok()?.trim().parse().ok()
}
