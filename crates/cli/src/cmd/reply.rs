//! `tiller reply --to <ask|comment>`: the orchestrator's answer to an ask or
//! a card comment, stored so the board shows it (`protocol.md` 6.4).

use clap::Args;
use serde_json::json;
use tiller_core::limits::{COMMENT_MAX_CHARS, within};
use tiller_core::state::replies::NewReply;

use super::{KeyArg, tag};
use crate::app::{App, Done, Outcome, text_input};

#[derive(Args, Debug)]
pub struct Reply {
    /// The ask (a ULID or its short form), or the message a comment arrived as
    #[arg(long)]
    pub to: String,
    /// The answer; or pass - instead to read it from standard input
    #[arg(long)]
    pub text: Option<String>,
    /// Read the answer from standard input
    #[arg(value_parser = ["-"])]
    pub stdin: Option<String>,
    /// The card the answer links to; by default the comment's card, or the
    /// first card the ask created or followed up
    #[arg(long)]
    pub card: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

pub fn run(app: &App, args: Reply) -> Done {
    let text = text_input(args.text.clone(), args.stdin.clone(), "the reply")?;
    within("a reply", &text, COMMENT_MAX_CHARS)?;
    let mut store = app.write()?;
    let keyed = app.key(
        &store,
        "ask.reply",
        &json!({"to": args.to, "text": text, "card": args.card}),
        &args.key.key,
    )?;
    let applied = store.reply_send(
        &app.ctx(),
        Some(&keyed.key),
        NewReply {
            to: args.to,
            text,
            card: args.card,
        },
    )?;
    Ok(Outcome::applied_as(applied, |view| {
        json!({
            "reply": view.reply,
            "to": view.to,
            "card": view.card.as_ref().map(|card| card.display_id.clone()),
            "tag": view.card.as_ref().map(|card| tag(&card.display_id)),
        })
    })
    .with_key(&keyed))
}
