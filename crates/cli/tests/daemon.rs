//! T2.8: `tiller daemon` through the real binary: one instance per home, a
//! clean stop on SIGTERM, a restart after SIGKILL, and exit 3 on a database
//! newer than the binary.
//!
//! Each daemon runs with an empty `PATH` and helper programs that do not
//! exist, so no test ever reaches a real herdr server, `gh` or `quota-axi`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use common::{Running, World, first_line, listening_port};
use serde_json::Value;

const WAIT: Duration = Duration::from_secs(20);

struct Daemon {
    child: Running,
    port: u16,
}

fn spawn(world: &World, args: &[&str]) -> Running {
    Running::new(
        daemon_command(world)
            .args(args)
            .env("TILLER_HERDR", "/nonexistent/herdr")
            .spawn()
            .unwrap(),
    )
}

/// The daemon command without `TILLER_HERDR`, so `config.toml` names herdr.
fn daemon_command(world: &World) -> Command {
    let empty = world.dir.path().join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let mut command = world.bare_command();
    command
        .args(["daemon", "--port", "0"])
        .env("PATH", &empty)
        .env_remove("TILLER_HERDR")
        .env("TILLER_QUOTA_AXI", "/nonexistent/quota-axi")
        .env("TILLER_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn start(world: &World) -> Daemon {
    start_with(world, &[])
}

/// `start` with extra `tiller daemon` arguments.
fn start_with(world: &World, args: &[&str]) -> Daemon {
    let mut child = spawn(world, args);
    let port = listening_port(&mut child);
    Daemon { child, port }
}

fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        // The guard stops it, with what it started, as the panic unwinds.
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

/// A raw HTTP/1.0 request to the daemon: the response head and body.
fn exchange(port: u16, request: &str) -> (String, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    (head.to_owned(), body.to_owned())
}

/// `GET /api/v1/health` over a plain socket.
fn health(port: u16) -> Value {
    let (head, body) = exchange(
        port,
        &format!("GET /api/v1/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");
    serde_json::from_str(&body).unwrap()
}

fn info(world: &World) -> Option<Value> {
    let text = std::fs::read_to_string(world.home().join("daemon.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// B4: the daemon derives its lock, peers, backups and mirror from
/// `TILLER_HOME`, so a `--db` (or `TILLER_DB`) that names another database is
/// refused, not half honored. The refusal is a usage error (exit 2) that names
/// `TILLER_HOME`, and nothing is created.
#[test]
fn a_db_other_than_the_home_database_is_refused() {
    let world = World::new();
    let other = world.dir.path().join("other.db");
    let mut child = spawn(&world, &["--db", other.to_str().unwrap()]);
    let status = wait(&mut child);
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("TILLER_HOME"), "{stderr}");
    assert!(stderr.contains("--db"), "{stderr}");
    assert!(!other.exists(), "the daemon created the named database");
    assert!(!world.home().exists(), "the daemon created a default home");

    // The environment variable is the same flag.
    let mut child = Running::new(
        daemon_command(&world)
            .env("TILLER_HERDR", "/nonexistent/herdr")
            .env("TILLER_DB", &other)
            .spawn()
            .unwrap(),
    );
    assert_eq!(wait(&mut child).code(), Some(2));
    assert!(!other.exists());
}

/// The one `--db` the daemon accepts is the database it opens anyway.
#[test]
fn the_home_database_is_accepted_for_the_daemon() {
    let world = World::initialized();
    let db = world.db();
    let mut daemon = start_with(&world, &["--db", db.to_str().unwrap()]);
    assert!(terminate(&mut daemon).success());
}

#[test]
fn a_second_daemon_names_the_first_and_exits_zero() {
    let world = World::initialized();
    let mut first = start(&world);
    let pid = u64::from(first.child.id());
    assert_eq!(health(first.port)["pid"], pid);
    assert_eq!(info(&world).unwrap()["port"], u64::from(first.port));

    let mut second = spawn(&world, &[]);
    let line = first_line(&mut second, WAIT);
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

    let mut daemon = spawn(&world, &[]);
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
    let (head, body) = exchange(
        port,
        &format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    (head.lines().next().unwrap().to_owned(), body)
}

/// `POST <path>` with a JSON body, as the signed-in board sends it.
fn post_as_board(world: &World, port: u16, path: &str, body: &str) -> (String, Value) {
    let now = tiller_core::Timestamp::now();
    let token = tiller_daemon::auth::mint_login(&world.home(), now)
        .unwrap()
        .token;
    let cookie = tiller_daemon::auth::redeem_login(&world.home(), &token, now)
        .unwrap()
        .unwrap();
    let (head, body) = exchange(
        port,
        &format!(
            "POST {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\n\
             Sec-Fetch-Site: same-origin\r\nCookie: {}={cookie}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            tiller_daemon::auth::COOKIE,
            body.len()
        ),
    );
    (
        head.lines().next().unwrap().to_owned(),
        serde_json::from_str(&body).unwrap(),
    )
}

/// The board's Start orchestrator is `tiller orchestrator start` run as the
/// board: the same checks refuse it, and a refused start writes nothing.
#[test]
fn the_board_starts_an_orchestrator_through_the_command_start() {
    let world = World::initialized();
    let mut daemon = start(&world);
    let sessions = || -> i64 {
        world
            .store()
            .conn()
            .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
            .unwrap()
    };
    let before = sessions();

    let path = "/api/v1/projects/alpha/orchestrator";
    let (status, answer) = post_as_board(&world, daemon.port, path, r#"{"idempotency_key":"k1"}"#);
    assert_eq!(status, "HTTP/1.0 400 Bad Request", "{answer}");
    assert_eq!(answer["error"]["code"], "usage");
    assert!(
        answer["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("checkouts.alpha")),
        "{answer}"
    );
    let path = "/api/v1/projects/nope/orchestrator";
    let (status, answer) = post_as_board(&world, daemon.port, path, r#"{"idempotency_key":"k1"}"#);
    assert_eq!(status, "HTTP/1.0 404 Not Found", "{answer}");
    assert_eq!(sessions(), before, "a refused start writes nothing");
    assert!(terminate(&mut daemon).success());
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

/// A daemon that started before anyone ran `tiller init` names the host after
/// the machine. `tiller init` with another label refuses, so `tiller host
/// edit --label` is how the owner renames it.
#[test]
fn a_self_initialized_host_takes_a_new_label_and_boundary() {
    let world = World::new();
    let mut daemon = start(&world);
    assert_eq!(health(daemon.port)["initialized"], true);
    assert!(terminate(&mut daemon).success());

    let refused = world.run(&["init", "--host-label", "work-mac", "--lane-capacity", "2"]);
    assert_eq!(refused.error_code(), "refused", "{}", refused.json);
    assert!(
        refused.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("tiller host edit --label"),
        "{}",
        refused.json
    );

    let edited = world.must(&["host", "edit", "--rev", "1", "--label", "work-mac"]);
    assert_eq!(edited.data()["label"], "work-mac");
    assert_eq!(edited.data()["rev"], 2);
    // `tiller init` with the new label is now the natural replay.
    world.must(&["init", "--host-label", "work-mac", "--lane-capacity", "2"]);
    // A replay never changes the boundary, so it does not answer ok as if
    // it had.
    let work = world.run(&[
        "init",
        "--host-label",
        "work-mac",
        "--lane-capacity",
        "2",
        "--boundary",
        "work",
    ]);
    assert_eq!(work.error_code(), "refused", "{}", work.json);
    assert!(
        work.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already keeps the personal boundary"),
        "{}",
        work.json
    );

    // Nothing records the boundary yet, so the owner may still pick it.
    let moved = world.must(&["host", "edit", "--rev", "2", "--boundary", "work"]);
    assert_eq!(moved.data()["rev"], 3);
    world.must(&[
        "init",
        "--host-label",
        "work-mac",
        "--lane-capacity",
        "2",
        "--boundary",
        "work",
    ]);
    // A floor the owner set clears again, as the board's empty field does.
    let floor = world.must(&["host", "edit", "--rev", "3", "--ram-floor-mb", "4096"]);
    assert_eq!(floor.data()["ram_floor_mb"], 4096);
    let cleared = world.must(&["host", "edit", "--rev", "4", "--no-ram-floor"]);
    assert!(cleared.data()["ram_floor_mb"].is_null(), "{}", cleared.json);
    assert_eq!(cleared.data()["rev"], 5);
    // A backup records it: from then on it stays.
    world.must(&["backup"]);
    let kept = world.run(&["host", "edit", "--rev", "5", "--boundary", "personal"]);
    assert_eq!(kept.error_code(), "refused", "{}", kept.json);
    assert!(
        kept.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("a backup already records the work boundary"),
        "{}",
        kept.json
    );
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

#[test]
fn the_daemon_drives_the_herdr_spawn_names() {
    let world = World::initialized();
    // A herdr command with a leading argument, like a named session's
    // wrapper; it records every call and answers none.
    let calls = world.dir.path().join("herdr-calls");
    let herdr = world.dir.path().join("herdr");
    std::fs::write(
        &herdr,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n", calls.display()),
    )
    .unwrap();
    std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    world.must(&[
        "config",
        "set",
        "spawn.herdr",
        &format!("[\"{}\", \"lab\"]", herdr.display()),
    ]);

    // Every failed call logs; nothing reads stderr here, so it must not fill.
    let child = Running::new(
        daemon_command(&world)
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut daemon = Daemon { child, port: 0 };
    let deadline = Instant::now() + WAIT;
    let seen = loop {
        let seen = std::fs::read_to_string(&calls).unwrap_or_default();
        if !seen.is_empty() || Instant::now() > deadline {
            break seen;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(terminate(&mut daemon).success());
    let first = seen
        .lines()
        .next()
        .unwrap_or_else(|| panic!("the daemon never ran spawn.herdr"));
    assert!(first.starts_with("lab "), "{seen}");
}

/// A herdr stand-in that records its arguments, led by `name`.
fn recording_herdr(world: &World, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let calls = world.dir.path().join(format!("{name}-calls"));
    let herdr = world.dir.path().join(name);
    std::fs::write(
        &herdr,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n", calls.display()),
    )
    .unwrap();
    std::fs::set_permissions(&herdr, std::fs::Permissions::from_mode(0o755)).unwrap();
    (herdr, calls)
}

fn wait_for_file(path: &std::path::Path) -> bool {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

#[test]
fn the_daemon_follows_a_changed_spawn_herdr_without_a_restart() {
    let world = World::initialized();
    let (first, first_calls) = recording_herdr(&world, "first");
    let (second, second_calls) = recording_herdr(&world, "second");
    let set = |herdr: &std::path::Path| {
        world.must(&[
            "config",
            "set",
            "spawn.herdr",
            &format!("[\"{}\"]", herdr.display()),
        ]);
    };
    set(&first);
    let child = Running::new(
        daemon_command(&world)
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut daemon = Daemon { child, port: 0 };
    assert!(
        wait_for_file(&first_calls),
        "the daemon never ran spawn.herdr"
    );

    set(&second);
    let followed = wait_for_file(&second_calls);
    // A half-written table keeps the herdr in use rather than blinding the
    // daemon.
    std::fs::write(world.home().join("config.toml"), "[spawn\n").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        daemon.child.try_wait().unwrap().is_none(),
        "the daemon died"
    );
    assert!(terminate(&mut daemon).success());
    assert!(
        followed,
        "the daemon kept driving the herdr it started with"
    );
}

#[test]
fn the_daemon_reads_only_spawn_and_needs_its_herdr() {
    let world = World::initialized();
    // Only [spawn] is read: a dispatch table spawn would refuse is no bar.
    std::fs::write(
        world.home().join("config.toml"),
        "[dispatch]\nmodels = [\"haiku\"]\n[dispatch.default]\nmodel = \"opus\"\neffort = \"low\"\n",
    )
    .unwrap();
    let mut daemon = start(&world);
    assert!(terminate(&mut daemon).success());

    std::fs::write(world.home().join("config.toml"), "[spawn]\nherdr = []\n").unwrap();
    let mut daemon = Running::new(daemon_command(&world).spawn().unwrap());
    let status = wait(&mut daemon);
    assert_eq!(status.code(), Some(1));
    let mut stderr = String::new();
    daemon
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("spawn: herdr names a command"), "{stderr}");
    assert!(info(&world).is_none());
}

/// `tiller board --login` names each `--allow-host` name with an https link;
/// the one token signs in once at any of them, with a `Secure` cookie at the
/// proxy's name, and that cookie writes only from the name's https origin.
#[test]
fn the_login_link_signs_in_once_at_an_allowed_host() {
    let world = World::initialized();
    // Matched as the browser sends it: lowercase, without the https port.
    let mut daemon = start_with(&world, &["--allow-host", "Tiller.Example.com:443"]);
    let port = daemon.port;
    assert_eq!(
        info(&world).unwrap()["allowed_hosts"],
        serde_json::json!(["tiller.example.com"])
    );

    let out = world.run(&["board", "--login"]);
    assert!(out.ok(), "{}", out.json);
    let local = out.data()["url"].as_str().unwrap().to_owned();
    let token = local
        .strip_prefix(&format!("http://127.0.0.1:{port}/login/"))
        .unwrap_or_else(|| panic!("{local}"));
    assert_eq!(
        out.data()["allowed_host_urls"],
        serde_json::json!([format!("https://tiller.example.com/login/{token}")])
    );
    assert_eq!(out.data()["expires_in_minutes"], 1);

    let login = format!("GET /login/{token} HTTP/1.0\r\nHost: tiller.example.com\r\n\r\n");
    let (head, _) = exchange(port, &login);
    assert!(head.starts_with("HTTP/1.0 303"), "{head}");
    let set = head
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap_or_else(|| panic!("no cookie: {head}"));
    assert!(set.ends_with("; Secure"), "{set}");
    let cookie = set.split(';').next().unwrap().to_owned();
    // The link works once, wherever it is used next.
    let (again, _) = exchange(port, &login);
    assert!(again.starts_with("HTTP/1.0 403"), "{again}");
    let (local_again, _) = exchange(
        port,
        &format!("GET /login/{token} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(local_again.starts_with("HTTP/1.0 403"), "{local_again}");

    let ask = |origin: &str| {
        let body = r#"{"idempotency_key":"k1","text":"From the phone"}"#;
        exchange(
            port,
            &format!(
                "POST /api/v1/projects/alpha/asks HTTP/1.0\r\nHost: tiller.example.com\r\n\
                 Origin: {origin}\r\nSec-Fetch-Site: same-origin\r\nCookie: {cookie}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let (refused, body) = ask("http://tiller.example.com");
    assert!(refused.starts_with("HTTP/1.0 403"), "{refused} {body}");
    let (written, body) = ask("https://tiller.example.com");
    assert!(written.starts_with("HTTP/1.0 200"), "{written} {body}");

    assert!(terminate(&mut daemon).success());
}

/// At an `--allow-host` name a signed-out browser gets nothing but the
/// sign-in screen; the code `tiller board --login` prints signs it in, and
/// signing out ends that cookie. Loopback reads stay open, and the daemon
/// never logs the code, even at its most verbose.
#[test]
fn the_login_code_signs_in_at_an_allowed_host_until_sign_out() {
    let world = World::initialized();
    let mut child = Running::new(
        daemon_command(&world)
            .args(["--allow-host", "tiller.example.com"])
            .env("TILLER_HERDR", "/nonexistent/herdr")
            .env("TILLER_LOG", "trace")
            .spawn()
            .unwrap(),
    );
    let port = listening_port(&mut child);
    let mut daemon = Daemon { child, port };
    let read = |cookie: &str| {
        exchange(
            port,
            &format!(
                "GET /api/v1/board HTTP/1.0\r\nHost: tiller.example.com\r\nCookie: {cookie}\r\n\r\n"
            ),
        )
    };
    let post = |path: &str, cookie: &str, body: &str| {
        exchange(
            port,
            &format!(
                "POST {path} HTTP/1.0\r\nHost: tiller.example.com\r\n\
                 Origin: https://tiller.example.com\r\nSec-Fetch-Site: same-origin\r\n\
                 Cookie: {cookie}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    };

    let (head, body) = read("tiller_owner=none");
    assert!(head.starts_with("HTTP/1.0 403"), "{head}");
    assert!(body.contains("signed_out"), "{body}");
    // The signed-out page carries no snapshot. A build without board/dist,
    // as in the Nix test sandbox, answers 503 with how to build it instead.
    let (page, html) = exchange(port, "GET / HTTP/1.0\r\nHost: tiller.example.com\r\n\r\n");
    assert!(
        page.starts_with("HTTP/1.0 200")
            || (page.starts_with("HTTP/1.0 503") && html.contains("bun run build")),
        "{page}"
    );
    assert!(!html.contains("tiller-boot"));

    let out = world.run(&["board", "--login"]);
    assert!(out.ok(), "{}", out.json);
    let code = out.data()["code"].as_str().unwrap().to_owned();
    assert!(
        code.len() == 8 && code.bytes().all(|b| b.is_ascii_digit()),
        "{code}"
    );
    let (wrong, body) = post(
        "/api/v1/login",
        "tiller_owner=none",
        &format!(r#"{{"code":"{}"}}"#, next_code(&code)),
    );
    assert!(wrong.starts_with("HTTP/1.0 403"), "{wrong}");
    assert!(body.contains("wrong_code"), "{body}");
    let (signed, body) = post(
        "/api/v1/login",
        "tiller_owner=none",
        &format!(r#"{{"code":"{code}"}}"#),
    );
    assert!(signed.starts_with("HTTP/1.0 200"), "{signed} {body}");
    let set = signed
        .lines()
        .find_map(|line| line.strip_prefix("set-cookie: "))
        .unwrap_or_else(|| panic!("no cookie: {signed}"));
    assert!(set.ends_with("; Secure"), "{set}");
    let cookie = set.split(';').next().unwrap().to_owned();
    let (head, _) = read(&cookie);
    assert!(head.starts_with("HTTP/1.0 200"), "{head}");

    let (out, body) = post("/api/v1/logout", &cookie, "{}");
    assert!(out.starts_with("HTTP/1.0 200"), "{out} {body}");
    assert!(out.contains("Max-Age=0"), "{out}");
    let (head, _) = read(&cookie);
    assert!(head.starts_with("HTTP/1.0 403"), "{head}");
    // Loopback reads need no cookie, as before.
    let (local, _) = exchange(
        port,
        &format!("GET /api/v1/board HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(local.starts_with("HTTP/1.0 200"), "{local}");

    assert!(terminate(&mut daemon).success());
    let mut logs = String::new();
    daemon
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut logs)
        .unwrap();
    assert!(!logs.contains(&code), "{logs}");
    assert!(!logs.contains(&next_code(&code)), "{logs}");
}

/// The board is public, so a stranger can flood the sign-in code through
/// the proxy. That spends only the stranger's own tries: the owner's fresh
/// `tiller board --login` code still signs in from the owner's address.
#[test]
fn a_stranger_flooding_codes_never_locks_the_owner_out() {
    let world = World::initialized();
    let mut daemon = start_with(&world, &["--allow-host", "tiller.example.com"]);
    let port = daemon.port;
    let enter = |from: &str, code: &str| {
        let body = format!(r#"{{"code":"{code}"}}"#);
        exchange(
            port,
            &format!(
                "POST /api/v1/login HTTP/1.0\r\nHost: tiller.example.com\r\n\
                 Origin: https://tiller.example.com\r\nSec-Fetch-Site: same-origin\r\n\
                 X-Forwarded-For: {from}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    let mint = || {
        let out = world.run(&["board", "--login"]);
        assert!(out.ok(), "{}", out.json);
        out.data()["code"].as_str().unwrap().to_owned()
    };

    let code = mint();
    let mut limited = false;
    for _ in 0..20 {
        let (head, body) = enter("198.51.100.7", &next_code(&code));
        assert!(head.starts_with("HTTP/1.0 403") || head.starts_with("HTTP/1.0 429"));
        limited = head.starts_with("HTTP/1.0 429");
        assert!(!limited || body.contains("too_many_tries"), "{body}");
    }
    assert!(limited, "the stranger was never limited");
    let (signed, body) = enter("192.0.2.10", &code);
    assert!(signed.starts_with("HTTP/1.0 200"), "{signed} {body}");
    // And a code minted after the flood works too.
    let (signed, body) = enter("192.0.2.10", &mint());
    assert!(signed.starts_with("HTTP/1.0 200"), "{signed} {body}");

    assert!(terminate(&mut daemon).success());
}

/// The 8-digit code after `code`, so never `code` itself.
fn next_code(code: &str) -> String {
    let code: u32 = code.parse().unwrap();
    format!("{:08}", (code + 1) % 100_000_000)
}

#[test]
fn an_allow_host_that_is_not_a_plain_name_is_refused() {
    let world = World::initialized();
    let out = daemon_command(&world)
        .args(["--allow-host", "*.example.com"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--allow-host"), "{stderr}");
}

/// A home whose `spawn.claude` is a stand-in curator with `body`, and one
/// full retro of a closed card. Returns the stand-in's path and the retro.
fn curator_home(world: &World, body: &str) -> (std::path::PathBuf, String) {
    world.must(&["config", "set", "learning.sample_every", "1"]);
    let scratch = world.dir.path().join("scratch");
    world.must(&[
        "config",
        "set",
        "spawn.scratch_root",
        scratch.to_str().unwrap(),
    ]);
    let fake = world.dir.path().join("fake-claude");
    std::fs::write(&fake, body).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    world.must(&["config", "set", "spawn.claude", fake.to_str().unwrap()]);
    let card = world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "Undo strokes",
    ]);
    let id = card.data()["id"].as_str().unwrap().to_owned();
    let tag = card.data()["display_id"].as_str().unwrap().to_owned();
    world
        .store()
        .conn()
        .execute(
            "UPDATE cards SET state = 'closed', outcome = 'landed', closed_at = '2026-11-03T10:00:00.000Z',
                 created_at = '2026-11-03T08:00:00.000Z', closed_by = 'owner' WHERE id = ?1",
            [&id],
        )
        .unwrap();
    world.must(&["retro", "sweep"]);
    let retro = world.must(&["retro", "show", &tag]).data()["retro"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (fake, retro)
}

/// A log the stand-in curators add a line to on each run, so a test counts
/// the runs.
struct RunLog(std::path::PathBuf);

impl RunLog {
    fn new(world: &World) -> Self {
        Self(world.dir.path().join("curator-runs"))
    }

    /// A stand-in curator that records its run, then runs `rest`.
    fn curator(&self, rest: &str) -> String {
        let path = self.0.display().to_string().replace('\'', "'\\''");
        format!("#!/bin/sh\necho run >> '{path}'\n{rest}")
    }

    fn count(&self) -> usize {
        std::fs::read_to_string(&self.0)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }
}

/// Asks for a reconcile pass with Run now, and waits until one that
/// started at or after `asked` is over. Run now answers with the poller's
/// health after the pass, or as it stands when the pass takes longer; then
/// `GET /api/v1/health` is read until it shows the pass.
fn reconciled_after(world: &World, port: u16, asked: tiller_core::Timestamp) {
    let after = |poller: &Value| {
        poller["last_attempt_at"]
            .as_str()
            .and_then(|at| at.parse::<tiller_core::Timestamp>().ok())
            .is_some_and(|at| at.as_millis() >= asked.as_millis())
    };
    let key = format!(r#"{{"idempotency_key":"reconcile-{}"}}"#, asked.as_millis());
    let (status, answer) = post_as_board(world, port, "/api/v1/pollers/reconcile/run", &key);
    assert_eq!(status, "HTTP/1.0 200 OK", "{answer}");
    if after(&answer["data"]) {
        return;
    }
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, body) = get(port, "/api/v1/health");
        let health: Value = serde_json::from_str(&body).unwrap();
        let passed = health["pollers"]
            .as_array()
            .and_then(|pollers| pollers.iter().find(|p| p["source"] == "reconcile"))
            .is_some_and(after);
        if passed {
            return;
        }
        assert!(Instant::now() < deadline, "no reconcile pass ran: {body}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The board's view of `retro` from `GET /api/v1/retros`.
fn retro_view(port: u16, retro: &str) -> Value {
    let (status, body) = get(port, "/api/v1/retros");
    assert_eq!(status, "HTTP/1.0 200 OK", "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    list["retros"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == retro)
        .cloned()
        .unwrap_or_else(|| panic!("{retro} is not listed: {list}"))
}

/// Waits for the board's run of `retro` to end, and returns its view.
fn settled(port: u16, retro: &str) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let view = retro_view(port, retro);
        if view["status"] != "running" {
            return view;
        }
        assert!(Instant::now() < deadline, "the run never ended: {view}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run_again(world: &World, port: u16, retro: &str, key: &str) -> (String, Value) {
    post_as_board(
        world,
        port,
        &format!("/api/v1/retros/{retro}/run"),
        &format!(r#"{{"idempotency_key":"{key}"}}"#),
    )
}

/// Run again on the board is `tiller retro run` started by the daemon: a
/// second failure shows its own reason, nothing runs it again by itself,
/// and a curator that finishes ends it.
#[test]
fn the_board_runs_an_unfinished_retro_again() {
    let world = World::initialized();
    let runs = RunLog::new(&world);
    let (fake, retro) = curator_home(&world, &runs.curator("exit 3\n"));
    let ran = world.must(&["retro", "run", &retro]);
    assert_eq!(ran.data()["retro"]["outcome"], "unfinished", "{}", ran.json);
    let mut daemon = start(&world);
    let view = retro_view(daemon.port, &retro);
    assert_eq!(view["status"], "unfinished", "{view}");
    assert_eq!(
        view["learning"],
        "curator exited 3 without finishing the retro"
    );
    assert!(view["run_refusal"].is_null(), "{view}");

    assert_eq!(runs.count(), 1);

    std::fs::write(&fake, runs.curator("exit 4\n")).unwrap();
    let (status, answer) = run_again(&world, daemon.port, &retro, "again-1");
    assert_eq!(status, "HTTP/1.0 200 OK", "{answer}");
    assert_eq!(answer["data"]["state"], "pending", "{answer}");
    let view = settled(daemon.port, &retro);
    assert_eq!(view["status"], "unfinished", "{view}");
    assert_eq!(
        view["learning"],
        "curator exited 4 without finishing the retro"
    );
    assert_eq!(view["started_by"], "board", "{view}");
    assert_eq!(runs.count(), 2, "one click ran one curator");
    // Nothing runs it again by itself. Only Run again starts a curator, and
    // its thread ends with the run. The one worker that comes back to retros
    // on its own is the learning sweep in the reconcile pass, which only
    // schedules them, so a whole pass that started after the run ended is
    // the proof, not a wait. A start there would claim the retro in the
    // store first, so the status and the start would show it.
    reconciled_after(&world, daemon.port, tiller_core::Timestamp::now());
    let later = retro_view(daemon.port, &retro);
    assert_eq!(later["status"], "unfinished", "{later}");
    assert_eq!(later["curator_started_at"], view["curator_started_at"]);
    assert_eq!(runs.count(), 2, "a curator ran that no click started");

    std::fs::write(
        &fake,
        runs.curator(&format!(
            "\"{tiller}\" retro finish \"$TILLER_RETRO_ID\" --nothing >/dev/null\n",
            tiller = env!("CARGO_BIN_EXE_tiller")
        )),
    )
    .unwrap();
    let (status, answer) = run_again(&world, daemon.port, &retro, "again-2");
    assert_eq!(status, "HTTP/1.0 200 OK", "{answer}");
    let view = settled(daemon.port, &retro);
    assert_eq!(view["status"], "done", "{view}");
    assert_eq!(view["outcome"], "nothing", "{view}");
    // A done retro is not run again.
    assert!(view["run_refusal"].is_string(), "{view}");
    let (status, answer) = run_again(&world, daemon.port, &retro, "again-3");
    assert_eq!(status, "HTTP/1.0 422 Unprocessable Entity", "{answer}");
    assert!(terminate(&mut daemon).success());
    assert_eq!(runs.count(), 3, "one curator for each click that ran one");
}

/// A retried Run again answers the same retro after the daemon restarts
/// (`protocol.md` 7.4): the store keeps the key, so no second curator runs
/// for one click.
#[test]
fn a_replayed_run_again_after_a_restart_starts_no_second_curator() {
    let world = World::initialized();
    let runs = RunLog::new(&world);
    let (_fake, retro) = curator_home(&world, &runs.curator("exit 4\n"));
    let mut daemon = start(&world);
    let (status, first) = run_again(&world, daemon.port, &retro, "click-1");
    assert_eq!(status, "HTTP/1.0 200 OK", "{first}");
    assert_eq!(first["replayed"], false, "{first}");
    let view = settled(daemon.port, &retro);
    assert_eq!(view["status"], "unfinished", "{view}");
    assert_eq!(runs.count(), 1);
    assert!(terminate(&mut daemon).success());
    // A start clears the curator's scratch directory and writes its launch
    // there before it answers, and a curator starts only from a written
    // launch. So a marker that is still there once the answer is in proves
    // that the replay neither prepared nor started a curator.
    let marker = world
        .dir
        .path()
        .join("scratch")
        .join("retros")
        .join(&retro)
        .join("first-run-marker");
    std::fs::write(&marker, "first run").unwrap();

    let mut daemon = start(&world);
    let (status, again) = run_again(&world, daemon.port, &retro, "click-1");
    assert_eq!(status, "HTTP/1.0 200 OK", "{again}");
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(again["data"]["id"], retro.as_str(), "{again}");
    assert_eq!(again["events"], serde_json::json!([]), "{again}");
    assert!(marker.exists(), "the replay prepared the curator again");
    let later = settled(daemon.port, &retro);
    assert_eq!(runs.count(), 1, "a second curator ran for one click");
    assert_eq!(later["status"], "unfinished", "{later}");
    assert_eq!(later["curator_started_at"], view["curator_started_at"]);
    // A new click runs it again.
    let (status, fresh) = run_again(&world, daemon.port, &retro, "click-2");
    assert_eq!(status, "HTTP/1.0 200 OK", "{fresh}");
    assert_eq!(fresh["replayed"], false, "{fresh}");
    settled(daemon.port, &retro);
    assert_eq!(runs.count(), 2);
    assert!(terminate(&mut daemon).success());
}

/// A Run again whose curator cannot be prepared ends the retro
/// `unfinished` with why, so it can run again at once. The end is not
/// refused for reusing the click's key.
#[test]
fn a_run_again_that_cannot_prepare_its_curator_ends_unfinished() {
    let world = World::initialized();
    let (_fake, retro) = curator_home(&world, "#!/bin/sh\nexit 0\n");
    // A file where the curator's scratch directories go.
    std::fs::create_dir_all(world.dir.path().join("scratch")).unwrap();
    std::fs::write(world.dir.path().join("scratch").join("retros"), "").unwrap();
    let mut daemon = start(&world);
    let (status, answer) = run_again(&world, daemon.port, &retro, "click-1");
    assert_ne!(status, "HTTP/1.0 200 OK", "{answer}");
    let view = retro_view(daemon.port, &retro);
    assert_eq!(view["status"], "unfinished", "{view}");
    let learning = view["learning"].as_str().unwrap();
    assert!(
        learning.starts_with("the curator could not be prepared: "),
        "{view}"
    );
    assert!(learning.len() > "the curator could not be prepared: ".len());
    assert!(terminate(&mut daemon).success());
}

/// A Run again whose curator cannot be started keeps the cause and its
/// hint on the retro: the board answered before the start failed, so the
/// row is the only place the owner sees them.
#[test]
fn a_run_again_that_cannot_start_its_curator_says_why() {
    let world = World::initialized();
    let (fake, retro) = curator_home(&world, "#!/bin/sh\nexit 0\n");
    std::fs::remove_file(&fake).unwrap();
    let mut daemon = start(&world);
    let (status, answer) = run_again(&world, daemon.port, &retro, "click-1");
    assert_eq!(status, "HTTP/1.0 200 OK", "{answer}");
    let view = settled(daemon.port, &retro);
    assert_eq!(view["status"], "unfinished", "{view}");
    let learning = view["learning"].as_str().unwrap();
    assert!(
        learning.starts_with("the curator could not be started: cannot start "),
        "{view}"
    );
    assert!(
        learning.ends_with("; set spawn.claude in config.toml"),
        "{view}"
    );
    assert!(terminate(&mut daemon).success());
}

/// A daemon that stops ends a curator the board started, with what the
/// curator started, and leaves its retro ready to run again.
#[test]
fn a_stopping_daemon_stops_the_curator_the_board_started() {
    let world = World::initialized();
    // The daemon runs with an empty PATH, so the stand-in names `sleep` in
    // full.
    let which = Command::new("sh")
        .args(["-c", "command -v sleep"])
        .output()
        .unwrap();
    let sleep = String::from_utf8(which.stdout).unwrap().trim().to_owned();
    let pid_file = |name: &str| world.dir.path().join(name);
    let (_fake, retro) = curator_home(
        &world,
        &format!(
            "#!/bin/sh\n\"{sleep}\" 300 &\necho $! > '{grandchild}'\necho $$ > '{curator}'\nwait\n",
            grandchild = pid_file("grandchild.pid").display(),
            curator = pid_file("curator.pid").display(),
        ),
    );
    let mut daemon = start(&world);
    let (status, answer) = run_again(&world, daemon.port, &retro, "run-1");
    assert_eq!(status, "HTTP/1.0 200 OK", "{answer}");
    let curator = written_pid(&pid_file("curator.pid"));
    let grandchild = written_pid(&pid_file("grandchild.pid"));
    let view = retro_view(daemon.port, &retro);
    assert_eq!(view["status"], "running", "{view}");
    // Run again is off while it runs.
    assert!(view["run_refusal"].is_string(), "{view}");

    let stopped = terminate(&mut daemon);
    // The daemon reaps its curator before it exits, so the curator is gone
    // the moment it has. Its child got SIGKILL with the curator's group.
    let ended = gone(&curator, Duration::ZERO) && gone(&grandchild, DYING);
    // Whatever happened, take every process this test started down with it.
    reap_all(&[&curator, &grandchild]);
    assert!(stopped.success());
    assert!(ended, "the curator or its child outlived the daemon");
    let shown = world.must(&["retro", "show", &retro]);
    let row = &shown.data()["retro"];
    assert_eq!(row["outcome"], "unfinished", "{}", shown.json);
    assert_eq!(
        row["learning"],
        "curator stopped: the daemon stopped before the retro was finished"
    );
}

/// The pid a stand-in wrote to `path`, once the whole line is there.
fn written_pid(path: &std::path::Path) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.ends_with('\n') {
            return text.trim().to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "{} was never written",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How long a process killed with SIGKILL may still show in `ps`: the
/// signal arrives a moment after `kill` returns.
const DYING: Duration = Duration::from_secs(5);

/// Whether `pid` is gone, or only a zombie, within `within`: a process
/// stopped after its parent is reaped by whatever adopted it.
fn gone(pid: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        let out = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&out.stdout);
        if stat.trim().is_empty() || stat.trim_start().starts_with('Z') {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Kills each of `pids` that still runs, so a failed check leaves none
/// behind. A pid that has ended already is not signalled.
fn reap_all(pids: &[&str]) {
    for pid in pids {
        if !gone(pid, Duration::ZERO) {
            let _ = Command::new("kill").args(["-KILL", pid]).status();
        }
    }
}

/// A test that fails while the board's curator runs leaves nothing behind:
/// dropping the guard stops the daemon, which stops the curator and the
/// `sleep` it started, though the curator leads a process group of its own.
#[test]
fn a_dropped_daemon_leaves_no_curator_behind() {
    let world = World::initialized();
    let which = Command::new("sh")
        .args(["-c", "command -v sleep"])
        .output()
        .unwrap();
    let sleep = String::from_utf8(which.stdout).unwrap().trim().to_owned();
    let pid_file = |name: &str| world.dir.path().join(name);
    let (_fake, retro) = curator_home(
        &world,
        &format!(
            "#!/bin/sh\n\"{sleep}\" 300 &\necho $! > '{grandchild}'\necho $$ > '{curator}'\nwait\n",
            grandchild = pid_file("grandchild.pid").display(),
            curator = pid_file("curator.pid").display(),
        ),
    );
    let daemon = start(&world);
    let (status, answer) = run_again(&world, daemon.port, &retro, "run-1");
    assert_eq!(status, "HTTP/1.0 200 OK", "{answer}");
    let curator = written_pid(&pid_file("curator.pid"));
    let grandchild = written_pid(&pid_file("grandchild.pid"));
    drop(daemon);
    let ended = (gone(&curator, DYING), gone(&grandchild, DYING));
    reap_all(&[&curator, &grandchild]);
    assert_eq!(
        ended,
        (true, true),
        "the curator or its child outlived the guard"
    );
}

/// Set in the copy of this test binary that
/// `a_guard_kills_a_child_that_ignores_sigterm_and_its_groups` starts: the
/// folder it writes its children's pids to.
const GUARD_CHILD: &str = "TILLER_GUARD_CHILD";

/// The guard kills a child that ignores SIGTERM, with what it started: a
/// process that leads a group of its own, as a curator does, and one whose
/// parent exits during the grace, which leaves it under no one.
///
/// A shell without a terminal cannot give a job its own group (`set -m`
/// turns job control off), so the child is a copy of this test binary,
/// started through a shell that ignores SIGTERM, which `exec` keeps.
#[test]
fn a_guard_kills_a_child_that_ignores_sigterm_and_its_groups() {
    if let Some(dir) = std::env::var_os(GUARD_CHILD) {
        use std::os::unix::process::CommandExt;
        let dir = std::path::PathBuf::from(dir);
        let mut leader = Command::new("sleep")
            .arg("300")
            .process_group(0)
            .spawn()
            .unwrap();
        std::fs::write(dir.join("leader"), format!("{}\n", leader.id())).unwrap();
        let mut plain = Command::new("sleep").arg("300").spawn().unwrap();
        std::fs::write(dir.join("plain"), format!("{}\n", plain.id())).unwrap();
        // It exits once `go` is there, which leaves its `sleep` an orphan.
        let mut parent = Command::new("sh")
            .args([
                "-c",
                "sleep 300 & echo $! > orphan; while [ ! -e go ]; do sleep 0.05; done",
            ])
            .current_dir(&dir)
            .spawn()
            .unwrap();
        // Waits until the guard kills them all.
        let _ = parent.wait();
        let _ = plain.wait();
        let _ = leader.wait();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let child = Command::new("sh")
        .args([
            "-c",
            "trap '' TERM; exec \"$0\" --exact \"$1\" --test-threads 1",
        ])
        .arg(std::env::current_exe().unwrap())
        .arg("a_guard_kills_a_child_that_ignores_sigterm_and_its_groups")
        .env(GUARD_CHILD, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let running = Running::with_grace(child, Duration::from_secs(1));
    let leader = written_pid(&dir.path().join("leader"));
    let plain = written_pid(&dir.path().join("plain"));
    let orphan = written_pid(&dir.path().join("orphan"));
    let group = Command::new("ps")
        .args(["-o", "pgid=", "-p", &leader])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&group.stdout).trim(),
        leader,
        "the stand-in does not lead a group of its own"
    );
    // The orphan's parent exits while the guard waits out the grace, after
    // the guard has seen the orphan under the child.
    let go = dir.path().join("go");
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(go, "").unwrap();
    });
    drop(running);
    late.join().unwrap();
    let ended = (
        gone(&leader, DYING),
        gone(&plain, DYING),
        gone(&orphan, DYING),
    );
    reap_all(&[&leader, &plain, &orphan]);
    assert_eq!(
        ended,
        (true, true, true),
        "a process the child started outlived the guard"
    );
}
