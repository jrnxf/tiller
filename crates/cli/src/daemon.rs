//! `tiller daemon`: runs `tiller-daemon` with this host's tools. herdr is the
//! one `tiller spawn` drives: `spawn.herdr` and `spawn.herdr_socket` from
//! `config.toml`, read again whenever that file changes. The helper programs can be replaced for tests and labs:
//! `TILLER_HERDR`, `TILLER_QUOTA_AXI` and `TILLER_SSH` each hold a command
//! line (a program and its leading arguments, split on whitespace), and
//! `TILLER_GH` names the GitHub CLI program the land loop and the direct-pr
//! publish run.
//! `[herdr] terminal` names the terminal app the board's "Open in herdr"
//! brings to the front (`protocol.md` 7.4); it is a macOS app, so other
//! systems ignore it.

use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;

use serde::Deserialize;
use tiller_daemon::assets::Assets;
use tiller_daemon::{Config, Ended};
use tiller_herdr::client::Cli;
use tiller_herdr::front::{MacApp, Terminal};

use crate::cmd::admin::{load_config, load_config_at};
use crate::cmd::spawn::{herdr_from_config, lavish_from_config};

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

/// The command line in `variable`, when it is set and not blank.
pub fn env_command(variable: &str) -> Option<Vec<OsString>> {
    let line = std::env::var(variable).ok()?;
    let words: Vec<OsString> = line.split_whitespace().map(OsString::from).collect();
    (!words.is_empty()).then_some(words)
}

pub fn command_line(variable: &str, default: &str) -> Vec<OsString> {
    env_command(variable).unwrap_or_else(|| vec![default.into()])
}

/// The `[herdr]` table of `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HerdrTable {
    /// The macOS app herdr runs in, such as `Ghostty`. Unset, "Open in
    /// herdr" only focuses the pane.
    terminal: Option<String>,
    /// What the app takes before a command to run; `["-e"]` when unset.
    terminal_exec: Option<Vec<String>>,
}

/// The terminal app from the `[herdr]` table, or none.
fn configured_terminal(table: &toml::Table) -> anyhow::Result<Option<MacApp>> {
    let herdr: HerdrTable = match table.get("herdr") {
        Some(value) => value
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| anyhow::anyhow!("config.toml [herdr]: {e}"))?,
        None => HerdrTable::default(),
    };
    let Some(app) = herdr.terminal else {
        return Ok(None);
    };
    if app.trim().is_empty() {
        anyhow::bail!("config.toml [herdr]: terminal names an app");
    }
    let terminal = MacApp::new(app);
    Ok(Some(match herdr.terminal_exec {
        Some(exec) => terminal.exec(exec.into_iter().map(OsString::from).collect()),
        None => terminal,
    }))
}

/// `initial` made to follow `config.toml`: every herdr call names the
/// `[spawn]` herdr as the file reads now, so editing `spawn.herdr`,
/// `spawn.herdr_session` or `spawn.herdr_socket` needs no daemon restart. A
/// file that no longer parses keeps the last herdr that did (one warning per
/// edit): the start refuses a bad table, a running daemon must not go blind
/// on a half-written one. `TILLER_HERDR` still replaces the table.
fn follow_config(initial: Cli, config: std::path::PathBuf) -> Cli {
    type Stamp = Option<(std::time::SystemTime, u64)>;
    let stamp = |path: &std::path::Path| -> Stamp {
        let meta = std::fs::metadata(path).ok()?;
        Some((meta.modified().ok()?, meta.len()))
    };
    let seen = std::sync::Mutex::new((stamp(&config), initial.clone()));
    initial.follow(move || {
        let mut seen = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = stamp(&config);
        if now != seen.0 {
            seen.0 = now;
            match load_config_at(&config).and_then(|table| herdr_from_config(&table)) {
                Ok(herdr) => {
                    tracing::info!("config.toml changed; driving the herdr it names now");
                    seen.1 = herdr;
                }
                Err(failure) => tracing::warn!(
                    "config.toml [spawn] cannot be read ({}); keeping the herdr in use",
                    failure.message
                ),
            }
        }
        seen.1.clone()
    })
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
    let app = crate::app::App::from_env(None);
    // A [spawn] table that does not parse stops the start: guessing the bare
    // default could drive another session's server.
    let failed = |failure: crate::app::Failure| match &failure.hint {
        Some(hint) => anyhow::anyhow!("{}; {hint}", failure.message),
        None => anyhow::anyhow!("{}", failure.message),
    };
    let table = load_config(&app).map_err(failed)?;
    let herdr = herdr_from_config(&table).map_err(failed)?;
    let herdr = follow_config(herdr, crate::cmd::admin::config_file(&app));
    let mut terminal = configured_terminal(&table)?;
    if !cfg!(target_os = "macos")
        && let Some(ignored) = terminal.take()
    {
        tracing::warn!(
            terminal = ignored.name(),
            "config.toml [herdr] terminal is a macOS app; Open in herdr only focuses here"
        );
    }
    let (lavish, review_hosts) = lavish_from_config(&table).map_err(failed)?;
    let config = Config {
        home: app.home,
        port: args.port,
        allowed_hosts: args.allow_hosts.clone(),
        herdr,
        terminal: terminal.map(|app| Arc::new(app) as _),
        quota: command_line("TILLER_QUOTA_AXI", "quota-axi"),
        lavish,
        review_hosts,
        ssh: command_line("TILLER_SSH", "ssh"),
        gh: std::env::var("TILLER_GH").unwrap_or_else(|_| "gh".into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tiller_herdr::front::Terminal;

    fn table(text: &str) -> toml::Table {
        text.parse().unwrap()
    }

    #[test]
    fn no_herdr_table_means_focus_only() {
        assert!(configured_terminal(&table("")).unwrap().is_none());
        assert!(configured_terminal(&table("[herdr]")).unwrap().is_none());
    }

    #[test]
    fn terminal_names_the_app_and_refuses_unknown_keys() {
        let app = configured_terminal(&table("[herdr]\nterminal = \"Ghostty\""))
            .unwrap()
            .unwrap();
        assert_eq!(app.name(), "Ghostty");
        let app = configured_terminal(&table(
            "[herdr]\nterminal = \"WezTerm\"\nterminal_exec = [\"start\", \"--\"]",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(app.name(), "WezTerm");
        let error = configured_terminal(&table("[herdr]\nterminl = \"Ghostty\""))
            .unwrap_err()
            .to_string();
        assert!(error.contains("[herdr]"), "{error}");
        assert!(configured_terminal(&table("[herdr]\nterminal = \" \"")).is_err());
    }
}
