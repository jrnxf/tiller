//! R07 and R19 on pull requests: closed without merging, a leaking body,
//! a force-push after approval.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{ApprovalState, AttentionKind, CardState, GrantKind, PrState};
use tiller_core::state::approvals::NewGrant;
use tiller_core::state::list;

/// R07: a PR closed without merging asks the owner; the card is not
/// cancelled on inference.
#[test]
fn pr_closed_without_merge_opens_recovery_item() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", false);
    let run = w.run_of(&worker);
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());
    w.forge.pr(REPO, 7, closed_pr("a1a1a1a1"));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("closed without merging"), "{report:?}");
    assert_eq!(w.pr(&pr).state, PrState::Closed);
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let items = w.open_items(&run.card_id);
    assert_eq!(
        items.len(),
        1,
        "the merge item was withdrawn, the question opened: {items:?}"
    );
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0].question.contains("closed without merging"),
        "{}",
        items[0].question
    );
    assert_eq!(w.card(&run.card_id).state, CardState::Open);
    assert!(w.run(&run.id).ended_at.is_none());
}

/// R19, G30: while a PR body fails the scrub no grant matches and no
/// approval is requested for it; the breach is recorded once per head, the
/// orchestrator gets the lines, and a public repo goes to the developer at
/// once.
#[test]
fn leaking_body_blocks_grant_match_and_request() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    let run = w.run_of(&worker);
    w.store_mut()
        .grant_create(
            &owner(),
            NewGrant {
                project_id: "alpha".into(),
                kind: GrantKind::Merge,
                conditions: serde_json::json!({"v": 1, "actions": ["squash"], "checks": "success"}),
            },
        )
        .unwrap();
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());

    let mut leaking = open_pr("b2b2b2b2");
    leaking.body_leaks =
        vec!["> please fix the thing the owner asked about in chat yesterday".into()];
    leaking.repo_public = Some(true);
    w.forge.pr(REPO, 7, leaking);
    w.forge.descends("a1a1a1a1", "b2b2b2b2");
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "b2b2b2b2", false, Some(1));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("G30"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let approvals: Vec<tiller_core::domain::Approval> =
        list(w.store().conn(), "pr_id = ?1", &[&pr]).unwrap();
    assert_eq!(
        approvals.len(),
        1,
        "no rematch and no request while the body leaks: {approvals:?}"
    );
    assert_eq!(w.breaches("G30").len(), 1);
    assert!(
        w.notes().iter().any(|n| n.body.contains("fails the scrub")),
        "{:?}",
        w.notes()
    );
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0].question.contains("public repo"),
        "{}",
        items[0].question
    );
    w.pass(now().plus_minutes(2));
    assert_eq!(w.breaches("G30").len(), 1, "once per head");
}

/// R19, G11: a force-push after approval is flagged (the breach names the
/// staled approvals), the approval is stale, and the developer is asked.
#[test]
fn force_push_after_approval_is_flagged_and_stale() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    let run = w.run_of(&worker);
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());
    w.forge.pr(REPO, 7, open_pr("f9f9f9f9"));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("G11"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let breaches = w.breaches("G11");
    assert_eq!(breaches.len(), 1);
    assert_eq!(breaches[0]["from"], "a1a1a1a1");
    assert_eq!(breaches[0]["to"], "f9f9f9f9");
    assert_eq!(breaches[0]["approvals_staled"][0], approval);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1);
    assert!(
        items[0].question.contains("force-pushed"),
        "{}",
        items[0].question
    );
    w.pass(now().plus_minutes(2));
    assert_eq!(w.breaches("G11").len(), 1, "once per head");
}
