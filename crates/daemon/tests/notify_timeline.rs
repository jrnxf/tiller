//! What the card timeline says about an owner notification (B6): the real
//! notifier, a fake herdr that refuses the send, and the daemon's timeline
//! read. A failed send must not read as a delivered notification.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use tiller_core::domain::{AttentionKind, Run};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::get;
use tiller_daemon::views::timeline;
use tiller_delivery::notify::Notifier;
use tiller_herdr::fake::FakeHerdr;
use tiller_herdr::testing::{Fleet, at};

struct World {
    fleet: Fleet,
    herdr: FakeHerdr,
    notifier: Notifier<FakeHerdr>,
    card: String,
}

impl World {
    fn new() -> Self {
        let mut fleet = Fleet::new();
        fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let run = fleet.workers[0].1.clone();
        let card = get::<Run>(fleet.store.conn(), "run", &run).unwrap().card_id;
        Self {
            notifier: Notifier::new(fleet.open(), herdr.clone()),
            fleet,
            herdr,
            card,
        }
    }

    fn open_decision(&mut self) {
        let ctx = tiller_core::Ctx::at(tiller_core::Actor::Owner, at(2));
        self.fleet
            .store
            .attention_open(
                &ctx,
                Some("q"),
                NewAttention {
                    card: self.card.clone(),
                    kind: AttentionKind::Decision,
                    question: "Raise capacity?".to_owned(),
                    ..NewAttention::default()
                },
            )
            .unwrap();
    }

    /// The notification lines of the card timeline, newest first.
    fn lines(&self) -> Vec<String> {
        timeline(&self.fleet.store, &self.card, None)
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.text)
            .filter(|text| text.contains("notif"))
            .collect()
    }
}

#[test]
fn a_notification_herdr_refused_does_not_read_as_sent() {
    let mut w = World::new();
    w.herdr.refuse("notify");
    w.open_decision();
    let report = w.notifier.pass(at(4)).unwrap();
    assert_eq!(report.failed.len(), 1, "{report:?}");
    assert!(report.notified.is_empty());
    assert_eq!(
        w.lines(),
        vec!["could not notify you about an item".to_owned()]
    );
}

#[test]
fn a_notification_herdr_took_reads_as_sent() {
    let mut w = World::new();
    w.open_decision();
    assert_eq!(w.notifier.pass(at(4)).unwrap().notified.len(), 1);
    assert_eq!(w.lines(), vec!["notified you about an item".to_owned()]);
}

/// A crash between the write-ahead record and the send leaves no outcome.
#[test]
fn a_record_with_no_outcome_reads_as_tried() {
    let mut w = World::new();
    w.open_decision();
    let item: String = w
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT entity_id FROM events WHERE kind = 'attention.opened'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let ctx = tiller_core::Ctx::at(tiller_core::Actor::Daemon, at(4));
    w.fleet.store.notification_record(&ctx, &item).unwrap();
    assert_eq!(
        w.lines(),
        vec!["tried to notify you about an item".to_owned()]
    );
}
