//! A launch loads the project's approved memory: the prompt holds the text
//! and `session_loads` names the revision (`memory.md` sections 2 and 7).
//! The real binary, one `state.db`, the memory repo under the home and a
//! herdr stand-in that answers the launch's calls.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::World;

/// An executable shell script at `path`.
fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A herdr that opens one workspace with one pane, shows an empty fleet and
/// takes every write.
const HERDR: &str = r#"case "$*" in
  *"workspace create"*) echo '{"result":{"workspace":{"workspace_id":"w1"},"root_pane":{"pane_id":"p1"}}}' ;;
  *"api snapshot"*) echo '{"result":{"workspaces":[],"panes":[]}}' ;;
  *) echo '{"result":{}}' ;;
esac
"#;

/// The places a launch writes, under the world's own directory, and the
/// stand-in herdr and claude.
fn launchable(world: &World) {
    let root = world.dir.path();
    let checkout = root.join("checkouts/alpha");
    std::fs::create_dir_all(&checkout).unwrap();
    world.must(&[
        "config",
        "set",
        "checkouts.alpha",
        checkout.to_str().unwrap(),
    ]);
    for (key, dir) in [
        ("spawn.pool_root", "pool"),
        ("spawn.scratch_root", "scratch"),
        ("spawn.orchestrator_home", "orchestrators"),
        ("spawn.checkouts_root", "checkouts"),
    ] {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        world.must(&["config", "set", key, dir.to_str().unwrap()]);
    }
    let herdr = root.join("herdr");
    script(&herdr, HERDR);
    world.set_env("TILLER_HERDR", herdr.to_str().unwrap());
    // No model runs: the claude the launch line names only answers --version.
    let claude = root.join("claude-stub");
    script(&claude, "echo '2.1.0 (Claude Code)'\n");
    world.must(&["config", "set", "spawn.claude", claude.to_str().unwrap()]);
}

/// One approved line in alpha's memory document, the way the owner adds it:
/// a note, a proposal, a passed holdout run and the approval.
fn approve_line(world: &World, text: &str) {
    let noted = world.must(&["mem", "note", "alpha", "--text", text]);
    let evidence = noted.data()["evidence_id"].as_str().unwrap().to_owned();
    let proposed = world.must(&[
        "mem",
        "propose",
        "alpha",
        "--add",
        "How we work here",
        "--text",
        text,
        "--evidence",
        &evidence,
        "--expected-effect",
        "sessions follow the line",
    ]);
    let proposal = proposed.data()["id"].as_str().unwrap().to_owned();
    world.must(&["learn", "bundle", "placement", &proposal]);
    let result = world.dir.path().join("result.json");
    std::fs::write(
        &result,
        r#"{"aggregates": {"casesTotal": 4, "casesPassed": 4, "overallScore": 0.97}, "costUsd": 1.0, "partial": false}"#,
    )
    .unwrap();
    world.must(&[
        "learn",
        "eval",
        "placement",
        "--result",
        result.to_str().unwrap(),
    ]);
    let approved = world.must(&["mem", "approve", &proposal]);
    assert_eq!(approved.data()["approved"], 1, "{}", approved.json);
}

/// The `(revision, trigger)` rows of `session`.
fn loads(world: &World, session: &str) -> Vec<(String, String)> {
    world
        .store()
        .conn()
        .prepare(
            "SELECT revision_id, trigger FROM session_loads WHERE session_id = ?1
             ORDER BY loaded_at, revision_id",
        )
        .unwrap()
        .query_map([session], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// The current approved revision of alpha's memory document.
fn approved_revision(world: &World) -> String {
    world
        .store()
        .conn()
        .query_row(
            "SELECT r.id FROM memory_revisions r JOIN memory_sources s ON s.id = r.source_id
             WHERE s.project_id = 'alpha' AND s.canonical_path = 'projects/alpha.md'
               AND r.review = 'approved'
             ORDER BY r.recorded_at DESC, r.id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_launch_loads_the_approved_memory_into_the_prompt_and_the_loads() {
    let world = World::initialized();
    launchable(&world);
    approve_line(&world, "Run the formatter before a commit.");
    let revision = approved_revision(&world);

    let started = world.must(&["orchestrator", "start", "alpha"]);
    let session = started.data()["session"]["id"].as_str().unwrap().to_owned();

    let prompt = std::fs::read_to_string(
        world
            .dir
            .path()
            .join("scratch")
            .join(&session)
            .join("prompt.md"),
    )
    .unwrap();
    assert!(
        prompt.contains("Run the formatter before a commit."),
        "{prompt}"
    );
    assert!(prompt.contains("# Memory"), "{prompt}");
    assert_eq!(
        loads(&world, &session),
        vec![(revision.clone(), "startup".to_owned())]
    );

    // The appended prompt survives `/clear`, `/compact` and a resume, so
    // each SessionStart observation copies the set under its trigger. The
    // startup bind writes nothing: the launch already did.
    for source in ["startup", "clear", "compact", "resume"] {
        world.hook(
            Some(&session),
            "SessionStart",
            &format!(r#"{{"session_id":"claude-orchestrator","source":"{source}"}}"#),
        );
        // Each copy gets its own `loaded_at`.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        loads(&world, &session),
        ["startup", "clear", "compact", "resume"]
            .map(|trigger| (revision.clone(), trigger.to_owned()))
            .to_vec()
    );
}
