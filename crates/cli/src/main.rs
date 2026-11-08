mod app;
mod cmd;
mod daemon;
mod guard;
mod hook;
mod lisbon;
mod toon;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use serde_json::Value;

use app::{App, Done, Failure};
use cmd::{
    admin, ahoy, approval, ask, attention, backup, bug, card, doctor, health, import, later, learn,
    mem, peer, read, records, review, run, spawn,
};

#[derive(Parser)]
#[command(name = "tiller", version, about)]
struct Cli {
    /// Print the stable JSON response instead of TOON
    #[arg(long, global = true)]
    json: bool,
    /// The state database; ~/.tiller/state.db by default
    #[arg(long, global = true, env = "TILLER_DB")]
    db: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Set up this host: its row, the fleet project and the health card
    Init(admin::InitArgs),
    /// Asks: what the owner or a script sent an orchestrator
    #[command(subcommand)]
    Ask(ask::Cmd),
    /// Intake statistics for the Stop hook's switching rule
    #[command(subcommand)]
    Intake(IntakeCmd),
    /// Cards
    #[command(subcommand)]
    Card(card::Cmd),
    /// Tiller bugs: file one where this host triages them
    #[command(subcommand)]
    Bug(bug::Cmd),
    /// Recap and walk through the open decisions (the owner's `/ahoy`)
    Ahoy(ahoy::Ahoy),
    /// Questions waiting for the owner
    #[command(subcommand)]
    Attention(attention::Cmd),
    /// Approvals to land work
    #[command(subcommand)]
    Approval(approval::Request),
    /// Decide an approval (owner)
    Approve(approval::Approve),
    /// Land an accepted approval through the daemon
    Land(approval::Land),
    /// Standing decisions to land matching work
    #[command(subcommand)]
    Grant(approval::Grant),
    /// Away mode
    #[command(subcommand)]
    Away(approval::Away),
    /// Memory notes and proposals
    #[command(subcommand)]
    Mem(mem::MemCmd),
    /// Retros: one per closed card, full ones run by the curator
    #[command(subcommand)]
    Retro(learn::RetroCmd),
    /// The learning loop: change sets, the holdout gate, status
    #[command(subcommand)]
    Learn(learn::LearnCmd),
    /// Start a project's orchestrator
    #[command(subcommand)]
    Orchestrator(later::OrchestratorCmd),
    /// Start a run on a card
    Spawn(spawn::Spawn),
    /// Report a worker's status line
    Status(run::Status),
    /// Runs
    #[command(subcommand)]
    Run(run::RunCmd),
    /// Pull requests
    #[command(subcommand)]
    Pr(run::Pr_),
    /// End a run
    Done(run::Done_),
    /// Control an agent's session
    #[command(subcommand)]
    Agent(later::AgentCmd),
    /// Send a steer or a note to a session
    Send(run::Send),
    /// Read and acknowledge messages from a wake-up line
    Inbox(run::Inbox),
    /// Obligations
    #[command(subcommand)]
    Obligation(records::ObligationCmd),
    /// Artifacts
    #[command(subcommand)]
    Artifact(records::ArtifactCmd),
    /// Reviews
    #[command(subcommand)]
    Review(review::ReviewCmd),
    /// Handoffs
    #[command(subcommand)]
    Handoff(records::HandoffCmd),
    /// Projects
    #[command(subcommand)]
    Project(admin::ProjectCmd),
    /// This host
    #[command(subcommand)]
    Host(admin::HostCmd),
    /// Configuration
    #[command(subcommand)]
    Config(admin::ConfigCmd),
    /// Other hosts running their own Tiller
    #[command(subcommand)]
    Peer(peer::PeerCmd),
    /// Which system owns this host
    #[command(subcommand)]
    Owner(admin::OwnerCmd),
    /// The boot snapshot
    Snapshot,
    /// The board: every open card by column
    Board(read::BoardArgs),
    /// Orchestrators with their workers
    Agents(read::AgentsArgs),
    /// Hosts, their health and free slots
    Machines(read::MachinesArgs),
    /// Quota windows, the dot and whether to start work
    Quota(read::QuotaArgs),
    /// The event log
    Events(read::EventsArgs),
    /// The version and schema
    Version,
    /// Take a verified backup of state.db now
    Backup,
    /// Restore state.db from a verified backup, or rebuild it from an export
    Restore(backup::RestoreArgs),
    /// Export every table as JSONL
    Export(backup::ExportArgs),
    /// Migrate the schema up, or roll it back
    #[command(subcommand)]
    Migrate(backup::MigrateCmd),
    /// Import a Firstmate export bundle (tiller-import/v1)
    Import(import::ImportArgs),
    /// Health checks
    Doctor,
    /// The Health numbers, for the T7.4 report
    #[command(subcommand)]
    Health(health::Cmd),
    /// Serve the board on localhost
    Daemon(daemon::Args),
    /// Record a Claude Code hook event (reads the hook input on stdin)
    Hook(hook::Args),
    /// The PreToolUse backstop for fleet sessions
    #[command(subcommand)]
    Guard(guard::Cmd),
}

#[derive(Subcommand)]
enum IntakeCmd {
    /// Stops, would-block stops and the false-block rate over a window
    Stats(read::IntakeStatsArgs),
}

fn dispatch(app: &App, command: Command) -> Done {
    match command {
        Command::Init(args) => admin::init(app, args),
        Command::Ask(cmd) => ask::run(app, cmd),
        Command::Intake(IntakeCmd::Stats(args)) => read::intake(app, args),
        Command::Card(cmd) => card::run(app, cmd),
        Command::Bug(cmd) => bug::run(app, cmd),
        Command::Ahoy(args) => ahoy::run(app, args),
        Command::Attention(cmd) => attention::run(app, cmd),
        Command::Approval(cmd) => approval::run_request(app, cmd),
        Command::Approve(args) => approval::approve(app, args),
        Command::Land(args) => approval::land(app, args),
        Command::Grant(cmd) => approval::run_grant(app, cmd),
        Command::Away(cmd) => approval::run_away(app, cmd),
        Command::Mem(cmd) => mem::run(app, cmd),
        Command::Retro(cmd) => learn::retro(app, cmd),
        Command::Learn(cmd) => learn::learn(app, cmd),
        Command::Orchestrator(cmd) => later::orchestrator(app, cmd),
        Command::Spawn(args) => spawn::spawn(app, args),
        Command::Status(args) => run::status(app, args),
        Command::Run(cmd) => run::run_cmd(app, cmd),
        Command::Pr(cmd) => run::pr_cmd(app, cmd),
        Command::Done(args) => run::done(app, args),
        Command::Agent(cmd) => later::agent(app, cmd),
        Command::Send(args) => run::send(app, args),
        Command::Inbox(args) => run::inbox(app, args),
        Command::Obligation(cmd) => records::obligation(app, cmd),
        Command::Artifact(cmd) => records::artifact(app, cmd),
        Command::Review(cmd) => review::review(app, cmd),
        Command::Handoff(cmd) => records::handoff(app, cmd),
        Command::Project(cmd) => admin::project(app, cmd),
        Command::Host(cmd) => admin::host(app, cmd),
        Command::Config(cmd) => admin::config(app, cmd),
        Command::Peer(cmd) => peer::run(app, cmd),
        Command::Owner(cmd) => admin::owner(app, cmd),
        Command::Snapshot => read::snapshot(app),
        Command::Board(args) => read::board(app, args),
        Command::Agents(args) => read::agents(app, args),
        Command::Machines(args) => read::machines(app, args),
        Command::Quota(args) => read::quota(app, args),
        Command::Events(args) => read::events(app, args),
        Command::Version => Ok(read::version()),
        Command::Backup => backup::backup(app),
        Command::Restore(args) => backup::restore(app, args),
        Command::Export(args) => backup::export(app, args),
        Command::Migrate(cmd) => backup::migrate(app, cmd),
        Command::Import(args) => import::import(app, args),
        Command::Doctor => doctor::run(app),
        Command::Health(cmd) => health::run(app, cmd),
        Command::Daemon(_) | Command::Hook(_) | Command::Guard(_) => Err(Failure::new(
            "internal",
            1,
            "daemon, hook and guard are run before dispatch",
        )),
    }
}

/// Prints the response to stdout: TOON for agents, JSON on request. Nothing
/// else goes to stdout (`protocol.md` 6.1).
fn emit(mut response: Value, json: bool) {
    let text = if json {
        response.to_string()
    } else {
        lisbon::localize(&mut response);
        toon::encode_response(&response).trim_end().to_owned()
    };
    println!("{text}");
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                error.exit();
            }
            // Claude Code reads exit 2 from a hook as a deny, so a broken
            // hook or guard command line must not block every tool call: it
            // allows, as a broken hook does, and the deny rules and the
            // sandbox still hold (`authority.md` section 9).
            if matches!(std::env::args().nth(1).as_deref(), Some("hook" | "guard")) {
                return ExitCode::SUCCESS;
            }
            let json = std::env::args().any(|arg| arg == "--json");
            let rendered = error.render().to_string();
            eprint!("{rendered}");
            let message = rendered
                .lines()
                .next()
                .unwrap_or("bad arguments")
                .trim_start_matches("error: ")
                .to_owned();
            let failure = Failure::usage(message).hint("tiller --help");
            emit(failure.into_response(), json);
            return ExitCode::from(2);
        }
    };
    // A hook never prints and always exits 0, so it skips the response path.
    if let Command::Hook(args) = &cli.command {
        hook::run(args);
        return ExitCode::SUCCESS;
    }
    // The guard answers in the hook transport, never the response path.
    if let Command::Guard(cmd) = &cli.command {
        return guard::run(cmd);
    }
    // `peer serve` owns stdout once it starts: the link's frames go there.
    if let Command::Peer(peer::PeerCmd::Serve { from }) = &cli.command {
        let app = App::from_env(cli.db.clone());
        return match peer::serve(&app, from.clone()) {
            Ok(code) => code,
            Err(failure) => {
                let exit = failure.exit;
                emit(failure.into_response(), cli.json);
                ExitCode::from(exit)
            }
        };
    }
    if let Command::Peer(peer::PeerCmd::Pane { from }) = &cli.command {
        let app = App::from_env(cli.db.clone());
        return match peer::pane(&app, from.clone()) {
            Ok(code) => code,
            Err(failure) => {
                let exit = failure.exit;
                emit(failure.into_response(), cli.json);
                ExitCode::from(exit)
            }
        };
    }
    if let Command::Daemon(args) = &cli.command {
        return match daemon::run(args) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("tiller daemon: {error:#}");
                ExitCode::from(1)
            }
        };
    }
    let json = cli.json;
    let app = App::from_env(cli.db);
    match dispatch(&app, cli.command) {
        Ok(outcome) => {
            emit(outcome.into_response(), json);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let exit = failure.exit;
            emit(failure.into_response(), json);
            ExitCode::from(exit)
        }
    }
}
