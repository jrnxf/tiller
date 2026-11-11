//! Owner notifications (`protocol.md` 3.4, P36): a file-backed `state.db`, the
//! real notifier and a fake herdr that records what it was asked to show.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use tiller_core::domain::{Attention, AttentionKind};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::list;
use tiller_delivery::notify::{NEW_MINUTES, Notifier};
use tiller_herdr::fake::FakeHerdr;
use tiller_herdr::testing::{Fleet, at, owner};

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
        let card = fleet.workers[0].1.clone();
        let card: String =
            tiller_core::state::get::<tiller_core::domain::Run>(fleet.store.conn(), "run", &card)
                .unwrap()
                .card_id;
        Self {
            notifier: Notifier::new(fleet.open(), herdr.clone()),
            fleet,
            herdr,
            card,
        }
    }

    fn open(&mut self, kind: AttentionKind, question: &str, now: i64) -> Attention {
        let ctx = tiller_core::Ctx::at(tiller_core::Actor::Owner, at(now));
        self.fleet
            .store
            .attention_open(
                &ctx,
                Some(question),
                NewAttention {
                    card: self.card.clone(),
                    kind,
                    question: question.to_owned(),
                    ..NewAttention::default()
                },
            )
            .unwrap()
            .value
    }

    fn shown(&self) -> Vec<String> {
        self.herdr
            .calls()
            .into_iter()
            .filter(|call| call.starts_with("notify"))
            .collect()
    }
}

#[test]
fn new_attention_item_notifies_once() {
    let mut w = World::new();
    assert!(w.notifier.pass(at(1)).unwrap().notified.is_empty());
    let decision = w.open(AttentionKind::Decision, "Raise capacity?", 2);
    let merge = w.open(AttentionKind::Merge, "PR 7 is ready", 3);

    let report = w.notifier.pass(at(4)).unwrap();
    assert_eq!(report.notified, vec![decision.id.clone(), merge.id.clone()]);
    assert_eq!(w.shown().len(), 2);
    assert!(w.shown()[0].contains("needs a decision"), "{:?}", w.shown());
    assert!(w.shown()[1].contains("ready for your merge"));

    // A second pass, and a fresh notifier as after a daemon restart, stay quiet.
    assert!(w.notifier.pass(at(5)).unwrap().notified.is_empty());
    let mut restarted = Notifier::new(w.fleet.open(), w.herdr.clone());
    assert!(restarted.pass(at(6)).unwrap().notified.is_empty());
    assert_eq!(w.shown().len(), 2);

    // Each notification is recorded in an event on its item.
    let events: i64 = w
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'attention.notified'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(events, 2);
}

#[test]
fn routine_turns_and_settled_items_never_notify() {
    let mut w = World::new();
    // Nothing opened: nothing to say.
    assert!(w.notifier.pass(at(2)).unwrap().notified.is_empty());

    // An item answered before the pass needs nobody.
    let answered = w.open(AttentionKind::Decision, "Already answered?", 3);
    w.fleet
        .store
        .attention_withdraw(&owner(), &answered.id, "no longer applies")
        .unwrap();
    assert!(w.notifier.pass(at(4)).unwrap().notified.is_empty());

    // An item opened long ago is backlog, not news.
    let old = w.open(AttentionKind::Decision, "Old question?", 5);
    let later = 5 + NEW_MINUTES * 60 + 1;
    assert!(w.notifier.pass(at(later)).unwrap().notified.is_empty());
    let open: Vec<Attention> = list(
        w.fleet.store.conn(),
        "answered_at IS NULL AND withdrawn_at IS NULL",
        &[],
    )
    .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, old.id);
    assert!(w.shown().is_empty());
}

fn kinds(w: &World, item: &str) -> Vec<String> {
    let mut stmt = w
        .fleet
        .store
        .conn()
        .prepare("SELECT kind FROM events WHERE entity_id = ?1 ORDER BY seq")
        .unwrap();
    stmt.query_map([item], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_refused_send_is_recorded_once_and_never_retried() {
    let mut w = World::new();
    w.herdr.refuse("notify");
    let item = w.open(AttentionKind::Decision, "Raise capacity?", 2);

    let report = w.notifier.pass(at(4)).unwrap();
    assert!(report.notified.is_empty());
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, item.id);
    assert_eq!(
        kinds(&w, &item.id),
        [
            "attention.opened",
            "attention.notified",
            "attention.notify_failed"
        ]
    );
    let error: String = w
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT json_extract(payload, '$.error') FROM events WHERE kind = 'attention.notify_failed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(error.contains("refused by the fake"), "{error}");

    // At most once: a second pass and a restart neither send nor record again.
    let mut restarted = Notifier::new(w.fleet.open(), w.herdr.clone());
    assert!(w.notifier.pass(at(5)).unwrap().failed.is_empty());
    assert!(restarted.pass(at(6)).unwrap().failed.is_empty());
    assert_eq!(kinds(&w, &item.id).len(), 3);
}

#[test]
fn a_sent_notification_writes_its_outcome_once() {
    let mut w = World::new();
    let item = w.open(AttentionKind::Decision, "Raise capacity?", 2);
    assert_eq!(
        w.notifier.pass(at(4)).unwrap().notified,
        std::slice::from_ref(&item.id)
    );
    assert_eq!(
        kinds(&w, &item.id),
        [
            "attention.opened",
            "attention.notified",
            "attention.notify_sent"
        ]
    );

    // At most once: a second pass and a restart write nothing more, and a
    // late outcome call applies nothing on an item that has its outcome.
    let mut restarted = Notifier::new(w.fleet.open(), w.herdr.clone());
    assert!(w.notifier.pass(at(5)).unwrap().notified.is_empty());
    assert!(restarted.pass(at(6)).unwrap().notified.is_empty());
    let ctx = tiller_core::Ctx::at(tiller_core::Actor::Daemon, at(7));
    assert!(
        w.fleet
            .store
            .notification_sent(&ctx, &item.id)
            .unwrap()
            .replayed
    );
    assert!(
        w.fleet
            .store
            .notification_fail(&ctx, &item.id, "late")
            .unwrap()
            .replayed
    );
    assert_eq!(kinds(&w, &item.id).len(), 3);
}

#[test]
fn a_record_alone_takes_one_outcome_and_only_one() {
    let mut w = World::new();
    let item = w.open(AttentionKind::Decision, "Raise capacity?", 2);
    let ctx = tiller_core::Ctx::at(tiller_core::Actor::Daemon, at(4));
    // No record yet: an outcome applies nothing.
    assert!(
        w.fleet
            .store
            .notification_sent(&ctx, &item.id)
            .unwrap()
            .replayed
    );
    w.fleet.store.notification_record(&ctx, &item.id).unwrap();
    assert!(
        !w.fleet
            .store
            .notification_sent(&ctx, &item.id)
            .unwrap()
            .replayed
    );
    assert!(
        w.fleet
            .store
            .notification_fail(&ctx, &item.id, "late")
            .unwrap()
            .replayed
    );
    assert_eq!(
        kinds(&w, &item.id),
        [
            "attention.opened",
            "attention.notified",
            "attention.notify_sent"
        ]
    );
}
