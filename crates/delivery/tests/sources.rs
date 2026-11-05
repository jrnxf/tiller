//! Review-page answers reach the worker that owns the page (`protocol.md`
//! 3.5, P47): a file-backed `state.db`, the real source, and a fake review
//! page tool.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tiller_core::domain::{
    Artifact, ArtifactKind, CardOutcome, Event, OutboxKind, OutboxMessage, OutboxState, RunOutcome,
};
use tiller_core::state::records::NewArtifact;
use tiller_core::state::runs::EndRun;
use tiller_core::state::{get, list};
use tiller_delivery::sources::{Entry, Feedback, PageAnswers, Pages};
use tiller_herdr::poller::Source;
use tiller_herdr::testing::{Fleet, at, owner};

const PAGE: &str = "/home/user/pages/mock.html";

/// What each page's queue holds. A read does not consume it, so a second
/// pass sees the same entries again, as a source that keeps its queue would.
#[derive(Clone, Default)]
struct Queues(Arc<Mutex<HashMap<PathBuf, Feedback>>>);

impl Queues {
    fn set(&self, page: &str, feedback: Feedback) {
        self.0.lock().unwrap().insert(PathBuf::from(page), feedback);
    }
}

impl Pages for Queues {
    fn feedback(&mut self, page: &Path) -> Result<Feedback, String> {
        self.0
            .lock()
            .unwrap()
            .get(page)
            .cloned()
            .ok_or_else(|| format!("{} has no session", page.display()))
    }
}

fn entry(id: &str, prompt: &str) -> Entry {
    Entry {
        id: id.into(),
        prompt: prompt.into(),
        selector: "main > h1".into(),
        tag: "h1".into(),
        text: "Pricing".into(),
    }
}

struct World {
    fleet: Fleet,
    queues: Queues,
    answers: PageAnswers<Queues>,
    worker: String,
    run: String,
    card: String,
    page: Artifact,
}

impl World {
    fn new() -> Self {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let run: tiller_core::domain::Run =
            get(fleet.store.conn(), "run", &fleet.workers[0].1).unwrap();
        let page = fleet
            .store
            .artifact_add(
                &owner(),
                NewArtifact {
                    card: run.card_id.clone(),
                    run_id: Some(run.id.clone()),
                    kind: ArtifactKind::Page,
                    title: "Pricing mock".into(),
                    url: None,
                    path: Some(PAGE.into()),
                },
            )
            .unwrap()
            .value;
        let queues = Queues::default();
        let answers = PageAnswers::new(fleet.open(), queues.clone());
        Self {
            queues,
            answers,
            worker,
            run: run.id,
            card: run.card_id,
            page,
            fleet,
        }
    }

    fn pass(&mut self, seconds: i64) -> tiller_delivery::sources::Report {
        let report = self.answers.pass(at(seconds)).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        report
    }

    /// What this source stored; other mutations, such as a run ending,
    /// enqueue messages of their own.
    fn outbox(&self) -> Vec<OutboxMessage> {
        list(
            self.fleet.store.conn(),
            "idempotency_key LIKE 'page:%' ORDER BY created_at, id",
            &[],
        )
        .unwrap()
    }

    fn end_run(&mut self, seconds: i64) {
        self.fleet
            .store
            .session_end(
                &tiller_core::Ctx::at(tiller_core::Actor::Hook, at(seconds)),
                &self.worker,
                "exited",
            )
            .unwrap();
        self.fleet
            .store
            .run_end(
                &owner(),
                &self.run,
                EndRun {
                    outcome: RunOutcome::Abandoned,
                    note: None,
                },
            )
            .unwrap();
    }

    fn feedback_events(&self) -> Vec<Event> {
        list(
            self.fleet.store.conn(),
            "kind = 'artifact.feedback' ORDER BY seq",
            &[],
        )
        .unwrap()
    }
}

#[test]
fn page_answer_reaches_owning_worker() {
    let mut w = World::new();
    w.queues.set(
        PAGE,
        Feedback::Entries(vec![entry("a1", "Make the price bigger")]),
    );
    let report = w.pass(1);
    assert_eq!((report.steered, report.noted, report.recorded), (1, 0, 0));

    let sent = w.outbox();
    assert_eq!(sent.len(), 1);
    let message = &sent[0];
    assert_eq!(message.target_session_id, w.worker);
    assert_eq!(message.kind, OutboxKind::Steer);
    assert_eq!(message.state, OutboxState::Pending);
    assert_eq!(message.card_id.as_deref(), Some(w.card.as_str()));
    assert_eq!(message.created_by, "daemon");
    assert_eq!(
        message.idempotency_key.as_deref(),
        Some(format!("page:{}:a1", w.page.id).as_str())
    );
    assert!(message.body.contains("Make the price bigger"));
    assert!(message.body.contains("main > h1"));
    assert!(message.body.contains("Pricing"));
    assert!(message.body.contains("Pricing mock"));
}

#[test]
fn page_answer_replay_is_not_delivered_twice() {
    let mut w = World::new();
    w.queues.set(
        PAGE,
        Feedback::Entries(vec![entry("a1", "Make the price bigger")]),
    );
    w.pass(1);
    w.pass(2);
    // A restarted daemon builds the source afresh and reads the same entry.
    w.answers = PageAnswers::new(w.fleet.open(), w.queues.clone());
    w.pass(3);
    assert_eq!(w.outbox().len(), 1);

    // A new entry on the same page is a new message.
    w.queues.set(
        PAGE,
        Feedback::Entries(vec![
            entry("a1", "Make the price bigger"),
            entry("a2", "Use the blue one"),
        ]),
    );
    w.pass(4);
    let bodies: Vec<String> = w.outbox().into_iter().map(|m| m.body).collect();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].contains("Use the blue one"));
}

#[test]
fn page_answer_for_an_ended_run_goes_to_the_orchestrator() {
    let mut w = World::new();
    w.end_run(1);
    w.queues
        .set(PAGE, Feedback::Entries(vec![entry("a1", "Still wrong")]));
    let report = w.pass(2);
    assert_eq!((report.steered, report.noted, report.recorded), (0, 1, 0));
    let sent = w.outbox();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].kind, OutboxKind::Note);
    assert_eq!(sent[0].target_session_id, w.fleet.orchestrator_session);
}

#[test]
fn page_answer_on_a_closed_card_is_recorded_and_wakes_nobody() {
    let mut w = World::new();
    w.end_run(1);
    w.fleet
        .store
        .card_close(
            &owner(),
            &w.card,
            CardOutcome::Cancelled,
            Some("not needed"),
            None,
        )
        .unwrap();
    w.queues
        .set(PAGE, Feedback::Entries(vec![entry("a1", "Too late")]));
    let report = w.pass(1);
    assert_eq!((report.steered, report.noted, report.recorded), (0, 0, 1));
    assert!(w.outbox().is_empty());
    let events = w.feedback_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].card_id.as_deref(), Some(w.card.as_str()));
    // Read again, it is not recorded again.
    w.pass(2);
    assert_eq!(w.feedback_events().len(), 1);
}

#[test]
fn retired_and_ended_pages_are_not_read() {
    let mut w = World::new();
    w.queues.set(PAGE, Feedback::Ended);
    w.pass(1);
    w.queues
        .set(PAGE, Feedback::Entries(vec![entry("a1", "After the end")]));
    // The owner ended the page: the source stops reading it.
    assert_eq!(w.pass(2).pages, 0);

    let mut w = World::new();
    w.fleet.store.artifact_retire(&owner(), &w.page.id).unwrap();
    w.queues
        .set(PAGE, Feedback::Entries(vec![entry("a1", "Retired")]));
    assert_eq!(w.pass(1).pages, 0);
    assert!(w.outbox().is_empty());
}

#[test]
fn an_unreadable_page_fails_the_poll_only_when_nothing_was_read() {
    let mut w = World::new();
    // No queue for the page: the fake reports it has no session.
    let report = w.answers.pass(at(1)).unwrap();
    assert_eq!(report.errors.len(), 1);
    assert!(w.answers.poll(at(1)).is_err());
}
