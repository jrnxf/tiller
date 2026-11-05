//! `tiller attention ...` (`protocol.md` section 6.4, Attention and approvals).

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_core::domain::{AnsweredVia, Ask, AskSource, Attention, AttentionKind};
use tiller_core::state::attention::{NewAttention, stale};
use tiller_core::state::{get, list, resolve_id};

use super::KeyArg;
use crate::app::{App, Done, Failure, Outcome, text_input};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Open a question for the owner on a card
    Open(Open),
    /// Answer an item with the owner's exact words
    Answer(Answer),
    /// Withdraw an item that no longer applies
    Withdraw {
        item: String,
        #[arg(long)]
        reason: String,
    },
    /// List items
    List {
        /// Only items still waiting for an answer
        #[arg(long)]
        open: bool,
    },
}

#[derive(Args, Debug)]
pub struct Open {
    pub card: String,
    #[arg(long)]
    pub kind: AttentionKind,
    #[arg(long)]
    pub question: String,
    #[arg(long)]
    pub evidence: Option<String>,
    #[arg(long)]
    pub recommendation: Option<String>,
    #[arg(long)]
    pub consequence: Option<String>,
    /// A JSON array of the choices the owner has: labels, or objects with
    /// `label`, `consequence`, `recommended` and an optional owner `action`
    /// (`{"kind": "project_capacity", "project": <id>, "from": <n>, "to": <n>}`)
    /// that applies when the owner picks that option on the board
    #[arg(long)]
    pub options: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct Answer {
    pub item: String,
    #[arg(long, conflicts_with = "ask")]
    pub text: Option<String>,
    #[arg(value_parser = ["-"])]
    pub stdin: Option<String>,
    /// Cite the ask that carries the owner's words from the orchestrator chat
    #[arg(long)]
    pub ask: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

pub fn run(app: &App, cmd: Cmd) -> Done {
    match cmd {
        Cmd::Open(args) => open(app, args),
        Cmd::Answer(args) => answer(app, args),
        Cmd::Withdraw { item, reason } => {
            let mut store = app.write()?;
            Ok(Outcome::applied(store.attention_withdraw(
                &app.ctx(),
                &item,
                &reason,
            )?))
        }
        Cmd::List { open } => list_items(app, open),
    }
}

fn open(app: &App, args: Open) -> Done {
    let options: Option<Value> = args
        .options
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;
    if options.as_ref().is_some_and(|value| !value.is_array()) {
        return Err(Failure::usage("--options is a JSON array"));
    }
    let mut store = app.write()?;
    let keyed = app.key(
        &store,
        "attention.open",
        &json!({
            "card": args.card, "kind": args.kind, "question": args.question,
            "evidence": args.evidence, "recommendation": args.recommendation,
            "consequence": args.consequence, "options": options,
        }),
        &args.key.key,
    )?;
    let run_id = app.session(&store)?.and_then(|session| session.run_id);
    let applied = store.attention_open(
        &app.ctx(),
        Some(&keyed.key),
        NewAttention {
            card: args.card,
            kind: args.kind,
            question: args.question,
            evidence: args.evidence,
            recommendation: args.recommendation,
            consequence: args.consequence,
            options,
            run_id,
            ..NewAttention::default()
        },
    )?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

fn answer(app: &App, args: Answer) -> Done {
    let mut store = app.write()?;
    let (words, via, cited) = match &args.ask {
        Some(reference) => {
            let ask = chat_ask(&store, app, reference)?;
            (ask.text.clone(), AnsweredVia::Chat, Some(ask.id))
        }
        None => (
            text_input(args.text.clone(), args.stdin.clone(), "the answer")?,
            AnsweredVia::Cli,
            None,
        ),
    };
    // The same words replay; a different answer is refused by the core.
    let keyed = app.key(
        &store,
        "attention.answer",
        &json!({"item": args.item, "answer": words, "ask": cited}),
        &args.key.key,
    )?;
    let applied = store.attention_answer(&app.ctx(), Some(&keyed.key), &args.item, &words, via)?;
    Ok(Outcome::applied_as(applied, |answered| {
        json!({
            "attention": answered.attention,
            "still_current": answered.still_current,
            "message": answered.message,
            "closed_card": answered.closed_card,
            "applied": answered.applied,
            "cited_ask": cited,
        })
    })
    .with_key(&keyed))
}

/// The ask that carries the owner's words from chat (`authority.md` G06): a
/// hook-recorded prompt, typed in the calling orchestrator's own session.
pub fn chat_ask(store: &tiller_core::Store, app: &App, reference: &str) -> Result<Ask, Failure> {
    let id = resolve_id(store.conn(), "asks", "ask", reference)?;
    let ask: Ask = get(store.conn(), "ask", &id)?;
    if ask.source != AskSource::Prompt {
        return Err(Failure::refused(format!(
            "ask {} came from {}, not a prompt the owner typed",
            ask.id, ask.source
        )));
    }
    let session = app.session(store)?;
    match (&session, &ask.session_id) {
        (Some(caller), Some(typed_in)) if &caller.id == typed_in => Ok(ask),
        _ => Err(Failure::refused(format!(
            "ask {} was not typed in the calling session",
            ask.id
        ))),
    }
}

fn list_items(app: &App, open: bool) -> Done {
    let store = app.read()?;
    let filter = if open {
        "answered_at IS NULL AND withdrawn_at IS NULL ORDER BY opened_at, id"
    } else {
        "1 = 1 ORDER BY opened_at, id"
    };
    let items: Vec<Attention> = list(store.conn(), filter, &[])?;
    let mut rows = Vec::new();
    for item in items {
        let (current, why) = if item.answered_at.is_none() && item.withdrawn_at.is_none() {
            let why = stale(store.conn(), &item)?;
            (Some(why.is_none()), why)
        } else {
            (None, None)
        };
        rows.push(json!({"item": item, "still_current": current, "stale": why}));
    }
    Ok(Outcome::read(json!({"attention": rows})))
}
