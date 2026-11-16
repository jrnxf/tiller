//! The owner CLI socket end to end (UA-03, `authority.md` section 7, Owner
//! CLI socket): two homes on one machine, each with its own `tiller daemon`,
//! paired through an SSH shim that runs the real `tiller peer serve` for the
//! peer's home. The owner's `tiller` on the board host lists the peer's rows
//! and changes the peer's cards through its own daemon's socket.
//!
//! Both daemons are started detached (a shell backgrounds them and exits),
//! so `peer serve`'s start check finds no Claude Code process among its
//! ancestors even when an agent runs this test. The board host's daemon has
//! the test stand-in for the owner (`TILLER_TEST_OWNER`), which checks only
//! the socket's caller and not the test runner's ancestors.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tiller_core::domain::{AttentionKind, CardKind, PeerScope};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::NewCard;
use tiller_core::state::peers::{
    KeyFile, NewPairing, NewPeerKey, generate_key, key_path, public_key_text, write_key_file,
};
use tiller_panes::testing::{Fleet, owner};

mod common;

const WAIT: Duration = Duration::from_secs(30);
const ALIAS: &str = "peer-alias";
const LABEL: &str = "peer";
const QUESTION: &str = "Which database should the peer use?";

fn tiller() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_tiller"))
}

/// A `tiller daemon` on a free port, detached from this process, stopped by
/// its pid when dropped.
struct Daemon {
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
            .env("HOME", home)
            .env("TILLER_HOME", home)
            .env("TILLER_QUOTA_AXI", "/nonexistent/quota-axi")
            .env("TILLER_LOG", "warn")
            .env("TILLER_TEST_OWNER", "1");
        if let Some(ssh) = ssh {
            command.env("TILLER_SSH", ssh);
        }
        assert!(command.status().unwrap().success());
        until("the daemon's listening line", || {
            std::fs::read_to_string(&out)
                .ok()
                .filter(|text| text.starts_with("listening on "))
                .map(|_| ())
        });
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        Self { pid }
    }

    fn stop(&self, home: &Path) {
        signal("-TERM", self.pid);
        let socket = tiller_daemon::socket::socket_path(home);
        until("the daemon to remove its socket", || {
            (!socket.exists()).then_some(())
        });
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
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What the board host's daemon runs as `ssh`: it drops the options, takes
/// the alias and runs the real `tiller peer serve` for the peer's home, the
/// way sshd would on the other machine.
fn write_shim(dir: &Path, peer_home: &Path) -> PathBuf {
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
export HOME="{home}" TILLER_HOME="{home}"
export SSH_CONNECTION="192.0.2.1 50000 192.0.2.2 22"
exec "{tiller}" "$@"
"#,
        home = peer_home.display(),
        tiller = tiller().display(),
    );
    std::fs::write(&shim, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    shim
}

/// One request over the board host's owner socket, as this process.
fn socket_request(home: &Path, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let mut stream = UnixStream::connect(tiller_daemon::socket::socket_path(home)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let body = body.map(Value::to_string).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.0\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

/// Every object in `value` with `host` set to the peer's label.
fn peer_rows(value: &Value) -> Vec<Value> {
    let mut found = Vec::new();
    let mut stack = vec![value];
    while let Some(next) = stack.pop() {
        match next {
            Value::Object(map) => {
                if map.get("host").and_then(Value::as_str) == Some(LABEL) {
                    found.push(next.clone());
                }
                stack.extend(map.values());
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    found
}

/// The board host and its peer, paired, with a daemon each.
struct World {
    board: Fleet,
    peer: Fleet,
    board_daemon: Daemon,
    _peer_daemon: Daemon,
    /// The peer's card, by its display id.
    card: String,
    /// The peer's open question on it.
    item: String,
}

impl World {
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
        let item = store
            .attention_open(
                &owner(),
                None,
                NewAttention {
                    card: card.card.id.clone(),
                    kind: AttentionKind::Decision,
                    question: QUESTION.into(),
                    ..NewAttention::default()
                },
            )
            .unwrap()
            .value;
        drop(store);

        let shim = write_shim(board.dir.path(), peer.dir.path());
        let peer_daemon = Daemon::start(peer.dir.path(), None);
        let board_daemon = Daemon::start(board.dir.path(), Some(&shim));
        let world = Self {
            card: card.card.display_id.clone(),
            item: item.id.clone(),
            board,
            peer,
            board_daemon,
            _peer_daemon: peer_daemon,
        };
        world.live();
        world
    }

    fn home(&self) -> &Path {
        self.board.dir.path()
    }

    /// Waits until the board host's daemon has the peer's records.
    fn live(&self) {
        until("the link to go live", || {
            let (status, machines) = socket_request(self.home(), "GET", "/api/v1/machines", None);
            assert_eq!(status, 200, "{machines}");
            let live = machines["machines"].as_array()?.iter().any(|machine| {
                machine["label"] == LABEL
                    && machine["link"].as_str() == Some("live")
                    && machine["last_seen_at"].is_string()
            });
            let (_, board) = socket_request(self.home(), "GET", "/api/v1/board?filter=all", None);
            (live && !peer_rows(&board).is_empty()).then_some(())
        });
    }

    /// `binary --json <args>` on the board host, as the owner at the test
    /// stand-in terminal.
    fn run_with(&self, binary: &Path, args: &[&str]) -> Value {
        let mut command = Command::new(binary);
        for name in common::CALLER_ENV {
            command.env_remove(name);
        }
        common::isolate(&mut command, self.home());
        let output = command
            .env("TILLER_HOME", self.home())
            .env("TILLER_TEST_OWNER", "1")
            .arg("--json")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        serde_json::from_str(&text).unwrap_or_else(|error| {
            panic!(
                "not JSON ({error}): {text:?}, stderr {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn run(&self, args: &[&str]) -> Value {
        self.run_with(&tiller(), args)
    }
}

const LISTS: [&[&str]; 5] = [
    &["recap"],
    &["board"],
    &["agents"],
    &["ask", "list"],
    &["attention", "list", "--open"],
];

/// With the daemon up, the owner's lists carry the peer's rows, each named
/// by its host, and say nothing is left out. With the daemon down they are
/// this host's alone and say so, as before.
#[test]
fn the_owners_lists_carry_the_peers_rows_through_the_socket() {
    let world = World::start();

    let board = world.run(&["board"]);
    assert_eq!(board["ok"], true, "{board}");
    assert!(board.get("warning").is_none(), "{board}");
    let rows = peer_rows(&board["data"]);
    assert!(
        rows.iter().any(|row| row["title"] == "Peer card"),
        "{board}"
    );

    let attention = world.run(&["attention", "list", "--open"]);
    assert!(attention.get("warning").is_none(), "{attention}");
    assert!(
        peer_rows(&attention["data"])
            .iter()
            .any(|row| row["question"] == QUESTION && row["id"] == world.item),
        "{attention}"
    );

    let recap = world.run(&["recap"]);
    assert!(recap.get("warning").is_none(), "{recap}");
    assert!(
        peer_rows(&recap["data"])
            .iter()
            .any(|row| row["question"] == QUESTION),
        "{recap}"
    );

    for args in [&["agents"][..], &["ask", "list", "--unresolved"]] {
        let out = world.run(args);
        assert_eq!(out["ok"], true, "{out}");
        assert!(out.get("warning").is_none(), "{args:?}: {out}");
    }
    // The link carries a peer's unresolved asks only, and the list says so.
    let asks = world.run(&["ask", "list"]);
    assert_eq!(
        asks["warning"], "Of peer, this lists the unresolved asks only: the link carries no other.",
        "{asks}"
    );

    // A list the link does not carry keeps today's warning.
    let cards = world.run(&["card", "list"]);
    assert!(
        cards["warning"]
            .as_str()
            .unwrap_or_default()
            .contains(LABEL)
    );

    world.board_daemon.stop(world.home());
    for args in LISTS {
        let out = world.run(args);
        assert_eq!(out["ok"], true, "{args:?}: {out}");
        assert!(peer_rows(&out["data"]).is_empty(), "{args:?}: {out}");
        let warning = out["warning"].as_str().unwrap_or_default();
        assert!(
            warning.contains("This lists") && warning.contains(LABEL),
            "{args:?}: {out}"
        );
    }
}

/// The socket serves the owner alone: a caller named `claude` is refused
/// whatever it asks, so its lists are this host's alone; and the owner gets
/// no route outside the allowlist and no change to this host's records.
#[test]
fn a_non_owner_cannot_use_the_socket() {
    let world = World::start();

    let socket = tiller_daemon::socket::socket_path(world.home());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // A copy of `tiller` named as Claude Code is: the daemon reads its name
    // from the kernel and refuses it, and the list falls back.
    let claude = world.board.dir.path().join("claude");
    std::fs::copy(tiller(), &claude).unwrap();
    let board = world.run_with(&claude, &["board"]);
    assert_eq!(board["ok"], true, "{board}");
    assert!(peer_rows(&board["data"]).is_empty(), "{board}");
    assert!(
        board["warning"]
            .as_str()
            .unwrap_or_default()
            .contains("This lists"),
        "{board}"
    );

    // The owner gets only the allowlist, and writes reach peers only.
    let (status, body) = socket_request(world.home(), "GET", "/api/v1/snapshot", None);
    assert_eq!(status, 403, "{body}");
    let (status, body) = socket_request(world.home(), "POST", "/api/v1/auth/session", None);
    assert_eq!(status, 403, "{body}");
    let local = world
        .board
        .open()
        .board_add(
            &owner(),
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: CardKind::Ship,
                title: "Local card".into(),
                ..NewCard::default()
            },
        )
        .unwrap()
        .value;
    let (status, body) = socket_request(
        world.home(),
        "POST",
        &format!("/api/v1/cards/{}/comment", local.card.id),
        Some(&json!({"idempotency_key": "k1", "text": "From the socket"})),
    );
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "forbidden", "{body}");
    let _ = &world.peer;
}
