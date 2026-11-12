//! `tiller holder` end to end: the real binary on a throwaway home, driven
//! through `HolderClient` as the daemon drives it. Each test's holder stops
//! when its `Running` guard drops, and its panes' children with it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{Running, World};
use tiller_panes::frame::{Frame, read_frame, write_frame};
use tiller_panes::handshake::Hello;
use tiller_panes::message::{ErrorCode, Request, SyncMode};
use tiller_panes::{
    Event, Exit, Format, HolderClient, Key, Live, Meta, Pane, PaneId, PaneState, Panes, PanesError,
    Read, Signal, Size, Spawn, StreamHandle, Subscribe, Update, socket_path,
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
    // Then the end: the reader's recv gives None, so its sender drops.
    assert_eq!(
        updates.rx.recv_timeout(WAIT).unwrap_err(),
        mpsc::RecvTimeoutError::Disconnected,
        "the stream did not end after the exit"
    );
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
            Err(err) => panic!(
                "no output for 1 s ({err}) after {seen} bytes\n{}",
                holder.log()
            ),
        }
    }
    assert!(seen > 1024 * 1024, "only {seen} bytes in 3 s");
    assert!(!holder.log().contains("was cut"), "{}", holder.log());
    drop(updates);
}

/// The holder's events, read on a thread so a wait can give up.
struct Watch {
    rx: mpsc::Receiver<Event>,
}

impl Watch {
    fn start(holder: &Holder) -> Watch {
        let mut events = holder.client.watch().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(event)) = events.recv() {
                if tx.send(event).is_err() {
                    break;
                }
            }
        });
        Watch { rx }
    }

    /// The first event `wanted` takes, past any others.
    fn until(&self, holder: &Holder, what: &str, wanted: impl Fn(&Event) -> bool) -> Event {
        loop {
            let event = self
                .rx
                .recv_timeout(WAIT)
                .unwrap_or_else(|err| panic!("no {what} event ({err})\n{}", holder.log()));
            if wanted(&event) {
                return event;
            }
        }
    }
}

/// Reads the pane's visible text until a row passes `done`.
fn screen_until(
    holder: &Holder,
    pane: &Pane,
    what: &str,
    done: impl Fn(&str) -> bool,
) -> Vec<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let rows = holder
            .client
            .screen(&pane.pane, Read::Visible, Format::Text)
            .unwrap()
            .rows_text;
        if rows.iter().any(|row| done(row)) {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "no {what} on the screen: {rows:#?}\n{}",
            holder.log()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Reads the pane's visible text until a row is `row`.
fn shows(holder: &Holder, pane: &Pane, row: &str) -> Vec<String> {
    screen_until(holder, pane, row, |seen| seen == row)
}

fn refused(err: &PanesError) -> ErrorCode {
    match err {
        PanesError::Refused { code, .. } => code.clone(),
        other => panic!("not refused: {other}"),
    }
}

fn keys(names: &[&str]) -> Vec<Key> {
    names.iter().map(|name| name.parse().unwrap()).collect()
}

#[test]
fn get_and_screen_read_a_pane_in_text_ansi_and_with_its_scrollback() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, &["/bin/sh"]);
    let got = holder.client.get(&pane.pane).unwrap().unwrap();
    assert_eq!((got.num, got.state), (pane.num, PaneState::Running));
    assert!(
        holder
            .client
            .get(&PaneId::from("p_none"))
            .unwrap()
            .is_none()
    );

    holder
        .client
        .input(&pane.pane, "printf '\\033[31mred\\033[0m %s\\n' $((6*7))\n")
        .unwrap();
    shows(&holder, &pane, "red 42");
    let ansi = holder
        .client
        .screen(&pane.pane, Read::Visible, Format::Ansi)
        .unwrap();
    assert!(
        ansi.rows_text
            .iter()
            .any(|row| row == "\x1b[0m\x1b[31mred\x1b[0m 42"),
        "{:#?}",
        ansi.rows_text
    );
    assert_eq!((ansi.cols, ansi.rows), (120, 40));
    assert_eq!(ansi.rows_text.len(), 40);

    // 60 more lines scroll the first ones off the 40-row screen.
    holder
        .client
        .input(
            &pane.pane,
            "i=0; while [ $i -lt 60 ]; do i=$((i+1)); echo line-$i; done\n",
        )
        .unwrap();
    let visible = shows(&holder, &pane, "line-60");
    assert!(!visible.iter().any(|row| row == "line-1"), "{visible:#?}");
    let recent = holder
        .client
        .screen(&pane.pane, Read::Recent(100), Format::Text)
        .unwrap()
        .rows_text;
    assert!(recent.len() > 40, "{recent:#?}");
    assert!(recent.iter().any(|row| row == "line-1"), "{recent:#?}");
    let last = holder
        .client
        .screen(&pane.pane, Read::Recent(2), Format::Text)
        .unwrap()
        .rows_text;
    assert_eq!(last.len(), 2, "{last:#?}");

    let err = holder
        .client
        .screen(&pane.pane, Read::Recent(0), Format::Text)
        .unwrap_err();
    assert_eq!(refused(&err), ErrorCode::BadRequest, "{err}");
    let err = holder
        .client
        .screen(&PaneId::from("p_none"), Read::Visible, Format::Text)
        .unwrap_err();
    assert!(matches!(err, PanesError::NotFound(_)), "{err}");
}

#[test]
fn input_paste_and_keys_reach_the_app_in_the_form_its_modes_ask_for() {
    let world = World::new();
    let holder = Holder::start(&world);
    // `cat -v` shows each escape it gets as `^[`. The terminal echoes
    // nothing, so each row is what the app read.
    let plain = holder.spawn(
        &world,
        &["/bin/sh", "-c", "stty -echo; echo ready; exec cat -v"],
    );
    shows(&holder, &plain, "ready");
    holder.client.input(&plain.pane, "typed\n").unwrap();
    shows(&holder, &plain, "typed");
    // No bracketed paste asked for: the text goes as it is.
    holder.client.paste(&plain.pane, "plain\n").unwrap();
    shows(&holder, &plain, "plain");
    holder
        .client
        .keys(&plain.pane, &keys(&["up", "enter"]))
        .unwrap();
    shows(&holder, &plain, "^[[A");

    // Bracketed paste and application cursor keys on.
    let modes = holder.spawn(
        &world,
        &[
            "/bin/sh",
            "-c",
            "stty -echo; printf '\\033[?2004h\\033[?1h'; echo ready; exec cat -v",
        ],
    );
    shows(&holder, &modes, "ready");
    let screen = holder
        .client
        .screen(&modes.pane, Read::Visible, Format::Text)
        .unwrap();
    for mode in ["bracketed_paste", "app_cursor"] {
        assert!(
            screen.modes.iter().any(|on| on == mode),
            "{:?}",
            screen.modes
        );
    }
    // An end marker inside the text cannot end the paste early.
    holder.client.paste(&modes.pane, "a\x1b[201~b\n").unwrap();
    shows(&holder, &modes, "^[[200~ab");
    // The paste's end comes after its last line, so it starts the next row.
    holder
        .client
        .keys(&modes.pane, &keys(&["up", "enter"]))
        .unwrap();
    shows(&holder, &modes, "^[[201~^[OA");

    let gone = holder.spawn(&world, &["/bin/sh", "-c", "exit 0"]);
    exit_of(&holder, &gone);
    for err in [
        holder.client.input(&gone.pane, "x").unwrap_err(),
        holder.client.paste(&gone.pane, "x").unwrap_err(),
        holder
            .client
            .keys(&gone.pane, &keys(&["enter"]))
            .unwrap_err(),
    ] {
        assert!(matches!(err, PanesError::Exited(_)), "{err}");
    }
}

/// A raw connection past its handshake.
fn raw_connection(holder: &Holder) -> UnixStream {
    let mut socket = UnixStream::connect(&holder.socket).unwrap();
    socket.set_read_timeout(Some(WAIT)).unwrap();
    send_raw(&mut socket, &Request::Hello(Hello::new("daemon")));
    assert_eq!(read_raw(&mut socket)["type"], "welcome");
    socket
}

fn send_raw(socket: &mut UnixStream, request: &Request) {
    let text = serde_json::to_string(request).unwrap();
    write_frame(socket, &Frame::Message(text)).unwrap();
}

/// The next message, past any output.
fn read_raw(socket: &mut UnixStream) -> serde_json::Value {
    loop {
        match read_frame(&mut *socket).unwrap() {
            Some(Frame::Message(text)) => return serde_json::from_str(&text).unwrap(),
            Some(_) => {}
            None => panic!("the holder closed the connection"),
        }
    }
}

#[test]
fn unsubscribe_ends_one_stream_and_release_answers_with_the_size() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, &["/bin/cat"]);

    let mut socket = raw_connection(&holder);
    send_raw(
        &mut socket,
        &Request::Subscribe {
            id: 2,
            pane: pane.pane.clone(),
            sync: SyncMode::Pass,
        },
    );
    let subscribed = read_raw(&mut socket);
    assert_eq!(subscribed["type"], "subscribed", "{subscribed}");
    let stream = u32::try_from(subscribed["stream"].as_u64().unwrap()).unwrap();
    assert_eq!(read_raw(&mut socket)["type"], "repaint");
    send_raw(&mut socket, &Request::Unsubscribe { id: 3, stream });
    assert_eq!(
        read_raw(&mut socket),
        serde_json::json!({"type":"ok","id":3})
    );
    // Output after it goes to no one on this connection: the next frame
    // is the reply to the ping.
    holder.client.input(&pane.pane, "after\n").unwrap();
    shows(&holder, &pane, "after");
    send_raw(&mut socket, &Request::Ping { id: 4 });
    let Some(Frame::Message(pong)) = read_frame(&mut socket).unwrap() else {
        panic!("a frame other than the pong came first");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&pong).unwrap(),
        serde_json::json!({"type":"pong","id":4})
    );
    send_raw(&mut socket, &Request::Unsubscribe { id: 5, stream });
    assert_eq!(read_raw(&mut socket)["code"], "not_found");

    // With no lease held, the size stays as it is.
    let updates = holder.updates(&pane);
    assert!(matches!(
        next(&updates, &holder, "repaint"),
        Update::Repaint { .. }
    ));
    updates.handle.release().unwrap();
    let mut update = next(&updates, &holder, "the release's size");
    while let Update::Output(_) = update {
        update = next(&updates, &holder, "the release's size");
    }
    assert_eq!(
        update,
        Update::Resized {
            cols: 120,
            rows: 40
        }
    );
}

#[test]
fn meta_merges_set_then_drops_remove_and_tells_watchers() {
    let world = World::new();
    let holder = Holder::start(&world);
    let watch = Watch::start(&holder);
    let spawn = Spawn {
        meta: Meta::from([
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
        ]),
        ..spawn_of(&world, &["/bin/cat"])
    };
    let pane = holder.client.spawn(&spawn).unwrap();
    watch.until(&holder, "spawned", |event| {
        *event
            == Event::Spawned {
                pane: pane.pane.clone(),
            }
    });
    let set = Meta::from([
        ("b".to_owned(), "3".to_owned()),
        ("c".to_owned(), "4".to_owned()),
    ]);
    let changed = holder.client.set_meta(&pane.pane, &set, &["a"]).unwrap();
    assert_eq!(changed.meta, set);
    watch.until(&holder, "meta", |event| {
        *event
            == Event::Meta {
                pane: pane.pane.clone(),
            }
    });
    assert_eq!(holder.client.get(&pane.pane).unwrap().unwrap().meta, set);
}

#[test]
fn watchers_hear_titles_bells_notifications_quiet_resizes_exits_and_forgets() {
    let world = World::new();
    let holder = Holder::start(&world);
    let watch = Watch::start(&holder);
    let pane = holder.spawn(&world, &["/bin/sh"]);
    let id = pane.pane.clone();
    watch.until(&holder, "spawned", |event| {
        *event == Event::Spawned { pane: id.clone() }
    });

    holder
        .client
        .input(
            &pane.pane,
            "printf '\\033]2;build\\007\\007\\033]777;notify;Done;All green\\007'\n",
        )
        .unwrap();
    let mut heard = Vec::new();
    while heard.len() < 3 {
        let event = watch.until(&holder, "title, bell or notify", |event| {
            matches!(
                event,
                Event::Title { .. } | Event::Bell { .. } | Event::Notify { .. }
            )
        });
        heard.push(event);
    }
    for event in [
        Event::Title {
            pane: id.clone(),
            title: "build".to_owned(),
        },
        Event::Bell { pane: id.clone() },
        Event::Notify {
            pane: id.clone(),
            title: "Done".to_owned(),
            body: "All green".to_owned(),
        },
    ] {
        assert!(heard.contains(&event), "{event:?} not in {heard:?}");
    }
    assert_eq!(holder.client.get(&id).unwrap().unwrap().title, "build");
    watch.until(
        &holder,
        "quiet",
        |event| matches!(event, Event::Quiet { pane, .. } if *pane == id),
    );

    let updates = holder.updates(&pane);
    updates.handle.resize(100, 30).unwrap();
    watch.until(&holder, "resized", |event| {
        *event
            == Event::Resized {
                pane: id.clone(),
                cols: 100,
                rows: 30,
            }
    });
    drop(updates);

    holder.client.input(&pane.pane, "exit 4\n").unwrap();
    let Event::Exited { exit, .. } = watch.until(
        &holder,
        "exited",
        |event| matches!(event, Event::Exited { pane, .. } if *pane == id),
    ) else {
        unreachable!()
    };
    assert_eq!(exit.code, Some(4));
    holder.client.forget(&id).unwrap();
    watch.until(&holder, "forgotten", |event| {
        *event
            == Event::Forgotten {
                pane: id.clone(),
                reason: "forget".to_owned(),
            }
    });
}

#[test]
fn signal_close_and_forget_take_a_pane_from_running_to_gone() {
    let world = World::new();
    let holder = Holder::start(&world);

    let sleeper = holder.spawn(&world, &["/bin/sh", "-c", "echo up; sleep 30"]);
    let err = holder.client.forget(&sleeper.pane).unwrap_err();
    assert_eq!(refused(&err), ErrorCode::BadRequest, "{err}");
    assert!(err.to_string().contains("is running"), "{err}");
    holder.client.signal(&sleeper.pane, Signal::Term).unwrap();
    let exit = exit_of(&holder, &sleeper);
    assert_eq!((exit.code, exit.signal), (None, Some(15)), "{exit:?}");
    let err = holder
        .client
        .signal(&sleeper.pane, Signal::Term)
        .unwrap_err();
    assert!(matches!(err, PanesError::Exited(_)), "{err}");
    // An exited pane keeps its screen until it is forgotten.
    shows(&holder, &sleeper, "up");
    holder.client.forget(&sleeper.pane).unwrap();
    assert!(holder.client.get(&sleeper.pane).unwrap().is_none());
    let err = holder.client.forget(&sleeper.pane).unwrap_err();
    assert!(matches!(err, PanesError::NotFound(_)), "{err}");

    // `close` hangs up, and a child that ends on it is not killed.
    let cat = holder.spawn(&world, &["/bin/cat"]);
    let exit = holder.client.close(&cat.pane, WAIT).unwrap();
    assert_eq!(exit.signal, Some(1), "{exit:?}");
    // A second close answers with the same exit.
    assert_eq!(holder.client.close(&cat.pane, WAIT).unwrap(), exit);

    // A child that ignores the hang-up is killed after the grace.
    let stubborn = holder.spawn(
        &world,
        &[
            "/bin/sh",
            "-c",
            "trap '' HUP; echo trapped; while :; do sleep 1; done",
        ],
    );
    shows(&holder, &stubborn, "trapped");
    let started = Instant::now();
    let exit = holder
        .client
        .close(&stubborn.pane, Duration::from_millis(300))
        .unwrap();
    assert_eq!(exit.signal, Some(9), "{exit:?}");
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(
        holder.log().contains("outlived its close grace"),
        "{}",
        holder.log()
    );
}

#[test]
fn switch_answers_for_a_running_pane_only() {
    let world = World::new();
    let holder = Holder::start(&world);
    let pane = holder.spawn(&world, &["/bin/cat"]);
    // No attach client runs, so none is told.
    assert_eq!(holder.client.switch(&pane.pane).unwrap(), 0);
    let gone = holder.spawn(&world, &["/bin/sh", "-c", "exit 0"]);
    exit_of(&holder, &gone);
    let err = holder.client.switch(&gone.pane).unwrap_err();
    assert!(matches!(err, PanesError::Exited(_)), "{err}");
    let err = holder.client.switch(&PaneId::from("p_none")).unwrap_err();
    assert!(matches!(err, PanesError::NotFound(_)), "{err}");
}

// Section 2.5: the spawn's env is the child's whole env, and the holder
// adds three vars and nothing of its own.
#[test]
fn the_child_gets_the_spawn_env_and_only_three_vars_more() {
    let world = World::new();
    let holder = Holder::start(&world);
    let spawn = Spawn {
        env: BTreeMap::from([
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("EXAMPLE_VAR".to_owned(), "a value".to_owned()),
            // The holder's own value wins over the spawn's.
            ("TERM".to_owned(), "dumb".to_owned()),
        ]),
        ..spawn_of(&world, &["/usr/bin/env"])
    };
    let pane = holder.client.spawn(&spawn).unwrap();
    exit_of(&holder, &pane);
    let mut env: Vec<String> = holder
        .client
        .screen(&pane.pane, Read::Visible, Format::Text)
        .unwrap()
        .rows_text
        .into_iter()
        .filter(|row| !row.is_empty())
        .collect();
    env.sort();
    assert_eq!(
        env,
        [
            "COLORTERM=truecolor".to_owned(),
            "EXAMPLE_VAR=a value".to_owned(),
            "PATH=/usr/bin:/bin".to_owned(),
            "TERM=xterm-256color".to_owned(),
            format!("TILLER_PANE={}", pane.pane),
        ]
    );
}

// Section 2.4: requests on one connection may overlap, so a close that
// waits out its grace holds up nothing else the client asks.
#[test]
fn a_close_that_waits_holds_up_none_of_the_connections_other_replies() {
    let world = World::new();
    let holder = Holder::start(&world);
    let stubborn = holder.spawn(
        &world,
        &[
            "/bin/sh",
            "-c",
            "trap '' HUP; echo trapped; while :; do sleep 1; done",
        ],
    );
    shows(&holder, &stubborn, "trapped");
    let grace = Duration::from_secs(2);
    std::thread::scope(|scope| {
        let closing = scope.spawn(|| holder.client.close(&stubborn.pane, grace));
        // The close is in the holder once the hang-up reached the shell.
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        holder.client.ping().unwrap();
        assert_eq!(holder.client.list().unwrap().panes.len(), 1);
        let other = holder.spawn(&world, &["/bin/cat"]);
        holder.client.get(&other.pane).unwrap().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert!(!closing.is_finished(), "the close did not wait its grace");
        let exit = closing.join().unwrap().unwrap();
        assert_eq!(exit.signal, Some(9), "{exit:?}");
    });
}
