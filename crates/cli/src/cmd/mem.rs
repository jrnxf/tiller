//! `tiller mem ...`: evidence and proposals for the memory store
//! (`memory.md` section 12). The record work is in
//! `tiller_core::state::memory`; this module reads the Tiller document at the
//! memory repo's `HEAD` under the repo lock, the same view the board edits.

use clap::{Args, Subcommand};
use serde_json::{Value, json};
use tiller_core::memory::document_path;
use tiller_core::state::learning::Gate;
use tiller_core::state::memory::{
    Note, PendingFilter, Proposal, Propose, ProposeOp, proposal_class, proposal_text,
};
use tiller_core::state::{get, resolve_id};
use tiller_daemon::memory::Writer;
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
    /// Approve open memory proposals: by id, or every one a filter matches
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

/// Name proposals by id, or by filter for a bulk approval; never both, and
/// never neither, so a bare `tiller mem approve` approves nothing.
#[derive(Args, Debug)]
#[command(group(clap::ArgGroup::new("pick").required(true).multiple(true)
    .args(["proposals", "class", "boundary", "project"])))]
pub struct ApproveArgs {
    /// Proposal ids (the full ULID or its last 7 characters)
    #[arg(group = "pick", conflicts_with_all = ["class", "boundary", "project"])]
    pub proposals: Vec<String>,
    /// Every open proposal of this class (rule, requirement, preference, note, ...)
    #[arg(long)]
    pub class: Option<String>,
    /// Every open proposal in this boundary
    #[arg(long, value_parser = ["personal", "work"])]
    pub boundary: Option<String>,
    /// Every open proposal for this project id (`fleet` for the shared document)
    #[arg(long)]
    pub project: Option<String>,
    /// List what would be approved and write nothing
    #[arg(long)]
    pub dry_run: bool,
    #[command(flatten)]
    pub key: KeyArg,
}

pub fn run(app: &App, cmd: MemCmd) -> Done {
    match cmd {
        MemCmd::Note(args) => note(app, args),
        MemCmd::Propose(args) => propose(app, args),
        MemCmd::Approve(args) => approve(app, args),
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

/// Keep on each named or matched proposal, one by one, through the same
/// `Writer::decide` the board's Keep runs: a held proposal is refused with
/// the gate's reason and the rest still go. A proposal that is no longer
/// `proposed` is skipped.
fn approve(app: &App, args: ApproveArgs) -> Done {
    app.require_owner("mem approve")?;
    let filter = PendingFilter {
        class: args.class,
        boundary: args.boundary,
        project: args.project,
    };
    let by_id = !args.proposals.is_empty();
    let store = if args.dry_run {
        app.read()?
    } else {
        app.write()?
    };
    let chosen: Vec<Proposal> = if by_id {
        let mut rows = Vec::new();
        for given in &args.proposals {
            let id = resolve_id(store.conn(), "proposals", "proposal", given)?;
            let row: Proposal = get(store.conn(), "proposal", &id)?;
            if !rows.iter().any(|seen: &Proposal| seen.id == row.id) {
                rows.push(row);
            }
        }
        rows
    } else {
        store.mem_pending(&filter)?
    };
    let line = |p: &Proposal| {
        json!({
            "id": p.id,
            "project_id": p.project_id,
            "boundary": p.boundary,
            "class": proposal_class(p),
            "op": p.op,
            "text": proposal_text(p),
        })
    };
    let with = |p: &Proposal, result: &str, extra: Value| {
        let mut item = line(p);
        item["result"] = json!(result);
        if let (Some(item), Some(extra)) = (item.as_object_mut(), extra.as_object()) {
            item.extend(extra.clone());
        }
        item
    };
    if args.dry_run {
        let repo = Repo::at(&app.home);
        let list = scrub::terms(&app.home);
        let results = chosen
            .iter()
            .map(|p| {
                if p.state != "proposed" {
                    return Ok(with(p, "skipped", json!({"reason": not_proposed(p)})));
                }
                let gate = store.learning_gate(&p.id)?;
                if !gate.ok {
                    return Ok(with(
                        p,
                        "would_refuse",
                        json!({"reason": gate_reason(&gate)}),
                    ));
                }
                // Each one is counted on the approved set as it is now, not
                // after the ones before it in this run.
                let file = repo.state(&document_path(&p.project_id), &list)?;
                Ok(
                    match store.mem_keep_budget(&p.id, &file, &|sha| repo.blob(sha)) {
                        Ok(_) => with(p, "would_approve", json!({})),
                        Err(error) => {
                            with(p, "would_refuse", json!({"reason": refusal(p, error)?}))
                        }
                    },
                )
            })
            .collect::<Result<Vec<Value>, Failure>>()?;
        let count = |what: &str| results.iter().filter(|r| r["result"] == what).count();
        return Ok(Outcome::read(json!({
            "dry_run": true,
            "would_approve": count("would_approve"),
            "would_refuse": count("would_refuse"),
            "skipped": count("skipped"),
            "results": results,
        })));
    }
    let normalized = json!({"approve": chosen.iter().map(|p| &p.id).collect::<Vec<_>>()});
    let keyed = app.key(&store, "mem.approve", &normalized, &args.key.key)?;
    let mut writer = Writer::open(store, &app.home)?;
    let mut results = Vec::new();
    let mut kept: Vec<(String, String)> = Vec::new();
    for p in &chosen {
        if p.state != "proposed" {
            results.push(with(p, "skipped", json!({"reason": not_proposed(p)})));
            continue;
        }
        // Each proposal is decided under a key of its own, named by its id
        // and bound to it: a later run under the same `--key` that finds
        // another proposal pending decides that one, and never replays an
        // earlier proposal's answer (protocol.md 6.3).
        let key = format!("{}:{}", keyed.key, p.id);
        let ctx = app
            .ctx()
            .with_key(key.clone())
            .with_args(key, &json!({"approve": p.id}));
        let after: Vec<String> = kept
            .iter()
            .filter(|(project, _)| *project == p.project_id)
            .map(|(_, id)| id.clone())
            .collect();
        match writer.decide(&ctx, &p.id, true, None, &after) {
            Ok((landed, _)) => {
                kept.push((p.project_id.clone(), p.id.clone()));
                results.push(with(
                    p,
                    "approved",
                    json!({"proposal_id": landed.proposal_id, "approval_id": landed.approval_id}),
                ));
            }
            Err(error @ tiller_core::Error::Refused { .. }) => {
                results.push(with(p, "refused", json!({"reason": refusal(p, error)?})));
            }
            Err(error) => results.push(with(p, "failed", json!({"reason": error.to_string()}))),
        }
    }
    let count = |what: &str| results.iter().filter(|r| r["result"] == what).count();
    let (approved, skipped) = (count("approved"), count("skipped"));
    let (refused, failed) = (count("refused"), count("failed"));
    let held = refused + failed;
    let warning =
        (held > 0).then(|| format!("{held} proposal(s) were not approved; see their reasons"));
    let mut outcome = Outcome::read(json!({
        "dry_run": false,
        "approved": approved,
        "skipped": skipped,
        "refused": refused,
        "failed": failed,
        "results": results,
    }))
    .with_warning(warning)
    .with_key(&keyed);
    outcome.mutation = Some((std::mem::take(&mut writer.events), writer.replayed));
    Ok(outcome)
}

/// A refusal's reason as the line about `proposal` shows it: the fact
/// alone when it is about that proposal, whose id the line already has.
/// An error that is not a refusal stays the error.
fn refusal(proposal: &Proposal, error: tiller_core::Error) -> Result<String, Failure> {
    match error {
        tiller_core::Error::Refused { entity, id, fact }
            if entity == "proposal" && id == proposal.id =>
        {
            Ok(fact)
        }
        error @ tiller_core::Error::Refused { .. } => Ok(error.to_string()),
        error => Err(error.into()),
    }
}

fn not_proposed(proposal: &Proposal) -> String {
    format!("it is {}, not proposed", proposal.state)
}

/// What the holdout gate says, with the command that clears it.
fn gate_reason(gate: &Gate) -> String {
    let reason = gate.reason.clone().unwrap_or_default();
    match &gate.hint {
        Some(hint) => format!("{reason}; {hint}"),
        None => reason,
    }
}
