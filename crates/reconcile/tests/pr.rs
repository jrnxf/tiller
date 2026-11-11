//! R07 and R19 on pull requests: closed without merging, a leaking body,
//! a force-push after approval.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{
    ApprovalState, AttentionKind, CardState, Checks, GrantKind, OutboxKind, PrState,
};
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

/// Stores `key` in `idempotency_keys` as `actor` stored it (`None`: a key
/// from before schema 15, which records no actor).
fn plant_key(w: &World, key: &str, actor: Option<&str>) {
    w.store()
        .conn()
        .execute(
            "INSERT INTO idempotency_keys (key, command, entity, entity_id, response, created_at, actor)
             VALUES (?1, 'guard.breach', 'card', 'x', '{}', ?2, ?3)",
            rusqlite::params![key, now(), actor],
        )
        .unwrap();
}

/// G30 (`protocol.md` 6.3): the pass's breach key, already used by another
/// actor, is not the pass's own write. A worker that stores
/// `reconcile:G30:<pr>:<head>` first must not silence the breach on its own
/// PR: the pass tries the write, the store refuses the key, and the pass
/// reports the refusal. A key from before schema 15 has no actor and still
/// counts as written.
#[test]
fn a_breach_key_another_actor_used_does_not_silence_the_breach() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());

    let mut leaking = open_pr("b2b2b2b2");
    leaking.body_leaks = vec!["> a line the owner wrote in chat".into()];
    w.forge.pr(REPO, 7, leaking.clone());
    let key = format!("reconcile:G30:{pr}:b2b2b2b2");
    plant_key(&w, &key, Some(&format!("session:{worker}")));
    let report = w.pass(now().plus_minutes(1));
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains(&key) && error.contains("not daemon")),
        "{report:?}"
    );
    assert!(!report.did("G30"), "{report:?}");

    // The same key with no actor, as schema 14 stored it: already written.
    let old = format!("reconcile:G30:{pr}:c3c3c3c3");
    plant_key(&w, &old, None);
    leaking.fetch.head_sha = Some("c3c3c3c3".into());
    w.forge.pr(REPO, 7, leaking);
    w.forge.descends("b2b2b2b2", "c3c3c3c3");
    let report = w.pass(now().plus_minutes(2));
    assert!(!report.did("G30"), "{report:?}");
    assert!(
        report.errors.iter().all(|error| !error.contains(&old)),
        "{report:?}"
    );
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

/// P14: a CI-green transition wakes the orchestrator once; a poll that
/// changes nothing about the checks wakes nobody again.
#[test]
fn ci_green_event_wakes_orchestrator_once() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let run = w.run_of(&worker);
    let wakes = |w: &World| {
        w.messages_to(&w.fleet.orchestrator_session.clone())
            .into_iter()
            .filter(|m| m.kind == OutboxKind::Wake && m.body.contains("checks passed"))
            .count()
    };

    let mut pending = open_pr("a1a1a1a1");
    pending.fetch.checks = Checks::Pending;
    w.forge.pr(REPO, 7, pending);
    w.pass(now());
    assert_eq!(wakes(&w), 0, "pending checks wake nobody");

    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now().plus_minutes(1));
    assert_eq!(wakes(&w), 1, "the green transition wakes once");
    let message = w
        .messages_to(&w.fleet.orchestrator_session.clone())
        .into_iter()
        .find(|m| m.body.contains("checks passed"))
        .unwrap();
    assert_eq!(message.card_id.as_deref(), Some(run.card_id.as_str()));
    assert!(message.body.contains("PR #7"), "{}", message.body);

    w.pass(now().plus_minutes(2));
    assert_eq!(wakes(&w), 1, "an unchanged poll does not wake again");
    assert_eq!(w.pr(&pr).checks, Checks::Success);
}
