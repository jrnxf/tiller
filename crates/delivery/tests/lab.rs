//! The adapter against a real herdr server: layout repair, row tokens and a
//! delivery into a real Claude Code pane. Skipped unless
//! `TILLER_HERDR_LAB` is `<wrapper>|<session>|<socket>`, where `<wrapper>`
//! runs `herdr` in an isolated session (`<wrapper> run <session> <args>`);
//! `TILLER_HERDR_LAB_CLAUDE` names the Claude Code command to start in the
//! worker's pane.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use tiller_core::domain::{OutboxKind, OutboxMessage, OutboxState};
use tiller_core::state::get;
use tiller_core::state::outbox::NewMessage;
use tiller_core::{Actor, Ctx, Timestamp};
use tiller_delivery::outbox::Deliverer;
use tiller_herdr::client::{Cli, Herdr};
use tiller_herdr::composer::{Composer, classify};
use tiller_herdr::poller::{HerdrSource, Registry};
use tiller_herdr::testing::Fleet;

fn lab() -> Option<Cli> {
    let spec = std::env::var("TILLER_HERDR_LAB").ok()?;
    let parts: Vec<&str> = spec.split('|').collect();
    Some(
        Cli::local()
            .program(vec![parts[0].into(), "run".into(), parts[1].into()])
            .socket(parts[2]),
    )
}

/// A key the owner presses, sent through the lab wrapper.
fn owner_key(pane_id: &str, key: &str) {
    let spec = std::env::var("TILLER_HERDR_LAB").unwrap();
    let parts: Vec<&str> = spec.split('|').collect();
    let status = std::process::Command::new(parts[0])
        .args(["run", parts[1], "pane", "send-keys", pane_id, key])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[ignore = "needs an isolated herdr lab session"]
fn real_herdr_layout_tokens_and_delivery() {
    let Some(herdr) = lab() else {
        return;
    };
    let orchestrator = herdr.create_workspace("scratch", None).unwrap();
    let notes = herdr.create_workspace("notes", None).unwrap();
    let project = tempfile::tempdir().unwrap();
    let worker = herdr
        .create_workspace("claude", Some(project.path()))
        .unwrap();

    let mut fleet = Fleet::with_orchestrator_pane(&orchestrator.pane_id);
    let session = fleet.worker(&worker.pane_id);
    let poll = |fleet: &Fleet| {
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        poller.poll_once(Timestamp::now());
        assert_eq!(
            poller.health().consecutive_failures,
            0,
            "{:?}",
            poller.health()
        );
    };
    poll(&fleet);

    let snapshot = herdr.snapshot().unwrap();
    let position = |id: &str| {
        snapshot
            .workspaces
            .iter()
            .position(|w| w.workspace_id == id)
            .unwrap()
    };
    let label = |id: &str| snapshot.workspaces[position(id)].label.clone();
    assert_eq!(label(&orchestrator.workspace_id), "alpha");
    assert_eq!(label(&worker.workspace_id), "└ alpha-1 build");
    assert_eq!(label(&notes.workspace_id), "notes");
    assert_eq!(
        position(&worker.workspace_id),
        position(&orchestrator.workspace_id) + 1,
        "the worker's row moved under its orchestrator"
    );
    let tokens = &snapshot.pane(&worker.pane_id).unwrap().tokens;
    assert_eq!(tokens["session"], session);
    assert_eq!(tokens["card"], "alpha-1");

    // A shell is never an empty composer: nothing is typed.
    let mut deliverer = Deliverer::new(fleet.open(), herdr.clone(), &fleet.host);
    let message = fleet
        .store
        .outbox_enqueue(
            &Ctx::new(Actor::Daemon),
            None,
            NewMessage {
                target_session_id: session.clone(),
                kind: OutboxKind::Steer,
                body: "lab steer".into(),
                card_id: None,
                reply_to: None,
            },
        )
        .unwrap()
        .value;
    let report = deliverer.pass(Timestamp::now()).unwrap();
    assert_eq!(report.deferred.len(), 1, "{report:?}");
    assert!(
        !herdr
            .read_visible(&worker.pane_id)
            .unwrap()
            .contains("tiller inbox")
    );

    let Ok(claude) = std::env::var("TILLER_HERDR_LAB_CLAUDE") else {
        return;
    };
    herdr.send_text(&worker.pane_id, &claude).unwrap();
    herdr.send_enter(&worker.pane_id).unwrap();
    // The lab acts as the owner: it accepts the folder trust prompt once.
    let mut trusted = false;
    wait_for("an empty Claude composer", Duration::from_secs(90), || {
        let screen = herdr.read_visible(&worker.pane_id).unwrap();
        if let Ok(dump) = std::env::var("TILLER_HERDR_LAB_DUMP") {
            std::fs::write(dump, &screen).unwrap();
        }
        match classify(&screen) {
            Composer::Empty => true,
            // The folder trust prompt selects "No, exit" first.
            Composer::Dialog if !trusted && screen.contains("Yes, I trust this folder") => {
                trusted = true;
                if screen.lines().any(|l| l.contains("❯ No")) {
                    owner_key(&worker.pane_id, "down");
                }
                herdr.send_enter(&worker.pane_id).unwrap();
                false
            }
            _ => false,
        }
    });
    poll(&fleet);

    let later = Timestamp::now().plus_minutes(1);
    let report = deliverer.pass(later).unwrap();
    assert_eq!(report.rung, std::slice::from_ref(&message.id), "{report:?}");
    let delivered: OutboxMessage = get(fleet.store.conn(), "outbox message", &message.id).unwrap();
    assert_eq!(delivered.state, OutboxState::Delivered);
    let line = format!("tiller inbox {} # steer", message.id);
    wait_for("the submitted line", Duration::from_secs(30), || {
        let screen = herdr.read_visible(&worker.pane_id).unwrap();
        if let Ok(dump) = std::env::var("TILLER_HERDR_LAB_DUMP") {
            std::fs::write(format!("{dump}.after"), &screen).unwrap();
        }
        screen.contains(&line) && !matches!(classify(&screen), Composer::Holds(_))
    });
}
