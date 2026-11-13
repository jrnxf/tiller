//! R02 (P21): one owner per run after a restart.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use tiller_core::domain::Activity;
use tiller_core::state::runs::NewOrchestrator;

/// Every active run is supervised by the project's one live orchestrator:
/// after a resume of the same session, and after a fresh session replaces a
/// dead one. Never two owners, never none while a successor exists.
#[test]
fn one_owner_per_run_after_restart() {
    let mut w = World::new();
    let a = w.worker("w2:p1");
    let b = w.worker("w3:p1");
    w.place();
    let first = w.fleet.orchestrator_session.clone();
    let t0 = now();
    w.pass(t0);

    // Resume with the same id: the owner is unchanged.
    w.remove_pane("w1:p1");
    w.pass(t0.plus_minutes(1));
    w.observe(&first, Activity::Idle, t0.plus_minutes(2));
    w.restart();
    w.pass(t0.plus_minutes(3));
    for worker in [&a, &b] {
        assert_eq!(w.run_of(worker).supervisor_session_id, first);
    }

    // The orchestrator dies for good and a fresh session starts: the runs
    // are reowned in the pass after it is live.
    w.store_mut()
        .session_end(&daemon(), &first, "crash: pane gone")
        .unwrap();
    let second = w
        .store_mut()
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "alpha".into(),
                pane_id: Some("w7:p1".into()),
                claude_session_id: Some("claude-orchestrator-2".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .value
        .session
        .id;
    w.add_pane("w7:p1", &second, Meta::new(), Screen::Idle);
    let report = w.pass(t0.plus_minutes(4));
    assert!(report.did("reowned"), "{report:?}");
    for worker in [&a, &b] {
        assert_eq!(w.run_of(worker).supervisor_session_id, second);
    }
    assert_eq!(
        w.count("SELECT count(*) FROM sessions WHERE role = 'orchestrator' AND project_id = 'alpha' AND ended_at IS NULL"),
        1
    );
    let again = w.pass(t0.plus_minutes(5));
    assert!(!again.did("reowned"), "{again:?}");
}
