//! `tiller holder` end to end: the real binary on a throwaway home, driven
//! through `HolderClient` as the daemon drives it. Each test's holder stops
//! when its `Running` guard drops, and its panes' children with it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{Running, World};
use tiller_panes::frame::{Frame, read_frame, write_frame};
use tiller_panes::message::ErrorCode;
use tiller_panes::{
    Exit, HolderClient, Meta, Pane, PaneState, Panes, PanesError, Size, Spawn, StreamHandle,
    Subscribe, Update, socket_path,
};

/// How long a test waits for anything the holder does.
const WAIT: Duration = Duration::from_secs(10);

/// A subscription's updates, read on a thread so a wait can give up. The
/// subscription ends when this drops.
struct Updates {
    rx: mpsc::Receiver<Update>,
    handle: StreamHandle,
}

impl Drop for Updates {
    fn drop(&mut self) {
        self.handle.end();
    }
}

/// A running holder on `world`'s home, and a client of it.
struct Holder {
    running: Running,
    client: HolderClient,
    socket: PathBuf,
    log: PathBuf,
}

impl Holder {
    fn start(world: &World) -> Self {
        let log = world.dir.path().join("holder.log");
        let running = Running::new(
            world
                .bare_command()
                .arg("holder")
                .env("TILLER_LOG", "debug")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&log).unwrap())
                .spawn()
                .unwrap(),
        );
        let socket = socket_path(&world.home());
        let deadline = Instant::now() + WAIT;
        let client = loop {
            match HolderClient::connect(&socket, "daemon") {
                Ok(client) => break client,
                Err(err) if Instant::now() < deadline => {
                    let _ = err;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => panic!(
                    "the holder did not listen: {err}\n{}",
                    std::fs::read_to_string(&log).unwrap_or_default()
                ),
            }
        };
        Self {
            running,
            client,
            socket,
            log,
        }
    }

    fn spawn(&self, world: &World, argv: &[&str]) -> Pane {
        self.client.spawn(&spawn_of(world, argv)).unwrap()
    }

    /// The pane's updates on a new subscription.
    fn updates(&self, pane: &Pane) -> Updates {
        let mut subscription = self
            .client
            .subscribe(&pane.pane, Subscribe::default())
            .unwrap();
        let handle = subscription.handle();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(update)) = subscription.recv() {
                if tx.send(update).is_err() {
                    break;
                }
            }
        });
        Updates { rx, handle }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn spawn_of(world: &World, argv: &[&str]) -> Spawn {
    Spawn {
        key: format!("key-{}", argv.join(" ")),
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: world.dir.path().display().to_string(),
        env: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
        size: None,
        meta: Meta::default(),
    }
}

/// The next update, or a panic naming `what` when none comes in time.
fn next(updates: &Updates, holder: &Holder, what: &str) -> Update {
    updates
        .rx
        .recv_timeout(WAIT)
        .unwrap_or_else(|err| panic!("no {what} ({err})\n{}", holder.log()))
}

/// Waits for the pane's exit on a new subscription: after the repaint when
/// it has exited already, or live.
fn exit_of(holder: &Holder, pane: &Pane) -> Exit {
    let updates = holder.updates(pane);
    loop {
        if let Update::Exited(exit) = next(&updates, holder, "exit") {
            return exit;
        }
    }
}

#[test]
fn a_shell_runs_in_the_holder_and_its_output_shows_in_the_repaint_and_live() {
    let world = World::new();
    let holder = Holder::start(&world);
    let welcome = holder.client.welcome();
    assert_eq!(welcome.proto, 1);
    assert!(welcome.features.is_empty(), "{welcome:?}");

    // One socket per home, only for its owner.
    let run = world.home().join("run");
    let mode = |path: &PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&run), 0o700);
    assert_eq!(mode(&holder.socket), 0o600);

    let pane = holder.spawn(&world, &["/bin/sh"]);
    assert_eq!(pane.state, PaneState::Running);
    assert_eq!((pane.cols, pane.rows), (120, 40));

    // The typed line holds `$((6*7))`; only the shell's output holds 42.
    holder
        .client
        .raw_input(pane.num, b"echo repaint-$((6*7))\n")
        .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let updates = holder.updates(&pane);
        let Update::Repaint { cols, rows, bytes } = next(&updates, &holder, "repaint") else {
            panic!("a subscription began with something else than its repaint");
        };
        assert_eq!((cols, rows), (120, 40));
        if String::from_utf8_lossy(&bytes).contains("repaint-42") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the repaint never showed the output: {:?}",
            String::from_utf8_lossy(&bytes)
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Live output follows the repaint.
    let updates = holder.updates(&pane);
    assert!(matches!(
        next(&updates, &holder, "repaint"),
        Update::Repaint { .. }
    ));
    holder
        .client
        .raw_input(pane.num, b"echo live-$((40+2))\n")
        .unwrap();
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("live-42") {
        match next(&updates, &holder, "live output") {
            Update::Output(bytes) => seen.extend(bytes),
            other => panic!("unexpected {other:?}"),
        }
    }

    // A second spawn with the key of a running pane starts nothing.
    let again = holder.spawn(&world, &["/bin/sh"]);
    assert_eq!(again.pane, pane.pane);
    let listing = holder.client.list().unwrap();
    assert_eq!(listing.instance, welcome.instance);
    assert_eq!(listing.panes.len(), 1);

    // Only the live subscription counts: the ended ones drop out once the
    // holder reads their close.
    let deadline = Instant::now() + WAIT;
    loop {
        let subscribers = holder.client.list().unwrap().panes[0].subscribers;
        if subscribers == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{subscribers} subscribers, not 1\n{}",
            holder.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(updates);
}

#[test]
fn an_exit_code_and_a_killing_signal_are_reported() {
    let world = World::new();
    let holder = Holder::start(&world);

    let three = holder.spawn(&world, &["/bin/sh", "-c", "echo bye; exit 3"]);
    let exit = exit_of(&holder, &three);
    assert_eq!((exit.code, exit.signal), (Some(3), None), "{exit:?}");

    let killed = holder.spawn(&world, &["/bin/sh", "-c", "kill -KILL $$"]);
    let exit = exit_of(&holder, &killed);
    assert_eq!((exit.code, exit.signal), (None, Some(9)), "{exit:?}");

    // The exited panes keep their records, and the last screen.
    let listing = holder.client.list().unwrap();
    for pane in [&three, &killed] {
        let record = listing
            .panes
            .iter()
            .find(|record| record.pane == pane.pane)
            .unwrap();
        assert_eq!(record.state, PaneState::Exited);
    }
    let updates = holder.updates(&three);
    let Update::Repaint { bytes, .. } = next(&updates, &holder, "repaint") else {
        panic!("no repaint first");
    };
    assert!(String::from_utf8_lossy(&bytes).contains("bye"));
    assert!(matches!(next(&updates, &holder, "exit"), Update::Exited(_)));
}

#[test]
fn a_second_holder_on_the_same_home_names_the_first_and_exits_0() {
    let world = World::new();
    let holder = Holder::start(&world);
    let mut second = Running::new(
        world
            .bare_command()
            .arg("holder")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the second holder kept running");
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut second.stderr.take().unwrap(), &mut stderr).unwrap();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "a holder already runs (pid {})",
            holder.running.id()
        )),
        "{stderr}"
    );
    // The first one still serves.
    holder.client.list().unwrap();
}

#[test]
fn a_client_with_no_shared_version_is_refused() {
    let world = World::new();
    let holder = Holder::start(&world);
    let mut socket = std::os::unix::net::UnixStream::connect(&holder.socket).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    let hello = r#"{"type":"hello","proto":[7,9],"client":"daemon","version":"9.0.0"}"#;
    write_frame(&mut socket, &Frame::Message(hello.into())).unwrap();
    let Some(Frame::Message(reply)) = read_frame(&mut socket).unwrap() else {
        panic!("no kind 0 reply");
    };
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["type"], "refused", "{reply}");
    assert_eq!(reply["proto"], serde_json::json!([1, 1]), "{reply}");
    // And the holder closes the connection.
    assert!(read_frame(&mut socket).unwrap().is_none());
}

#[test]
fn input_for_an_exited_pane_is_dropped_and_holds_up_nothing() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, &["/bin/sh", "-c", "exit 0"]);
    exit_of(&holder, &pane);
    // Far more than the PTY and the input queue take: were any of it
    // waiting for the gone app, the list below would wait too.
    let chunk = vec![b'x'; 64 * 1024];
    for _ in 0..512 {
        holder.client.raw_input(pane.num, &chunk).unwrap();
    }
    let started = Instant::now();
    let listing = holder.client.list().unwrap();
    assert!(started.elapsed() < WAIT, "{:?}", started.elapsed());
    assert_eq!(listing.panes[0].state, PaneState::Exited);
}

#[test]
fn a_size_out_of_bounds_is_a_bad_request() {
    let world = World::new();
    let holder = Holder::start(&world);
    for (cols, rows) in [(1, 40), (120, 0), (5000, 40), (120, 5000)] {
        let spawn = Spawn {
            size: Some(Size { cols, rows }),
            ..spawn_of(&world, &["/bin/sh"])
        };
        let err = holder.client.spawn(&spawn).unwrap_err();
        assert!(
            matches!(
                err,
                PanesError::Refused {
                    code: ErrorCode::BadRequest,
                    ..
                }
            ),
            "{cols} by {rows}: {err}"
        );
    }
    assert!(holder.client.list().unwrap().panes.is_empty());
}

#[test]
fn input_past_the_cap_for_an_app_that_reads_none_cuts_that_client_only() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, &["/bin/sh", "-c", "sleep 30"]);
    // Whole lines, so the PTY's line buffer fills and then takes no more.
    let chunk = b"0123456789abcdef\n".repeat(4 * 1024);
    // About 6.6 MiB: past the PTY's buffer and the client's 4 MiB cap.
    for _ in 0..100 {
        if holder.client.raw_input(pane.num, &chunk).is_err() {
            break;
        }
    }
    let err = holder.client.list().unwrap_err();
    assert!(matches!(err, PanesError::Unreachable(_)), "{err}");
    assert!(holder.log().contains("was cut"), "{}", holder.log());
    // Another client is served, and the pane runs on.
    let other = HolderClient::connect(&holder.socket, "daemon").unwrap();
    let listing = other.list().unwrap();
    assert_eq!(listing.panes[0].state, PaneState::Running);
}

#[test]
fn a_client_cut_for_a_stuck_pane_leaves_other_clients_and_panes_alone() {
    let world = World::new();
    let holder = Holder::start(&world);
    let stuck = holder.spawn(&world, &["/bin/sh", "-c", "sleep 30"]);
    let reading = holder.spawn(&world, &["/bin/cat"]);
    let updates = holder.updates(&reading);
    assert!(matches!(
        next(&updates, &holder, "repaint"),
        Update::Repaint { .. }
    ));

    // Two clients each leave 3 MiB unread in the stuck pane: past the cap
    // together, but each under it, so neither is cut.
    let flooder = HolderClient::connect(&holder.socket, "daemon").unwrap();
    let chunk = b"0123456789abcdef\n".repeat(4 * 1024);
    for client in [&flooder, &holder.client] {
        for _ in 0..45 {
            client.raw_input(stuck.num, &chunk).unwrap();
        }
    }
    flooder.list().unwrap();
    holder.client.list().unwrap();
    // The flooder goes on past its own cap and is cut for it alone.
    for _ in 0..100 {
        if flooder.raw_input(stuck.num, &chunk).is_err() {
            break;
        }
    }
    let err = flooder.list().unwrap_err();
    assert!(matches!(err, PanesError::Unreachable(_)), "{err}");
    // The other client's input to the stuck pane still counts as its own.
    holder.client.raw_input(stuck.num, &chunk).unwrap();
    holder.client.list().unwrap();

    // Its input to the pane that reads still goes through.
    holder
        .client
        .raw_input(reading.num, b"cross-pane\n")
        .unwrap();
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("cross-pane") {
        match next(&updates, &holder, "the reading pane's output") {
            Update::Output(bytes) => seen.extend(bytes),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(holder.client.list().unwrap().panes.len(), 2);
    assert_eq!(
        holder.log().matches("was cut").count(),
        1,
        "{}",
        holder.log()
    );
    drop(updates);
}

#[test]
fn a_db_other_than_the_homes_is_refused() {
    let world = World::new();
    let output = world
        .bare_command()
        .args(["--db", "/home/user/elsewhere.db", "holder"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("do not apply to the holder"), "{stderr}");
    let home = world.home().display().to_string();
    assert!(stderr.contains(&home), "{stderr}");
    assert!(!socket_path(&world.home()).exists());
}

#[test]
fn a_subscriber_that_reads_all_the_time_on_a_flooding_pane_is_not_cut() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, &["/usr/bin/yes"]);
    let updates = holder.updates(&pane);
    assert!(matches!(
        next(&updates, &holder, "repaint"),
        Update::Repaint { .. }
    ));
    // The reader thread takes every frame as it comes, so its queue never
    // fills: live output keeps coming, and no cut.
    let mut seen = 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match updates.rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Update::Output(bytes)) => seen += bytes.len(),
            Ok(other) => panic!("unexpected {other:?} after {seen} bytes\n{}", holder.log()),
            Err(err) => panic!("no output for 1 s ({err}) after {seen} bytes\n{}", holder.log()),
        }
    }
    assert!(seen > 1024 * 1024, "only {seen} bytes in 3 s");
    assert!(!holder.log().contains("was cut"), "{}", holder.log());
    drop(updates);
}
