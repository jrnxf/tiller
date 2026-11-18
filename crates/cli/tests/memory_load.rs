//! A launch loads the kb notes in the project's scopes: the prompt holds
//! them and `session_notes` names each one (`memory.md` sections 2 and 7).
//! A pending note reaches no prompt. The real binary, one `state.db`, a kb
//! store in a temp dir and a terminal holder that takes the launch.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use common::holder::start_holder;
use common::{World, git_only_daemon};

const WAIT: Duration = Duration::from_secs(30);

/// An executable shell script at `path`.
fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The places a launch writes, under the world's own directory, and the
/// stand-in claude.
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
    // No model runs: the claude the launch line names only answers --version.
    let claude = root.join("claude-stub");
    script(&claude, "echo '2.1.0 (Claude Code)'\n");
    world.must(&["config", "set", "spawn.claude", claude.to_str().unwrap()]);
}

/// A kb store under the world's directory with one global note, set as
/// `KB_HOME` for every later command. Never the owner's own store.
fn kb_store(world: &World) {
    let kb = world.dir.path().join("kb");
    let notes = kb.join("notes/global");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("run-the-formatter.md"),
        "---\nid: run-the-formatter\ntitle: Run the formatter before a commit\nscope: global\n\
         kind: convention\ncreated: 2026-11-10T09:00:00Z\nlast_verified: 2026-11-10T09:00:00Z\n\
         confidence: medium\nindexed: true\n---\nThe gate fails on an unformatted file.\n",
    )
    .unwrap();
    world.set_env("KB_HOME", kb.to_str().unwrap());
}

/// The `(note, trigger)` rows of `session`.
fn loads(world: &World, session: &str) -> Vec<(String, String)> {
    world
        .store()
        .conn()
        .prepare(
            "SELECT note_id, trigger FROM session_notes WHERE session_id = ?1
             ORDER BY loaded_at, note_id",
        )
        .unwrap()
        .query_map([session], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// A pending note's text, which must reach no fleet surface.
const CANARY: &str = "canary-7f3c pending text";

#[test]
fn a_launch_loads_the_kb_notes_into_the_prompt_and_the_loads() {
    let world = World::initialized();
    launchable(&world);
    let (_holder, _) = start_holder(&world);
    kb_store(&world);
    // A pending note waits for the owner.
    world
        .store()
        .conn()
        .execute(
            "INSERT INTO memory_pending (id, project_id, kind, title, body, source, created_at)
             VALUES ('01JC00000000000000000000N1', 'alpha', 'gotcha', ?1, ?1, 'migration',
                     '2026-11-10T09:00:00.000Z')",
            [CANARY],
        )
        .unwrap();
    let note = "run-the-formatter".to_owned();

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
        prompt.contains("Run the formatter before a commit"),
        "{prompt}"
    );
    assert!(prompt.contains("\n# Memory\n"), "{prompt}");
    assert!(!prompt.contains("canary-7f3c"), "{prompt}");
    assert_eq!(
        loads(&world, &session),
        vec![(note.clone(), "startup".to_owned())]
    );

    // The appended prompt survives `/clear`, `/compact` and a resume. The
    // hook only records each SessionStart as an observation: the startup
    // row came from the launch, and the copies are the daemon's work.
    for source in ["startup", "clear", "compact", "resume"] {
        world.hook(
            Some(&session),
            "SessionStart",
            &format!(r#"{{"session_id":"claude-orchestrator","source":"{source}"}}"#),
        );
        // Each observation gets its own `observed_at`.
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        loads(&world, &session),
        vec![(note.clone(), "startup".to_owned())]
    );

    // The daemon copies the set under each later trigger, once.
    let _daemon = git_only_daemon(&world);
    let want = ["startup", "clear", "compact", "resume"]
        .map(|trigger| (note.clone(), trigger.to_owned()))
        .to_vec();
    let deadline = Instant::now() + WAIT;
    while loads(&world, &session) != want {
        assert!(
            Instant::now() < deadline,
            "the daemon never copied the loads: {:?}",
            loads(&world, &session)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
