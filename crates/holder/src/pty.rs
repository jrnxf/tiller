//! PTYs and the children on them (`terminal-holder.md` 2.5).
//!
//! The holder opens the PTY pair itself with `rustix` and keeps the raw
//! master fd, which the live upgrade (H05) hands over. The child gets the
//! slave as its stdio and controlling terminal, a new session, default
//! signal handling and no other fd, and exactly the environment the request
//! names plus `TERM`, `COLORTERM` and `TILLER_PANE`.
//!
//! The part between fork and exec is the only unsafe code in the holder. It
//! is checked on Linux; the macOS build compiles the same calls but is not
//! tested yet.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use rustix::fs::{Mode, OFlags};
use rustix::io::FdFlags;
use rustix::pty::OpenptFlags;
use rustix::termios::Winsize;
use tiller_panes::Size;

/// What a new child is started with.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// `argv[0]` is an absolute path: there is no `PATH` search.
    pub argv: &'a [String],
    pub cwd: &'a Path,
    /// The child's whole environment, before the holder's three additions.
    pub env: &'a BTreeMap<String, String>,
    pub size: Size,
    /// The pane id, for `TILLER_PANE`.
    pub pane: &'a str,
}

/// A started child and the master side of its PTY.
#[derive(Debug)]
pub struct Started {
    /// Non-blocking and close-on-exec.
    pub master: OwnedFd,
    pub pid: u32,
}

/// Opens a PTY pair and starts `launch` on it.
pub fn start(launch: Launch<'_>) -> io::Result<Started> {
    let Some((program, args)) = launch.argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "argv is empty"));
    };
    if !Path::new(program).is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("argv[0] {program:?} is not an absolute path"),
        ));
    }
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    // Set apart from the open: not every system takes it as an open flag.
    rustix::io::fcntl_setfd(&master, FdFlags::CLOEXEC)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    set_size(&master, launch.size)?;
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    rustix::io::ioctl_fionbio(&master, true)?;

    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(launch.env)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TILLER_PANE", launch.pane)
        .current_dir(launch.cwd)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    child_setup(&mut command);
    let child = command.spawn()?;
    // The command holds the parent's copies of the slave: drop them now, so
    // the master reads end once the child and its own children close it.
    drop(command);
    Ok(Started {
        master,
        pid: child.id(),
    })
}

/// Sets the PTY's size, which sends the child's group `SIGWINCH`.
pub fn set_size(master: impl AsFd, size: Size) -> io::Result<()> {
    rustix::termios::tcsetwinsize(
        master,
        Winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;
    Ok(())
}

/// The child's setup after fork and before exec: a new session with the
/// slave (already its fd 0) as its controlling terminal, default signal
/// dispositions and an empty mask, and every fd above 2 closed at exec.
#[allow(unsafe_code)]
fn child_setup(command: &mut Command) {
    // SAFETY: the closure runs in the child between fork and exec, so it
    // may only make async-signal-safe calls and must not allocate or take
    // locks. It calls only setsid, ioctl, signal, sigemptyset,
    // pthread_sigmask, close_range and fcntl, all async-signal-safe, on no
    // heap data.
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
            reset_signals();
            close_others_on_exec();
            Ok(())
        });
    }
}

/// Sets every signal back to its default action and unblocks them all. A
/// signal the holder ignores or blocks would stay so across exec otherwise.
#[allow(unsafe_code)]
fn reset_signals() {
    // The highest signal number on any host the holder runs on (Linux's
    // real-time range ends at 64; macOS stops at 31). A number the system
    // does not have fails and is skipped.
    const LAST_SIGNAL: libc::c_int = 64;
    for signal in 1..=LAST_SIGNAL {
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        // SAFETY: setting a signal to its default action has no handler to
        // run and touches no memory; a number that is not a signal is
        // refused with EINVAL.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
        }
    }
    // SAFETY: `set` is a local sigset_t that sigemptyset fills before
    // pthread_sigmask reads it, and a null old set is allowed.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
    }
}

/// Marks every fd above 2 close-on-exec, so the child keeps only its stdio.
/// Marking, not closing, keeps the pipe through which std reports an exec
/// failure working (it is close-on-exec already).
#[allow(unsafe_code)]
fn close_others_on_exec() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: close_range takes plain integers and only changes fd flags.
        let done = unsafe {
            libc::syscall(
                libc::SYS_close_range,
                3 as libc::c_uint,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            )
        };
        if done == 0 {
            return;
        }
    }
    // A kernel without close_range, and macOS: each fd below the fd limit,
    // and below a fixed bound when the limit is very high.
    const FD_BOUND: u64 = 65_536;
    let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile)
        .current
        .map_or(FD_BOUND, |limit| limit.min(FD_BOUND));
    let limit = libc::c_int::try_from(limit).unwrap_or(libc::c_int::MAX);
    for fd in 3..limit {
        // SAFETY: fcntl on an fd number that may not be open fails with
        // EBADF and changes nothing; on an open one it only sets its flags.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}
