mod daemon;
mod hook;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tiller", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the board on localhost
    Daemon(daemon::Args),
    /// Record a Claude Code hook event (reads the hook input on stdin)
    Hook(hook::Args),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Daemon(args) => daemon::run(&args),
        Command::Hook(args) => {
            hook::run(&args);
            Ok(())
        }
    }
}
