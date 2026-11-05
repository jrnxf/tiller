//! A decision option that carries an owner action, end to end: the real
//! binary and one `state.db`. The orchestrator asks, the owner picks, and the
//! change the option names happens in the same answer.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::World;
use serde_json::{Value, json};

fn capacity(world: &World) -> i64 {
    let out = world.must(&["project", "list"]);
    let projects = out.data()["projects"].as_array().unwrap();
    projects
        .iter()
        .find(|project| project["id"] == "alpha")
        .and_then(|project| project["capacity"].as_i64())
        .unwrap()
}

fn ask(world: &World, session: &str) -> String {
    let card = world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "second worker",
    ]);
    let card = card.data()["display_id"].as_str().unwrap().to_owned();
    let options = json!([
        {"label": "Raise to 4", "recommended": true,
         "action": {"kind": "project_capacity", "project": "alpha", "from": 3, "to": 4}},
        "Wait for fleet-3",
    ])
    .to_string();
    let out = world.must_as(
        session,
        &[
            "attention",
            "open",
            &card,
            "--kind",
            "decision",
            "--question",
            "Raise alpha capacity to 4, or wait for fleet-3?",
            "--options",
            &options,
        ],
    );
    out.data()["id"].as_str().unwrap().to_owned()
}

/// The CP5 bug: the owner answered "raise" and capacity stayed 1, because the
/// orchestrator that asked may not edit capacity.
#[test]
fn the_owner_picking_a_capacity_option_raises_capacity() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let item = ask(&world, &orchestrator);

    // The orchestrator relaying the owner's chat words may not apply it.
    let relayed = world.run_as(
        &orchestrator,
        &["attention", "answer", &item, "--text", "Raise to 4"],
    );
    assert_eq!(relayed.error_code(), "forbidden", "{}", relayed.json);
    assert_eq!(capacity(&world), 3);

    let answered = world.must(&["attention", "answer", &item, "--text", "Raise to 4"]);
    assert_eq!(
        answered.data()["applied"],
        json!({"kind": "project_capacity", "project": "alpha", "from": 3, "to": 4})
    );
    assert_eq!(capacity(&world), 4);
    let body = answered.data()["message"]["body"].as_str().unwrap();
    assert!(
        body.ends_with("applied: project alpha capacity 3 to 4"),
        "{body}"
    );
}

/// The other option records the answer, changes nothing, and says so.
#[test]
fn the_other_option_applies_nothing() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let item = ask(&world, &orchestrator);
    let answered = world.must(&["attention", "answer", &item, "--text", "Wait for fleet-3"]);
    assert_eq!(answered.data()["applied"], Value::Null);
    assert_eq!(capacity(&world), 3);
    let body = answered.data()["message"]["body"].as_str().unwrap();
    assert!(body.ends_with("applied: nothing"), "{body}");
}
