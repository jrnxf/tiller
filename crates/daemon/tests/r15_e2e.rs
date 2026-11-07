//! R15 end to end (`recovery.md` R15): a detached `tiller daemon` returns an
//! ended run's worktree to the pool when it provably holds nothing, and
//! keeps one with uncommitted, untracked or unlanded work.
//!
//! The worktrees are real: a git checkout named in `config.toml`
//! `[checkouts]` and three worktrees of it, one per ended run. herdr is a
//! script that serves a snapshot where the runs' panes are gone, and the
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
use tiller_herdr::testing::{Fleet, owner};

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
    fn start(home: &Path, herdr: &Path, path: &Path) -> Self {
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
            .env("TILLER_HERDR", herdr)
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

    /// Whether a reconcile pass has finished, with or without errors.
    fn reconciled(&self) -> bool {
        self.health()["pollers"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|poller| poller["source"] == "reconcile" && !poller["last_attempt_at"].is_null())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .status();
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

    // Three builder runs, each in its own worktree, all ended.
    let mut runs = Vec::new();
    for (pane, name) in [("w2:p1", "empty"), ("w4:p1", "dirty"), ("w5:p1", "ahead")] {
        let session = fleet.worker(pane);
        let run = fleet
            .workers
            .iter()
            .find(|(s, _, _)| *s == session)
            .map(|(_, run, _)| run.clone())
            .unwrap();
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

    // herdr shows the orchestrator at work and each worker pane back at a
    // shell prompt: the runs' agents have exited.
    let mut workspaces =
        vec![serde_json::json!({"workspace_id": "w1", "label": "alpha", "number": 1})];
    let mut panes = vec![serde_json::json!({
        "pane_id": fleet.orchestrator_pane, "workspace_id": "w1",
        "agent": "claude", "agent_status": "idle",
    })];
    for (number, (_, _, pane)) in fleet.workers.iter().enumerate() {
        let workspace = pane.split(':').next().unwrap();
        workspaces.push(
            serde_json::json!({"workspace_id": workspace, "label": "claude", "number": number + 2}),
        );
        panes.push(serde_json::json!({"pane_id": pane, "workspace_id": workspace, "agent_status": "unknown"}));
    }
    let snapshot = serde_json::json!({"result": {"snapshot": {
        "version": "0.9.5", "workspaces": workspaces, "panes": panes,
    }}});
    let herdr = lab.path().join("herdr");
    script(
        &herdr,
        &format!(
            r#"echo "$*" >> '{calls}'
case "$1 $2" in
"api snapshot") echo '{snapshot}' ;;
*) echo '{{"result":{{}}}}' ;;
esac"#,
            calls = lab.path().join("herdr.log").display(),
            snapshot = snapshot
        ),
    );
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

    let daemon = Daemon::start(fleet.dir.path(), &herdr, &git_dir(lab.path()));
    until("a reconcile pass", || daemon.reconciled().then_some(()));
    let calls = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let errors = std::fs::read_to_string(fleet.dir.path().join("daemon.err")).unwrap_or_default();
    let herdr_calls = std::fs::read_to_string(lab.path().join("herdr.log")).unwrap_or_default();

    assert!(
        cleaned(&fleet, &empty.0),
        "the empty run is clean: {errors}\n{herdr_calls}"
    );
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
