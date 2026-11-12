//! `tiller holder --detach` and `tiller holder upgrade` end to end
//! (`terminal-holder.md` 2.2, 6.2): the real binary on a throwaway home.
//! A detached holder is no child of the test, so a guard stops it by pid.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::World;
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, getsid};
use serde_json::{Value, json};
use tiller_panes::frame::{Frame, read_frame, write_frame};
use tiller_panes::handshake::Welcome;
use tiller_panes::message::HolderMessage;
use tiller_panes::{HolderClient, socket_path};

const WAIT: Duration = Duration::from_secs(10);

/// Stops a detached holder by pid when it drops.
struct Detached(Pid);

impl Drop for Detached {
    fn drop(&mut self) {
        let _ = kill(self.0, Signal::SIGTERM);
        let deadline = Instant::now() + WAIT;
        while kill(self.0, None).is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if kill(self.0, None).is_ok() {
            let _ = kill(self.0, Signal::SIGKILL);
        }
    }
}

fn lock_pid(world: &World) -> i32 {
    std::fs::read_to_string(world.home().join("run/holder.lock"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn holder_command(world: &World, args: &[&str]) -> std::process::Output {
    world
        .bare_command()
        .arg("holder")
        .args(args)
        .env("TILLER_LOG", "debug")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn detach_starts_a_holder_whose_lock_and_socket_work_after_the_parent_exits() {
    let world = World::new();
    let started = Instant::now();
    let out = holder_command(&world, &["--detach"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pid = lock_pid(&world);
    let _guard = Detached(Pid::from_raw(pid));
    assert!(started.elapsed() < WAIT);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&format!("started (pid {pid})")),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The parent has exited, and the holder serves its socket.
    let client = HolderClient::connect(socket_path(&world.home()), "daemon").unwrap();
    client.ping().unwrap();

    // It leads a session of its own, so the terminal's hangup does not reach
    // it, and no terminal is its own.
    let own = getsid(None).unwrap();
    let holder = getsid(Some(Pid::from_raw(pid))).unwrap();
    assert_ne!(holder, own, "the holder shares the caller's session");
    assert_ne!(holder.as_raw(), pid, "the holder leads its session");

    // Its log is a file in the run directory, owner only.
    let log = world.home().join("run/holder.log");
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("the holder is listening")
    );
    let mode =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&log).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);

    // The lock is held: a second holder, detached or not, finds this one.
    for args in [&["--detach"][..], &[][..]] {
        let again = holder_command(&world, args);
        assert!(again.status.success());
        assert!(
            String::from_utf8_lossy(&again.stderr)
                .contains(&format!("a holder already runs (pid {pid})")),
            "{args:?}: {}",
            String::from_utf8_lossy(&again.stderr)
        );
    }
    client.ping().unwrap();
}

#[test]
fn detach_reports_a_holder_that_ends_before_it_answers() {
    let world = World::new();
    // Something answers on the socket path without the lock, so the holder
    // refuses to start (`Occupied`) and ends.
    let run = world.home().join("run");
    std::fs::create_dir_all(&run).unwrap();
    let occupant = UnixListener::bind(run.join("holder.sock")).unwrap();
    // It hangs up on every connect, as a program that is no holder does.
    std::thread::spawn(move || {
        for stream in occupant.incoming() {
            drop(stream);
        }
    });
    let out = holder_command(&world, &["--detach"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the holder ended before it answered"),
        "{stderr}"
    );
    assert!(stderr.contains("holder.log"), "{stderr}");
    let log = std::fs::read_to_string(run.join("holder.log")).unwrap();
    assert!(log.contains("without the holder lock"), "{log}");
}

/// A stand-in holder: it answers the handshake, a ping, and `upgrade` with
/// the message the test chose.
fn fake_holder(socket: &Path, upgrade: Value) {
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let upgrade = upgrade.clone();
            std::thread::spawn(move || {
                while let Ok(Some(Frame::Message(text))) = read_frame(&mut stream) {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let id = request["id"].clone();
                    let reply = match request["type"].as_str().unwrap() {
                        "hello" => serde_json::to_string(&HolderMessage::Welcome(Welcome {
                            proto: 1,
                            holder: "9.9.9".into(),
                            instance: "01FAKEINSTANCE".into(),
                            boot_id: "boot".into(),
                            started_at: jiff::Timestamp::UNIX_EPOCH,
                            features: Vec::new(),
                        }))
                        .unwrap(),
                        "upgrade" => {
                            let mut reply = upgrade.clone();
                            reply["id"] = id;
                            reply.to_string()
                        }
                        _ => json!({"type":"pong","id":id}).to_string(),
                    };
                    if write_frame(&mut stream, &Frame::Message(reply)).is_err() {
                        return;
                    }
                }
            });
        }
    });
}

#[test]
fn upgrade_says_clearly_that_a_holder_without_upgrade_cannot_do_it() {
    let world = World::new();
    let socket = socket_path(&world.home());
    // A holder from before the live upgrade answers it with `unsupported`.
    fake_holder(
        &socket,
        json!({"type":"error","code":"unsupported","message":"upgrade is not supported"}),
    );
    for args in [
        &["holder", "upgrade"][..],
        &["holder", "upgrade", "--dry-run"],
    ] {
        let out = world.run(args);
        assert_eq!(
            (out.code, out.error_code()),
            (5, "unsupported"),
            "{}",
            out.json
        );
        let message = out.json["error"]["message"].as_str().unwrap();
        assert!(message.contains("version 9.9.9"), "{message}");
        assert!(message.contains("does not support upgrade"), "{message}");
        assert!(message.contains("keeps running as it is"), "{message}");
    }
    // It still serves.
    HolderClient::connect(&socket, "daemon")
        .unwrap()
        .ping()
        .unwrap();
}

#[test]
fn upgrade_reports_a_refusal_with_its_reason_and_a_start_with_the_binary() {
    let refused = World::new();
    fake_holder(
        &socket_path(&refused.home()),
        json!({"type":"upgrade_refused","reason":"exec"}),
    );
    let out = refused.run(&["holder", "upgrade"]);
    assert_eq!(
        (out.code, out.error_code()),
        (5, "upgrade_refused"),
        "{}",
        out.json
    );
    let message = out.json["error"]["message"].as_str().unwrap();
    assert!(message.contains("exec"), "{message}");
    assert!(message.contains("keeps running as it is"), "{message}");

    let started = World::new();
    fake_holder(&socket_path(&started.home()), json!({"type":"upgrading"}));
    let tiller = Path::new(env!("CARGO_BIN_EXE_tiller"))
        .canonicalize()
        .unwrap();
    let out = started.must(&["holder", "upgrade"]);
    assert_eq!(out.data()["upgrade"], "started");
    assert_eq!(out.data()["binary"], tiller.to_str().unwrap());
    assert_eq!(out.data()["holder"], "9.9.9");
    let out = started.must(&[
        "holder",
        "upgrade",
        "--dry-run",
        "--binary",
        tiller.to_str().unwrap(),
    ]);
    assert_eq!(out.data()["upgrade"], "ready");
    assert_eq!(out.data()["dry_run"], true);
}

#[test]
fn upgrade_with_no_holder_says_none_answers_and_how_to_start_one() {
    let world = World::new();
    let out = world.run(&["holder", "upgrade"]);
    assert_eq!(
        (out.code, out.error_code()),
        (7, "unavailable"),
        "{}",
        out.json
    );
    assert!(
        out.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no holder answers")
    );
    assert!(
        out.json["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("tiller holder --detach")
    );
}

#[test]
fn upgrade_to_a_binary_that_is_not_there_is_refused_before_any_connection() {
    let world = World::new();
    let out = world.run(&["holder", "upgrade", "--binary", "/home/user/no/such/tiller"]);
    assert_eq!((out.code, out.error_code()), (5, "refused"), "{}", out.json);
}
