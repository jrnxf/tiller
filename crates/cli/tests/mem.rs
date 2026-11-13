//! `tiller mem note` and `tiller mem propose` end to end: the real binary,
//! one `state.db` and the memory repo under the home (`memory.md` section 12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Out, World};

/// Runs `tiller --json` as the curator of `retro`.
fn run_as_curator(world: &World, retro: &str, args: &[&str]) -> Out {
    let output = common::tiller(world.dir.path())
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
}

/// `initialized`, plus a second project to file proposals against.
fn two_projects() -> World {
    let world = World::initialized();
    world.must(&[
        "project",
        "add",
        "beta",
        "--name",
        "Beta",
        "--repo",
        "example/beta",
        "--delivery",
        "direct-pr",
        "--capacity",
        "3",
    ]);
    world
}

/// An open proposal as the owner files it: a note of evidence, then the line.
fn propose_line(world: &World, project: &str, section: &str, text: &str) -> String {
    let noted = world.must(&["mem", "note", project, "--text", text]);
    let evidence = noted.data()["evidence_id"].as_str().unwrap().to_owned();
    let proposed = world.must(&[
        "mem",
        "propose",
        project,
        "--add",
        section,
        "--text",
        text,
        "--evidence",
        &evidence,
        "--expected-effect",
        "sessions follow the line",
    ]);
    proposed.data()["id"].as_str().unwrap().to_owned()
}

/// Puts `proposals` in one change set whose holdout run passed, so the keep
/// gate lets them through. The set is named after its first proposal, so a
/// test can open more than one.
fn open_gate(world: &World, proposals: &[&String]) {
    let name = format!("placement-{}", proposals[0].to_lowercase());
    let mut args = vec!["learn", "bundle", name.as_str()];
    args.extend(proposals.iter().map(|p| p.as_str()));
    world.must(&args);
    let result = world.dir.path().join("result.json");
    std::fs::write(
        &result,
        r#"{"aggregates": {"casesTotal": 4, "casesPassed": 4, "overallScore": 0.97}, "costUsd": 1.0, "partial": false}"#,
    )
    .unwrap();
    world.must(&["learn", "eval", &name, "--result", result.to_str().unwrap()]);
}

fn states(world: &World) -> Vec<(String, String)> {
    world
        .store()
        .conn()
        .prepare("SELECT id, state FROM proposals ORDER BY created_at, id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn results(out: &Out) -> Vec<(String, String)> {
    out.data()["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_owned(),
                r["result"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn approve_takes_ids_or_a_filter_and_never_everything() {
    let world = two_projects();
    let rule = propose_line(
        &world,
        "alpha",
        "How we work here",
        "Run the formatter before a commit.",
    );
    world.must(&[
        "project",
        "add",
        "gamma",
        "--name",
        "Gamma",
        "--repo",
        "example/gamma",
        "--delivery",
        "direct-pr",
        "--capacity",
        "3",
    ]);
    let note = propose_line(
        &world,
        "gamma",
        "Watch out for",
        "The staging box resets at midnight.",
    );
    let other = propose_line(
        &world,
        "beta",
        "How we work here",
        "Never edit the vendored copy.",
    );
    open_gate(&world, &[&rule, &note, &other]);

    // No ids and no filter: refused as usage, nothing approved.
    let bare = world.run(&["mem", "approve"]);
    assert_eq!(
        (bare.code, bare.error_code()),
        (2, "usage"),
        "{}",
        bare.json
    );
    let both = world.run(&["mem", "approve", &rule, "--class", "rule"]);
    assert_eq!(
        (both.code, both.error_code()),
        (2, "usage"),
        "{}",
        both.json
    );
    assert!(states(&world).iter().all(|(_, state)| state == "proposed"));

    // A dry run lists the match and writes nothing.
    let seq = world.store().last_seq().unwrap();
    let dry = world.must(&["mem", "approve", "--class", "rule", "--dry-run"]);
    assert_eq!(dry.data()["dry_run"], true);
    assert_eq!(dry.data()["would_approve"], 2);
    assert_eq!(
        results(&dry),
        vec![
            (rule.clone(), "would_approve".to_owned()),
            (other.clone(), "would_approve".to_owned())
        ]
    );
    assert_eq!(dry.data()["results"][0]["class"], "rule");
    assert_eq!(dry.data()["results"][0]["project_id"], "alpha");
    assert_eq!(world.store().last_seq().unwrap(), seq);
    assert!(states(&world).iter().all(|(_, state)| state == "proposed"));

    // The class filter takes the rules and leaves the note.
    let done = world.must(&["mem", "approve", "--class", "rule"]);
    assert_eq!(done.data()["approved"], 2);
    assert_eq!(
        results(&done),
        vec![
            (rule.clone(), "approved".to_owned()),
            (other.clone(), "approved".to_owned())
        ]
    );
    let after = states(&world);
    let state_of = |id: &str| after.iter().find(|(i, _)| i == id).unwrap().1.clone();
    assert_eq!(state_of(&rule), "applied");
    assert_eq!(state_of(&other), "applied");
    assert_eq!(state_of(&note), "proposed");
    let approvals: i64 = world
        .store()
        .conn()
        .query_row(
            "SELECT count(*) FROM approvals WHERE kind = 'memory' AND state = 'succeeded'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(approvals, 2);
    let text = std::fs::read_to_string(world.home().join("memory/projects/alpha.md")).unwrap();
    assert!(
        text.contains("Run the formatter before a commit."),
        "{text}"
    );
    assert!(!world.home().join("memory/projects/gamma.md").exists());

    // The same filter again finds nothing left: approved items are not redone.
    let again = world.must(&["mem", "approve", "--class", "rule"]);
    assert_eq!(again.data()["approved"], 0);
    assert!(results(&again).is_empty());

    // Named by id, an approved one is skipped and the open one goes.
    let named = world.must(&["mem", "approve", &rule, &note]);
    assert_eq!(
        results(&named),
        vec![
            (rule.clone(), "skipped".to_owned()),
            (note.clone(), "approved".to_owned())
        ]
    );
    assert_eq!(named.data()["skipped"], 1);
    assert_eq!(named.data()["approved"], 1);
}

#[test]
fn approve_filters_by_project_and_boundary() {
    let world = two_projects();
    let alpha = propose_line(
        &world,
        "alpha",
        "How we work here",
        "Run the formatter before a commit.",
    );
    let beta = propose_line(
        &world,
        "beta",
        "How we work here",
        "Never edit the vendored copy.",
    );
    open_gate(&world, &[&alpha, &beta]);

    let dry = world.must(&["mem", "approve", "--project", "beta", "--dry-run"]);
    assert_eq!(
        results(&dry),
        vec![(beta.clone(), "would_approve".to_owned())]
    );

    let work = world.must(&["mem", "approve", "--boundary", "work", "--dry-run"]);
    assert!(results(&work).is_empty());

    let personal = world.must(&[
        "mem",
        "approve",
        "--boundary",
        "personal",
        "--project",
        "alpha",
    ]);
    assert_eq!(
        results(&personal),
        vec![(alpha.clone(), "approved".to_owned())]
    );
    let after = states(&world);
    assert_eq!(
        after.iter().find(|(id, _)| *id == beta).unwrap().1,
        "proposed"
    );

    let wrong = world.run(&["mem", "approve", "--boundary", "office"]);
    assert_eq!(wrong.code, 2, "{}", wrong.json);
}

#[test]
fn approve_refuses_what_the_holdout_gate_holds_and_goes_on() {
    let world = two_projects();
    let held = propose_line(
        &world,
        "alpha",
        "How we work here",
        "Run the formatter before a commit.",
    );
    let ready = propose_line(
        &world,
        "beta",
        "How we work here",
        "Never edit the vendored copy.",
    );
    open_gate(&world, &[&ready]);

    let dry = world.must(&["mem", "approve", "--class", "rule", "--dry-run"]);
    assert_eq!(dry.data()["would_approve"], 1);
    assert_eq!(dry.data()["would_refuse"], 1);
    let reason = dry.data()["results"][0]["reason"].as_str().unwrap();
    assert!(
        reason.contains("no change set") && reason.contains("tiller learn eval"),
        "{reason}"
    );

    let done = world.must(&["mem", "approve", "--class", "rule"]);
    assert_eq!(done.data()["approved"], 1);
    assert_eq!(done.data()["refused"], 1);
    assert_eq!(
        results(&done),
        vec![
            (held.clone(), "refused".to_owned()),
            (ready.clone(), "approved".to_owned())
        ]
    );
    assert!(done.json["warning"].is_string(), "{}", done.json);
    let after = states(&world);
    assert_eq!(
        after.iter().find(|(id, _)| *id == held).unwrap().1,
        "proposed"
    );
    assert_eq!(
        after.iter().find(|(id, _)| *id == ready).unwrap().1,
        "applied"
    );
}

/// A Keep that would put a role's startup memory over the budget is refused
/// with the numbers, in the dry run and the run, and the line says them
/// plainly: the reason alone, since the line already names the proposal.
#[test]
fn approve_refuses_a_keep_over_the_startup_budget_with_the_numbers() {
    let world = two_projects();
    let long = propose_line(&world, "alpha", "How we work here", &"w".repeat(29_600));
    open_gate(&world, &[&long]);
    world.must(&["mem", "approve", &long]);
    let over = propose_line(&world, "alpha", "How we work here", &"o".repeat(600));
    open_gate(&world, &[&over]);

    let dry = world.must(&["mem", "approve", &over, "--dry-run"]);
    assert_eq!(dry.data()["would_refuse"], 1, "{}", dry.json);
    let reason = dry.data()["results"][0]["reason"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        reason.starts_with(
            "it would put every role's startup memory in alpha over the 7,500-token budget: "
        ) && reason.contains(" tokens now, ")
            && reason.contains(" from this change, ")
            && reason.ends_with(" after"),
        "{reason}"
    );

    let done = world.must(&["mem", "approve", &over]);
    assert_eq!(done.data()["refused"], 1, "{}", done.json);
    assert_eq!(done.data()["results"][0]["reason"], reason.as_str());
    assert_eq!(
        states(&world).iter().find(|(id, _)| *id == over).unwrap().1,
        "proposed"
    );
}

#[test]
fn approve_takes_several_proposals_for_one_document_in_one_pass() {
    let world = two_projects();
    let first = propose_line(
        &world,
        "alpha",
        "How we work here",
        "Run the formatter before a commit.",
    );
    let second = propose_line(
        &world,
        "alpha",
        "How we work here",
        "Keep commits small and focused.",
    );
    open_gate(&world, &[&first, &second]);

    let done = world.must(&["mem", "approve", "--project", "alpha"]);
    assert_eq!(done.data()["approved"], 2, "{}", done.json);
    let text = std::fs::read_to_string(world.home().join("memory/projects/alpha.md")).unwrap();
    assert!(
        text.contains("Run the formatter before a commit."),
        "{text}"
    );
    assert!(text.contains("Keep commits small and focused."), "{text}");
}

/// One `--key` names one request. A later run of the same command under that
/// key must not replay an earlier proposal's answer for a new proposal: each
/// proposal is decided under its own key, bound to its own id.
#[test]
fn approve_under_a_reused_key_never_replays_another_proposals_answer() {
    let world = two_projects();
    let first = propose_line(
        &world,
        "alpha",
        "How we work here",
        "Run the formatter before a commit.",
    );
    let second = propose_line(
        &world,
        "beta",
        "How we work here",
        "Never edit the vendored copy.",
    );
    open_gate(&world, &[&first, &second]);
    let done = world.must(&["mem", "approve", "--class", "rule", "--key", "k"]);
    assert_eq!(done.data()["approved"], 2, "{}", done.json);

    // A true retry of the same request replays and writes nothing more.
    let retry = world.must(&["mem", "approve", &first, &second, "--key", "k"]);
    assert_eq!(retry.data()["skipped"], 2, "{}", retry.json);

    // Later, only a new proposal is pending, and the owner runs the same
    // command with the same key.
    let third = propose_line(
        &world,
        "alpha",
        "How we work here",
        "Keep commits small and focused.",
    );
    world.must(&["learn", "bundle", "later", &third]);
    let result = world.dir.path().join("later.json");
    std::fs::write(
        &result,
        r#"{"aggregates": {"casesTotal": 4, "casesPassed": 4, "overallScore": 0.97}, "costUsd": 1.0, "partial": false}"#,
    )
    .unwrap();
    world.must(&[
        "learn",
        "eval",
        "later",
        "--result",
        result.to_str().unwrap(),
    ]);

    let again = world.must(&["mem", "approve", "--class", "rule", "--key", "k"]);
    assert_eq!(
        results(&again),
        vec![(third.clone(), "approved".to_owned())],
        "{}",
        again.json
    );
    assert_eq!(
        again.data()["results"][0]["proposal_id"],
        third,
        "{}",
        again.json
    );
    let after = states(&world);
    assert_eq!(
        after.iter().find(|(id, _)| *id == third).unwrap().1,
        "applied",
        "{}",
        again.json
    );
    let text = std::fs::read_to_string(world.home().join("memory/projects/alpha.md")).unwrap();
    assert!(text.contains("Keep commits small and focused."), "{text}");
}
