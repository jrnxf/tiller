//! The weekly review due date of a pre-launch project
//! (`docs/design/pre-launch-validation.md` section 5): an ordinary `date`
//! obligation that the timer meets at the Saturday slot, once, with a note to
//! the project's live orchestrator.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::{ObligationState, Stage};
use tiller_core::state::review::{REVIEW_WHAT, due_reviews, open_weekly};

#[test]
fn weekly_review_due_notes_orchestrator() {
    let mut w = World::new();
    w.place();
    let staged_at: Timestamp = "2026-11-03T14:00:00Z".parse().unwrap();
    let rev = w.store_mut().project("alpha").unwrap().rev;
    w.store_mut()
        .project_stage(
            &tiller_core::Ctx::at(tiller_core::Actor::Owner, staged_at),
            "alpha",
            rev,
            Stage::PreLaunch,
            Some("1111111111111111111111111111111111111111"),
        )
        .unwrap();
    let weekly = open_weekly(w.store().conn(), "alpha").unwrap().unwrap();
    let slot: Timestamp = "2026-11-07T06:00:00Z".parse().unwrap();
    assert_eq!(weekly.due_at, Some(slot));
    let notes = |w: &World| {
        w.count(&format!(
            "SELECT count(*) FROM outbox WHERE target_session_id = '{}' AND body = 'obligation met: {REVIEW_WHAT}'",
            w.fleet.orchestrator_session
        ))
    };

    // A minute before the slot nothing fires.
    w.pass(slot.plus_minutes(-1));
    assert_eq!(notes(&w), 0);
    assert!(
        due_reviews(w.store().conn(), Some("alpha"), slot)
            .unwrap()
            .is_empty()
    );

    // At the slot the timer meets it and notes the orchestrator, once.
    let report = w.pass(slot);
    assert!(report.did("timer: obligation"), "{report:?}");
    let met: tiller_core::domain::Obligation =
        tiller_core::state::get(w.store().conn(), "obligation", &weekly.id).unwrap();
    assert_eq!(met.state, ObligationState::Met);
    assert_eq!(notes(&w), 1);
    w.pass(slot.plus_minutes(5));
    assert_eq!(notes(&w), 1, "fired once");

    // Until a pass starts from it, `tiller review due` lists it.
    let due = due_reviews(w.store().conn(), Some("alpha"), slot.plus_minutes(5)).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].obligation.id, weekly.id);
    assert!(!due[0].overdue);
    let due = due_reviews(w.store().conn(), Some("alpha"), slot.plus_minutes(24 * 60)).unwrap();
    assert!(due[0].overdue);
}
