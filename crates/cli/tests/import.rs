//! T2.4: `tiller import <bundle> [--dry-run]` through the real binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;

use common::World;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A two-card, two-entry `tiller-import/v1` bundle.
fn write_bundle(dir: &Path) {
    let projects = json!([{"id": "beta", "repo": "example/beta", "remote": null,
                           "default_branch": "main", "delivery": "no-mistakes", "capacity": 2, "added": "2026-09-01"}])
    .to_string();
    let cards = [
        json!({"id": "beta-one", "project": "beta", "title": "Ship the first page", "column": "done",
               "kind": "ship", "priority": 1, "since": "2026-10-01", "hold": null,
               "pr": "https://github.com/example/beta/pull/3", "runs": [], "depends_on": [],
               "category": null, "size": null, "done": {"outcome": "merged", "at": "2026-10-02"}, "body": []}),
        json!({"id": "beta-two", "project": "beta", "title": "Pick a launch date", "column": "needs",
               "kind": "captain", "priority": null, "since": "2026-10-03",
               "hold": {"kind": "captain", "until": null, "reason": "Which week?"}, "pr": null,
               "runs": [], "depends_on": [], "category": null, "size": null, "done": null, "body": []}),
    ]
    .iter()
    .map(|row| format!("{row}\n"))
    .collect::<String>();
    let memory = [
        json!({"id": "mem-1", "text": "Keep PRs small.", "scope": "global", "section": "Rules",
               "class": "rule", "rationale": null, "secret_ref": false, "provenance": "legacy-import"}),
        json!({"id": "mem-2", "text": "An old flow.", "scope": "global", "section": "Rules",
               "class": "stale", "rationale": null, "reason": "Replaced.", "secret_ref": false,
               "provenance": "legacy-import"}),
    ]
    .iter()
    .map(|row| format!("{row}\n"))
    .collect::<String>();
    std::fs::write(dir.join("projects.json"), &projects).unwrap();
    std::fs::write(dir.join("cards.jsonl"), &cards).unwrap();
    std::fs::write(dir.join("memory.jsonl"), &memory).unwrap();
    let manifest = json!({
        "schema": "tiller-import/v1", "mode": "dry", "host_label": "host-a",
        "generated_at": "2026-11-01T21:05:00+00:00",
        "counts": {"projects": 1, "cards": 2, "inflight": 0, "memory": 2},
        "files": {"projects.json": sha256(&projects), "cards.jsonl": sha256(&cards), "memory.jsonl": sha256(&memory)},
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

fn events(world: &World) -> i64 {
    world
        .store()
        .conn()
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn dry_run_prints_the_plan_then_import_writes_it_once() {
    let world = World::initialized();
    let bundle = world.dir.path().join("bundle");
    std::fs::create_dir_all(&bundle).unwrap();
    write_bundle(&bundle);
    let path = bundle.to_str().unwrap();
    let before = events(&world);

    let dry = world.must(&["import", path, "--dry-run"]);
    assert_eq!(events(&world), before, "a dry run wrote");
    let plan = dry.data();
    assert!(dry.json.get("events").is_none());
    assert!(
        plan["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["ok"] == true),
        "{plan}"
    );
    let cards: Vec<(&str, &str, &str)> = plan["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["legacy_id"].as_str().unwrap(),
                c["display_id"].as_str().unwrap(),
                c["column"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        cards,
        [
            ("beta-one", "beta-1", "done"),
            ("beta-two", "beta-2", "needs_you")
        ]
    );
    assert_eq!(
        plan["memory"][0]["placement"],
        json!({"as": "proposal", "target": "fleet.md", "section": "How we work here"})
    );
    assert_eq!(
        plan["memory"][1]["placement"],
        json!({"as": "not_placed", "reason": "Replaced."})
    );

    let first = world.must(&["import", path]);
    assert!(!first.replayed());
    assert!(!first.events().is_empty());
    let board = world.must(&["card", "show", "beta-2"]);
    assert_eq!(
        board.data()["card"]["legacy_id"],
        "beta-two",
        "{}",
        board.json
    );

    let second = world.must(&["import", path]);
    assert!(second.replayed());
    assert!(second.events().is_empty());
    assert_eq!(second.data()["already_imported"], Value::Bool(true));
}

#[test]
fn import_is_owner_only_and_refuses_a_tampered_bundle() {
    let world = World::initialized();
    let bundle = world.dir.path().join("bundle");
    std::fs::create_dir_all(&bundle).unwrap();
    write_bundle(&bundle);
    let path = bundle.to_str().unwrap();

    let session = world.orchestrator();
    let out = world.run_as(&session, &["import", path]);
    assert_eq!(
        (out.code, out.error_code()),
        (6, "forbidden"),
        "{}",
        out.json
    );

    std::fs::write(bundle.join("cards.jsonl"), "{}\n").unwrap();
    let out = world.run(&["import", path, "--dry-run"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);
    assert!(
        out.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cards.jsonl differs"),
        "{}",
        out.json
    );
}
