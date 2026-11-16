//! What the card timeline says about a worker's own report (`tiller status`):
//! the line names the worker, says the state and quotes the words, so the
//! owner reads "blocked" and why without opening the run.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use tiller_core::domain::{AnsweredVia, AttentionKind, Run, StatusState};
use tiller_core::state::attention::NewAttention;
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

/// The default timeline is the owner's news: the question, the answer and a
/// comment in their own words. The steps of the machinery are marked, so the
/// board keeps them out of the way, and none of them is credited to "you".
#[test]
fn the_owners_news_is_quoted_and_the_machinery_is_marked() {
    let mut fleet = Fleet::new();
    fleet.worker("w2:p1");
    let (_, run, _) = fleet.workers[0].clone();
    let card = get::<Run>(fleet.store.conn(), "run", &run).unwrap().card_id;
    let orchestrator = Ctx::new(Actor::Session(fleet.orchestrator_session.clone()));
    let item = fleet
        .store
        .attention_open(
            &orchestrator,
            Some("q"),
            NewAttention {
                card: card.clone(),
                kind: AttentionKind::Decision,
                question: "Rotate with an overlap, or cut over at once?".to_owned(),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value;
    let owner = Ctx::new(Actor::Board);
    fleet
        .store
        .card_comment(&owner, Some("c"), &card, "Prefer the overlap.")
        .unwrap();
    fleet
        .store
        .attention_answer(&owner, Some("a"), &item.id, "Overlap", AnsweredVia::Board)
        .unwrap();

    let entries = timeline(&fleet.store, &card, None).unwrap().entries;
    let line = |text: &str| {
        entries
            .iter()
            .find(|entry| entry.text == text)
            .unwrap_or_else(|| panic!("no line {text:?} in {entries:?}"))
    };

    let asked = line("asked you for a decision");
    assert_eq!(asked.actor, "orchestrator");
    assert_eq!(
        asked.quote.as_deref(),
        Some("Rotate with an overlap, or cut over at once?")
    );
    assert!(!asked.system);

    let answered = line("answered");
    assert_eq!(answered.actor, "you");
    assert_eq!(answered.quote.as_deref(), Some("Overlap"));
    assert!(!answered.system);

    // The comment is the owner's words, without the card tag of its message.
    let comment = line("commented");
    assert_eq!(comment.actor, "you");
    assert_eq!(comment.quote.as_deref(), Some("Prefer the overlap."));
    assert!(!comment.system);

    // Every other queued or delivered message is machinery, and so is the
    // session bookkeeping of the run. None of it reads as the owner's act.
    let system: Vec<_> = entries.iter().filter(|entry| entry.system).collect();
    assert!(
        system.iter().any(|entry| entry.text == "queued a message"),
        "the answer's own message is machinery: {entries:?}"
    );
    assert!(
        system.iter().all(|entry| entry.actor != "you"),
        "{system:?}"
    );
    assert!(
        entries
            .iter()
            .filter(|entry| entry.text.starts_with("started a session")
                || entry.text.starts_with("queued")
                || entry.text.starts_with("delivered"))
            .all(|entry| entry.system),
        "{entries:?}"
    );
}
