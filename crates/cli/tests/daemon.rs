//! T2.8: `tiller daemon` through the real binary: one instance per home, a
//! clean stop on SIGTERM, a restart after SIGKILL, and exit 3 on a database
//! newer than the binary.
//!
//! Each daemon runs with an empty `PATH` and helper programs that do not
//! exist, so no test ever reaches a real herdr server, `gh` or `quota-axi`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::World;
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(20);

struct Daemon {
    child: Child,
    port: u16,
}

fn spawn(world: &World) -> Child {
    let empty = world.dir.path().join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    Command::new(env!("CARGO_BIN_EXE_tiller"))
        .args(["daemon", "--port", "0"])
        .env("TILLER_HOME", world.home())
        .env("PATH", &empty)
        .env("TILLER_HERDR", "/nonexistent/herdr")
        .env("TILLER_QUOTA_AXI", "/nonexistent/quota-axi")
        .env("TILLER_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The first stdout line, or `None` when stdout closes first.
fn first_line(child: &mut Child) -> Option<String> {
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line).unwrap_or(0);
        let _ = tx.send((read > 0).then_some(line));
    });
    rx.recv_timeout(WAIT).expect("the daemon said nothing")
}

fn start(world: &World) -> Daemon {
    let mut child = spawn(world);
    let line = first_line(&mut child).expect("the daemon exited before listening");
    let port = line
        .trim()
        .strip_prefix("listening on http://127.0.0.1:")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .parse()
        .unwrap();
    Daemon { child, port }
}

fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "the daemon did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn terminate(daemon: &mut Daemon) -> ExitStatus {
    let pid = daemon.child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success()
    );
    wait(&mut daemon.child)
}

/// `GET /api/v1/health` over a plain socket.
fn health(port: u16) -> Value {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET /api/v1/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");
    serde_json::from_str(body).unwrap()
}

fn info(world: &World) -> Option<Value> {
    let text = std::fs::read_to_string(world.home().join("daemon.json")).ok()?;
    serde_json::from_str(&text).ok()
}

#[test]
fn a_second_daemon_names_the_first_and_exits_zero() {
    let world = World::initialized();
    let mut first = start(&world);
    let pid = u64::from(first.child.id());
    assert_eq!(health(first.port)["pid"], pid);
    assert_eq!(info(&world).unwrap()["port"], u64::from(first.port));

    let mut second = spawn(&world);
    let line = first_line(&mut second);
    assert!(wait(&mut second).success());
    assert_eq!(
        line.as_deref().map(str::trim),
        Some(format!("tiller daemon already runs as pid {pid}").as_str())
    );

    // SIGTERM stops it cleanly: exit 0, a stop event, no stale pid file.
    assert!(terminate(&mut first).success());
    assert!(info(&world).is_none());
    let kind: String = world
        .store()
        .conn()
        .query_row(
            "SELECT kind FROM events ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(kind, "daemon.stopped");
}

#[test]
fn a_killed_daemon_starts_again_on_the_same_home() {
    let world = World::initialized();
    let mut first = start(&world);
    let killed = first.child.id();
    first.child.kill().unwrap();
    assert!(!wait(&mut first.child).success());
    // The pid file outlives SIGKILL; the lock does not.
    assert_eq!(info(&world).unwrap()["pid"], u64::from(killed));

    let mut again = start(&world);
    let health = health(again.port);
    assert_eq!(health["pid"], u64::from(again.child.id()));
    assert_eq!(health["initialized"], true);
    assert_eq!(info(&world).unwrap()["pid"], u64::from(again.child.id()));
    assert!(terminate(&mut again).success());
}

#[test]
fn a_database_newer_than_the_binary_exits_three() {
    let world = World::initialized();
    let schema: i64 = world
        .store()
        .conn()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    rusqlite::Connection::open(world.db())
        .unwrap()
        .execute_batch(&format!("PRAGMA user_version = {};", schema + 1))
        .unwrap();
    let before = std::fs::read(world.db()).unwrap();

    let mut daemon = spawn(&world);
    let status = wait(&mut daemon);
    assert_eq!(status.code(), Some(3));
    let mut stderr = String::new();
    daemon
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("newer than this binary"), "{stderr}");
    // An older binary never writes to a newer database.
    assert_eq!(std::fs::read(world.db()).unwrap(), before);
    assert!(info(&world).is_none());
}

#[test]
fn a_migration_under_a_running_daemon_stops_it_with_three() {
    let world = World::initialized();
    let mut daemon = start(&world);
    let schema: i64 = rusqlite::Connection::open(world.db())
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    // A newer CLI migrated the database.
    rusqlite::Connection::open(world.db())
        .unwrap()
        .execute_batch(&format!("PRAGMA user_version = {};", schema + 1))
        .unwrap();
    assert_eq!(wait(&mut daemon.child).code(), Some(3));
    assert!(info(&world).is_none());
    // The old binary stopped without its stop event.
    let stops: i64 = rusqlite::Connection::open(world.db())
        .unwrap()
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'daemon.stopped'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stops, 0);
}

/// `GET <path>` over a plain socket: the status line and the body.
fn get(port: u16, path: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_owned(), body.to_owned())
}

#[test]
fn a_daemon_on_an_empty_home_makes_the_database_and_serves_agents() {
    let world = World::new();
    assert!(!world.db().exists());
    let mut daemon = start(&world);

    let (status, _) = get(daemon.port, "/api/v1/agents");
    assert_eq!(status, "HTTP/1.0 200 OK");
    assert_eq!(health(daemon.port)["initialized"], true);
    let hosts: i64 = world
        .store()
        .conn()
        .query_row("SELECT count(*) FROM hosts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(hosts, 1);
    assert!(terminate(&mut daemon).success());

    // A restart finds the database and leaves it alone.
    let host_id: String = world
        .store()
        .conn()
        .query_row("SELECT value FROM meta WHERE key = 'host_id'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut again = start(&world);
    let after: String = world
        .store()
        .conn()
        .query_row("SELECT value FROM meta WHERE key = 'host_id'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(host_id, after);
    assert!(terminate(&mut again).success());
}

#[test]
fn a_daemon_leaves_a_home_owned_by_another_system_uninitialized() {
    let world = World::new();
    std::fs::create_dir_all(world.home()).unwrap();
    std::fs::write(world.home().join("owner"), "firstmate\n").unwrap();
    let mut daemon = start(&world);

    assert!(!world.db().exists());
    let (status, _) = get(daemon.port, "/api/v1/agents");
    assert_eq!(status, "HTTP/1.0 503 Service Unavailable");
    assert!(terminate(&mut daemon).success());
}
