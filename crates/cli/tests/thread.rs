//! `tiller thread`: the owner's chat with a project's orchestrator, read and
//! marked read with the real binary on one `state.db`. The board reads the
//! same items from `GET /api/v1/projects/{id}/thread`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{World, snap};
use serde_json::{Value, json};
use tiller_core::domain::{Checks, Mergeable, PipelineState, PrState, ReviewDecision};
use tiller_core::state::approvals::PrFetch;
use tiller_core::state::context::ContextUse;
use tiller_core::{Actor, Ctx, Timestamp};

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// The `type` of each item, oldest first.
fn types(out: &common::Out) -> Vec<String> {
    out.data()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["type"].as_str().unwrap().to_owned())
        .collect()
}

/// A merge approval on `alpha-1`, requested by the owner after the worker
/// linked PR #7 at [`HEAD`].
fn request_merge(world: &World, worker: &str) -> String {
    world.must_as(
        worker,
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
    out.data()["id"].as_str().unwrap().to_owned()
}

/// One thread holds the ask, what the orchestrator did about it, the
/// approval it led to, the reply, and the owner's decision. Opening it marks
/// the reply read once, and a page never splits an activity.
#[test]
fn an_ask_its_activity_a_reply_and_an_approval_are_one_thread() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let noted = world.must(&[
        "ask",
        "note",
        "alpha",
        "--text",
        "Can the export run nightly?",
    ]);
    let ask = noted.data()["ask"]["id"].as_str().unwrap().to_owned();
    let short = &ask[ask.len() - 7..];
    world.must_as(
        &orchestrator,
        &[
            "ask",
            "resolve",
            short,
            "new",
            "--kind",
            "ship",
            "--title",
            "Run the export nightly",
        ],
    );
    world.must_as(
        &orchestrator,
        &["card", "comment", "alpha-1", "--text", "Builder next."],
    );
    let (_, worker) = world.builder_run("alpha-1");
    let approval = request_merge(&world, &worker);
    world.must_as(
        &orchestrator,
        &[
            "reply",
            "--to",
            short,
            "--text",
            "Yes. PR #7 runs it at 02:00; it waits for your merge.",
        ],
    );
    world.must(&["approve", &approval, "--sha", HEAD]);
    world
        .store()
        .session_context_record(
            &Ctx::new(Actor::Daemon),
            &orchestrator,
            ContextUse {
                used: 82_771,
                window: Some(1_000_000),
                at: Timestamp::now(),
            },
        )
        .unwrap();

    let thread = world.must(&["thread", "alpha", "--json"]);
    snap(&world, "thread_alpha", &thread);
    assert_eq!(
        types(&thread),
        ["checkpoint", "ask", "activity", "decision", "reply"],
        "{}",
        thread.json
    );
    let items = thread.data()["items"].as_array().unwrap();
    assert_eq!(items[1]["text"], "Can the export run nightly?");
    assert_eq!(items[1]["cards"][0]["display_id"], "alpha-1");
    assert_eq!(items[1]["cards"][0]["relation"], "created");
    // The worker's PR link and the daemon's refresh are not the
    // orchestrator's: only its own steps are in the activity.
    let steps = items[2]["steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .all(|step| step["kind"] != "pr.linked" && step["kind"] != "pr.changed"),
        "{}",
        items[2]
    );
    assert!(steps.iter().any(|step| step["kind"] == "card.created"));
    // The card comment is a note queued to the orchestrator.
    assert!(steps.iter().any(|step| step["kind"] == "outbox.enqueued"));
    assert_eq!(items[2]["session_id"], orchestrator.as_str());
    assert_eq!(items[3]["source"], "approval");
    assert_eq!(items[3]["state"], "accepted");
    assert_eq!(items[3]["choice"], "accepted");
    assert_eq!(items[3]["decided_by"], "owner");
    assert_eq!(items[3]["pr"]["number"], 7);
    // The merge question the daemon opened for the approval is the
    // approval's own, not a second decision.
    assert_eq!(
        items[3]["question"],
        "Merge alpha-1 at 0123456789ab (squash)?"
    );
    assert_eq!(items[4]["reply"]["card"]["display_id"], "alpha-1");
    assert!(items[4]["reply"]["read_at"].is_null());
    assert_eq!(
        thread.data()["context"],
        json!({"used": 82_771, "window": 1_000_000})
    );
    assert!(thread.data()["next_before"].is_null());

    // Opening the thread marks its reply read, as the owner; a session is
    // refused, and a second open writes nothing.
    let refused = world.run_as(&orchestrator, &["thread", "alpha", "--mark-read"]);
    assert_eq!(refused.error_code(), "forbidden", "{}", refused.json);
    let read = world.must(&["thread", "alpha", "--mark-read", "--json"]);
    assert_eq!(read.data()["marked_read"], 1, "{}", read.json);
    assert_eq!(read.events().len(), 1);
    assert!(read.data()["items"][4]["reply"]["read_at"].is_string());
    let again = world.must(&["thread", "alpha", "--mark-read", "--json"]);
    assert_eq!(again.data()["marked_read"], 0);
    assert!(again.events().is_empty(), "{}", again.json);
    // Any session reads the thread.
    world.must_as(&orchestrator, &["thread", "alpha"]);

    // Pages from the newest: a page of 3 ends at a whole activity, and the
    // next page goes on from its first seq.
    let newest = world.must(&["thread", "alpha", "--limit", "3", "--json"]);
    assert_eq!(types(&newest), ["activity", "decision", "reply"]);
    assert_eq!(newest.data()["items"][0], items[2], "the activity is whole");
    let before = newest.data()["next_before"].as_i64().unwrap();
    let activity_seq: i64 = world
        .store()
        .conn()
        .query_row(
            "SELECT MIN(seq) FROM events WHERE actor = ?1",
            [format!("session:{orchestrator}")],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(before, activity_seq);
    let help = newest.json["help"][0].as_str().unwrap();
    assert_eq!(
        help,
        format!("Older items: tiller thread alpha --before {before}")
    );
    // The edge: the older page holds exactly the items left, so it is the
    // last one.
    let older = world.must(&[
        "thread",
        "alpha",
        "--before",
        &before.to_string(),
        "--limit",
        "2",
        "--json",
    ]);
    assert_eq!(types(&older), ["checkpoint", "ask"]);
    assert!(older.data()["next_before"].is_null(), "{}", older.json);
    assert!(older.json.get("help").is_none());
    let one = world.must(&[
        "thread",
        "alpha",
        "--before",
        &before.to_string(),
        "--limit",
        "1",
        "--json",
    ]);
    assert_eq!(types(&one), ["ask"]);
    assert_eq!(one.data()["next_before"], items[1]["seq"]);

    for limit in ["0", "201"] {
        let out = world.run(&["thread", "alpha", "--limit", limit]);
        assert_eq!(out.error_code(), "usage", "{limit}: {}", out.json);
    }
    let missing = world.run(&["thread", "nope"]);
    assert_eq!(missing.error_code(), "not_found", "{}", missing.json);
    assert_eq!(missing.data(), &Value::Null);
}
