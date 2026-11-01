mod daemon;

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
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Daemon(args) => daemon::run(&args),
    }
}
