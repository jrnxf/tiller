//! `tiller mem ...` end to end: the real binary, one `state.db` and a fake
//! kb command in a temp dir (`memory.md` section 4). A fleet session
//! suggests a note; only the owner lists, edits, keeps or discards it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::os::unix::fs::PermissionsExt;

use common::World;
use serde_json::Value;

/// A pending note's text, which must reach no fleet CLI output.
const CANARY: &str = "canary-51ad pending text";

/// A world whose alpha has a repo scope and a checkout, with `kb_command`
/// a stand-in script: it records its input and arguments, or refuses with
/// `refuse`. Never the owner's own kb.
fn kb_world(refuse: bool) -> World {
    let world = World::initialized();
    world
        .store()
        .conn()
        .execute(
            "UPDATE projects SET remote = 'git@example.com:example/alpha.git' WHERE id = 'alpha'",
            [],
        )
        .unwrap();
    let root = world.dir.path();
    let checkout = root.join("checkouts/alpha");
    std::fs::create_dir_all(&checkout).unwrap();
    world.must(&[
        "config",
        "set",
        "checkouts.alpha",
        checkout.to_str().unwrap(),
    ]);
    let out = root.display();
    let body = if refuse {
        "cat > /dev/null\nprintf '{\"error\":\"a note with this title exists\"}'\nexit 1\n"
            .to_owned()
    } else {
        format!(
            "cat > {out}/input.json\nprintf '%s ' \"$@\" > {out}/args\npwd > {out}/cwd\n\
             printf '{{\"id\":\"never-rebase\",\"status\":\"added\"}}'\n"
        )
    };
    let script = root.join("fake-kb");
    std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let config = world.home().join("config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap_or_default();
    text.push_str(&format!(
        "\n[memory]\nkb_command = [{:?}]\n",
        script.display().to_string()
    ));
    std::fs::write(&config, text).unwrap();
    world.set_env("KB_HOME", root.join("kb").to_str().unwrap());
    world
}

/// A live builder session of alpha, under a live orchestrator.
fn builder(world: &World) -> String {
    world.orchestrator();
    let card = world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "the export",
    ]);
    let tag = card.data()["display_id"].as_str().unwrap().to_owned();
    world.builder_run(&tag).1
}

fn pending(world: &World) -> Vec<Value> {
    world.must(&["mem", "pending"]).data()["pending"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn a_fleet_note_waits_until_the_owner_edits_and_keeps_it() {
    let world = kb_world(false);
    let session = builder(&world);
    let noted = world.run_with_stdin_as(
        &session,
        &[
            "mem",
            "note",
            "--kind",
            "gotcha",
            "--title",
            "Never rebase a pushed branch",
        ],
        "A rebase loses the reviewer's comments.\n",
    );
    assert!(noted.ok(), "{}", noted.json);
    let id = noted.data()["id"].as_str().unwrap().to_owned();
    assert_eq!(noted.data()["source"], "fleet");
    assert_eq!(
        noted.data()["body"],
        "A rebase loses the reviewer's comments."
    );

    // The owner does not suggest notes; the session does not see the queue.
    let owner = world.run(&[
        "mem", "note", "--kind", "gotcha", "--title", "x", "--body", "y",
    ]);
    assert_eq!(owner.code, 6, "{}", owner.json);
    for args in [
        vec!["mem", "pending"],
        vec!["mem", "edit", &id, "--title", "x"],
        vec!["mem", "keep", &id],
        vec!["mem", "discard", &id],
        vec!["mem", "live", "--project", "alpha"],
    ] {
        let refused = world.run_as(&session, &args);
        assert_eq!(refused.code, 6, "{args:?}: {}", refused.json);
    }

    let listed = pending(&world);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], id.as_str());
    let edited = world.must(&[
        "mem",
        "edit",
        &id,
        "--title",
        "Never rebase a branch once pushed",
    ]);
    assert_eq!(edited.data()["rev"], 2, "{}", edited.json);

    let kept = world.must(&["mem", "keep", &id, "--key", "keep-1"]);
    assert_eq!(kept.data()["note_id"], "never-rebase", "{}", kept.json);
    assert_eq!(kept.data()["scope"], "example.com/example/alpha");
    assert!(pending(&world).is_empty());
    let root = world.dir.path();
    let input: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("input.json")).unwrap()).unwrap();
    assert_eq!(input["title"], "Never rebase a branch once pushed");
    assert_eq!(input["scope"], "example.com/example/alpha");
    let args = std::fs::read_to_string(root.join("args")).unwrap();
    assert!(args.starts_with("add --json --repo "), "{args}");
    // A retry under the same key replays and runs no add.
    std::fs::remove_file(root.join("input.json")).unwrap();
    let again = world.must(&["mem", "keep", &id, "--key", "keep-1"]);
    assert!(again.replayed(), "{}", again.json);
    assert!(!root.join("input.json").exists());
}

#[test]
fn a_refused_keep_leaves_the_note_pending_and_discard_drops_it() {
    let world = kb_world(true);
    let session = builder(&world);
    let noted = world.must_as(
        &session,
        &[
            "mem", "note", "--kind", "gotcha", "--title", "t", "--body", "b",
        ],
    );
    let id = noted.data()["id"].as_str().unwrap().to_owned();
    let refused = world.run(&["mem", "keep", &id]);
    assert_eq!(refused.code, 5, "{}", refused.json);
    assert!(
        refused.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("a note with this title exists"),
        "{}",
        refused.json
    );
    assert_eq!(pending(&world).len(), 1);
    let dropped = world.must(&["mem", "discard", &id]);
    assert!(dropped.ok());
    assert!(pending(&world).is_empty());
}

/// Spec section 7: a pending note's text reaches no fleet CLI output, nor
/// any event a session can read, before or after it is discarded.
#[test]
fn a_pending_note_reaches_no_fleet_output() {
    let world = kb_world(false);
    let session = builder(&world);
    let noted = world.run_with_stdin_as(
        &session,
        &["mem", "note", "--kind", "gotcha", "--title", CANARY],
        CANARY,
    );
    let id = noted.data()["id"].as_str().unwrap().to_owned();
    let check = |when: &str| {
        for args in [
            vec!["snapshot"],
            vec!["events", "--limit", "200"],
            vec!["card", "list", "alpha"],
            vec!["mem", "pending"],
            vec!["mem", "live", "--project", "alpha"],
        ] {
            let out = world.run_as(&session, &args);
            assert!(
                !out.json.to_string().contains("canary-51ad"),
                "{when}: {args:?}: {}",
                out.json
            );
        }
    };
    check("pending");
    world.must(&["mem", "discard", &id]);
    check("discarded");
}

#[test]
fn live_shows_what_a_launch_loads_now() {
    let world = kb_world(false);
    let notes = world.dir.path().join("kb/notes/global");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("small-commits.md"),
        "---\nid: small-commits\ntitle: Small commits\nscope: global\nkind: convention\n\
         created: 2026-11-10T09:00:00Z\nlast_verified: 2026-11-10T09:00:00Z\n\
         confidence: medium\nindexed: true\n---\nCommit in small steps.\n",
    )
    .unwrap();
    let live = world.must(&["mem", "live", "--project", "alpha", "--role", "scout"]);
    let data = live.data();
    assert_eq!(data["role"], "scout");
    assert_eq!(data["kb"]["state"], "ok", "{data}");
    assert_eq!(data["live"][0]["id"], "small-commits");
    assert_eq!(data["live"][0]["loaded"], true);
    assert!(data["budget"]["used"].as_i64().unwrap() > 0);
    let bad = world.run(&["mem", "live", "--role", "owner"]);
    assert_eq!(bad.code, 2, "{}", bad.json);
}
