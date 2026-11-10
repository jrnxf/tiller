//! Behavior that can break silently and that a snapshot would not show:
//! the chat approval route, the Stop hook's switching rule, config diff
//! against a peer, and the wake-up line's target check.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::World;
use tiller_core::domain::{
    Activity, ApprovalKind, AskSource, Checks, Mergeable, ObservationSource, PipelineState,
    PrState, ReviewDecision,
};
use tiller_core::state::approvals::PrFetch;
use tiller_core::state::asks::NewAsk;
use tiller_core::state::runs::NewObservation;
use tiller_core::{Actor, Ctx, Timestamp};

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
/// The Ed25519 base point: a public key that parses.
const BASEPOINT: &str = "WGZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmY=";

fn hook() -> Ctx {
    Ctx::new(Actor::Hook)
}

/// A prompt the owner typed in `session`, as the UserPromptSubmit hook records it.
fn prompt(world: &World, session: &str, text: &str) -> String {
    world
        .store()
        .ask_record(
            &hook(),
            None,
            NewAsk {
                project_id: "alpha".into(),
                session_id: Some(session.into()),
                source: AskSource::Prompt,
                text: text.into(),
            },
        )
        .unwrap()
        .value
        .ask
        .id
}

fn requested_approval(world: &World, worker_session: &str) -> String {
    world.must_as(
        worker_session,
        &["pr", "link", "https://example.com/example/alpha/pull/7"],
    );
    let mut store = world.store();
    let pr: String = store
        .conn()
        .query_row("SELECT id FROM prs", [], |r| r.get(0))
        .unwrap();
    store
        .pr_refresh(
            &Ctx::new(Actor::Daemon),
            &pr,
            PrFetch {
                state: PrState::Open,
                head_sha: Some(HEAD.into()),
                base_ref: Some("main".into()),
                checks: Checks::Success,
                review_decision: ReviewDecision::Approved,
                mergeable: Mergeable::Clean,
                merged_at: None,
                merge_sha: None,
                pipeline_state: PipelineState::Passed,
                fetch_error: None,
            },
        )
        .unwrap();
    let out = world.must(&["approval", "request", "alpha-1", "--kind", "merge"]);
    assert_eq!(out.data()["kind"], ApprovalKind::Merge.as_str());
    out.data()["id"].as_str().unwrap().to_owned()
}

#[test]
fn chat_approval_needs_a_newer_prompt_from_the_calling_orchestrator() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (_, worker) = world.builder_run("alpha-1");
    let older = prompt(&world, &orchestrator, "merge it when ready");
    let approval = requested_approval(&world, &worker);

    // Said before the request: refused.
    let out = world.run_as(&orchestrator, &["approve", &approval, "--ask", &older]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);

    // Said after the request, but in a script note: not a prompt.
    let note = world
        .must(&["ask", "note", "alpha", "--text", "yes merge"])
        .data()["ask"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = world.run_as(&orchestrator, &["approve", &approval, "--ask", &note]);
    assert_eq!(out.code, 5, "{}", out.json);

    // A worker may not use the route at all.
    let newer = prompt(&world, &orchestrator, "yes, merge alpha-1");
    let out = world.run_as(&worker, &["approve", &approval, "--ask", &newer]);
    assert_eq!(
        (out.code, out.error_code()),
        (6, "forbidden"),
        "{}",
        out.json
    );

    // The owner's word in the orchestrator's own chat decides it, and the
    // receipt quotes the words.
    let out = world.must_as(&orchestrator, &["approve", &approval, "--ask", &newer]);
    assert_eq!(out.data()["approval"]["state"], "accepted");
    assert_eq!(out.data()["approval"]["decided_by"], "owner");
    assert_eq!(out.data()["quoted_ask"]["text"], "yes, merge alpha-1");
    // The decided event names the ask it cites.
    let cited: String = world
        .store()
        .conn()
        .query_row(
            "SELECT json_extract(payload, '$.ask_id') FROM events
             WHERE kind = 'approval.decided' ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cited, newer);

    // Without --ask, a session is not the owner. The backstop denies this
    // form before it runs (`agents::guards` test
    // `orchestrator_bare_approve_is_denied`); this is the second layer.
    let again = world.run_as(&orchestrator, &["approve", &approval]);
    assert_eq!((again.code, again.error_code()), (6, "forbidden"));
}

#[test]
fn a_stale_sha_is_refused_before_anything_is_decided() {
    let world = World::initialized();
    world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (_, worker) = world.builder_run("alpha-1");
    let approval = requested_approval(&world, &worker);
    let seq = world.store().last_seq().unwrap();
    let out = world.run(&["approve", &approval, "--sha", "deadbeef"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"));
    assert!(
        out.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("stale")
    );
    assert_eq!(world.store().last_seq().unwrap(), seq);
    let ok = world.must(&["approve", &approval, "--sha", &HEAD[..10]]);
    assert_eq!(ok.data()["approval"]["state"], "accepted");
}

/// The owner's accept names the head they reviewed. Without `--sha` the CLI
/// refuses with a usage error and writes nothing; 7 hex characters of the
/// bound head still match; a reject needs no head.
#[test]
fn an_accept_without_sha_is_a_usage_error() {
    let world = World::initialized();
    world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (_, worker) = world.builder_run("alpha-1");
    let approval = requested_approval(&world, &worker);
    let seq = world.store().last_seq().unwrap();

    let out = world.run(&["approve", &approval]);
    assert_eq!((out.code, out.error_code()), (2, "usage"), "{}", out.json);
    let message = out.json["error"]["message"].as_str().unwrap();
    assert!(message.contains("--sha"), "{message}");
    assert!(message.contains("head you reviewed"), "{message}");
    assert_eq!(world.store().last_seq().unwrap(), seq);

    // A prefix of 7 hex characters is enough; 6 is not.
    let short = world.run(&["approve", &approval, "--sha", &HEAD[..6]]);
    assert_eq!(short.code, 2, "{}", short.json);
    assert_eq!(world.store().last_seq().unwrap(), seq);
    let ok = world.must(&["approve", &approval, "--sha", &HEAD[..7]]);
    assert_eq!(ok.data()["approval"]["state"], "accepted");
}

#[test]
fn a_reject_needs_no_sha() {
    let world = World::initialized();
    world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (_, worker) = world.builder_run("alpha-1");
    let approval = requested_approval(&world, &worker);
    let rejected = world.must(&["approve", &approval, "--reject"]);
    assert_eq!(rejected.data()["approval"]["state"], "rejected");
}

/// Records a Stop observation at `at` for the orchestrator.
fn stop(world: &World, session: &str, at: Timestamp, payload: serde_json::Value) {
    world
        .store()
        .observation_record(
            &hook(),
            NewObservation {
                session_id: Some(session.into()),
                run_id: None,
                source: ObservationSource::Hook,
                event: "Stop".into(),
                verdict: Some(Activity::Idle),
                observed_at: at,
                payload,
            },
        )
        .unwrap();
}

#[test]
fn intake_stats_count_false_blocks_and_gate_block_once() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let ask = prompt(&world, &orchestrator, "add a thing");

    // The orchestrator stops with the ask unresolved, then resolves it within
    // a minute: that would-block stop was a false block.
    let stopped_at = Timestamp::now();
    stop(
        &world,
        &orchestrator,
        stopped_at,
        serde_json::json!({"would_block": true, "unresolved_ask_ids": [ask], "unknown_tags": []}),
    );
    world.must_as(&orchestrator, &["ask", "resolve", &ask, "chat"]);
    // A plain stop, and a would-block stop nothing settled.
    stop(
        &world,
        &orchestrator,
        Timestamp::now(),
        serde_json::json!({"would_block": false}),
    );
    let later = Timestamp::from_millis(stopped_at.as_millis() + 1).unwrap();
    stop(
        &world,
        &orchestrator,
        later,
        serde_json::json!({"would_block": true, "unresolved_ask_ids": [], "unknown_tags": ["alpha-99"]}),
    );

    let stats = world.must(&["intake", "stats"]);
    let data = stats.data();
    assert_eq!(data["stops"], 3);
    assert_eq!(data["would_block"], 2);
    assert_eq!(data["false_blocks"], 1);
    assert_eq!(data["rule_met"], false, "far fewer than 200 stops");

    // The switch is refused with the stats, and observe is always allowed.
    let refused = world.run(&["config", "set", "stop_hook_mode", "block-once"]);
    assert_eq!((refused.code, refused.error_code()), (5, "refused"));
    assert_eq!(refused.json["error"]["current"]["stops"], 3);
    world.must(&["config", "set", "stop_hook_mode", "observe"]);
    assert_eq!(
        world.must(&["config", "get", "stop_hook_mode"]).data()["value"],
        "observe"
    );
}

/// `authority.md` section 3 (G22): a call from an agent shell or with no
/// terminal is never the owner, whatever its environment leaves out. It is a
/// script: refused an owner-only command, it may still drop a note.
#[test]
fn an_agent_shell_or_a_pipe_is_never_the_owner() {
    let world = World::initialized();
    let seq = world.store().last_seq().unwrap();
    let refused = |why: &str| {
        let out = world.run(&["owner", "set", "tiller"]);
        assert_eq!(
            (out.code, out.error_code()),
            (6, "forbidden"),
            "{}",
            out.json
        );
        let message = out.json["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("needs the owner at a terminal") && message.contains(why),
            "{message}"
        );
    };
    world.set_terminal(false);
    refused("stdin is not a terminal");
    world.set_terminal(true);
    world.set_env("CLAUDECODE", "1");
    refused("CLAUDECODE is set");
    assert_eq!(
        world.store().last_seq().unwrap(),
        seq,
        "nothing was recorded"
    );

    let note = world.must(&["ask", "note", "alpha", "--text", "from cron"]);
    let actor: String = world
        .store()
        .conn()
        .query_row(
            "SELECT actor FROM events WHERE seq = ?1",
            [note.events()[0]],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actor, "script");
}

/// `protocol.md` 10.1: pairing needs the owner at a terminal on both hosts,
/// and `federation.accept = false` refuses it before anyone is asked. A
/// refused `peer add` reaches no peer and leaves no key file.
#[test]
fn pairing_needs_the_owner_at_a_terminal() {
    let world = World::initialized();
    let marker = world.dir.path().join("ssh-ran");
    let fake = world.dir.path().join("ssh");
    std::fs::write(&fake, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    world.set_env("TILLER_SSH", fake.to_str().unwrap());
    let seq = world.store().last_seq().unwrap();

    world.set_terminal(false);
    let out = world.run(&["peer", "add", "host-b", "--ssh", "host-b-alias"]);
    assert_eq!(
        (out.code, out.error_code()),
        (6, "forbidden"),
        "{}",
        out.json
    );
    assert!(
        out.json.to_string().contains("not a terminal"),
        "{}",
        out.json
    );
    world.set_terminal(true);
    for bad in ["../b", "-b", "a b"] {
        let out = world.run(&["peer", "add", bad, "--ssh", "host-b-alias"]);
        assert_eq!(out.code, 2, "{bad}: {}", out.json);
    }
    assert!(!marker.exists(), "a refused peer add runs no ssh");
    assert!(
        !world.home().join("peers").exists(),
        "and writes no key file"
    );

    let accept = |world: &World| {
        world.run(&[
            "peer",
            "accept",
            "--from",
            "board",
            "--viewer-host-id",
            "01JC0000000000000000B0ARD1",
            "--pubkey",
            BASEPOINT,
            "--scope",
            "act",
        ])
    };
    world.set_terminal(false);
    let out = accept(&world);
    world.set_terminal(true);
    assert_eq!(
        (out.code, out.error_code()),
        (6, "forbidden"),
        "{}",
        out.json
    );
    world.must(&["config", "set", "federation.accept", "false"]);
    let off_seq = world.store().last_seq().unwrap();
    let out = accept(&world);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);
    assert!(
        out.json.to_string().contains("federation.accept"),
        "{}",
        out.json
    );
    let out = world.run(&[
        "peer",
        "accept",
        "--from",
        "board",
        "--viewer-host-id",
        "01JC0000000000000000B0ARD1",
        "--pubkey",
        "AAAA",
        "--scope",
        "act",
    ]);
    assert_eq!(out.code, 2, "a key that does not parse: {}", out.json);
    assert_eq!(world.store().last_seq().unwrap(), off_seq);
    assert_eq!(off_seq, seq + 1, "only the config change was written");

    let list = world.must(&["peer", "list"]);
    assert_eq!(list.data()["federation_accept"], false);
    assert_eq!(list.data()["peers"], serde_json::json!([]));
    assert_eq!(list.data()["accepted"], serde_json::json!([]));
}

#[test]
fn config_diff_lists_what_differs_from_a_peer() {
    let world = World::initialized();
    world.must(&["config", "set", "quota_providers", "[\"claude\"]"]);
    world.must(&["config", "set", "stop_hook_mode", "observe"]);
    world.must(&["config", "set", "peers.host-b.ssh", "host-b-alias"]);

    // A fake ssh that answers as the peer's `tiller config get --json`.
    let fake = world.dir.path().join("ssh");
    std::fs::write(
        &fake,
        r#"#!/bin/sh
echo '{"ok":true,"data":{"config":{"stop_hook_mode":"off","quota_providers":["claude"],"extra":1}}}'
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tiller"))
        .env("TILLER_HOME", world.home())
        .env("TILLER_SSH", &fake)
        .args(["--json", "config", "diff", "host-b"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["data"]["same"], false);
    let keys: Vec<&str> = json["data"]["differences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["extra", "peers", "stop_hook_mode"]);
}

#[test]
fn a_wake_up_line_for_another_session_is_not_acked() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (run, worker) = world.builder_run("alpha-1");
    let sent = world.must(&["send", &run, "--kind", "steer", "--text", "go left"]);
    let message = sent.data()["id"].as_str().unwrap().to_owned();

    let wrong = world.run_as(&orchestrator, &["inbox", &message]);
    assert_ne!(wrong.code, 0, "{}", wrong.json);
    let state: String = world
        .store()
        .conn()
        .query_row("SELECT state FROM outbox WHERE id = ?1", [&message], |r| {
            r.get(0)
        })
        .unwrap();
    assert_ne!(state, "acked");

    let right = world.must_as(&worker, &["inbox", &message]);
    assert_eq!(right.data()["messages"][0]["body"], "go left");
    // Reading it again returns the body and writes nothing.
    let again = world.must_as(&worker, &["inbox", &message]);
    assert!(again.replayed());
}

#[test]
fn fleet_sessions_cannot_run_owner_commands() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    for args in [
        vec!["config", "set", "x", "1"],
        vec!["config", "get"],
        vec!["owner", "set", "firstmate"],
        vec!["host", "edit", "--rev", "1", "--lane-capacity", "9"],
        vec!["grant", "create", "alpha", "--kind", "merge"],
        vec![
            "away",
            "start",
            "--until",
            "2099-01-01",
            "--projects",
            "alpha",
        ],
        vec!["peer", "add", "b", "--ssh", "b"],
        vec![
            "peer",
            "accept",
            "--from",
            "b",
            "--viewer-host-id",
            "01JC0000000000000000B0ARD1",
            "--pubkey",
            BASEPOINT,
            "--scope",
            "act",
        ],
        vec!["peer", "list"],
        vec!["peer", "remove", "b"],
        vec!["peer", "revoke", "b"],
    ] {
        let out = world.run_as(&orchestrator, &args);
        assert_eq!(
            (out.code, out.error_code()),
            (6, "forbidden"),
            "{args:?}: {}",
            out.json
        );
    }
    assert!(world.run(&["owner", "show"]).data()["owner"].is_null());
}

#[test]
fn a_worker_whose_orchestrator_ended_is_still_listed() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (_, worker) = world.builder_run("alpha-1");
    world
        .store()
        .session_end(&Ctx::new(Actor::Daemon), &orchestrator, "crashed")
        .unwrap();
    let agents = world.must(&["agents"]);
    let groups = agents.data()["agents"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "{}", agents.json);
    assert!(groups[0]["orchestrator"].is_null());
    assert_eq!(groups[0]["workers"][0]["session_id"], worker);
}

#[test]
fn bad_inputs_are_refused_before_anything_is_written() {
    let world = World::initialized();
    world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (_, worker) = world.builder_run("alpha-1");
    let seq = world.store().last_seq().unwrap();

    // A too-short sha would match any head.
    let approval = requested_approval(&world, &worker);
    let seq_after_request = world.store().last_seq().unwrap();
    for sha in ["", "a", "xyzxyzxyz"] {
        let out = world.run(&["approve", &approval, "--sha", sha]);
        assert_eq!(out.code, 2, "--sha {sha:?}: {}", out.json);
    }
    // A pause date in the past must not leave the status line behind.
    let out = world.run_as(
        &worker,
        &["status", "paused", "waiting", "--until", "2001-01-01"],
    );
    assert_eq!(out.code, 5, "{}", out.json);
    // An ssh alias that starts with a dash would be read by ssh as an option.
    let out = world.run(&["peer", "add", "evil", "--ssh", "-oProxyCommand=true"]);
    assert_eq!(out.code, 2, "{}", out.json);
    assert!(seq <= seq_after_request);
    assert_eq!(world.store().last_seq().unwrap(), seq_after_request);
}

#[test]
fn a_rewritten_body_file_is_a_different_command_for_the_derived_key() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let file = world.dir.path().join("body.md");
    let create = |body: &str| {
        std::fs::write(&file, body).unwrap();
        world.must_as(
            &orchestrator,
            &[
                "card",
                "create",
                "alpha",
                "--kind",
                "ship",
                "--title",
                "Same title",
                "--body-file",
                file.to_str().unwrap(),
            ],
        )
    };
    let first = create("first body");
    let second = create("second body");
    assert!(!second.replayed());
    assert_ne!(first.data()["id"], second.data()["id"]);
    let retry = create("second body");
    assert!(retry.replayed());
}
