//! Reviews: the per-run review record, and the pre-launch review pass
//! (`docs/design/pre-launch-validation.md` sections 4-6): plan, start, the
//! chunk and summary briefs, finish, abandon and the due list.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_agents::review::{
    GhLookup, HeadError, NoLookup, Plan, PullLookup, checkout_head as checkout_head_in,
    chunk_brief, collect, summary_brief,
};
use tiller_agents::spawn::config::SpawnConfig;
use tiller_core::Store;
use tiller_core::domain::{Card, Project, ReviewPass, ReviewPassState, Role};
use tiller_core::state::get;
use tiller_core::state::review::{chunk_count, due_reviews, due_to_start, live_pass, resolve_pass};

use super::admin::load_config;
use super::records::{ReviewRecord, review_record};
use super::tag;
use crate::app::{App, Done, Failure, Outcome};

#[derive(Subcommand, Debug)]
pub enum ReviewCmd {
    /// Record a review of a run
    Record(ReviewRecord),
    /// Print the chunk plan of the project's next pre-launch review pass
    Plan(ReviewProject),
    /// Start the due pre-launch review pass: fix its head and store its plan
    Start(ReviewProject),
    /// Write the brief of one chunk run, or of the summary run
    Brief(ReviewBrief),
    /// Finish a pass: advance the reviewed commit and schedule the next
    Finish(ReviewFinish),
    /// Abandon a pass; the next one covers the same commits again
    Abandon(ReviewAbandon),
    /// List weekly reviews that came due with no pass started from them
    Due(ReviewDue),
}

#[derive(Args, Debug)]
pub struct ReviewProject {
    pub project: String,
}

#[derive(Args, Debug)]
pub struct ReviewBrief {
    /// The pass id, short form, or its card
    pub pass: String,
    /// The 1-based chunk
    #[arg(long, group = "which", required = true)]
    pub chunk: Option<usize>,
    /// The summary run's brief
    #[arg(long, group = "which")]
    pub summary: bool,
    /// Only these files of the chunk: the "Not covered" list of a cut run
    #[arg(long, requires = "chunk")]
    pub only: Vec<String>,
    /// Write the brief here, for `tiller spawn --brief-file`
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ReviewFinish {
    pub pass: String,
    /// The report page artifact; defaults to the newest page on the pass card
    #[arg(long)]
    pub report: Option<String>,
    /// A fix card filed from the report (repeat for each)
    #[arg(long = "fix-card")]
    pub fix_cards: Vec<String>,
}

#[derive(Args, Debug)]
pub struct ReviewAbandon {
    pub pass: String,
    #[arg(long)]
    pub reason: String,
}

#[derive(Args, Debug)]
pub struct ReviewDue {
    /// One project; an orchestrator session defaults to its own
    #[arg(long)]
    pub project: Option<String>,
}

pub fn review(app: &App, cmd: ReviewCmd) -> Done {
    match cmd {
        ReviewCmd::Record(args) => review_record(app, args),
        ReviewCmd::Plan(args) => plan(app, args),
        ReviewCmd::Start(args) => start(app, args),
        ReviewCmd::Brief(args) => brief(app, args),
        ReviewCmd::Finish(args) => finish(app, args),
        ReviewCmd::Abandon(args) => abandon(app, args),
        ReviewCmd::Due(args) => due(app, args),
    }
}

/// A pass is driven by the project's orchestrator, or the owner.
fn require_orchestrator(
    app: &App,
    store: &Store,
    project: &str,
    what: &str,
) -> Result<(), Failure> {
    match app.session(store)? {
        Some(session) if session.role != Role::Orchestrator => Err(Failure::forbidden(format!(
            "tiller review {what} is run by an orchestrator session or the owner"
        ))),
        Some(session) if session.project_id != project => Err(Failure::forbidden(format!(
            "an orchestrator runs tiller review {what} on its own project"
        ))),
        _ => Ok(()),
    }
}

/// The project's checkout and its default-branch head, fetched just now: the
/// remote's, or for a `local-only` project the local branch's.
pub fn checkout_head(app: &App, project: &Project) -> Result<(PathBuf, String), Failure> {
    let config = SpawnConfig::from_table(&load_config(app)?)
        .map_err(|error| Failure::usage(format!("config.toml: {error}")))?;
    checkout_head_in(&config, project).map_err(|error| match error {
        HeadError::NoCheckout(_) => Failure::refused(error.to_string())
            .hint(format!("tiller config set checkouts.{} <path>", project.id)),
        HeadError::Read { .. } => Failure::refused(error.to_string()),
    })
}

/// `TILLER_GH` names the forge client, so a test can stand in a fake.
fn lookup(project: &Project) -> Box<dyn PullLookup> {
    match project.repo {
        Some(_) => Box::new(GhLookup::new(
            std::env::var("TILLER_GH").unwrap_or_else(|_| "gh".into()),
        )),
        None => Box::new(NoLookup),
    }
}

/// The base the next pass starts from: a pending pass's own base, else the
/// project's reviewed commit.
fn next_base(store: &Store, project: &Project) -> Result<(Option<ReviewPass>, String), Failure> {
    let live = live_pass(store.conn(), &project.id)?;
    let base = match &live {
        Some(pass) => pass.base_sha.clone(),
        None => project.reviewed_through_sha.clone().ok_or_else(|| {
            Failure::refused(format!(
                "{} has no reviewed commit; it was never pre-launch",
                project.id
            ))
        })?,
    };
    Ok((live, base))
}

fn compute(app: &App, store: &Store, project: &Project, base: &str) -> Result<Plan, Failure> {
    let (checkout, head) = checkout_head(app, project)?;
    let (units, excluded) = collect(
        store.conn(),
        project,
        &checkout,
        base,
        &head,
        lookup(project).as_ref(),
    )
    .map_err(|error| Failure::refused(error.to_string()))?;
    Ok(Plan::new(base, &head, units, excluded))
}

fn plan(app: &App, args: ReviewProject) -> Done {
    let store = app.read()?;
    let project: Project = get(store.conn(), "project", &args.project)?;
    let (_, base) = next_base(&store, &project)?;
    let plan = compute(app, &store, &project, &base)?;
    Ok(Outcome::read(json!({"plan": plan})))
}

fn start(app: &App, args: ReviewProject) -> Done {
    let mut store = app.write()?;
    require_orchestrator(app, &store, &args.project, "start")?;
    let project: Project = get(store.conn(), "project", &args.project)?;
    let (live, base) = next_base(&store, &project)?;
    // A running pass keeps its head and plan: starting it again is a no-op.
    let (head, plan) = match live {
        Some(pass) if pass.state == ReviewPassState::Running => {
            (pass.head_sha.unwrap_or_default(), pass.chunks)
        }
        live => {
            // Nothing pending and nothing due: refuse before reading the range.
            if live.is_none() {
                due_to_start(store.conn(), &project, app.ctx().now)?;
            }
            let plan = compute(app, &store, &project, &base)?;
            (plan.head_sha.clone(), serde_json::to_value(plan)?)
        }
    };
    let applied = store.review_start(&app.ctx(), &project.id, &head, plan)?;
    let card: Card = get(store.conn(), "card", &applied.value.card_id)?;
    let chunks = chunk_count(&applied.value.chunks).unwrap_or(0);
    let short = applied.value.id.clone();
    let display = card.display_id.clone();
    let mut outcome = Outcome::applied_as(
        applied,
        |pass| json!({"pass": pass, "card": display, "chunks": chunks}),
    );
    if chunks > 0 {
        outcome = outcome.with_help(format!(
            "{} run one reviewer per chunk, one after another: tiller review brief {short} --chunk 1 --out brief.md, then tiller spawn {} reviewer --brief-file brief.md",
            tag(&card.display_id),
            card.display_id
        ));
    }
    Ok(outcome.with_help(format!(
        "after the chunks: tiller review brief {short} --summary --out summary.md, then tiller spawn {} reviewer --model sonnet --effort medium --brief-file summary.md",
        card.display_id
    )))
}

fn brief(app: &App, args: ReviewBrief) -> Done {
    let store = app.read()?;
    let pass = resolve_pass(store.conn(), &args.pass)?;
    let project: Project = get(store.conn(), "project", &pass.project_id)?;
    if pass.state != ReviewPassState::Running {
        return Err(Failure::refused(format!(
            "the pass is {}; briefs come from a running pass",
            pass.state
        )));
    }
    let plan: Plan = serde_json::from_value(pass.chunks.clone())
        .map_err(|error| Failure::refused(format!("the pass has no chunk plan: {error}")))?;
    let text = match args.chunk {
        Some(chunk) => chunk_brief(&project, &pass, &plan, chunk, &args.only).ok_or_else(|| {
            Failure::usage(format!(
                "the pass has {} chunks; --chunk is 1 to {}",
                plan.chunks.len(),
                plan.chunks.len()
            ))
        })?,
        None => summary_brief(&project, &pass, &plan),
    };
    Ok(Outcome::read(match args.out {
        Some(path) => {
            std::fs::write(&path, &text)?;
            json!({"path": path, "lines": text.lines().count()})
        }
        None => json!({"brief": text}),
    }))
}

fn finish(app: &App, args: ReviewFinish) -> Done {
    let mut store = app.write()?;
    let pass = resolve_pass(store.conn(), &args.pass)?;
    require_orchestrator(app, &store, &pass.project_id, "finish")?;
    let applied = store.review_finish(
        &app.ctx(),
        &pass.id,
        args.report.as_deref(),
        &args.fix_cards,
    )?;
    let next: Option<Value> = applied
        .value
        .next_obligation
        .as_ref()
        .map(|o| json!(o.due_at));
    Ok(Outcome::applied(applied).with_help(match next {
        Some(due) => format!("the next weekly review is due {due}"),
        None => "no weekly review is scheduled after this pass".into(),
    }))
}

fn abandon(app: &App, args: ReviewAbandon) -> Done {
    let mut store = app.write()?;
    let pass = resolve_pass(store.conn(), &args.pass)?;
    require_orchestrator(app, &store, &pass.project_id, "abandon")?;
    Ok(Outcome::applied(store.review_abandon(
        &app.ctx(),
        &pass.id,
        &args.reason,
    )?))
}

fn due(app: &App, args: ReviewDue) -> Done {
    let store = app.read()?;
    let project = match (args.project, app.session(&store)?) {
        (Some(project), _) => Some(project),
        (None, Some(session)) if session.role == Role::Orchestrator => Some(session.project_id),
        (None, _) => None,
    };
    let due = due_reviews(store.conn(), project.as_deref(), app.ctx().now)?;
    let outcome = Outcome::read(json!({"due": due}));
    Ok(match due.first() {
        Some(first) => outcome.with_help(format!("tiller review start {}", first.project_id)),
        None => outcome,
    })
}
