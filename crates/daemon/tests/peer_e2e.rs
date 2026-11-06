//! The federated board end to end (F6, `protocol.md` section 10): two homes
//! on one machine, each with its own `tiller daemon`, joined through an SSH
//! shim that runs the real `tiller peer serve` for the peer's home. Nothing
//! is linked in memory: the board host's daemon dials the shim exactly as it
//! dials `ssh`, so the whole path runs - hello, snapshot, events, a signed
//! `mutate` frame, and the peer's outbox.
//!
//! The board host's daemon is started detached (a shell backgrounds it and
//! exits), so `peer serve`'s start check (`authority.md` G44) finds no Claude
//! Code process among its ancestors even when this test is run by an agent.
//! A frozen peer is the `peer serve` process under the shim, stopped with
//! `SIGSTOP`: the TCP-like pipe stays open and nothing answers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tiller_core::Timestamp;
use tiller_core::domain::{AttentionKind, CardKind, PeerScope};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::NewCard;
use tiller_core::state::peers::{
    KeyFile, NewPairing, NewPeerKey, generate_key, key_path, public_key_text, write_key_file,
};
use tiller_daemon::auth;
use tiller_herdr::testing::{Fleet, owner};

const WAIT: Duration = Duration::from_secs(30);
const ALIAS: &str = "peer-alias";
const LABEL: &str = "peer";

// -- processes --------------------------------------------------------------

/// The `tiller` binary this build produced: next to the test executable's
/// `deps` directory, where the workspace build puts it.
fn tiller() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(Path::parent).unwrap();
    let binary = dir.join("tiller");
    assert!(
        binary.exists(),
        "{} is missing; build the workspace first (cargo build -p tiller-cli)",
        binary.display()
    );
    binary
}

/// A `tiller daemon` on a free port, detached from this process.
struct Daemon {
    port: u16,
    pid: u32,
}

impl Daemon {
    fn start(home: &Path, ssh: Option<&Path>) -> Self {
        let out = home.join("daemon.out");
        let pid_file = home.join("daemon.pid");
        let _ = std::fs::remove_file(&out);
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#""$0" daemon --port 0 >"$1" 2>/dev/null </dev/null & echo $! >"$2""#,
            ])
            .arg(tiller())
            .arg(&out)
            .arg(&pid_file)
            .env_clear()
            .env("PATH", "")
            .env("TILLER_HOME", home)
            .env("TILLER_HERDR", "/nonexistent/herdr")
            .env("TILLER_QUOTA_AXI", "/nonexistent/quota-axi")
            .env("TILLER_LOG", "warn");
        if let Some(ssh) = ssh {
            command.env("TILLER_SSH", ssh);
        }
        assert!(command.status().unwrap().success());
        let line = until("the daemon's listening line", || {
            std::fs::read_to_string(&out)
                .ok()
                .and_then(|text| text.lines().next().map(str::to_owned))
        });
        let port = line
            .strip_prefix("listening on http://127.0.0.1:")
            .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
            .parse()
            .unwrap();
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        Self { port, pid }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        signal("-TERM", self.pid);
    }
}

fn signal(name: &str, pid: u32) {
    let _ = Command::new("kill").arg(name).arg(pid.to_string()).status();
}

fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    until_within(what, WAIT, &mut check)
}

fn until_within<T>(what: &str, within: Duration, check: &mut impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

// -- the two homes ----------------------------------------------------------

/// The board host and its peer, paired, with a daemon each.
struct World {
    board: Fleet,
    peer: Fleet,
    board_daemon: Daemon,
    peer_daemon: Daemon,
    cookie: String,
    /// The shim records the `peer serve` pid here, then execs it.
    serve_pid: PathBuf,
}

impl World {
    /// Pairs `peer` onto `board` as `tiller peer add` and `peer accept`
    /// leave both homes (the commands need a terminal), then starts both
    /// daemons. The board host's reaches the peer through the SSH shim.
    fn start() -> Self {
        let board = Fleet::new();
        let peer = Fleet::new();

        let key = generate_key().unwrap();
        let accepted = peer
            .open()
            .peer_accept(
                &owner(),
                true,
                NewPeerKey {
                    viewer_label: "test-host".into(),
                    viewer_host_id: board.host.clone(),
                    public_key: public_key_text(&key),
                    scope: PeerScope::Act,
                },
            )
            .unwrap()
            .value;
        board
            .open()
            .peer_pair(
                &owner(),
                NewPairing {
                    label: LABEL.into(),
                    herdr_machine: None,
                    scope: PeerScope::Act,
                    peer_host_id: peer.host.clone(),
                },
            )
            .unwrap();
        let file = KeyFile::new(LABEL, PeerScope::Act, &key).accepted(&peer.host, &accepted.id);
        write_key_file(&key_path(board.dir.path(), LABEL).unwrap(), &file).unwrap();
        std::fs::write(
            board.dir.path().join("config.toml"),
            format!("[peers.{LABEL}]\nssh = \"{ALIAS}\"\n"),
        )
        .unwrap();

        // What the peer's orchestrator asked the owner.
        let mut store = peer.open();
        let card = store
            .board_add(
                &owner(),
                None,
                NewCard {
                    project_id: "alpha".into(),
                    kind: CardKind::Ship,
                    title: "Peer card".into(),
                    ..NewCard::default()
                },
            )
            .unwrap()
            .value;
        store
            .attention_open(
                &owner(),
                None,
                NewAttention {
                    card: card.card.id.clone(),
                    kind: AttentionKind::Decision,
                    question: "Which database should the peer use?".into(),
                    ..NewAttention::default()
                },
            )
            .unwrap();

        let serve_pid = board.dir.path().join("serve.pid");
        let shim = write_shim(board.dir.path(), peer.dir.path(), &serve_pid);
        let peer_daemon = Daemon::start(peer.dir.path(), None);
        let board_daemon = Daemon::start(board.dir.path(), Some(&shim));
        let token = auth::mint_login(board.dir.path(), Timestamp::now())
            .unwrap()
            .token;
        let cookie = auth::redeem_login(board.dir.path(), &token, Timestamp::now())
            .unwrap()
            .unwrap();
        Self {
            board,
            peer,
            board_daemon,
            peer_daemon,
            cookie: format!("tiller_owner={cookie}"),
            serve_pid,
        }
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let (status, body) = http(self.board_daemon.port, "GET", path, None, None);
        (status, parse(&body))
    }

    fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let (status, body) = http(
            self.board_daemon.port,
            "POST",
            path,
            Some(body),
            Some(&self.cookie),
        );
        (status, parse(&body))
    }

    /// The link state `peers[]` gives for the peer.
    fn link(&self) -> Option<String> {
        let (status, snapshot) = self.get("/api/v1/snapshot");
        assert_eq!(status, 200, "{snapshot}");
        snapshot["peers"]
            .as_array()?
            .iter()
            .find(|peer| peer["host"] == LABEL)
            .and_then(|peer| peer["link"].as_str().map(str::to_owned))
    }

    fn live(&self) {
        until("the link to go live", || {
            (self.link().as_deref() == Some("live")).then_some(())
        });
    }

    fn board_rows(&self) -> Vec<Value> {
        let (status, board) = self.get("/api/v1/board?filter=all");
        assert_eq!(status, 200, "{board}");
        board["rows"].as_array().unwrap().clone()
    }

    fn peer_question(&self) -> Option<Value> {
        let (status, items) = self.get("/api/v1/attention?state=open");
        assert_eq!(status, 200, "{items}");
        items
            .as_array()
            .or_else(|| items["items"].as_array())?
            .iter()
            .find(|item| item["host"] == LABEL)
            .cloned()
    }

    fn freeze_peer(&self) -> u32 {
        let pid = std::fs::read_to_string(&self.serve_pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        signal("-STOP", pid);
        pid
    }
}

/// What the board host's daemon runs as `ssh`: it drops the options, takes
/// the alias and runs the real `tiller peer serve` for the peer's home, the
/// way sshd would on the other machine.
fn write_shim(dir: &Path, peer_home: &Path, serve_pid: &Path) -> PathBuf {
    let shim = dir.join("ssh-shim");
    let script = format!(
        r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift 2 ;;
    -T) shift ;;
    *) break ;;
  esac
done
[ "$1" = "{ALIAS}" ] || exit 255
shift
[ "$1" = tiller ] || exit 127
shift
echo $$ > "{pid}"
export TILLER_HOME="{home}" TILLER_HERDR=/nonexistent/herdr
export SSH_CONNECTION="192.0.2.1 50000 192.0.2.2 22"
exec "{tiller}" "$@"
"#,
        pid = serve_pid.display(),
        home = peer_home.display(),
        tiller = tiller().display(),
    );
    std::fs::write(&shim, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    shim
}

// -- http -------------------------------------------------------------------

fn parse(body: &str) -> Value {
    serde_json::from_str(body).unwrap_or_else(|error| panic!("not JSON ({error}): {body:?}"))
}

/// One request as the board sends it; a mutation carries the cookie and the
/// board's own origin.
fn http(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&Value>,
    cookie: Option<&str>,
) -> (u16, String) {
    let host = format!("127.0.0.1:{port}");
    let body = body.map(Value::to_string);
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(body) = &body {
        request += &format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\nSec-Fetch-Site: same-origin\r\nOrigin: http://{host}\r\n",
            body.len()
        );
    }
    if let Some(cookie) = cookie {
        request += &format!("Cookie: {cookie}\r\n");
    }
    request += "\r\n";
    request += body.as_deref().unwrap_or_default();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_owned()
    };
    (status, body)
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 || tail.len() < size {
            break;
        }
        out += &tail[..size];
        rest = tail[size..].trim_start_matches("\r\n");
    }
    out
}

/// `GET /api/v1/events`, read on a thread; each `data:` line arrives on the
/// channel.
fn sse(port: u16) -> mpsc::Receiver<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET /api/v1/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
    )
    .unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            if let Some(data) = line.strip_prefix("data:")
                && tx.send(data.trim().to_owned()).is_err()
            {
                return;
            }
        }
    });
    rx
}

fn heard(rx: &mpsc::Receiver<String>, within: Duration, needle: &str) -> bool {
    let deadline = Instant::now() + within;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(data) if data.contains(needle) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

fn count(fleet: &Fleet, sql: &str) -> i64 {
    fleet
        .open()
        .conn()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

// -- the tests --------------------------------------------------------------

/// The board shows the peer's question next to its own cards; the owner
/// answers it there and the answer lands in the peer's outbox, addressed to
/// the peer's orchestrator, with nothing written on the board host.
#[test]
fn board_answer_reaches_the_peers_orchestrator_outbox() {
    let world = World::start();
    world.live();

    let question = until("the peer's question on the board", || world.peer_question());
    assert_eq!(question["question"], "Which database should the peer use?");
    assert_eq!(question["link"], "live");
    assert!(
        world
            .board_rows()
            .iter()
            .any(|row| row["host"] == LABEL && row["title"] == "Peer card")
    );
    let item = question["id"].as_str().unwrap().to_owned();

    let host_events = count(&world.board, "SELECT count(*) FROM events");
    let (status, answered) = world.post(
        &format!("/api/v1/attention/{item}/answer"),
        &json!({"idempotency_key": "answer-1", "host": LABEL, "answer": "Use SQLite."}),
    );
    assert_eq!(status, 200, "{answered}");
    assert_eq!(answered["host"], LABEL);

    // The peer's outbox holds it, for the peer's orchestrator.
    let (target, kind, body, reply_to): (String, String, String, String) = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT target_session_id, kind, body, reply_to FROM outbox WHERE kind = 'answer'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(target, world.peer.orchestrator_session);
    assert_eq!(kind, "answer");
    assert!(body.contains("Use SQLite."), "{body}");
    assert_eq!(reply_to, item);
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM outbox WHERE kind = 'answer'"
        ),
        1
    );

    // The peer recorded who asked, and the board host wrote nothing.
    let actor: String = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT actor FROM events WHERE kind = 'attention.answered'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actor, "board@test-host");
    assert_eq!(
        count(&world.board, "SELECT count(*) FROM events"),
        host_events
    );
    assert_eq!(count(&world.board, "SELECT count(*) FROM outbox"), 0);

    // The same key again applies once.
    let (status, again) = world.post(
        &format!("/api/v1/attention/{item}/answer"),
        &json!({"idempotency_key": "answer-1", "host": LABEL, "answer": "Use SQLite."}),
    );
    assert_eq!(status, 200, "{again}");
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM outbox WHERE kind = 'answer'"
        ),
        1
    );

    // The peer's own board shows it answered, and the board host's copy
    // catches up through the link.
    let (status, peers_view) = http(
        world.peer_daemon.port,
        "GET",
        "/api/v1/attention?state=open",
        None,
        None,
    );
    assert_eq!(status, 200, "{peers_view}");
    assert!(!peers_view.contains("Which database"), "{peers_view}");
    until("the question to leave the board", || {
        world.peer_question().is_none().then_some(())
    });
}

/// With the peer's `peer serve` stopped, the board host's own cards, writes
/// and event stream go on, the peer's rows keep their last values with an
/// "as of" and a link that is not live, and a mutation for the peer is
/// refused at once with nothing queued. When the peer thaws, the link and
/// the controls come back.
#[test]
fn frozen_peer_leaves_the_board_host_unaffected() {
    let world = World::start();
    world.live();
    until("the peer's question on the board", || world.peer_question());
    let events = sse(world.board_daemon.port);
    let item = world.peer_question().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let frozen = world.freeze_peer();

    // A local write is applied and streamed at once, not after a timeout.
    let started = Instant::now();
    let (status, created) = world.post(
        "/api/v1/cards",
        &json!({"idempotency_key": "local-1", "project": "alpha", "kind": "ship", "title": "Local while frozen"}),
    );
    assert_eq!(status, 200, "{created}");
    assert!(
        heard(&events, Duration::from_secs(5), "Local while frozen"),
        "the local write did not reach the board's own SSE"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        world
            .board_rows()
            .iter()
            .any(|row| row["title"] == "Local while frozen" && row.get("as_of").is_none())
    );

    // After 25 s without a frame the link is no longer live, and the peer's
    // rows say how old they are. They are still there.
    until_within(
        "the link to leave live",
        Duration::from_secs(45),
        &mut || (world.link().as_deref() != Some("live")).then_some(()),
    );
    let rows = world.board_rows();
    let theirs: Vec<_> = rows.iter().filter(|row| row["host"] == LABEL).collect();
    assert!(!theirs.is_empty(), "the peer's cards vanished: {rows:?}");
    for row in &theirs {
        assert!(row["as_of"].is_string(), "no as_of on {row}");
        assert_ne!(row["link"], "live", "{row}");
    }
    assert!(
        rows.iter()
            .filter(|row| row["host"] != LABEL)
            .all(|row| row.get("as_of").is_none())
    );
    let stale = world
        .peer_question()
        .expect("the peer's question stays, dimmed");
    assert_ne!(stale["link"], "live");
    assert!(stale["as_of"].is_string());

    // Local work still goes through.
    let (status, _) = world.post(
        "/api/v1/cards",
        &json!({"idempotency_key": "local-2", "project": "alpha", "kind": "ship", "title": "Still local"}),
    );
    assert_eq!(status, 200);

    // A mutation for the stopped peer is refused at once; nothing is queued.
    let started = Instant::now();
    let (status, refused) = world.post(
        &format!("/api/v1/attention/{item}/answer"),
        &json!({"idempotency_key": "answer-frozen", "host": LABEL, "answer": "Use SQLite."}),
    );
    assert_eq!(status, 503, "{refused}");
    assert_eq!(refused["error"]["code"], "peer_unreachable", "{refused}");
    assert!(started.elapsed() < Duration::from_secs(3));
    // The peer's own daemon writes events of its own; none is the board's.
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM events WHERE actor LIKE 'board@%'"
        ),
        0
    );
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM outbox WHERE kind = 'answer'"
        ),
        0
    );

    // The peer's own daemon never needed the link.
    let (status, _) = http(
        world.peer_daemon.port,
        "GET",
        "/api/v1/board?filter=all",
        None,
        None,
    );
    assert_eq!(status, 200);

    // Thawed, the link is live again and the same control works.
    signal("-CONT", frozen);
    world.live();
    let (status, answered) = world.post(
        &format!("/api/v1/attention/{item}/answer"),
        &json!({"idempotency_key": "answer-thawed", "host": LABEL, "answer": "Use SQLite."}),
    );
    assert_eq!(status, 200, "{answered}");
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM outbox WHERE kind = 'answer'"
        ),
        1
    );
}
