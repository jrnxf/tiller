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
use tiller_core::store::Actor;
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
                    unattended: false,
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
        Self::run(board, peer)
    }

    /// Starts both homes' daemons, the board host's with the SSH shim.
    fn run(board: Fleet, peer: Fleet) -> Self {
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
/// the alias and runs the real `tiller peer serve` (or Add a host's `tiller
/// peer accept --unattended`) for the peer's home, the way sshd would on the
/// other machine. Its herdr is [`peer_herdr`], which a test writes only when
/// the peer must start an orchestrator.
fn write_shim(dir: &Path, peer_home: &Path, serve_pid: &Path) -> PathBuf {
    let shim = dir.join("ssh-shim");
    let script = format!(
        r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift 2 ;;
    -T) shift ;;
    --) shift; break ;;
    *) break ;;
  esac
done
[ "$1" = "{ALIAS}" ] || exit 255
shift
[ "$1" = tiller ] || exit 127
shift
echo $$ > "{pid}"
export HOME="{home}" TILLER_HOME="{home}" TILLER_HERDR="{herdr}"
export SSH_CONNECTION="192.0.2.1 50000 192.0.2.2 22"
exec "{tiller}" "$@"
"#,
        pid = serve_pid.display(),
        home = peer_home.display(),
        herdr = peer_herdr(peer_home).display(),
        tiller = tiller().display(),
    );
    std::fs::write(&shim, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    shim
}

/// The herdr the peer's `peer serve` drives; missing unless a test writes it.
fn peer_herdr(peer_home: &Path) -> PathBuf {
    peer_home.join("bin/herdr")
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

/// The events `fleet` recorded after `seq`, as `kind by actor`, other than
/// its daemon's records of the hosts. Those come from the daemon's own
/// pollers and links, whatever a request does, and land when they land:
/// the herdr poller's and each link change's `host.changed`, and the first
/// reconcile pass's `daemon.started` and host checks, which can come after
/// the link is live. Every other event counts, whoever wrote it.
fn written_since(fleet: &Fleet, seq: i64) -> Vec<String> {
    let daemon = Actor::Daemon.to_string();
    fleet
        .open()
        .events_since(seq, i64::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| !(event.actor == daemon && event.entity == "host"))
        .map(|event| format!("{} by {}", event.kind, event.actor))
        .collect()
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

    let host_seq = world.board.open().last_seq().unwrap();
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
    let written = written_since(&world.board, host_seq);
    assert!(written.is_empty(), "the board host wrote {written:?}");
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

/// Add a host from the board: with nothing paired, the owner's `POST
/// /api/v1/peers` runs the peer's real `tiller peer accept --unattended`
/// through the SSH shim, and the link goes live with no other step. The
/// peer's audit marks the accept as one no terminal saw.
#[test]
fn board_pairing_brings_the_link_up() {
    let world = World::run(Fleet::new(), Fleet::new());
    assert_eq!(world.link(), None);

    let (status, paired) = world.post(
        "/api/v1/peers",
        &json!({"idempotency_key": "pair-1", "label": LABEL, "ssh": ALIAS, "scope": "act"}),
    );
    assert_eq!(status, 200, "{paired}");
    assert_eq!(paired["data"]["label"], LABEL);
    assert_eq!(paired["data"]["ssh"], ALIAS);
    assert!(
        paired["data"]["fingerprint"]
            .as_str()
            .is_some_and(|print| print.starts_with("SHA256:")),
        "{paired}"
    );
    world.live();

    let (actor, payload): (String, String) = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT actor, payload FROM events WHERE kind = 'peer.accepted'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(actor, "board@test-host");
    let payload: Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["unattended"], true, "{payload}");
    assert_eq!(payload["scope"], "act", "{payload}");

    // A second pairing of the label is refused before ssh runs.
    let (status, again) = world.post(
        "/api/v1/peers",
        &json!({"idempotency_key": "pair-2", "label": LABEL, "ssh": ALIAS, "scope": "act"}),
    );
    assert_eq!(status, 422, "{again}");
    assert_eq!(again["error"]["code"], "label_taken", "{again}");
    assert_eq!(count(&world.peer, "SELECT count(*) FROM peer_keys"), 1);
}

/// The peer's session that runs an older tiller shows on the board host's
/// roster, judged against the peer's own tiller, and the owner restarts it
/// from there: the board host relays `restart-stale` to the peer, which
/// queues the working session. The peer restarts only what it was asked to.
#[test]
fn board_restarts_a_peers_older_sessions() {
    let world = World::start();
    world.live();

    // The peer's orchestrator has no launch stamp, so it is stale there; it
    // is working, so a restart queues it.
    world
        .peer
        .open()
        .conn()
        .execute(
            "UPDATE sessions SET activity = 'working',
                    activity_observed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            [&world.peer.orchestrator_session],
        )
        .unwrap();
    let session = world.peer.orchestrator_session.clone();

    let row = until("the peer's older session on the board host", || {
        let (status, roster) = world.get("/api/v1/agents");
        assert_eq!(status, 200, "{roster}");
        roster["orchestrators"]
            .as_array()?
            .iter()
            .find(|row| row["session_id"] == session.as_str() && row["host"] == LABEL)
            .cloned()
    });
    assert_eq!(row["stale"]["since"], "unknown", "{row}");
    assert_eq!(row["stale"]["restart_queued"], false);
    assert_eq!(row["link"], "live");
    let (_, machines) = world.get("/api/v1/machines");
    let peer = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["label"] == LABEL)
        .unwrap();
    assert_eq!(peer["peer_scope"], "act", "{machines}");
    assert!(peer["version"].is_string(), "{machines}");

    let (status, restarted) = world.post(
        "/api/v1/agents/restart-stale",
        &json!({"idempotency_key": "restart-1", "host": LABEL, "sessions": [&session]}),
    );
    assert_eq!(status, 200, "{restarted}");
    assert_eq!(restarted["host"], LABEL);
    assert_eq!(
        restarted["data"]["sessions"][0]["session_id"],
        session.as_str()
    );
    assert_eq!(restarted["data"]["sessions"][0]["outcome"], "queued");

    // The peer holds the queued restart, and its row says so on the board
    // host once the link carries it.
    let queued: i64 = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT count(*) FROM sessions WHERE id = ?1 AND restart_queued_at IS NOT NULL",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(queued, 1);
    until("the queued restart on the board host", || {
        let (_, roster) = world.get("/api/v1/agents");
        roster["orchestrators"]
            .as_array()?
            .iter()
            .find(|row| row["session_id"] == session.as_str() && row["host"] == LABEL)
            .filter(|row| row["stale"]["restart_queued"] == true)
            .map(|_| ())
    });
}

/// The ask box aimed at a peer's project with no live orchestrator, as the
/// owner sends it from the board host: Start orchestrator first, then the
/// ask, both forwarded. The peer starts its own orchestrator through the
/// real start, as `board@<label>`, and the ask waits in that orchestrator's
/// outbox. The board host starts and records nothing.
#[test]
fn board_starts_a_peers_orchestrator_and_asks_it() {
    let world = World::start();
    let old = world.peer.orchestrator_session.clone();
    world.peer.open().session_end(&owner(), &old, "x").unwrap();

    // The peer's start needs its roots and a herdr that opens a workspace.
    let lab = world.peer.dir.path().join("lab");
    let checkout = lab.join("checkouts/alpha");
    for dir in [&checkout, &lab.join("pool"), &lab.join("scratch")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(
        world.peer.dir.path().join("config.toml"),
        format!(
            "[spawn]\npool_root = {:?}\nscratch_root = {:?}\norchestrator_home = {:?}\ncheckouts_root = {:?}\n\n[checkouts]\nalpha = {:?}\n",
            lab.join("pool").to_str().unwrap(),
            lab.join("scratch").to_str().unwrap(),
            lab.join("orchestrators").to_str().unwrap(),
            lab.join("checkouts").to_str().unwrap(),
            checkout.to_str().unwrap(),
        ),
    )
    .unwrap();
    let calls = lab.join("herdr.log");
    let herdr = peer_herdr(world.peer.dir.path());
    std::fs::create_dir_all(herdr.parent().unwrap()).unwrap();
    std::fs::write(
        &herdr,
        format!(
            r#"#!/bin/sh
echo "$*" >> '{calls}'
case "$1 $2" in
"workspace create") echo '{{"result":{{"workspace":{{"workspace_id":"w9"}},"root_pane":{{"pane_id":"w9:p1"}}}}}}' ;;
"api snapshot") echo '{{"result":{{"snapshot":{{"version":"0.9.5","workspaces":[{{"workspace_id":"w9","label":"alpha","number":1}}],"panes":[{{"pane_id":"w9:p1","workspace_id":"w9","agent_status":"unknown"}}]}}}}}}' ;;
*) echo '{{"result":{{}}}}' ;;
esac
"#,
            calls = calls.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    world.live();

    let board_sessions = count(&world.board, "SELECT count(*) FROM sessions");
    let (status, started) = world.post(
        "/api/v1/projects/alpha/orchestrator",
        &json!({"idempotency_key": "start-1", "host": LABEL}),
    );
    assert_eq!(status, 200, "{started}");
    assert_eq!(started["host"], LABEL);
    let session = started["data"]["session"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(session, old);
    let live: String = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT id FROM sessions WHERE role = 'orchestrator' AND project_id = 'alpha'
                    AND ended_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live, session);
    let actor: String = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT actor FROM events WHERE kind = 'session.started' AND entity_id = ?1",
            [&session],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actor, "board@test-host");
    let log = std::fs::read_to_string(&calls).unwrap();
    assert!(log.contains("workspace create"), "{log}");
    assert!(log.contains("pane send-text w9:p1"), "{log}");

    // The same key again is the start that landed, not a second one.
    let (status, again) = world.post(
        "/api/v1/projects/alpha/orchestrator",
        &json!({"idempotency_key": "start-1", "host": LABEL}),
    );
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["data"]["session"]["id"], session.as_str());

    let (status, asked) = world.post(
        "/api/v1/projects/alpha/asks",
        &json!({
            "idempotency_key": "ask-1",
            "host": LABEL,
            "text": "write a haiku and put it in a new haiku.txt file",
        }),
    );
    assert_eq!(status, 200, "{asked}");
    assert_eq!(asked["host"], LABEL);
    let target: String = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT target_session_id FROM outbox WHERE body LIKE '%haiku.txt%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(target, session);
    assert_eq!(
        count(&world.board, "SELECT count(*) FROM sessions"),
        board_sessions
    );
    assert_eq!(count(&world.board, "SELECT count(*) FROM asks"), 0);
}

/// Claude's screen mid-turn.
const WORKING: &str = include_str!("../../panes/tests/fixtures/claude/working.ansi");

/// A builder on the peer in herdr pane `w2:p1`, beside the orchestrator in
/// `w1:p1`, and the herdr the peer's `peer serve` drives. The herdr logs
/// each call to the returned file and answers from files next to it: the
/// builder's `status` (`working` to start), the `screen` a read gives
/// ([`WORKING`] to start), the
/// `after-keys` screen a typed digit leaves (marked by `typed`), and
/// `orchestrator-gone`, which
/// makes the orchestrator's pane gone. Returns the builder's session and
/// the log.
fn peer_builder(world: &mut World) -> (String, PathBuf) {
    let session = world.peer.worker("w2:p1");
    let lab = world.peer.dir.path().join("lab");
    std::fs::create_dir_all(&lab).unwrap();
    std::fs::write(lab.join("status"), "working").unwrap();
    std::fs::write(lab.join("screen"), WORKING).unwrap();
    let herdr = peer_herdr(world.peer.dir.path());
    std::fs::create_dir_all(herdr.parent().unwrap()).unwrap();
    std::fs::write(
        &herdr,
        format!(
            r#"#!/bin/sh
# The daemons run with an empty PATH: builtins only.
show() {{ while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line"; done < "$1"; }}
echo "$*" >> '{lab}/herdr.log'
case "$*" in
"pane get w2:p1"*)
  read -r status < '{lab}/status'
  printf '{{"result":{{"pane":{{"pane_id":"w2:p1","workspace_id":"w2","agent":"claude","agent_status":"%s","tokens":{{"session":"{session}"}}}}}}}}\n' "$status" ;;
"pane get w1:p1"*)
  if [ -e '{lab}/orchestrator-gone' ]; then
    echo '{{"error":{{"code":"pane_not_found","message":"no pane w1:p1"}}}}' >&2
    exit 1
  fi
  echo '{{"result":{{"pane":{{"pane_id":"w1:p1","workspace_id":"w1","agent":"claude","agent_status":"idle"}}}}}}' ;;
"pane layout"*)
  echo '{{"result":{{"layout":{{"panes":[{{"pane_id":"w2:p1","rect":{{"width":120,"height":40}}}}]}}}}}}' ;;
"pane read"*)
  if [ -e '{lab}/typed' ]; then show '{lab}/after-keys'; else show '{lab}/screen'; fi ;;
"pane send-keys w2:p1 "[0-9]*)
  if [ -e '{lab}/after-keys' ]; then : > '{lab}/typed'; fi ;;
"api snapshot"*)
  echo '{{"result":{{"snapshot":{{"version":"0.9.5","workspaces":[{{"workspace_id":"w1","label":"alpha","number":1}},{{"workspace_id":"w2","label":"alpha-1","number":2}}],"panes":[{{"pane_id":"w1:p1","workspace_id":"w1","agent":"claude","agent_status":"idle"}},{{"pane_id":"w2:p1","workspace_id":"w2","agent":"claude","agent_status":"working"}}]}}}}}}' ;;
*) echo '{{"result":{{}}}}' ;;
esac
"#,
            lab = lab.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    (session, lab.join("herdr.log"))
}

/// The agent's own hook on the peer: once `log` shows the Escape reached
/// `w2:p1`, the agent's turn ends with a `Stop`.
fn stop_hook_after_escape(world: &World, log: &Path, session: &str) -> std::thread::JoinHandle<()> {
    let store = world.peer.open();
    let (log, session) = (log.to_owned(), session.to_owned());
    std::thread::spawn(move || {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            let calls = std::fs::read_to_string(&log).unwrap_or_default();
            if calls.lines().any(|call| call.starts_with("pane send-keys w2:p1 esc")) {
                store
                    .conn()
                    .execute(
                        "INSERT INTO observations (id, session_id, source, event, observed_at, recorded_at, payload)
                         VALUES ('01M7AEQD3P56M1JB0XWF6BRST0', ?1, 'hook', 'Stop', ?2, ?2, '{}')",
                        rusqlite::params![session, Timestamp::now().to_string()],
                    )
                    .unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })
}

/// Puts the peer's `session` on Claude's permission dialog for a Bash call,
/// as its hook and its pane show it; a typed digit leaves an empty composer.
fn ask_permission(world: &World, lab: &Path, session: &str) {
    let now = Timestamp::now().to_string();
    let store = world.peer.open();
    store
        .conn()
        .execute(
            "INSERT INTO observations (id, session_id, source, event, observed_at, recorded_at, payload)
             VALUES ('01M7AEQD3P56M1JB0XWF6BRWT0', ?1, 'hook', 'PermissionRequest', ?2, ?2, ?3)",
            rusqlite::params![session, now, json!({"tool_name": "Bash"}).to_string()],
        )
        .unwrap();
    store
        .conn()
        .execute(
            "UPDATE sessions SET activity = 'waiting_permission', activity_observed_at = ?2
             WHERE id = ?1",
            rusqlite::params![session, now],
        )
        .unwrap();
    std::fs::write(
        lab.join("screen"),
        include_str!("../../panes/tests/fixtures/claude/permission-dialog.ansi"),
    )
    .unwrap();
    std::fs::write(
        lab.join("after-keys"),
        include_str!("../../panes/tests/fixtures/claude/empty.ansi"),
    )
    .unwrap();
}

/// The owner, on the board host, works a builder that runs on the peer
/// (D20): reads its tail, interrupts its turn, reads and answers its
/// permission prompt, steers it, and stops the peer's orchestrator. Each
/// read is a query and each verb a signed `mutate` on the real link; the
/// peer's real controls type into its own herdr and write its own store,
/// as `board@<label>`. The board host writes nothing.
#[test]
fn board_interrupts_answers_steers_and_stops_a_peer_agent() {
    let mut world = World::start();
    let (session, log) = peer_builder(&mut world);
    let lab = log.parent().unwrap().to_owned();
    world.live();
    let host_seq = world.board.open().last_seq().unwrap();
    let agent = |verb: &str| format!("/api/v1/agents/{session}/{verb}");

    // The tail, off the peer's pane.
    let (status, tail) = world.post(&agent("terminal/tail"), &json!({"host": LABEL}));
    assert_eq!(status, 200, "{tail}");
    assert_eq!(tail["session_id"], session.as_str());

    // An interrupt: the Escape reaches the peer's pane, and the turn ends.
    let hook = stop_hook_after_escape(&world, &log, &session);
    let (status, interrupted) = world.post(
        &agent("interrupt"),
        &json!({"idempotency_key": "int-1", "host": LABEL}),
    );
    hook.join().unwrap();
    assert_eq!(status, 200, "{interrupted}");
    assert_eq!(interrupted["host"], LABEL);
    assert_eq!(interrupted["data"]["cancel"], "confirmed", "{interrupted}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("pane send-keys w2:p1 esc"), "{calls}");

    // The prompt, read and answered on the peer's own screen.
    std::fs::write(lab.join("status"), "blocked").unwrap();
    ask_permission(&world, &lab, &session);
    let (status, prompt) = world.post(&agent("prompt"), &json!({"host": LABEL}));
    assert_eq!(status, 200, "{prompt}");
    assert_eq!(
        prompt["prompt"]["command"], "rm -f nothing-here.txt && touch made.txt",
        "{prompt}"
    );
    let screen = prompt["prompt"]["screen"].as_str().unwrap().to_owned();
    let answer = json!({"idempotency_key": "answer-1", "host": LABEL, "option": 4, "screen": screen});
    let (status, answered) = world.post(&agent("prompt/answer"), &answer);
    assert_eq!(status, 200, "{answered}");
    assert_eq!(answered["host"], LABEL);
    assert_eq!(answered["data"]["sent"]["option"], 4, "{answered}");
    let digits = || {
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .filter(|call| *call == "pane send-keys w2:p1 4")
            .count()
    };
    assert_eq!(digits(), 1);
    // The same key again is the stored answer: nothing is typed twice.
    let (status, again) = world.post(&agent("prompt/answer"), &answer);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(digits(), 1);

    // A steer waits in the peer's outbox for the builder.
    std::fs::write(lab.join("status"), "working").unwrap();
    std::fs::write(lab.join("screen"), WORKING).unwrap();
    let _ = std::fs::remove_file(lab.join("after-keys"));
    let _ = std::fs::remove_file(lab.join("typed"));
    let (status, steered) = world.post(
        &agent("steer"),
        &json!({"idempotency_key": "steer-1", "host": LABEL, "text": "use the new API"}),
    );
    assert_eq!(status, 200, "{steered}");
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM outbox WHERE kind = 'steer' AND body LIKE '%use the new API%'"
        ),
        1
    );

    // The peer's orchestrator is stopped through the peer's own starter:
    // its pane is gone, so the stop is proven at once.
    std::fs::write(lab.join("orchestrator-gone"), "").unwrap();
    let orchestrator = world.peer.orchestrator_session.clone();
    let (status, stopped) = world.post(
        &format!("/api/v1/agents/{orchestrator}/stop"),
        &json!({"idempotency_key": "stop-1", "host": LABEL}),
    );
    assert_eq!(status, 200, "{stopped}");
    assert_eq!(stopped["host"], LABEL);
    let ended: Option<String> = world
        .peer
        .open()
        .conn()
        .query_row(
            "SELECT ended_at FROM sessions WHERE id = ?1",
            [&orchestrator],
            |row| row.get(0),
        )
        .unwrap();
    assert!(ended.is_some(), "{stopped}");

    // All of it was written on the peer as the forwarded owner, and none of
    // it on the board host.
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM events WHERE kind = 'outbox.enqueued' AND actor = 'board@test-host'"
        ),
        1
    );
    let written = written_since(&world.board, host_seq);
    assert!(written.is_empty(), "the board host wrote {written:?}");
    assert_eq!(count(&world.board, "SELECT count(*) FROM outbox"), 0);
}

/// The owner pauses, unpauses, resumes and relaunches a run on the peer
/// from the board host, and withdraws an attention item there that no
/// longer applies. Each is a signed `mutate` on the real link, applied by
/// the peer's own store and starter.
#[test]
fn board_pauses_resumes_and_relaunches_a_peer_run() {
    let mut world = World::start();
    let (session, log) = peer_builder(&mut world);
    // The run's worktree is on the peer's disk.
    let worktree = log.parent().unwrap().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    world
        .peer
        .open()
        .conn()
        .execute(
            "UPDATE runs SET worktree_path = ?1",
            [worktree.to_str().unwrap()],
        )
        .unwrap();
    // An item about a session that is gone.
    world
        .peer
        .open()
        .conn()
        .execute(
            "INSERT INTO attention (id, card_id, kind, question, opened_at, subject_session_id)
             SELECT '01M7AEQD3P56M1JB0XWF6BRAT1', id, 'recovery', 'Backup failed', ?1, 'gone'
             FROM cards LIMIT 1",
            [Timestamp::now().to_string()],
        )
        .unwrap();
    world.live();
    let host_seq = world.board.open().last_seq().unwrap();
    let agent = |verb: &str| format!("/api/v1/agents/{session}/{verb}");
    let paused = || -> Option<String> {
        world
            .peer
            .open()
            .conn()
            .query_row(
                "SELECT pause_until FROM runs WHERE id = (SELECT run_id FROM sessions WHERE id = ?1)",
                [&session],
                |row| row.get(0),
            )
            .unwrap()
    };

    let until = Timestamp::now().plus_minutes(60).to_string();
    let (status, body) = world.post(
        &agent("pause"),
        &json!({"idempotency_key": "pause-1", "host": LABEL, "until": until, "reason": "waits on review"}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["host"], LABEL);
    assert!(paused().is_some());

    let (status, body) = world.post(
        &agent("unpause"),
        &json!({"idempotency_key": "unpause-1", "host": LABEL}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(paused(), None);

    // The peer's herdr shows Claude in the run's pane, so the resume
    // leaves it alone.
    let (status, body) = world.post(
        &agent("resume"),
        &json!({"idempotency_key": "resume-1", "host": LABEL}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["host"], LABEL);
    assert_eq!(body["data"]["left_alone"], true, "{body}");

    // The peer's own relaunch judges the run: its worktree is gone from
    // the peer's disk now, so it refuses, and the refusal comes back as the
    // peer's.
    std::fs::remove_dir(&worktree).unwrap();
    let (status, body) = world.post(
        &agent("relaunch"),
        &json!({"idempotency_key": "relaunch-1", "host": LABEL, "note": "start from the tests"}),
    );
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"]["host"], LABEL, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("worktree"),
        "{body}"
    );

    let (status, body) = world.post(
        "/api/v1/attention/01M7AEQD3P56M1JB0XWF6BRAT1/withdraw",
        &json!({"idempotency_key": "withdraw-1", "host": LABEL}),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM attention WHERE withdrawn_at IS NOT NULL"
        ),
        1
    );

    assert_eq!(
        count(
            &world.peer,
            "SELECT count(*) FROM events WHERE kind IN ('run.paused', 'run.unpaused') AND actor = 'board@test-host'"
        ),
        2
    );
    let written = written_since(&world.board, host_seq);
    assert!(written.is_empty(), "the board host wrote {written:?}");
}
