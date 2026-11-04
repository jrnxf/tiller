//! Commands whose work happens outside `state.db`: they launch panes, call
//! herdr or the forge, or read memory sources. Their arguments are parsed here
//! so the command surface is complete and a wrong call is a usage error.
//! `orchestrator start` and `agent focus` run (`spawn.rs`); the rest answer
//! `unavailable` (exit 7, nothing written) until the task that owns the
//! outside half lands: relaunch and resume (T4.5), interrupt and doctor
//! (T2.5, T2.8).

use clap::Subcommand;

use crate::app::{App, Done, Failure};

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

fn later(what: &str, needs: &str) -> Done {
    Err(Failure::unavailable(format!(
        "{what} needs {needs}, which this build does not have yet"
    ))
    .hint("nothing was written; retry once it ships"))
}

pub fn orchestrator(app: &App, cmd: OrchestratorCmd) -> Done {
    let OrchestratorCmd::Start { project } = cmd;
    super::spawn::orchestrator_start(app, &project)
}

pub fn agent(app: &App, cmd: AgentCmd) -> Done {
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
        AgentCmd::Focus { target } => super::spawn::focus(app, &target),
    }
}

pub fn doctor(_app: &App) -> Done {
    later(
        "tiller doctor",
        "the health checks (tool versions, herdr, hook registration; T2.8)",
    )
}
