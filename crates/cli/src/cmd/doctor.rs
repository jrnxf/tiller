//! `tiller doctor` (P43, T2.8): the host's health checks, read only. It
//! reports the versions of the tools Tiller drives, whether herdr answers,
//! whether the packaged hook registration is whole, the stored `merge_gate`
//! of every project and whether `config.toml` is newer than its last
//! `config.changed` event. It never fixes, installs or writes anything: a
//! failed required check makes the command exit 5 with the full report in
//! the error's `current`.
//!
//! The tools are found the way the daemon and spawn find them:
//! `TILLER_HERDR`, `TILLER_QUOTA_AXI` and `TILLER_GH` first, then the
//! `[spawn]` tables of `config.toml`, then the bare program name.

use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use tiller_agents::roles::Role;
use tiller_agents::spawn::config::SpawnConfig;
use tiller_core::Timestamp;
use tiller_herdr::client::{Cli, Herdr};
use tiller_observe::hooks::{EVENTS, Handler};

use super::admin::{config_file, load_config};
use crate::app::{App, Done, Failure, Outcome};

/// The Claude Code version the hook table in `protocol.md` 4.2 was checked
/// against (`host-capabilities.md`). A newer one is a warning: it may add
/// events the table does not map.
const CLAUDE_CHECKED: &str = "2.1.312";

/// A tool that answers `--version` within this long is alive.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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

/// A program and its leading arguments from an environment variable.
fn env_command(variable: &str) -> Option<Vec<OsString>> {
    let line = std::env::var(variable).ok()?;
    let words: Vec<OsString> = line.split_whitespace().map(OsString::from).collect();
    (!words.is_empty()).then_some(words)
}

fn words(words: &[String]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// `program --version` within [`PROBE_TIMEOUT`]: the first line of stdout.
fn read_version(program: &[OsString]) -> Result<String, String> {
    let (first, rest) = program.split_first().ok_or("no command is configured")?;
    run_first_line(first, rest, "--version").and_then(|line| line.ok_or("printed nothing".into()))
}

/// Runs the command with one more argument; `Ok(Some(line))` is the first
/// non-empty stdout line, `Ok(None)` a clean exit that printed nothing.
fn run_first_line(
    first: &OsString,
    rest: &[OsString],
    arg: &str,
) -> Result<Option<String>, String> {
    let mut child = Command::new(first)
        .args(rest)
        .args(arg.split_whitespace())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run {}: {error}", first.to_string_lossy()))?;
    let mut stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_string(&mut text);
        }
        let _ = tx.send(text);
    });
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("did not answer within {PROBE_TIMEOUT:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(error.to_string()),
        }
    };
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    let text = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "kept its output open".to_owned())?;
    Ok(text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned))
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

/// The numeric parts of a version such as `2.1.312 (Claude Code)`.
fn parts(version: &str) -> Vec<u64> {
    version
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('v')
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
}

fn claude(program: &[OsString]) -> Check {
    let mut check = tool("claude", true, program);
    if let Some(version) = &check.version
        && parts(version) > parts(CLAUDE_CHECKED)
    {
        check.status = Status::Warn;
        check.detail = format!(
            "newer than {CLAUDE_CHECKED}, the version the hook table was checked against; \
             it may add hook events the table does not map"
        );
    }
    check
}

/// The no-mistakes daemon, when the CLI is installed. This CLI's `daemon
/// status` does not report the daemon's version, so skew against the CLI is
/// not readable and the check says so.
fn no_mistakes(cli: &Check) -> Check {
    if cli.status != Status::Ok {
        return Check::new(
            "no-mistakes daemon",
            false,
            Status::Warn,
            "the no-mistakes CLI is missing",
        );
    }
    match run_first_line(&"no-mistakes".into(), &[], "daemon status") {
        Ok(line) => Check::new(
            "no-mistakes daemon",
            false,
            Status::Ok,
            format!(
                "{}; the CLI does not report the daemon's version, so skew is not checked",
                line.unwrap_or_else(|| "running".into())
            ),
        ),
        Err(error) => Check::missing("no-mistakes daemon", false, error),
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
    checks.push(herdr(
        env_command("TILLER_HERDR").unwrap_or_else(|| words(&spawn.herdr)),
    ));
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
    use super::parts;

    #[test]
    fn versions_compare_by_number_not_text() {
        assert!(parts("2.1.312 (Claude Code)") == vec![2, 1, 312]);
        assert!(parts("2.1.1000") > parts("2.1.312"));
        assert!(parts("v3.1.2") == vec![3, 1, 2]);
        assert!(parts("herdr 0.9.5").is_empty());
    }
}
