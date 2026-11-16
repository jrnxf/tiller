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

/// UA-17: a question typed in a shell is answered in the same shell. The
/// owner sees that the note arrived, reads the reply's words, and finds the
/// unread answer in the ask list and in the recap.
#[test]
fn the_owner_reads_the_answer_in_the_cli() {
    let world = World::initialized();
    let orchestrator = world.orchestrator();
    let noted = world.must(&["ask", "note", "alpha", "--text", "Is the export nightly?"]);
    let ask = noted.data()["ask"]["id"].as_str().unwrap().to_owned();
    let short = &ask[ask.len() - 7..];
    // The owner's next step is the command that shows the answer.
    let help = noted.json["help"][0].as_str().unwrap();
    assert!(
        help.starts_with(&format!("tiller ask show {short}")),
        "{}",
        noted.json
    );

    // Before the answer: the note's state, no reply, and a line that says so.
    let waiting = world.must(&["ask", "show", short]);
    assert_eq!(waiting.data()["delivery"]["state"], "pending");
    assert_eq!(waiting.data()["replies"], serde_json::json!([]));
    assert!(waiting.data()["ask"]["answered_at"].is_null());
    assert!(
        waiting.json["help"][0]
            .as_str()
            .unwrap()
            .starts_with("no answer yet"),
        "{}",
        waiting.json
    );
    let listed = world.must(&["ask", "list", "--unresolved"]);
    assert!(listed.data()["asks"][0]["ask"]["answered_at"].is_null());

    world.must_as(&orchestrator, &["ask", "resolve", short, "chat"]);
    world.must_as(
        &orchestrator,
        &["reply", "--to", short, "--text", "Yes, at 02:00."],
    );
    world.must_as(
        &orchestrator,
        &["reply", "--to", short, "--text", "It takes 4 minutes."],
    );

    // After: every reply in the order it came, and no "no answer" line.
    let answered = world.must(&["ask", "show", short]);
    let replies = answered.data()["replies"].as_array().unwrap();
    assert_eq!(replies.len(), 2, "{}", answered.json);
    assert_eq!(replies[0]["body"], "Yes, at 02:00.");
    assert_eq!(replies[1]["body"], "It takes 4 minutes.");
    assert!(replies[0]["read_at"].is_null());
    assert_eq!(answered.data()["ask"]["replies"], 2);
    assert_eq!(answered.data()["ask"]["unread_replies"], 2);
    assert!(answered.data()["ask"]["answered_at"].is_string());
    assert!(answered.json.get("help").is_none(), "{}", answered.json);

    let listed = world.must(&["ask", "list"]);
    assert!(listed.data()["asks"][0]["answered_at"].is_string());
    assert_eq!(listed.data()["asks"][0]["unread_replies"], 2);

    // The recap lists the unread answers with the question, newest first.
    let recap = world.must(&["recap"]);
    let unread = recap.data()["replies"].as_array().unwrap();
    assert_eq!(unread.len(), 2, "{}", recap.json);
    assert_eq!(unread[0]["body"], "It takes 4 minutes.");
    assert_eq!(unread[0]["question"], "Is the export nightly?");
    assert_eq!(unread[0]["project"], "alpha");

    // A reply the owner marked read on the board leaves the recap and stays
    // in `ask show`.
    let first = replies[0]["id"].as_str().unwrap();
    world.store().reply_read(&world.owner_ctx(), first).unwrap();
    let recap = world.must(&["recap"]);
    assert_eq!(recap.data()["replies"].as_array().unwrap().len(), 1);
    let answered = world.must(&["ask", "show", short]);
    assert!(answered.data()["replies"][0]["read_at"].is_string());
    assert_eq!(answered.data()["ask"]["unread_replies"], 1);
}
