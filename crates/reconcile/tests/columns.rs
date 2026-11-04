//! Board column derivation (R13, `state.md` section 6).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{Checks, Column, RunOutcome, StatusState};
use tiller_core::state::runs::EndRun;
use tiller_reconcile::columns;
use tiller_reconcile::thresholds::Thresholds;

/// R13 (P08): a builder's word is never enough. "done" with no PR leaves
/// the card in Working, `run.end landed` is refused, and the orchestrator
/// is told; an open PR with pending checks is Landing; green checks with
/// no approval is Needs you only once the merge item exists.
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

    // Checks green, not merged: Working again until an approval exists, and
    // a fresh "done" is answered with the record as it stands now.
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
    assert_eq!(
        columns::column(w.store(), &run.card_id).unwrap().column,
        Column::Working
    );
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
    assert_eq!(columns::filter(Column::NeedsYou), "needs");
    assert_eq!(columns::filter(Column::Working), "active");
}
