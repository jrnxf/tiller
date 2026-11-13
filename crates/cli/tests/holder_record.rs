//! `tiller holder record` (`terminal-holder.md` 12): the real binary, built
//! with the `record` feature, records a scripted shell pane. No model runs.
//! CI runs this test with `--features record`: no other build has the
//! command.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{Running, World};
use serde_json::Value;
use tiller_panes::message::SyncMode;
use tiller_panes::{HolderClient, Meta, Panes, Size, Spawn, Subscribe, Update, socket_path};

const WAIT: Duration = Duration::from_secs(10);

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_recording_holder_writes_a_scripted_panes_raw_output_and_its_resizes() {
    let world = World::new();
    let out = world.dir.path().join("recordings");
    let log = world.dir.path().join("holder.log");
    let _holder = Running::new(
        world
            .bare_command()
            .args(["holder", "record", "--out"])
            .arg(&out)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap(),
    );
    let socket = socket_path(&world.home());
    let deadline = Instant::now() + WAIT;
    let client = loop {
        match HolderClient::connect(&socket, "test") {
            Ok(client) => break client,
            Err(err) => assert!(
                Instant::now() < deadline,
                "the holder did not listen: {err}\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            ),
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    let pane = client
        .spawn(&Spawn {
            key: "record".to_owned(),
            argv: [
                "/bin/sh",
                "-c",
                "stty -echo; echo ready; read a; echo first-$a; read b; echo second-$b",
            ]
            .map(str::to_owned)
            .to_vec(),
            cwd: world.dir.path().display().to_string(),
            env: BTreeMap::from([("PATH".to_owned(), common::pane_path())]),
            size: Some(Size {
                cols: 120,
                rows: 40,
            }),
            meta: Meta::default(),
        })
        .unwrap();

    use tiller_panes::Live;
    let mut subscription = client
        .subscribe(
            &pane.pane,
            Subscribe {
                sync: SyncMode::Pass,
            },
        )
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
    // The text of the stream so far, until `until` shows in it.
    let mut seen = String::new();
    let mut wait_for = |what: &str, until: &dyn Fn(&Update, &str) -> bool| {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let update = rx
                .recv_timeout(left)
                .unwrap_or_else(|err| panic!("no {what} ({err}), saw {seen:?}"));
            match &update {
                Update::Repaint { bytes, .. } | Update::Output(bytes) => {
                    seen.push_str(&text(bytes));
                }
                _ => {}
            }
            if until(&update, &seen) {
                return;
            }
        }
    };

    wait_for("ready", &|_, seen| seen.contains("ready"));
    client.raw_input(pane.num, b"one\n").unwrap();
    wait_for("first-one", &|_, seen| seen.contains("first-one"));
    handle.resize(100, 30).unwrap();
    wait_for("the resize", &|update, _| {
        matches!(
            update,
            Update::Resized {
                cols: 100,
                rows: 30
            }
        )
    });
    client.raw_input(pane.num, b"two\n").unwrap();
    wait_for("the exit", &|update, _| matches!(update, Update::Exited(_)));

    let raw = std::fs::read(out.join(format!("{}.raw", pane.pane))).unwrap();
    let raw_text = text(&raw);
    let first = raw_text
        .find("first-one")
        .expect("first-one in the raw file");
    let second = raw_text
        .find("second-two")
        .expect("second-two in the raw file");
    assert!(raw_text.starts_with("ready"), "{raw_text:?}");
    assert!(first < second);

    let events: Vec<Value> = std::fs::read_to_string(out.join(format!("{}.jsonl", pane.pane)))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let name = |event: &Value| event["event"].as_str().unwrap().to_owned();
    let names: Vec<String> = events.iter().map(name).collect();
    assert_eq!(names.first().map(String::as_str), Some("start"));
    assert_eq!(names.last().map(String::as_str), Some("exit"));
    assert_eq!(events[0]["cols"], 120);
    assert_eq!(events[0]["rows"], 40);

    // The reads add up to the raw file, one after the other.
    let mut offset = 0;
    for read in events.iter().filter(|event| name(event) == "output") {
        assert_eq!(read["offset"], offset);
        offset += read["len"].as_u64().unwrap();
    }
    assert_eq!(offset as usize, raw.len());
    assert_eq!(events.last().unwrap()["offset"], raw.len());

    // The resize sits between the two lines: the shell printed the first
    // before it, and the second after.
    let resizes: Vec<&Value> = events.iter().filter(|e| name(e) == "resize").collect();
    assert_eq!(resizes.len(), 1, "{events:?}");
    assert_eq!(
        (resizes[0]["cols"].as_u64(), resizes[0]["rows"].as_u64()),
        (Some(100), Some(30))
    );
    let at = usize::try_from(resizes[0]["offset"].as_u64().unwrap()).unwrap();
    assert!(
        first < at && at <= second,
        "resize at {at}, lines at {first} and {second}"
    );
}
