//! Commands whose work happens outside `state.db`: they launch panes, call
//! herdr or the forge, or read memory sources. Their arguments are parsed here
//! so the command surface is complete and a wrong call is a usage error, but
//! each answers `unavailable` (exit 7, nothing written) until the task that
//! owns the outside half lands: spawn and orchestrator launch (T4.2), landing
//! (T4.3), relaunch and resume (T4.5), herdr control and doctor (T2.5, T2.8),
//! memory (T7.2).

use clap::{Args, Subcommand};

use crate::app::{App, Done, Failure};

#[derive(Args, Debug)]
pub struct Spawn {
    pub card: String,
    /// scout, builder, reviewer or verifier
    pub role: tiller_core::domain::RunRole,
    #[arg(long, requires = "effort")]
    pub model: Option<String>,
    #[arg(long, requires = "model")]
    pub effort: Option<tiller_core::domain::Effort>,
    #[arg(long)]
    pub brief_file: Option<std::path::PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum OrchestratorCmd {
    /// Open the project's herdr workspace and launch its orchestrator
    Start { project: String },
}

#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    /// A new session on the same run, or a new attempt with a new brief
    Relaunch {
        run: String,
        #[arg(long)]
        note: String,
        #[arg(long, requires = "brief_file")]
        new_attempt: bool,
        #[arg(long)]
        brief_file: Option<std::path::PathBuf>,
    },
    /// Resume the run's Claude session in its worktree
    Resume { run: String },
    /// Send Escape to a working session
    Interrupt { run: String },
    /// Focus the agent's pane in herdr
    Focus { target: String },
}

#[derive(Subcommand, Debug)]
pub enum MemCmd {
    Note(MemArgs),
    Propose(MemArgs),
    Approve(MemArgs),
}

#[derive(Args, Debug)]
pub struct MemArgs {
    #[arg(long)]
    pub contradicts: Option<String>,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub rest: Vec<String>,
}

fn later(what: &str, needs: &str) -> Done {
    Err(Failure::unavailable(format!(
        "{what} needs {needs}, which this build does not have yet"
    ))
    .hint("nothing was written; retry once it ships"))
}

pub fn spawn(_app: &App, args: Spawn) -> Done {
    later(
        &format!("tiller spawn {} {}", args.card, args.role),
        "the spawn path (worktree, herdr pane and capacity reservation, T4.2)",
    )
}

pub fn orchestrator(_app: &App, cmd: OrchestratorCmd) -> Done {
    let OrchestratorCmd::Start { project } = cmd;
    later(
        &format!("tiller orchestrator start {project}"),
        "herdr to open the workspace and launch Claude (T4.2)",
    )
}

pub fn agent(_app: &App, cmd: AgentCmd) -> Done {
    match cmd {
        AgentCmd::Relaunch { run, .. } => later(
            &format!("tiller agent relaunch {run}"),
            "herdr to stop and start the session (T4.5)",
        ),
        AgentCmd::Resume { run } => later(
            &format!("tiller agent resume {run}"),
            "herdr to relaunch Claude with --resume (T4.5)",
        ),
        AgentCmd::Interrupt { run } => later(
            &format!("tiller agent interrupt {run}"),
            "herdr to send the Escape (T2.5)",
        ),
        AgentCmd::Focus { target } => later(
            &format!("tiller agent focus {target}"),
            "herdr to focus the pane (T2.5)",
        ),
    }
}

pub fn mem(_app: &App, _cmd: MemCmd) -> Done {
    later("tiller mem", "the memory store (T7.2)")
}

pub fn doctor(_app: &App) -> Done {
    later(
        "tiller doctor",
        "the health checks (tool versions, herdr, hook registration; T2.8)",
    )
}
