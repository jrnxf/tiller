//! R15 end to end (`recovery.md` R15): a detached `tiller daemon` returns an
//! ended run's worktree to the pool when it provably holds nothing, and
//! keeps one with uncommitted, untracked or unlanded work.
//!
//! The worktrees are real: a git checkout named in `config.toml`
//! `[checkouts]` and three worktrees of it, one per ended run. A `tiller holder`
//! in the fleet's home holds each run's pane with its agent exited, and the
//! `[spawn] treehouse` is a script that logs each call it gets.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use tiller_core::domain::RunOutcome;
use tiller_core::state::runs::EndRun;
use tiller_panes::testing::{Fleet, owner};
use tiller_panes::{PaneId, PaneState, Panes, Spawn};

const WAIT: Duration = Duration::from_secs(60);

fn tiller() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).unwrap();
    let binary = dir.join("tiller");
    assert!(
        binary.exists(),
        "{} is missing; build the workspace first (cargo build -p tiller-cli)",
        binary.display()
    );
    binary
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", text]);
}

fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// A directory holding only a link to the real `git`.
fn git_dir(into: &Path) -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let path = String::from_utf8(output.stdout).unwrap();
    let dir = into.join("git-only");
    std::fs::create_dir(&dir).unwrap();
    std::os::unix::fs::symlink(path.trim(), dir.join("git")).unwrap();
    dir
}

struct Daemon {
    port: u16,
    pid: u32,
}

impl Daemon {
    fn start(home: &Path, path: &Path) -> Self {
        let out = home.join("daemon.out");
        let pid_file = home.join("daemon.pid");
        let status = Command::new("/bin/sh")
            .args([
                "-c",
                r#""$0" daemon --port 0 >"$1" 2>"$3" </dev/null & echo $! >"$2""#,
            ])
            .arg(tiller())
            .arg(&out)
            .arg(&pid_file)
            .arg(home.join("daemon.err"))
            .env_clear()
            .env("PATH", path)
            .env("TILLER_HOME", home)
            .env("TILLER_QUOTA_AXI", "/nonexistent/quota-axi")
            .env("TILLER_LOG", "warn")
            .status()
            .unwrap();
        assert!(status.success());
        let line = until("the daemon's listening line", || {
            std::fs::read_to_string(&out)
                .ok()
                .and_then(|text| text.lines().next().map(str::to_owned))
        });
        let port = line
            .strip_prefix("listening on http://127.0.0.1:")
            .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
            .parse()
            .unwrap();
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        Self { port, pid }
    }

    fn health(&self) -> Value {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            stream,
            "GET /api/v1/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            self.port
        )
        .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let start = body.find('{').unwrap();
        let end = body.rfind('}').unwrap();
        serde_json::from_str(&body[start..=end]).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .status();
    }
}

/// A `tiller holder` in `home`, stopped when dropped: its listing is what
/// shows the runs' panes are gone.
struct Holder(std::process::Child);

impl Holder {
    fn start(home: &Path) -> Self {
        let child = Command::new(tiller())
            .arg("holder")
            .env_clear()
            .env("PATH", "")
            .env("HOME", home)
            .env("TILLER_HOME", home)
            .env("TILLER_LOG", "warn")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let socket = tiller_panes::socket_path(home);
        until("the holder's socket", || {
            tiller_panes::HolderClient::connect(&socket, "test")
                .ok()
                .map(|_| ())
        });
        Self(child)
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status();
        let _ = self.0.wait();
    }
}

fn cleaned(fleet: &Fleet, run: &str) -> bool {
    fleet
        .open()
        .conn()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE entity = 'run' AND entity_id = ?1 AND kind = 'run.cleaned')",
            [run],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn daemon_returns_an_ended_runs_empty_worktree_and_keeps_the_rest() {
    let mut fleet = Fleet::new();
    let lab = tempfile::tempdir().unwrap();
    let checkout = lab.path().join("checkouts").join("alpha");
    std::fs::create_dir_all(&checkout).unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["config", "user.email", "dev@example.com"]);
    git(&checkout, &["config", "user.name", "Dev"]);
    commit(&checkout, "base.txt", "base");

    // Three builder runs, each in its own worktree, all ended. Each run's
    // pane is in the holder, its agent exited.
    let _holder = Holder::start(fleet.dir.path());
    let panes =
        tiller_panes::HolderClient::connect(tiller_panes::socket_path(fleet.dir.path()), "test")
            .unwrap();
    let mut runs = Vec::new();
    for name in ["empty", "dirty", "ahead"] {
        let session = fleet.worker(PaneId::generate().as_str());
        let run = fleet
            .workers
            .iter()
            .find(|(s, _, _)| *s == session)
            .map(|(_, run, _)| run.clone())
            .unwrap();
        let pane = panes
            .spawn(&Spawn {
                key: session.clone(),
                argv: vec!["/bin/sh".into(), "-c".into(), "exit 0".into()],
                cwd: lab.path().to_string_lossy().into_owned(),
                env: Default::default(),
                size: None,
                meta: [("session", session.as_str()), ("run", run.as_str())]
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .into(),
            })
            .unwrap()
            .pane;
        fleet.bind(&session, pane.as_str());
        until("the run's agent to exit", || {
            panes
                .get(&pane)
                .unwrap()
                .filter(|pane| pane.state == PaneState::Exited)
        });
        let worktree = lab.path().join("pool").join(name);
        let branch = format!("tiller/{name}");
        git(
            &checkout,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                worktree.to_str().unwrap(),
            ],
        );
        fleet
            .store
            .conn()
            .execute(
                "UPDATE runs SET worktree_path = ?2, branch = ?3 WHERE id = ?1",
                rusqlite::params![run, worktree.to_string_lossy(), branch],
            )
            .unwrap();
        fleet
            .store
            .run_end(
                &owner(),
                &run,
                EndRun {
                    outcome: RunOutcome::Abandoned,
                    note: Some("not needed".into()),
                },
            )
            .unwrap();
        runs.push((run, worktree));
    }
    let (empty, dirty, ahead) = (&runs[0], &runs[1], &runs[2]);
    // Untracked work, even where the repository hides untracked files.
    git(&dirty.1, &["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(dirty.1.join("notes.md"), "draft").unwrap();
    // A commit that is on no remote and not on main.
    git(&ahead.1, &["config", "user.email", "dev@example.com"]);
    git(&ahead.1, &["config", "user.name", "Dev"]);
    commit(&ahead.1, "feature.txt", "feature");

    let log = lab.path().join("treehouse.log");
    let treehouse = lab.path().join("treehouse");
    script(&treehouse, &format!("echo \"$*\" >> '{}'", log.display()));
    std::fs::write(
        fleet.dir.path().join("config.toml"),
        format!(
            "[spawn]\ntreehouse = [{:?}]\n\n[checkouts]\nalpha = {:?}\n",
            treehouse.to_str().unwrap(),
            checkout.to_str().unwrap()
        ),
    )
    .unwrap();

    let daemon = Daemon::start(fleet.dir.path(), &git_dir(lab.path()));
    let calls = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    // A pass may need more than one go (a pane probe, a holder starting), so
    // wait for the return itself, then for one more pass.
    until("the empty run's return", || {
        (cleaned(&fleet, &empty.0) && !calls().is_empty()).then_some(())
    });
    let passes = |health: &Value| {
        health["pollers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|poller| poller["source"] == "reconcile")
            .map(|poller| poller["last_attempt_at"].clone())
    };
    let seen = passes(&daemon.health());
    until("one more reconcile pass", || {
        (passes(&daemon.health()) != seen).then_some(())
    });
    // The running daemon also runs the nightly backup (`recovery.md` R10):
    // its first pass schedules the first night in the manifest.
    let backup = daemon.health()["pollers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|poller| poller["source"] == "backup")
        .cloned();
    assert!(backup.is_some(), "the daemon runs the backup worker");
    until("the first night scheduled", || {
        std::fs::read_to_string(fleet.dir.path().join("backups/manifest.json"))
            .ok()
            .and_then(|manifest| serde_json::from_str::<Value>(&manifest).ok())
            .filter(|manifest| manifest["next_night"].is_string())
    });

    assert_eq!(
        calls(),
        vec![format!(
            "return {} --if-lease-holder tiller:{}",
            empty.1.display(),
            empty.0
        )],
        "only the empty worktree goes back, under its run's lease"
    );
    for (run, worktree) in [dirty, ahead] {
        assert!(!cleaned(&fleet, run), "{} is kept", worktree.display());
        assert!(worktree.join("base.txt").exists());
    }
    assert!(dirty.1.join("notes.md").exists());
}
