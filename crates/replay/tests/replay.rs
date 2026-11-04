//! A replay of a small invented bundle, end to end: the real hooks, the
//! recorded orchestrator, spawn against the fake herdr and the stub pool,
//! and the audit.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiller_core::import::Bundle;
use tiller_replay::harness::{self, Options, Replay};
use tiller_replay::sandbox::git;
use tiller_replay::{Audit, Recorded, Sandbox, audit, baseline};

const ASKS: &[&str] = &[
    "please write the docs page",
    "the login form broke again",
    "what does orbit do",
    "plan the next orbit step",
    "never mind that",
    "login again, and add the extra page",
];

fn write_bundle(dir: &Path) {
    fs::create_dir_all(dir.join("baseline")).unwrap();
    let projects = json!([
        {"id": "acme.web", "repo": "acme/web", "remote": "https://example.com/acme/web.git",
         "default_branch": "main", "delivery": "direct-pr", "capacity": 2, "added": "2026-01-05"},
        {"id": "orbit", "repo": "acme/orbit", "remote": "https://example.com/acme/orbit.git",
         "default_branch": "main", "delivery": "direct-pr", "capacity": 2, "added": "2026-01-05"},
    ]);
    let card = |id: &str, project: &str, kind: &str, since: &str, outcome: &str| {
        json!({"id": id, "project": project, "title": format!("Card {id}"), "column": "done",
               "kind": kind, "since": since, "done": {"outcome": outcome, "at": since},
               "body": ["An invented card."], "runs": [], "depends_on": []})
    };
    let cards = [
        card("web-login", "acme.web", "ship", "2026-02-01", "merged"),
        card("web-docs", "acme.web", "ship", "2026-03-10", "merged"),
        card("web-extra", "acme.web", "ship", "2026-03-12", "merged"),
        card("orbit-plan", "orbit", "captain", "2026-03-11", "answered"),
        card("orbit-probe", "orbit", "scout", "2026-03-11", "reported"),
    ];
    let lines = |rows: &[Value]| rows.iter().map(|r| format!("{r}\n")).collect::<String>();
    let files = [
        ("projects.json", projects.to_string()),
        ("cards.jsonl", lines(&cards)),
        ("memory.jsonl", String::new()),
    ];
    let mut hashes = serde_json::Map::new();
    for (name, text) in &files {
        fs::write(dir.join(name), text).unwrap();
        let hash = Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        hashes.insert((*name).to_owned(), json!(hash));
    }
    let manifest = json!({
        "schema": "tiller-import/v1", "mode": "dry", "host_label": "example",
        "counts": {"projects": 2, "cards": cards.len(), "inflight": 0, "memory": 0},
        "files": hashes,
    });
    fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();

    let ask = |text: &str, ts: &str, cards: Value, handling: Value, kind: Value, holds: Value| json!({"ask": text, "ts": ts, "cards": cards, "handling": handling, "kind": kind, "holds": holds});
    let asks = [
        ask(
            ASKS[0],
            "2026-03-10T10:00:00Z",
            json!(["web-docs"]),
            json!(["new card"]),
            json!(["ship"]),
            json!([]),
        ),
        ask(
            ASKS[1],
            "2026-03-10T11:00:00Z",
            json!(["web-login"]),
            json!(["follow-up"]),
            json!(["ship"]),
            json!([]),
        ),
        ask(
            ASKS[2],
            "2026-03-10T12:00:00Z",
            json!([]),
            json!(["chat"]),
            json!([]),
            json!([]),
        ),
        ask(
            ASKS[3],
            "2026-03-11T09:00:00Z",
            json!(["orbit-plan", "orbit-probe"]),
            json!(["new card"]),
            json!(["captain", "scout"]),
            json!([{"card": "orbit-plan", "kind": "captain", "until": "2099-01-01", "reason": "waiting on the owner"}]),
        ),
        ask(
            ASKS[4],
            "2026-03-11T10:00:00Z",
            json!([]),
            json!([]),
            json!([]),
            json!([]),
        ),
        ask(
            ASKS[5],
            "2026-03-12T09:00:00Z",
            json!(["web-login", "web-extra"]),
            json!(["follow-up", "new card"]),
            json!(["ship", "ship"]),
            json!([]),
        ),
    ];
    fs::write(dir.join("baseline").join("replay.jsonl"), lines(&asks)).unwrap();
}

#[test]
fn replays_every_ask_with_no_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let bundle = temp.path().join("bundle");
    write_bundle(&bundle);
    let out = temp.path().join("out");
    let options = Options {
        bundle: bundle.clone(),
        sandbox: temp.path().join("sandbox"),
        out: out.clone(),
        dispatch: None,
        real_home: None,
    };
    let summary = harness::run(&options, None).unwrap();

    assert_eq!(summary.asks, ASKS.len());
    let dispositions: Vec<(&str, usize)> = summary
        .dispositions
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    assert_eq!(
        dispositions,
        [("cancelled", 1), ("chat", 1), ("followup", 2), ("new", 2)]
    );
    assert_eq!(summary.refused, 0, "{summary:?}");
    assert_eq!(summary.stop_unresolved, 0);
    // A builder each for the docs, the login follow-ups and the extra page,
    // a scout for the probe; the held question gets none.
    assert_eq!(summary.runs, 5);
    assert!(summary.audit.clean(), "{:?}", summary.audit.findings);
    assert_eq!(summary.audit.leases_taken, 5);
    assert_eq!(summary.audit.leases_returned, 5);
    assert_eq!(summary.audit.events.get("hold.set"), Some(&1));

    let log = fs::read_to_string(out.join("decisions.jsonl")).unwrap();
    let records: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // The import renamed the dotted slug, and the ask went to that project.
    assert_eq!(records[0]["project"], "acmeweb");
    // Of the mixed ask, the card dated to its day was filed, the other
    // followed up.
    let relations: Vec<(&str, &str)> = records[5]["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["legacy_id"].as_str().unwrap_or("new"),
                c["relation"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(relations, [("web-login", "followup"), ("new", "created")]);
    // No words from the bundle reach a record.
    for text in ASKS
        .iter()
        .copied()
        .chain(["Card web", "waiting on the owner"])
    {
        assert!(!log.contains(text), "a record quotes {text:?}");
    }
}

#[test]
fn the_audit_sees_a_push_to_an_origin() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("bundle");
    write_bundle(&dir);
    let before = audit::fingerprint(&dir).unwrap();
    let bundle = Bundle::read(&dir).unwrap();
    let asks = baseline::read(&dir.join("baseline").join("replay.jsonl")).unwrap();
    let mut sandbox = Sandbox::create(&temp.path().join("sandbox"), &bundle, None).unwrap();
    let imported = sandbox.imported(&bundle).unwrap();
    let projects: HashMap<_, _> = imported
        .iter()
        .map(|(k, c)| (k.clone(), c.project_id.clone()))
        .collect();
    let mut recorded = Recorded::new(&asks, &bundle, &projects);
    let ids = imported.into_iter().map(|(k, c)| (k, c.id)).collect();
    let mut replay = Replay::new(&mut sandbox, "orbit".into(), ids);
    replay.ask(0, &asks[0], &mut recorded).unwrap();
    drop(replay);
    assert!(Audit::run(&sandbox, &dir, &before, None).unwrap().clean());

    // A worker that pushed its branch would leave it on the origin.
    let checkout = sandbox.config.checkouts["acmeweb"].clone();
    git(
        &checkout,
        &[
            "push",
            "--quiet",
            "origin",
            "HEAD:refs/heads/tiller/escaped",
        ],
    )
    .unwrap();
    let found = Audit::run(&sandbox, &dir, &before, None).unwrap();
    assert!(
        found
            .findings
            .iter()
            .any(|f| f.contains("refs beyond its seed")),
        "{:?}",
        found.findings
    );
}
