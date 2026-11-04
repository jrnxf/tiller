//! `tiller mem ...`: evidence and proposals for the memory store
//! (`memory.md` section 12). The record work is in
//! `tiller_core::state::memory`; this module reads the Tiller document at the
//! memory repo's `HEAD` under the repo lock, the same view the board edits.

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_core::memory::document_path;
use tiller_core::state::memory::{Note, Propose, ProposeOp};
use tiller_daemon::memory::apply::Repo;
use tiller_daemon::memory::scrub;

use super::KeyArg;
use crate::app::{App, Done, Failure, Outcome};

#[derive(Subcommand, Debug)]
pub enum MemCmd {
    /// Record one fact as evidence; with --contradicts, that memory comes due
    Note(NoteArgs),
    /// Propose a change to one entry of a project or fleet memory document
    Propose(ProposeArgs),
    /// Approve a memory proposal from the command line
    Approve(ApproveArgs),
}

#[derive(Args, Debug)]
pub struct NoteArgs {
    /// The project the fact is about; a fleet session or the curator notes for its own
    pub project: Option<String>,
    /// The fact, one or two sentences
    #[arg(long)]
    pub text: String,
    /// The evidence kind; default `observation`
    #[arg(long)]
    pub kind: Option<String>,
    /// JSON pointers into the record that back the fact
    #[arg(long = "ref")]
    pub reference: Option<String>,
    /// The card the fact comes from, when it is not the caller's own
    #[arg(long)]
    pub card: Option<String>,
    /// A memory this fact contradicts: it comes due for review
    #[arg(long)]
    pub contradicts: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("op").required(true).args(["add", "rewrite", "retire"])))]
pub struct ProposeArgs {
    /// `fleet`, or a project id
    pub project: String,
    /// Add a line to this section
    #[arg(long, requires = "text")]
    pub add: Option<String>,
    /// Rewrite this memory's line
    #[arg(long, requires = "text")]
    pub rewrite: Option<String>,
    /// Retire this memory's line
    #[arg(long, conflicts_with = "text")]
    pub retire: Option<String>,
    /// The line as it should read
    #[arg(long)]
    pub text: Option<String>,
    /// Evidence rows the change stands on (`tiller mem note` makes them)
    #[arg(long, num_args = 1..)]
    pub evidence: Vec<String>,
    /// What should change, and on which kind of task
    #[arg(long)]
    pub expected_effect: Option<String>,
    /// Why; for a retire, the reason recorded on the memory
    #[arg(long)]
    pub rationale: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct ApproveArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

pub fn run(app: &App, cmd: MemCmd) -> Done {
    match cmd {
        MemCmd::Note(args) => note(app, args),
        MemCmd::Propose(args) => propose(app, args),
        MemCmd::Approve(_) => Err(Failure::unavailable(
            "tiller mem approve is not in this build; Keep a proposal in the board's memory view, which runs the holdout gate",
        )
        .hint("nothing was written")),
    }
}

fn note(app: &App, args: NoteArgs) -> Done {
    let mut store = app.write()?;
    let reference = args
        .reference
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|error| Failure::usage(format!("--ref is not JSON: {error}")))?;
    let note = Note {
        project: args.project,
        text: args.text,
        kind: args.kind,
        reference,
        card: args.card,
        contradicts: args.contradicts,
    };
    let normalized = json!({"note": note});
    let keyed = app.key(&store, "mem.note", &normalized, &args.key.key)?;
    let applied = store.mem_note(&app.ctx(), Some(&keyed.key), &note)?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

fn propose(app: &App, args: ProposeArgs) -> Done {
    let mut store = app.write()?;
    let text = args.text.unwrap_or_default();
    let op = match (args.add, args.rewrite, args.retire) {
        (Some(section), None, None) => ProposeOp::Add { section, text },
        (None, Some(memory), None) => ProposeOp::Rewrite { memory, text },
        (None, None, Some(memory)) => ProposeOp::Retire { memory },
        _ => {
            return Err(Failure::usage(
                "say what changes: --add <section>, --rewrite <memory> or --retire <memory>",
            ));
        }
    };
    let list = scrub::terms(&app.home);
    let lines = match &op {
        ProposeOp::Add { section, text } => vec![section, text],
        ProposeOp::Rewrite { text, .. } => vec![text],
        ProposeOp::Retire { .. } => vec![],
    };
    if let Some(finding) = lines.iter().find_map(|line| scrub::check(line, &list)) {
        return Err(Failure::refused(format!(
            "the line would not pass the memory scrub ({finding})"
        ))
        .hint("memory is written as if it will be public; drop the private name and retry"));
    }
    let propose = Propose {
        project: args.project,
        op,
        evidence: args.evidence,
        expected_effect: args.expected_effect,
        rationale: args.rationale,
    };
    let normalized = json!({"propose": propose});
    let keyed = app.key(&store, "mem.propose", &normalized, &args.key.key)?;
    let repo = Repo::at(&app.home);
    let _lock = repo.lock()?;
    let file = repo.state(&document_path(&propose.project), &list)?;
    let applied = store.mem_propose(&app.ctx(), Some(&keyed.key), &propose, &file)?;
    // The id and the target only: a fleet session never reads a proposals
    // row back (`memory.md` section 9).
    Ok(Outcome::applied_as(applied, |p| {
        json!({
            "id": p.id,
            "state": p.state,
            "project_id": p.project_id,
            "op": p.op,
            "memory_id": p.memory_id,
            "card_id": p.card_id,
        })
    })
    .with_key(&keyed))
}
