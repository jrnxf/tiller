//! `GET /api/v1/health` reports the tools Tiller drives (P43, `recovery.md`
//! R01): a missing tool reads as `null`, and a `no-mistakes` daemon on an
//! older version than its CLI shows as a skew with the command that fixes it.
//!
//! The daemon runs as a detached `tiller daemon` with a `PATH` of fake tools,
//! so the version reads are the real ones: `--version` for each tool, the
//! daemon's pid file, and the process list (`ps`) for the binary that pid runs.
//! Health answers before `tiller init`, so the home is empty.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

const WAIT: Duration = Duration::from_secs(30);

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

/// An executable that prints `output`, or runs `body` when it is a script.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Daemon {
    port: u16,
    pid: u32,
}

impl Daemon {
    /// A detached `tiller daemon` on a free port whose `PATH` is `bin`, then
    /// the real `git`'s directory.
    fn start(home: &Path, bin: &Path, nm_home: &Path, git_dir: &Path) -> Self {
        let out = home.join("daemon.out");
        let pid_file = home.join("daemon.pid");
        let status = Command::new("/bin/sh")
            .args([
                "-c",
                r#""$0" daemon --port 0 >"$1" 2>/dev/null </dev/null & echo $! >"$2""#,
            ])
            .arg(tiller())
            .arg(&out)
            .arg(&pid_file)
            .env_clear()
            .env("PATH", format!("{}:{}", bin.display(), git_dir.display()))
            .env("TILLER_HOME", home)
            .env("TILLER_NO_MISTAKES_HOME", nm_home)
            .env("TILLER_HERDR", "/nonexistent/herdr")
            .env("TILLER_QUOTA_AXI", bin.join("quota-axi"))
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
        let host = format!("127.0.0.1:{}", self.port);
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            stream,
            "GET /api/v1/health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        // Short bodies are not chunked by axum; a chunked one has the size
        // line first, which the JSON start brace finds.
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

fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A directory holding only a link to the real `git`, so the rest of its
/// neighbours (a real `gh-axi`, say) stay off the daemon's `PATH`.
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

#[test]
fn reports_missing_and_outdated_tools() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let nm_home = tempfile::tempdir().unwrap();

    // `gh-axi` is not installed. `quota-axi` and the `no-mistakes` CLI are.
    script(bin.path(), "quota-axi", "echo quota-axi 0.4.2");
    script(
        bin.path(),
        "no-mistakes",
        "echo 'no-mistakes version v1.106.1 (a1b2c3d) 2026-11-01T08:30:42Z'",
    );
    // The daemon still runs the binary of the version before the upgrade, and
    // the process list says so.
    let old = script(
        bin.path(),
        "no-mistakes-1.84.0",
        "echo 'no-mistakes version v1.84.0 (4822244) 2026-09-26T08:30:42Z'",
    );
    std::fs::write(
        nm_home.path().join("daemon.pid"),
        r#"{"pid":4242,"started_at":"2026-11-04T15:03:40Z"}"#,
    )
    .unwrap();
    script(
        bin.path(),
        "ps",
        &format!(
            "echo '{} daemon run --root /home/user/.no-mistakes'",
            old.display()
        ),
    );

    let daemon = Daemon::start(
        home.path(),
        bin.path(),
        nm_home.path(),
        &git_dir(bin.path()),
    );
    // The versions are read off the request path, once at start.
    let tools = until("the tool versions", || {
        let tools = daemon.health()["tools"].clone();
        tools["no-mistakes-daemon"].is_string().then_some(tools)
    });

    assert!(tools["herdr"].is_null(), "herdr is not running: {tools}");
    assert!(tools["gh-axi"].is_null(), "gh-axi is missing: {tools}");
    assert_eq!(tools["quota-axi"], "quota-axi 0.4.2");
    assert!(tools["git"].as_str().unwrap().starts_with("git version"));
    assert!(tools["no-mistakes"].as_str().unwrap().contains("v1.106.1"));
    assert!(
        tools["no-mistakes-daemon"]
            .as_str()
            .unwrap()
            .contains("v1.84.0")
    );
    let skew = tools["skew"].as_str().unwrap();
    assert!(
        skew.contains("v1.84.0, older than its CLI v1.106.1"),
        "{skew}"
    );
    assert!(skew.contains("no-mistakes daemon restart"), "{skew}");
}

#[test]
fn a_daemon_at_its_cli_version_is_no_skew() {
    let home = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let nm_home = tempfile::tempdir().unwrap();
    let version = "echo 'no-mistakes version v1.106.1 (a1b2c3d) 2026-11-01T08:30:42Z'";
    script(bin.path(), "quota-axi", "echo quota-axi 0.4.2");
    script(bin.path(), "no-mistakes", version);
    let current = script(bin.path(), "no-mistakes-current", version);
    std::fs::write(nm_home.path().join("daemon.pid"), r#"{"pid":4242}"#).unwrap();
    script(
        bin.path(),
        "ps",
        &format!(
            "echo '{} daemon run --root /home/user/.no-mistakes'",
            current.display()
        ),
    );

    let daemon = Daemon::start(
        home.path(),
        bin.path(),
        nm_home.path(),
        &git_dir(bin.path()),
    );
    let tools = until("the tool versions", || {
        let tools = daemon.health()["tools"].clone();
        tools["no-mistakes-daemon"].is_string().then_some(tools)
    });
    assert!(tools["skew"].is_null(), "{tools}");
}
