//! `tiller holder`: runs the terminal holder for this home
//! (`terminal-holder.md` 2). It keeps the agents' PTYs and serves them on
//! `$TILLER_HOME/run/holder.sock`. One runs per home: a second one finds the
//! first and exits 0.

use std::io::IsTerminal;
use std::process::ExitCode;

use tiller_holder::Outcome;

pub fn run() -> ExitCode {
    // Logs go to stderr, which launchd and systemd keep.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TILLER_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
    let home = crate::app::App::from_env(None).home;
    match tiller_holder::run(&home) {
        Ok(Outcome::Stopped) => ExitCode::SUCCESS,
        Ok(Outcome::AlreadyRuns { pid }) => {
            match pid {
                Some(pid) => eprintln!("tiller holder: a holder already runs (pid {pid})"),
                None => eprintln!("tiller holder: a holder already runs (pid unknown)"),
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tiller holder: {error}");
            ExitCode::from(1)
        }
    }
}
