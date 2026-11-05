//! `tiller orchestrator` and `tiller agent`: commands whose work happens
//! outside `state.db`, in herdr panes. `orchestrator start` and
//! `agent focus` run in `spawn.rs`; `agent relaunch`, `resume` and
//! `interrupt` in `control.rs`.

use clap::Subcommand;

use crate::app::{App, Done};

#[derive(Subcommand, Debug)]
pub enum OrchestratorCmd {
    /// Open the project's herdr workspace and launch its orchestrator
    Start {
        project: String,
        /// Stop the live orchestrator and start a new one, so it runs on
        /// the current tiller binary and role prompt (owner only)
        #[arg(long)]
        restart: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentCmd {
    /// A new session on the same run, or a new attempt with a new brief
    Relaunch(super::control::Relaunch),
    /// Resume the run's Claude session in its worktree
    Resume { run: String },
    /// Send Escape to a working session. Not idempotent: a second Escape
    /// reaches whatever the agent does next, so it refuses unless the
    /// session is working
    Interrupt { run: String },
    /// Focus the agent's pane in herdr
    Focus { target: String },
}

pub fn orchestrator(app: &App, cmd: OrchestratorCmd) -> Done {
    let OrchestratorCmd::Start { project, restart } = cmd;
    super::spawn::orchestrator_start(app, &project, restart)
}

pub fn agent(app: &App, cmd: AgentCmd) -> Done {
    match cmd {
        AgentCmd::Relaunch(args) => super::control::relaunch(app, args),
        AgentCmd::Resume { run } => super::control::resume(app, &run),
        AgentCmd::Interrupt { run } => super::control::interrupt(app, &run),
        AgentCmd::Focus { target } => super::spawn::focus(app, &target),
    }
}
