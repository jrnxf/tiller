//! `tiller recap` (P35): the owner's `/recap` reaches the orchestrator as an
//! ask, and the command hands it the recap and every open decision with its
//! evidence, most blocking first. The real binary and one `state.db`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::World;
use serde_json::Value;

fn open(world: &World, session: &str, card: &str, kind: &str, question: &str, evidence: &str) {
    world.must_as(
        session,
        &[
            "attention",
            "open",
            card,
            "--kind",
            kind,
            "--question",
            question,
            "--evidence",
            evidence,
            "--recommendation",
            "take the first option",
            "--consequence",
            "the card waits until then",
        ],
    );
}

fn card(world: &World, title: &str) -> String {
    let out = world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", title,
    ]);
    out.data()["display_id"].as_str().unwrap().to_owned()
}

#[test]
fn recap_lists_every_open_decision_with_its_evidence_most_blocking_first() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let first = card(&world, "first change");
    let second = card(&world, "second change");
    let third = card(&world, "third change");
    open(
        &world,
        &orchestrator,
        &first,
        "decision",
        "Ship it as is?",
        "{\"tests\":\"green\"}",
    );
    open(
        &world,
        &orchestrator,
        &second,
        "report",
        "Read the scout report",
        "{}",
    );
    open(
        &world,
        &orchestrator,
        &third,
        "credential",
        "Log in to the forge",
        "{\"error\":\"401\"}",
    );

    let out = world.must_as(&orchestrator, &["recap"]);
    let decisions = out.data()["decisions"].as_array().unwrap();
    let order: Vec<&str> = decisions
        .iter()
        .map(|d| d["kind"].as_str().unwrap())
        .collect();
    assert_eq!(order, ["credential", "decision", "report"]);
    assert_eq!(decisions[0]["card"], Value::from(third.as_str()));
    assert_eq!(decisions[0]["position"], 1);
    assert_eq!(decisions[1]["question"], "Ship it as is?");
    assert_eq!(decisions[1]["evidence"], "{\"tests\":\"green\"}");
    assert_eq!(decisions[1]["recommendation"], "take the first option");
    assert_eq!(decisions[1]["consequence"], "the card waits until then");

    let recap = out.data()["recap"].as_array().unwrap();
    let created = recap.iter().filter(|e| e["kind"] == "card.created").count();
    assert_eq!(created, 3, "{recap:?}");
    assert!(recap.iter().any(|e| e["kind"] == "attention.opened"));
}

#[test]
fn recap_drops_answered_items_and_walks_down_the_rest() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let one = card(&world, "one");
    let two = card(&world, "two");
    open(
        &world,
        &orchestrator,
        &one,
        "decision",
        "First question?",
        "{}",
    );
    open(
        &world,
        &orchestrator,
        &two,
        "decision",
        "Second question?",
        "{}",
    );

    let out = world.must_as(&orchestrator, &["recap"]);
    let decisions = out.data()["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), 2);
    // Equal weight: the oldest question comes first.
    assert_eq!(decisions[0]["question"], "First question?");

    let id = decisions[0]["id"].as_str().unwrap().to_owned();
    world.must(&["attention", "answer", &id, "--text", "yes"]);
    let out = world.must_as(&orchestrator, &["recap"]);
    let decisions = out.data()["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["question"], "Second question?");
    assert_eq!(decisions[0]["position"], 1);
}

#[test]
fn recap_with_nothing_open_says_so_with_an_empty_list() {
    let world = World::initialized();
    let out = world.must(&["recap"]);
    assert_eq!(out.data()["decisions"].as_array().unwrap().len(), 0);
}

#[test]
fn ahoy_is_a_hidden_alias_for_recap() {
    let world = World::initialized();
    let old = world.must(&["ahoy"]);
    let new = world.must(&["recap"]);
    assert_eq!(old.data()["decisions"], new.data()["decisions"]);

    let help = std::process::Command::new(env!("CARGO_BIN_EXE_tiller"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(
        help.lines()
            .any(|line| line.trim_start().starts_with("recap "))
    );
    assert!(!help.contains("ahoy"));
}
