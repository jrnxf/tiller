//! Records the fleet writes along the way: obligations, artifacts, reviews and
//! handoffs (`protocol.md` section 6.4, Records).

use std::io::Read;

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_agents::handoff::{Brief, CLEAR_HELP};
use tiller_core::domain::{ArtifactKind, Coverage, Reviewer, Role, Run, TriggerKind, Verdict};
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
        /// The obligation: a ULID or its short form
        obligation: String,
        /// Why it is cancelled
        #[arg(long, default_value = "cancelled")]
        reason: String,
    },
}

#[derive(Args, Debug)]
pub struct ObligationOpen {
    /// The card: a display id like alpha-42, a ULID or its short form
    pub card: String,
    /// The duty, in words
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
    /// The card: a display id like alpha-42, a ULID or its short form
    pub card: String,
    /// page, screenshot, report, video or other
    #[arg(long)]
    pub kind: ArtifactKind,
    /// A short title for the artifact
    #[arg(long)]
    pub title: String,
    /// Where the artifact is served; a review page gives its URL and its file.
    #[arg(long)]
    pub url: Option<String>,
    /// The file, made absolute against the current directory: the daemon
    /// reads and polls review pages by it from its own.
    #[arg(long)]
    pub path: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct ReviewRecord {
    /// The run: a ULID or short form, `<display id> <role>`, or a herdr name like
    /// alpha-42-build
    pub run: String,
    /// Who reviewed: code-review, spec-review, no-mistakes or human
    #[arg(long)]
    pub reviewer: Reviewer,
    /// What the review covered: diff, spec or full
    #[arg(long)]
    pub coverage: Coverage,
    /// pass, fail or ask-user
    #[arg(long)]
    pub verdict: Verdict,
    /// The full SHA of the commit you reviewed. A grant counts the review
    /// only while the PR head is this commit
    #[arg(long)]
    pub sha: String,
    /// How many high-severity findings
    #[arg(long, default_value_t = 0)]
    pub high: i64,
    /// How many medium-severity findings
    #[arg(long, default_value_t = 0)]
    pub medium: i64,
    /// How many low-severity findings
    #[arg(long, default_value_t = 0)]
    pub low: i64,
    /// Where the review is written up
    #[arg(long)]
    pub url: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Subcommand, Debug)]
pub enum HandoffCmd {
    /// Write the orchestrator's handoff
    Write(HandoffWrite),
    /// Show the project's newest handoff, resolved, to resume from
    Show(HandoffShow),
}

#[derive(Args, Debug)]
pub struct HandoffShow {
    /// The project; an orchestrator session defaults to its own
    #[arg(long)]
    pub project: Option<String>,
}

#[derive(Args, Debug)]
pub struct HandoffWrite {
    /// What the orchestrator concluded; needs --next
    #[arg(long, requires = "next")]
    pub conclusions: Option<String>,
    /// What to do next; needs --conclusions
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
    let path = match args.path {
        Some(path) => Some(
            std::path::absolute(&path)
                .map_err(|e| Failure::usage(format!("--path {path}: {e}")))?
                .to_string_lossy()
                .into_owned(),
        ),
        None => None,
    };
    let mut store = app.write()?;
    let run_id = app.session(&store)?.and_then(|session| session.run_id);
    let keyed = app.key(
        &store,
        "artifact.add",
        &json!({"card": args.card, "kind": args.kind, "title": args.title,
                "url": args.url, "path": path}),
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
            path,
        },
    )?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

pub fn review_record(app: &App, args: ReviewRecord) -> Done {
    let mut store = app.write()?;
    let run: Run = resolve_run(&store, &args.run)?;
    let keyed = app.key(
        &store,
        "review.record",
        &json!({"run": run.id, "reviewer": args.reviewer, "coverage": args.coverage,
                "verdict": args.verdict, "sha": args.sha, "high": args.high,
                "medium": args.medium, "low": args.low, "url": args.url}),
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
            head_sha: args.sha,
        },
    )?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

pub fn handoff(app: &App, cmd: HandoffCmd) -> Done {
    match cmd {
        HandoffCmd::Write(args) => handoff_write(app, args),
        HandoffCmd::Show(args) => handoff_show(app, args),
    }
}

fn handoff_show(app: &App, args: HandoffShow) -> Done {
    let store = app.read()?;
    let project = match (args.project, app.session(&store)?) {
        (_, Some(session)) if session.role != Role::Orchestrator => {
            return Err(Failure::forbidden(
                "tiller handoff show is run by an orchestrator session or the owner",
            ));
        }
        // An orchestrator reads its own project's record only.
        (Some(project), Some(session)) if project != session.project_id => {
            return Err(Failure::forbidden(
                "an orchestrator reads its own project's handoff",
            ));
        }
        (Some(project), _) => project,
        (None, Some(session)) => session.project_id,
        (None, None) => return Err(Failure::usage("give --project <project>")),
    };
    store.project(&project)?;
    let Some(brief) = Brief::load(store.conn(), &project)? else {
        return Ok(Outcome::read(json!({"project": project, "handoff": null}))
            .with_help("no handoff yet: start from `tiller card list` and `tiller run list`"));
    };
    Ok(Outcome::read(brief).with_help(
        "read each active card with `tiller card show <card>`; a handoff line is a lead, the record decides",
    ))
}

fn handoff_write(app: &App, args: HandoffWrite) -> Done {
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
    Ok(Outcome::applied(applied)
        .with_key(&keyed)
        .with_help(CLEAR_HELP))
}
