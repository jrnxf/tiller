//! Ask intake: `tiller ask note | resolve | link | list | show` and `tiller
//! intake stats` (`protocol.md` section 2).

use clap::{Args, Subcommand};
use serde_json::json;
use tiller_core::domain::{Ask, AskCard, AskSource, Card};
use tiller_core::limits::{ASK_NOTE_MAX_CHARS, within};
use tiller_core::state::asks::{NewAsk, Resolution};
use tiller_core::state::{get, list, resolve_id};

use super::{CardFields, KeyArg, tag};
use crate::app::{App, Done, Failure, Outcome, text_input};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Record a note and wake the project's orchestrator
    Note(Note),
    /// Give an ask its disposition: new, followup:<card>, chat or cancelled
    Resolve(Resolve),
    /// Link a further card to a resolved ask
    Link(Link),
    /// List asks
    List(List),
    /// Show one ask with the cards it touched
    Show {
        /// The ask: a ULID or its short form
        ask: String,
    },
}

#[derive(Args, Debug)]
pub struct Note {
    /// The project's id (its slug, like alpha)
    pub project: String,
    /// The note's text; or pass - instead to read it from standard input
    #[arg(long)]
    pub text: Option<String>,
    /// Read the text from standard input
    #[arg(value_parser = ["-"])]
    pub stdin: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct Resolve {
    /// The ask: a ULID or its short form
    pub ask: String,
    /// new, followup:<card>, chat or cancelled
    pub target: String,
    #[command(flatten)]
    pub card: CardFields,
    /// Why, for chat or cancelled
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Args, Debug)]
pub struct Link {
    /// The ask: a ULID or its short form
    pub ask: String,
    /// new, or followup:<card>
    pub target: String,
    #[command(flatten)]
    pub card: CardFields,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct List {
    /// Only asks nobody has resolved, oldest first, with their age
    #[arg(long)]
    pub unresolved: bool,
    /// Only this project's asks (its id)
    #[arg(long)]
    pub project: Option<String>,
    /// The most asks to list
    #[arg(long, default_value_t = 50)]
    pub limit: i64,
}

pub fn run(app: &App, cmd: Cmd) -> Done {
    match cmd {
        Cmd::Note(args) => note(app, args),
        Cmd::Resolve(args) => resolve(app, args),
        Cmd::Link(args) => link(app, args),
        Cmd::List(args) => list_asks(app, args),
        Cmd::Show { ask } => show(app, &ask),
    }
}

fn note(app: &App, args: Note) -> Done {
    let text = text_input(args.text.clone(), args.stdin.clone(), "the note")?;
    within("an ask", text.trim(), ASK_NOTE_MAX_CHARS)?;
    let mut store = app.write()?;
    let keyed = app.key(
        &store,
        "ask.note",
        &json!({"project": args.project, "text": text}),
        &args.key.key,
    )?;
    let session = app.session(&store)?;
    let applied = store.ask_record(
        &app.ctx(),
        Some(&keyed.key),
        NewAsk {
            project_id: args.project.clone(),
            session_id: session.map(|session| session.id),
            source: AskSource::Script,
            text,
        },
    )?;
    let warning = applied.value.warning.clone();
    Ok(Outcome::applied(applied)
        .with_warning(warning)
        .with_key(&keyed)
        .with_help("tiller ask resolve <ask> new|followup:<card>|chat|cancelled"))
}

enum Target {
    New,
    Followup(String),
    Chat,
    Cancelled,
}

fn parse_target(text: &str) -> Result<Target, Failure> {
    match text {
        "new" => Ok(Target::New),
        "chat" => Ok(Target::Chat),
        "cancelled" => Ok(Target::Cancelled),
        other => match other.strip_prefix("followup:") {
            Some(card) if !card.is_empty() => Ok(Target::Followup(card.to_owned())),
            _ => Err(Failure::usage(format!(
                "not a disposition: {other:?} (new, followup:<card>, chat or cancelled)"
            ))),
        },
    }
}

fn resolution(
    target: Target,
    card: CardFields,
    reason: Option<String>,
) -> Result<Resolution, Failure> {
    Ok(match target {
        Target::New => Resolution::New(card.into_new("")?),
        Target::Followup(card) => Resolution::Followup { card },
        Target::Chat => Resolution::Chat { reason },
        Target::Cancelled => Resolution::Cancelled { reason },
    })
}

fn resolved_data(resolved: &tiller_core::state::asks::Resolved) -> serde_json::Value {
    json!({
        "ask": resolved.ask,
        "card": resolved.card,
        "tag": resolved.card.as_ref().map(|card| tag(&card.display_id)),
    })
}

fn resolve(app: &App, args: Resolve) -> Done {
    let target = parse_target(&args.target)?;
    let resolution = resolution(target, args.card, args.reason)?;
    let mut store = app.write()?;
    let applied = store.ask_resolve(&app.ctx(), &args.ask, resolution)?;
    Ok(Outcome::applied_as(applied, resolved_data))
}

fn link(app: &App, args: Link) -> Done {
    let target = parse_target(&args.target)?;
    if matches!(target, Target::Chat | Target::Cancelled) {
        return Err(Failure::usage(
            "ask link takes new or followup:<card>; chat and cancelled create no card",
        ));
    }
    let normalized =
        json!({"ask": args.ask, "target": args.target, "card": args.card.normalized()});
    let resolution = resolution(target, args.card, None)?;
    let mut store = app.write()?;
    let keyed = app.key(&store, "ask.link", &normalized, &args.key.key)?;
    let applied = store.ask_link(&app.ctx(), Some(&keyed.key), &args.ask, resolution)?;
    Ok(Outcome::applied_as(applied, resolved_data).with_key(&keyed))
}

fn list_asks(app: &App, args: List) -> Done {
    let store = app.read()?;
    if args.unresolved {
        let rows = store.asked(args.project.as_deref())?;
        let rows: Vec<_> = rows
            .into_iter()
            .map(|row| {
                json!({
                    "ask": row.ask,
                    "age_seconds": row.age_seconds.round(),
                    "age": crate::lisbon::age(row.age_seconds),
                })
            })
            .collect();
        return Ok(Outcome::read(json!({"asks": rows})));
    }
    let asks: Vec<Ask> = list(
        store.conn(),
        "(?1 IS NULL OR project_id = ?1) ORDER BY received_at DESC, id DESC LIMIT ?2",
        &[&args.project, &args.limit],
    )?;
    Ok(Outcome::read(json!({"asks": asks})))
}

fn show(app: &App, reference: &str) -> Done {
    let store = app.read()?;
    let id = resolve_id(store.conn(), "asks", "ask", reference)?;
    let ask: Ask = get(store.conn(), "ask", &id)?;
    let links: Vec<AskCard> = list(store.conn(), "ask_id = ?1 ORDER BY linked_at", &[&id])?;
    let mut cards = Vec::new();
    for link in &links {
        let card: Card = get(store.conn(), "card", &link.card_id)?;
        cards.push(json!({
            "card": card.display_id,
            "id": card.id,
            "title": card.title,
            "relation": link.relation,
            "linked_at": link.linked_at,
        }));
    }
    Ok(Outcome::read(json!({"ask": ask, "cards": cards})))
}
