use rusqlite::params;
use serde_json::{Value, json};

use super::*;
use crate::state::projects::ProjectEdit;
use crate::state::tests::{card, memory_store, owner};
use crate::store::Actor;

const RAISE: &str = "Raise to 4";
const WAIT: &str = "Wait for fleet-3";

fn capacity(store: &Store) -> i64 {
    get::<Project>(store.conn(), "project", "alpha")
        .unwrap()
        .capacity
}

fn raise(from: i64, to: i64) -> Value {
    json!([
        {"label": RAISE, "recommended": true, "consequence": "a second run starts now",
         "action": {"kind": "project_capacity", "project": "alpha", "from": from, "to": to}},
        {"label": WAIT, "consequence": "the card waits"},
    ])
}

fn open(store: &mut Store, options: Value) -> Result<Attention> {
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "second worker"), None)
        .unwrap()
        .value;
    store
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: card.id,
                question: "Raise alpha capacity to 4, or wait for fleet-3?".into(),
                options: Some(options),
                ..NewAttention::default()
            },
        )
        .map(|applied| applied.value)
}

fn answer(store: &mut Store, actor: Actor, item: &str, words: &str) -> Result<Answered> {
    store
        .attention_answer(&Ctx::new(actor), None, item, words, AnsweredVia::Board)
        .map(|applied| applied.value)
}

fn edits(store: &Store) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'project.edited' AND entity_id = 'alpha'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// The CP5 bug: the owner picked "raise" and capacity stayed where it was.
/// Picking the option on the board applies its owner action in the answer's
/// transaction, and the orchestrator hears what was applied.
#[test]
fn a_board_answer_applies_the_picked_options_owner_action() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    let answered = answer(&mut store, Actor::Board, &item.id, RAISE).unwrap();
    assert_eq!(capacity(&store), 4);
    assert_eq!(
        answered.applied,
        Some(OwnerAction::ProjectCapacity {
            project: "alpha".into(),
            from: 3,
            to: 4
        })
    );
    let body = answered.message.unwrap().body;
    assert!(
        body.ends_with("applied: project alpha capacity 3 to 4"),
        "{body}"
    );
    let payload: Value = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'attention.answered' AND entity_id = ?1",
            [&item.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload["applied"]["to"], 4);
    assert_eq!(edits(&store), 1);
}

/// The board sends the label, then the owner's note after a blank line; an
/// owner terminal sends the label. Both pick the option.
#[test]
fn an_option_with_a_note_and_an_owner_terminal_answer_both_apply() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    let noted = format!("{RAISE}\n\nonly until fleet-3 lands");
    answer(&mut store, Actor::Board, &item.id, &noted).unwrap();
    assert_eq!(capacity(&store), 4);

    let item = open(&mut store, raise(4, 5)).unwrap();
    let answered = answer(&mut store, Actor::Owner, &item.id, RAISE).unwrap();
    assert!(answered.applied.is_some());
    assert_eq!(capacity(&store), 5);
}

/// The same words again replay the first answer and apply nothing twice.
#[test]
fn a_replayed_answer_applies_once() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    answer(&mut store, Actor::Board, &item.id, RAISE).unwrap();
    let again = answer(&mut store, Actor::Board, &item.id, RAISE).unwrap();
    assert_eq!(again.applied, None);
    assert_eq!(capacity(&store), 4);
    assert_eq!(edits(&store), 1);
}

/// An owner terminal answer picks the option by its first line, ignoring
/// case and surrounding space; the rest of the answer is the owner's note.
#[test]
fn a_pick_ignores_case_and_space_and_reads_the_first_line() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    let answered = answer(
        &mut store,
        Actor::Owner,
        &item.id,
        "  raise TO 4 \nonly today",
    )
    .unwrap();
    assert!(answered.applied.is_some());
    assert_eq!(capacity(&store), 4);
}

/// The answer's idempotency key belongs to the item, not to the project the
/// action edited.
#[test]
fn the_answers_key_belongs_to_the_item() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    store
        .attention_answer(
            &Ctx::new(Actor::Board),
            Some("answer-1"),
            &item.id,
            RAISE,
            AnsweredVia::Board,
        )
        .unwrap();
    let entity: (String, String) = store
        .conn()
        .query_row(
            "SELECT entity, entity_id FROM idempotency_keys WHERE key = 'answer-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(entity, ("attention".into(), item.id));
}

/// A question that no longer stands applies nothing: the action is refused
/// and the item stays open for another option.
#[test]
fn a_stale_question_applies_no_action() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    store
        .conn()
        .execute(
            "UPDATE cards SET state = 'closed', outcome = 'cancelled', closed_at = created_at WHERE id = ?1",
            [&item.card_id],
        )
        .unwrap();
    let error = answer(&mut store, Actor::Owner, &item.id, RAISE).unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(error.to_string().contains("no longer current"), "{error}");
    assert_eq!(capacity(&store), 3);
    let answered = answer(&mut store, Actor::Owner, &item.id, WAIT).unwrap();
    assert!(!answered.still_current);
}

/// An item opened before options carried actions may hold any `action`
/// value; it answers as before and applies nothing.
#[test]
fn an_item_from_before_actions_still_answers() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, json!(["Ship it", "Wait"])).unwrap();
    store
        .conn()
        .execute(
            "UPDATE attention SET options = ?2 WHERE id = ?1",
            params![
                item.id,
                json!([{"label": "Ship it", "action": "merge"}, "Wait"]).to_string()
            ],
        )
        .unwrap();
    let answered = answer(&mut store, Actor::Board, &item.id, "Ship it").unwrap();
    assert_eq!(answered.applied, None);
    assert!(!answered.message.unwrap().body.contains("applied:"));
}

/// Another option, or the owner's own words, records the answer and changes
/// nothing; the orchestrator is told that nothing was applied.
#[test]
fn another_option_applies_nothing_and_says_so() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    let answered = answer(&mut store, Actor::Board, &item.id, WAIT).unwrap();
    assert_eq!(answered.applied, None);
    assert_eq!(capacity(&store), 3);
    let body = answered.message.unwrap().body;
    assert!(body.ends_with("applied: nothing"), "{body}");

    let item = open(&mut store, raise(3, 4)).unwrap();
    let answered = answer(&mut store, Actor::Board, &item.id, "raise").unwrap();
    assert_eq!(answered.applied, None);
    assert_eq!(capacity(&store), 3);
}

/// An owner action applies only where the owner principal answers: the
/// orchestrator recording chat words and a forwarded peer answer are refused,
/// and nothing is recorded.
#[test]
fn only_the_local_owner_picks_an_option_with_an_owner_action() {
    let (mut store, session) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    for actor in [Actor::Session(session), Actor::Peer("host-b".into())] {
        let error = answer(&mut store, actor, &item.id, RAISE).unwrap_err();
        assert_eq!(error.code(), "forbidden", "{error}");
        assert!(error.to_string().contains("this host's board"), "{error}");
    }
    let item: Attention = get(store.conn(), "attention", &item.id).unwrap();
    assert!(item.answered_at.is_none());
    assert_eq!(capacity(&store), 3);
}

/// Capacity that moved since the question was asked refuses the option, so
/// an old question never undoes a newer change.
#[test]
fn an_option_whose_capacity_moved_is_refused() {
    let (mut store, _) = memory_store();
    let item = open(&mut store, raise(3, 4)).unwrap();
    store
        .project_edit(
            &owner(),
            "alpha",
            get::<Project>(store.conn(), "project", "alpha")
                .unwrap()
                .rev,
            ProjectEdit {
                capacity: Some(5),
                ..ProjectEdit::default()
            },
        )
        .unwrap();
    let error = answer(&mut store, Actor::Board, &item.id, RAISE).unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(
        error.to_string().contains("capacity is 5 now, not 3"),
        "{error}"
    );
    assert_eq!(capacity(&store), 5);
    let item: Attention = get(store.conn(), "attention", &item.id).unwrap();
    assert!(item.answered_at.is_none());
    answer(&mut store, Actor::Board, &item.id, WAIT).unwrap();
}

/// `attention.open` checks every action against the record before the owner
/// ever sees it.
#[test]
fn open_refuses_an_action_that_cannot_apply() {
    let (mut store, _) = memory_store();
    let cases = [
        (raise(2, 4), "capacity is 3 now, not 2"),
        (raise(3, 3), "already"),
        (raise(3, 0), "at least 1"),
        (
            json!([{"label": RAISE, "action": {"kind": "project_capacity", "project": "beta", "from": 1, "to": 2}}]),
            "is in alpha",
        ),
        (
            json!([{"label": RAISE, "action": {"kind": "merge_now"}}]),
            "action",
        ),
        (
            json!([{"label": RAISE, "action": {"kind": "project_capacity", "project": "alpha", "from": 3, "to": 4, "extra": 1}}]),
            "action",
        ),
        (
            json!([{"action": {"kind": "project_capacity", "project": "alpha", "from": 3, "to": 4}}]),
            "label",
        ),
        (
            json!([{"label": RAISE, "action": {"kind": "project_capacity", "project": "alpha", "from": 3, "to": 4}}, RAISE]),
            "twice",
        ),
        (
            json!([{"label": RAISE, "action": {"kind": "project_capacity", "project": "alpha", "from": 3, "to": 4}}, "raise to 4 "]),
            "twice",
        ),
    ];
    for (options, needle) in cases {
        let error = open(&mut store, options.clone()).unwrap_err();
        assert!(
            error.to_string().contains(needle),
            "{options}: {error} lacks {needle:?}"
        );
    }
    // Options with no action keep their old shapes.
    open(
        &mut store,
        json!(["green", {"label": "blue"}, {"label": "blue"}]),
    )
    .unwrap();
}
