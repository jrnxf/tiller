//! The real terminal holder and the board's view of its panes, for the
//! tests that run `tiller holder` and the daemon together.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::Value;
use tiller_panes::{HolderClient, socket_path};

use super::{Running, World};

/// How long a test waits for anything the holder or the daemon does.
pub const WAIT: Duration = Duration::from_secs(20);

/// The `fake-agent` binary cargo built beside `tiller`.
pub fn fake_agent() -> PathBuf {
    let path = Path::new(env!("CARGO_BIN_EXE_tiller"))
        .parent()
        .unwrap()
        .join("fake-agent");
    assert!(
        path.exists(),
        "{} is missing: build it with `cargo build -p tiller-panes --features testing --bin fake-agent`",
        path.display()
    );
    path
}

/// Writes an executable `sh` script.
pub fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A running `tiller holder` on `world`'s home, and a client of it.
pub fn start_holder(world: &World) -> (Running, HolderClient) {
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
pub fn cookie(world: &World) -> String {
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
pub fn post(port: u16, cookie: &str, path: &str, body: &str) -> TcpStream {
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
pub fn send(port: u16, cookie: &str, session: &str, input: &Value) {
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
pub struct View {
    stream: TcpStream,
    raw: Vec<u8>,
    /// Where the body starts in `raw`, once the head is read.
    body: Option<usize>,
    /// The body lines read so far.
    pub lines: Vec<Value>,
    /// The bytes of every frame so far.
    pub screen: Vec<u8>,
}

impl View {
    pub fn open(port: u16, cookie: &str, session: &str) -> Self {
        Self::open_with(port, cookie, session, "{}")
    }

    /// Opens the view with `body`, such as a Fit.
    pub fn open_with(port: u16, cookie: &str, session: &str, body: &str) -> Self {
        let stream = post(
            port,
            cookie,
            &format!("/api/v1/agents/{session}/terminal"),
            body,
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
    pub fn until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
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
                // A quiet stream or a signal: the deadline check above says what
                // came.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::Interrupted
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

    pub fn shows(&self, text: &str) -> bool {
        String::from_utf8_lossy(&self.screen).contains(text)
    }
}
