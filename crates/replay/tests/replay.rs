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
use tiller_replay::board::{BoardCard, History};
use tiller_replay::harness::{self, Options, Replay};
use tiller_replay::sandbox::git;
use tiller_replay::{Audit, Decision, Orchestrator, Recorded, Sandbox, Turn, audit, baseline};

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
    let mut cards = [
        card("web-login", "acme.web", "ship", "2026-02-01", "merged"),
        card("web-docs", "acme.web", "ship", "2026-03-10", "merged"),
        card("web-extra", "acme.web", "ship", "2026-03-12", "merged"),
        card("orbit-plan", "orbit", "captain", "2026-03-11", "answered"),
        card("orbit-probe", "orbit", "scout", "2026-03-11", "reported"),
    ];
    // The exporter renamed this card; the baseline names it by its earlier id.
    cards[0]["id"] = json!("web-signin");
    cards[0]["legacy_id"] = json!("web-login");
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
    let history = History::new(&bundle.cards, &asks);
    let mut replay = Replay::new(&mut sandbox, "orbit".into(), imported, history);
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

/// An invented Claude Code: it logs the model and effort each call asked
/// for, answers every orchestrator turn in chat, and gives the judge a
/// verdict for each difference id, the first one "worse".
const FAKE_CLAUDE: &str = r###"#!/bin/sh
log="$(dirname "$0")/calls.log"
schema=""
while [ $# -gt 1 ]; do
  case "$1" in
    --model) model="$2"; shift ;;
    --effort) effort="$2"; shift ;;
    --append-system-prompt) shift ;;
    --json-schema) schema="$2"; shift ;;
  esac
  shift
done
echo "$model $effort" >> "$log"
case "$schema" in
  *verdicts*)
    ids=$(printf '%s' "$1" | grep -o '"id": "a[0-9]*\.[0-9]*"' | sed 's/.*"\(a[^"]*\)"/\1/')
    items=""
    verdict=worse
    for id in $ids; do
      [ -n "$items" ] && items="$items,"
      items="$items{\"id\":\"$id\",\"verdict\":\"$verdict\",\"evidence\":[{\"source\":\"ask\",\"quote\":\"## The ask\"},{\"source\":\"ask\",\"quote\":\"not in the input\"}],\"rationale\":\"invented\",\"confidence\":\"low\",\"blame\":\"tiller\",\"fix\":\"an invented fix\"}"
      verdict=neutral
    done
    answer="{\"verdicts\":[$items]}" ;;
  *) answer='{"decisions":[{"do":"chat"},{"do":"reply","text":"noted"}]}' ;;
esac
printf '{"is_error":false,"total_cost_usd":0.25,"modelUsage":{"claude-test":{}},"structured_output":%s}\n' "$answer"
"###;

#[test]
fn the_cli_replays_on_claude_and_judges_each_difference() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let bundle = temp.path().join("bundle");
    write_bundle(&bundle);
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    fs::write(&claude, FAKE_CLAUDE).unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    let out = temp.path().join("out");
    let tool = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_tiller-replay"))
            .args(args)
            .arg(&bundle)
            .arg("--out")
            .arg(&out)
            .arg("--claude-program")
            .arg(&claude)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let calls = || fs::read_to_string(bin.join("calls.log")).unwrap();

    let summary = tool(&["run", "--claude"]);
    assert_eq!(summary["asks"], ASKS.len());
    assert_eq!(summary["dispositions"], json!({"chat": ASKS.len()}));
    // Every turn ran the packaged orchestrator role's model and effort.
    assert_eq!(calls(), "opus medium\n".repeat(ASKS.len()));
    let turns = fs::read_to_string(out.join("turns.jsonl")).unwrap();
    assert_eq!(turns.lines().count(), ASKS.len());

    let counts = tool(&["judge"]);
    let differences: Vec<Value> =
        serde_json::from_slice(&fs::read(out.join("differences.json")).unwrap()).unwrap();
    assert!(!differences.is_empty());
    assert_eq!(counts["differences"], differences.len());
    let log = calls();
    let judged = log.lines().skip(ASKS.len()).collect::<Vec<_>>();
    assert!(!judged.is_empty());
    assert!(
        judged.iter().all(|c| *c == "claude-opus-5-5 high"),
        "{judged:?}"
    );
    let report = fs::read_to_string(out.join("report.md")).unwrap();
    for d in &differences {
        assert!(report.contains(&format!("**{}**", d["id"].as_str().unwrap())));
    }
    assert!(report.contains("## Uncertainty"));
    assert!(report.contains("Tiller defect. Fix: an invented fix"));
    assert!(report.contains("- Evidence (ask): \"## The ask\"\n"));
    assert!(report.contains("\"not in the input\" (not found verbatim)"));

    // A rewrite from the verdicts already judged calls no model.
    let before = calls();
    let again = tool(&["judge", "--no-judge"]);
    assert_eq!(again, counts);
    assert_eq!(calls(), before);
}

/// Decides every turn with the same list.
struct Scripted(Value);

impl Orchestrator for Scripted {
    fn decide(&mut self, _: &Turn) -> Result<Vec<Decision>, tiller_replay::Error> {
        Ok(serde_json::from_value(self.0.clone()).unwrap())
    }
}

#[test]
fn a_spawn_on_another_projects_card_is_refused_as_live() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("bundle");
    write_bundle(&dir);
    let before = audit::fingerprint(&dir).unwrap();
    let bundle = Bundle::read(&dir).unwrap();
    let asks = baseline::read(&dir.join("baseline").join("replay.jsonl")).unwrap();
    let mut sandbox = Sandbox::create(&temp.path().join("sandbox"), &bundle, None).unwrap();
    let imported = sandbox.imported(&bundle).unwrap();
    let history = History::new(&bundle.cards, &asks);
    let mut replay = Replay::new(&mut sandbox, "orbit".into(), imported, history);
    // The card-less question goes to orbit's orchestrator, which works a
    // web card. The live config has a checkout for every project, so spawn
    // refuses it for the live reason, not for a missing checkout.
    let mut orchestrator = Scripted(json!([
        {"do": "reopen", "card": "web-login", "reason": "broke again"},
        {"do": "followup", "card": "web-login"},
        {"do": "spawn", "card": "web-login", "role": "builder"},
        {"do": "reply", "text": "on it"},
    ]));
    let record = replay.ask(2, &asks[2], &mut orchestrator).unwrap();
    drop(replay);
    assert_eq!(record.project, "orbit");
    let refused: Vec<&str> = record
        .decisions
        .iter()
        .filter(|s| !s.ok)
        .map(|s| s.result.as_str().unwrap())
        .collect();
    assert_eq!(refused.len(), 1, "{:?}", record.decisions);
    assert!(
        refused[0].contains("is not the live orchestrator of acmeweb"),
        "{refused:?}"
    );
    assert!(record.runs.is_empty());
    assert!(Audit::run(&sandbox, &dir, &before, None).unwrap().clean());
}

/// Answers each turn from a list by ask index, keeping each board shown,
/// and gives no answer on the turns in `failing`.
struct Watching {
    answers: HashMap<usize, Value>,
    boards: HashMap<usize, Vec<BoardCard>>,
    failing: Vec<usize>,
}

impl Orchestrator for Watching {
    fn decide(&mut self, turn: &Turn) -> Result<Vec<Decision>, tiller_replay::Error> {
        self.boards.insert(turn.index, turn.board.clone());
        if self.failing.contains(&turn.index) {
            return Err(tiller_replay::Error::Orchestrator(
                "no valid decision list".into(),
            ));
        }
        let answer = self.answers.get(&turn.index).cloned();
        Ok(serde_json::from_value(answer.unwrap_or_else(|| json!([{"do": "chat"}]))).unwrap())
    }
}

#[test]
fn a_label_names_a_card_in_its_own_turn_and_later_boards_show_every_filed_card() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("bundle");
    write_bundle(&dir);
    let bundle = Bundle::read(&dir).unwrap();
    let asks = baseline::read(&dir.join("baseline").join("replay.jsonl")).unwrap();
    let mut sandbox = Sandbox::create(&temp.path().join("sandbox"), &bundle, None).unwrap();
    let imported = sandbox.imported(&bundle).unwrap();
    let history = History::new(&bundle.cards, &asks);
    let mut replay = Replay::new(&mut sandbox, "orbit".into(), imported, history);
    let filing = |title: &str| {
        json!([
            {"do": "new", "label": "new1", "kind": "ship", "title": title},
            {"do": "attention", "card": "new1", "question": "which page?"},
        ])
    };
    let mut orchestrator = Watching {
        answers: HashMap::from([(0, filing("Docs page")), (1, filing("Login fix"))]),
        boards: HashMap::new(),
        failing: Vec::new(),
    };
    let first = replay.ask(0, &asks[0], &mut orchestrator).unwrap();
    let second = replay.ask(1, &asks[1], &mut orchestrator).unwrap();
    let filed = |record: &harness::Record| record.decisions[0].result["card"].clone();
    // A card waits on one an earlier turn filed.
    orchestrator.answers.insert(
        5,
        json!([
            {"do": "new", "label": "extra", "kind": "ship", "title": "Extra page"},
            {"do": "dep", "card": "extra", "depends_on": filed(&first)},
        ]),
    );
    let last = replay.ask(5, &asks[5], &mut orchestrator).unwrap();
    drop(replay);

    let asked = |record: &harness::Record| record.decisions[1].result["card"]["display_id"].clone();
    assert_ne!(filed(&first), filed(&second));
    assert_eq!(asked(&first), filed(&first));
    assert_eq!(asked(&second), filed(&second));
    let dep = &last.decisions[1];
    assert!(dep.ok, "{dep:?}");
    assert_eq!(dep.result["card"]["display_id"], filed(&last));
    assert_eq!(dep.result["depends_on"]["display_id"], filed(&first));

    let board = &orchestrator.boards[&5];
    let replayed: Vec<&str> = board
        .iter()
        .filter(|c| c.replayed)
        .map(|c| c.title.as_str())
        .collect();
    assert_eq!(replayed, ["Docs page", "Login fix"]);
    // Ask 0 filed web-docs in Firstmate: the replay's own card stands in.
    assert!(
        board.iter().all(|c| c.title != "Card web-docs"),
        "{board:?}"
    );
}

#[test]
fn one_hold_date_is_one_instant_and_an_unanswered_turn_does_not_end_the_replay() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("bundle");
    write_bundle(&dir);
    let bundle = Bundle::read(&dir).unwrap();
    let asks = baseline::read(&dir.join("baseline").join("replay.jsonl")).unwrap();
    let mut sandbox = Sandbox::create(&temp.path().join("sandbox"), &bundle, None).unwrap();
    let imported = sandbox.imported(&bundle).unwrap();
    let history = History::new(&bundle.cards, &asks);
    let mut replay = Replay::new(&mut sandbox, "orbit".into(), imported, history);
    let holding = json!([
        {"do": "followup", "card": "web-login"},
        {"do": "reopen", "card": "web-login", "reason": "the login breaks again"},
        {"do": "hold", "card": "web-login", "until": "2026-03-20T09:00:00Z", "reason": "after the release"},
        {"do": "spawn", "card": "web-login", "role": "builder", "effort": "high"},
    ]);
    let mut orchestrator = Watching {
        answers: HashMap::from([(1, holding.clone()), (5, holding)]),
        boards: HashMap::new(),
        failing: vec![2],
    };
    let first = replay.ask(1, &asks[1], &mut orchestrator).unwrap();
    let unanswered = replay.ask(2, &asks[2], &mut orchestrator).unwrap();
    let again = replay.ask(5, &asks[5], &mut orchestrator).unwrap();
    drop(replay);

    // Two days apart, the same date is the same hold.
    for record in [&first, &again] {
        assert!(record.decisions[2].ok, "{:?}", record.decisions[2]);
    }
    assert_eq!(
        first.decisions[2].result["until"],
        again.decisions[2].result["until"]
    );
    // The record names the card by the id the baseline uses.
    assert_eq!(first.decisions[1].result["legacy"], "web-login");
    assert_eq!(first.decisions[2].result["card"]["legacy_id"], "web-login");
    // As `tiller spawn`, an effort with no model is refused.
    let spawn = &first.decisions[3];
    assert!(!spawn.ok);
    assert!(
        spawn.result.as_str().unwrap().contains("only with a model"),
        "{spawn:?}"
    );
    assert_eq!(
        unanswered.unanswered.as_deref(),
        Some("no valid decision list")
    );
    assert!(unanswered.decisions.is_empty());
}
