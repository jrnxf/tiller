//! A reopen starts the card over (`state.md` section 6): no reconcile rule
//! reads a merge, an approval, an answer or a review recorded before the
//! card's last reopen as evidence for the work after it (R07, G10, G33).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use serde_json::json;
use tiller_core::domain::lifecycle::Finish;
use tiller_core::domain::{
    AnsweredVia, ApprovalAction, ApprovalKind, AttentionKind, CardKind, CardOutcome, CardState,
    Coverage, Delivery, Effort, Reviewer, RunRole, Verdict,
};
use tiller_core::state::approvals::{Decision, NewApproval};
use tiller_core::state::cards::NewCard;
use tiller_core::state::projects::NewProject;
use tiller_core::state::records::NewReview;
use tiller_core::state::runs::{NewOrchestrator, NewRun, SpawnChecks, Started};
use tiller_reconcile::recovery::approvals::{LANDED_BY_ME, SUPERSEDED};

/// A builder run on `card`, started by the owner.
fn start_run(
    w: &mut World,
    card: &str,
    path: &str,
    branch: Option<&str>,
    base: Option<&str>,
) -> Started {
    w.store_mut()
        .run_start(
            &owner(),
            None,
            NewRun {
                card: card.to_owned(),
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: Some(path.into()),
                branch: branch.map(str::to_owned),
                base_branch: branch.map(|_| "main".to_owned()),
                base_sha: base.map(str::to_owned),
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value
}

/// A worker's PR merged outside Tiller, R07's question answered `answer`,
/// the card closed with `outcome`, then reopened with a rework run. Returns
/// the card id and the rework run id.
fn outside_merge_answered_then_reopened(
    w: &mut World,
    answer: &str,
    outcome: CardOutcome,
) -> (String, String) {
    let worker = w.worker("w2:p1");
    w.place();
    w.scanner.list(Some(Vec::new()));
    let run = w.run_of(&worker);
    w.link_pr(&worker, 7, "a1a1a1a1");
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "a1a1a1a1", false, Some(1));
    let t0 = now();
    w.forge.pr(REPO, 7, merged_pr("a1a1a1a1", "e1e1e1e1", t0));
    w.pass(t0);
    let recovery = w
        .open_items(&run.card_id)
        .into_iter()
        .find(|item| item.kind == AttentionKind::Recovery)
        .expect("R07 asks about the merge");
    w.store_mut()
        .attention_answer(&owner(), None, &recovery.id, answer, AnsweredVia::Board)
        .unwrap();
    // The daemon carries out either answer: it closes the card on it.
    let report = w.pass(t0.plus_minutes(1));
    assert!(report.errors.is_empty(), "{report:?}");
    let closed = w.card(&run.card_id);
    assert_eq!(closed.outcome, Some(outcome));
    assert_eq!(closed.closed_by.as_deref(), Some("daemon"));

    w.store_mut()
        .card_reopen(&owner(), &run.card_id, "the fix regressed")
        .unwrap();
    let rework = start_run(w, &run.card_id, "/home/user/worktrees/rework", None, None);
    w.scanner.worktree(
        "/home/user/worktrees/rework",
        "fm/rework",
        "a1a1a1a1",
        false,
        Some(0),
    );
    (run.card_id, rework.run.id)
}

/// R07: the owner's "Mark landed" from before a reopen is not an answer
/// about the rework. No pass ends the rework run on it, and no pass reports
/// the refusal core would give.
#[test]
fn a_landed_answer_before_a_reopen_ends_no_rework_run() {
    let mut w = World::new();
    let (card, rework) =
        outside_merge_answered_then_reopened(&mut w, LANDED_BY_ME, CardOutcome::Landed);
    for minutes in [2, 3] {
        let report = w.pass(now().plus_minutes(minutes));
        assert!(report.errors.is_empty(), "pass at +{minutes}: {report:?}");
        assert!(!report.did("R07"), "pass at +{minutes}: {report:?}");
    }
    assert!(w.run(&rework).ended_at.is_none());
    assert_eq!(w.card(&card).state, CardState::Open);
}

/// R07: a "Superseded" answer from before a reopen does not close the
/// reopened card again, nor abandon its rework run.
#[test]
fn a_superseded_answer_before_a_reopen_does_not_close_the_card_again() {
    let mut w = World::new();
    let (card, rework) =
        outside_merge_answered_then_reopened(&mut w, SUPERSEDED, CardOutcome::Superseded);
    for minutes in [2, 3] {
        let report = w.pass(now().plus_minutes(minutes));
        assert!(report.errors.is_empty(), "pass at +{minutes}: {report:?}");
        assert!(!report.did("R07"), "pass at +{minutes}: {report:?}");
    }
    assert!(w.run(&rework).ended_at.is_none());
    assert_eq!(w.card(&card).state, CardState::Open);
}

/// G10, R07 on a direct-push project: the `land_direct` that landed the
/// card before a reopen does not cover the rework run's head reaching the
/// default branch outside `tiller land`.
#[test]
fn a_land_direct_before_a_reopen_does_not_cover_a_new_landing() {
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
    w.store_mut()
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "beta".into(),
                pane_id: Some("w9:p1".into()),
                claude_session_id: Some("claude-beta".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let card = w
        .store_mut()
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "beta".into(),
                kind: CardKind::Ship,
                title: "direct".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let first = start_run(
        &mut w,
        &card.id,
        "/home/user/worktrees/beta-1",
        Some("fm/beta-1"),
        Some("base"),
    );
    let approval = w
        .store_mut()
        .approval_request(
            &owner(),
            NewApproval {
                kind: ApprovalKind::LandDirect,
                card: card.id.clone(),
                run_id: Some(first.run.id.clone()),
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
            Decision {
                accept: true,
                via: AnsweredVia::Cli,
                memory: Vec::new(),
                ask_id: None,
            },
        )
        .unwrap();
    w.store_mut()
        .approval_execute(&daemon(), &approval.id, "a1a1a1a1")
        .unwrap();
    w.store_mut()
        .approval_finish(
            &daemon(),
            &approval.id,
            Finish::Succeeded,
            json!({"sha": "a1a1a1a1"}),
        )
        .unwrap();
    assert_eq!(w.card(&card.id).outcome, Some(CardOutcome::Landed));

    w.store_mut()
        .card_reopen(&owner(), &card.id, "the fix regressed")
        .unwrap();
    let rework = start_run(
        &mut w,
        &card.id,
        "/home/user/worktrees/beta-2",
        Some("fm/beta-2"),
        Some("a1a1a1a1"),
    );
    // The rework's head reaches the default branch by a plain push.
    w.scanner.worktree(
        "/home/user/worktrees/beta-2",
        "fm/beta-2",
        "c3c3c3c3",
        false,
        Some(1),
    );
    w.forge.default_head("beta", "d4d4d4d4");
    w.forge.descends("c3c3c3c3", "d4d4d4d4");
    let report = w.pass(now());
    assert!(report.did("G10"), "{report:?}");
    assert_eq!(w.breaches("G10").len(), 1);
    let items = w.open_items(&card.id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].question.contains("no land_direct approval"),
        "{}",
        items[0].question
    );
    assert!(w.run(&rework.run.id).ended_at.is_none());
}

/// G33: a no-mistakes ask-user finding the pass never saw before the card
/// closed is about the work before the reopen: the reopened card gets no
/// item for it. A finding recorded after the reopen still goes to the owner.
#[test]
fn an_ask_user_finding_before_a_reopen_is_not_raised_on_the_reopened_card() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let review = |w: &mut World, run_id: &str| {
        w.store_mut()
            .review_record(
                &owner(),
                NewReview {
                    run_id: run_id.into(),
                    reviewer: Reviewer::NoMistakes,
                    coverage: Coverage::Diff,
                    findings_high: 0,
                    findings_medium: 1,
                    findings_low: 0,
                    verdict: Verdict::AskUser,
                    source_url: Some("https://example.com/runs/1".into()),
                    head_sha: "a1".repeat(20),
                },
            )
            .unwrap();
    };
    review(&mut w, &run.id);
    // The owner cancels the card before any pass reads the finding.
    w.store_mut()
        .card_cancel(&owner(), &run.card_id, "wrong approach")
        .unwrap();
    w.store_mut()
        .card_reopen(&owner(), &run.card_id, "try again")
        .unwrap();
    let report = w.pass(now());
    assert!(!report.did("G33"), "{report:?}");
    assert!(
        w.open_items(&run.card_id)
            .iter()
            .all(|item| item.kind != AttentionKind::Decision),
        "{:?}",
        w.open_items(&run.card_id)
    );

    let rework = start_run(
        &mut w,
        &run.card_id,
        "/home/user/worktrees/rework",
        None,
        None,
    );
    review(&mut w, &rework.run.id);
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("G33"), "{report:?}");
    let decisions: Vec<_> = w
        .open_items(&run.card_id)
        .into_iter()
        .filter(|item| item.kind == AttentionKind::Decision)
        .collect();
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(
        decisions[0].opened_by_run_id.as_deref(),
        Some(rework.run.id.as_str())
    );
}
