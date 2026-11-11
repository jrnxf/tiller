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
