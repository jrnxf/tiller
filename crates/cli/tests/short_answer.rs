//! A person at a terminal reads a short answer: the `summary` sentence, a
//! `warning` and the `help` lines. `--json`, `--full`, a pipe and an agent
//! session keep the whole record (`protocol.md` 6.1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::World;

/// Runs `command` with its stdout on a pseudo-terminal and returns what it
/// printed there.
fn at_terminal(mut command: Command) -> String {
    use rustix::pty::OpenptFlags;
    let controller = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    rustix::pty::grantpt(&controller).unwrap();
    rustix::pty::unlockpt(&controller).unwrap();
    let path = rustix::pty::ptsname(&controller, Vec::new()).unwrap();
    let flags = rustix::fs::OFlags::RDWR | rustix::fs::OFlags::NOCTTY;
    let terminal = rustix::fs::open(path.as_c_str(), flags, rustix::fs::Mode::empty()).unwrap();
    // A reader runs before the command does, as on a real terminal, and the
    // slave stays open here until the command has printed.
    let printed = Arc::new(Mutex::new(Vec::new()));
    let mut reader = std::fs::File::from(controller);
    let sink = Arc::clone(&printed);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(len) = reader.read(&mut buf) {
            if len == 0 {
                break;
            }
            sink.lock().unwrap().extend_from_slice(&buf[..len]);
        }
    });
    let mut child = command
        .stdin(Stdio::null())
        .stdout(terminal.try_clone().unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if printed.lock().unwrap().ends_with(b"\n") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let printed = printed.lock().unwrap().clone();
    String::from_utf8_lossy(&printed).replace("\r\n", "\n")
}

fn owner(world: &World, args: &[&str]) -> Command {
    let mut command = world.bare_command();
    command.env("TILLER_TEST_OWNER", "1").args(args);
    command
}

fn piped(mut command: Command) -> String {
    let out = command.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// A world with a held card: `card unhold` has no sentence of its own.
fn held() -> (World, String) {
    let world = World::initialized();
    let filed = world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "scout",
        "--title",
        "Look at it",
    ]);
    let card = filed.json["data"]["display_id"]
        .as_str()
        .unwrap()
        .to_owned();
    world.must(&[
        "card",
        "hold",
        &card,
        "--until",
        "2026-12-01",
        "--reason",
        "wait",
    ]);
    (world, card)
}

#[test]
fn a_person_at_a_terminal_reads_only_the_sentence_the_warning_and_the_next_step() {
    let world = World::initialized();
    let shown = at_terminal(owner(
        &world,
        &["ask", "note", "alpha", "--text", "Is the export nightly?"],
    ));
    assert!(
        shown.starts_with("Recorded for Alpha: 'Is the export nightly?'."),
        "{shown}"
    );
    assert!(shown.contains("tiller orchestrator start alpha"), "{shown}");
    assert!(shown.contains("--full"), "{shown}");
    for record in ["ok:", "data:", "events", "replayed", "created_at"] {
        assert!(!shown.contains(record), "{record} in\n{shown}");
    }
}

#[test]
fn a_mutation_with_no_sentence_of_its_own_gets_one() {
    let (world, card) = held();
    let shown = at_terminal(owner(&world, &["card", "unhold", &card]));
    assert!(shown.starts_with("Done: `tiller card unhold`."), "{shown}");
    assert!(!shown.contains("data:"), "{shown}");
}

#[test]
fn full_and_json_and_a_pipe_keep_the_whole_record() {
    let world = World::initialized();
    let args = ["ask", "note", "alpha", "--text", "Is the export nightly?"];

    let full = at_terminal(owner(&world, &[&["--full"], &args[..]].concat()));
    assert!(full.starts_with("ok: true"), "{full}");
    assert!(
        full.contains("data:") && full.contains("summary:"),
        "{full}"
    );

    let json = at_terminal(owner(&world, &[&["--json"], &args[..]].concat()));
    let json: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(json["ok"], true, "{json}");
    assert!(json["data"]["ask"].is_object(), "{json}");

    let pipe = piped(owner(&world, &args));
    assert!(pipe.starts_with("ok: true"), "{pipe}");
    assert!(pipe.contains("data:"), "{pipe}");
}

#[test]
fn a_read_with_no_sentence_prints_its_record_at_a_terminal() {
    let world = World::initialized();
    let shown = at_terminal(owner(&world, &["project", "list"]));
    assert!(shown.starts_with("ok: true"), "{shown}");
    assert!(shown.contains("alpha"), "{shown}");
}

#[test]
fn an_agent_session_at_a_terminal_still_gets_the_toon_record() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let mut command = world.bare_command();
    command.env("TILLER_SESSION_ID", &orchestrator).args([
        "card",
        "create",
        "alpha",
        "--kind",
        "scout",
        "--title",
        "Look at the import",
    ]);
    let shown = at_terminal(command);
    assert!(shown.starts_with("ok: true"), "{shown}");
    assert!(shown.contains("data:"), "{shown}");
    assert!(!shown.contains("summary:"), "{shown}");
    assert!(!shown.contains("--full"), "{shown}");
}
