//! T2.2 acceptance: every command has an insta snapshot of its JSON output
//! and its exit code, and the exit codes of `protocol.md` section 6.2 are
//! each reached. The scenario runs the real binary against one home.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{World, snap};
use tiller_core::domain::{Checks, Mergeable, PipelineState, PrState, ReviewDecision};
use tiller_core::state::approvals::PrFetch;
use tiller_core::state::caches::QuotaReading;

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// Runs, requires success, snapshots under `name` and returns the response.
fn step(world: &World, name: &str, args: &[&str]) -> common::Out {
    let out = world.run(args);
    assert_eq!(out.code, 0, "{name}: {}", out.json);
    snap(world, name, &out);
    out
}

/// Runs a command that must fail with `code`, and snapshots the error.
fn fails(world: &World, code: i32, name: &str, args: &[&str]) -> common::Out {
    let out = world.run(args);
    assert_eq!(out.code, code, "{name}: {}", out.json);
    assert!(!out.ok());
    snap(world, name, &out);
    out
}

fn step_as(world: &World, session: &str, name: &str, args: &[&str]) -> common::Out {
    let out = world.run_as(session, args);
    assert_eq!(out.code, 0, "{name}: {}", out.json);
    snap(world, name, &out);
    out
}

fn id(out: &common::Out, path: &str) -> String {
    out.json
        .pointer(path)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no {path} in {}", out.json))
        .to_owned()
}

#[test]
fn every_command_has_a_snapshot_and_an_exit_code() {
    let world = World::new();

    // Setup and projects.
    let out = step(
        &world,
        "init",
        &["init", "--host-label", "host-a", "--lane-capacity", "2"],
    );
    assert_eq!(out.code, 0);
    step(&world, "version", &["version"]);
    step(
        &world,
        "project_add",
        &[
            "project",
            "add",
            "alpha",
            "--name",
            "Alpha",
            "--repo",
            "example/alpha",
            "--delivery",
            "direct-pr",
            "--capacity",
            "3",
        ],
    );
    step(&world, "project_list", &["project", "list"]);
    step(
        &world,
        "project_edit",
        &[
            "project",
            "edit",
            "alpha",
            "--rev",
            "1",
            "--name",
            "Alpha Project",
        ],
    );
    step(
        &world,
        "host_edit",
        &["host", "edit", "--rev", "1", "--lane-capacity", "3"],
    );

    // Asks and intake, with a live orchestrator to wake.
    let orchestrator = world.orchestrator();
    let note = step(
        &world,
        "ask_note",
        &[
            "ask",
            "note",
            "alpha",
            "--text",
            "add a dark mode",
            "--key",
            "note-1",
        ],
    );
    let ask1 = id(&note, "/data/ask/id");
    step(
        &world,
        "ask_list_unresolved",
        &["ask", "list", "--unresolved"],
    );
    step(&world, "ask_show", &["ask", "show", &ask1]);
    let resolved = step(
        &world,
        "ask_resolve_new",
        &[
            "ask",
            "resolve",
            &ask1,
            "new",
            "--kind",
            "ship",
            "--title",
            "Dark mode",
            "--priority",
            "1",
            "--body",
            "Add a dark theme",
        ],
    );
    assert_eq!(id(&resolved, "/data/tag"), "[alpha-1]");
    step(
        &world,
        "ask_resolve_replay",
        &[
            "ask",
            "resolve",
            &ask1,
            "new",
            "--kind",
            "ship",
            "--title",
            "Dark mode",
            "--priority",
            "1",
            "--body",
            "Add a dark theme",
        ],
    );
    let ask2 = id(
        &world.must(&["ask", "note", "alpha", "--text", "just thinking aloud"]),
        "/data/ask/id",
    );
    step(
        &world,
        "ask_resolve_chat",
        &["ask", "resolve", &ask2, "chat", "--reason", "talk only"],
    );
    let ask3 = id(
        &world.must(&["ask", "note", "alpha", "--text", "never mind"]),
        "/data/ask/id",
    );
    step(
        &world,
        "ask_resolve_cancelled",
        &[
            "ask",
            "resolve",
            &ask3,
            "cancelled",
            "--reason",
            "withdrawn",
        ],
    );
    let ask4 = id(
        &world.must(&["ask", "note", "alpha", "--text", "also fix the footer"]),
        "/data/ask/id",
    );
    step(
        &world,
        "ask_resolve_followup",
        &["ask", "resolve", &ask4, "followup:alpha-1"],
    );
    step(
        &world,
        "ask_link_new",
        &[
            "ask",
            "link",
            &ask4,
            "new",
            "--kind",
            "scout",
            "--title",
            "Footer survey",
            "--key",
            "link-1",
        ],
    );
    step(&world, "ask_list", &["ask", "list", "--limit", "10"]);
    step(&world, "intake_stats", &["intake", "stats", "--days", "7"]);

    // Cards.
    step(
        &world,
        "card_create",
        &[
            "card",
            "create",
            "alpha",
            "--kind",
            "ops",
            "--title",
            "Rotate the deploy token",
            "--key",
            "card-1",
        ],
    );
    step(
        &world,
        "card_edit",
        &[
            "card",
            "edit",
            "alpha-1",
            "--rev",
            "1",
            "--title",
            "Dark mode for the board",
            "--size",
            "M",
        ],
    );
    fails(
        &world,
        4,
        "card_edit_stale_rev",
        &[
            "card",
            "edit",
            "alpha-1",
            "--rev",
            "1",
            "--title",
            "Something else",
        ],
    );
    step(
        &world,
        "card_edit_dispatch",
        &[
            "card",
            "edit",
            "alpha-1",
            "--rev",
            "2",
            "--dispatch",
            "opus:high",
        ],
    );
    step(
        &world,
        "card_hold",
        &[
            "card",
            "hold",
            "alpha-3",
            "--until",
            "2099-01-05",
            "--reason",
            "waiting for the vendor",
        ],
    );
    step(&world, "card_unhold", &["card", "unhold", "alpha-3"]);
    step(
        &world,
        "card_dep_add",
        &["card", "dep", "add", "alpha-3", "alpha-1"],
    );
    step(
        &world,
        "card_dep_rm",
        &["card", "dep", "rm", "alpha-3", "alpha-1"],
    );
    step(
        &world,
        "card_comment",
        &[
            "card",
            "comment",
            "alpha-1",
            "--text",
            "please keep it small",
            "--key",
            "comment-1",
        ],
    );
    step(
        &world,
        "card_complete",
        &[
            "card",
            "complete",
            "alpha-3",
            "--evidence",
            "token rotated, deploy green",
        ],
    );
    step(
        &world,
        "card_reopen",
        &[
            "card",
            "reopen",
            "alpha-3",
            "--reason",
            "the token expired again",
        ],
    );
    fails(
        &world,
        5,
        "card_close_without_report",
        &["card", "close", "alpha-2", "--reported"],
    );
    world.must(&[
        "artifact",
        "add",
        "alpha-2",
        "--kind",
        "report",
        "--title",
        "Footer findings",
        "--path",
        "reports/footer.md",
    ]);
    step(
        &world,
        "card_close",
        &["card", "close", "alpha-2", "--reported"],
    );
    step(
        &world,
        "card_cancel",
        &["card", "cancel", "alpha-3", "--reason", "no longer needed"],
    );
    step(&world, "card_list", &["card", "list", "--project", "alpha"]);

    // Attention.
    let item = step(
        &world,
        "attention_open",
        &[
            "attention",
            "open",
            "alpha-1",
            "--kind",
            "decision",
            "--question",
            "Follow the system theme?",
            "--recommendation",
            "yes",
            "--options",
            "[\"yes\",\"no\"]",
            "--key",
            "att-1",
        ],
    );
    let item_id = id(&item, "/data/id");
    step(
        &world,
        "attention_list_open",
        &["attention", "list", "--open"],
    );
    step(
        &world,
        "attention_answer",
        &["attention", "answer", &item_id, "--text", "yes, follow it"],
    );
    let second = world.must(&[
        "attention",
        "open",
        "alpha-1",
        "--kind",
        "report",
        "--question",
        "Anything else?",
    ]);
    step(
        &world,
        "attention_withdraw",
        &[
            "attention",
            "withdraw",
            &id(&second, "/data/id"),
            "--reason",
            "answered elsewhere",
        ],
    );
    step(&world, "attention_list", &["attention", "list"]);

    // A builder run and its worker.
    let (run, worker) = world.builder_run("alpha-1");
    step_as(
        &world,
        &worker,
        "status_working",
        &["status", "working", "reading the board code"],
    );
    step_as(
        &world,
        &worker,
        "status_paused",
        &[
            "status",
            "paused",
            "waiting for CI",
            "--until",
            "2099-01-02",
        ],
    );
    step_as(
        &world,
        &worker,
        "pr_link",
        &["pr", "link", "https://example.com/example/alpha/pull/7"],
    );
    step(
        &world,
        "run_show",
        &["run", "show", "alpha-1 build", "--tail", "20"],
    );
    step(&world, "run_list", &["run", "list", "--active"]);
    step(
        &world,
        "run_pause",
        &[
            "run",
            "pause",
            "alpha-1-build",
            "--until",
            "2099-01-03",
            "--reason",
            "owner is away",
        ],
    );
    step(
        &world,
        "send_steer",
        &[
            "send",
            &run,
            "--kind",
            "steer",
            "--text",
            "use the existing theme tokens",
            "--key",
            "send-1",
        ],
    );
    step_as(&world, &worker, "inbox_drain", &["inbox"]);
    step(
        &world,
        "artifact_add",
        &[
            "artifact",
            "add",
            "alpha-1",
            "--kind",
            "page",
            "--title",
            "Review page",
            "--url",
            "https://example.com/review",
            "--key",
            "art-1",
        ],
    );
    step(
        &world,
        "review_record",
        &[
            "review",
            "record",
            &run,
            "--reviewer",
            "code-review",
            "--coverage",
            "diff",
            "--verdict",
            "pass",
            "--low",
            "2",
            "--key",
            "rev-1",
        ],
    );
    step(
        &world,
        "obligation_open",
        &[
            "obligation",
            "open",
            "alpha-1",
            "--what",
            "tell the owner when CI is green",
            "--on",
            "pr.checks",
            "--key",
            "obl-1",
        ],
    );
    let obligation = world
        .store()
        .conn()
        .query_row("SELECT id FROM obligations LIMIT 1", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    step(
        &world,
        "obligation_cancel",
        &[
            "obligation",
            "cancel",
            &obligation,
            "--reason",
            "not needed",
        ],
    );
    step_as(
        &world,
        &orchestrator,
        "handoff_write",
        &[
            "handoff",
            "write",
            "--conclusions",
            "dark mode is under way",
            "--next",
            "review the PR",
            "--key",
            "hand-1",
        ],
    );
    step_as(&world, &orchestrator, "handoff_show", &["handoff", "show"]);

    // Landing: a PR the forge poller has read, then the approval.
    let store_ctx = world.owner_ctx();
    {
        let mut store = world.store();
        let pr: String = store
            .conn()
            .query_row("SELECT id FROM prs", [], |r| r.get(0))
            .unwrap();
        store
            .pr_refresh(
                &tiller_core::Ctx::new(tiller_core::Actor::Daemon),
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
    }
    let approval = step(
        &world,
        "approval_request",
        &["approval", "request", "alpha-1", "--kind", "merge"],
    );
    let approval_id = id(&approval, "/data/id");
    fails(
        &world,
        5,
        "approve_stale_sha",
        &["approve", &approval_id, "--sha", "ffffffff"],
    );
    step(
        &world,
        "approve",
        &[
            "approve",
            &approval_id,
            "--sha",
            &HEAD[..12],
            "--key",
            "ok-1",
        ],
    );
    fails(&world, 7, "land_needs_daemon", &["land", &approval_id]);
    let grant = step(
        &world,
        "grant_create",
        &[
            "grant",
            "create",
            "alpha",
            "--kind",
            "merge",
            "--conditions",
            "{\"v\":1,\"actions\":[\"squash\"]}",
            "--expires",
            "2099-02-01",
            "--key",
            "grant-1",
        ],
    );
    step(
        &world,
        "grant_revoke",
        &[
            "grant",
            "revoke",
            &id(&grant, "/data/id"),
            "--reason",
            "changed my mind",
        ],
    );
    step(
        &world,
        "away_start",
        &[
            "away",
            "start",
            "--until",
            "2099-03-01",
            "--projects",
            "alpha",
            "--key",
            "away-1",
        ],
    );
    step(&world, "away_end", &["away", "end", "--reason", "back"]);

    // Ending the run, cleaning it.
    fails(&world, 5, "done_refused", &["done", &run]);
    step(&world, "done", &["done", &run, "--outcome", "abandoned"]);
    fails(&world, 5, "run_clean_refused", &["run", "clean", &run]);

    // Records the caches hold: quota, then the reads.
    {
        let mut store = world.store();
        store
            .cache_quota(
                &store_ctx,
                vec![QuotaReading {
                    label: "claude:seven_day".into(),
                    percent_remaining: Some(62.0),
                    resets_at: Some("2099-01-04T09:00:00.000Z".parse().unwrap()),
                    pace: Some("behind".into()),
                    reserve: Some(5.0),
                    burn_multiple: Some(0.8),
                    elapsed_percent: Some(40.0),
                    projected_exhaustion_at: None,
                    title: Some("Weekly".into()),
                    kind: Some("weekly".into()),
                    fetch_error: None,
                }],
            )
            .unwrap();
    }
    step(&world, "quota", &["quota"]);
    step(&world, "board", &["board", "--filter", "all"]);
    step(
        &world,
        "board_needs",
        &["board", "--project", "alpha", "--filter", "needs"],
    );
    step(&world, "agents", &["agents"]);
    step(&world, "machines", &["machines"]);
    step(&world, "machines_local", &["machines", "--local"]);
    step(&world, "snapshot", &["snapshot"]);
    step(
        &world,
        "events",
        &["events", "--since", "40", "--limit", "5"],
    );
    step(
        &world,
        "events_card",
        &["events", "--card", "alpha-1", "--limit", "3"],
    );
    step(&world, "card_show", &["card", "show", "alpha-1"]);
    step(
        &world,
        "card_timeline",
        &["card", "timeline", "alpha-1", "--limit", "4"],
    );

    // Config, peers and the owner marker.
    step(
        &world,
        "config_set",
        &["config", "set", "stop_hook_mode", "observe"],
    );
    step(
        &world,
        "config_set_nested",
        &["config", "set", "dispatch.default.model", "sonnet"],
    );
    step(
        &world,
        "config_set_same",
        &["config", "set", "stop_hook_mode", "observe"],
    );
    fails(
        &world,
        5,
        "config_set_block_once_refused",
        &["config", "set", "stop_hook_mode", "block-once"],
    );
    step(&world, "config_get", &["config", "get"]);
    step(
        &world,
        "config_get_key",
        &["config", "get", "dispatch.default.model"],
    );
    step(&world, "config_hash", &["config", "hash"]);
    // Pairing needs the owner at a terminal on both hosts; the harness has
    // none, so the happy path is the core's tests and a live pairing.
    fails(
        &world,
        6,
        "peer_add_needs_a_terminal",
        &["peer", "add", "host-b", "--ssh", "host-b-ssh"],
    );
    let accept = [
        "peer",
        "accept",
        "--from",
        "host-b",
        "--viewer-host-id",
        "01JC0000000000000000B0ARD1",
        "--pubkey",
        "WGZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmZmY=",
        "--scope",
        "act",
    ];
    fails(&world, 6, "peer_accept_needs_a_terminal", &accept);
    fails(
        &world,
        2,
        "config_set_federation_accept_not_bool",
        &["config", "set", "federation.accept", "maybe"],
    );
    step(
        &world,
        "config_set_federation_accept_off",
        &["config", "set", "federation.accept", "false"],
    );
    fails(&world, 5, "peer_accept_refused_accept_off", &accept);
    step(
        &world,
        "config_set_federation_accept_on",
        &["config", "set", "federation.accept", "true"],
    );
    step(&world, "peer_list", &["peer", "list"]);
    fails(
        &world,
        3,
        "peer_revoke_unknown",
        &["peer", "revoke", "host-b"],
    );
    fails(
        &world,
        3,
        "peer_remove_unknown",
        &["peer", "remove", "host-b"],
    );
    step(&world, "owner_show", &["owner", "show"]);
    step(
        &world,
        "owner_set_firstmate",
        &["owner", "set", "firstmate"],
    );
    fails(
        &world,
        6,
        "owner_marker_refuses_mutation",
        &[
            "card", "create", "alpha", "--kind", "ship", "--title", "blocked",
        ],
    );
    step(&world, "owner_marker_serves_reads", &["card", "list"]);
    step(&world, "owner_set_tiller", &["owner", "set", "tiller"]);

    // The pre-launch stage (`docs/design/pre-launch-validation.md`).
    let rev = || {
        let out = world.run(&["project", "list"]);
        out.data()["projects"][0]["rev"]
            .as_i64()
            .unwrap()
            .to_string()
    };
    step(
        &world,
        "project_stage_prelaunch",
        &[
            "project",
            "stage",
            "alpha",
            "pre-launch",
            "--rev",
            &rev(),
            "--from",
            HEAD,
        ],
    );
    step(&world, "review_due", &["review", "due"]);
    step(
        &world,
        "project_stage_launched",
        &["project", "stage", "alpha", "launched", "--rev", &rev()],
    );

    // A hard delete: refused without --yes, and the id is free afterwards.
    let gamma = [
        "project",
        "add",
        "gamma",
        "--name",
        "Gamma",
        "--delivery",
        "local-only",
    ];
    world.run(&gamma);
    world.run(&[
        "card",
        "create",
        "gamma",
        "--kind",
        "ship",
        "--title",
        "short-lived",
    ]);
    world.run(&["config", "set", "checkouts.gamma", "/home/user/gamma"]);
    fails(
        &world,
        5,
        "project_delete_unconfirmed",
        &["project", "delete", "gamma"],
    );
    step(
        &world,
        "project_delete",
        &["project", "delete", "gamma", "--yes"],
    );
    let again = world.run(&gamma);
    assert_eq!(again.code, 0, "{}", again.json);
    fails(
        &world,
        5,
        "project_delete_fleet",
        &["project", "delete", "fleet", "--yes"],
    );
}

/// Each exit code of `protocol.md` section 6.2 is reached by a real command.
#[test]
fn exit_codes_follow_the_table() {
    let world = World::initialized();
    let card = world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let rev = card.data()["rev"].to_string();

    assert_eq!(world.run(&["card", "show", "alpha-1"]).code, 0);
    assert_eq!(
        world.run(&["card", "list", "--state", "bogus"]).code,
        2,
        "usage"
    );
    assert_eq!(
        world.run(&["card", "show", "alpha-99"]).code,
        3,
        "not found"
    );
    world.must(&["card", "edit", "alpha-1", "--rev", &rev, "--title", "Two"]);
    let stale = world.run(&["card", "edit", "alpha-1", "--rev", &rev, "--title", "Three"]);
    assert_eq!(stale.code, 4, "conflict");
    assert_eq!(stale.error_code(), "conflict");
    assert_eq!(stale.json["error"]["current"]["field"]["value"], "Two");
    assert_eq!(
        world
            .run(&["card", "close", "alpha-1", "--outcome", "landed"])
            .code,
        5,
        "refused"
    );

    let twice = world.run(&["init", "--host-label", "x", "--lane-capacity", "1"]);
    assert_eq!(twice.code, 5, "a host is initialized once");
    let orchestrator = world.orchestrator();
    let forbidden = world.run_as(&orchestrator, &["project", "retire", "alpha"]);
    assert_eq!((forbidden.code, forbidden.error_code()), (6, "forbidden"));
    let gone = world.run_as("01ARZ3NDEKTSV4RRFFQ69G5FAV", &["card", "list"]);
    assert_eq!(gone.code, 0, "reads need no session");
    let ghost = world.run_as(
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        &["card", "comment", "alpha-1", "--text", "x"],
    );
    assert_eq!(
        ghost.code, 6,
        "a session that resolves to no row is refused"
    );

    // 2: a spawn for a project with no checkout configured writes nothing.
    let seq = world.store().last_seq().unwrap();
    let out = world.run(&["spawn", "alpha-1", "builder"]);
    assert_eq!((out.code, out.error_code()), (2, "usage"));
    assert!(
        out.json["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("checkouts.alpha")),
        "{}",
        out.json
    );
    assert_eq!(world.store().last_seq().unwrap(), seq);

    // A second orchestrator start returns the live one and writes nothing.
    let out = world.run(&["orchestrator", "start", "alpha"]);
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(out.json["data"]["session"]["id"], orchestrator.as_str());
    assert_eq!(world.store().last_seq().unwrap(), seq);

    // Focus is the owner's, needs a known agent and a pane to point at.
    let out = world.run_as(&orchestrator, &["agent", "focus", &orchestrator]);
    assert_eq!((out.code, out.error_code()), (6, "forbidden"));
    let out = world.run(&["agent", "focus", "alpha-1-build"]);
    assert_eq!((out.code, out.error_code()), (3, "not_found"));
    let out = world.run(&["agent", "focus", &orchestrator]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);

    // Agent control needs a known run, and a relaunch's own model goes
    // with a new attempt only.
    for args in [
        vec!["agent", "relaunch", "alpha-1-build", "--note", "x"],
        vec!["agent", "resume", "alpha-1-build"],
        vec!["agent", "interrupt", "alpha-1-build"],
    ] {
        let out = world.run(&args);
        assert_eq!((out.code, out.error_code()), (3, "not_found"), "{args:?}");
    }
    let out = world.run(&[
        "agent",
        "relaunch",
        "alpha-1-build",
        "--note",
        "x",
        "--model",
        "opus",
        "--effort",
        "high",
    ]);
    assert_eq!((out.code, out.error_code()), (2, "usage"), "{}", out.json);
    assert_eq!(world.store().last_seq().unwrap(), seq);

    // 7: the daemon is needed and absent, and nothing was written.
    for args in [
        vec!["mem", "approve", "01JC00000000000000000000P1"],
        vec!["board", "--login"],
    ] {
        let out = world.run(&args);
        assert_eq!((out.code, out.error_code()), (7, "unavailable"), "{args:?}");
    }
    assert_eq!(
        world.store().last_seq().unwrap(),
        seq,
        "an unavailable command writes nothing"
    );

    // Before init there is no state to read.
    let empty = World::new();
    let out = empty.run(&["card", "list"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"));
    assert!(!empty.db().exists(), "a read never creates state.db");
}

/// A session's home always exists, so a missing `state.db` under a session
/// most likely means the sandbox hid it: the refusal names the packaged path,
/// the form that runs outside it with or without hooks (`authority.md`
/// section 7, "Routes and their controls").
#[test]
fn a_session_that_cannot_see_state_db_is_told_it_ran_sandboxed() {
    let empty = World::new();
    let out = empty.run_as("01JSESSION", &["handoff", "show"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);
    let error = out.json["error"].to_string();
    let packaged = format!("`{} <command>`", env!("CARGO_BIN_EXE_tiller"));
    assert!(error.contains("inside the sandbox"), "{error}");
    assert!(error.contains(&packaged), "{error}");
    assert!(error.contains("tiller init"), "{error}");
    empty.set_env("TILLER_RETRO_ID", "01JRETRO");
    let out = empty.run(&["retro", "show", "01JRETRO"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);
    let error = out.json["error"].to_string();
    assert!(error.contains("inside the sandbox"), "{error}");
    assert!(error.contains(&packaged), "{error}");
    let out = World::new().run(&["handoff", "show"]);
    assert!(
        out.json["error"].to_string().contains("run tiller init"),
        "{}",
        out.json
    );
}

#[test]
fn orchestrator_start_needs_the_project_checkout_configured() {
    let world = World::initialized();
    let seq = world.store().last_seq().unwrap();
    let out = world.run(&["orchestrator", "start", "alpha"]);
    assert_eq!((out.code, out.error_code()), (2, "usage"));
    assert!(
        out.json["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("checkouts.alpha")),
        "{}",
        out.json
    );
    assert_eq!(world.store().last_seq().unwrap(), seq, "nothing is written");
    let out = world.run(&["orchestrator", "start", "nope"]);
    assert_eq!((out.code, out.error_code()), (3, "not_found"));
}

#[test]
fn bad_arguments_print_an_error_response_and_exit_2() {
    let world = World::new();
    let out = world.run(&["card", "create"]);
    assert_eq!((out.code, out.error_code()), (2, "usage"));
}

#[test]
fn toon_is_the_default_and_shows_lisbon_time() {
    let world = World::initialized();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tiller"))
        .env("TILLER_HOME", world.home())
        .args(["project", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("ok: true\ndata:\n"), "{text}");
    assert!(text.contains("id: alpha"), "{text}");
    // A stored time shows with its weekday, not as RFC 3339.
    assert!(!text.contains(".000Z") && !text.contains("T13"), "{text}");
    assert!(
        text.contains("added_at: \"") && text.contains(" 20"),
        "{text}"
    );
}
