//! Ask intake: `tiller ask note | resolve | link | list | show` and `tiller
//! intake stats` (`protocol.md` section 2).

use clap::{Args, Subcommand};
use serde_json::json;
use tiller_core::domain::{Ask, AskCard, AskSource, Card, OutboxMessage, Reply};
use tiller_core::state::asks::{NewAsk, Resolution};
use tiller_core::state::replies;
use tiller_core::state::{get, list, resolve_id};

use super::{CardFields, KeyArg, summary, tag};
use crate::app::{App, Done, Failure, Outcome, text_input};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Record a note and wake the project's orchestrator
    Note(Note),
    /// Give an ask its disposition: new, followup:<card>, chat or cancelled
    Resolve(Resolve),
    /// Link a further card to a resolved ask
    Link(Link),
    /// List asks, each with the time of its answer
    List(List),
    /// Show one ask: whether it reached the orchestrator, every answer, and
    /// the cards it touched
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
            session_id: session.as_ref().map(|session| session.id.clone()),
            source: AskSource::Script,
            text,
        },
    )?;
    let warning = applied.value.warning.clone();
    // The orchestrator files the ask. Everyone else waits for its answer.
    let help = match &session {
        Some(_) => "tiller ask resolve <ask> new|followup:<card>|chat|cancelled".to_owned(),
        None => format!(
            "tiller ask show {}: whether it arrived, and the answer",
            short(&applied.value.ask.id)
        ),
    };
    let sent = applied.value.message.is_some();
    let outcome = Outcome::applied(applied)
        .with_warning(warning)
        .with_key(&keyed);
    if session.is_some() || !app.person() {
        return Ok(outcome.with_help(help));
    }
    let project = summary::project_name(&store, &args.project);
    let line = summary::asked(&outcome.data, &project);
    let outcome = outcome.with_summary(line).with_help(help);
    Ok(if sent {
        outcome
    } else {
        outcome.with_help(format!(
            "tiller orchestrator start {}: then it reads the question",
            args.project
        ))
    })
}

/// The short form of a ULID: its last 7 characters.
fn short(id: &str) -> &str {
    id.get(id.len().saturating_sub(7)..).unwrap_or(id)
}

/// The answer fields every ask row carries: when the latest reply came, how
/// many came, and how many the owner has not marked read.
fn answer_fields(replies: &[Reply]) -> serde_json::Value {
    json!({
        "answered_at": replies.last().map(|reply| reply.replied_at),
        "replies": replies.len(),
        "unread_replies": replies.iter().filter(|reply| reply.read_at.is_none()).count(),
    })
}

/// An ask row with its answer fields beside the ask's own.
fn with_answer(store: &tiller_core::Store, ask: &Ask) -> Result<serde_json::Value, Failure> {
    let replies = replies::for_ask(store.conn(), &ask.id)?;
    let mut row = crate::app::to_value(ask);
    if let (Some(row), serde_json::Value::Object(fields)) =
        (row.as_object_mut(), answer_fields(&replies))
    {
        row.extend(fields);
    }
    Ok(row)
}

/// Whether the ask's words reached the orchestrator, from the note they
/// went as.
fn delivery(note: Option<&OutboxMessage>) -> serde_json::Value {
    match note {
        Some(note) => json!({
            "state": note.state,
            "sent_at": note.created_at,
            "delivered_at": note.delivered_at,
            "seen_at": note.acked_at,
            "failed_at": note.failed_at,
            "failure_reason": note.failure_reason,
        }),
        None => serde_json::Value::Null,
    }
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
    // A followup ignores the card fields, so its body is never read.
    let (card, fields) = match target {
        Target::New => {
            let card = args.card.read_body()?;
            let fields = card.normalized();
            (card, fields)
        }
        _ => (args.card, serde_json::Value::Null),
    };
    let normalized = json!({"ask": args.ask, "target": args.target, "card": fields});
    let resolution = resolution(target, card, None)?;
    let mut store = app.write()?;
    let keyed = app.key(&store, "ask.link", &normalized, &args.key.key)?;
    let applied = store.ask_link(&app.ctx(), Some(&keyed.key), &args.ask, resolution)?;
    Ok(Outcome::applied_as(applied, resolved_data).with_key(&keyed))
}

fn list_asks(app: &App, args: List) -> Done {
    let store = app.read()?;
    if args.unresolved {
        let rows = store.asked(args.project.as_deref())?;
        let mut asks = Vec::new();
        for row in rows {
            asks.push(json!({
                "ask": with_answer(&store, &row.ask)?,
                "age_seconds": row.age_seconds.round(),
                "age": crate::lisbon::age(row.age_seconds),
            }));
        }
        return Ok(Outcome::read(json!({"asks": asks})));
    }
    let asks: Vec<Ask> = list(
        store.conn(),
        "(?1 IS NULL OR project_id = ?1) ORDER BY received_at DESC, id DESC LIMIT ?2",
        &[&args.project, &args.limit],
    )?;
    let mut rows = Vec::new();
    for ask in &asks {
        rows.push(with_answer(&store, ask)?);
    }
    Ok(Outcome::read(json!({"asks": rows})))
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
    let answers = replies::for_ask(store.conn(), &id)?;
    let note = replies::ask_note(store.conn(), &id)?;
    let mut shown = Vec::new();
    for reply in &answers {
        let card: Option<Card> = match &reply.card_id {
            Some(card) => Some(get(store.conn(), "card", card)?),
            None => None,
        };
        shown.push(json!({
            "id": reply.id,
            "body": reply.body,
            "replied_at": reply.replied_at,
            "read_at": reply.read_at,
            "card": card.map(|card| card.display_id),
        }));
    }
    let outcome = Outcome::read(json!({
        "ask": with_answer(&store, &ask)?,
        "delivery": delivery(note.as_ref()),
        "replies": shown,
        "cards": cards,
    }));
    Ok(match (answers.is_empty(), &note) {
        (false, _) => outcome,
        (true, Some(note)) => outcome.with_help(format!(
            "no answer yet; the note to the orchestrator is {}",
            note.state
        )),
        (true, None) => outcome.with_help(
            "no answer yet, and no note went to an orchestrator: `tiller agents` shows whether \
             the project has one",
        ),
    })
}
