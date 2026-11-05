//! P15: "report it ready once CI passes" is an obligation record that the
//! event retires, whichever came first: the promise or the green checks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use serde_json::json;
use tiller_core::domain::{Checks, Obligation, ObligationState, TriggerKind};
use tiller_core::state::records::NewObligation;

const WHAT: &str = "report the PR ready once CI passes";

fn promise(w: &mut World, card_id: &str, filter: serde_json::Value) -> String {
    w.store_mut()
        .obligation_open(
            &owner(),
            NewObligation {
                project_id: "alpha".into(),
                card_id: Some(card_id.into()),
                what: WHAT.into(),
                trigger_kind: TriggerKind::Event,
                trigger_event: Some(filter),
                due_at: None,
            },
        )
        .unwrap()
        .value
        .id
}

fn obligation(w: &World, id: &str) -> Obligation {
    tiller_core::state::get(w.store().conn(), "obligation", id).unwrap()
}

fn notes(w: &World) -> i64 {
    w.count(&format!(
        "SELECT count(*) FROM outbox WHERE target_session_id = '{}' AND body = 'obligation met: {WHAT}'",
        w.fleet.orchestrator_session
    ))
}

#[test]
fn ci_green_retires_report_promise() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let card = w.run_of(&worker).card_id;
    let id = promise(&mut w, &card, json!({"kind": "pr.checks"}));

    // Checks still pending: the promise stays open.
    let mut pending = open_pr("a1a1a1a1");
    pending.fetch.checks = Checks::Pending;
    w.forge.pr(REPO, 7, pending);
    w.pass(now());
    assert_eq!(obligation(&w, &id).state, ObligationState::Open);
    assert_eq!(notes(&w), 0);

    // Green: met in the transaction that records the trigger, with the note.
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("P15"), "{report:?}");
    let met = obligation(&w, &id);
    assert_eq!(met.state, ObligationState::Met);
    let seq = met.met_by_event_seq.unwrap();
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM events WHERE seq = {seq} AND kind = 'pr.checks' AND entity_id = '{pr}'"
        )),
        1
    );
    assert_eq!(notes(&w), 1);

    // Retired once.
    w.pass(now().plus_minutes(2));
    assert_eq!(notes(&w), 1);
}

#[test]
fn promise_made_after_green_checks_retires_on_the_next_pass() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    w.link_pr(&worker, 7, "a1a1a1a1");
    let card = w.run_of(&worker).card_id;
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());

    let id = promise(&mut w, &card, json!({"kind": "pr.checks"}));
    w.pass(now().plus_minutes(1));
    assert_eq!(obligation(&w, &id).state, ObligationState::Met);
}

#[test]
fn other_pr_or_other_checks_leave_the_promise_open() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    w.link_pr(&worker, 7, "a1a1a1a1");
    let card = w.run_of(&worker).card_id;
    let other_pr = promise(&mut w, &card, json!({"kind": "pr.checks", "number": 8}));
    let failure = promise(
        &mut w,
        &card,
        json!({"kind": "pr.checks", "checks": "failure"}),
    );
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());
    assert_eq!(obligation(&w, &other_pr).state, ObligationState::Open);
    assert_eq!(obligation(&w, &failure).state, ObligationState::Open);
}
