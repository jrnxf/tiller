//! `tiller doctor` (P43, T2.8): the host's health checks, read only. It
//! reports the versions of the tools Tiller drives, whether herdr answers,
//! whether the packaged hook registration is whole, the stored `merge_gate`
//! of every project and whether `config.toml` is newer than its last
//! `config.changed` event, who owns the host (the owner marker and whether a
//! Firstmate is running on it). It never fixes, installs or writes anything: a
//! failed required check makes the command exit 5 with the full report in
//! the error's `current`.
//!
//! The tools are found the way the daemon and spawn find them:
//! `TILLER_HERDR`, `TILLER_QUOTA_AXI` and `TILLER_GH` first, then the
//! `[spawn]` tables of `config.toml`, then the bare program name.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::{Value, json};
use tiller_agents::land::app::{APP_DIR, AppKey, KEY_FILE, KeyError};
use tiller_agents::roles::Role;
use tiller_agents::spawn::config::SpawnConfig;
use tiller_core::Timestamp;
use tiller_herdr::client::{Cli, Herdr};
use tiller_observe::hooks::{EVENTS, Handler};
use tiller_reconcile::tools::{self, PROBE_TIMEOUT, read_version, version_parts};

use super::admin::{config_file, load_config};
use super::spawn::{herdr_program, words};
use crate::app::{App, Done, Failure, Outcome};
use crate::daemon::env_command;

/// The Claude Code version the hook table in `protocol.md` 4.2 was checked
/// against (`host-capabilities.md`). A newer one is a warning: it may add
/// events the table does not map.
const CLAUDE_CHECKED: &str = "2.1.312";

/// The Firstmate scripts whose presence in the process list means a
/// Firstmate is supervising this host: the supervisor daemon and the
/// watcher. The one-shot `fm-watch-arm.sh` is not listed, because it exits
/// as soon as it has armed the watcher.
const FIRSTMATE_SCRIPTS: [&str; 2] = ["fm-supervise-daemon.sh", "fm-watch.sh"];

/// A config file this much newer than its newest event was edited by hand.
const CONFIG_SLACK_MS: i64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
struct Check {
    name: String,
    status: Status,
    /// A failed required check fails the command; a failed optional one is
    /// a warning.
    required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    detail: String,
}

impl Check {
    fn new(name: &str, required: bool, status: Status, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            status,
            required,
            version: None,
            detail: detail.into(),
        }
    }

    fn version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }

    /// A check that did not pass: `fail` when required, else `warn`.
    fn missing(name: &str, required: bool, detail: impl Into<String>) -> Self {
        let status = if required { Status::Fail } else { Status::Warn };
        Self::new(name, required, status, detail)
    }

    fn failed(&self) -> bool {
        self.required && self.status == Status::Fail
    }
}

#[derive(Debug, Serialize)]
struct Gate {
    project: String,
    merge_gate: String,
    status: Status,
    /// The newest `project.merge_gate_changed` event. The hourly ruleset
    /// check only writes when the value moves, so no check time is stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_changed_at: Option<String>,
    detail: String,
}

#[derive(Debug, Serialize)]
struct Report {
    ok: bool,
    checks: Vec<Check>,
    merge_gates: Vec<Gate>,
}

/// One tool that must (or may) answer `--version`.
fn tool(name: &str, required: bool, program: &[OsString]) -> Check {
    match read_version(program) {
        Ok(version) => {
            Check::new(name, required, Status::Ok, "answers --version").version(Some(version))
        }
        Err(error) => Check::missing(name, required, error),
    }
}

/// herdr must answer a snapshot, which also reports the server's version.
fn herdr(program: Vec<OsString>) -> Check {
    match Cli::local()
        .program(program)
        .timeout(PROBE_TIMEOUT)
        .snapshot()
    {
        Ok(snapshot) => Check::new(
            "herdr",
            true,
            Status::Ok,
            format!(
                "reachable, {} workspaces and {} panes",
                snapshot.workspaces.len(),
                snapshot.panes.len()
            ),
        )
        .version(snapshot.version),
        Err(error) => Check::missing("herdr", true, error.to_string()),
    }
}

fn claude(program: &[OsString]) -> Check {
    let mut check = tool("claude", true, program);
    if let Some(version) = &check.version
        && version_parts(version) > version_parts(CLAUDE_CHECKED)
    {
        check.status = Status::Warn;
        check.detail = format!(
            "newer than {CLAUDE_CHECKED}, the version the hook table was checked against; \
             it may add hook events the table does not map"
        );
    }
    check
}

/// The no-mistakes daemon, when the CLI is installed. The CLI's `daemon
/// status` does not report the daemon's version, so it is read from the
/// binary the daemon's pid runs (`tools::running_daemon_version`) and
/// compared with the CLI's: a daemon on another version than its CLI is a
/// warning, the shape of the Firstmate regression where an upgraded CLI kept
/// talking to an old daemon.
fn no_mistakes(cli: &Check) -> Check {
    const NAME: &str = "no-mistakes daemon";
    if cli.status != Status::Ok {
        return Check::new(NAME, false, Status::Warn, "the no-mistakes CLI is missing");
    }
    let running = match tools::run_first_line("no-mistakes".as_ref(), &[], "daemon status") {
        Ok(line) => line.unwrap_or_else(|| "running".into()),
        Err(error) => return Check::missing(NAME, false, error),
    };
    no_mistakes_skew(
        &running,
        cli.version.as_deref(),
        tools::running_daemon_version(),
    )
}

/// The verdict on a running daemon, given the CLI's version line and the
/// daemon's, when it could be read.
fn no_mistakes_skew(running: &str, cli: Option<&str>, daemon: Option<String>) -> Check {
    const NAME: &str = "no-mistakes daemon";
    let Some(daemon) = daemon else {
        return Check::new(
            NAME,
            false,
            Status::Warn,
            format!(
                "{running}; its version could not be read, so skew against the CLI was not checked"
            ),
        );
    };
    match tools::skew(cli, Some(&daemon)) {
        Some(skew) => Check::new(NAME, false, Status::Warn, skew).version(Some(daemon)),
        None => Check::new(
            NAME,
            false,
            Status::Ok,
            format!("{running}; same version as its CLI"),
        )
        .version(Some(daemon)),
    }
}

/// The daemon's merge App (`authority.md`, Enforced merge gate): its key
/// and id under `<home>/forge-app/`. Optional: without it every project runs
/// as `detect`, so a missing key warns; a key the group or others can read
/// fails, because it has leaked off the owner's account. Names files and
/// the reason, never the key.
fn merge_app(home: &Path) -> Check {
    const NAME: &str = "merge app";
    let dir = home.join(APP_DIR);
    match AppKey::load(&dir) {
        Ok(Some(key)) => Check::new(
            NAME,
            false,
            Status::Ok,
            format!("App {} with its key in {}", key.id(), dir.display()),
        ),
        Ok(None) => Check::missing(
            NAME,
            false,
            format!(
                "no merge App key at {}, so every project runs as detect (docs/plan/merge-app-setup.md)",
                dir.join(KEY_FILE).display()
            ),
        ),
        Err(error) => {
            let status = if matches!(error, KeyError::Exposed { .. }) {
                Status::Fail
            } else {
                Status::Warn
            };
            Check::new(
                NAME,
                false,
                status,
                format!("the merge App key is not used, so every project runs as detect: {error}"),
            )
        }
    }
}

/// Every packaged fleet fragment registers `tiller hook <event>` for each
/// handled event and the guard for `PreToolUse` (`protocol.md` 4.1 and 4.2),
/// and no Tiller hook sits in the user's own Claude settings, because
/// Tiller registers in run scope only.
fn hooks() -> Check {
    let expected: Vec<&str> = EVENTS
        .iter()
        .filter(|spec| spec.handler != Handler::Ignore)
        .map(|spec| spec.name)
        .collect();
    let mut problems = Vec::new();
    for role in Role::ALL {
        let template: Value = match serde_json::from_str(role.settings_template()) {
            Ok(template) => template,
            Err(error) => {
                problems.push(format!("{role}: settings do not parse: {error}"));
                continue;
            }
        };
        let registered = template.get("hooks").and_then(Value::as_object);
        let Some(registered) = registered else {
            problems.push(format!("{role}: no hooks registered"));
            continue;
        };
        for event in &expected {
            let command = registered
                .get(*event)
                .and_then(|entries| entries.pointer("/0/hooks/0/command"))
                .and_then(Value::as_str);
            let want = if *event == "PreToolUse" {
                "<tiller> guard pretool".to_owned()
            } else {
                format!("<tiller> hook {event}")
            };
            if !command.is_some_and(|command| command.starts_with(&want)) {
                problems.push(format!("{role}: {event} is not registered as {want}"));
            }
        }
        for event in registered.keys() {
            if !expected.contains(&event.as_str()) {
                problems.push(format!("{role}: {event} is registered but has no handler"));
            }
        }
    }
    if !problems.is_empty() {
        return Check::missing("hook registration", true, problems.join("; "));
    }
    let detail = format!(
        "{} roles register {} events; run scope only",
        Role::ALL.len(),
        expected.len()
    );
    match user_scope_hooks() {
        Some(file) => Check::new(
            "hook registration",
            true,
            Status::Warn,
            format!(
                "{detail}; but {file} also registers tiller hooks, which Tiller never does in user scope"
            ),
        ),
        None => Check::new("hook registration", true, Status::Ok, detail),
    }
}

/// The user-scope Claude settings file when it registers a tiller hook.
fn user_scope_hooks() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".claude")))?;
    let path = dir.join("settings.json");
    let settings: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let mut commands = Vec::new();
    collect_commands(settings.get("hooks")?, &mut commands);
    commands
        .iter()
        .any(|command| {
            let mut words = command.split_whitespace();
            words
                .next()
                .is_some_and(|program| program.rsplit('/').next() == Some("tiller"))
                && matches!(words.next(), Some("hook" | "guard"))
        })
        .then(|| path.display().to_string())
}

/// Every `command` string anywhere under a hooks value.
fn collect_commands(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                match (key.as_str(), inner) {
                    ("command", Value::String(command)) => out.push(command.clone()),
                    _ => collect_commands(inner, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_commands(item, out)),
        _ => {}
    }
}

/// The state-backed checks: each project's stored merge gate, and a
/// `config.toml` newer than its newest `config.changed` event.
fn stored(app: &App, checks: &mut Vec<Check>) -> Vec<Gate> {
    let store = match app.read() {
        Ok(store) => store,
        Err(failure) => {
            checks.push(Check::new(
                "state",
                false,
                Status::Warn,
                if app.db.exists() {
                    format!("state.db cannot be opened ({}); merge gates and config drift were not checked", failure.message)
                } else {
                    "state.db is not initialized, so merge gates and config drift were not checked".into()
                },
            ));
            return Vec::new();
        }
    };
    let mut gates = Vec::new();
    match store.projects() {
        Ok(projects) => {
            for project in projects
                .into_iter()
                .filter(|p| p.repo.is_some() && p.retired_at.is_none())
            {
                let changed = match store.merge_gate_changed_at(&project.id) {
                    Ok(at) => at.map(|at| at.to_string()),
                    Err(error) => {
                        checks.push(Check::new(
                            "state",
                            false,
                            Status::Warn,
                            format!(
                                "merge gate history of {} is unreadable: {error}",
                                project.id
                            ),
                        ));
                        None
                    }
                };
                let enforced = project.merge_gate == tiller_core::domain::MergeGate::Enforced;
                gates.push(Gate {
                    project: project.id,
                    merge_gate: project.merge_gate.to_string(),
                    status: if enforced { Status::Ok } else { Status::Warn },
                    last_changed_at: changed,
                    detail: if enforced {
                        "the ruleset check holds".into()
                    } else {
                        "merge gate: detection only; merges are detected, not prevented".into()
                    },
                });
            }
        }
        Err(error) => checks.push(Check::new(
            "state",
            false,
            Status::Warn,
            format!("projects are unreadable: {error}"),
        )),
    }
    if let Some(check) = config_drift(app, &store) {
        checks.push(check);
    }
    gates
}

fn config_drift(app: &App, store: &tiller_core::Store) -> Option<Check> {
    const NAME: &str = "config drift";
    // A file that does not parse is already a failed `config` check.
    load_config(app).ok()?;
    let modified = std::fs::metadata(config_file(app)).ok()?.modified().ok()?;
    let modified = Timestamp::from_jiff(jiff::Timestamp::try_from(modified).ok()?);
    let changed = match store.config_changed_at() {
        Ok(changed) => changed,
        Err(error) => {
            return Some(Check::new(
                NAME,
                false,
                Status::Warn,
                format!("config.changed events are unreadable: {error}"),
            ));
        }
    };
    let Some(at) = changed else {
        return Some(Check::new(
            NAME,
            false,
            Status::Ok,
            "config.toml was never set through tiller config set, so there is nothing to compare",
        ));
    };
    Some(if modified.as_millis() > at.as_millis() + CONFIG_SLACK_MS {
        Check::new(
            NAME,
            false,
            Status::Warn,
            "config.toml is newer than its last config.changed event; \
             it was edited by hand, or a config set stopped between the file and its event",
        )
    } else {
        Check::new(
            NAME,
            false,
            Status::Ok,
            "config.toml matches its last event",
        )
    })
}

/// Whether a Firstmate runs on this host: some process has an argument
/// whose file name is one of [`FIRSTMATE_SCRIPTS`]. `lines` is the process
/// list, one command line per entry.
fn firstmate_scripts(lines: &str) -> Vec<&'static str> {
    FIRSTMATE_SCRIPTS
        .into_iter()
        .filter(|script| {
            lines.lines().any(|line| {
                line.split_whitespace()
                    .any(|word| word.rsplit('/').next() == Some(script))
            })
        })
        .collect()
}

/// The process list as command lines: `ps -axo args=`, or the command in
/// `TILLER_PS` (the way the tests inject a fixed list).
fn process_list() -> Result<String, String> {
    let program = env_command("TILLER_PS").unwrap_or_else(|| vec!["ps".into()]);
    let (first, rest) = program.split_first().ok_or("no command is configured")?;
    let output = Command::new(first)
        .args(rest)
        .args(["-axo", "args="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run {}: {error}", first.to_string_lossy()))?;
    if !output.status.success() {
        return Err(format!("exited with {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether a daemon holds `<home>/daemon.lock` now. A home that never ran a
/// daemon has no lock file, and the probe must not create one.
fn daemon_running(home: &Path) -> bool {
    home.join(tiller_agents::land::LOCK_FILE).exists()
        && tiller_agents::land::DaemonLock::held(home)
}

/// Who owns the host (`<home>/owner`, R20). A missing marker names no one,
/// and the daemon dispatches under it; a marker naming `firstmate` makes the
/// daemon and every mutation read only, so a daemon that is still running
/// beside it is a host that cannot be trusted to stay quiet.
fn owner_marker(marker: Option<&str>, daemon: bool) -> Check {
    const NAME: &str = "owner marker";
    match marker {
        None => Check::new(
            NAME,
            false,
            Status::Warn,
            format!(
                "no owner marker, so tiller dispatches{}; name the owner with: tiller owner set <owner>",
                if daemon {
                    " (the daemon is running)"
                } else {
                    " when its daemon runs"
                }
            ),
        ),
        Some("firstmate") if daemon => Check::missing(
            NAME,
            true,
            "the owner marker says firstmate owns this host, but the tiller daemon is running; \
             tiller must not dispatch here. Stop the daemon, or hand the host over with: tiller owner set tiller",
        ),
        Some("firstmate") => Check::new(
            NAME,
            true,
            Status::Ok,
            "firstmate owns this host and the tiller daemon is not running",
        ),
        Some("tiller") => Check::new(NAME, true, Status::Ok, "tiller owns this host"),
        Some(other) => Check::new(
            NAME,
            true,
            Status::Ok,
            format!("the owner marker names {other:?}; tiller is read only here"),
        ),
    }
}

/// A running Firstmate beside a Tiller that would also dispatch. `list` is
/// the process list, or why it could not be read.
fn firstmate(list: Result<String, String>, marker: Option<&str>, daemon: bool) -> Check {
    const NAME: &str = "firstmate";
    let list = match list {
        Ok(list) => list,
        Err(error) => {
            return Check::new(
                NAME,
                false,
                Status::Warn,
                format!(
                    "cannot list processes, so a running Firstmate was not looked for: {error}"
                ),
            );
        }
    };
    let found = firstmate_scripts(&list);
    if found.is_empty() {
        return Check::new(NAME, false, Status::Ok, "no Firstmate process is running");
    }
    let running = format!("Firstmate is running ({})", found.join(", "));
    if matches!(marker, None | Some("tiller")) {
        Check::new(
            NAME,
            false,
            Status::Warn,
            format!(
                "{running} and tiller would also dispatch{}, so both could start the same work; \
                 freeze one of them, or name the owner with: tiller owner set <owner>",
                if daemon {
                    " (its daemon is running)"
                } else {
                    ""
                }
            ),
        )
    } else {
        Check::new(
            NAME,
            false,
            Status::Ok,
            format!("{running}, and the owner marker keeps tiller from dispatching"),
        )
    }
}

pub fn run(app: &App) -> Done {
    let mut checks = Vec::new();
    let table = load_config(app);
    let config = match table
        .as_ref()
        .map_err(|failure| failure.message.clone())
        .and_then(|table| SpawnConfig::from_table(table).map_err(|error| error.to_string()))
    {
        Ok(config) => config,
        Err(error) => {
            checks.push(Check::missing(
                "config",
                true,
                format!("config.toml is not usable, so the defaults are checked: {error}"),
            ));
            SpawnConfig::default()
        }
    };
    let spawn = &config.spawn;

    checks.push(tool("git", true, &["git".into()]));
    checks.push(tool("treehouse", true, &words(&spawn.treehouse)));
    checks.push(herdr(herdr_program(spawn)));
    checks.push(claude(&[OsString::from(&spawn.claude)]));
    checks.push(tool(
        "gh",
        true,
        &env_command("TILLER_GH").unwrap_or_else(|| vec!["gh".into()]),
    ));
    checks.push(tool("gh-axi", false, &["gh-axi".into()]));
    checks.push(tool(
        "quota-axi",
        config.quota.enabled,
        &env_command("TILLER_QUOTA_AXI").unwrap_or_else(|| vec!["quota-axi".into()]),
    ));
    let nm = tool("no-mistakes", false, &["no-mistakes".into()]);
    let daemon = no_mistakes(&nm);
    checks.push(nm);
    checks.push(daemon);
    checks.push(hooks());
    let marker = app.owner_marker();
    let daemon = daemon_running(&app.home);
    checks.push(owner_marker(marker.as_deref(), daemon));
    checks.push(firstmate(process_list(), marker.as_deref(), daemon));
    checks.push(merge_app(&app.home));
    let merge_gates = stored(app, &mut checks);

    let ok = !checks.iter().any(Check::failed);
    let report = Report {
        ok,
        checks,
        merge_gates,
    };
    if ok {
        return Ok(Outcome::read(report));
    }
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|check| check.failed())
        .map(|check| check.name.as_str())
        .collect();
    let mut failure = Failure::refused(format!(
        "{} required check{} failed: {}",
        failed.len(),
        if failed.len() == 1 { "" } else { "s" },
        failed.join(", ")
    ))
    .hint("doctor changes nothing; fix the named tools, then run tiller doctor again");
    failure.current = Some(json!(report));
    Err(failure)
}

#[cfg(test)]
mod tests {
    use super::{Status, firstmate, firstmate_scripts, no_mistakes_skew, owner_marker};

    const FM: &str = "bash /opt/fm/bin/fm-watch.sh\n/bin/zsh -l\nbash /opt/fm/bin/fm-supervise-daemon.sh --loop\n";

    #[test]
    fn firstmate_scripts_match_by_file_name_not_substring() {
        assert_eq!(
            firstmate_scripts(FM),
            ["fm-supervise-daemon.sh", "fm-watch.sh"]
        );
        assert!(
            firstmate_scripts(
                "vim fm-watch.sh.bak\ntail -f /x/fm-watch.sh.log\nbash fm-watch-arm.sh"
            )
            .is_empty()
        );
        assert_eq!(firstmate_scripts("fm-watch.sh"), ["fm-watch.sh"]);
    }

    #[test]
    fn owner_marker_cases() {
        assert_eq!(owner_marker(None, false).status, Status::Warn);
        assert!(
            owner_marker(None, false)
                .detail
                .contains("tiller owner set <owner>")
        );
        let clash = owner_marker(Some("firstmate"), true);
        assert!(clash.failed());
        assert_eq!(owner_marker(Some("firstmate"), false).status, Status::Ok);
        assert_eq!(owner_marker(Some("tiller"), true).status, Status::Ok);
    }

    #[test]
    fn firstmate_warns_only_when_tiller_would_also_dispatch() {
        assert_eq!(firstmate(Ok(FM.into()), None, false).status, Status::Warn);
        assert_eq!(
            firstmate(Ok(FM.into()), Some("tiller"), true).status,
            Status::Warn
        );
        assert_eq!(
            firstmate(Ok(FM.into()), Some("firstmate"), true).status,
            Status::Ok
        );
        assert_eq!(firstmate(Ok("zsh".into()), None, true).status, Status::Ok);
        assert_eq!(
            firstmate(Err("no ps".into()), None, true).status,
            Status::Warn
        );
    }

    #[test]
    fn no_mistakes_daemon_older_than_its_cli_warns() {
        let cli = Some("no-mistakes version v1.106.1 (a1b2c3d) 2026-11-01");
        let old = || Some("no-mistakes version v1.84.0 (4822244) 2026-09-26".to_owned());
        let skewed = no_mistakes_skew("daemon running (pid 7)", cli, old());
        assert_eq!(skewed.status, Status::Warn);
        assert!(
            skewed.detail.contains("older than its CLI"),
            "{}",
            skewed.detail
        );
        assert!(!skewed.failed());
        let same = no_mistakes_skew("daemon running (pid 7)", cli, cli.map(str::to_owned));
        assert_eq!(same.status, Status::Ok);
        let unread = no_mistakes_skew("daemon running (pid 7)", cli, None);
        assert_eq!(unread.status, Status::Warn);
        assert!(unread.detail.contains("could not be read"));
    }
}
