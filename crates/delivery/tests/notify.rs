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
