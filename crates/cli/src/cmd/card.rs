//! `tiller card ...` (`protocol.md` section 6.4, Cards).

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_core::domain::{
    Approval, Artifact, Attention, CardDep, CardOutcome, CardState, Column, Hold,
    HoldReleaseReason, Obligation, Pr, Review, Run, Session,
};
use tiller_core::state::cards::{CardEdit, DispatchOverride};
use tiller_core::state::{list, resolve_card};

use super::{CardFields, KeyArg, card_of, clearable, parse_dispatch, tag};
use crate::app::{App, Done, Failure, Outcome, text_input};
use crate::lisbon;

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create a card
    Create(Create),
    /// Edit a card's fields against the rev you read
    Edit(Edit),
    /// Park a card until a date
    Hold(Hold_),
    /// Release a card's hold
    Unhold { card: String },
    /// Add or remove a dependency
    #[command(subcommand)]
    Dep(Dep),
    /// Close a card with an outcome
    Close(Close),
    /// Close an ops card as completed, with evidence
    Complete(Complete),
    /// Cancel a card and end its runs
    Cancel(Reasoned),
    /// Reopen a closed card
    Reopen(Reasoned),
    /// Send the project's orchestrator a comment on a card
    Comment(Comment),
    /// Show a card with its column, runs, holds, PRs and approvals
    Show { card: String },
    /// List cards
    List(List),
    /// A card's events, newest first
    Timeline(Timeline),
}

#[derive(Args, Debug)]
pub struct Create {
    pub project: String,
    #[command(flatten)]
    pub fields: CardFields,
    /// The ask this card answers
    #[arg(long)]
    pub ask: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct Edit {
    pub card: String,
    /// The rev you read; a stale rev is a conflict
    #[arg(long)]
    pub rev: i64,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub priority: Option<i64>,
    /// An empty value clears the field
    #[arg(long)]
    pub body: Option<String>,
    #[arg(long)]
    pub size: Option<String>,
    #[arg(long)]
    pub acceptance: Option<String>,
    #[arg(long)]
    pub category: Option<String>,
    /// Owner only: <model>:<effort>
    #[arg(long, conflicts_with = "no_dispatch")]
    pub dispatch: Option<String>,
    /// Owner only: clear the override
    #[arg(long)]
    pub no_dispatch: bool,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct Hold_ {
    pub card: String,
    #[arg(long)]
    pub until: String,
    #[arg(long)]
    pub reason: String,
}

#[derive(Subcommand, Debug)]
pub enum Dep {
    Add { card: String, depends_on: String },
    Rm { card: String, depends_on: String },
}

#[derive(Args, Debug)]
pub struct Close {
    pub card: String,
    #[arg(long, required_unless_present = "reported")]
    pub outcome: Option<CardOutcome>,
    /// Short for --outcome reported
    #[arg(long, conflicts_with = "outcome")]
    pub reported: bool,
    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Args, Debug)]
pub struct Complete {
    pub card: String,
    #[arg(long)]
    pub evidence: String,
}

#[derive(Args, Debug)]
pub struct Reasoned {
    pub card: String,
    #[arg(long)]
    pub reason: String,
}

#[derive(Args, Debug)]
pub struct Comment {
    pub card: String,
    #[arg(long)]
    pub text: Option<String>,
    #[arg(value_parser = ["-"])]
    pub stdin: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct List {
    #[arg(long)]
    pub project: Option<String>,
    /// open or closed
    #[arg(long)]
    pub state: Option<CardState>,
    /// asked, queued, working, needs_you, landing or done
    #[arg(long)]
    pub column: Option<Column>,
}

#[derive(Args, Debug)]
pub struct Timeline {
    pub card: String,
    /// Only events before this seq
    #[arg(long)]
    pub before: Option<i64>,
    #[arg(long, default_value_t = 50)]
    pub limit: i64,
}

pub fn run(app: &App, cmd: Cmd) -> Done {
    match cmd {
        Cmd::Create(args) => create(app, args),
        Cmd::Edit(args) => edit(app, args),
        Cmd::Hold(args) => hold(app, args),
        Cmd::Unhold { card } => unhold(app, &card),
        Cmd::Dep(dep) => dep_cmd(app, dep),
        Cmd::Close(args) => close(app, args),
        Cmd::Complete(args) => complete(app, args),
        Cmd::Cancel(args) => cancel(app, args),
        Cmd::Reopen(args) => reopen(app, args),
        Cmd::Comment(args) => comment(app, args),
        Cmd::Show { card } => show(app, &card),
        Cmd::List(args) => list_cards(app, args),
        Cmd::Timeline(args) => timeline(app, args),
    }
}

fn create(app: &App, args: Create) -> Done {
    let normalized =
        json!({"project": args.project, "ask": args.ask, "card": args.fields.normalized()});
    let new = args.fields.into_new(&args.project)?;
    let mut store = app.write()?;
    let keyed = app.key(&store, "card.create", &normalized, &args.key.key)?;
    let applied = store.card_create(&app.ctx(), Some(&keyed.key), new, args.ask.as_deref())?;
    let label = tag(&applied.value.display_id);
    Ok(Outcome::applied(applied)
        .with_key(&keyed)
        .with_help(format!("start each line about this card with {label}")))
}

fn edit(app: &App, args: Edit) -> Done {
    let size = match args.size {
        None => None,
        Some(text) if text.is_empty() => Some(None),
        Some(text) => Some(Some(text.parse()?)),
    };
    let dispatch_override = if args.no_dispatch {
        Some(None)
    } else if let Some(text) = &args.dispatch {
        let (model, effort) = parse_dispatch(text)?;
        Some(Some(DispatchOverride { model, effort }))
    } else {
        None
    };
    let edit = CardEdit {
        title: args.title,
        body: clearable(args.body),
        priority: args.priority,
        category: clearable(args.category),
        size,
        acceptance: clearable(args.acceptance),
        dispatch_override,
    };
    let mut store = app.write()?;
    // Natural at the current rev; only an explicit key replays at a stale one.
    let applied = store.card_edit(
        &app.ctx(),
        args.key.key.as_deref(),
        &args.card,
        args.rev,
        edit,
    )?;
    Ok(Outcome::applied(applied))
}

fn hold(app: &App, args: Hold_) -> Done {
    let until = lisbon::parse_when(&args.until)?;
    let mut store = app.write()?;
    let applied = store.card_hold(&app.ctx(), &args.card, until, &args.reason)?;
    Ok(Outcome::applied(applied))
}

fn unhold(app: &App, card: &str) -> Done {
    let mut store = app.write()?;
    let applied = store.hold_release(&app.ctx(), card, HoldReleaseReason::Removed)?;
    Ok(Outcome::applied_as(applied, |hold| json!({"hold": hold})))
}

fn dep_cmd(app: &App, dep: Dep) -> Done {
    let mut store = app.write()?;
    match dep {
        Dep::Add { card, depends_on } => Ok(Outcome::applied(store.dep_add(
            &app.ctx(),
            &card,
            &depends_on,
        )?)),
        Dep::Rm { card, depends_on } => {
            let applied = store.dep_release(&app.ctx(), &card, &depends_on)?;
            Ok(Outcome::applied_as(
                applied,
                |dep| json!({"dependency": dep}),
            ))
        }
    }
}

fn closed_data(closed: &tiller_core::state::cards::Closed) -> Value {
    json!({
        "card": closed.card,
        "released_holds": closed.released_holds,
        "released_dependants": closed.released_dependants,
    })
}

fn close(app: &App, args: Close) -> Done {
    let outcome = if args.reported {
        CardOutcome::Reported
    } else {
        args.outcome
            .ok_or_else(|| Failure::usage("give --outcome <o>, or --reported"))?
    };
    let mut store = app.write()?;
    let applied = store.card_close(
        &app.ctx(),
        &args.card,
        outcome,
        args.reason.as_deref(),
        None,
    )?;
    Ok(Outcome::applied_as(applied, closed_data))
}

fn complete(app: &App, args: Complete) -> Done {
    let mut store = app.write()?;
    let applied = store.card_close(
        &app.ctx(),
        &args.card,
        CardOutcome::Completed,
        None,
        Some(&args.evidence),
    )?;
    Ok(Outcome::applied_as(applied, closed_data))
}

fn cancel(app: &App, args: Reasoned) -> Done {
    let mut store = app.write()?;
    let applied = store.card_cancel(&app.ctx(), &args.card, &args.reason)?;
    Ok(Outcome::applied_as(applied, |cancelled| {
        let mut data = closed_data(&cancelled.closed);
        data["ended_runs"] = json!(cancelled.ended_runs);
        data["withdrawn_attention"] = json!(cancelled.withdrawn_attention);
        data
    }))
}

fn reopen(app: &App, args: Reasoned) -> Done {
    let mut store = app.write()?;
    Ok(Outcome::applied(store.card_reopen(
        &app.ctx(),
        &args.card,
        &args.reason,
    )?))
}

fn comment(app: &App, args: Comment) -> Done {
    let text = text_input(args.text.clone(), args.stdin.clone(), "the comment")?;
    let mut store = app.write()?;
    let keyed = app.key(
        &store,
        "card.comment",
        &json!({"card": args.card, "text": text}),
        &args.key.key,
    )?;
    let applied = store.card_comment(&app.ctx(), Some(&keyed.key), &args.card, &text)?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

fn show(app: &App, reference: &str) -> Done {
    let store = app.read()?;
    let card = card_of(&store, reference)?;
    let conn = store.conn();
    let column = store.card_column(&card.id)?;
    let runs: Vec<Run> = list(conn, "card_id = ?1 ORDER BY started_at, id", &[&card.id])?;
    let mut run_rows = Vec::new();
    for run in &runs {
        let session: Option<Session> = tiller_core::state::runs::live_session(conn, &run.id)?;
        run_rows.push(json!({
            "run": run,
            "name": herdr_name(&card.display_id, run),
            "session": session.map(|s| json!({
                "id": s.id,
                "activity": s.activity,
                "activity_observed_at": s.activity_observed_at,
                "pane_id": s.pane_id,
            })),
        }));
    }
    let holds: Vec<Hold> = list(conn, "card_id = ?1 ORDER BY set_at", &[&card.id])?;
    let deps: Vec<CardDep> = list(
        conn,
        "card_id = ?1 OR depends_on_card_id = ?1 ORDER BY added_at",
        &[&card.id],
    )?;
    let attention: Vec<Attention> = list(conn, "card_id = ?1 ORDER BY opened_at", &[&card.id])?;
    let prs: Vec<Pr> = list(conn, "card_id = ?1 ORDER BY number", &[&card.id])?;
    let approvals: Vec<Approval> = list(conn, "card_id = ?1 ORDER BY requested_at", &[&card.id])?;
    let artifacts: Vec<Artifact> = list(conn, "card_id = ?1 ORDER BY created_at", &[&card.id])?;
    let reviews: Vec<Review> = list(conn, "card_id = ?1 ORDER BY recorded_at", &[&card.id])?;
    let obligations: Vec<Obligation> = list(conn, "card_id = ?1 ORDER BY opened_at", &[&card.id])?;
    Ok(Outcome::read(json!({
        "card": card,
        "tag": tag(&card.display_id),
        "column": column,
        "runs": run_rows,
        "holds": holds,
        "dependencies": deps,
        "attention": attention,
        "prs": prs,
        "approvals": approvals,
        "artifacts": artifacts,
        "reviews": reviews,
        "obligations": obligations,
    })))
}

/// The herdr name of a run: `alpha-42-build`, then `alpha-42-build-2`.
pub fn herdr_name(display_id: &str, run: &Run) -> String {
    let base = format!("{display_id}-{}", run.role.short_name());
    if run.attempt > 1 {
        format!("{base}-{}", run.attempt)
    } else {
        base
    }
}

fn list_cards(app: &App, args: List) -> Done {
    let store = app.read()?;
    let rows = store.card_columns(args.project.as_deref())?;
    let mut cards = Vec::new();
    for row in rows {
        if args.column.is_some_and(|column| column != row.column) {
            continue;
        }
        let card = resolve_card(store.conn(), &row.id)?;
        if args.state.is_some_and(|state| state != card.state) {
            continue;
        }
        cards.push(json!({
            "id": card.id,
            "display_id": card.display_id,
            "project_id": card.project_id,
            "kind": card.kind,
            "title": card.title,
            "priority": card.priority,
            "state": card.state,
            "outcome": card.outcome,
            "column": row.column,
            "badge": row.badge,
            "rev": card.rev,
        }));
    }
    Ok(Outcome::read(json!({"cards": cards})))
}

fn timeline(app: &App, args: Timeline) -> Done {
    let store = app.read()?;
    let card = card_of(&store, &args.card)?;
    let before = args.before.unwrap_or(i64::MAX);
    let events: Vec<tiller_core::domain::Event> = list(
        store.conn(),
        "card_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        &[&card.id, &before, &args.limit],
    )?;
    Ok(Outcome::read(
        json!({"card": card.display_id, "events": events}),
    ))
}
