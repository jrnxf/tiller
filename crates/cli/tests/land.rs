//! `tiller land` end to end: the owner asks, the daemon lands. The test holds
//! the daemon's lock and plays the daemon's part through the state core; the
//! CLI never merges in its own process (`authority.md` section 6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::World;
use serde_json::json;
use tiller_agents::land::DaemonLock;
use tiller_core::domain::{
    AttentionKind, Checks, Finish, Mergeable, PipelineState, PrState, ReviewDecision,
};
use tiller_core::state::approvals::PrFetch;
use tiller_core::state::attention::NewAttention;
use tiller_core::{Actor, Ctx, Store};

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// Card `alpha-1` with PR #7 at [`HEAD`] and a merge approval requested on it.
fn requested(world: &World) -> String {
    world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (_, worker) = world.builder_run("alpha-1");
    world.must_as(
        &worker,
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
                pipeline_state: PipelineState::None,
                fetch_error: None,
            },
        )
        .unwrap();
    let out = world.must(&["approval", "request", "alpha-1", "--kind", "merge"]);
    out.data()["id"].as_str().unwrap().to_owned()
}

#[test]
fn land_without_the_daemon_exits_seven() {
    let world = World::initialized();
    let approval = requested(&world);
    let out = world.run(&["land", &approval]);
    assert_eq!(
        (out.code, out.error_code()),
        (5, "refused"),
        "requested, not accepted: {}",
        out.json
    );
    world.must(&["approve", &approval, "--sha", &HEAD[..12]]);
    let out = world.run(&["land", &approval]);
    assert_eq!(
        (out.code, out.error_code()),
        (7, "unavailable"),
        "{}",
        out.json
    );
    // Nothing moved: the CLI never lands by itself.
    let state: String = world
        .store()
        .conn()
        .query_row(
            "SELECT state FROM approvals WHERE id = ?1",
            [&approval],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "accepted");
}

#[test]
fn land_waits_for_the_daemon_and_reports_the_result() {
    let world = World::initialized();
    let approval = requested(&world);
    world.must(&["approve", &approval, "--sha", &HEAD[..12]]);
    let lock = DaemonLock::acquire(&world.home()).unwrap();
    let db = world.db();
    let id = approval.clone();
    let daemon = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        let mut store = Store::open(db).unwrap();
        let ctx = Ctx::new(Actor::Daemon);
        store.approval_execute(&ctx, &id, HEAD).unwrap();
        store
            .approval_finish(&ctx, &id, Finish::Succeeded, json!({"sha": "f".repeat(40)}))
            .unwrap();
    });
    let started = Instant::now();
    let out = world.run(&["land", &approval]);
    daemon.join().unwrap();
    assert_eq!(out.code, 0, "{}", out.json);
    assert_eq!(out.data()["approval"]["state"], "succeeded");
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "it waited for the daemon"
    );
    // Natural: asking again reports the same result at once.
    let out = world.must(&["land", &approval]);
    assert_eq!(out.data()["approval"]["state"], "succeeded");
    drop(lock);
}

/// The daemon is up but cannot land yet: `tiller land` gives up after its
/// wait, names what the daemon waits on, and the approval stays accepted.
#[test]
fn land_gives_up_after_its_wait_and_names_the_notice() {
    let world = World::initialized();
    let approval = requested(&world);
    world.must(&["approve", &approval, "--sha", &HEAD[..12]]);
    let lock = DaemonLock::acquire(&world.home()).unwrap();
    let mut store = world.store();
    let card: String = store
        .conn()
        .query_row(
            "SELECT card_id FROM approvals WHERE id = ?1",
            [&approval],
            |r| r.get(0),
        )
        .unwrap();
    store
        .attention_open(
            &lock.ctx(),
            Some(&format!("land:{approval}:identity")),
            NewAttention {
                card,
                kind: AttentionKind::Merge,
                question: "the merge App gave no token".into(),
                evidence: Some(json!({"approval_id": approval}).to_string()),
                ..NewAttention::default()
            },
        )
        .unwrap();
    let started = Instant::now();
    let out = world.run(&["land", &approval, "--wait", "1"]);
    assert!(started.elapsed() >= Duration::from_secs(1), "it waited");
    assert_eq!(
        (out.code, out.error_code()),
        (7, "unavailable"),
        "{}",
        out.json
    );
    let current = &out.json["error"]["current"];
    assert_eq!(current["approval"]["state"], "accepted");
    assert_eq!(
        current["waiting_on"][0]["question"],
        "the merge App gave no token"
    );
    drop(lock);
}

#[test]
fn land_with_an_unreadable_result_exits_eight() {
    let world = World::initialized();
    let approval = requested(&world);
    world.must(&["approve", &approval, "--sha", &HEAD[..12]]);
    let mut store = world.store();
    let ctx = Ctx::new(Actor::Daemon);
    store.approval_execute(&ctx, &approval, HEAD).unwrap();
    store
        .approval_finish(
            &ctx,
            &approval,
            Finish::Uncertain,
            json!({"error": "timed out"}),
        )
        .unwrap();
    let out = world.run(&["land", &approval]);
    assert_eq!(
        (out.code, out.error_code()),
        (8, "uncertain"),
        "{}",
        out.json
    );
}

#[test]
fn land_is_owner_only() {
    let world = World::initialized();
    let approval = requested(&world);
    let orchestrator = world.orchestrator();
    world.must(&["approve", &approval, "--sha", &HEAD[..12]]);
    let out = world.run_as(&orchestrator, &["land", &approval]);
    assert_eq!(
        (out.code, out.error_code()),
        (6, "forbidden"),
        "{}",
        out.json
    );
}

/// T5: a second owner run with no `--key` of `approval request` returns the
/// open approval, and of `approve` is refused: neither writes anything.
#[test]
fn a_second_owner_run_of_request_and_approve_is_harmless() {
    let world = World::initialized();
    let approval = requested(&world);
    let seq = |world: &World| world.store().last_seq().unwrap();

    let again = world.must(&["approval", "request", "alpha-1", "--kind", "merge"]);
    assert_eq!(again.data()["id"], approval.as_str());
    let before = seq(&world);
    let sha = &HEAD[..12];
    let first = world.must(&["approve", &approval, "--sha", sha]);
    assert!(!first.replayed(), "{}", first.json);
    let accepted = seq(&world);
    assert!(accepted > before);
    let second = world.run(&["approve", &approval, "--sha", sha]);
    // Refused with the state it finds. A retry that must replay carries --key.
    assert_eq!(
        (second.code, second.error_code()),
        (5, "refused"),
        "{}",
        second.json
    );
    assert_eq!(seq(&world), accepted, "a second accept wrote");
    let reject = world.run(&["approve", &approval, "--reject"]);
    assert_eq!(
        (reject.code, reject.error_code()),
        (5, "refused"),
        "{}",
        reject.json
    );
    assert_eq!(seq(&world), accepted, "a reject after an accept wrote");
}
