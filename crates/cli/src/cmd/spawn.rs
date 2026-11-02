//! `tiller spawn`, and the tools it, `tiller done` and `tiller run clean`
//! reach outside `state.db` with (T4.2): treehouse, herdr and git.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;
use tiller_agents::spawn::config::SpawnConfig;
use tiller_agents::spawn::pool::Treehouse;
use tiller_agents::spawn::{self as agents, Request, SpawnError, World, readings};
use tiller_core::state::resolve_card;
use tiller_herdr::client::{Cli, HerdrError};
use tiller_reconcile::git::GitScanner;

use super::KeyArg;
use super::admin::load_config;
use crate::app::{App, Done, Failure, Outcome};

#[derive(Args, Debug)]
pub struct Spawn {
    pub card: String,
    /// scout, builder, reviewer or verifier
    pub role: tiller_core::domain::RunRole,
    #[arg(long, requires = "effort")]
    pub model: Option<String>,
    #[arg(long, requires = "model")]
    pub effort: Option<tiller_core::domain::Effort>,
    /// The brief; without it the worker is briefed with the card
    #[arg(long)]
    pub brief_file: Option<PathBuf>,
    #[command(flatten)]
    pub key: KeyArg,
}

/// What a spawn error means to the caller (`protocol.md` 6.1 exit codes).
pub fn failure(error: SpawnError) -> Failure {
    match error {
        SpawnError::State(error) => error.into(),
        SpawnError::Usage(message) => Failure::usage(message),
        SpawnError::Config(message) => Failure::usage(format!("config.toml: {message}"))
            .hint("tiller config set <key> <value>"),
        // A downgrade needs the owner (`protocol.md` 6.4, dispatch rules).
        SpawnError::Downgrade(message) => Failure::forbidden(message),
        error @ (SpawnError::Refused(_) | SpawnError::Pool(_) | SpawnError::Git(_)) => {
            Failure::refused(error.to_string())
        }
        SpawnError::Herdr(error @ HerdrError::Unreachable(_)) => {
            Failure::unavailable(format!("herdr: {error}")).hint("start herdr, then retry")
        }
        error @ (SpawnError::Herdr(_)
        | SpawnError::Role(_)
        | SpawnError::Io(_)
        | SpawnError::Json(_)) => Failure::new("internal", 1, error.to_string()),
        // The spawn's own error decides the exit; the undo notes only add.
        SpawnError::Undone { error, notes } => {
            let mut failure = failure(*error);
            failure.message = format!("{}; {notes}", failure.message);
            failure
        }
    }
}

/// The tools a spawn and a cleanup use, from `config.toml`.
pub struct Tools {
    pub config: SpawnConfig,
    pool: Treehouse,
    herdr: Cli,
    scanner: GitScanner,
    tiller: PathBuf,
    scratch: PathBuf,
}

fn words(words: &[String]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

impl Tools {
    pub fn load(app: &App) -> Result<Self, Failure> {
        let config = SpawnConfig::from_table(&load_config(app)?).map_err(failure)?;
        let mut herdr = Cli::local().program(words(&config.spawn.herdr));
        if let Some(socket) = &config.spawn.herdr_socket {
            herdr = herdr.socket(socket);
        }
        let scratch = match &config.spawn.scratch_root {
            Some(root) => root.clone(),
            None => std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".local/state/tiller/runs"))
                .ok_or_else(|| {
                    Failure::usage("HOME is not set; set spawn.scratch_root in config.toml")
                })?,
        };
        Ok(Self {
            pool: Treehouse::new(words(&config.spawn.treehouse)),
            herdr,
            scanner: GitScanner::new(),
            tiller: std::env::current_exe()?,
            scratch,
            config,
        })
    }

    pub fn world(&self, ram_free_mb: Option<i64>, disk_free_mb: Option<i64>) -> World<'_> {
        World {
            config: &self.config,
            pool: &self.pool,
            herdr: &self.herdr,
            scanner: &self.scanner,
            tiller: &self.tiller,
            scratch_root: &self.scratch,
            ram_free_mb,
            disk_free_mb,
        }
    }
}

fn read_brief(path: &Path) -> Result<String, Failure> {
    let brief = std::fs::read_to_string(path)
        .map_err(|error| Failure::usage(format!("--brief-file {}: {error}", path.display())))?;
    if brief.trim().is_empty() {
        return Err(Failure::usage(format!(
            "--brief-file {} is empty",
            path.display()
        )));
    }
    Ok(brief)
}

pub fn spawn(app: &App, args: Spawn) -> Done {
    let brief = args.brief_file.as_deref().map(read_brief).transpose()?;
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let card = resolve_card(store.conn(), &args.card)?;
    // The worktree lands beside the checkout, so that is the disk that fills.
    let disk = tools
        .config
        .checkouts
        .get(&card.project_id)
        .and_then(|checkout| readings::disk_free_mb(checkout));
    let world = tools.world(readings::ram_free_mb(), disk);
    let normalized = json!({
        "card": card.id,
        "role": args.role,
        "model": args.model,
        "effort": args.effort,
        "brief": brief,
    });
    let keyed = app.key(&store, "run.start", &normalized, &args.key.key)?;
    let asked = args.model.zip(args.effort);
    let applied = agents::spawn(
        &mut store,
        &app.ctx(),
        Some(&keyed.key),
        &world,
        Request {
            card: card.id,
            role: args.role,
            asked,
            brief,
        },
    )
    .map_err(failure)?;
    let warning = match &applied.value {
        agents::Outcome::Spawned(spawned) => spawned.warning.clone(),
        agents::Outcome::Replayed { .. } => None,
    };
    Ok(Outcome::applied(applied)
        .with_key(&keyed)
        .with_warning(warning))
}

/// `tiller run clean`: closes the run's dead pane and returns its worktree
/// once a scan taken now shows it holds nothing.
pub fn clean(app: &App, run: &tiller_core::domain::Run) -> Done {
    let tools = Tools::load(app)?;
    let mut store = app.write()?;
    let applied =
        agents::cleanup(&mut store, &app.ctx(), &tools.world(None, None), run).map_err(failure)?;
    let warning = (!applied.value.warnings.is_empty()).then(|| applied.value.warnings.join("; "));
    Ok(Outcome::applied_as(applied, |cleaned| {
        json!({
            "run": cleaned.run,
            "closed_panes": cleaned.closed_panes,
            "returned": cleaned.returned,
        })
    })
    .with_warning(warning))
}

/// `tiller done` past its outcome: the worktree is read now, and the run
/// ends only when it holds nothing unlanded, or as failed or abandoned.
pub fn done(
    app: &App,
    store: &mut tiller_core::Store,
    run: &tiller_core::domain::Run,
    outcome: tiller_core::domain::RunOutcome,
) -> Done {
    let applied =
        agents::done(store, &app.ctx(), &GitScanner::new(), run, outcome).map_err(failure)?;
    let warning = applied.value.kept.clone();
    Ok(Outcome::applied_as(applied, |ended| json!(ended.run)).with_warning(warning))
}
