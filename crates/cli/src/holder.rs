//! `tiller holder`: runs the terminal holder for this home
//! (`terminal-holder.md` 2). It keeps the agents' PTYs and serves them on
//! `$TILLER_HOME/run/holder.sock`. One runs per home: a second one finds the
//! first and exits 0.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Subcommand, ValueHint};
use serde_json::json;
use tiller_holder::Outcome;
use tiller_holder::detach::{Detached, Started};
use tiller_panes::message::ErrorCode;
use tiller_panes::{HolderClient, PanesError, Upgrade, socket_path};

use crate::app::{Failure, Outcome as Reply};

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Args {
    /// Start the holder in the background, so it outlives this terminal. It
    /// waits until the holder answers, or says it already runs.
    #[arg(long)]
    detach: bool,
    /// The live upgrade's preflight: print what this binary takes over
    #[arg(long, hide = true, conflicts_with_all = ["detach", "dry_run_handover", "resume"])]
    check_handover: bool,
    /// The live upgrade's dry run: read a handover record on stdin back
    #[arg(long, hide = true, conflicts_with_all = ["detach", "resume"])]
    dry_run_handover: bool,
    /// The live upgrade's new image: take over from the record behind fd
    #[arg(long, hide = true, value_name = "FD", conflicts_with = "detach")]
    resume: Option<i32>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Ask the running holder to re-execute from a new binary, keeping its
    /// panes
    Upgrade {
        /// Check the upgrade and answer, without re-executing
        #[arg(long)]
        dry_run: bool,
        /// The binary to re-execute from (default: this one, resolved)
        #[arg(long, value_hint = ValueHint::FilePath)]
        binary: Option<PathBuf>,
    },
}

/// `db` is the `--db` or `TILLER_DB` given, if any.
pub fn run(args: &Args, db: Option<&Path>, json: bool) -> ExitCode {
    // The live upgrade's two checks print JSON for the old holder to read,
    // whatever the home.
    if args.check_handover {
        return print_json(&tiller_holder::upgrade::Accepts::this_build());
    }
    if args.dry_run_handover {
        return dry_run_handover();
    }
    let home = crate::app::App::from_env(None).home;
    // The holder keeps no database: a `--db` other than the home's own
    // would suggest a home it does not serve.
    if db.is_some_and(|db| !crate::daemon::is_home_db(db, &home)) {
        eprintln!(
            "tiller holder: --db and TILLER_DB do not apply to the holder: it serves the home {}. Set TILLER_HOME to choose another.",
            home.display()
        );
        return ExitCode::from(2);
    }
    if let Some(Cmd::Upgrade { dry_run, binary }) = &args.cmd {
        return upgrade(&home, *dry_run, binary.as_deref(), json);
    }
    if args.detach {
        // Before any thread or log: the holder forks from here.
        match tiller_holder::detach::detach(&home) {
            Ok(Detached::Holder) => {}
            Ok(Detached::Caller(Started::Started { pid })) => {
                match pid {
                    Some(pid) => eprintln!("tiller holder: started (pid {pid})"),
                    None => eprintln!("tiller holder: started"),
                }
                return ExitCode::SUCCESS;
            }
            Ok(Detached::Caller(Started::AlreadyRuns { pid })) => {
                already_runs(pid);
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                eprintln!("tiller holder: {error}");
                return ExitCode::from(1);
            }
        }
    }
    // Logs go to stderr, which launchd and systemd keep.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TILLER_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
    let ran = match args.resume {
        Some(fd) => tiller_holder::resume(&home, fd),
        None => tiller_holder::run(&home),
    };
    match ran {
        Ok(Outcome::Stopped) => ExitCode::SUCCESS,
        Ok(Outcome::AlreadyRuns { pid }) => {
            already_runs(pid);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tiller holder: {error}");
            ExitCode::from(1)
        }
    }
}

fn print_json(value: &impl serde::Serialize) -> ExitCode {
    match serde_json::to_string(value) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tiller holder: {error}");
            ExitCode::from(1)
        }
    }
}

/// Reads a handover record on stdin and prints the checksum of it read
/// back, or exits 1 with why it does not read.
fn dry_run_handover() -> ExitCode {
    let mut bytes = Vec::new();
    let limit = tiller_holder::handover::RECORD_MAX as u64 + 1;
    if let Err(error) = std::io::stdin().lock().take(limit).read_to_end(&mut bytes) {
        eprintln!("tiller holder: the handover record could not be read: {error}");
        return ExitCode::from(1);
    }
    if bytes.len() > tiller_holder::handover::RECORD_MAX {
        eprintln!("tiller holder: the handover record is over its limit");
        return ExitCode::from(1);
    }
    match tiller_holder::upgrade::dry_run(&bytes) {
        Ok(read) => print_json(&read),
        Err(error) => {
            eprintln!("tiller holder: the handover record does not read back: {error}");
            ExitCode::from(1)
        }
    }
}

fn already_runs(pid: Option<u32>) {
    match pid {
        Some(pid) => eprintln!("tiller holder: a holder already runs (pid {pid})"),
        None => eprintln!("tiller holder: a holder already runs (pid unknown)"),
    }
}

/// `tiller holder upgrade` (`terminal-holder.md` 6.2): the same request the
/// daemon sends when the holder runs another binary than the profile's.
fn upgrade(home: &Path, dry_run: bool, binary: Option<&Path>, json: bool) -> ExitCode {
    match ask_upgrade(home, dry_run, binary) {
        Ok(reply) => {
            crate::emit(reply.into_response(), json);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let exit = failure.exit;
            crate::emit(failure.into_response(), json);
            ExitCode::from(exit)
        }
    }
}

fn ask_upgrade(home: &Path, dry_run: bool, binary: Option<&Path>) -> Result<Reply, Failure> {
    let unusable = |error: std::io::Error| {
        Failure::refused(format!("the binary to upgrade to is unusable: {error}"))
    };
    let binary = match binary {
        Some(binary) => binary.to_owned(),
        None => std::env::current_exe().map_err(unusable)?,
    }
    .canonicalize()
    .map_err(unusable)?;
    let binary = binary.to_str().ok_or_else(|| {
        Failure::refused(format!("the binary path {} is not UTF-8", binary.display()))
    })?;
    let down = |error: &PanesError| {
        Failure::unavailable(format!("no holder answers for this home: {error}"))
            .hint("start one with: tiller holder --detach")
    };
    let client = HolderClient::connect(socket_path(home), "cli").map_err(|error| down(&error))?;
    match client.upgrade(binary, dry_run) {
        Ok(Upgrade::Upgrading) => Ok(Reply::read(json!({
            "binary": binary,
            "dry_run": dry_run,
            "upgrade": if dry_run { "ready" } else { "started" },
            "holder": client.welcome().holder,
            "instance": client.welcome().instance,
        }))),
        Ok(Upgrade::Refused(reason)) => Err(Failure::new(
            "upgrade_refused",
            5,
            format!("the holder refused the upgrade and keeps running as it is: {reason}"),
        )),
        Err(PanesError::Refused {
            code: ErrorCode::Unsupported,
            ..
        }) => Err(Failure::new(
            "unsupported",
            5,
            format!(
                "this holder (version {}) does not support upgrade; it keeps running as it is",
                client.welcome().holder
            ),
        )
        .hint("a holder built with the live upgrade accepts this command")),
        Err(error @ PanesError::Unreachable(_)) => Err(down(&error)),
        Err(error) => Err(Failure::unavailable(format!("the upgrade failed: {error}"))),
    }
}
