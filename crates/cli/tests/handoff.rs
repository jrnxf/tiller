//! `tiller handoff` end to end: the real binary and one `state.db`. The
//! resume read, the breakpoint help line, and the `PreCompact` hook's note.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;
use std::process::{Command, Stdio};

use common::World;
use tiller_core::domain::{OutboxKind, OutboxMessage};
use tiller_core::state::list;

fn card(world: &World, title: &str) -> String {
    let out = world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", title,
    ]);
    out.data()["display_id"].as_str().unwrap().to_owned()
}

#[test]
fn a_fresh_session_reads_the_handoff_the_old_one_wrote() {
    let world = World::initialized();
    let old = world.orchestrator();
    let login = card(&world, "the login page");

    // Nothing written yet: the read says so and names where to start.
    let empty = world.must_as(&old, &["handoff", "show"]);
    assert!(empty.data()["handoff"].is_null());
    assert_eq!(empty.data()["project"], "alpha");

    let written = world.must_as(
        &old,
        &[
            "handoff",
            "write",
            "--conclusions",
            "the login page is the only open card",
            "--next",
            "start a builder on it",
        ],
    );
    // The orchestrator ends a natural breakpoint with the word for the owner.
    let help = written.json["help"].to_string();
    assert!(help.contains("good time to /clear"), "{help}");
    assert!(help.contains("only the owner clears a session"), "{help}");

    world
        .store()
        .session_end(&world.owner_ctx(), &old, "clear")
        .unwrap();
    let fresh = world.orchestrator();
    assert_ne!(fresh, old);

    // The successor defaults to its own project; the owner must name one.
    let shown = world.must_as(&fresh, &["handoff", "show"]);
    assert_eq!(
        shown.data()["conclusions"],
        "the login page is the only open card"
    );
    assert_eq!(shown.data()["next_action"], "start a builder on it");
    assert_eq!(shown.data()["cards"][0]["card"], login);
    assert_eq!(shown.data()["cards"][0]["title"], "the login page");
    let owner = world.must(&["handoff", "show", "--project", "alpha"]);
    assert_eq!(owner.data()["next_action"], "start a builder on it");
    let no_project = world.run(&["handoff", "show"]);
    assert_eq!(no_project.code, 2, "{}", no_project.json);
}

#[test]
fn a_compaction_hook_leaves_one_note_asking_for_the_handoff() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let hook = |input: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tiller"))
            .args(["hook", "PreCompact"])
            .env("TILLER_HOME", world.home())
            .env("TILLER_SESSION_ID", &orchestrator)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    };
    let notes = || -> Vec<OutboxMessage> {
        list(
            world.store().conn(),
            "kind = 'note' ORDER BY created_at, id",
            &[],
        )
        .unwrap()
    };

    hook(r#"{"session_id":"claude-orchestrator","trigger":"auto"}"#);
    let sent = notes();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].kind, OutboxKind::Note);
    assert_eq!(sent[0].target_session_id, orchestrator);
    assert!(sent[0].body.contains("tiller handoff write"));
    assert!(sent[0].body.contains("Only the owner clears a session"));

    // The same call again is a replay: still one note.
    hook(r#"{"session_id":"claude-orchestrator","trigger":"auto"}"#);
    assert_eq!(notes().len(), 1);
}
