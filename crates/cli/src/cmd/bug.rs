//! `tiller bug file` (`protocol.md` section 6.4, Tiller bugs).

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_core::state::cards::{BugRoute, NewBug};

use super::{KeyArg, tag};
use crate::app::{App, Done, Failure, Outcome, text_input};

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// File a Tiller bug where this host triages them
    File(File),
}

#[derive(Args, Debug)]
pub struct File {
    /// A short title for the bug
    #[arg(long)]
    pub title: String,
    /// What happened and what you expected; - reads standard input
    #[arg(long)]
    pub body: String,
    /// What you saw, as JSON (a command, its exit code, its output)
    #[arg(long)]
    pub evidence: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

pub fn run(app: &App, cmd: Cmd) -> Done {
    let Cmd::File(args) = cmd;
    file(app, args)
}

fn file(app: &App, args: File) -> Done {
    let body = text_input(Some(args.body), None, "the body")?;
    let evidence = match &args.evidence {
        Some(text) => Some(
            serde_json::from_str::<Value>(text)
                .map_err(|error| Failure::usage(format!("--evidence is not JSON: {error}")))?,
        ),
        None => None,
    };
    let normalized = json!({"title": args.title, "body": body, "evidence": evidence});
    let bug = NewBug {
        title: args.title,
        body,
        evidence,
    };
    let mut store = app.write()?;
    let keyed = app.key(&store, "bug.file", &normalized, &args.key.key)?;
    let applied = store.bug_file(&app.ctx(), Some(&keyed.key), bug)?;
    let card = &applied.value.card;
    let help = match applied.value.route {
        BugRoute::BugProject => format!(
            "filed as {}; the {} orchestrator triages it",
            tag(&card.display_id),
            card.project_id
        ),
        BugRoute::NoBugProject => format!(
            "filed as {} in fleet: this host has no bug project, so it waits for the owner",
            tag(&card.display_id)
        ),
        BugRoute::BugProjectRetired => format!(
            "filed as {} in fleet: this host's bug project is retired, so it waits for the owner",
            tag(&card.display_id)
        ),
    };
    Ok(Outcome::applied(applied).with_key(&keyed).with_help(help))
}
