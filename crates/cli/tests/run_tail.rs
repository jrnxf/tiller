//! `tiller run show --tail N` reads the last N lines of the run's live pane
//! through herdr, without terminal escapes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::World;

/// A herdr stand-in that answers `pane read` for `w1:p1` with `screen` and
/// refuses any other pane.
fn herdr_showing(world: &World, screen: &str) {
    let screen_file = world.dir.path().join("screen.txt");
    std::fs::write(&screen_file, screen).unwrap();
    let program = world.dir.path().join("herdr");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\n\
             if [ \"$1 $2 $3\" = 'pane read w1:p1' ]; then cat '{}'; exit 0; fi\n\
             echo 'pane not found' >&2\nexit 1\n",
            screen_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    world.set_env("TILLER_HERDR", program.to_str().unwrap());
}

fn bind_pane(db: &Path, run: &str, pane: &str) {
    rusqlite::Connection::open(db)
        .unwrap()
        .execute(
            "UPDATE sessions SET pane_id = ?2 WHERE run_id = ?1 AND ended_at IS NULL",
            rusqlite::params![run, pane],
        )
        .unwrap();
}

fn card(world: &World) -> String {
    world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "Tail",
    ]);
    "alpha-1".into()
}

#[test]
fn tail_is_the_last_lines_of_the_live_pane_without_escapes() {
    let world = World::initialized();
    let card = card(&world);
    let (run, _) = world.builder_run(&card);
    bind_pane(&world.db(), &run, "w1:p1");
    let mut screen = String::new();
    for n in 1..=60 {
        screen.push_str(&format!(
            "\u{1b}[1;32mline {n}\u{1b}[0m\u{1b}]0;title\u{7}\n"
        ));
    }
    screen.push_str("\n   \n");
    herdr_showing(&world, &screen);

    let out = world.must(&["run", "show", &run, "--tail", "5"]);

    let tail = &out.data()["pane_tail"];
    assert_eq!(tail["available"], true, "{tail}");
    assert_eq!(tail["requested"], 5);
    assert_eq!(
        tail["lines"],
        serde_json::json!(["line 56", "line 57", "line 58", "line 59", "line 60"])
    );
    assert!(tail.get("reason").is_none());
}

#[test]
fn tail_without_a_live_pane_is_unavailable_with_a_reason() {
    let world = World::initialized();
    let card = card(&world);
    let (run, _) = world.builder_run(&card);
    herdr_showing(&world, "never read\n");

    // The session has no pane yet.
    let out = world.must(&["run", "show", &run, "--tail", "5"]);
    let tail = &out.data()["pane_tail"];
    assert_eq!(tail["available"], false);
    assert!(tail["lines"].is_null());
    assert_eq!(tail["reason"], "the session has no pane");

    // A pane herdr no longer has.
    bind_pane(&world.db(), &run, "w1:p9");
    let out = world.must(&["run", "show", &run, "--tail", "5"]);
    let tail = &out.data()["pane_tail"];
    assert_eq!(tail["available"], false);
    assert!(tail["lines"].is_null());
    assert!(
        tail["reason"]
            .as_str()
            .unwrap()
            .starts_with("herdr cannot read the pane")
    );
}

#[test]
fn tail_of_an_ended_run_has_no_live_session() {
    let world = World::initialized();
    let card = card(&world);
    let (run, _) = world.builder_run(&card);
    bind_pane(&world.db(), &run, "w1:p1");
    herdr_showing(&world, "text\n");
    rusqlite::Connection::open(world.db())
        .unwrap()
        .execute(
            "UPDATE sessions SET ended_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE run_id = ?1",
            [&run],
        )
        .unwrap();

    let out = world.must(&["run", "show", &run, "--tail", "5"]);

    let tail = &out.data()["pane_tail"];
    assert_eq!(tail["available"], false);
    assert_eq!(tail["reason"], "the run has no live session");
}

#[test]
fn no_tail_flag_leaves_the_pane_unread() {
    let world = World::initialized();
    let card = card(&world);
    let (run, _) = world.builder_run(&card);
    bind_pane(&world.db(), &run, "w1:p1");
    herdr_showing(&world, "text\n");

    let out = world.must(&["run", "show", &run]);

    assert_eq!(
        out.data()["pane_tail"],
        serde_json::json!({"requested": 0, "lines": null, "available": false})
    );
}
