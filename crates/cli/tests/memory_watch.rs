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
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{World, git_only_daemon};

const WAIT: Duration = Duration::from_secs(30);

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
    let (_daemon, port) = git_only_daemon(&world);
    let rx = changes(port);
    watching(port);
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
    assert!(fleet_text(port).contains("Edited by hand."));
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
