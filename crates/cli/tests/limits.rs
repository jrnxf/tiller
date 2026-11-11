//! The CLI takes text up to the API's length caps (`protocol.md` 7.4,
//! "Length caps") and refuses longer text as a usage error (exit 2) that
//! writes nothing, so the CLI and the board path agree on what fits.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Out, World};
use tiller_core::limits::{
    ANSWER_MAX_CHARS, ASK_NOTE_MAX_CHARS, BODY_MAX_CHARS, COMMENT_MAX_CHARS, TITLE_MAX_CHARS,
};

/// `count` characters of two bytes each, so a byte count would differ.
fn long(count: usize) -> String {
    "é".repeat(count)
}

/// `out` is the usage refusal `message` names, and nothing was written
/// since `before`.
#[track_caller]
fn refused(world: &World, out: &Out, message: &str, before: i64) {
    assert_eq!(out.code, 2, "{}", out.json);
    assert_eq!(out.error_code(), "usage", "{}", out.json);
    let said = out.json["error"]["message"].as_str().unwrap_or_default();
    assert!(said.contains(message), "{}", out.json);
    assert_eq!(world.store().last_seq().unwrap(), before, "a refusal wrote");
}

#[test]
fn text_over_the_caps_is_refused() {
    let world = World::initialized();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let item = world
        .must(&[
            "attention",
            "open",
            "alpha-1",
            "--kind",
            "decision",
            "--question",
            "Ship it?",
        ])
        .data()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ask = world
        .must(&["ask", "note", "alpha", "--text", "make a card"])
        .data()["ask"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = world.store().last_seq().unwrap();
    let title = long(TITLE_MAX_CHARS + 1);
    let body = long(BODY_MAX_CHARS + 1);
    let title_cap = format!("title is at most {TITLE_MAX_CHARS} characters");
    let body_cap = format!("body is at most {BODY_MAX_CHARS} characters");

    fn create<'a>(extra: &[&'a str]) -> Vec<&'a str> {
        let mut args = vec!["card", "create", "alpha", "--kind", "ship"];
        args.extend_from_slice(extra);
        args
    }
    let out = world.run(&create(&["--title", &title]));
    refused(&world, &out, &title_cap, before);
    let out = world.run_with_stdin(&create(&["--title", "Two", "--body", "-"]), &body);
    refused(&world, &out, &body_cap, before);
    let out = world.run_with_stdin(&create(&["--title", "Two", "--body-file", "-"]), &body);
    refused(&world, &out, &body_cap, before);

    let out = world.run(&["card", "edit", "alpha-1", "--rev", "1", "--title", &title]);
    refused(&world, &out, &title_cap, before);
    // As one argument: ASCII, to stay under the per-argument limit.
    let ascii = "b".repeat(BODY_MAX_CHARS + 1);
    let out = world.run(&["card", "edit", "alpha-1", "--rev", "1", "--body", &ascii]);
    refused(&world, &out, &body_cap, before);

    let out = world.run(&[
        "ask", "resolve", &ask, "new", "--kind", "ship", "--title", &title,
    ]);
    refused(&world, &out, &title_cap, before);

    let out = world.run(&["bug", "file", "--title", &title, "--body", "it broke"]);
    refused(&world, &out, &title_cap, before);
    let out = world.run_with_stdin(
        &["bug", "file", "--title", "It broke", "--body", "-"],
        &body,
    );
    refused(&world, &out, &body_cap, before);

    let comment = long(COMMENT_MAX_CHARS + 1);
    let out = world.run(&["card", "comment", "alpha-1", "--text", &comment]);
    refused(
        &world,
        &out,
        &format!("a comment is at most {COMMENT_MAX_CHARS} characters"),
        before,
    );
    let out = world.run_with_stdin(&["card", "comment", "alpha-1", "-"], &comment);
    refused(
        &world,
        &out,
        &format!("a comment is at most {COMMENT_MAX_CHARS} characters"),
        before,
    );

    let answer = long(ANSWER_MAX_CHARS + 1);
    let out = world.run(&["attention", "answer", &item, "--text", &answer]);
    refused(
        &world,
        &out,
        &format!("an answer is at most {ANSWER_MAX_CHARS} characters"),
        before,
    );

    // An ask counts without the space around it, as the API does.
    let note = long(ASK_NOTE_MAX_CHARS + 1);
    let out = world.run(&["ask", "note", "alpha", "--text", &note]);
    refused(
        &world,
        &out,
        &format!("an ask is at most {ASK_NOTE_MAX_CHARS} characters"),
        before,
    );
}

#[test]
fn text_at_the_caps_is_taken() {
    let world = World::initialized();
    let title = long(TITLE_MAX_CHARS);
    let body = long(BODY_MAX_CHARS);
    let made = world.run_with_stdin(
        &[
            "card", "create", "alpha", "--kind", "ship", "--title", &title, "--body", "-",
        ],
        &body,
    );
    assert!(made.ok() && made.code == 0, "{}", made.json);
    assert_eq!(made.data()["title"], title.as_str());
    world.must(&["card", "edit", "alpha-1", "--rev", "1", "--title", &title]);
    let note = format!("  {}\n", long(ASK_NOTE_MAX_CHARS));
    world.must(&["ask", "note", "alpha", "--text", &note]);
    let item = world
        .must(&[
            "attention",
            "open",
            "alpha-1",
            "--kind",
            "decision",
            "--question",
            "Ship it?",
        ])
        .data()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    world.must(&[
        "attention",
        "answer",
        &item,
        "--text",
        &long(ANSWER_MAX_CHARS),
    ]);
}

/// A bug's cap is on what the filer wrote: a body at the cap is filed even
/// though the evidence block Tiller adds takes the stored body past it.
#[test]
fn a_bug_body_at_the_cap_is_filed_with_its_evidence() {
    let world = World::initialized();
    let out = world.run_with_stdin(
        &[
            "bug",
            "file",
            "--title",
            "It broke",
            "--body",
            "-",
            "--evidence",
            r#"{"command": "tiller board", "exit": 1}"#,
        ],
        &long(BODY_MAX_CHARS),
    );
    assert!(out.ok() && out.code == 0, "{}", out.json["error"]);
}

/// `tiller send --kind steer` takes the steer cap the board's message bar
/// takes, since both are the store's `outbox_send`.
#[test]
fn a_steer_over_its_cap_is_refused() {
    use tiller_core::limits::STEER_MAX_CHARS;

    let world = World::initialized();
    let session = world.orchestrator();
    let before = world.store().last_seq().unwrap();
    let send = |text: &str| world.run(&["send", &session, "--kind", "steer", "--text", text]);
    let out = send(&long(STEER_MAX_CHARS + 1));
    refused(
        &world,
        &out,
        &format!("the message is at most {STEER_MAX_CHARS} characters"),
        before,
    );
    let out = send(&long(STEER_MAX_CHARS));
    assert!(out.ok(), "{}", out.json);
}

fn asks(world: &World) -> Vec<String> {
    let store = world.store();
    let mut statement = store
        .conn()
        .prepare("SELECT text FROM asks ORDER BY received_at, id")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A prompt typed in a pane reaches the store through `tiller hook`, which
/// takes the same ask cap as `ask note` and the board: a longer prompt is not
/// recorded, and the hook still exits 0, so Claude Code is never blocked.
#[test]
fn a_hook_prompt_over_the_ask_cap_is_not_recorded() {
    let world = World::initialized();
    world.orchestrator();
    let prompt = |text: &str| {
        serde_json::json!({"session_id": "claude-orchestrator", "prompt": text}).to_string()
    };
    world.hook(
        None,
        "UserPromptSubmit",
        &prompt(&long(ASK_NOTE_MAX_CHARS + 1)),
    );
    assert!(asks(&world).is_empty(), "an over-long prompt was recorded");
    let refusal: String = world
        .store()
        .conn()
        .query_row(
            "SELECT json_extract(payload, '$.intake_error') FROM observations
             WHERE event = 'UserPromptSubmit' ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        refusal,
        format!("an ask is at most {ASK_NOTE_MAX_CHARS} characters")
    );

    let fits = long(ASK_NOTE_MAX_CHARS);
    world.hook(None, "UserPromptSubmit", &prompt(&fits));
    assert_eq!(asks(&world), [fits]);
}
