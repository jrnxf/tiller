//! R14: a run cut by the harness.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{AttentionKind, RunOutcome, StatusState};
use tiller_reconcile::sources::Cut;

/// R14 (P55): the harness's own record ends the run `cut` with what was
/// done and what remains; the worktree stays; the orchestrator is told; the
/// second cut on one card goes to the developer.
#[test]
fn cut_fix_agent_reports_partial_work() {
    let mut w = World::new();
    let worker = w.worker("w2:p1");
    w.place();
    let run = w.run_of(&worker);
    w.store_mut()
        .run_status(
            &session_ctx(&worker),
            &run.id,
            StatusState::Working,
            "fixing the lint errors",
        )
        .unwrap();
    w.harness.cut(
        &run.id,
        Cut {
            reason: "no-mistakes fix agent reached its 30 minute limit".into(),
            done: Some("3 of 5 lint errors fixed".into()),
            remains: Some("2 lint errors in board/src".into()),
        },
    );
    let report = w.pass(now());
    assert!(report.did("R14"), "{report:?}");
    let ended = w.run(&run.id);
    assert_eq!(ended.outcome, Some(RunOutcome::Cut));
    let note = ended.outcome_note.unwrap();
    assert!(
        note.contains("3 of 5 lint errors fixed") && note.contains("2 lint errors in board/src"),
        "{note}"
    );
    assert!(note.contains("fixing the lint errors"), "{note}");
    let path = run.worktree_path.clone().unwrap();
    assert_eq!(
        w.count(&format!(
            "SELECT count(*) FROM worktrees WHERE path = '{path}' AND gone_at IS NULL"
        )),
        1,
        "partial work stays"
    );
    assert!(
        w.notes()
            .iter()
            .any(|n| n.body.contains("was cut by the harness")),
        "{:?}",
        w.notes()
    );
    assert!(
        w.open_items(&run.card_id).is_empty(),
        "the first cut is the orchestrator's"
    );

    // A second attempt, cut again: the developer.
    let orchestrator = w.fleet.orchestrator_session.clone();
    let relaunched = w
        .store_mut()
        .run_relaunch(
            &session_ctx(&orchestrator),
            &run.id,
            tiller_core::state::runs::Relaunch {
                note: "finish the lint".into(),
                new_attempt: true,
                stop_proof: Some("cut".into()),
            },
        )
        .unwrap()
        .value;
    w.harness.cut(
        &relaunched.run.id,
        Cut {
            reason: "no-mistakes fix agent reached its 30 minute limit".into(),
            done: None,
            remains: None,
        },
    );
    w.pass(now().plus_minutes(1));
    let items = w.open_items(&run.card_id);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(
        items[0].question.contains("cut 2 times"),
        "{}",
        items[0].question
    );
}

/// R14: only the harness's record makes a cut. A dead or silent worker is
/// R03 or R12, never `cut`.
#[test]
fn cut_is_recorded_only_from_harness_evidence() {
    let mut w = World::new();
    let dead = w.worker("w2:p1");
    let silent = w.worker("w3:p1");
    w.place();
    w.observe(&silent, tiller_core::domain::Activity::Working, ago(45));
    let t0 = now();
    w.pass(t0);
    w.herdr.remove_pane("w2:p1");
    w.pass(t0.plus_minutes(1));
    for session in [&dead, &silent] {
        let run = w.run_of(session);
        assert!(run.ended_at.is_none(), "{run:?}");
        assert_ne!(run.outcome, Some(RunOutcome::Cut));
    }
    assert_eq!(
        w.count("SELECT count(*) FROM runs WHERE outcome = 'cut'"),
        0
    );
}
