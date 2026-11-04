//! `tiller mem note` and `tiller mem propose` end to end: the real binary,
//! one `state.db` and the memory repo under the home (`memory.md` section 12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::Command;

use common::{Out, World};

/// Runs `tiller --json` as the curator of `retro`.
fn run_as_curator(world: &World, retro: &str, args: &[&str]) -> Out {
    let output = Command::new(env!("CARGO_BIN_EXE_tiller"))
        .env("TILLER_HOME", world.home())
        .env("TILLER_RETRO_ID", retro)
        .env_remove("TILLER_SESSION_ID")
        .env_remove("CLAUDECODE")
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    Out {
        code: output.status.code().unwrap(),
        json: serde_json::from_str(&text).unwrap_or_else(|e| panic!("{args:?}: {e}: {text}")),
    }
}

/// A failed card with its pending full retro. Returns (card id, retro id).
fn failed_retro(world: &World) -> (String, String) {
    world.must(&["config", "set", "learning.sample_every", "1000000"]);
    let card = world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "the export",
    ]);
    let card = card.data()["id"].as_str().unwrap().to_owned();
    world
        .store()
        .conn()
        .execute(
            "UPDATE cards SET state = 'closed', outcome = 'failed', closed_at = '2026-11-03T10:00:00.000Z',
                 created_at = '2026-11-03T08:00:00.000Z', closed_by = 'owner' WHERE id = ?1",
            [&card],
        )
        .unwrap();
    let swept = world.must(&["retro", "sweep"]);
    let retro = swept.data()["scheduled"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (card, retro)
}

fn cited(world: &World, proposal: &str) -> Vec<String> {
    world
        .store()
        .conn()
        .prepare("SELECT evidence_id FROM proposal_evidence WHERE proposal_id = ?1")
        .unwrap()
        .query_map([proposal], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[test]
fn the_curator_notes_proposes_and_finishes_with_the_proposal() {
    let world = World::initialized();
    let (card, retro) = failed_retro(&world);

    let noted = run_as_curator(
        &world,
        &retro,
        &[
            "mem",
            "note",
            "--kind",
            "incident",
            "--text",
            "The export failed twice on the same missing fixture.",
            "--ref",
            r#"{"events": [3, 4]}"#,
        ],
    );
    assert!(noted.ok(), "{}", noted.json);
    assert_eq!(noted.data()["project_id"], "alpha");
    assert_eq!(noted.data()["card_id"], card.as_str());
    let evidence = noted.data()["evidence_id"].as_str().unwrap().to_owned();

    let proposed = run_as_curator(
        &world,
        &retro,
        &[
            "mem",
            "propose",
            "alpha",
            "--add",
            "Watch out for",
            "--text",
            "The export tests need the fixture build first.",
            "--evidence",
            &evidence,
            "--expected-effect",
            "no export run fails on a missing fixture",
        ],
    );
    assert!(proposed.ok(), "{}", proposed.json);
    let proposal = proposed.data()["id"].as_str().unwrap().to_owned();
    assert_eq!(proposed.data()["state"], "proposed");
    assert_eq!(proposed.data()["card_id"], card.as_str());
    assert_eq!(cited(&world, &proposal), vec![evidence]);
    assert!(world.home().join("memory/.git").is_dir());

    let done = run_as_curator(
        &world,
        &retro,
        &["retro", "finish", &retro, "--proposed", &proposal],
    );
    assert!(done.ok(), "{}", done.json);
    assert_eq!(done.data()["outcome"], "proposed");

    // A finished retro's curator may not note or propose any more.
    let late = run_as_curator(&world, &retro, &["mem", "note", "--text", "One more fact."]);
    assert_eq!(late.code, 5, "{}", late.json);
}

#[test]
fn a_proposal_needs_evidence_and_a_line_that_passes_the_scrub() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let noted = world.must_as(
        &orchestrator,
        &["mem", "note", "--text", "Staging resets at midnight."],
    );
    let evidence = noted.data()["evidence_id"].as_str().unwrap().to_owned();

    let bare = world.run_as(
        &orchestrator,
        &[
            "mem",
            "propose",
            "alpha",
            "--add",
            "Watch out for",
            "--text",
            "Staging resets at midnight.",
            "--expected-effect",
            "no midnight retries",
        ],
    );
    assert_eq!(
        (bare.code, bare.error_code()),
        (2, "usage"),
        "{}",
        bare.json
    );

    let private = concat!("Logs are in ~", "/notes/staging.txt.");
    let leaky = world.run_as(
        &orchestrator,
        &[
            "mem",
            "propose",
            "alpha",
            "--add",
            "Watch out for",
            "--text",
            private,
            "--evidence",
            &evidence,
            "--expected-effect",
            "no midnight retries",
        ],
    );
    assert_eq!(
        (leaky.code, leaky.error_code()),
        (5, "refused"),
        "{}",
        leaky.json
    );
    let message = leaky.json["error"]["message"].as_str().unwrap();
    assert!(!message.contains("notes/staging"), "{message}");

    let elsewhere = world.run_as(
        &orchestrator,
        &[
            "mem",
            "propose",
            "beta",
            "--retire",
            "01JC00000000000000000000M1",
        ],
    );
    assert_eq!(elsewhere.code, 6, "{}", elsewhere.json);

    let approve = world.run(&["mem", "approve", "01JC00000000000000000000P1"]);
    assert_eq!((approve.code, approve.error_code()), (7, "unavailable"));
}
