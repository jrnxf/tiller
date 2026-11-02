//! R08: stale approvals and `uncertain` executions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{
    ApprovalAction, ApprovalKind, ApprovalState, AttentionKind, CardOutcome, Delivery, Effort,
    GrantKind, RunRole,
};
use tiller_core::state::approvals::{NewApproval, NewGrant};
use tiller_core::state::cards::NewCard;
use tiller_core::state::list;
use tiller_core::state::projects::NewProject;
use tiller_core::state::runs::{NewOrchestrator, NewRun, SpawnChecks};

/// R08 (also the T4.3 acceptance): a push after approval marks it stale,
/// withdraws its merge item, and asks again at the new head.
#[test]
fn push_after_approval_marks_it_stale() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    let run = w.run_of(&worker);
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());
    assert_eq!(w.approval(&approval).state, ApprovalState::Accepted);

    // The worker pushes h2 (a fast-forward), and no grant exists.
    w.forge.pr(REPO, 7, open_pr("b2b2b2b2"));
    w.forge.descends("a1a1a1a1", "b2b2b2b2");
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "b2b2b2b2", false, Some(1));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("R08 PR #7 head moved"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let approvals: Vec<tiller_core::domain::Approval> = list(
        w.store().conn(),
        "pr_id = ?1 ORDER BY requested_at, id",
        &[&pr],
    )
    .unwrap();
    assert_eq!(approvals.len(), 2, "{approvals:?}");
    assert_eq!(approvals[1].state, ApprovalState::Requested);
    assert_eq!(approvals[1].head_sha.as_deref(), Some("b2b2b2b2"));
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Merge);
    assert!(
        items[0].question.contains("b2b2b2b2"),
        "{}",
        items[0].question
    );
    assert!(
        w.breaches("G11").is_empty(),
        "a fast-forward is not a force-push"
    );
}

/// R08: an `uncertain` merge is `succeeded` when the forge shows the PR
/// merged at the bound head (and the card lands), `failed` with a recovery
/// item when it shows anything else.
#[test]
fn uncertain_execution_resolves_from_forge() {
    let mut w = World::new();
    let a = w.worker("w2:p1");
    let b = w.worker("w3:p1");
    w.place();
    let pr_a = w.link_pr(&a, 1, "a1a1a1a1");
    let pr_b = w.link_pr(&b, 2, "d1d1d1d1");
    let approval_a = w.approve(&a, &pr_a, "a1a1a1a1", true);
    let approval_b = w.approve(&b, &pr_b, "d1d1d1d1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval_a, "a1a1a1a1")
        .unwrap();
    w.store_mut()
        .approval_execute(&daemon(), &approval_b, "d1d1d1d1")
        .unwrap();
    w.forge
        .pr(REPO, 1, merged_pr("a1a1a1a1", "e1e1e1e1", now()));
    w.forge.pr(REPO, 2, open_pr("d1d1d1d1"));
    let report = w.pass(now());
    assert!(report.did("R01 approval"), "{report:?}");
    assert_eq!(w.approval(&approval_a).state, ApprovalState::Succeeded);
    assert_eq!(
        w.card(&w.run_of(&a).card_id).outcome,
        Some(CardOutcome::Landed)
    );
    assert_eq!(w.approval(&approval_b).state, ApprovalState::Failed);
    let items = w.open_items(&w.run_of(&b).card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0].question.contains("Approve again"),
        "{}",
        items[0].question
    );
}

/// R08: a `uncertain` `land_direct` is resolved from the default branch:
/// the bound head reachable from it means `succeeded`.
#[test]
fn uncertain_land_direct_resolves_from_default_branch() {
    let mut w = World::new();
    w.place();
    w.store_mut()
        .project_add(
            &owner(),
            NewProject {
                id: "beta".into(),
                name: "Beta".into(),
                repo: Some("example/beta".into()),
                remote: Some("https://example.com/beta.git".into()),
                default_branch: None,
                delivery: Delivery::DirectPush,
                checks: None,
                capacity: Some(2),
                deploy_workflow: None,
                deploy_environment: None,
                deploy_timeout_minutes: None,
            },
        )
        .unwrap();
    let orchestrator = w
        .store_mut()
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "beta".into(),
                pane_id: Some("w9:p1".into()),
                claude_session_id: Some("claude-beta".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .value
        .session
        .id;
    let card = w
        .store_mut()
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "beta".into(),
                kind: tiller_core::domain::CardKind::Ship,
                title: "direct".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let started = w
        .store_mut()
        .run_start(
            &session_ctx(&orchestrator),
            None,
            NewRun {
                card: card.id.clone(),
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: Some("/home/user/worktrees/beta-1".into()),
                branch: Some("fm/beta-1".into()),
                base_branch: Some("main".into()),
                base_sha: Some("base".into()),
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value;
    let approval = w
        .store_mut()
        .approval_request(
            &session_ctx(&orchestrator),
            NewApproval {
                kind: ApprovalKind::LandDirect,
                card: card.id.clone(),
                run_id: Some(started.run.id.clone()),
                pr_id: None,
                branch: Some("fm/beta-1".into()),
                head_sha: "a1a1a1a1".into(),
                action: ApprovalAction::Push,
                memory_paths: None,
                grant_id: None,
            },
        )
        .unwrap()
        .value;
    w.store_mut()
        .approval_decide(
            &owner(),
            None,
            &approval.id,
            tiller_core::state::approvals::Decision {
                accept: true,
                via: tiller_core::domain::AnsweredVia::Cli,
                memory: Vec::new(),
                ask_id: None,
            },
        )
        .unwrap();
    w.store_mut()
        .approval_execute(&daemon(), &approval.id, "a1a1a1a1")
        .unwrap();
    w.scanner.worktree(
        "/home/user/worktrees/beta-1",
        "fm/beta-1",
        "a1a1a1a1",
        false,
        Some(0),
    );
    w.forge.default_head("beta", "b2b2b2b2");
    w.forge.descends("a1a1a1a1", "b2b2b2b2");
    let report = w.pass(now());
    assert!(report.did("R08 uncertain land_direct"), "{report:?}");
    assert_eq!(w.approval(&approval.id).state, ApprovalState::Succeeded);
    assert_eq!(w.card(&card.id).outcome, Some(CardOutcome::Landed));
    assert!(
        w.breaches("G10").is_empty(),
        "the daemon's own push is covered"
    );
}

/// R08, R19: a rebase or force-push stales the approval and is never
/// rematched to a grant that would otherwise match.
#[test]
fn rebase_stales_approval_and_blocks_rematch() {
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
    // h2 does not descend from h1.
    w.forge.pr(REPO, 7, open_pr("b2b2b2b2"));
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "b2b2b2b2", false, Some(1));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("force-pushed"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let approvals: Vec<tiller_core::domain::Approval> =
        list(w.store().conn(), "pr_id = ?1", &[&pr]).unwrap();
    assert_eq!(
        approvals.len(),
        1,
        "no rematch, no new request: {approvals:?}"
    );
    assert_eq!(w.breaches("G11").len(), 1);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0].question.contains("force-pushed"),
        "{}",
        items[0].question
    );
}

/// R08: a fast-forward the run itself pushed is rematched to a standing
/// grant whose conditions hold, as an `accepted` approval at the new head.
#[test]
fn fast_forward_pushed_by_run_rematches_grant() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    let run = w.run_of(&worker);
    let grant = w
        .store_mut()
        .grant_create(
            &owner(),
            NewGrant {
                project_id: "alpha".into(),
                kind: GrantKind::Merge,
                conditions: serde_json::json!({
                    "v": 1, "actions": ["squash"], "base_ref": "main", "head_ref_pattern": "fm/*",
                    "opened_by_fleet_run": true, "card_kinds": ["ship"], "checks": "success",
                    "pipeline": "passed", "exclude_paths": [".github/workflows/**"]
                }),
            },
        )
        .unwrap()
        .value;
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());
    w.forge.pr(REPO, 7, open_pr("b2b2b2b2"));
    w.forge.descends("a1a1a1a1", "b2b2b2b2");
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "b2b2b2b2", false, Some(1));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("rematched"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let fresh: tiller_core::domain::Approval = list::<tiller_core::domain::Approval>(
        w.store().conn(),
        "pr_id = ?1 AND state = 'accepted'",
        &[&pr],
    )
    .unwrap()
    .remove(0);
    assert_eq!(fresh.head_sha.as_deref(), Some("b2b2b2b2"));
    assert_eq!(fresh.grant_id.as_deref(), Some(grant.id.as_str()));
    assert!(
        w.open_items(&run.card_id).is_empty(),
        "nothing to ask the owner"
    );

    // A head the run did not push is never rematched: the owner is asked.
    w.forge.pr(REPO, 7, open_pr("c3c3c3c3"));
    w.forge.descends("b2b2b2b2", "c3c3c3c3");
    w.pass(now().plus_minutes(2));
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Merge);
}
