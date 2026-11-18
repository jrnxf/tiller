//! `tiller project sync`: the real binary fetches the default branch and
//! fast-forwards a clean checkout, for the owner and for the project's own
//! orchestrator, and leaves a checkout with work in progress as it is.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::process::Command;

use common::World;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

/// alpha's checkout on `main`, one commit behind its remote. Returns the
/// checkout and the remote's head; the dirs live under the world's.
fn behind(world: &World) -> (std::path::PathBuf, String) {
    let root = world.dir.path();
    let remote = root.join("remote.git");
    let checkout = root.join("alpha");
    let other = root.join("other");
    for dir in [&remote, &checkout, &other] {
        std::fs::create_dir_all(dir).unwrap();
    }
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    let url = remote.display().to_string();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["remote", "add", "origin", &url]);
    commit(&checkout, "base");
    git(&checkout, &["push", "-q", "origin", "main"]);
    git(&other, &["clone", "-q", &url, "."]);
    let new = commit(&other, "merged");
    git(&other, &["push", "-q", "origin", "main"]);
    let path = checkout.display().to_string();
    assert!(world.run(&["config", "set", "checkouts.alpha", &path]).ok());
    (checkout, new)
}

#[test]
fn the_owner_and_the_orchestrator_bring_the_checkout_up_to_the_remote() {
    let world = World::initialized();
    let (checkout, new) = behind(&world);
    let synced = world.run(&["project", "sync", "alpha"]);
    assert_eq!(synced.code, 0, "{}", synced.json);
    assert_eq!(synced.data()["result"], "advanced", "{}", synced.json);
    assert_eq!(synced.data()["to"], new.as_str());
    assert_eq!(git(&checkout, &["rev-parse", "main"]), new);
    assert_eq!(git(&checkout, &["rev-parse", "origin/main"]), new);
    assert!(checkout.join("merged").exists());

    let orchestrator = world.orchestrator();
    let again = world.run_as(&orchestrator, &["project", "sync", "alpha"]);
    assert_eq!(again.code, 0, "{}", again.json);
    assert_eq!(again.data()["result"], "current", "{}", again.json);
}

#[test]
fn work_in_progress_holds_the_checkout_and_names_why() {
    let world = World::initialized();
    let (checkout, new) = behind(&world);
    std::fs::write(checkout.join("base"), "edited").unwrap();
    let held = world.run(&["project", "sync", "alpha"]);
    assert_eq!(held.code, 0, "{}", held.json);
    assert_eq!(held.data()["result"], "held", "{}", held.json);
    assert!(
        held.data()["why"]
            .as_str()
            .is_some_and(|why| why.contains("uncommitted")),
        "{}",
        held.json
    );
    assert_eq!(held.data()["remote"], new.as_str());
    assert_ne!(git(&checkout, &["rev-parse", "main"]), new);
    assert_eq!(
        std::fs::read_to_string(checkout.join("base")).unwrap(),
        "edited"
    );
}

#[test]
fn a_project_with_no_checkout_is_refused_and_a_worker_may_not_sync() {
    let world = World::initialized();
    let none = world.run(&["project", "sync", "alpha"]);
    assert!(!none.ok(), "{}", none.json);
    assert_eq!(none.error_code(), "refused", "{}", none.json);

    let (_checkout, _new) = behind(&world);
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    world.orchestrator();
    let (_, worker) = world.builder_run("alpha-1");
    let refused = world.run_as(&worker, &["project", "sync", "alpha"]);
    assert!(!refused.ok(), "{}", refused.json);
    assert_eq!(refused.error_code(), "forbidden", "{}", refused.json);
}
