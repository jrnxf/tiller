//! Test helpers (feature `testing`): a small fleet in a file-backed
//! `state.db` (project `alpha` with a live orchestrator, and builder runs
//! bound to panes), a PTY with a stand-in shell, and process checks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tiller_core::domain::{Boundary, CardKind, Delivery, Effort, RunRole};
use tiller_core::state::cards::NewCard;
use tiller_core::state::projects::{Init, NewProject, NewRemoteHost};
use tiller_core::state::runs::{Bind, NewOrchestrator, NewRun, SpawnChecks};
use tiller_core::{Actor, Ctx, Store, Timestamp};

/// A fixed instant plus `seconds`, so tests read as a timeline.
pub fn at(seconds: i64) -> Timestamp {
    Timestamp::from_millis(1_900_000_000_000 + seconds * 1000).unwrap()
}

pub fn owner() -> Ctx {
    Ctx::new(Actor::Owner)
}

pub struct Fleet {
    pub dir: TempDir,
    pub db: PathBuf,
    pub store: Store,
    /// The local host's id.
    pub host: String,
    pub orchestrator_session: String,
    /// The orchestrator's pane.
    pub orchestrator_pane: String,
    /// `(session id, run id, pane id)` per worker, in creation order.
    pub workers: Vec<(String, String, String)>,
}

impl Default for Fleet {
    fn default() -> Self {
        Self::new()
    }
}

impl Fleet {
    /// A fleet whose orchestrator sits in pane `w1:p1`.
    pub fn new() -> Self {
        Self::with_orchestrator_pane("w1:p1")
    }

    pub fn with_orchestrator_pane(pane_id: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let mut store = Store::open(&db).unwrap();
        let host = store
            .init(
                &owner(),
                Init {
                    host_label: "test-host".into(),
                    lane_capacity: 4,
                    ram_floor_mb: None,
                    disk_floor_mb: None,
                    boundary: Boundary::Personal,
                },
            )
            .unwrap()
            .value
            .host
            .id;
        store
            .project_add(
                &owner(),
                NewProject {
                    id: "alpha".into(),
                    name: "Alpha".into(),
                    repo: Some("example/alpha".into()),
                    remote: None,
                    default_branch: None,
                    delivery: Delivery::DirectPr,
                    checks: None,
                    capacity: Some(4),
                    deploy_workflow: None,
                    deploy_environment: None,
                    deploy_timeout_minutes: None,
                },
            )
            .unwrap();
        let orchestrator_pane = pane_id.to_owned();
        let orchestrator_session = store
            .session_start(
                &owner(),
                NewOrchestrator {
                    project_id: "alpha".into(),
                    pane_id: Some(orchestrator_pane.clone()),
                    claude_session_id: Some("claude-orchestrator".into()),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap()
            .value
            .session
            .id;
        Self {
            dir,
            db,
            store,
            host,
            orchestrator_session,
            orchestrator_pane,
            workers: Vec::new(),
        }
    }

    /// Another connection to the same database, as a second process has.
    pub fn open(&self) -> Store {
        Store::open(&self.db).unwrap()
    }

    /// A builder run on a new card, its session bound to `pane_id`. Returns
    /// the session id.
    pub fn worker(&mut self, pane_id: &str) -> String {
        let card = self
            .store
            .card_create(
                &Ctx::new(Actor::Session(self.orchestrator_session.clone())),
                None,
                NewCard {
                    project_id: "alpha".into(),
                    kind: CardKind::Ship,
                    title: format!("work in {pane_id}"),
                    ..NewCard::default()
                },
                None,
            )
            .unwrap()
            .value;
        let started = self
            .store
            .run_start(
                &owner(),
                None,
                NewRun {
                    worktree_path: Some(format!("/home/user/worktrees/{}", card.display_id)),
                    card: card.display_id,
                    role: RunRole::Builder,
                    model: "test-model".into(),
                    effort: Effort::Medium,
                    host_id: None,
                    branch: None,
                    base_branch: None,
                    base_sha: None,
                    config_hash: None,
                    checks: SpawnChecks::default(),
                },
            )
            .unwrap()
            .value;
        let session = started.session.id;
        self.bind(&session, pane_id);
        self.workers
            .push((session.clone(), started.run.id, pane_id.to_owned()));
        session
    }

    /// Binds `session` to `pane_id`, as its `SessionStart` hook does.
    pub fn bind(&mut self, session: &str, pane_id: &str) {
        self.store
            .run_bind_session(
                &Ctx::new(Actor::Hook),
                Bind {
                    session_id: session.into(),
                    claude_session_id: format!("claude-{session}"),
                    pane_id: Some(pane_id.into()),
                    transcript_path: None,
                    source: "startup".into(),
                },
            )
            .unwrap();
    }

    /// A remote host. Returns its host id.
    pub fn remote_host(&self, label: &str) -> String {
        let mut store = self.open();
        store
            .host_add(
                &owner(),
                NewRemoteHost {
                    label: label.into(),
                },
            )
            .unwrap()
            .value
            .id
    }

    /// Lays the fleet out on `panes` as a fresh server would (see
    /// [`Place`]).
    pub fn place<P: Place + ?Sized>(&self, panes: &P) {
        panes.place(self);
    }
}

/// A pane server a [`Fleet`] can lay itself out on, such as the fake herdr.
pub trait Place {
    /// Opens the orchestrator's pane, an unrelated one, then each worker's.
    fn place(&self, fleet: &Fleet);
}

/// A PTY with a stand-in shell on it, as herdr gives each pane: the shell
/// reads from the PTY's terminal, so its stdin tells the PTY's window size.
/// The shell is killed when this drops.
pub struct Pty {
    controller: std::os::fd::OwnedFd,
    shell: std::process::Child,
}

impl Pty {
    pub fn new(cols: u16, rows: u16) -> Self {
        use rustix::pty::OpenptFlags;
        let controller = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        rustix::pty::grantpt(&controller).unwrap();
        rustix::pty::unlockpt(&controller).unwrap();
        let path = rustix::pty::ptsname(&controller, Vec::new()).unwrap();
        // Never the test process's controlling terminal, which would hang
        // it up when the controller closes.
        let flags = rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY;
        let terminal = rustix::fs::open(path.as_c_str(), flags, rustix::fs::Mode::empty()).unwrap();
        let shell = std::process::Command::new("sleep")
            .arg("600")
            .stdin(terminal)
            .spawn()
            .unwrap();
        let pty = Self { controller, shell };
        pty.resize(cols, rows);
        pty
    }

    /// The path of the PTY's terminal.
    pub fn path(&self) -> PathBuf {
        let path = rustix::pty::ptsname(&self.controller, Vec::new()).unwrap();
        PathBuf::from(path.into_string().unwrap())
    }

    pub fn shell_pid(&self) -> u32 {
        self.shell.id()
    }

    /// Resizes the PTY, as a herdr client does when its window changes.
    pub fn resize(&self, cols: u16, rows: u16) {
        let size = rustix::termios::Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        rustix::termios::tcsetwinsize(&self.controller, size).unwrap();
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.shell.kill();
        let _ = self.shell.wait();
    }
}

/// Whether `pid` has exited: no such process, or one only waiting to be
/// reaped.
pub fn gone(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .expect("ps runs");
    let stat = String::from_utf8_lossy(&out.stdout);
    stat.trim().is_empty() || stat.trim_start().starts_with('Z')
}

/// Polls `what` for up to five seconds: a process killed with its group
/// can still show for a moment after its parent is reaped.
pub fn eventually(what: impl Fn() -> bool) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(5) {
        if what() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// The pid a stand-in wrote to `file`, once it is there.
pub fn pid_in(file: &Path) -> String {
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
pub fn reap(pid: &str) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", pid])
        .status();
}
