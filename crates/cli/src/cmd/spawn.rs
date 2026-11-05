//! `tiller spawn` and `tiller orchestrator start`, and the tools they,
//! `tiller done`, `tiller run clean` and `tiller agent` reach outside
//! `state.db` with (T4.2): treehouse, herdr and git.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;
use tiller_agents::control::Waits;
use tiller_agents::spawn::config::{Paths, SpawnConfig};
use tiller_agents::spawn::pool::Treehouse;
use tiller_agents::spawn::trust::ClaudeConfig;
use tiller_agents::spawn::{self as agents, Request, SpawnError, World, readings};
use tiller_core::state::resolve_card;
use tiller_herdr::client::{Cli, HerdrError};
use tiller_reconcile::git::GitScanner;

use super::admin::load_config;
use super::{KeyArg, resolve_session};
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
    /// Build on the card's reported scout: same worktree and branch, and the
    /// card becomes a ship card (builder only)
    #[arg(long)]
    pub promote: bool,
    /// The harness to launch on; G35 refuses one without the guards
    #[arg(long, default_value = "claude")]
    pub harness: String,
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
    tiller_home: PathBuf,
    scratch: PathBuf,
    claude: ClaudeConfig,
}

/// `~/.local/state/tiller`, where the scratch and orchestrator roots
/// default to; `key` is the setting that would make it unnecessary.
fn state_dir(key: &str) -> Result<PathBuf, Failure> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/state/tiller"))
        .ok_or_else(|| Failure::usage(format!("HOME is not set; set {key} in config.toml")))
}

pub(crate) fn words(words: &[String]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// The Tiller home as the launch fragments name it: absolute (a relative
/// `TILLER_HOME` is anchored at the current directory) and without a
/// trailing slash.
fn absolute_home(home: &Path) -> Result<PathBuf, Failure> {
    Ok(std::path::absolute(home)?.components().collect())
}

/// The herdr `tiller spawn` drives: `spawn.herdr`, with `spawn.herdr_socket`
/// for `workspace.move`.
fn herdr_cli(paths: &Paths) -> Cli {
    let herdr = Cli::local().program(words(&paths.herdr));
    match &paths.herdr_socket {
        Some(socket) => herdr.socket(socket),
        None => herdr,
    }
}

/// The herdr every command and the daemon drive: `TILLER_HERDR` when set
/// (tests and labs; the program only, socket and all replaced), else
/// `spawn.herdr` with `spawn.herdr_socket`.
pub fn resolve_herdr(paths: &Paths) -> Cli {
    match crate::daemon::env_command("TILLER_HERDR") {
        Some(program) => Cli::local().program(program),
        None => herdr_cli(paths),
    }
}

/// The herdr command the daemon, `tiller peer serve` and `tiller doctor`
/// reach: `TILLER_HERDR` when set (tests and labs), else `spawn.herdr`.
pub fn herdr_program(paths: &Paths) -> Vec<OsString> {
    crate::daemon::env_command("TILLER_HERDR").unwrap_or_else(|| words(&paths.herdr))
}

/// The daemon's and `tiller peer serve`'s herdr, from the `[spawn]` table of
/// `config.toml`: the one `tiller spawn` drives, unless `TILLER_HERDR`
/// replaces it, socket and all.
pub fn configured_herdr(app: &App) -> Result<Cli, Failure> {
    herdr_from_config(&load_config(app)?)
}

/// [`configured_herdr`] from a `config.toml` already read.
pub fn herdr_from_config(table: &toml::Table) -> Result<Cli, Failure> {
    let paths = Paths::from_table(table).map_err(failure)?;
    Ok(resolve_herdr(&paths))
}

impl Tools {
    pub fn load(app: &App) -> Result<Self, Failure> {
        let config = SpawnConfig::from_table(&load_config(app)?).map_err(failure)?;
        let herdr = resolve_herdr(&config.spawn);
        let scratch = match &config.spawn.scratch_root {
            Some(root) => root.clone(),
            None => state_dir("spawn.scratch_root")?.join("runs"),
        };
        Ok(Self {
            pool: Treehouse::new(words(&config.spawn.treehouse)),
            herdr,
            scanner: GitScanner::new(),
            tiller: std::env::current_exe()?,
            tiller_home: absolute_home(&app.home)?,
            scratch,
            claude: ClaudeConfig::from_env(),
            config,
        })
    }

    /// The run scratch root, which the curator's retro directories sit beside.
    pub fn scratch_root(&self) -> &Path {
        &self.scratch
    }

    /// The packaged `tiller` binary.
    pub fn tiller(&self) -> &Path {
        &self.tiller
    }

    /// The effective Tiller home, absolute and without a trailing slash.
    pub fn tiller_home(&self) -> &Path {
        &self.tiller_home
    }

    pub fn world(&self, ram_free_mb: Option<i64>, disk_free_mb: Option<i64>) -> World<'_> {
        World {
            config: &self.config,
            pool: &self.pool,
            herdr: &self.herdr,
            scanner: &self.scanner,
            tiller: &self.tiller,
            scratch_root: &self.scratch,
            tiller_home: &self.tiller_home,
            claude_config: &self.claude,
            ram_free_mb,
            disk_free_mb,
        }
    }
}

pub(crate) fn read_brief(path: &Path) -> Result<String, Failure> {
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
    if args.promote && args.role != tiller_core::domain::RunRole::Builder {
        return Err(Failure::usage(
            "--promote starts a builder; give the role builder",
        ));
    }
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
        "promote": args.promote,
        "harness": args.harness,
    });
    let keyed = app.key(&store, "run.start", &normalized, &args.key.key)?;
    let asked = args.model.zip(args.effort);
    let applied = if args.promote {
        agents::promote::promote(
            &mut store,
            &app.ctx(),
            Some(&keyed.key),
            &world,
            agents::promote::Request {
                card: card.id,
                asked,
                brief,
                harness: args.harness,
            },
        )
    } else {
        agents::spawn(
            &mut store,
            &app.ctx(),
            Some(&keyed.key),
            &world,
            Request {
                card: card.id,
                role: args.role,
                asked,
                brief,
                harness: args.harness,
            },
        )
    }
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

/// `tiller orchestrator start <project>`: the live orchestrator, or a new
/// one launched in the project's herdr workspace. With `restart`, the live
/// one is stopped first and replaced.
pub fn orchestrator_start(app: &App, project: &str, restart: bool) -> Done {
    let tools = Tools::load(app)?;
    let homes = match &tools.config.spawn.orchestrator_home {
        Some(root) => root.clone(),
        None => state_dir("spawn.orchestrator_home")?.join("orchestrators"),
    };
    let mut store = app.write()?;
    let applied = agents::orchestrator::start(
        &mut store,
        &app.ctx(),
        &tools.world(None, None),
        &homes,
        project,
        restart.then(Waits::default).as_ref(),
    )
    .map_err(failure)?;
    let warning = applied.value.warning.clone();
    Ok(Outcome::applied(applied).with_warning(warning))
}

/// `tiller agent focus <run|session>`: focuses the agent's pane in the
/// herdr client, on its saved machine for a remote host. Writes nothing.
pub fn focus(app: &App, target: &str) -> Done {
    app.require_owner("tiller agent focus")?;
    let tools = Tools::load(app)?;
    let store = app.read()?;
    let session = resolve_session(&store, target)?;
    let (pane, machine) = tiller_core::state::runs::focus_target(store.conn(), &session)?;
    let herdr = match &machine {
        Some(machine) => tools.herdr.clone().on_machine(machine),
        None => tools.herdr.clone(),
    };
    herdr
        .focus_agent(&pane)
        .map_err(|error| failure(SpawnError::Herdr(error)))?;
    Ok(Outcome::read(json!({
        "session_id": session.id,
        "pane_id": pane,
        "machine": machine,
    })))
}
