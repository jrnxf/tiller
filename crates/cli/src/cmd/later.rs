//! `tiller orchestrator` and `tiller agent`: commands whose work happens
//! outside `state.db`, in herdr panes. `orchestrator start` and
//! `agent focus` run in `spawn.rs`; `agent relaunch`, `resume`,
//! `interrupt` and `stop` in `control.rs`.

use clap::Subcommand;

use crate::app::{App, Done};

#[derive(Subcommand, Debug)]
pub enum OrchestratorCmd {
    /// Open the project's herdr workspace and launch its orchestrator
    Start {
        /// The project's id (its slug, like alpha)
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
    Resume {
        /// The run: a ULID or short form, `<display id> <role>`, or a herdr name like
        /// alpha-42-build
        run: String,
    },
    /// Send Escape to a working session. Not idempotent: a second Escape
    /// reaches whatever the agent does next, so it refuses unless the
    /// session is working
    Interrupt {
        /// The run: a ULID or short form, `<display id> <role>`, or a herdr name like
        /// alpha-42-build
        run: String,
    },
    /// Stop the agent of an ended run, or an orchestrator, with a proven
    /// `/exit`, then close its pane and return its worktree when it holds
    /// nothing
    Stop {
        /// A session (a ULID or its short form), or a run, which means its live session
        target: String,
    },
    /// Focus the agent's pane in herdr
    Focus {
        /// A session (a ULID or its short form), or a run, which means its live session
        target: String,
    },
    /// Restart the sessions that run an older tiller, so they pick up the
    /// current role prompt and hooks. An idle session restarts now and a
    /// working one when it next goes idle
    Restart {
        /// The sessions that run an older tiller
        #[arg(long, required = true)]
        stale: bool,
        /// List them without restarting any
        #[arg(long)]
        dry_run: bool,
    },
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
        AgentCmd::Stop { target } => super::control::stop(app, &target),
        AgentCmd::Focus { target } => super::spawn::focus(app, &target),
        AgentCmd::Restart { stale: _, dry_run } => super::stale::restart(app, dry_run),
    }
}
