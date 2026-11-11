//! The watcher against a real git repo. Each test drives `look` by hand, so
//! nothing here waits on a clock: a change is written on the first look
//! that finds the repo still.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use axum::http::StatusCode;
use serde_json::{Value, json};
use tiller_core::memory::RULE_SECTION;
use tiller_core::{Store, Timestamp};
use tiller_herdr::testing::Fleet;

use super::Watcher;
use crate::api::tests::{app_with_peers, get, mutation, owner_cookie, send};
use crate::memory::apply::Repo;
use crate::peer::link::Peers;

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_NAME", "Someone")
        .env("GIT_AUTHOR_EMAIL", "someone@example.com")
        .env("GIT_COMMITTER_NAME", "Someone")
        .env("GIT_COMMITTER_EMAIL", "someone@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A memory repo under `home` with one commit of `fleet.md`.
fn repo(home: &Path, text: &str) -> std::path::PathBuf {
    let root = home.join("memory");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    commit(&root, "fleet.md", text);
    root
}

/// Writes `path` and commits it, as someone outside Tiller would.
fn commit(root: &Path, path: &str, text: &str) {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
    git(root, &["add", path]);
    git(
        root,
        &["commit", "--quiet", "--no-gpg-sign", "-m", "by hand"],
    );
}

fn look(watcher: &mut Watcher) -> Option<Vec<String>> {
    watcher.look(Timestamp::now()).unwrap()
}

/// The payloads of every `memory_repo.changed` event, in order.
fn changes(watcher: &Watcher) -> Vec<Value> {
    let conn = watcher.store.conn();
    let mut rows = conn
        .prepare(
            "SELECT entity, payload FROM events WHERE kind = 'memory_repo.changed' ORDER BY seq",
        )
        .unwrap();
    rows.query_map([], |row| {
        let entity: String = row.get(0)?;
        assert_eq!(entity, "memory_repo");
        row.get::<_, String>(1)
    })
    .unwrap()
    .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
    .collect()
}

fn watcher(home: &Path) -> Watcher {
    Watcher::new(home, Store::open_in_memory().unwrap())
}

#[test]
fn an_outside_commit_is_one_change_once_the_repo_is_still() {
    let home = tempfile::tempdir().unwrap();
    let root = repo(home.path(), "# Fleet\n\n- First.\n");
    let mut watcher = watcher(home.path());
    // The first look is the baseline, and a still repo says nothing.
    assert_eq!(look(&mut watcher), None);
    assert_eq!(look(&mut watcher), None);

    commit(&root, "fleet.md", "# Fleet\n\n- Edited by hand.\n");
    assert_eq!(
        look(&mut watcher),
        None,
        "it waits for the repo to go still"
    );
    assert_eq!(look(&mut watcher), Some(vec!["fleet.md".to_owned()]));
    assert_eq!(look(&mut watcher), None);
    assert_eq!(look(&mut watcher), None);

    let head = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(
        changes(&watcher),
        [json!({"head": head.trim(), "files": ["fleet.md"]})]
    );
}

#[test]
fn an_editor_save_is_a_change_and_a_touch_is_not() {
    let home = tempfile::tempdir().unwrap();
    let root = repo(home.path(), "# Fleet\n\n- First.\n");
    let mut watcher = watcher(home.path());
    look(&mut watcher);

    std::fs::write(root.join("fleet.md"), "# Fleet\n\n- Not committed yet.\n").unwrap();
    look(&mut watcher);
    assert_eq!(look(&mut watcher), Some(vec!["fleet.md".to_owned()]));

    // The same bytes written again, as an editor's save with no edit.
    let file = root.join("fleet.md");
    let same = std::fs::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    std::fs::write(&file, same).unwrap();
    look(&mut watcher);
    assert_eq!(look(&mut watcher), None);

    // A new project document, never committed, is a change too.
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::write(root.join("projects/alpha.md"), "# Alpha\n").unwrap();
    std::fs::write(root.join("projects/.alpha.md.swp"), "swap").unwrap();
    look(&mut watcher);
    assert_eq!(
        look(&mut watcher),
        Some(vec!["projects/alpha.md".to_owned()])
    );
    assert_eq!(changes(&watcher).len(), 2);
}

#[test]
fn a_burst_of_writes_is_one_change() {
    let home = tempfile::tempdir().unwrap();
    let root = repo(home.path(), "# Fleet\n");
    let mut watcher = watcher(home.path());
    look(&mut watcher);

    // A pull that lands over three polls.
    commit(&root, "fleet.md", "# Fleet\n\n- One.\n");
    assert_eq!(look(&mut watcher), None);
    commit(&root, "projects/alpha.md", "# Alpha\n");
    assert_eq!(look(&mut watcher), None);
    commit(&root, "projects/beta.md", "# Beta, the longer one\n");
    assert_eq!(look(&mut watcher), None);
    assert_eq!(
        look(&mut watcher),
        Some(vec![
            "fleet.md".to_owned(),
            "projects/alpha.md".to_owned(),
            "projects/beta.md".to_owned(),
        ])
    );
    assert_eq!(changes(&watcher).len(), 1);

    // A repo that never goes still is read after a bounded wait.
    let mut wrote = None;
    for poll in 0..=super::MOST_WAIT + 1 {
        std::fs::write(root.join("fleet.md"), "x".repeat(poll as usize + 1)).unwrap();
        if let Some(files) = look(&mut watcher) {
            wrote = Some(poll);
            assert_eq!(files, ["fleet.md"]);
            break;
        }
    }
    assert_eq!(wrote, Some(super::MOST_WAIT));
}

#[test]
fn a_writer_holding_the_lock_is_waited_for() {
    let home = tempfile::tempdir().unwrap();
    let root = repo(home.path(), "# Fleet\n");
    let mut watcher = watcher(home.path());
    look(&mut watcher);

    let lock = Repo::at(home.path()).lock().unwrap();
    commit(&root, "fleet.md", "# Fleet\n\n- Mid-write.\n");
    for _ in 0..4 {
        assert_eq!(look(&mut watcher), None);
    }
    drop(lock);
    assert_eq!(look(&mut watcher), Some(vec!["fleet.md".to_owned()]));
}

#[test]
fn a_missing_repo_is_quiet_and_a_new_or_replaced_one_is_a_change() {
    let home = tempfile::tempdir().unwrap();
    let mut watcher = watcher(home.path());
    assert_eq!(look(&mut watcher), None);
    assert_eq!(look(&mut watcher), None);
    // The watcher never makes the repo.
    assert!(!home.path().join("memory").exists());

    let root = repo(home.path(), "# Fleet\n");
    look(&mut watcher);
    assert_eq!(look(&mut watcher), Some(vec!["fleet.md".to_owned()]));

    // Replaced by a fresh clone: another history, other text.
    std::fs::remove_dir_all(&root).unwrap();
    repo(home.path(), "# Fleet, cloned again\n");
    look(&mut watcher);
    assert_eq!(look(&mut watcher), Some(vec!["fleet.md".to_owned()]));

    // Removed: no HEAD and no documents.
    std::fs::remove_dir_all(&root).unwrap();
    look(&mut watcher);
    assert_eq!(look(&mut watcher), Some(vec!["fleet.md".to_owned()]));
    let last = changes(&watcher).pop().unwrap();
    assert_eq!(last, json!({"head": null, "files": ["fleet.md"]}));
    assert_eq!(look(&mut watcher), None);
}

/// The board's own edit commits through the same repo; its events already
/// refresh the board, so the watcher writes nothing for it. An editor's
/// save that lands with it still is a change.
#[tokio::test(flavor = "multi_thread")]
async fn the_boards_own_write_is_not_an_outside_change() {
    let fleet = Fleet::new();
    let home = fleet.dir.path();
    let app = app_with_peers(home, Peers::idle());
    let cookie = owner_cookie(home);
    let mut watcher = Watcher::new(home, Store::open(&fleet.db).unwrap());
    look(&mut watcher);

    let edit = |key: &'static str, text: &'static str| {
        let app = app.clone();
        let cookie = cookie.clone();
        async move {
            let view = get(&app, "/api/v1/memory?scope=fleet").await;
            let view: Value = serde_json::from_str(&view.body).unwrap();
            let body = json!({"idempotency_key": key, "project": "fleet", "op": "add",
                              "section": RULE_SECTION, "text": text,
                              "base_blob": view["doc"]["blob"]});
            let answer = send(
                &app,
                mutation("POST", "/api/v1/memory/edits", body, &[("cookie", &cookie)]),
            )
            .await;
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        }
    };
    let root = Repo::at(home).root().to_owned();
    // The first edit makes the repo; the second commits on top of it. Each
    // time the watcher reads the new HEAD and writes nothing.
    for (key, text) in [
        ("k-1", "Keep the first line"),
        ("k-2", "Keep the second line"),
    ] {
        edit(key, text).await;
        look(&mut watcher);
        assert_eq!(look(&mut watcher), None);
        let head = git(&root, &["rev-parse", "HEAD"]);
        let known = watcher.known.as_ref().unwrap();
        assert_eq!(
            known.seen.head.as_deref(),
            Some(head.trim()),
            "it read the commit"
        );
    }
    assert_eq!(changes(&watcher), Vec::<Value>::new());

    // An edit and an editor's save of another document in one window.
    edit("k-3", "Keep the third line").await;
    std::fs::create_dir_all(root.join("projects")).unwrap();
    std::fs::write(root.join("projects/alpha.md"), "# Alpha, by hand\n").unwrap();
    look(&mut watcher);
    assert_eq!(
        look(&mut watcher),
        Some(vec!["fleet.md".to_owned(), "projects/alpha.md".to_owned()])
    );
}

/// An edit made while the daemon is down is seen when it starts again: the
/// watcher compares its first look with the last look before the stop.
#[test]
fn an_edit_made_while_the_daemon_is_down_is_seen_on_start() {
    let fleet = Fleet::new();
    let home = fleet.dir.path();
    let root = repo(home, "# Fleet\n\n- First.\n");
    let mut first = Watcher::new(home, Store::open(&fleet.db).unwrap());
    assert_eq!(look(&mut first), None);
    assert_eq!(look(&mut first), None);
    drop(first);

    // A restart with nothing changed says nothing.
    let mut again = Watcher::new(home, Store::open(&fleet.db).unwrap());
    assert_eq!(look(&mut again), None);
    drop(again);

    // The daemon is down while someone commits by hand.
    commit(
        &root,
        "fleet.md",
        "# Fleet\n\n- Edited while the daemon was down.\n",
    );
    let mut started = Watcher::new(home, Store::open(&fleet.db).unwrap());
    assert_eq!(look(&mut started), Some(vec!["fleet.md".to_owned()]));
    assert_eq!(look(&mut started), None);
    let head = git(&root, &["rev-parse", "HEAD"]);
    assert_eq!(
        changes(&started),
        [json!({"head": head.trim(), "files": ["fleet.md"]})]
    );
    drop(started);

    // The change it wrote is the new last look, so the next start is quiet.
    let mut later = Watcher::new(home, Store::open(&fleet.db).unwrap());
    assert_eq!(look(&mut later), None);
    assert_eq!(changes(&later).len(), 1);
}

/// A same-length rewrite inside one tick of a coarse file clock leaves the
/// stat as it was. The watcher reads such a document again until its stat
/// is older than the clock can blur, so the rewrite is still a change.
#[test]
fn a_rewrite_that_leaves_the_stat_as_it_was_is_still_a_change() {
    let home = tempfile::tempdir().unwrap();
    let root = repo(home.path(), "# Fleet\n\n- One.\n");
    let mut watcher = watcher(home.path());
    look(&mut watcher);

    std::fs::write(root.join("fleet.md"), "# Fleet\n\n- Two.\n").unwrap();
    // As a coarse clock would: the stat after the rewrite is the stat the
    // last look took before it.
    let stat = super::Stamp::take(&watcher.repo);
    watcher.known.as_mut().unwrap().stamp = stat;
    look(&mut watcher);
    assert_eq!(look(&mut watcher), Some(vec!["fleet.md".to_owned()]));
    assert_eq!(look(&mut watcher), None);
}
