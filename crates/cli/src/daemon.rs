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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde::Deserialize;
use tiller_daemon::assets::Assets;
use tiller_daemon::{Config, Ended};
use tiller_herdr::client::Cli;
use tiller_herdr::front::{MacApp, Terminal};

use crate::cmd::admin::{load_config, load_config_at};
use crate::cmd::spawn::{BoardStart, herdr_from_config, lavish_from_config};

#[derive(clap::Args)]
pub struct Args {
    /// Port to listen on, 0 picks a free one. The daemon only binds localhost.
    #[arg(long, default_value_t = tiller_daemon::PORT)]
    port: u16,
    /// Also serve the board to requests with exactly this `Host` header, the
    /// name of a reverse proxy that serves it over https; repeatable. There
    /// the board shows only to a browser signed in with the login code or
    /// link, and mutations come only from its https origin. The proxy should
    /// append the browser's address to X-Forwarded-For, which counts sign-in
    /// code tries per browser.
    #[arg(long = "allow-host", value_name = "HOST", value_parser = tiller_daemon::allowed_host)]
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

/// Why this `--db` (or `TILLER_DB`) cannot go to the daemon, or `None` when
/// there is none or it names the database the daemon opens anyway.
///
/// The daemon takes every path from `TILLER_HOME`: `state.db`, its lock and
/// `daemon.json`, the backups, the mirror and the peer keys. Honoring `--db`
/// alone would split one daemon across two places, so a database anywhere but
/// `<home>/state.db` is refused and the message names `TILLER_HOME`.
pub fn refuse_db(db: Option<&Path>) -> Option<String> {
    let db = db?;
    let home = crate::app::App::from_env(None).home;
    let own = home.join("state.db");
    if same_path(db, &own) {
        return None;
    }
    Some(format!(
        "--db and TILLER_DB do not apply to the daemon: it opens {} and keeps its lock, backups and mirror in {}. Set TILLER_HOME to the directory that holds the database to serve.",
        own.display(),
        home.display()
    ))
}

/// Whether two paths name one file, even when it does not exist yet.
fn same_path(a: &Path, b: &Path) -> bool {
    let resolved = |path: &Path| -> PathBuf {
        // A bare name's parent is "": the working directory.
        let parent = path.parent().map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        });
        match (parent.map(Path::canonicalize), path.file_name()) {
            (Some(Ok(parent)), Some(name)) => parent.join(name),
            _ => path.to_path_buf(),
        }
    };
    a == b || resolved(a) == resolved(b)
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
        home: app.home.clone(),
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
        holder: Some(Arc::new(tiller_daemon::holder::HolderLink::new(
            &std::path::absolute(&app.home)?,
        ))),
        orchestrators: Some(Arc::new(BoardStart { app })),
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

    /// `cd ~/.tiller && tiller --db state.db daemon` names the home database:
    /// a bare name resolves against the working directory.
    #[test]
    fn a_bare_name_is_the_file_in_the_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        assert!(same_path(Path::new("Cargo.toml"), &cwd.join("Cargo.toml")));
        assert!(same_path(
            Path::new("not-there.db"),
            &cwd.join("not-there.db")
        ));
        assert!(!same_path(
            Path::new("Cargo.toml"),
            &cwd.join("src/Cargo.toml")
        ));
    }

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
