//! `tiller mem ...`: the pending queue (`memory.md` section 4). A fleet
//! session suggests a note with `note`; the owner lists, edits, keeps and
//! discards them, and sees what a launch loads now with `live`. The record
//! work is in `tiller_core::state::memory`; Keep writes to kb through
//! `tiller_core::kb::add` before the row goes.

use std::io::Read as _;

use clap::{Args, Subcommand};
use serde_json::json;
use tiller_agents::roles::Role;
use tiller_agents::spawn::config::SpawnConfig;
use tiller_agents::spawn::memory::{self as launch, STARTUP_BUDGET};
use tiller_agents::spawn::trust::ClaudeConfig;
use tiller_core::kb::{self, KbConfig};
use tiller_core::state::memory::{NewNote, NoteEdit};
use tiller_daemon::memory::dto::{
    MemoryBudget, MemoryKb, MemoryKept, MemoryLive, MemoryPending, state_name,
};

use super::KeyArg;
use super::admin::load_config;
use crate::app::{App, Done, Failure, Outcome};

#[derive(Subcommand, Debug)]
pub enum MemCmd {
    /// Suggest one note for the owner to keep or discard; it loads nowhere until kept
    Note(NoteArgs),
    /// List the notes that wait for the owner
    Pending {
        /// Only this project's notes (its id)
        #[arg(long)]
        project: Option<String>,
    },
    /// Change a pending note's title, body or kind
    Edit(EditArgs),
    /// Keep a pending note: add it to kb, then drop it from the queue
    Keep(DecideArgs),
    /// Discard a pending note
    Discard(DecideArgs),
    /// The notes a launch would load now, with the budget
    Live {
        /// The project whose launch to show (its id)
        #[arg(long)]
        project: Option<String>,
        /// The fleet role whose launch to show; default `builder`
        #[arg(long)]
        role: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct NoteArgs {
    /// `convention`, `decision`, `gotcha` or `how-to`
    #[arg(long)]
    pub kind: String,
    /// One line that says the fact
    #[arg(long)]
    pub title: String,
    /// The detail; read from stdin when absent
    #[arg(long)]
    pub body: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct EditArgs {
    /// The pending note's id
    pub id: String,
    /// The new title: one line, at most 120 characters
    #[arg(long)]
    pub title: Option<String>,
    /// The new body, at most 2000 characters
    #[arg(long)]
    pub body: Option<String>,
    /// `convention`, `decision`, `gotcha` or `how-to`
    #[arg(long)]
    pub kind: Option<String>,
    #[command(flatten)]
    pub key: KeyArg,
}

#[derive(Args, Debug)]
pub struct DecideArgs {
    /// The pending note's id
    pub id: String,
    #[command(flatten)]
    pub key: KeyArg,
}

pub fn run(app: &App, cmd: MemCmd) -> Done {
    match cmd {
        MemCmd::Note(args) => note(app, args),
        MemCmd::Pending { project } => {
            app.require_owner("mem pending")?;
            let store = app.read()?;
            let pending: Vec<MemoryPending> = store
                .pending_notes(project.as_deref())?
                .into_iter()
                .map(MemoryPending::from)
                .collect();
            Ok(Outcome::read(json!({"pending": pending})))
        }
        MemCmd::Edit(args) => edit(app, args),
        MemCmd::Keep(args) => keep(app, args),
        MemCmd::Discard(args) => discard(app, args),
        MemCmd::Live { project, role } => live(app, project, role),
    }
}

fn note(app: &App, args: NoteArgs) -> Done {
    let body = match args.body {
        Some(body) => body,
        None => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text.trim_end().to_owned()
        }
    };
    let note = NewNote {
        project: None,
        kind: args.kind,
        title: args.title,
        body,
    };
    let mut store = app.write()?;
    let normalized = serde_json::to_value(&note)?;
    let keyed = app.key(&store, "mem.note", &normalized, &args.key.key)?;
    let applied = store.mem_note(&app.ctx(), Some(&keyed.key), &note)?;
    Ok(
        Outcome::applied_as(applied, |row| json!(MemoryPending::from(row.clone())))
            .with_key(&keyed),
    )
}

/// The row's current rev: the CLI shows no rev, so it acts on the row as it
/// stands. A row already gone gives 0, and the store refuses it, unless the
/// key replays an earlier run.
fn rev_of(store: &tiller_core::Store, id: &str) -> i64 {
    store.pending_note(id).map(|note| note.rev).unwrap_or(0)
}

fn edit(app: &App, args: EditArgs) -> Done {
    app.require_owner("mem edit")?;
    let edit = NoteEdit {
        title: args.title,
        body: args.body,
        kind: args.kind,
    };
    let mut store = app.write()?;
    let normalized = json!({"id": args.id, "edit": edit});
    let keyed = app.key(&store, "mem.edit", &normalized, &args.key.key)?;
    let rev = rev_of(&store, &args.id);
    let applied = store.mem_edit(&app.ctx(), Some(&keyed.key), &args.id, rev, &edit)?;
    Ok(
        Outcome::applied_as(applied, |row| json!(MemoryPending::from(row.clone())))
            .with_key(&keyed),
    )
}

fn keep(app: &App, args: DecideArgs) -> Done {
    app.require_owner("mem keep")?;
    let table = load_config(app)?;
    let command = KbConfig::from_table(&table)
        .map_err(|error| Failure::usage(format!("config.toml: {error}")))?
        .command(ClaudeConfig::from_env().home_dir().as_deref());
    let mut store = app.write()?;
    let normalized = json!({"id": args.id});
    let keyed = app.key(&store, "mem.keep", &normalized, &args.key.key)?;
    // A replayed Keep finds no row and runs no add.
    let found = store.pending_note(&args.id).ok();
    let checkout = found.as_ref().and_then(|note| {
        SpawnConfig::from_table(&table)
            .ok()
            .and_then(|config| config.checkouts.get(&note.project_id).cloned())
    });
    let rev = found.map(|note| note.rev).unwrap_or(0);
    let ctx = app.ctx().with_key(keyed.key.clone());
    let applied = store.mem_keep_with(&ctx, &args.id, rev, |input| {
        let command = command.ok_or_else(|| kb::NO_COMMAND.to_owned())?;
        kb::add(&command, checkout.as_deref(), input)
    })?;
    Ok(Outcome::applied_as(applied, |kept| {
        json!(MemoryKept {
            note_id: kept.note_id.clone(),
            scope: kept.scope.clone(),
        })
    })
    .with_key(&keyed))
}

fn discard(app: &App, args: DecideArgs) -> Done {
    app.require_owner("mem discard")?;
    let mut store = app.write()?;
    let normalized = json!({"id": args.id});
    let keyed = app.key(&store, "mem.discard", &normalized, &args.key.key)?;
    let rev = rev_of(&store, &args.id);
    let applied = store.mem_discard(&app.ctx(), Some(&keyed.key), &args.id, rev)?;
    Ok(Outcome::applied(applied).with_key(&keyed))
}

/// What a launch of `role` in `project` loads now: the memory view's `live`
/// half (`GET /api/v1/memory`). Without a project, only the store's state.
fn live(app: &App, project: Option<String>, role: Option<String>) -> Done {
    app.require_owner("mem live")?;
    let role_name = role.unwrap_or_else(|| Role::Builder.name().to_owned());
    let role = Role::parse(&role_name)
        .ok_or_else(|| Failure::usage(format!("{role_name} is not a fleet role")))?;
    let kb_home = kb::home_from_env();
    let shown = kb_home.as_ref().map(|path| path.display().to_string());
    let Some(project) = project else {
        let read = kb::read(kb_home.as_deref());
        return Ok(Outcome::read(json!({
            "project_id": null,
            "role": role.name(),
            "kb": MemoryKb { state: state_name(read.state), home: shown, scopes: Vec::new() },
            "budget": MemoryBudget { used: 0, limit: STARTUP_BUDGET },
            "live": Vec::<MemoryLive>::new(),
        })));
    };
    let config = KbConfig::from_table(&load_config(app)?)
        .map_err(|error| Failure::usage(format!("config.toml: {error}")))?;
    let store = app.read()?;
    let project = store.project(&project)?;
    let rendered = launch::render(&config, kb_home.as_deref(), &project, role)
        .map_err(super::spawn::failure)?;
    Ok(Outcome::read(json!({
        "project_id": project.id,
        "role": role.name(),
        "kb": MemoryKb {
            state: state_name(rendered.kb),
            home: shown,
            scopes: launch::scopes(&config, &project),
        },
        "budget": MemoryBudget { used: rendered.used, limit: STARTUP_BUDGET },
        "live": rendered.entries.iter().map(MemoryLive::from).collect::<Vec<_>>(),
    })))
}
