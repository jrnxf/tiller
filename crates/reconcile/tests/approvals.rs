//! R08: stale approvals and `uncertain` executions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{
    AnsweredVia, ApprovalAction, ApprovalKind, ApprovalState, AttentionKind, CardOutcome, Coverage,
    Delivery, Effort, GrantKind, Reviewer, RunOutcome, RunRole, Verdict,
};
use tiller_core::state::approvals::{NewApproval, NewGrant};
use tiller_core::state::cards::NewCard;
use tiller_core::state::list;
use tiller_core::state::projects::NewProject;
use tiller_core::state::records::NewReview;
use tiller_core::state::runs::{EndRun, NewOrchestrator, NewRun, SpawnChecks};

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
    assert!(
        items[0]
            .question
            .contains("is failed: the merge did not happen"),
        "the item shows the state the pass recorded: {}",
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

/// P34: while the owner is away with `merge: "none"`, no standing grant
/// merges, even for a head the run pushed; the owner is asked instead.
#[test]
fn away_with_no_merges_stops_a_standing_grant_matching() {
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
                conditions: serde_json::json!({
                    "v": 1, "actions": ["squash"], "base_ref": "main", "head_ref_pattern": "fm/*",
                    "opened_by_fleet_run": true, "card_kinds": ["ship"], "checks": "success",
                    "pipeline": "passed", "exclude_paths": [".github/workflows/**"]
                }),
            },
        )
        .unwrap();
    w.store_mut()
        .away_start(
            &owner(),
            serde_json::json!({
                "until_at": now().plus_minutes(24 * 60),
                "projects": ["alpha"],
                "merge": "none",
            }),
        )
        .unwrap();
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());
    w.forge.pr(REPO, 7, open_pr("b2b2b2b2"));
    w.forge.descends("a1a1a1a1", "b2b2b2b2");
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "b2b2b2b2", false, Some(1));
    let report = w.pass(now().plus_minutes(1));
    assert!(!report.did("rematched"), "{report:?}");
    assert_eq!(w.approval(&approval).state, ApprovalState::Stale);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Merge);
}

/// G04 (`authority.md` section 11): the owner's decision passes the pass; a
/// decision some other principal wrote raises the unauthorized-action
/// trigger once, with a recovery item on the card.
#[test]
fn decision_by_wrong_principal_opens_recovery() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    let card = w.run_of(&worker).card_id;
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    let report = w.pass(now());
    assert!(!report.did("G04"), "{report:?}");
    assert!(w.breaches("G04").is_empty());

    let orchestrator = w.fleet.orchestrator_session.clone();
    w.store()
        .conn()
        .execute(
            "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, card_id, payload)
             VALUES ('01JC000000000000000000F0RG', ?1, 'approval.decide', ?2, 'approval', ?3,
                     'approval.decided', ?4, '{\"accepted\": true}')",
            rusqlite::params![now(), format!("session:{orchestrator}"), approval, card],
        )
        .unwrap();
    let report = w.pass(now().plus_minutes(1));
    assert!(
        report.did(&format!(
            "G04 approval {approval} decided by session:{orchestrator}"
        )),
        "{report:?}"
    );
    let breaches = w.breaches("G04");
    assert_eq!(breaches.len(), 1, "{breaches:?}");
    assert_eq!(breaches[0]["event_id"], "01JC000000000000000000F0RG");
    let items: Vec<_> = w
        .open_items(&card)
        .into_iter()
        .filter(|item| item.kind == AttentionKind::Recovery)
        .collect();
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].question.contains(&approval),
        "{}",
        items[0].question
    );

    let report = w.pass(now().plus_minutes(2));
    assert!(!report.did("G04"), "{report:?}");
    assert_eq!(w.breaches("G04").len(), 1);
}

/// The builder run ends before the merge approval is asked (the daemon
/// publishes the PR after the run), yet the owner's question stays current:
/// a merge item follows the card, its open PR and the bound head, not the
/// run that asked. It stales only when the head moves.
#[test]
fn a_merge_ask_outlives_the_run_that_raised_it() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let run = w.run_of(&worker);
    w.store_mut()
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    w.approve(&worker, &pr, "a1a1a1a1", false);
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Merge);
    assert!(
        tiller_core::state::attention::still_current(w.store().conn(), &items[0]).unwrap(),
        "a merge ask is not stale because its run ended"
    );
}

/// B11: R08's rematch counts only a review recorded at the new head. A pass
/// at the old head leaves the new head to the owner; a pass at the new head
/// lets the grant accept it.
#[test]
fn rematch_needs_a_review_at_the_new_head() {
    const OLD: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
    const NEW: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
    let rematch = |reviewed: &str| {
        let mut w = World::new();
        let worker = w.worker("w2:p1");
        w.place();
        let pr = w.link_pr(&worker, 7, OLD);
        w.approve(&worker, &pr, OLD, true);
        let run = w.run_of(&worker);
        w.store_mut()
            .grant_create(
                &owner(),
                NewGrant {
                    project_id: "alpha".into(),
                    kind: GrantKind::Merge,
                    conditions: serde_json::json!({
                        "v": 1, "actions": ["squash"], "checks": "success",
                        "reviews": {"require": ["code-review"], "max_high": 0}
                    }),
                },
            )
            .unwrap();
        w.store_mut()
            .review_record(
                &owner(),
                NewReview {
                    run_id: run.id.clone(),
                    reviewer: Reviewer::CodeReview,
                    coverage: Coverage::Diff,
                    findings_high: 0,
                    findings_medium: 0,
                    findings_low: 0,
                    verdict: Verdict::Pass,
                    source_url: None,
                    head_sha: reviewed.into(),
                },
            )
            .unwrap();
        w.forge.pr(REPO, 7, open_pr(OLD));
        w.pass(now());
        w.forge.pr(REPO, 7, open_pr(NEW));
        w.forge.descends(OLD, NEW);
        let path = run.worktree_path.clone().unwrap();
        w.scanner.worktree(&path, "fm/work", NEW, false, Some(1));
        w.pass(now().plus_minutes(1))
    };
    let report = rematch(OLD);
    assert!(report.did("no grant matched"), "{report:?}");
    let report = rematch(NEW);
    assert!(report.did("rematched to a standing grant"), "{report:?}");
}

/// R08: a refused write while one `uncertain` merge resolves is reported,
/// and the next `uncertain` merge on the same PR is still resolved.
#[test]
fn a_refused_uncertain_merge_does_not_block_the_next() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let mut uncertain = Vec::new();
    for _ in 0..2 {
        let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
        w.store_mut()
            .approval_execute(&daemon(), &approval, "a1a1a1a1")
            .unwrap();
        w.store()
            .conn()
            .execute(
                "UPDATE approvals SET state = 'uncertain' WHERE id = ?1",
                [&approval],
            )
            .unwrap();
        uncertain.push(approval);
    }
    uncertain.sort();
    let (first, second) = (&uncertain[0], &uncertain[1]);
    w.store()
        .conn()
        .execute_batch(&format!(
            "CREATE TRIGGER refuse_first BEFORE UPDATE ON approvals \
             WHEN OLD.id = '{first}' BEGIN SELECT RAISE(ABORT, 'refused for the test'); END;"
        ))
        .unwrap();
    w.forge
        .pr(REPO, 7, merged_pr("a1a1a1a1", "e1e1e1e1", now()));
    let report = w.pass(now());
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains(&format!("uncertain approval {first}"))),
        "{report:?}"
    );
    assert_eq!(w.approval(first).state, ApprovalState::Uncertain);
    assert_eq!(w.approval(second).state, ApprovalState::Succeeded);
}

/// R08: a `failed` resolution whose recovery item could not be opened in
/// its pass gets the item from a later pass, once.
#[test]
fn a_failed_execution_gets_its_item_on_a_later_pass() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.store()
        .conn()
        .execute_batch(
            "CREATE TRIGGER no_items BEFORE INSERT ON attention \
             BEGIN SELECT RAISE(ABORT, 'no items for the test'); END;",
        )
        .unwrap();
    let report = w.pass(now());
    assert_eq!(w.approval(&approval).state, ApprovalState::Failed);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains("no items for the test")),
        "{report:?}"
    );
    let card = w.run_of(&worker).card_id;
    let failed_items = |w: &World| {
        w.open_items(&card)
            .into_iter()
            .filter(|item| {
                item.question
                    .contains("is failed: the merge did not happen")
            })
            .count()
    };
    assert_eq!(failed_items(&w), 0);
    w.store()
        .conn()
        .execute_batch("DROP TRIGGER no_items;")
        .unwrap();
    let report = w.pass(now().plus_minutes(1));
    assert!(
        report.did(&format!("R08 approval {approval} failed: recovery item")),
        "{report:?}"
    );
    assert_eq!(failed_items(&w), 1);
    let report = w.pass(now().plus_minutes(2));
    assert!(!report.did("R08 approval"), "once: {report:?}");
    assert_eq!(failed_items(&w), 1);
}

/// R08: an execution the forge would not answer for ten minutes goes to
/// the developer as `uncertain`. When the forge answers later and the
/// execution is `failed`, the developer is told that too.
#[test]
fn an_unreadable_execution_that_failed_says_failed() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    w.forge.pr_error(REPO, 7, "the forge is down");
    w.pass(now().plus_minutes(11));
    assert_eq!(w.approval(&approval).state, ApprovalState::Uncertain);
    let card = w.run_of(&worker).card_id;
    let items = w.open_items(&card);
    assert!(
        items.iter().any(|item| item
            .question
            .contains("is uncertain: the forge has been unreadable")),
        "{items:?}"
    );

    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now().plus_minutes(12));
    assert_eq!(w.approval(&approval).state, ApprovalState::Failed);
    let items = w.open_items(&card);
    assert!(
        items.iter().any(|item| item
            .question
            .contains("is failed: the merge did not happen")),
        "the item shows the final state: {items:?}"
    );
    assert!(
        items.iter().any(|item| item
            .question
            .contains("the forge answers now, after the earlier question")),
        "it says it follows the uncertain one: {items:?}"
    );
}

/// R08: a `failed` approval a newer approval of its kind followed is
/// settled, so a later pass does not ask about it.
#[test]
fn a_failed_execution_a_newer_approval_followed_is_not_asked_about() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.store()
        .conn()
        .execute_batch(
            "CREATE TRIGGER no_items BEFORE INSERT ON attention \
             BEGIN SELECT RAISE(ABORT, 'no items for the test'); END;",
        )
        .unwrap();
    w.pass(now());
    assert_eq!(w.approval(&approval).state, ApprovalState::Failed);
    w.store()
        .conn()
        .execute_batch("DROP TRIGGER no_items;")
        .unwrap();
    let newer = w.approve(&worker, &pr, "a1a1a1a1", true);
    assert_ne!(newer, approval);
    let report = w.pass(now().plus_minutes(1));
    assert!(
        !report.did(&format!("R08 approval {approval}")),
        "settled: {report:?}"
    );
}

/// R08: a `failed` item's key another actor holds is reported, not
/// skipped without a word.
#[test]
fn a_failed_execution_key_another_actor_holds_is_reported() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    let key = format!("reconcile:R08:failed:{approval}");
    plant_key(&w, &key, Some("session:another"));
    let report = w.pass(now());
    assert_eq!(w.approval(&approval).state, ApprovalState::Failed);
    assert!(
        report
            .errors
            .iter()
            .any(|e| e.contains(&format!("pass key {key}"))),
        "{report:?}"
    );
}

/// R08: the developer answered the unreadable item, and nothing acted on
/// it, so the `failed` resolution still gets its own item.
#[test]
fn a_failed_execution_whose_unreadable_item_was_answered_says_failed() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    w.forge.pr_error(REPO, 7, "the forge is down");
    w.pass(now().plus_minutes(11));
    let card = w.run_of(&worker).card_id;
    let items = w.open_items(&card);
    let uncertain = items
        .iter()
        .find(|item| item.question.contains("is uncertain"))
        .unwrap_or_else(|| panic!("{items:?}"));
    w.store_mut()
        .attention_answer(
            &owner(),
            None,
            &uncertain.id,
            "Approve again",
            AnsweredVia::Board,
        )
        .unwrap();
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now().plus_minutes(12));
    assert_eq!(w.approval(&approval).state, ApprovalState::Failed);
    let items = w.open_items(&card);
    assert!(
        items.iter().any(|item| item
            .question
            .contains("is failed: the merge did not happen")),
        "{items:?}"
    );
}

/// R08: an unreadable key another actor planted is no earlier question
/// the pass asked, so the `failed` item does not refer to one.
#[test]
fn a_failed_item_does_not_refer_to_a_question_another_actor_planted() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let approval = w.approve(&worker, &pr, "a1a1a1a1", true);
    w.store_mut()
        .approval_execute(&daemon(), &approval, "a1a1a1a1")
        .unwrap();
    plant_key(
        &w,
        &format!("reconcile:R08:execution:{approval}"),
        Some("session:another"),
    );
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now());
    let card = w.run_of(&worker).card_id;
    let items = w.open_items(&card);
    let item = items
        .iter()
        .find(|item| {
            item.question
                .contains("is failed: the merge did not happen")
        })
        .unwrap_or_else(|| panic!("{items:?}"));
    assert!(
        !item.question.contains("earlier question"),
        "{}",
        item.question
    );
}
