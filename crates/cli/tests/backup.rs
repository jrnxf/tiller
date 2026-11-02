//! T2.3: `tiller backup`, `restore`, `export` and `migrate` through the real
//! binary, each with a snapshot of its JSON output and its exit code.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Out, World, snap};
use serde_json::Value;
use tiller_core::backup::{DaemonLock, Paths};

/// Replaces what differs between runs (backup stamps, hashes, sizes and the
/// temporary home) before the snapshot.
fn stable(world: &World, mut out: Out) -> Out {
    let home = world.home().display().to_string();
    let export = world.dir.path().display().to_string();
    fn walk(value: &mut Value, home: &str, export: &str) {
        match value {
            Value::String(text) => {
                if text.starts_with("backups/") && text.ends_with(".db") {
                    *text = "backups/<stamp>.db".into();
                } else if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
                    *text = "<sha256>".into();
                } else {
                    let mut scrubbed = text.replace(home, "<home>").replace(export, "<tmp>");
                    if let Some(at) = scrubbed.find(".pre-restore-") {
                        scrubbed.truncate(at);
                        scrubbed.push_str(".pre-restore-<stamp>");
                    }
                    *text = scrubbed;
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| walk(item, home, export)),
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if key == "size" {
                        *item = Value::String("<size>".into());
                    } else {
                        walk(item, home, export);
                    }
                }
            }
            _ => {}
        }
    }
    walk(&mut out.json, &home, &export);
    out
}

fn step(world: &World, name: &str, args: &[&str]) -> Out {
    let out = world.run(args);
    assert_eq!(out.code, 0, "{name}: {}", out.json);
    let out = stable(world, out);
    snap(world, name, &out);
    out
}

fn fails(world: &World, code: i32, name: &str, args: &[&str]) -> Out {
    let out = world.run(args);
    assert_eq!(out.code, code, "{name}: {}", out.json);
    let out = stable(world, out);
    snap(world, name, &out);
    out
}

#[test]
fn backup_restore_export_and_migrate_commands() {
    let world = World::initialized();
    world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "Before the backup",
    ]);

    let out = step(&world, "backup", &["backup"]);
    assert_eq!(out.data()["outcome"], "done");

    // A card the backup does not hold.
    world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "After the backup",
    ]);

    let out = step(&world, "restore_dry_run", &["restore", "--dry-run"]);
    assert_eq!(out.data()["dry_run"], true);
    assert_eq!(
        world
            .must(&["board"])
            .json
            .to_string()
            .matches("After the backup")
            .count(),
        1
    );

    // The daemon holds its lock: restore refuses.
    let lock = DaemonLock::acquire(&Paths::in_home(world.home())).unwrap();
    fails(&world, 5, "restore_while_daemon_runs", &["restore"]);
    drop(lock);

    let session = world.orchestrator();
    let out = world.run_as(&session, &["restore"]);
    assert_eq!(out.code, 6, "{}", out.json);

    let out = step(&world, "restore", &["restore"]);
    assert!(
        out.data()["moved_to"]
            .as_str()
            .unwrap()
            .contains(".pre-restore-")
    );
    let board = world.must(&["board"]).json.to_string();
    assert!(board.contains("Before the backup"));
    assert!(!board.contains("After the backup"));

    let export = world.dir.path().join("export");
    let export_arg = export.to_str().unwrap();
    let out = step(&world, "export", &["export", "--jsonl", export_arg]);
    assert_eq!(out.data()["format"], "tiller-export/v1");
    fails(
        &world,
        5,
        "restore_jsonl_over_existing",
        &["restore", "--jsonl", export_arg, "--boundary", "personal"],
    );
    let other = World::new();
    let out = other.run(&["restore", "--jsonl", export_arg, "--boundary", "personal"]);
    assert_eq!(out.code, 0, "{}", out.json);
    assert!(
        other
            .must(&["board"])
            .json
            .to_string()
            .contains("Before the backup")
    );

    step(&world, "migrate_up", &["migrate", "up"]);
    fails(
        &world,
        2,
        "migrate_down_to_latest",
        &["migrate", "down", "--to", "1"],
    );
}
