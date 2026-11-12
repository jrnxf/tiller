//! The daemon on the real terminal holder (`terminal-holder.md` 9.2): an
//! agent launched by `tiller` runs in a `tiller holder` pane, the board's
//! terminal endpoint serves it and types into it, and a daemon restart with
//! the holder kept up serves the same pane, with its history, again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine as _;
use common::{Running, World, git_only_daemon};
use serde_json::Value;
use tiller_panes::{HolderClient, PaneState, Panes, socket_path};

/// How long the test waits for anything the holder or the daemon does.
const WAIT: Duration = Duration::from_secs(20);

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

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

/// A running `tiller holder` on `world`'s home, and a client of it.
fn start_holder(world: &World) -> (Running, HolderClient) {
    let log = world.dir.path().join("holder.log");
    let running = Running::new(
        world
            .bare_command()
            .arg("holder")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
    );
    let socket = socket_path(&world.home());
    let deadline = Instant::now() + WAIT;
    loop {
        match HolderClient::connect(&socket, "test") {
            Ok(client) => return (running, client),
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => panic!(
                "the holder did not listen: {error}\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            ),
        }
    }
}

/// The owner's board cookie for `world`'s daemon.
fn cookie(world: &World) -> String {
    let now = tiller_core::Timestamp::now();
    let token = tiller_daemon::auth::mint_login(&world.home(), now)
        .unwrap()
        .token;
    tiller_daemon::auth::redeem_login(&world.home(), &token, now)
        .unwrap()
        .unwrap()
}

/// A board POST to `path`, sent on a new connection; the connection is
/// returned with the request written.
fn post(port: u16, cookie: &str, path: &str, body: &str) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream
        .write_all(
            format!(
                "POST {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\n\
                 Sec-Fetch-Site: same-origin\r\nCookie: {}={cookie}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                tiller_daemon::auth::COOKIE,
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    stream
}

/// Types `input` into the session's pane through the board's input path.
fn send(port: u16, cookie: &str, session: &str, input: &Value) {
    let mut stream = post(
        port,
        cookie,
        &format!("/api/v1/agents/{session}/terminal/input"),
        &input.to_string(),
    );
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.0 200"), "{answer}");
}

/// The board's live view of a session's pane: the NDJSON stream, read as
/// it comes.
struct View {
    stream: TcpStream,
    raw: Vec<u8>,
    /// Where the body starts in `raw`, once the head is read.
    body: Option<usize>,
    /// The body lines read so far.
    lines: Vec<Value>,
    /// The bytes of every frame so far.
    screen: Vec<u8>,
}

impl View {
    fn open(port: u16, cookie: &str, session: &str) -> Self {
        let stream = post(
            port,
            cookie,
            &format!("/api/v1/agents/{session}/terminal"),
            "{}",
        );
        Self {
            stream,
            raw: Vec::new(),
            body: None,
            lines: Vec::new(),
            screen: Vec::new(),
        }
    }

    /// Reads until `done` holds for the lines read so far, or fails naming
    /// `what` with what came.
    fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        let mut buf = [0_u8; 8192];
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "no {what} in the view: {:?}\n{}",
                self.lines,
                String::from_utf8_lossy(&self.screen)
            );
            self.stream.set_read_timeout(Some(left)).unwrap();
            let read = match self.stream.read(&mut buf) {
                Ok(read) => read,
                // A quiet stream: the deadline check above says what came.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => panic!("reading the view: {error}"),
            };
            assert!(
                read > 0,
                "the view ended before {what}: {}",
                String::from_utf8_lossy(&self.raw)
            );
            self.raw.extend_from_slice(&buf[..read]);
            self.parse();
        }
    }

    fn parse(&mut self) {
        let start = match self.body {
            Some(start) => start,
            None => {
                let Some(end) = self.raw.windows(4).position(|w| w == b"\r\n\r\n") else {
                    return;
                };
                let head = String::from_utf8_lossy(&self.raw[..end]).into_owned();
                assert!(head.starts_with("HTTP/1.0 200"), "{head}");
                self.body = Some(end + 4);
                end + 4
            }
        };
        let mut at = start;
        while let Some(newline) = self.raw[at..].iter().position(|b| *b == b'\n') {
            let line: Value = serde_json::from_slice(&self.raw[at..at + newline]).unwrap();
            if line["type"] == "frame" {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(line["bytes"].as_str().unwrap())
                    .unwrap();
                self.screen.extend_from_slice(&bytes);
            }
            self.lines.push(line);
            at += newline + 1;
        }
        self.raw.drain(start..at);
    }

    fn shows(&self, text: &str) -> bool {
        String::from_utf8_lossy(&self.screen).contains(text)
    }
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
