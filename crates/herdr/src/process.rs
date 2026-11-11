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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
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
/// The handlers stay installed after it is dropped (tokio never removes
/// them), so those signals no longer end the process: hold one only in a
/// command that exits soon after the child ends.
pub struct Interrupt {
    caught: Arc<AtomicBool>,
    done: Option<tokio::sync::oneshot::Sender<()>>,
    listener: Option<JoinHandle<()>>,
}

impl Interrupt {
    /// Installs the handlers. They are in place when this returns.
    pub fn catch() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        let caught = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&caught);
        let (ready_tx, ready) = std::sync::mpsc::channel::<io::Result<()>>();
        let (done, done_rx) = tokio::sync::oneshot::channel::<()>();
        let listener = std::thread::spawn(move || {
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
                tokio::select! {
                    _ = int.recv() => flag.store(true, Ordering::SeqCst),
                    _ = term.recv() => flag.store(true, Ordering::SeqCst),
                    _ = hup.recv() => flag.store(true, Ordering::SeqCst),
                    _ = done_rx => {}
                }
            });
        });
        ready
            .recv()
            .map_err(|_| io::Error::other("the signal listener ended before it started"))??;
        Ok(Self {
            caught,
            done: Some(done),
            listener: Some(listener),
        })
    }

    /// An interrupt the caller raises by setting `flag`, with no signal
    /// handlers: for a child run inside a process that lives on, such as
    /// the daemon, which stops the child when it stops itself.
    pub fn flag(flag: Arc<AtomicBool>) -> Self {
        Self {
            caught: flag,
            done: None,
            listener: None,
        }
    }

    /// Whether one of the signals arrived, or the flag was set.
    pub fn caught(&self) -> bool {
        self.caught.load(Ordering::SeqCst)
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
    }
}

/// Test helpers shared with the CLI adapter's tests.
#[cfg(test)]
pub(crate) mod tests_support {
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// Whether `pid` has exited: no such process, or one only waiting to be
    /// reaped.
    pub(crate) fn gone(pid: &str) -> bool {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .expect("ps runs");
        let stat = String::from_utf8_lossy(&out.stdout);
        stat.trim().is_empty() || stat.trim_start().starts_with('Z')
    }

    /// Polls `what` for up to five seconds.
    pub(crate) fn eventually(what: impl Fn() -> bool) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if what() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The pid a stand-in wrote to `file`, once it is there.
    pub(crate) fn pid_in(file: &Path) -> String {
        assert!(
            eventually(|| std::fs::read_to_string(file).is_ok_and(|s| s.ends_with('\n'))),
            "{} was never written",
            file.display()
        );
        std::fs::read_to_string(file)
            .expect("pid file")
            .trim()
            .to_owned()
    }

    /// Kills `pid` if it still runs, so a failed test leaves nothing behind.
    pub(crate) fn reap(pid: &str) {
        let _ = Command::new("kill").args(["-KILL", pid]).status();
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;

    use super::tests_support::{eventually, gone, pid_in, reap};
    use super::*;

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
