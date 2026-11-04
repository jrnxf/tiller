//! `tiller daemon`: runs `tiller-daemon` with this host's tools. Both helper
//! programs can be replaced for tests and labs: `TILLER_HERDR` and
//! `TILLER_QUOTA_AXI` each hold a command line (a program and its leading
//! arguments, split on whitespace).

use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

use tiller_daemon::assets::Assets;
use tiller_daemon::{Config, Ended};
use tiller_herdr::client::Cli;

#[derive(clap::Args)]
pub struct Args {
    /// Port to listen on, 0 picks a free one. The daemon only binds localhost.
    #[arg(long, default_value_t = tiller_daemon::PORT)]
    port: u16,
    /// Also serve reads to requests with this `Host` header, such as a
    /// reverse proxy's name; repeatable. Mutations stay on 127.0.0.1.
    #[arg(long = "allow-host", value_name = "HOST")]
    allow_hosts: Vec<String>,
}

pub fn command_line(variable: &str, default: &str) -> Vec<OsString> {
    let line = std::env::var(variable)
        .ok()
        .filter(|line| !line.trim().is_empty())
        .unwrap_or_else(|| default.to_owned());
    line.split_whitespace().map(OsString::from).collect()
}

/// The exit code for a database newer than this binary (`recovery.md` R11).
const NEWER_SCHEMA: u8 = 3;

pub fn run(args: &Args) -> anyhow::Result<ExitCode> {
    // Logs go to stderr, which launchd and systemd keep.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TILLER_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        // A service manager's log file gets plain text.
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
    let home = crate::app::App::from_env(None).home;
    let config = Config {
        home,
        port: args.port,
        allowed_hosts: args.allow_hosts.clone(),
        herdr: Cli::local().program(command_line("TILLER_HERDR", "herdr")),
        quota: command_line("TILLER_QUOTA_AXI", "quota-axi"),
        ssh: command_line("TILLER_SSH", "ssh"),
        assets: Assets::board(),
    };
    match tiller_daemon::run(config)? {
        Ended::Stopped => Ok(ExitCode::SUCCESS),
        Ended::AlreadyRunning { pid } => {
            match pid {
                Some(pid) => println!("tiller daemon already runs as pid {pid}"),
                None => println!("tiller daemon already runs"),
            }
            Ok(ExitCode::SUCCESS)
        }
        Ended::WorkersFailed => {
            eprintln!("tiller daemon: the workers failed to start; see the log above");
            Ok(ExitCode::FAILURE)
        }
        Ended::NewerSchema { found } => {
            eprintln!(
                "tiller daemon: state.db has schema {found}, newer than this binary; exiting for the service manager to restart the new one"
            );
            Ok(ExitCode::from(NEWER_SCHEMA))
        }
    }
}
