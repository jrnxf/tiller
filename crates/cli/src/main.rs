mod app;
mod cmd;
mod daemon;
mod hook;
mod lisbon;
mod toon;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{Parser, Subcommand};
use serde_json::Value;

use app::{App, Done, Failure};
use cmd::{admin, approval, ask, attention, backup, card, import, later, read, records, run};

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
    Mem(later::MemCmd),
    /// Start a project's orchestrator
    #[command(subcommand)]
    Orchestrator(later::OrchestratorCmd),
    /// Start a run on a card
    Spawn(later::Spawn),
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
    Review(records::ReviewCmd),
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
    Peer(admin::PeerCmd),
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
    /// Quota windows and the dot
    Quota,
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
    /// Serve the board on localhost
    Daemon(daemon::Args),
    /// Record a Claude Code hook event (reads the hook input on stdin)
    Hook(hook::Args),
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
        Command::Attention(cmd) => attention::run(app, cmd),
        Command::Approval(cmd) => approval::run_request(app, cmd),
        Command::Approve(args) => approval::approve(app, args),
        Command::Land(args) => approval::land(app, args),
        Command::Grant(cmd) => approval::run_grant(app, cmd),
        Command::Away(cmd) => approval::run_away(app, cmd),
        Command::Mem(cmd) => later::mem(app, cmd),
        Command::Orchestrator(cmd) => later::orchestrator(app, cmd),
        Command::Spawn(args) => later::spawn(app, args),
        Command::Status(args) => run::status(app, args),
        Command::Run(cmd) => run::run_cmd(app, cmd),
        Command::Pr(cmd) => run::pr_cmd(app, cmd),
        Command::Done(args) => run::done(app, args),
        Command::Agent(cmd) => later::agent(app, cmd),
        Command::Send(args) => run::send(app, args),
        Command::Inbox(args) => run::inbox(app, args),
        Command::Obligation(cmd) => records::obligation(app, cmd),
        Command::Artifact(cmd) => records::artifact(app, cmd),
        Command::Review(cmd) => records::review(app, cmd),
        Command::Handoff(cmd) => records::handoff(app, cmd),
        Command::Project(cmd) => admin::project(app, cmd),
        Command::Host(cmd) => admin::host(app, cmd),
        Command::Config(cmd) => admin::config(app, cmd),
        Command::Peer(cmd) => admin::peer(app, cmd),
        Command::Owner(cmd) => admin::owner(app, cmd),
        Command::Snapshot => read::snapshot(app),
        Command::Board(args) => read::board(app, args),
        Command::Agents(args) => read::agents(app, args),
        Command::Machines(args) => read::machines(app, args),
        Command::Quota => read::quota(app),
        Command::Events(args) => read::events(app, args),
        Command::Version => Ok(read::version()),
        Command::Backup => backup::backup(app),
        Command::Restore(args) => backup::restore(app, args),
        Command::Export(args) => backup::export(app, args),
        Command::Migrate(cmd) => backup::migrate(app, cmd),
        Command::Import(args) => import::import(app, args),
        Command::Doctor => later::doctor(app),
        Command::Daemon(_) | Command::Hook(_) => Err(Failure::new(
            "internal",
            1,
            "daemon and hook are run before dispatch",
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
    if let Command::Daemon(args) = &cli.command {
        return match daemon::run(args) {
            Ok(()) => ExitCode::SUCCESS,
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
