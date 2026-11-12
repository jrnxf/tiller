//! `tiller holder`: runs the terminal holder for this home
//! (`terminal-holder.md` 2). It keeps the agents' PTYs and serves them on
//! `$TILLER_HOME/run/holder.sock`. One runs per home: a second one finds the
//! first and exits 0.

use std::io::IsTerminal;
use std::path::Path;
use std::process::ExitCode;

use tiller_holder::Outcome;

/// `db` is the `--db` or `TILLER_DB` given, if any.
pub fn run(db: Option<&Path>) -> ExitCode {
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
    // Logs go to stderr, which launchd and systemd keep.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TILLER_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init();
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
