//! `cli::idempotency::same_key_twice_has_no_second_effect` (`protocol.md`
//! section 11): every keyed command, run twice with one key, has no second
//! effect; natural commands replay; a session's retry inside one turn
//! replays and the same command after the next turn boundary is new.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Out, World};
use tiller_core::Timestamp;
use tiller_core::domain::{Activity, ObservationSource};
use tiller_core::state::runs::NewObservation;

fn rows(world: &World) -> Vec<(String, i64)> {
    let store = world.store();
    let tables: Vec<String> = store
        .conn()
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'idempotency_keys' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let count = store
                .conn()
                .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table, count)
        })
        .collect()
}

/// Runs `args` twice as `session` (or the owner) and checks the second run
/// replayed the first and wrote nothing.
fn twice(world: &World, session: Option<&str>, args: &[&str]) -> (Out, Out) {
    let run = |args: &[&str]| match session {
        Some(session) => world.run_as(session, args),
        None => world.run(args),
    };
    let first = run(args);
    assert!(first.ok() && first.code == 0, "{args:?}: {}", first.json);
    assert!(!first.replayed(), "{args:?} replayed on its first run");
    let before = (rows(world), world.store().last_seq().unwrap());
    let second = run(args);
    assert!(
        second.ok() && second.code == 0,
        "{args:?} again: {}",
        second.json
    );
    assert!(
        second.replayed(),
        "{args:?} did not replay: {}",
        second.json
    );
    assert_eq!(
        second.data(),
        first.data(),
        "{args:?} replayed a different response"
    );
    assert_eq!(second.events(), first.events());
    assert_eq!(
        (rows(world), world.store().last_seq().unwrap()),
        before,
        "{args:?} had a second effect"
    );
    (first, second)
}

#[test]
fn same_key_twice_has_no_second_effect() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let (run, worker) = world.builder_run("alpha-1");

    let note = twice(
        &world,
        None,
        &["ask", "note", "alpha", "--text", "hello", "--key", "k-note"],
    )
    .0;
    let ask = note.data()["ask"]["id"].as_str().unwrap().to_owned();
    world.must(&["ask", "resolve", &ask, "chat"]);
    let ask2 = world
        .must(&["ask", "note", "alpha", "--text", "second"])
        .data()["ask"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    world.must(&[
        "ask", "resolve", &ask2, "new", "--kind", "ship", "--title", "Two",
    ]);
    twice(
        &world,
        None,
        &[
            "ask", "link", &ask2, "new", "--kind", "scout", "--title", "Three", "--key", "k-link",
        ],
    );
    twice(
        &world,
        None,
        &[
            "card", "create", "alpha", "--kind", "ops", "--title", "Four", "--key", "k-card",
        ],
    );
    twice(
        &world,
        None,
        &[
            "card",
            "comment",
            "alpha-1",
            "--text",
            "note",
            "--key",
            "k-comment",
        ],
    );
    let item = twice(
        &world,
        None,
        &[
            "attention",
            "open",
            "alpha-1",
            "--kind",
            "decision",
            "--question",
            "Q?",
            "--key",
            "k-att",
        ],
    )
    .0;
    let item_id = item.data()["id"].as_str().unwrap().to_owned();
    twice(
        &world,
        None,
        &[
            "attention",
            "answer",
            &item_id,
            "--text",
            "yes",
            "--key",
            "k-ans",
        ],
    );
    twice(
        &world,
        None,
        &[
            "send", &run, "--kind", "note", "--text", "hi", "--key", "k-send",
        ],
    );
    twice(
        &world,
        None,
        &[
            "artifact",
            "add",
            "alpha-1",
            "--kind",
            "page",
            "--title",
            "P",
            "--url",
            "https://example.com/p",
            "--key",
            "k-art",
        ],
    );
    twice(
        &world,
        None,
        &[
            "review",
            "record",
            &run,
            "--reviewer",
            "human",
            "--coverage",
            "full",
            "--verdict",
            "pass",
            "--key",
            "k-rev",
        ],
    );
    twice(
        &world,
        None,
        &[
            "obligation",
            "open",
            "alpha-1",
            "--what",
            "w",
            "--on",
            "pr.checks",
            "--key",
            "k-obl",
        ],
    );
    twice(
        &world,
        None,
        &[
            "grant",
            "create",
            "alpha",
            "--kind",
            "merge",
            "--conditions",
            r#"{"v":1,"actions":["squash"]}"#,
            "--key",
            "k-grant",
        ],
    );
    twice(
        &world,
        None,
        &[
            "away",
            "start",
            "--until",
            "2099-03-01",
            "--projects",
            "alpha",
            "--key",
            "k-away",
        ],
    );
    twice(
        &world,
        Some(&orchestrator),
        &[
            "handoff",
            "write",
            "--conclusions",
            "c",
            "--next",
            "n",
            "--key",
            "k-hand",
        ],
    );

    // A worker's status line takes a derived key, so a retry in one turn replays.
    twice(&world, Some(&worker), &["status", "working", "reading"]);
}

#[test]
fn a_key_is_not_reusable_for_another_command() {
    let world = World::initialized();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One", "--key", "shared",
    ]);
    let out = world.run(&[
        "card", "comment", "alpha-1", "--text", "x", "--key", "shared",
    ]);
    assert_eq!((out.code, out.error_code()), (5, "refused"));
}

#[test]
fn natural_commands_replay_without_a_key() {
    let world = World::initialized();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "Two",
    ]);
    let project_rev = world.must(&["project", "list"]).data()["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["id"] == "alpha")
        .unwrap()["rev"]
        .to_string();
    for args in [
        vec![
            "card",
            "hold",
            "alpha-1",
            "--until",
            "2099-01-05",
            "--reason",
            "wait",
        ],
        vec!["card", "dep", "add", "alpha-1", "alpha-2"],
        vec![
            "card",
            "close",
            "alpha-1",
            "--outcome",
            "abandoned",
            "--reason",
            "dropped",
        ],
        vec!["card", "edit", "alpha-2", "--rev", "1", "--title", "Two"],
        vec![
            "project",
            "edit",
            "alpha",
            "--rev",
            &project_rev,
            "--name",
            "Alpha",
        ],
        vec!["config", "set", "answer", "42"],
    ] {
        let first = world.must(&args);
        let seq = world.store().last_seq().unwrap();
        let again = world.must(&args);
        // The first run of a no-op edit already replays; every other one writes once.
        assert!(again.replayed(), "{args:?} did not replay: {}", again.json);
        assert_eq!(
            world.store().last_seq().unwrap(),
            seq,
            "{args:?} wrote twice"
        );
        let _ = first;
    }
}

#[test]
fn a_shell_caller_gets_a_minted_key_and_repeating_it_replays() {
    let world = World::initialized();
    let first = world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let key = first.json["key"]
        .as_str()
        .expect("the response prints the minted key")
        .to_owned();
    assert!(key.starts_with("shell:"));

    let replay = world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One", "--key", &key,
    ]);
    assert!(replay.replayed());
    assert_eq!(replay.data()["id"], first.data()["id"]);
    // Without the key the same command creates again, by design.
    let again = world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    assert!(!again.replayed());
    assert_eq!(again.data()["display_id"], "alpha-2");
}

#[test]
fn a_sessions_retry_replays_within_a_turn_and_is_new_after_the_next_boundary() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let status = [
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "From the orchestrator",
    ];

    let first = world.must_as(&orchestrator, &status);
    assert!(
        first.json.get("key").is_none(),
        "a session's derived key is not printed"
    );
    let retry = world.must_as(&orchestrator, &status);
    assert!(retry.replayed());
    assert_eq!(retry.data()["id"], first.data()["id"]);

    // The turn ends: a Stop observation is the next boundary.
    world
        .store()
        .observation_record(
            &tiller_core::Ctx::new(tiller_core::Actor::Hook),
            NewObservation {
                session_id: Some(orchestrator.clone()),
                run_id: None,
                source: ObservationSource::Hook,
                event: "Stop".into(),
                verdict: Some(Activity::Idle),
                observed_at: Timestamp::now(),
                payload: serde_json::json!({}),
            },
        )
        .unwrap();
    let next_turn = world.must_as(&orchestrator, &status);
    assert!(!next_turn.replayed());
    assert_eq!(next_turn.data()["display_id"], "alpha-2");
}

/// The response to a keyed edit was lost: the retry names the same rev, which
/// is stale now, and still replays. Without the key it is a conflict.
#[test]
fn a_keyed_card_edit_retried_after_a_lost_response_replays() {
    let world = World::initialized();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let edit = [
        "card", "edit", "alpha-1", "--rev", "1", "--title", "Two", "--key", "edit-1",
    ];
    let first = world.must(&edit);
    assert!(!first.replayed());
    assert_eq!(first.data()["rev"], 2);
    let seq = world.store().last_seq().unwrap();

    let retry = world.must(&edit);
    assert!(retry.replayed());
    assert_eq!(retry.data(), first.data());
    assert_eq!(world.store().last_seq().unwrap(), seq);

    let unkeyed = world.run(&["card", "edit", "alpha-1", "--rev", "1", "--title", "Two"]);
    assert_eq!((unkeyed.code, unkeyed.error_code()), (4, "conflict"));

    // A key does not cover a different edit: it is refused, not replayed.
    let other = world.run(&[
        "card", "edit", "alpha-1", "--rev", "2", "--title", "Three", "--key", "edit-1",
    ]);
    assert_eq!(
        (other.code, other.error_code()),
        (5, "refused"),
        "{}",
        other.json
    );
    assert_eq!(world.store().last_seq().unwrap(), seq);
    assert_eq!(
        world.must(&["card", "show", "alpha-1"]).data()["card"]["title"],
        "Two"
    );
}

/// B5: one key, one request. The same key with other arguments is refused and
/// writes nothing, and a true retry still returns the first result.
#[test]
fn a_reused_key_with_other_arguments_is_refused() {
    let world = World::initialized();
    world.orchestrator();
    world.must(&[
        "project",
        "add",
        "beta",
        "--name",
        "Beta",
        "--repo",
        "example/beta",
        "--delivery",
        "direct-pr",
        "--capacity",
        "3",
    ]);
    let first_args = [
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "Key reuse A",
        "--key",
        "audit-k2",
    ];
    let first = world.must(&first_args);
    let card = first.data()["id"].as_str().unwrap().to_owned();

    let before = (rows(&world), world.store().last_seq().unwrap());
    let other = world.run(&[
        "card",
        "create",
        "beta",
        "--kind",
        "ship",
        "--title",
        "Key reuse B",
        "--key",
        "audit-k2",
    ]);
    assert_eq!(
        (other.code, other.error_code()),
        (5, "refused"),
        "{}",
        other.json
    );
    assert!(
        other.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("other arguments"),
        "{}",
        other.json
    );
    assert!(!other.json.to_string().contains(&card), "{}", other.json);
    assert_eq!((rows(&world), world.store().last_seq().unwrap()), before);
    let beta = world.must(&["card", "list", "--project", "beta"]);
    assert_eq!(beta.data()["cards"].as_array().unwrap().len(), 0);

    // A true retry returns the first card and writes nothing.
    let retry = world.must(&first_args);
    assert!(retry.replayed(), "{}", retry.json);
    assert_eq!(retry.data()["id"], card.as_str());
    assert_eq!((rows(&world), world.store().last_seq().unwrap()), before);

    // The same holds for an append: a comment key does not cover other text.
    let comment = [
        "card",
        "comment",
        "alpha-1",
        "--text",
        "first words",
        "--key",
        "audit-c1",
    ];
    world.must(&comment);
    let seq = world.store().last_seq().unwrap();
    let changed = world.run(&[
        "card",
        "comment",
        "alpha-1",
        "--text",
        "other words",
        "--key",
        "audit-c1",
    ]);
    assert_eq!(
        (changed.code, changed.error_code()),
        (5, "refused"),
        "{}",
        changed.json
    );
    assert!(world.must(&comment).replayed());
    assert_eq!(world.store().last_seq().unwrap(), seq);
}
