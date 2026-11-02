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

#[test]
fn config_diff_lists_what_differs_from_a_peer() {
    let world = World::initialized();
    world.must(&["config", "set", "quota_providers", "[\"claude\"]"]);
    world.must(&["config", "set", "stop_hook_mode", "observe"]);
    world.must(&["peer", "add", "host-b", "--ssh", "host-b-alias"]);

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
