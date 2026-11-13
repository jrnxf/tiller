//! Runs, status lines, PR links, the outbox (`tiller send`, `tiller inbox`)
//! and `tiller done` (`protocol.md` section 6.4, Runs and agents).

use clap::{Args, Subcommand};
use serde_json::json;
use tiller_core::domain::{
    Artifact, ArtifactKind, Card, Delivery, OutboxKind, OutboxMessage, Pr, PrState, Project,
    Review, Run, RunOutcome, RunRole, StatusState,
};
use tiller_core::state::approvals::NewPr;
use tiller_core::state::publishes::publish;
use tiller_core::state::{find, get, list, resolve_card, resolve_id};
use tiller_herdr::client::Herdr;

use super::card::herdr_name;
use super::{KeyArg, resolve_run, resolve_session};
use crate::app::{App, Done, Failure, Outcome, body_input, text_input};
use crate::lisbon;

#[derive(Args, Debug)]
pub struct Status {
    /// working, needs-decision, blocked, paused, done or failed
    pub state: StatusState,
    /// The status line, one short sentence
    pub text: String,
    /// With paused: silence this run's alarms until then
    #[arg(long)]
    pub until: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum RunCmd {
    /// Pause a run until a date
    Pause {
        /// The run: a ULID or short form, `<display id> <role>`, or a herdr name like
        /// alpha-42-build
        run: String,
        /// When the pause ends: 2026-11-07 (09:00 Lisbon), 2026-11-07T14:00 (Lisbon) or an RFC
        /// 3339 time with an offset
        #[arg(long)]
        until: String,
        /// Why the run is paused
        #[arg(long)]
        reason: String,
    },
    /// Show a run
    Show {
        /// The run: a ULID or short form, `<display id> <role>`, or a herdr name like
        /// alpha-42-build
        run: String,
        /// Lines of the pane to include, at most 200
        #[arg(long)]
        tail: Option<u32>,
    },
    /// List runs
    List {
        /// Only this card's runs: a display id like alpha-42, a ULID or its short form
        #[arg(long)]
        card: Option<String>,
        /// Only runs that have not ended
        #[arg(long)]
        active: bool,
    },
    /// Close a run's pane record and return its worktree record
    Clean {
        /// The run: a ULID or short form, `<display id> <role>`, or a herdr name like
        /// alpha-42-build
        run: String,
    },
}

#[derive(Args, Debug)]
pub struct Done_ {
    /// Required outside a worker session
    pub run: Option<String>,
    /// How the run ended: landed, reported, failed, abandoned, relaunched, cancelled or cut
    #[arg(long)]
    pub outcome: Option<RunOutcome>,
}

#[derive(Subcommand, Debug)]
pub enum Pr_ {
    /// Link a PR to the run's card
    Link {
        /// The pull request URL, like https://github.com/owner/name/pull/42
        url: String,
        /// The card to link it to: a display id, a ULID or its short form. A worker session
        /// defaults to its own card
        #[arg(long)]
        card: Option<String>,
    },
    /// Give the title and body the daemon opens the run's PR with
    /// (direct-pr delivery: the worker commits, the daemon pushes and opens)
    Describe {
        /// The PR title
        #[arg(long)]
        title: String,
        /// The PR body in markdown
        #[arg(long)]
        body: Option<String>,
        /// A file holding the body, or - for standard input
        #[arg(long)]
        body_file: Option<std::path::PathBuf>,
    },
}

#[derive(Args, Debug)]
pub struct Send {
    /// A run or a session
    pub target: String,
    /// steer or note
    #[arg(long)]
    pub kind: OutboxKind,
    /// The message; or pass - instead to read it from standard input
    #[arg(long)]
    pub text: Option<String>,
    /// Pass - to read the message from standard input
    #[arg(value_parser = ["-"])]
    pub stdin: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct Inbox {
    /// The message id from a wake-up line; none drains everything pending
    pub message: Option<String>,
}

pub fn status(app: &App, args: Status) -> Done {
    if args.until.is_some() && args.state != StatusState::Paused {
        return Err(Failure::usage("--until goes with the paused state"));
    }
    let until = args.until.as_deref().map(lisbon::parse_when).transpose()?;
    if until.is_some_and(|until| until <= tiller_core::Timestamp::now()) {
        // Checked before the status line is written, so a bad date leaves nothing behind.
        return Err(Failure::refused("--until is not in the future"));
    }
    let mut store = app.write()?;
    let session = app
        .session(&store)?
        .ok_or_else(|| Failure::forbidden("tiller status is run by a worker session"))?;
    let run_id = session
        .run_id
        .ok_or_else(|| Failure::refused("the calling session has no run"))?;
    if args.state == StatusState::Done {
        undescribed(&store, &run_id)?;
    }
    let keyed = app.key(
        &store,
        "run.status",
        &json!({"state": args.state, "text": args.text, "until": until}),
        &None,
    )?;
    let mut applied = store.run_status(
        &app.ctx().with_key(&keyed.key),
        &run_id,
        args.state,
        &args.text,
    )?;
    if let Some(until) = until {
        let paused = store.run_pause(&app.ctx(), &run_id, until, Some(&args.text))?;
        applied.events.extend(paused.events);
        applied.replayed &= paused.replayed;
        applied.value = paused.value;
    }
    Ok(Outcome::applied(applied))
}

/// A direct-pr builder reports done only once its PR is described: the
/// daemon pushes the branch and opens the PR from that description, and
/// without one it would have nothing to open.
fn undescribed(store: &tiller_core::store::Store, run_id: &str) -> Result<(), Failure> {
    let run: Run = get(store.conn(), "run", run_id)?;
    if run.role != RunRole::Builder || run.ended_at.is_some() {
        return Ok(());
    }
    let project: Project = get(
        store.conn(),
        "project",
        &store.card(&run.card_id)?.project_id,
    )?;
    if project.effective_delivery() != Delivery::DirectPr
        || publish(store.conn(), &run.id)?.is_some()
    {
        return Ok(());
    }
    Err(Failure::refused(
        "describe the PR first: tiller pr describe --title <t> --body-file <f>; \
         the daemon pushes the branch and opens the PR when you report done",
    ))
}

pub fn run_cmd(app: &App, cmd: RunCmd) -> Done {
    match cmd {
        RunCmd::Pause { run, until, reason } => {
            let until = lisbon::parse_when(&until)?;
            let mut store = app.write()?;
            let run = resolve_run(&store, &run)?;
            let applied = store.run_pause(&app.ctx(), &run.id, until, Some(&reason))?;
            Ok(Outcome::applied_as(
                applied,
                |run| json!({"run": run, "reason": reason}),
            ))
        }
        RunCmd::Show { run, tail } => show(app, &run, tail),
        RunCmd::List { card, active } => list_runs(app, card, active),
        RunCmd::Clean { run } => clean(app, &run),
    }
}

fn run_row(store: &tiller_core::Store, run: &Run) -> Result<serde_json::Value, Failure> {
    let card: Card = get(store.conn(), "card", &run.card_id)?;
    let session = tiller_core::state::runs::live_session(store.conn(), &run.id)?;
    Ok(json!({
        "run": run,
        "name": herdr_name(&card.display_id, run),
        "card": card.display_id,
        "session": session,
    }))
}

fn show(app: &App, reference: &str, tail: Option<u32>) -> Done {
    let store = app.read()?;
    let run = resolve_run(&store, reference)?;
    let mut data = run_row(&store, &run)?;
    let events: Vec<tiller_core::domain::Event> = list(
        store.conn(),
        "entity = 'run' AND entity_id = ?1 ORDER BY seq DESC LIMIT 20",
        &[&run.id],
    )?;
    data["events"] = json!(events);
    let requested = tail.unwrap_or(0).min(200);
    data["pane_tail"] = if requested == 0 {
        json!({"requested": 0, "lines": null, "available": false})
    } else {
        match read_tail(app, &store, &run, requested as usize) {
            Ok(lines) => json!({"requested": requested, "lines": lines, "available": true}),
            Err(reason) => {
                json!({"requested": requested, "lines": null, "available": false, "reason": reason})
            }
        }
    };
    Ok(Outcome::read(data))
}

/// The last `keep` lines of the run's live pane, read the way the board
/// reads it: the terminal holder for a holder pane, else the saved machine
/// of a remote host, or the local herdr. The error is why there is no tail, for the caller to read.
fn read_tail(
    app: &App,
    store: &tiller_core::Store,
    run: &Run,
    keep: usize,
) -> Result<Vec<String>, String> {
    let conn = store.conn();
    let session = tiller_core::state::runs::live_session(conn, &run.id)
        .map_err(|error| error.to_string())?
        .ok_or("the run has no live session")?;
    let pane = tiller_core::state::runs::focus_target(conn, &session)
        .map_err(|_| "the session has no pane".to_owned())?;
    if tiller_core::ids::is_holder_pane(&pane) {
        use tiller_panes::Panes as _;
        let home = super::spawn::absolute_home(&app.home).map_err(|f| f.message.clone())?;
        if !tiller_daemon::holder::HolderLink::present(&home) {
            return Err("no holder runs on this host".into());
        }
        return tiller_daemon::holder::HolderLink::new(&home)
            .screen(
                &tiller_panes::PaneId::from(pane.as_str()),
                tiller_panes::Read::Visible,
                tiller_panes::Format::Text,
            )
            .map(|screen| tiller_daemon::api::pane_tail(&screen.text(), keep))
            .map_err(|error| format!("the terminal holder cannot read the pane: {error}"));
    }
    let host: tiller_core::domain::Host =
        get(conn, "host", &session.host_id).map_err(|error| error.to_string())?;
    if host.kind == tiller_core::domain::HostKind::Remote {
        return Err(format!(
            "the session is on the remote host {}; read it there",
            host.label
        ));
    }
    let herdr = super::spawn::configured_herdr(app).map_err(|failure| failure.message.clone())?;
    herdr
        .read_visible(&pane)
        .map(|screen| tiller_daemon::api::pane_tail(&screen, keep))
        .map_err(|error| format!("herdr cannot read the pane: {error}"))
}

fn list_runs(app: &App, card: Option<String>, active: bool) -> Done {
    let store = app.read()?;
    let card_id = card
        .map(|card| resolve_card(store.conn(), &card).map(|card| card.id))
        .transpose()?;
    let runs: Vec<Run> = list(
        store.conn(),
        "(?1 IS NULL OR card_id = ?1) AND (?2 = 0 OR ended_at IS NULL) ORDER BY started_at, id",
        &[&card_id, &(active as i64)],
    )?;
    let rows = runs
        .iter()
        .map(|run| run_row(&store, run))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Outcome::read(json!({"runs": rows})))
}

fn clean(app: &App, reference: &str) -> Done {
    let run = resolve_run(&app.read()?, reference)?;
    super::spawn::clean(app, &run)
}

pub fn done(app: &App, args: Done_) -> Done {
    let mut store = app.write()?;
    let run = match &args.run {
        Some(reference) => resolve_run(&store, reference)?,
        None => {
            let session = app.session(&store)?.ok_or_else(|| {
                Failure::usage("name the run: tiller done <run> (outside a worker session)")
            })?;
            let run_id = session
                .run_id
                .ok_or_else(|| Failure::refused("the calling session has no run"))?;
            get(store.conn(), "run", &run_id)?
        }
    };
    let outcome = match args.outcome {
        Some(outcome) => outcome,
        None => default_outcome(&store, &run)?,
    };
    super::spawn::done(app, &mut store, &run, outcome)
}

/// What a run ends as without `--outcome` (`protocol.md` 6.4, `tiller done`).
fn default_outcome(store: &tiller_core::Store, run: &Run) -> Result<RunOutcome, Failure> {
    let conn = store.conn();
    match run.role {
        RunRole::Scout => {
            let report: Option<Artifact> = find(
                conn,
                "run_id = ?1 AND kind = ?2 AND retired_at IS NULL",
                &[&run.id, &ArtifactKind::Report],
            )?;
            report.map(|_| RunOutcome::Reported).ok_or_else(|| {
                Failure::refused("no report is recorded for this run")
                    .hint("tiller artifact add <card> --kind report")
            })
        }
        RunRole::Reviewer | RunRole::Verifier => {
            let review: Option<Review> = find(conn, "run_id = ?1", &[&run.id])?;
            review.map(|_| RunOutcome::Reported).ok_or_else(|| {
                Failure::refused("no review is recorded for this run").hint("tiller review record")
            })
        }
        RunRole::Builder => {
            let prs: Vec<Pr> = list(conn, "card_id = ?1 ORDER BY number", &[&run.card_id])?;
            if prs.iter().any(|pr| pr.state == PrState::Merged) {
                return Ok(RunOutcome::Landed);
            }
            // Ending on an open PR frees the slot but ends the session, so
            // the builder asks for it by name.
            match prs.iter().find(|pr| pr.state == PrState::Open) {
                Some(pr) => Err(Failure::refused(format!(
                    "PR #{} is not merged; stay on the card, or end with --outcome reported once it holds your last commit",
                    pr.number
                ))
                .hint("tiller done --outcome reported")),
                None => Err(Failure::refused("the work is not landed; stay on the card")),
            }
        }
    }
}

pub fn pr_cmd(app: &App, cmd: Pr_) -> Done {
    match cmd {
        Pr_::Link { url, card } => pr_link(app, url, card),
        Pr_::Describe {
            title,
            body,
            body_file,
        } => {
            let body = body_input(body, body_file.as_deref())?
                .ok_or_else(|| Failure::usage("give the PR body with --body or --body-file"))?;
            let mut store = app.write()?;
            let run_id = app
                .session(&store)?
                .and_then(|session| session.run_id)
                .ok_or_else(|| {
                    Failure::forbidden("tiller pr describe is run by a worker session")
                })?;
            let applied = store.pr_describe(&app.ctx(), &run_id, &title, &body)?;
            Ok(Outcome::applied(applied))
        }
    }
}

fn pr_link(app: &App, url: String, card: Option<String>) -> Done {
    let number = pr_number(&url)?;
    let mut store = app.write()?;
    let session = app.session(&store)?;
    let run_id = session.as_ref().and_then(|session| session.run_id.clone());
    let card = match (card, &run_id) {
        (Some(card), _) => resolve_card(store.conn(), &card)?.id,
        (None, Some(run_id)) => get::<Run>(store.conn(), "run", run_id)?.card_id,
        (None, None) => {
            return Err(Failure::usage(
                "name the card: tiller pr link <url> --card <card>",
            ));
        }
    };
    let applied = store.pr_link(
        &app.ctx(),
        NewPr {
            card,
            run_id,
            number,
            url,
            head_sha: None,
            base_ref: None,
        },
    )?;
    Ok(Outcome::applied(applied))
}

fn pr_number(url: &str) -> Result<i64, Failure> {
    url.trim_end_matches('/')
        .rsplit_once("/pull/")
        .and_then(|(_, number)| number.split(['/', '?', '#']).next())
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| {
            Failure::usage(format!(
                "not a pull request URL: {url:?} (expected .../pull/<n>)"
            ))
        })
}

pub fn send(app: &App, args: Send) -> Done {
    if !matches!(args.kind, OutboxKind::Steer | OutboxKind::Note) {
        return Err(Failure::usage("send takes --kind steer or note"));
    }
    let text = text_input(args.text.clone(), args.stdin.clone(), "the message")?;
    let mut store = app.write()?;
    let session = resolve_session(&store, &args.target)?;
    let keyed = app.key(
        &store,
        "outbox.enqueue",
        &json!({"target": session.id, "kind": args.kind, "text": text}),
        &args.key.key,
    )?;
    let applied = store.outbox_send(&app.ctx(), Some(&keyed.key), &session.id, args.kind, text)?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

pub fn inbox(app: &App, args: Inbox) -> Done {
    let mut store = app.write()?;
    let caller = app.session(&store)?;
    let mut events = Vec::new();
    let mut replayed = true;
    let mut messages: Vec<OutboxMessage> = Vec::new();
    if let Some(reference) = &args.message {
        let id = resolve_id(store.conn(), "outbox", "outbox message", reference)?;
        let applied = store.outbox_ack(&app.ctx(), &id)?;
        replayed &= applied.replayed;
        events.extend(applied.events);
        let first = applied.value;
        // The line names the oldest; the ack returns every pending message for
        // the session, so a batch reaches the agent in one go (3.3).
        if caller
            .as_ref()
            .is_some_and(|session| session.id == first.target_session_id)
        {
            let rest = store.outbox_drain(&app.ctx(), &first.target_session_id)?;
            replayed &= rest.replayed;
            events.extend(rest.events);
            messages.push(first);
            messages.extend(rest.value);
        } else {
            messages.push(first);
        }
    } else {
        let session = caller.ok_or_else(|| {
            Failure::usage("tiller inbox with no id drains the calling session's messages; run it in a session")
        })?;
        let applied = store.outbox_drain(&app.ctx(), &session.id)?;
        replayed = applied.replayed;
        events = applied.events;
        messages = applied.value;
    }
    let notes: Vec<_> = messages
        .iter()
        .filter(|message| message.state == tiller_core::domain::OutboxState::Failed)
        .map(|message| format!("message {} was given up on; reconcile owns it", message.id))
        .collect();
    let mut outcome = Outcome::applied(tiller_core::Applied {
        value: json!({"messages": messages}),
        events,
        replayed,
    });
    outcome.warning = notes.into_iter().next();
    Ok(outcome)
}
