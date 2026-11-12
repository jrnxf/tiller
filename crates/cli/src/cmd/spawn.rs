//! `tiller spawn` and `tiller orchestrator start`, and the tools they,
//! `tiller done`, `tiller run clean` and `tiller agent` reach outside
//! `state.db` with (T4.2): treehouse, herdr and git.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::json;
use tiller_agents::control::Waits;
use tiller_agents::spawn::config::{Paths, SpawnConfig};
use tiller_agents::spawn::listeners::Lavish;
use tiller_agents::spawn::pool::Treehouse;
use tiller_agents::spawn::trust::ClaudeConfig;
use tiller_agents::spawn::{self as agents, Request, SpawnError, World, readings};
use tiller_core::state::resolve_card;
use tiller_daemon::api::{BoardRelaunch, Controlled, StartError};
use tiller_daemon::holder::HolderLink;
use tiller_herdr::client::{Cli, HerdrError};
use tiller_panes::{Panes, PanesError};
use tiller_reconcile::git::GitScanner;

use super::admin::load_config;
use super::{KeyArg, resolve_session};
use crate::app::{App, Done, Failure, Outcome};

#[derive(Args, Debug)]
pub struct Spawn {
    /// The card: a display id like alpha-42, a ULID or its short form
    pub card: String,
    /// scout, builder, reviewer or verifier
    pub role: tiller_core::domain::RunRole,
    /// The model to run, like opus; needs --effort. The dispatch rule picks one when left out
    #[arg(long, requires = "effort")]
    pub model: Option<String>,
    /// low, medium, high, xhigh or max; needs --model
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
    /// Start below the quota floor, on the owner's answered "Start now
    /// anyway" decision for the card
    #[arg(long)]
    pub past_quota_floor: bool,
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
        SpawnError::Panes(error @ PanesError::Unreachable(_)) => {
            Failure::unavailable(error.to_string()).hint("start `tiller holder`, then retry")
        }
        SpawnError::Panes(error @ (PanesError::Refused { .. } | PanesError::Version { .. })) => {
            Failure::refused(format!("the terminal holder: {error}"))
        }
        error @ (SpawnError::Herdr(_)
        | SpawnError::Panes(_)
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
    /// The terminal holder new agents launch in, when one has made its
    /// socket in this home; else herdr.
    holder: Option<HolderLink>,
    scanner: GitScanner,
    listeners: Lavish,
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

/// The review page tool `tiller spawn` ends pages with and the daemon reads
/// their annotations from: `TILLER_LAVISH_AXI` when set (tests and labs),
/// else `spawn.lavish`.
pub fn lavish_program(paths: &Paths) -> Vec<OsString> {
    crate::daemon::env_command("TILLER_LAVISH_AXI").unwrap_or_else(|| words(&paths.lavish))
}

/// [`lavish_program`] and the review server's hostnames
/// (`spawn.review_allowed_hosts`) from a `config.toml` already read.
pub fn lavish_from_config(table: &toml::Table) -> Result<(Vec<OsString>, Vec<String>), Failure> {
    let paths = Paths::from_table(table).map_err(failure)?;
    Ok((lavish_program(&paths), paths.review_allowed_hosts))
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
        let tiller_home = absolute_home(&app.home)?;
        // A home whose holder has made its socket launches new agents
        // there; agents already in herdr stay in herdr.
        let holder = HolderLink::present(&tiller_home).then(|| HolderLink::new(&tiller_home));
        Ok(Self {
            pool: Treehouse::new(words(&config.spawn.treehouse)),
            herdr,
            holder,
            scanner: GitScanner::new(),
            listeners: Lavish::new(
                lavish_program(&config.spawn),
                config.spawn.review_allowed_hosts.clone(),
            ),
            tiller: crate::stable::tiller_path()?,
            tiller_home,
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
            panes: self.holder.as_ref().map(|holder| holder as &dyn Panes),
            scanner: &self.scanner,
            listeners: &self.listeners,
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
        "past_quota_floor": args.past_quota_floor,
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
                past_quota_floor: args.past_quota_floor,
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
                past_quota_floor: args.past_quota_floor,
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
    let homes = orchestrator_homes(&tools)?;
    let mut store = app.write()?;
    let applied = start_orchestrator(&tools, &homes, &mut store, &app.ctx(), project, restart)
        .map_err(failure)?;
    let warning = applied.value.warning.clone();
    Ok(Outcome::applied(applied).with_warning(warning))
}

/// Where orchestrator homes live: `spawn.orchestrator_home`, or its default.
pub(super) fn orchestrator_homes(tools: &Tools) -> Result<PathBuf, Failure> {
    match &tools.config.spawn.orchestrator_home {
        Some(root) => Ok(root.clone()),
        None => Ok(state_dir("spawn.orchestrator_home")?.join("orchestrators")),
    }
}

/// The one orchestrator start: `tiller orchestrator start` and the board's
/// Start orchestrator (through the daemon) both come here.
pub(super) fn start_orchestrator(
    tools: &Tools,
    homes: &Path,
    store: &mut tiller_core::Store,
    ctx: &tiller_core::Ctx,
    project: &str,
    restart: bool,
) -> Result<tiller_core::Applied<agents::orchestrator::Orchestrator>, SpawnError> {
    agents::orchestrator::start(
        store,
        ctx,
        &tools.world(None, None),
        homes,
        project,
        restart.then(Waits::default).as_ref(),
    )
}

/// The board's Start orchestrator and its agent controls, which the daemon
/// runs as the board: the tools are read from `config.toml` at each call,
/// as the commands read them, and each call is the command's own path. A
/// live orchestrator is returned unless the owner asked for a restart.
pub struct BoardStart {
    pub app: App,
}

impl tiller_daemon::api::OrchestratorStart for BoardStart {
    fn start(
        &self,
        store: &mut tiller_core::Store,
        ctx: &tiller_core::Ctx,
        project: &str,
        restart: bool,
    ) -> Result<tiller_core::Applied<agents::orchestrator::Orchestrator>, StartError> {
        let tools = Tools::load(&self.app).map_err(refused)?;
        let homes = orchestrator_homes(&tools).map_err(refused)?;
        start_orchestrator(&tools, &homes, store, ctx, project, restart).map_err(
            |error| match error {
                // A start returns the first setting it finds wrong; the board
                // shows every one.
                error @ SpawnError::Config(_) => {
                    let mut refusal = refused(failure(error));
                    let problems = tiller_agents::spawn::settings::problems(
                        &tools.config,
                        &homes,
                        tools.scratch_root(),
                        Some(project),
                    );
                    // Only a refusal that names one of the listed settings is
                    // about them; any other config error stays as it is.
                    if let StartError::Refused {
                        current, message, ..
                    } = &mut refusal
                        && problems
                            .iter()
                            .any(|problem| message.contains(&problem.key))
                    {
                        *current = Some(json!({ "settings": problems }));
                    }
                    refusal
                }
                error => start_error(error),
            },
        )
    }

    fn stop_agent(
        &self,
        store: &mut tiller_core::Store,
        ctx: &tiller_core::Ctx,
        session: &tiller_core::domain::Session,
    ) -> Result<Controlled, StartError> {
        let tools = Tools::load(&self.app).map_err(refused)?;
        controlled(super::control::stop_session(&tools, store, ctx, session))
    }

    fn resume_run(
        &self,
        store: &mut tiller_core::Store,
        ctx: &tiller_core::Ctx,
        run: &tiller_core::domain::Run,
    ) -> Result<Controlled, StartError> {
        let tools = Tools::load(&self.app).map_err(refused)?;
        controlled(super::control::resume_run(&tools, store, ctx, run))
    }

    fn relaunch_run(
        &self,
        store: &mut tiller_core::Store,
        ctx: &tiller_core::Ctx,
        run: &tiller_core::domain::Run,
        relaunch: BoardRelaunch,
    ) -> Result<Controlled, StartError> {
        let tools = Tools::load(&self.app).map_err(refused)?;
        let brief = if relaunch.new_attempt {
            Some(super::control::run_brief(&tools, run).map_err(start_error)?)
        } else {
            None
        };
        controlled(super::control::relaunch_run(
            &tools,
            store,
            ctx,
            ctx.key.as_deref(),
            run,
            tiller_agents::control::RelaunchRequest {
                note: relaunch.note,
                new_attempt: relaunch.new_attempt,
                brief,
                asked: None,
            },
        ))
    }

    fn doctor(&self) -> Option<tiller_daemon::dto::DoctorReport> {
        Some(super::doctor::report(&self.app))
    }

    fn settings(&self) -> tiller_daemon::dto::StartSettings {
        let mut settings = tiller_daemon::dto::StartSettings {
            pool_root: None,
            checkouts_root: None,
            orchestrator_home: None,
            scratch_root: None,
            problems: Vec::new(),
            config_error: None,
        };
        match Tools::load(&self.app).and_then(|tools| {
            let homes = orchestrator_homes(&tools)?;
            Ok((tools, homes))
        }) {
            Ok((tools, homes)) => {
                let shown = |path: &Path| path.display().to_string();
                settings.pool_root = tools.config.spawn.pool_root.as_deref().map(shown);
                settings.checkouts_root = tools.config.spawn.checkouts_root.as_deref().map(shown);
                settings.orchestrator_home = Some(shown(&homes));
                settings.scratch_root = Some(shown(tools.scratch_root()));
                settings.problems = tiller_agents::spawn::settings::problems(
                    &tools.config,
                    &homes,
                    tools.scratch_root(),
                    None,
                )
                .into_iter()
                .map(Into::into)
                .collect();
            }
            Err(failure) => settings.config_error = Some(failure.message.clone()),
        }
        settings
    }

    fn check_setting(&self, key: &str, value: &str) -> Result<Vec<String>, String> {
        let tools = Tools::load(&self.app).map_err(|failure| failure.message.clone())?;
        let homes = orchestrator_homes(&tools).map_err(|failure| failure.message.clone())?;
        tiller_agents::spawn::settings::check(
            &tools.config,
            &homes,
            tools.scratch_root(),
            key,
            value,
        )
    }

    fn restart(
        &self,
        store: &mut tiller_core::Store,
        ctx: &tiller_core::Ctx,
        session: &tiller_core::domain::Session,
    ) -> Result<(), String> {
        self.restart_stale_session(store, ctx, session)
    }

    fn retro_run(
        &self,
        store: &mut tiller_core::Store,
        ctx: &tiller_core::Ctx,
        retro: &str,
    ) -> Result<tiller_daemon::api::CuratorRun, StartError> {
        // A store error goes to the daemon as one, so it checks for damage.
        super::learn::board_run(&self.app, store, ctx, retro).map_err(|error| match error {
            super::learn::ClaimError::State(error) => StartError::from(error),
            super::learn::ClaimError::Refused(failure) => refused(failure),
            // The store's error leads, and the cause and hint follow it.
            super::learn::ClaimError::Unended { error, notes, .. } => StartError::State {
                error,
                notes: Some(notes),
            },
        })
    }
}

impl BoardStart {
    /// The restart of a stale session, the one `tiller agent restart
    /// --stale` makes.
    fn restart_stale_session(
        &self,
        store: &mut tiller_core::Store,
        ctx: &tiller_core::Ctx,
        session: &tiller_core::domain::Session,
    ) -> Result<(), String> {
        let tools = Tools::load(&self.app).map_err(|failure| failure.message.clone())?;
        let homes = orchestrator_homes(&tools).map_err(|failure| failure.message.clone())?;
        super::stale::restart_session(&tools, &homes, store, ctx, session)
    }
}

/// A control's answer as the daemon hands it to the board.
fn controlled<T: serde::Serialize>(
    done: super::control::Warned<T>,
) -> Result<Controlled, StartError> {
    let (applied, warning) = done.map_err(start_error)?;
    let value = serde_json::to_value(&applied.value)
        .map_err(|error| StartError::from(tiller_core::Error::from(error)))?;
    Ok(Controlled {
        applied: tiller_core::Applied {
            value,
            events: applied.events,
            replayed: applied.replayed,
        },
        warning,
    })
}

/// A start or a control that failed, as the board gets it: the store's
/// own error, so the daemon checks the database, or the command's refusal.
fn start_error(error: SpawnError) -> StartError {
    match error {
        SpawnError::State(error) => StartError::from(error),
        SpawnError::Undone { error, notes } => match *error {
            SpawnError::State(error) => StartError::State {
                error: Box::new(error),
                notes: Some(notes),
            },
            error => refused(failure(SpawnError::Undone {
                error: Box::new(error),
                notes,
            })),
        },
        error => refused(failure(error)),
    }
}

/// A refusal as the board gets it: the code and exit the command gives.
fn refused(failure: Failure) -> StartError {
    StartError::Refused {
        exit: failure.exit,
        code: failure.code,
        message: failure.message.clone(),
        hint: failure.hint.clone(),
        current: None,
    }
}

/// `tiller agent focus <run|session>`: focuses the agent's pane in the
/// herdr client, on its saved machine for a remote host. Writes nothing.
pub fn focus(app: &App, target: &str) -> Done {
    app.require_owner("tiller agent focus")?;
    let tools = Tools::load(app)?;
    let store = app.read()?;
    let session = resolve_session(&store, target)?;
    let (pane, machine) = tiller_core::state::runs::focus_target(store.conn(), &session)?;
    if tiller_core::ids::is_holder_pane(&pane) {
        return Err(failure(SpawnError::Refused(format!(
            "focus is not ready yet for an agent in the terminal holder (pane {pane}); watch it in the board's live view"
        ))));
    }
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
