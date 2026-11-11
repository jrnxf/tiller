//! U16: an edit made in the memory repo outside the board reaches an open
//! board over the change stream (`protocol.md` 7.5, `memory.md` section 12).
//!
//! The daemon runs through the real binary with only `git` on its `PATH`,
//! so no test reaches a real herdr server, `gh` or `quota-axi`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::World;

const WAIT: Duration = Duration::from_secs(30);

struct Daemon {
    child: Child,
    port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A failed test leaves no daemon behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The daemon with a `PATH` that holds `git` and nothing else.
fn start(world: &World) -> Daemon {
    let bin = world.dir.path().join("git-only");
    std::fs::create_dir_all(&bin).unwrap();
    let found = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    let git = String::from_utf8(found.stdout).unwrap();
    std::os::unix::fs::symlink(git.trim(), bin.join("git")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tiller"))
        .args(["daemon", "--port", "0"])
        .env("TILLER_HOME", world.home())
        .env("PATH", &bin)
        .env("TILLER_HERDR", "/nonexistent/herdr")
        .env("TILLER_QUOTA_AXI", "/nonexistent/quota-axi")
        .env("TILLER_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(WAIT).expect("the daemon said nothing");
    let port = line
        .trim()
        .strip_prefix("listening on http://127.0.0.1:")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .parse()
        .unwrap();
    Daemon { child, port }
}

/// `GET /api/v1/events` read on a thread: each `data:` line of a `change`
/// message arrives on the channel.
fn changes(port: u16) -> mpsc::Receiver<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET /api/v1/events HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut change = false;
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            if line.is_empty() {
                change = false;
            } else if line == "event: change" {
                change = true;
            } else if change
                && let Some(data) = line.strip_prefix("data:")
                && tx.send(data.trim().to_owned()).is_err()
            {
                return;
            }
        }
    });
    rx
}

/// Waits for a change whose data holds `needle`; every change seen is
/// returned, so a failure shows what did arrive.
fn heard(
    rx: &mpsc::Receiver<String>,
    within: Duration,
    needle: &str,
) -> Result<String, Vec<String>> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(data) if data.contains(needle) => return Ok(data),
            Ok(data) => seen.push(data),
            Err(_) => break,
        }
    }
    Err(seen)
}

/// Every change that arrives within `within`.
fn drain(rx: &mpsc::Receiver<String>, within: Duration) -> Vec<String> {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(data) => seen.push(data),
            Err(_) => break,
        }
    }
    seen
}

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", "Someone")
        .env("GIT_AUTHOR_EMAIL", "someone@example.com")
        .env("GIT_COMMITTER_NAME", "Someone")
        .env("GIT_COMMITTER_EMAIL", "someone@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An editor saves `fleet.md` and someone commits it, all outside Tiller.
fn commit_outside(root: &Path, text: &str) {
    std::fs::write(root.join("fleet.md"), text).unwrap();
    git(root, &["add", "fleet.md"]);
    git(
        root,
        &["commit", "--quiet", "--no-gpg-sign", "-m", "edit by hand"],
    );
}

/// One `GET`, the whole answer with its head.
fn fetch(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    std::io::Read::read_to_string(&mut stream, &mut answer).unwrap();
    answer
}

/// The memory document as the board reads it.
fn fleet_text(port: u16) -> String {
    fetch(port, "/api/v1/memory?scope=fleet")
}

/// Waits until the `memory` poller has taken its first look, the baseline
/// a later edit is told apart from.
fn watching(port: u16) {
    let deadline = Instant::now() + WAIT;
    loop {
        let answer = fetch(port, "/api/v1/health");
        let body = answer.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        let health: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
        let looked = health["pollers"].as_array().is_some_and(|pollers| {
            pollers
                .iter()
                .any(|p| p["source"] == "memory" && p["last_success_at"].is_string())
        });
        if looked {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the memory poller never ran: {body}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A commit made in the memory repo by hand, and an editor's save that is
/// not committed, each reach an open stream as one change the board
/// refetches its memory reads on.
#[test]
fn an_edit_made_outside_the_board_reaches_the_stream() {
    let world = World::initialized();
    let root = world.home().join("memory");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    commit_outside(
        &root,
        "# Fleet\n\n## How we work here\n\n- The first line.\n",
    );
    let daemon = start(&world);
    let rx = changes(daemon.port);
    watching(daemon.port);
    // The first look is a baseline, not a change.
    let early = drain(&rx, Duration::from_secs(2));
    assert!(
        early.iter().all(|data| !data.contains("memory_repo")),
        "{early:?}"
    );

    commit_outside(
        &root,
        "# Fleet\n\n## How we work here\n\n- Edited by hand.\n",
    );
    let change = heard(&rx, WAIT, "\"entity\":\"memory_repo\"")
        .unwrap_or_else(|seen| panic!("no memory change arrived; saw {seen:?}"));
    assert!(
        change.contains("\"kind\":\"memory_repo.changed\""),
        "{change}"
    );
    assert!(fleet_text(daemon.port).contains("Edited by hand."));
    // One change for one edit: nothing more follows while the repo is still.
    let more = drain(&rx, Duration::from_secs(3));
    assert!(
        more.iter().all(|data| !data.contains("memory_repo")),
        "{more:?}"
    );

    std::fs::write(root.join("fleet.md"), "# Fleet\n\nNot committed.\n").unwrap();
    heard(&rx, WAIT, "\"entity\":\"memory_repo\"")
        .unwrap_or_else(|seen| panic!("no change for the editor's save; saw {seen:?}"));
}
