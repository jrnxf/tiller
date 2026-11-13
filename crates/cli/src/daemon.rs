//! `tiller daemon`: runs `tiller-daemon` with this host's tools and its
//! terminal holder. The helper programs can be replaced for tests and labs:
//! `TILLER_QUOTA_AXI` and `TILLER_SSH` each hold a command
//! line (a program and its leading arguments, split on whitespace), and
//! `TILLER_GH` names the GitHub CLI program the land loop and the direct-pr
//! publish run.
//! `[attach] terminal` names the terminal app the board's "Open in terminal"
//! brings to the front (`protocol.md` 7.4); it is a macOS app, so other
//! systems ignore it. `[herdr] terminal` still works when `[attach]` sets
//! none, and the start logs a warning for it and every other herdr key
//! (`terminal-holder.md` 11.1).

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde::Deserialize;
use tiller_daemon::assets::Assets;
use tiller_daemon::{Config, Ended};
use tiller_panes::open::{MacApp, Terminal};

use crate::cmd::admin::load_config;
use crate::cmd::spawn::{BoardStart, lavish_from_config};

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

/// The terminal keys of the `[attach]` table, or of the `[herdr]` one
/// they replace.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TerminalKeys {
    /// The macOS app the pane opens in, such as `Ghostty`. Unset, "Open in
    /// terminal" only focuses the pane.
    terminal: Option<String>,
    /// What the app takes before a command to run; `["-e"]` when unset.
    terminal_exec: Option<Vec<String>>,
}

/// The terminal keys of table `name`; `[attach]` keeps `prefix` beside
/// them, which `tiller attach` reads.
fn terminal_keys(table: &toml::Table, name: &str) -> anyhow::Result<TerminalKeys> {
    let Some(value) = table.get(name) else {
        return Ok(TerminalKeys::default());
    };
    let mut value = value.clone();
    if name == "attach"
        && let Some(keys) = value.as_table_mut()
    {
        keys.remove("prefix");
    }
    value
        .try_into()
        .map_err(|e: toml::de::Error| anyhow::anyhow!("config.toml [{name}]: {e}"))
}

/// The terminal app from the `[attach]` table, else from the `[herdr]` one,
/// or none.
fn configured_terminal(table: &toml::Table) -> anyhow::Result<Option<MacApp>> {
    let attach = terminal_keys(table, "attach")?;
    let herdr = terminal_keys(table, "herdr")?;
    let (keys, name) = if attach.terminal.is_some() {
        (attach, "attach")
    } else {
        (herdr, "herdr")
    };
    let Some(app) = keys.terminal else {
        return Ok(None);
    };
    if app.trim().is_empty() {
        anyhow::bail!("config.toml [{name}]: terminal names an app");
    }
    let terminal = MacApp::new(app);
    Ok(Some(match keys.terminal_exec {
        Some(exec) => terminal.exec(exec.into_iter().map(OsString::from).collect()),
        None => terminal,
    }))
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
    if is_home_db(db, &home) {
        return None;
    }
    Some(format!(
        "--db and TILLER_DB do not apply to the daemon: it opens {} and keeps its lock, backups and mirror in {}. Set TILLER_HOME to the directory that holds the database to serve.",
        home.join("state.db").display(),
        home.display()
    ))
}

/// Whether `db` names `home`'s own `state.db`.
pub fn is_home_db(db: &Path, home: &Path) -> bool {
    same_path(db, &home.join("state.db"))
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
    for key in crate::cmd::doctor::herdr_keys(&table) {
        tracing::warn!(
            key = key.key,
            "config.toml names a herdr key; the terminal holder replaces herdr: {}",
            key.instead
        );
    }
    let mut terminal = configured_terminal(&table)?;
    if !cfg!(target_os = "macos")
        && let Some(ignored) = terminal.take()
    {
        tracing::warn!(
            terminal = ignored.name(),
            "config.toml [attach] terminal is a macOS app; Open in terminal only focuses here"
        );
    }
    let (lavish, review_hosts) = lavish_from_config(&table).map_err(failed)?;
    let holder = Arc::new(tiller_daemon::holder::HolderLink::new(
        &std::path::absolute(&app.home)?,
    ));
    let config = Config {
        home: app.home.clone(),
        port: args.port,
        allowed_hosts: args.allow_hosts.clone(),
        terminal: terminal.map(|app| Arc::new(app) as _),
        quota: command_line("TILLER_QUOTA_AXI", "quota-axi"),
        lavish,
        review_hosts,
        ssh: command_line("TILLER_SSH", "ssh"),
        gh: std::env::var("TILLER_GH").unwrap_or_else(|_| "gh".into()),
        assets: Assets::board(),
        holder: Arc::clone(&holder) as _,
        // The holder re-execs into the binary this daemon was started by
        // (`terminal-holder.md` 6.2).
        holder_source: tiller_daemon::workers::HolderWatch {
            link: holder,
            stable: crate::stable::tiller_path().ok(),
        },
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
    fn no_terminal_key_means_focus_only() {
        assert!(configured_terminal(&table("")).unwrap().is_none());
        assert!(configured_terminal(&table("[herdr]")).unwrap().is_none());
        assert!(
            configured_terminal(&table("[attach]\nprefix = \"ctrl-a\""))
                .unwrap()
                .is_none()
        );
    }

    /// `[attach] terminal` wins over the `[herdr]` one it replaces, which
    /// still works alone, and `prefix` sits beside it.
    #[test]
    fn the_attach_terminal_wins_over_the_retired_one() {
        let app = configured_terminal(&table(
            "[attach]\nterminal = \"WezTerm\"\nprefix = \"ctrl-a\"\n[herdr]\nterminal = \"Ghostty\"",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(app.name(), "WezTerm");
        let error = configured_terminal(&table("[attach]\nterminl = \"Ghostty\""))
            .unwrap_err()
            .to_string();
        assert!(error.contains("[attach]"), "{error}");
        assert!(configured_terminal(&table("[attach]\nterminal = \"\"")).is_err());
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
