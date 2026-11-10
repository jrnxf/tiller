//! Board column derivation (R13, `state.md` section 6).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{
    AnsweredVia, ApprovalAction, ApprovalKind, AttentionKind, CardKind, CardState, Checks, Column,
    Delivery, Effort, PipelineState, PrState, RunOutcome, RunRole, StatusState,
};
use tiller_core::state::approvals::{Decision, NewApproval, NewPr};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::NewCard;
use tiller_core::state::projects::NewProject;
use tiller_core::state::runs::{EndRun, NewOrchestrator, NewRun, SpawnChecks};
use tiller_reconcile::columns;
use tiller_reconcile::recovery::approvals::LANDED_BY_ME;
use tiller_reconcile::thresholds::Thresholds;

/// R13 (P08): a builder's word is never enough. "done" with no PR leaves
/// the card in Working, `run.end landed` is refused, and the orchestrator
/// is told; an open PR with pending checks is Landing, and the card stays
/// there once the checks are green, though the builder is still at work.
#[test]
fn ship_card_stays_working_without_pr() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    w.store_mut()
        .run_status(
            &session_ctx(&worker),
            &run.id,
            StatusState::Done,
            "all tests pass, pushed",
        )
        .unwrap();
    let refused = w.store_mut().run_end(
        &session_ctx(&worker),
        &run.id,
        EndRun {
            outcome: RunOutcome::Landed,
            note: None,
        },
    );
    assert!(refused.is_err(), "landed without a merge: {refused:?}");
    assert_eq!(
        columns::column(w.store(), &run.card_id).unwrap().column,
        Column::Working
    );

    let report = w.pass(now());
    assert!(report.did("R13"), "{report:?}");
    assert!(
        w.notes().iter().any(|n| n.body.contains("no PR is linked")),
        "{:?}",
        w.notes()
    );
    assert_eq!(
        columns::column(w.store(), &run.card_id).unwrap().column,
        Column::Working
    );
    assert!(w.run(&run.id).ended_at.is_none());

    // A PR with checks pending: Landing, badge checks.
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let mut pending = open_pr("a1a1a1a1");
    pending.fetch.checks = Checks::Pending;
    w.forge.pr(REPO, 7, pending);
    w.pass(now().plus_minutes(1));
    let column = columns::column(w.store(), &run.card_id).unwrap();
    assert_eq!(column.column, Column::Landing);
    assert_eq!(column.badge, "checks");

    // Checks green, not merged: still Landing while the builder works (a
    // card never moves left), and a fresh "done" is answered with the
    // record as it stands now.
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.store_mut()
        .run_status(
            &session_ctx(&worker),
            &run.id,
            StatusState::Done,
            "PR is up, done",
        )
        .unwrap();
    w.pass(now().plus_minutes(2));
    let column = columns::column(w.store(), &run.card_id).unwrap();
    assert_eq!(column.column, Column::Landing);
    assert_ne!(column.badge, "checks");
    assert!(
        w.notes()
            .iter()
            .any(|n| n.body.contains("PR #7 is open, not merged")),
        "{:?}",
        w.notes()
    );
    assert!(
        w.run(&run.id).ended_at.is_none(),
        "the card never reaches Done on a word"
    );
    let _ = pr;
}

/// P08 (R13): the pipeline is part of the evidence. A ship card whose PR is
/// open but whose pipeline never ran (`pipeline_state` none) is not done on
/// the worker's word: the pass leaves the orchestrator one note that names
/// the missing pipeline, the card stays in Landing, and the run stays open.
#[test]
fn a_done_report_with_an_open_pr_and_no_pipeline_notes_the_orchestrator() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let mut read = open_pr("a1a1a1a1");
    read.fetch.pipeline_state = PipelineState::None;
    w.forge.pr(REPO, 7, read);
    w.store_mut()
        .run_status(
            &session_ctx(&worker),
            &run.id,
            StatusState::Done,
            "PR is up, done",
        )
        .unwrap();

    let t0 = now();
    let report = w.pass(t0);
    assert_eq!(w.pr(&pr).pipeline_state, PipelineState::None);
    assert!(report.did("R13"), "{report:?}");
    let notes: Vec<_> = w
        .notes()
        .into_iter()
        .filter(|n| n.body.contains("reports \"done"))
        .collect();
    assert_eq!(notes.len(), 1, "{:?}", w.notes());
    assert!(
        notes[0].body.contains("PR #7 is open, not merged")
            && notes[0].body.contains("pipeline none"),
        "{}",
        notes[0].body
    );
    assert!(
        notes[0].body.contains("Steer the worker to the pipeline"),
        "{}",
        notes[0].body
    );
    assert_eq!(
        columns::column(w.store(), &run.card_id).unwrap().column,
        Column::Landing
    );
    assert!(w.run(&run.id).ended_at.is_none());

    // The same word is not noted again.
    let again = w.pass(t0.plus_minutes(1));
    assert!(!again.did("R13"), "{again:?}");
    assert_eq!(
        w.notes()
            .iter()
            .filter(|n| n.body.contains("reports \"done"))
            .count(),
        1
    );
}

/// R13 waits for the daemon on a direct-pr run: a described PR the daemon
/// has not opened yet is the expected state after "done", not a claim
/// without evidence. Once the daemon records the PR, the usual note runs.
#[test]
fn a_direct_pr_run_waiting_on_its_publish_gets_no_note() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    w.store_mut()
        .pr_describe(
            &session_ctx(&worker),
            &run.id,
            "Add a greeting",
            "One line.",
        )
        .unwrap();
    w.store_mut()
        .run_status(
            &session_ctx(&worker),
            &run.id,
            StatusState::Done,
            "ready in branch fm/work",
        )
        .unwrap();
    let report = w.pass(now());
    assert!(!report.did("R13"), "{report:?}");
    assert!(w.notes().is_empty(), "{:?}", w.notes());

    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    w.store_mut()
        .publish_done(&daemon(), &run.id, "a1a1a1a1", &pr)
        .unwrap();
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    let report = w.pass(now().plus_minutes(1));
    assert!(report.did("R13"), "{report:?}");
    assert!(
        w.notes()
            .iter()
            .any(|n| n.body.contains("PR #7 is open, not merged")),
        "{:?}",
        w.notes()
    );
}

/// R13 after a reopen: the merge that closed the card the first time is not
/// evidence for the run that reworks it. A "done" from the new run with no
/// merge since the reopen gets the note, though the old PR is still
/// recorded as merged and the old "Mark landed" answer still stands.
#[test]
fn a_done_report_after_a_reopen_needs_a_merge_since_the_reopen() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "a1a1a1a1", false, Some(1));
    let t0 = now();
    w.forge.pr(REPO, 7, merged_pr("a1a1a1a1", "e1e1e1e1", t0));
    w.pass(t0);
    assert_eq!(w.pr(&pr).state, PrState::Merged);
    let items = w.open_items(&run.card_id);
    let recovery = items
        .iter()
        .find(|item| item.kind == AttentionKind::Recovery)
        .expect("R07 asks about the merge");
    w.store_mut()
        .attention_answer(
            &owner(),
            None,
            &recovery.id,
            LANDED_BY_ME,
            AnsweredVia::Board,
        )
        .unwrap();
    w.pass(t0.plus_minutes(1));
    assert_eq!(w.card(&run.card_id).state, CardState::Closed);

    w.store_mut()
        .card_reopen(&owner(), &run.card_id, "the fix regressed")
        .unwrap();
    let rework = w
        .store_mut()
        .run_start(
            &owner(),
            None,
            NewRun {
                card: run.card_id.clone(),
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: Some("/home/user/worktrees/rework".into()),
                branch: None,
                base_branch: None,
                base_sha: None,
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value;
    w.store_mut()
        .run_status(
            &session_ctx(&rework.session.id),
            &rework.run.id,
            StatusState::Done,
            "fixed again",
        )
        .unwrap();
    let report = w.pass(t0.plus_minutes(2));
    assert!(report.did("R13"), "{report:?}");
    assert!(
        w.notes().iter().any(|n| n.body.contains(&rework.run.id)
            && n.body.contains("since the card was last reopened")),
        "{:?}",
        w.notes()
    );

    // The rework's own PR merges: that is evidence, and a fresh "done"
    // gets no note.
    w.store_mut()
        .pr_link(
            &owner(),
            NewPr {
                card: run.card_id.clone(),
                run_id: Some(rework.run.id.clone()),
                number: 8,
                url: format!("https://github.com/{REPO}/pull/8"),
                head_sha: Some("b2b2b2b2".into()),
                base_ref: Some("main".into()),
            },
        )
        .unwrap();
    w.forge.pr(REPO, 8, open_pr("b2b2b2b2"));
    w.pass(t0.plus_minutes(3));
    w.forge.pr(
        REPO,
        8,
        merged_pr("b2b2b2b2", "f2f2f2f2", t0.plus_minutes(4)),
    );
    w.pass(t0.plus_minutes(4));
    w.store_mut()
        .run_status(
            &session_ctx(&rework.session.id),
            &rework.run.id,
            StatusState::Done,
            "merged, done",
        )
        .unwrap();
    let report = w.pass(t0.plus_minutes(5));
    assert!(w.run(&rework.run.id).ended_at.is_none());
    assert!(!report.did("R13"), "{report:?}");
}

/// The board shape: every column present, cards in theirs, asks separate,
/// the thresholds the client ages with.
#[test]
fn board_groups_every_column_and_carries_thresholds() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let board = columns::board(w.store(), None, &Thresholds::default(), now()).unwrap();
    assert_eq!(board.columns.len(), Column::ALL.len());
    assert!(
        board.columns[&Column::Working]
            .iter()
            .any(|c| c.id == run.card_id)
    );
    assert!(
        board.columns[&Column::Queued]
            .iter()
            .any(|c| c.display_id == "fleet-1")
    );
    assert!(board.asked.is_empty());
    assert_eq!(board.thresholds.worker_stale_minutes, 15);
    assert_eq!(board.thresholds.orchestrator_stale_minutes, 30);
    let alpha = columns::board(w.store(), Some("alpha"), &Thresholds::default(), now()).unwrap();
    assert!(
        alpha.columns[&Column::Queued]
            .iter()
            .all(|c| c.project_id == "alpha")
    );
    let working = board.columns[&Column::Working]
        .iter()
        .find(|c| c.id == run.card_id)
        .unwrap();
    assert_eq!(columns::filter(working), "active");
}

/// A card moves forward only. A gated ship card whose builder ended
/// `reported` stays in Landing while its PR waits for checks to report and
/// then for the merge approval to be asked for; a card whose run ended with
/// no PR stays in Working until its next run, and a hold put on it then
/// shows in its badge. The owner rejecting the merge approval leaves the
/// card in Landing, sent back, and the run that reworks it keeps it there.
#[test]
fn a_card_never_steps_back_between_steps() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    let relaunched = w.worker("w3:p1");
    w.place();
    let run = w.run_of(&worker);
    let column = |w: &World, card: &str| {
        let row = columns::column(w.store(), card).unwrap();
        (row.column, row.badge)
    };
    let end = |w: &mut World, run: &str, outcome: RunOutcome| {
        w.store_mut()
            .run_end(
                &daemon(),
                run,
                EndRun {
                    outcome,
                    note: None,
                },
            )
            .unwrap();
    };

    // The builder opens its PR and exits before any check reports.
    let pr = w.link_pr(&worker, 7, "a1a1a1a1");
    let path = run.worktree_path.clone().unwrap();
    w.scanner
        .worktree(&path, "fm/work", "a1a1a1a1", false, Some(1));
    let mut unchecked = open_pr("a1a1a1a1");
    unchecked.fetch.checks = Checks::None;
    w.forge.pr(REPO, 7, unchecked);
    w.pass(now());
    end(&mut w, &run.id, RunOutcome::Reported);
    assert_eq!(column(&w, &run.card_id).0, Column::Landing);

    let mut pending = open_pr("a1a1a1a1");
    pending.fetch.checks = Checks::Pending;
    w.forge.pr(REPO, 7, pending);
    w.pass(now().plus_minutes(1));
    assert_eq!(column(&w, &run.card_id), (Column::Landing, "checks".into()));

    // Checks green, no approval asked for yet.
    w.forge.pr(REPO, 7, open_pr("a1a1a1a1"));
    w.pass(now().plus_minutes(2));
    assert_eq!(column(&w, &run.card_id), (Column::Landing, "ready".into()));

    // A run that ends with no PR, before the next one starts.
    let other = w.run_of(&relaunched);
    end(&mut w, &other.id, RunOutcome::Relaunched);
    assert_eq!(
        column(&w, &other.card_id),
        (Column::Working, "between_runs".into())
    );
    w.store_mut()
        .card_hold(
            &owner(),
            &other.card_id,
            now().plus_minutes(60),
            "waiting on a vendor",
        )
        .unwrap();
    assert_eq!(
        column(&w, &other.card_id),
        (Column::Working, "parked".into())
    );

    // Asking for the merge keeps the card in Landing: it waits on the
    // owner there, so it never moves left.
    let approval = w.approve(&worker, &pr, "a1a1a1a1", false);
    let asked = columns::column(w.store(), &run.card_id).unwrap();
    assert_eq!(
        (asked.column, asked.badge.as_str(), asked.needs_you),
        (Column::Landing, "merge", true)
    );
    assert_eq!(columns::filter(&asked), "needs");
    w.store_mut()
        .approval_decide(
            &owner(),
            None,
            &approval,
            Decision {
                accept: false,
                via: AnsweredVia::Board,
                memory: Vec::new(),
                ask_id: None,
            },
        )
        .unwrap();
    let back = columns::column(w.store(), &run.card_id).unwrap();
    assert_eq!(
        (back.column, back.badge.as_str(), back.needs_you),
        (Column::Landing, "sent_back", false)
    );

    // The rework run starts: the card stays in Landing.
    w.store_mut()
        .run_start(
            &owner(),
            None,
            NewRun {
                card: run.card_id.clone(),
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: Some("/home/user/worktrees/rework".into()),
                branch: None,
                base_branch: None,
                base_sha: None,
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap();
    assert_eq!(column(&w, &run.card_id).0, Column::Landing);
}

/// A card that waits on the owner stays in the column its work has reached:
/// a decision asked mid-run is Working, flagged for the Needs you filter,
/// and answering it clears the flag without moving the card.
#[test]
fn a_card_that_needs_you_stays_in_its_column() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    let item = w
        .store_mut()
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: run.card_id.clone(),
                kind: AttentionKind::Decision,
                question: "Retry on 502 or fail fast?".into(),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value;
    let waiting = columns::column(w.store(), &run.card_id).unwrap();
    assert_eq!(
        (waiting.column, waiting.badge.as_str(), waiting.needs_you),
        (Column::Working, "decision", true)
    );

    w.store_mut()
        .attention_answer(&owner(), None, &item.id, "retry", AnsweredVia::Board)
        .unwrap();
    let answered = columns::column(w.store(), &run.card_id).unwrap();
    assert_eq!(
        (answered.column, answered.needs_you),
        (Column::Working, false)
    );
}

/// A direct-push card has no PR, and its builder stays live until the push.
/// While its `land_direct` approval waits, the card waits on the owner in
/// Landing, like a ship card waiting on its merge, not in Working.
#[test]
fn a_direct_land_waits_in_landing() {
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
                kind: CardKind::Ship,
                title: "direct".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let run = w
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
        .value
        .run;
    w.store_mut()
        .approval_request(
            &session_ctx(&orchestrator),
            NewApproval {
                kind: ApprovalKind::LandDirect,
                card: card.id.clone(),
                run_id: Some(run.id.clone()),
                pr_id: None,
                branch: Some("fm/beta-1".into()),
                head_sha: "a1a1a1a1".into(),
                action: ApprovalAction::Push,
                memory_paths: None,
                grant_id: None,
            },
        )
        .unwrap();
    let waiting = columns::column(w.store(), &card.id).unwrap();
    assert_eq!(
        (waiting.column, waiting.badge.as_str(), waiting.needs_you),
        (Column::Landing, "merge", true)
    );
}
