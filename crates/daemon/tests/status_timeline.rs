//! What the card timeline says about a worker's own report (`tiller status`):
//! the line names the worker, says the state and quotes the words, so the
//! owner reads "blocked" and why without opening the run.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use tiller_core::domain::{Run, StatusState};
use tiller_core::state::get;
use tiller_core::{Actor, Ctx};
use tiller_daemon::views::timeline;
use tiller_panes::testing::Fleet;

#[test]
fn a_workers_report_names_the_worker_and_quotes_its_line() {
    let mut fleet = Fleet::new();
    fleet.worker("w2:p1");
    let (session, run, _) = fleet.workers[0].clone();
    let card = get::<Run>(fleet.store.conn(), "run", &run).unwrap().card_id;
    fleet
        .store
        .run_status(
            &Ctx::new(Actor::Session(session)),
            &run,
            StatusState::NeedsDecision,
            "The staging mail server refuses the test login",
        )
        .unwrap();

    let entries = timeline(&fleet.store, &card, None).unwrap().entries;
    let report = entries
        .iter()
        .find(|entry| entry.text.starts_with("reported"))
        .expect("the report is on the timeline");
    assert_eq!(report.actor, "worker");
    assert_eq!(report.text, "reported needs decision");
    assert_eq!(
        report.quote.as_deref(),
        Some("The staging mail server refuses the test login")
    );
    // The orchestrator's own lines keep its name.
    assert!(
        entries.iter().any(|entry| entry.actor == "orchestrator"),
        "{entries:?}"
    );
}
