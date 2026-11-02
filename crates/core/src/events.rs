//! `state.md` invariant 3 as a property test: every mutation writes its
//! event (`core::events::every_mutation_writes_its_event`).

use proptest::prelude::*;

use crate::domain::*;
use crate::state::asks::{NewAsk, Resolution};
use crate::state::attention::NewAttention;
use crate::state::cards::CardEdit;
use crate::state::runs::EndRun;
use crate::state::tests::{builder_run, card, memory_store, owner, snapshot};
use crate::store::{Actor, Ctx, Store};
use crate::time::Timestamp;

/// One random mutation against the store, by index.
#[derive(Clone, Debug)]
struct Op {
    which: u8,
    a: u8,
    b: u8,
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(
        (0u8..16, any::<u8>(), any::<u8>()).prop_map(|(which, a, b)| Op { which, a, b }),
        1..40,
    )
}

fn pick<T: Clone>(items: &[T], i: u8) -> Option<T> {
    (!items.is_empty()).then(|| items[i as usize % items.len()].clone())
}

/// Applies `op` and returns the mutation name it ran, or `None` when it had
/// nothing to act on. Refusals are fine; they must write nothing.
fn apply(
    store: &mut Store,
    session: &str,
    op: &Op,
) -> Option<(&'static str, crate::Result<Vec<i64>>)> {
    let cards: Vec<String> = store
        .conn()
        .prepare("SELECT id FROM cards WHERE project_id = 'alpha' ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let items: Vec<String> = store
        .conn()
        .prepare("SELECT id FROM attention ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let runs: Vec<String> = store
        .conn()
        .prepare("SELECT id FROM runs ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let asks: Vec<String> = store
        .conn()
        .prepare("SELECT id FROM asks ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let ctx = owner();
    let kinds = [
        CardKind::Ship,
        CardKind::Scout,
        CardKind::Question,
        CardKind::Ops,
    ];
    macro_rules! run {
        ($txn:literal, $call:expr) => {
            Some(($txn, $call.map(|applied| applied.events)))
        };
    }
    match op.which {
        0 => run!(
            "card.create",
            store.card_create(&ctx, None, card(kinds[op.a as usize % 4], "t"), None)
        ),
        1 => {
            let card = pick(&cards, op.a)?;
            let rev = store.card(&card).unwrap().rev - i64::from(op.b % 2);
            let edit = CardEdit {
                title: Some(format!("title {}", op.b % 3)),
                ..CardEdit::default()
            };
            run!("card.edit", store.card_edit(&ctx, None, &card, rev, edit))
        }
        2 => run!(
            "card.hold",
            store.card_hold(
                &ctx,
                &pick(&cards, op.a)?,
                Timestamp::now().plus_minutes(60),
                "later"
            )
        ),
        3 => run!(
            "hold.release",
            store.hold_release(&ctx, &pick(&cards, op.a)?, HoldReleaseReason::Removed)
        ),
        4 => run!(
            "dep.add",
            store.dep_add(&ctx, &pick(&cards, op.a)?, &pick(&cards, op.b)?)
        ),
        5 => run!(
            "card.close",
            store.card_close(
                &ctx,
                &pick(&cards, op.a)?,
                CardOutcome::Abandoned,
                Some("gave up"),
                None
            )
        ),
        6 => run!(
            "card.reopen",
            store.card_reopen(&ctx, &pick(&cards, op.a)?, "again")
        ),
        7 => run!(
            "card.cancel",
            store.card_cancel(&ctx, &pick(&cards, op.a)?, "no")
        ),
        8 => run!(
            "attention.open",
            store.attention_open(
                &ctx,
                None,
                NewAttention {
                    card: pick(&cards, op.a)?,
                    question: "q".into(),
                    ..NewAttention::default()
                }
            )
        ),
        9 => run!(
            "attention.answer",
            store.attention_answer(
                &ctx,
                None,
                &pick(&items, op.a)?,
                &format!("a{}", op.b % 2),
                AnsweredVia::Board
            )
        ),
        10 => run!(
            "attention.withdraw",
            store.attention_withdraw(&ctx, &pick(&items, op.a)?, "moot")
        ),
        11 => run!(
            "ask.record",
            store.ask_record(
                &ctx,
                None,
                NewAsk {
                    project_id: "alpha".into(),
                    session_id: None,
                    source: AskSource::Script,
                    text: "note".into(),
                }
            )
        ),
        12 => {
            let resolution = match op.b % 3 {
                0 => Resolution::Chat { reason: None },
                1 => Resolution::Followup {
                    card: pick(&cards, op.b).unwrap_or_default(),
                },
                _ => Resolution::New(card(CardKind::Ship, "from ask")),
            };
            run!(
                "ask.resolve",
                store.ask_resolve(&ctx, &pick(&asks, op.a)?, resolution)
            )
        }
        13 => run!(
            "run.start",
            store.run_start(
                &Ctx::new(Actor::Session(session.into())),
                None,
                builder_run(&pick(&cards, op.a)?)
            )
        ),
        14 => run!(
            "run.end",
            store.run_end(
                &ctx,
                &pick(&runs, op.a)?,
                EndRun {
                    outcome: RunOutcome::Failed,
                    note: None,
                }
            )
        ),
        _ => run!(
            "run.status",
            store.run_status(
                &Ctx::new(Actor::Session(session.into())),
                &pick(&runs, op.a)?,
                StatusState::Working,
                "busy"
            )
        ),
    }
}

/// The tables whose writes are rebuildable, not facts (`state.md` section 10).
const REBUILDABLE: &[&str] = &[
    "events",
    "observations",
    "panes",
    "worktrees",
    "quota_windows",
    "idempotency_keys",
];

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, failure_persistence: None, ..ProptestConfig::default() })]

    /// `state.md` invariant 3 (`core::events::every_mutation_writes_its_event`):
    /// any random sequence of mutations, each one that changes an
    /// authoritative row writes events under its own `txn`, and each refusal
    /// changes nothing. A closed card never has open attention or an active
    /// run (invariant 4).
    #[test]
    fn every_mutation_writes_its_event(ops in ops()) {
        let (mut store, session) = memory_store();
        for op in &ops {
            let before = snapshot(store.conn(), REBUILDABLE);
            let last = store.last_seq().unwrap();
            let Some((txn, result)) = apply(&mut store, &session, op) else { continue };
            let after = snapshot(store.conn(), REBUILDABLE);
            let new_events = store.events_since(last, 1000).unwrap();
            match result {
                Ok(events) => {
                    prop_assert_eq!(&events, &new_events.iter().map(|e| e.seq).collect::<Vec<_>>());
                    if before != after {
                        prop_assert!(!new_events.is_empty(), "{} changed rows without an event", txn);
                    }
                    for event in &new_events {
                        prop_assert_eq!(event.txn.as_str(), txn);
                    }
                }
                Err(error) => {
                    prop_assert!(before == after, "{} failed ({}) but changed rows", txn, error);
                    prop_assert!(new_events.is_empty());
                }
            }
            let broken: i64 = store
                .conn()
                .query_row(
                    "SELECT count(*) FROM cards c WHERE c.state = 'closed' AND (
                       EXISTS (SELECT 1 FROM attention a WHERE a.card_id = c.id AND a.answered_at IS NULL AND a.withdrawn_at IS NULL)
                       OR EXISTS (SELECT 1 FROM runs r WHERE r.card_id = c.id AND r.ended_at IS NULL))",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            prop_assert_eq!(broken, 0);
        }
    }
}
