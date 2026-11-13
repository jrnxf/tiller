//! `tiller run show --tail N` reads the last N lines of the run's live pane
//! through the terminal holder, without terminal escapes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use common::holder::{WAIT, start_holder};
use common::{Running, World};
use tiller_panes::{Format, PaneId, Panes, Read, Spawn};

/// A `tiller holder` in the world's home with one pane that prints `screen`
/// and waits. Returns the holder and the pane, once the pane shows `last`.
fn holder_showing(world: &World, screen: &str, last: &str) -> (Running, PaneId) {
    let (holder, client) = start_holder(world);
    let screen_file = world.dir.path().join("screen.txt");
    std::fs::write(&screen_file, screen).unwrap();
    let pane = client
        .spawn(&Spawn {
            key: "tail".to_owned(),
            argv: ["/bin/sh", "-c", "stty -echo; cat \"$0\"; read line"]
                .map(str::to_owned)
                .into_iter()
                .chain([screen_file.display().to_string()])
                .collect(),
            cwd: world.dir.path().display().to_string(),
            env: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
            size: None,
            meta: Default::default(),
        })
        .unwrap()
        .pane;
    let deadline = Instant::now() + WAIT;
    loop {
        let text = client
            .screen(&pane, Read::Visible, Format::Text)
            .unwrap()
            .text();
        if text.contains(last) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the pane never showed {last:?}: {text}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (holder, pane)
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
    let mut screen = String::new();
    for n in 1..=60 {
        screen.push_str(&format!(
            "\u{1b}[1;32mline {n}\u{1b}[0m\u{1b}]0;title\u{7}\n"
        ));
    }
    screen.push_str("\n   \n");
    let (_holder, pane) = holder_showing(&world, &screen, "line 60");
    bind_pane(&world.db(), &run, pane.as_str());

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
    let (_holder, _) = holder_showing(&world, "never read\n", "never read");

    // The session has no pane yet.
    let out = world.must(&["run", "show", &run, "--tail", "5"]);
    let tail = &out.data()["pane_tail"];
    assert_eq!(tail["available"], false);
    assert!(tail["lines"].is_null());
    assert_eq!(tail["reason"], "the session has no pane");

    // A pane the holder does not have.
    bind_pane(&world.db(), &run, PaneId::generate().as_str());
    let out = world.must(&["run", "show", &run, "--tail", "5"]);
    let tail = &out.data()["pane_tail"];
    assert_eq!(tail["available"], false);
    assert!(tail["lines"].is_null());
    assert!(
        tail["reason"]
            .as_str()
            .unwrap()
            .starts_with("the terminal holder cannot read the pane")
    );
}

#[test]
fn tail_of_an_ended_run_has_no_live_session() {
    let world = World::initialized();
    let card = card(&world);
    let (run, _) = world.builder_run(&card);
    bind_pane(&world.db(), &run, PaneId::generate().as_str());
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
    bind_pane(&world.db(), &run, PaneId::generate().as_str());

    let out = world.must(&["run", "show", &run]);

    assert_eq!(
        out.data()["pane_tail"],
        serde_json::json!({"requested": 0, "lines": null, "available": false})
    );
}
