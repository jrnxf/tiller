//! `tiller agent restart --stale`: the dry run lists the sessions that run an
//! older tiller and writes nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::World;

#[test]
fn the_dry_run_lists_the_older_sessions_and_writes_nothing() {
    let world = World::initialized();
    // Started before launches recorded a version: counted as older.
    let orchestrator = world.orchestrator();
    let seq = world.store().last_seq().unwrap();

    let out = world.must(&["agent", "restart", "--stale", "--dry-run"]);
    assert_eq!(out.data()["dry_run"], true);
    assert_eq!(out.data()["tiller_version"], env!("CARGO_PKG_VERSION"));
    let sessions = out.data()["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{}", out.json);
    let row = &sessions[0];
    assert_eq!(row["session"], orchestrator.as_str());
    assert_eq!(row["role"], "orchestrator");
    assert_eq!(row["started_on"], "unknown");
    assert_eq!(row["restart_queued"], false);
    assert!(row["action"].is_string());
    assert_eq!(
        world.store().last_seq().unwrap(),
        seq,
        "a dry run writes nothing"
    );

    // A version alone does not clear it: the launch hash is still missing.
    world
        .store()
        .conn()
        .execute(
            "UPDATE sessions SET tiller_version = ?1 WHERE id = ?2",
            rusqlite::params![env!("CARGO_PKG_VERSION"), orchestrator],
        )
        .unwrap();
    let out = world.must(&["agent", "restart", "--stale", "--dry-run"]);
    assert_eq!(out.data()["sessions"].as_array().unwrap().len(), 1);
}

#[test]
fn a_restart_needs_the_stale_flag_and_the_owner() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();

    let out = world.run(&["agent", "restart"]);
    assert_eq!((out.code, out.error_code()), (2, "usage"), "{}", out.json);
    let out = world.run_as(&orchestrator, &["agent", "restart", "--stale", "--dry-run"]);
    assert_eq!(
        (out.code, out.error_code()),
        (6, "forbidden"),
        "{}",
        out.json
    );
}
