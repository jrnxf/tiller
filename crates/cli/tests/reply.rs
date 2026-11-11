//! `tiller reply` (P5): the orchestrator answers an ask or a card comment,
//! and the answer is stored for the board. The real binary and one
//! `state.db`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::World;

#[test]
fn the_orchestrator_replies_to_an_ask_and_a_comment() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let noted = world.must(&[
        "ask",
        "note",
        "alpha",
        "--text",
        "Can the export run nightly?",
    ]);
    let ask = noted.data()["ask"]["id"].as_str().unwrap().to_owned();
    let short = &ask[ask.len() - 7..];
    world.must_as(
        &orchestrator,
        &[
            "ask",
            "resolve",
            short,
            "new",
            "--kind",
            "ship",
            "--title",
            "Run the export nightly",
        ],
    );

    let args = [
        "reply",
        "--to",
        short,
        "--text",
        "Yes. A nightly run at 02:00 covers every merge of the day.",
    ];
    let sent = world.must_as(&orchestrator, &args);
    assert_eq!(sent.data()["to"], "ask", "{}", sent.json);
    assert_eq!(sent.data()["card"], "alpha-1", "{}", sent.json);
    assert_eq!(sent.data()["tag"], "[alpha-1]", "{}", sent.json);
    assert_eq!(sent.data()["reply"]["ask_id"], ask.as_str());
    assert_eq!(sent.events().len(), 1);

    // The same words again in the same turn replay the first reply.
    let again = world.must_as(&orchestrator, &args);
    assert!(again.replayed(), "{}", again.json);
    assert_eq!(again.data()["reply"]["id"], sent.data()["reply"]["id"]);

    // A comment on a card arrives as a message; its reply links that card.
    let made = world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        "Retry the upload",
    ]);
    let card = made.data()["display_id"].as_str().unwrap().to_owned();
    let comment = world.must(&[
        "card",
        "comment",
        &card,
        "--text",
        "Why 3 retries and not 5?",
    ]);
    let message = comment.data()["id"].as_str().unwrap().to_owned();
    let answered = world.must_as(
        &orchestrator,
        &[
            "reply",
            "--to",
            &message,
            "--text",
            "The shared bucket is flaky.",
        ],
    );
    assert_eq!(answered.data()["to"], "comment", "{}", answered.json);
    assert_eq!(answered.data()["card"], card.as_str());

    let rows: i64 = world
        .store()
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM replies WHERE read_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 2);
}

#[test]
fn only_the_orchestrator_replies() {
    let world = World::initialized();
    world.orchestrator();
    let noted = world.must(&["ask", "note", "alpha", "--text", "Status?"]);
    let ask = noted.data()["ask"]["id"].as_str().unwrap().to_owned();
    let refused = world.run(&["reply", "--to", &ask, "--text", "Fine."]);
    assert!(!refused.ok(), "{}", refused.json);
    assert_eq!(refused.error_code(), "forbidden", "{}", refused.json);

    let missing = world.run(&["reply", "--to", &ask]);
    assert!(!missing.ok(), "{}", missing.json);
    assert_eq!(missing.error_code(), "usage", "{}", missing.json);
}
