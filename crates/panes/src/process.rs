//! Child processes that must end by a deadline, with everything they
//! started.
//!
//! A child runs in a process group of its own ([`own_group`]). When it has
//! to be stopped (its deadline passed, the owner interrupted the caller, or
//! waiting for it failed), [`stop`] kills the whole group and reaps the
//! child, so a grandchild such as a shell's `sleep` or a server the child
//! started does not live on. A child that exits by itself is only reaped by
//! [`wait`]: what it left behind may be meant to outlive it. [`wait_all`]
//! stops the group then too, for a child nothing of which may outlive it.

use std::io;
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Puts the child `command` spawns in a new process group, led by the child.
pub fn own_group(command: &mut Command) -> &mut Command {
    std::os::unix::process::CommandExt::process_group(command, 0)
}

/// Kills the process group `child` leads and reaps `child`. Falls back to
/// killing `child` alone when the group cannot be signalled. Call it only
/// before `child` was reaped: once it is, its id may be reused.
pub fn stop(child: &mut Child) {
    use rustix::process::{Pid, Signal, kill_process_group};
    if kill_process_group(Pid::from_child(child), Signal::KILL).is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

/// How [`wait`] ended.
#[derive(Debug)]
pub enum Ended {
    /// The child exited by itself and is reaped.
    Exited(ExitStatus),
    /// The deadline passed; the group is stopped.
    TimedOut,
    /// An [`Interrupt`] was caught; the group is stopped.
    Interrupted,
    /// Waiting for the child failed; the group is stopped all the same.
    Failed(io::Error),
}

/// Waits for `child`, checking every `poll`, and stops its group (see
/// [`stop`]) when `deadline` passes, `interrupt` is caught, or the wait
/// fails. A child that exits by itself is only reaped. `child` must lead
/// its own group ([`own_group`]).
pub fn wait(
    child: &mut Child,
    deadline: Instant,
    poll: Duration,
    interrupt: Option<&Interrupt>,
) -> Ended {
    waiting(child, deadline, poll, interrupt, false)
}

/// [`wait`], except that a child that exits by itself has its group stopped
/// too, before it is reaped, so nothing it started outlives it: a server or
/// a `sleep` it left behind, which may also hold its stdout open.
pub fn wait_all(
    child: &mut Child,
    deadline: Instant,
    poll: Duration,
    interrupt: Option<&Interrupt>,
) -> Ended {
    waiting(child, deadline, poll, interrupt, true)
}

fn waiting(
    child: &mut Child,
    deadline: Instant,
    poll: Duration,
    interrupt: Option<&Interrupt>,
    all: bool,
) -> Ended {
    loop {
        let ended = if all {
            try_wait_all(child)
        } else {
            child.try_wait()
        };
        match ended {
            Ok(Some(status)) => return Ended::Exited(status),
            Ok(None) => {
                if interrupt.is_some_and(Interrupt::caught) {
                    stop(child);
                    return Ended::Interrupted;
                }
                let now = Instant::now();
                if now >= deadline {
                    stop(child);
                    return Ended::TimedOut;
                }
                std::thread::sleep(poll.min(deadline - now));
            }
            Err(error) => {
                stop(child);
                return Ended::Failed(error);
            }
        }
    }
}

/// `child.try_wait()`, except that once `child` has exited its group is
/// killed before it is reaped. Until it is reaped `child` is a zombie, so
/// its group id cannot have been reused by then.
fn try_wait_all(child: &mut Child) -> io::Result<Option<ExitStatus>> {
    use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
    let pid = Pid::from_child(child);
    let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    if waitid(WaitId::Pid(pid), options)?.is_none() {
        return Ok(None);
    }
    // No group left (nothing the child started lives on) is not an error.
    let _ = kill_process_group(pid, Signal::KILL);
    child.wait().map(Some)
}

/// Catches SIGINT, SIGTERM and SIGHUP while it lives, so the caller can stop
/// a child in its own group: a child outside the terminal's foreground group
/// no longer gets the owner's Ctrl-C itself.
///
/// tokio never removes a signal handler, so the handlers stay installed for
/// the rest of the process. While no `Interrupt` from [`Interrupt::catch`]
/// lives, SIGINT and SIGTERM end the process as they would with no
/// handlers: the signal's default action is put back and the signal raised
/// again, so a shell sees a command the signal killed (exit code 130 for
/// SIGINT, 143 for SIGTERM). SIGHUP is then ignored: the child it was caught
/// for has ended, and what is left (recording how the child ended) is worth
/// finishing after the terminal closes, as it is under `nohup`.
pub struct Interrupt {
    caught: Arc<AtomicBool>,
    /// Whether this one came from [`Interrupt::catch`] and is in [`HELD`].
    held: bool,
}

/// The flags of the [`Interrupt`]s from [`Interrupt::catch`] that live now.
/// A signal raises each one, or ends the process when there are none.
static HELD: Mutex<Vec<Arc<AtomicBool>>> = Mutex::new(Vec::new());

/// Whether the handlers' listener runs. The first [`Interrupt::catch`]
/// that starts it keeps it for the rest of the process, as the handlers
/// are; a start that failed is tried again by the next one.
static LISTENING: Mutex<bool> = Mutex::new(false);

/// The held flags, even after a thread panicked while holding the lock.
fn held() -> std::sync::MutexGuard<'static, Vec<Arc<AtomicBool>>> {
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Starts the thread that waits for the signals, and returns once the
/// handlers are installed.
fn listen() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let (ready_tx, ready) = std::sync::mpsc::channel::<io::Result<()>>();
    std::thread::Builder::new()
        .name("interrupt".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            runtime.block_on(async move {
                let signals = signal(SignalKind::interrupt()).and_then(|int| {
                    Ok((
                        int,
                        signal(SignalKind::terminate())?,
                        signal(SignalKind::hangup())?,
                    ))
                });
                let (mut int, mut term, mut hup) = match signals {
                    Ok(signals) => {
                        let _ = ready_tx.send(Ok(()));
                        signals
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                loop {
                    let number = tokio::select! {
                        _ = int.recv() => signal_hook::consts::SIGINT,
                        _ = term.recv() => signal_hook::consts::SIGTERM,
                        _ = hup.recv() => signal_hook::consts::SIGHUP,
                    };
                    let held = held();
                    if held.is_empty() {
                        if number == signal_hook::consts::SIGHUP {
                            continue;
                        }
                        // It aborts the process when the default action
                        // cannot be put back, and returns only for a signal
                        // it does not know.
                        let _ = signal_hook::low_level::emulate_default_handler(number);
                        std::process::exit(128 + number);
                    }
                    for flag in held.iter() {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
            });
        })?;
    ready
        .recv()
        .map_err(|_| io::Error::other("the signal listener ended before it started"))?
}

impl Interrupt {
    /// Installs the handlers, the first time, and holds the signals for
    /// this `Interrupt` until it is dropped. They are in place when this
    /// returns.
    pub fn catch() -> io::Result<Self> {
        let caught = Arc::new(AtomicBool::new(false));
        // Held before the handlers exist, so no signal falls between them.
        held().push(Arc::clone(&caught));
        let mut listening = LISTENING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*listening {
            if let Err(error) = listen() {
                held().retain(|flag| !Arc::ptr_eq(flag, &caught));
                return Err(error);
            }
            *listening = true;
        }
        Ok(Self { caught, held: true })
    }

    /// An interrupt the caller raises by setting `flag`, with no signal
    /// handlers: for a child run inside a process that lives on, such as
    /// the daemon, which stops the child when it stops itself.
    pub fn flag(flag: Arc<AtomicBool>) -> Self {
        Self {
            caught: flag,
            held: false,
        }
    }

    /// Whether one of the signals arrived, or the flag was set.
    pub fn caught(&self) -> bool {
        self.caught.load(Ordering::SeqCst)
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        if self.held {
            held().retain(|flag| !Arc::ptr_eq(flag, &self.caught));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use super::*;
    use crate::testing::{eventually, gone, pid_in, reap};

    /// A shell, in its own group, that starts a `sleep` and writes the
    /// sleep's pid to `file` before running `then`.
    fn parent(file: &std::path::Path, then: &str) -> Child {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!(
                "sleep 300 & echo $! > '{}'; {then}",
                file.display()
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        own_group(&mut command);
        command.spawn().unwrap()
    }

    #[test]
    fn a_timeout_stops_the_child_and_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        let mut child = parent(&file, "sleep 300");
        let grandchild = pid_in(&file);
        let ended = wait(
            &mut child,
            Instant::now() + Duration::from_millis(200),
            Duration::from_millis(10),
            None,
        );
        let stopped = eventually(|| gone(&grandchild));
        reap(&grandchild);
        assert!(matches!(ended, Ended::TimedOut), "{ended:?}");
        assert!(stopped, "the grandchild {grandchild} outlived the timeout");
    }

    #[test]
    fn a_failed_wait_still_stops_the_group() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        let mut child = parent(&file, "exit 0");
        let grandchild = pid_in(&file);
        // Something else reaps the child behind `Child`'s back, so its next
        // wait fails.
        rustix::process::waitpid(
            Some(rustix::process::Pid::from_child(&child)),
            rustix::process::WaitOptions::empty(),
        )
        .unwrap();
        let ended = wait(
            &mut child,
            Instant::now() + Duration::from_secs(60),
            Duration::from_millis(10),
            None,
        );
        let stopped = eventually(|| gone(&grandchild));
        reap(&grandchild);
        assert!(matches!(ended, Ended::Failed(_)), "{ended:?}");
        assert!(
            stopped,
            "the grandchild {grandchild} outlived the failed wait"
        );
    }

    #[test]
    fn a_child_that_exits_is_only_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        let mut child = parent(&file, "exit 3");
        let grandchild = pid_in(&file);
        let ended = wait(
            &mut child,
            Instant::now() + Duration::from_secs(60),
            Duration::from_millis(10),
            None,
        );
        let survived = !gone(&grandchild);
        reap(&grandchild);
        assert!(
            matches!(&ended, Ended::Exited(status) if status.code() == Some(3)),
            "{ended:?}"
        );
        assert!(survived, "a clean exit must not kill what the child left");
    }

    #[test]
    fn an_interrupt_stops_the_group() {
        let interrupt = Interrupt::catch().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        let mut child = parent(&file, "sleep 300");
        let grandchild = pid_in(&file);
        // The owner's Ctrl-C reaches this process only, not the child's group.
        rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::INT)
            .unwrap();
        assert!(eventually(|| interrupt.caught()));
        let ended = wait(
            &mut child,
            Instant::now() + Duration::from_secs(60),
            Duration::from_millis(10),
            Some(&interrupt),
        );
        let stopped = eventually(|| gone(&grandchild));
        reap(&grandchild);
        assert!(matches!(ended, Ended::Interrupted), "{ended:?}");
        assert!(
            stopped,
            "the grandchild {grandchild} outlived the interrupt"
        );
    }

    /// Set in the copy of this test binary that
    /// `a_signal_after_the_interrupt_ends_the_process` runs.
    const SIGNALLED_CHILD: &str = "TILLER_TEST_SIGNALLED_CHILD";

    /// Once no `Interrupt` holds the signals, SIGTERM kills the process, as
    /// it would with no handlers, and SIGHUP does not; while a later one
    /// holds them, it catches them again. Run in a copy of this test binary,
    /// since the signal ends the process it reaches.
    #[test]
    fn a_signal_after_the_interrupt_ends_the_process() {
        use rustix::process::{Signal, getpid, kill_process};
        if std::env::var_os(SIGNALLED_CHILD).is_some() {
            drop(Interrupt::catch().unwrap());
            let again = Interrupt::catch().unwrap();
            kill_process(getpid(), Signal::INT).unwrap();
            assert!(eventually(|| again.caught()));
            drop(again);
            // The terminal closing after the run does not cut its record.
            kill_process(getpid(), Signal::HUP).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            kill_process(getpid(), Signal::TERM).unwrap();
            std::thread::sleep(Duration::from_secs(10));
            return;
        }
        let started = Instant::now();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::tests::a_signal_after_the_interrupt_ends_the_process",
                "--nocapture",
            ])
            .env(SIGNALLED_CHILD, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(signal_hook::consts::SIGTERM),
            "{status:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn wait_all_stops_the_group_of_a_child_that_exits() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        let mut child = parent(&file, "exit 3");
        let grandchild = pid_in(&file);
        let ended = wait_all(
            &mut child,
            Instant::now() + Duration::from_secs(60),
            Duration::from_millis(10),
            None,
        );
        let stopped = eventually(|| gone(&grandchild));
        reap(&grandchild);
        assert!(
            matches!(&ended, Ended::Exited(status) if status.code() == Some(3)),
            "{ended:?}"
        );
        assert!(stopped, "the grandchild {grandchild} outlived its parent");
    }

    #[test]
    fn a_raised_flag_stops_the_group() {
        let flag = Arc::new(AtomicBool::new(false));
        let interrupt = Interrupt::flag(Arc::clone(&flag));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("grandchild.pid");
        let mut child = parent(&file, "sleep 300");
        let grandchild = pid_in(&file);
        let raiser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::SeqCst);
        });
        let ended = wait(
            &mut child,
            Instant::now() + Duration::from_secs(60),
            Duration::from_millis(10),
            Some(&interrupt),
        );
        let _ = raiser.join();
        let stopped = eventually(|| gone(&grandchild));
        reap(&grandchild);
        assert!(matches!(ended, Ended::Interrupted), "{ended:?}");
        assert!(stopped, "the grandchild {grandchild} outlived the stop");
    }
}
