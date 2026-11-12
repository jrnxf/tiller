//! The daemon on the real terminal holder (`terminal-holder.md` 9.2): an
//! agent launched by `tiller` runs in a `tiller holder` pane, the board's
//! terminal endpoint serves it and types into it, and a daemon restart with
//! the holder kept up serves the same pane, with its history, again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Read;
use std::time::{Duration, Instant};

use common::holder::{View, WAIT, cookie, post, script, send, start_holder};
use common::{World, git_only_daemon};
use tiller_panes::{HolderClient, Live as _, PaneState, Panes};

/// A home whose orchestrator launch runs a plain shell: the `claude` the
/// launch line names answers `--version` and otherwise becomes `sh`.
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
    // No herdr runs: a launch that reached it would fail.
    world.set_env("TILLER_HERDR", "/nonexistent/herdr");
    let claude = root.join("claude-stub");
    script(
        &claude,
        "if [ \"$1\" = --version ]; then echo '2.1.0 (Claude Code)'; exit 0; fi\nPS1='$ ' exec /bin/sh\n",
    );
    world.must(&["config", "set", "spawn.claude", claude.to_str().unwrap()]);
}

/// Spec 9.2, the first test: a daemon restart with the holder kept up
/// serves the same pane, with its history, and it still takes input.
#[test]
fn a_daemon_restart_serves_the_same_holder_pane_with_its_history_and_input() {
    let world = World::initialized();
    launchable(&world);
    let (_holder, client) = start_holder(&world);

    let started = world.must(&["orchestrator", "start", "alpha"]);
    let session = started.data()["session"]["id"].as_str().unwrap().to_owned();
    let pane: String = world
        .store()
        .conn()
        .query_row(
            "SELECT pane_id FROM sessions WHERE id = ?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    assert!(tiller_core::ids::is_holder_pane(&pane), "{pane}");

    let cookie = cookie(&world);
    let (daemon, port) = git_only_daemon(&world);
    let mut view = View::open(port, &cookie, &session);
    view.until("size", |view| {
        view.lines.first().is_some_and(|l| l["type"] == "size")
    });
    // The shell computes the marker, so its output, not the typed line,
    // proves the input ran.
    send(
        port,
        &cookie,
        &session,
        &serde_json::json!({"text": "echo first-$((6*7))"}),
    );
    send(
        port,
        &cookie,
        &session,
        &serde_json::json!({"keys": ["enter"]}),
    );
    view.until("the first output", |view| view.shows("first-42"));
    drop(view);

    // The daemon only: the holder and its pane run on.
    drop(daemon);
    let (_daemon, port) = git_only_daemon(&world);
    let mut view = View::open(port, &cookie, &session);
    view.until("the repaint", |view| {
        view.lines
            .iter()
            .any(|l| l["type"] == "frame" && l["full"] == true)
    });
    assert!(view.shows("first-42"), "the repaint lost the history");
    send(
        port,
        &cookie,
        &session,
        &serde_json::json!({"text": "echo second-$((6*7))"}),
    );
    send(
        port,
        &cookie,
        &session,
        &serde_json::json!({"keys": ["enter"]}),
    );
    view.until("the second output", |view| view.shows("second-42"));

    // The same pane, never a second one for the session.
    let listing = client.list().unwrap();
    let ours: Vec<_> = listing.panes.iter().filter(|p| p.key == session).collect();
    assert_eq!(ours.len(), 1, "{:?}", listing.panes);
    assert_eq!(ours[0].pane.to_string(), pane);
    assert_eq!(ours[0].state, PaneState::Running);
}

/// The size of the session's pane in the holder now.
fn pane_size(client: &HolderClient, session: &str) -> (u16, u16) {
    let listing = client.list().unwrap();
    let pane = listing.panes.iter().find(|p| p.key == session).unwrap();
    (pane.cols, pane.rows)
}

/// The board's Fit on a holder pane: the PTY takes the board's size while
/// the view is open, and gets its own size back when the view ends.
#[test]
fn a_fit_sizes_the_holder_pane_and_the_view_gives_it_back_when_it_ends() {
    let world = World::initialized();
    launchable(&world);
    let (_holder, client) = start_holder(&world);
    let started = world.must(&["orchestrator", "start", "alpha"]);
    let session = started.data()["session"]["id"].as_str().unwrap().to_owned();
    let own = pane_size(&client, &session);
    assert_ne!(own, (100, 30));

    let cookie = cookie(&world);
    let (_daemon, port) = git_only_daemon(&world);
    let mut view = View::open_with(
        port,
        &cookie,
        &session,
        r#"{"fit": {"cols": 100, "rows": 30}}"#,
    );
    view.until("the Fit's size", |view| {
        view.lines
            .iter()
            .any(|l| l["type"] == "size" && l["cols"] == 100 && l["rows"] == 30)
    });
    assert!(
        !view.lines.iter().any(|l| l["type"] == "unfit"),
        "{:?}",
        view.lines
    );
    assert_eq!(pane_size(&client, &session), (100, 30));

    drop(view);
    let deadline = Instant::now() + WAIT;
    while pane_size(&client, &session) != own {
        assert!(
            Instant::now() < deadline,
            "the pane kept the Fit's size: {:?}",
            pane_size(&client, &session)
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Another holder client resizes `session`'s pane through its own lease, as
/// `tiller attach` will, and waits for the holder to take it. The holder
/// tells only that client, so the board's view never hears of it.
fn resize_as_another_client(client: &HolderClient, session: &str, cols: u16, rows: u16) {
    let listing = client.list().unwrap();
    let pane = listing
        .panes
        .iter()
        .find(|p| p.key == session)
        .unwrap()
        .pane
        .clone();
    let mut other = client
        .subscribe(&pane, tiller_panes::Subscribe::default())
        .unwrap();
    other.resize(cols, rows).unwrap();
    loop {
        match other.recv().unwrap() {
            Some(tiller_panes::Update::Resized { cols: c, rows: r }) => {
                assert_eq!((c, r), (cols, rows));
                break;
            }
            Some(_) => {}
            None => panic!("the other client's stream ended before its resize"),
        }
    }
}

/// Holds `pane_size` to `want` for a while after a view ends, so a give back
/// that came late would show.
fn keeps_size(client: &HolderClient, session: &str, want: (u16, u16)) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        assert_eq!(
            pane_size(client, session),
            want,
            "the give back overwrote the other client's size"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits for the session's pane to reach `want`.
fn reaches_size(client: &HolderClient, session: &str, want: (u16, u16)) {
    let deadline = Instant::now() + WAIT;
    while pane_size(client, session) != want {
        assert!(
            Instant::now() < deadline,
            "the pane is at {:?}, not {want:?}",
            pane_size(client, session)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The real holder tells no other subscriber of a resize, so the Fit reads
/// the pane's size itself: a size another client set during the Fit is
/// kept when the view ends, and is the one given back after a later refit.
#[test]
fn a_size_another_holder_client_sets_during_a_fit_is_kept_or_given_back() {
    let world = World::initialized();
    launchable(&world);
    let (_holder, client) = start_holder(&world);
    let started = world.must(&["orchestrator", "start", "alpha"]);
    let session = started.data()["session"]["id"].as_str().unwrap().to_owned();
    let cookie = cookie(&world);
    let (_daemon, port) = git_only_daemon(&world);

    // Another client sizes the pane during the Fit, and the view ends: the
    // pane keeps the other client's size.
    let mut view = View::open_with(
        port,
        &cookie,
        &session,
        r#"{"fit": {"cols": 100, "rows": 30}}"#,
    );
    view.until("the Fit's size", |view| {
        view.lines
            .iter()
            .any(|l| l["type"] == "size" && l["cols"] == 100 && l["rows"] == 30)
    });
    reaches_size(&client, &session, (100, 30));
    resize_as_another_client(&client, &session, 90, 25);
    drop(view);
    keeps_size(&client, &session, (90, 25));

    // Another client sizes the pane, the board refits, and the view ends:
    // the pane gets the other client's size back, not the one before it.
    let mut view = View::open_with(
        port,
        &cookie,
        &session,
        r#"{"fit": {"cols": 100, "rows": 30}}"#,
    );
    view.until("the second Fit's size", |view| {
        view.lines
            .iter()
            .any(|l| l["type"] == "size" && l["cols"] == 100 && l["rows"] == 30)
    });
    reaches_size(&client, &session, (100, 30));
    resize_as_another_client(&client, &session, 95, 28);
    let mut refit = post(
        port,
        &cookie,
        &format!("/api/v1/agents/{session}/terminal/fit"),
        r#"{"cols": 110, "rows": 35}"#,
    );
    let mut answer = String::new();
    refit.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.0 200"), "{answer}");
    reaches_size(&client, &session, (110, 35));
    drop(view);
    reaches_size(&client, &session, (95, 28));
    keeps_size(&client, &session, (95, 28));
}
