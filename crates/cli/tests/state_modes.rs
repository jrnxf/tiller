//! P57: the state directory and every file in it are owner-only, even when the
//! caller's umask would let the group write.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use common::World;

/// Every entry under `dir` with its mode, the directory itself included.
fn modes(dir: &Path, found: &mut Vec<(std::path::PathBuf, u32)>) {
    let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
    found.push((dir.to_owned(), mode));
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            modes(&path, found);
        } else {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            found.push((path, mode));
        }
    }
}

/// P57: `tiller init` under umask 002 leaves a 0700 state dir and a 0600
/// database (and 0600 sidecar files), in a home that did not exist before.
#[test]
fn p57_init_creates_owner_only_state_under_a_permissive_umask() {
    let world = World::new();
    let db = world
        .dir
        .path()
        .join("fresh")
        .join("state")
        .join("state.db");
    let mut sh = Command::new("sh");
    common::isolate(&mut sh, world.dir.path());
    let output = sh
        .args(["-c", "umask 002; exec \"$0\" \"$@\""])
        .arg(env!("CARGO_BIN_EXE_tiller"))
        .arg("--json")
        .arg("--db")
        .arg(&db)
        .args(["init", "--host-label", "host-a", "--lane-capacity", "2"])
        .env("TILLER_HOME", world.home())
        .env_remove("TILLER_DB")
        .env_remove("TILLER_SESSION_ID")
        .env_remove("CLAUDECODE")
        .env("TILLER_TEST_OWNER", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let state = db.parent().unwrap();
    let mut found = Vec::new();
    modes(state, &mut found);
    assert!(found.iter().any(|(path, _)| path == &db), "{found:?}");
    for (path, mode) in &found {
        let want = if path.is_dir() { 0o700 } else { 0o600 };
        assert_eq!(*mode, want, "{} is {mode:o}, want {want:o}", path.display());
    }
}
