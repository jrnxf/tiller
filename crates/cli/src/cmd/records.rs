//! Records the fleet writes along the way: obligations, artifacts, reviews and
//! handoffs (`protocol.md` section 6.4, Records).

use std::io::Read;

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_core::domain::{ArtifactKind, Coverage, Reviewer, Run, TriggerKind, Verdict};
use tiller_core::state::records::{NewArtifact, NewHandoff, NewObligation, NewReview};
use tiller_core::state::resolve_card;

use super::{KeyArg, resolve_run};
use crate::app::{App, Done, Failure, Outcome};
use crate::lisbon;

#[derive(Subcommand, Debug)]
pub enum ObligationCmd {
    /// Open a duty the orchestrator must meet
    Open(ObligationOpen),
    /// Cancel an obligation
    Cancel {
        obligation: String,
        #[arg(long, default_value = "cancelled")]
        reason: String,
    },
}

#[derive(Args, Debug)]
pub struct ObligationOpen {
    pub card: String,
    #[arg(long)]
    pub what: String,
    /// Met once, when an event matches this JSON filter
    #[arg(long, group = "trigger")]
    pub on: Option<String>,
    /// Met at a date
    #[arg(long, group = "trigger")]
    pub due: Option<String>,
    /// Met every time an event matches this JSON filter
    #[arg(long, group = "trigger")]
    pub standing: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Subcommand, Debug)]
pub enum ArtifactCmd {
    /// Record a page, screenshot, report or video on a card
    Add(ArtifactAdd),
}

#[derive(Args, Debug)]
pub struct ArtifactAdd {
    pub card: String,
    #[arg(long)]
    pub kind: ArtifactKind,
    #[arg(long)]
    pub title: String,
    #[arg(long, group = "where_")]
    pub url: Option<String>,
    #[arg(long, group = "where_")]
    pub path: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Subcommand, Debug)]
pub enum ReviewCmd {
    /// Record a review of a run
    Record(ReviewRecord),
}

#[derive(Args, Debug)]
pub struct ReviewRecord {
    pub run: String,
    #[arg(long)]
    pub reviewer: Reviewer,
    #[arg(long)]
    pub coverage: Coverage,
    #[arg(long)]
    pub verdict: Verdict,
    #[arg(long, default_value_t = 0)]
    pub high: i64,
    #[arg(long, default_value_t = 0)]
    pub medium: i64,
    #[arg(long, default_value_t = 0)]
    pub low: i64,
    #[arg(long)]
    pub url: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Subcommand, Debug)]
pub enum HandoffCmd {
    /// Write the orchestrator's handoff
    Write(HandoffWrite),
}

#[derive(Args, Debug)]
pub struct HandoffWrite {
    #[arg(long, requires = "next")]
    pub conclusions: Option<String>,
    #[arg(long, requires = "conclusions")]
    pub next: Option<String>,
    /// Read {"conclusions": "...", "next": "..."} from standard input
    #[arg(value_parser = ["-"], conflicts_with_all = ["conclusions", "next"])]
    pub stdin: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

pub fn obligation(app: &App, cmd: ObligationCmd) -> Done {
    let mut store = app.write()?;
    match cmd {
        ObligationCmd::Open(args) => {
            let (trigger_kind, trigger_event, due_at) = match (&args.on, &args.due, &args.standing)
            {
                (Some(filter), None, None) => {
                    (TriggerKind::Event, Some(filter_json(filter)?), None)
                }
                (None, Some(due), None) => {
                    (TriggerKind::Date, None, Some(lisbon::parse_when(due)?))
                }
                (None, None, Some(filter)) => {
                    (TriggerKind::Standing, Some(filter_json(filter)?), None)
                }
                _ => {
                    return Err(Failure::usage(
                        "give exactly one of --on <filter>, --due <date> or --standing <filter>",
                    ));
                }
            };
            let card = resolve_card(store.conn(), &args.card)?;
            let keyed = app.key(
                &store,
                "obligation.open",
                &json!({"card": card.id, "what": args.what, "kind": trigger_kind,
                        "event": trigger_event, "due": due_at}),
                &args.key.key,
            )?;
            let applied = store.obligation_open(
                &app.ctx().with_key(&keyed.key),
                NewObligation {
                    project_id: card.project_id,
                    card_id: Some(card.id),
                    what: args.what,
                    trigger_kind,
                    trigger_event,
                    due_at,
                },
            )?;
            Ok(Outcome::applied(applied).with_key(&keyed))
        }
        ObligationCmd::Cancel { obligation, reason } => {
            let id = tiller_core::state::resolve_id(
                store.conn(),
                "obligations",
                "obligation",
                &obligation,
            )?;
            Ok(Outcome::applied(store.obligation_cancel(
                &app.ctx(),
                &id,
                &reason,
            )?))
        }
    }
}

/// An event filter: a JSON object, or a bare event kind.
fn filter_json(text: &str) -> Result<Value, Failure> {
    let value = match serde_json::from_str::<Value>(text) {
        Ok(value) => value,
        Err(_) => json!({"kind": text}),
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(Failure::usage(
            "an event filter is a JSON object, like {\"kind\": \"pr.merged\"}",
        ))
    }
}

pub fn artifact(app: &App, cmd: ArtifactCmd) -> Done {
    let ArtifactCmd::Add(args) = cmd;
    if args.url.is_none() && args.path.is_none() {
        return Err(Failure::usage("give --url <u> or --path <p>"));
    }
    let mut store = app.write()?;
    let run_id = app.session(&store)?.and_then(|session| session.run_id);
    let keyed = app.key(
        &store,
        "artifact.add",
        &json!({"card": args.card, "kind": args.kind, "title": args.title,
                "url": args.url, "path": args.path}),
        &args.key.key,
    )?;
    let applied = store.artifact_add(
        &app.ctx().with_key(&keyed.key),
        NewArtifact {
            card: args.card,
            run_id,
            kind: args.kind,
            title: args.title,
            url: args.url,
            path: args.path,
        },
    )?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

pub fn review(app: &App, cmd: ReviewCmd) -> Done {
    let ReviewCmd::Record(args) = cmd;
    let mut store = app.write()?;
    let run: Run = resolve_run(&store, &args.run)?;
    let keyed = app.key(
        &store,
        "review.record",
        &json!({"run": run.id, "reviewer": args.reviewer, "coverage": args.coverage,
                "verdict": args.verdict, "high": args.high, "medium": args.medium,
                "low": args.low, "url": args.url}),
        &args.key.key,
    )?;
    let applied = store.review_record(
        &app.ctx().with_key(&keyed.key),
        NewReview {
            run_id: run.id.clone(),
            reviewer: args.reviewer,
            coverage: args.coverage,
            findings_high: args.high,
            findings_medium: args.medium,
            findings_low: args.low,
            verdict: args.verdict,
            source_url: args.url,
        },
    )?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

pub fn handoff(app: &App, cmd: HandoffCmd) -> Done {
    let HandoffCmd::Write(args) = cmd;
    let (conclusions, next_action) = match (&args.conclusions, &args.next, &args.stdin) {
        (Some(conclusions), Some(next), None) => (conclusions.clone(), next.clone()),
        (None, None, Some(_)) => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            let value: Value = serde_json::from_str(&text)?;
            let field = |name: &str| {
                value
                    .get(name)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        Failure::usage(format!("standard input needs a \"{name}\" string"))
                    })
            };
            (field("conclusions")?, field("next")?)
        }
        _ => {
            return Err(Failure::usage(
                "give --conclusions <t> --next <t>, or - to read them as JSON from standard input",
            ));
        }
    };
    let mut store = app.write()?;
    let session = app.session(&store)?.ok_or_else(|| {
        Failure::forbidden("tiller handoff write is run by an orchestrator session")
    })?;
    let keyed = app.key(
        &store,
        "handoff.write",
        &json!({"conclusions": conclusions, "next": next_action}),
        &args.key.key,
    )?;
    let applied = store.handoff_write(
        &app.ctx().with_key(&keyed.key),
        NewHandoff {
            session_id: session.id,
            conclusions,
            next_action,
        },
    )?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}
