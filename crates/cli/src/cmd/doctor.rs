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
//!
//! The probes run at the same time, and the report waits for them at most
//! [`REPORT_DEADLINE`]: a probe still running then is reported as not
//! answering, so hung tools cannot hold the report past it.

use std::ffi::OsString;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tiller_agents::land::app::{APP_DIR, AppKey, KEY_FILE, KeyError};
use tiller_agents::roles::Role;
use tiller_agents::spawn::config::SpawnConfig;
use tiller_core::Timestamp;
use tiller_daemon::dto::{CheckStatus, DoctorCheck, DoctorGate, DoctorReport};
use tiller_herdr::client::{Cli, Herdr};
use tiller_holder::lock::{live_pid, lock_path};
use tiller_observe::hooks::{EVENTS, Handler};
use tiller_panes::{HolderClient, run_dir, socket_path};
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

// The shapes are the daemon's, so `tiller doctor --json` and the board's
// `GET /api/v1/doctor` carry the same report.
type Status = CheckStatus;
type Check = DoctorCheck;
type Gate = DoctorGate;
type Report = DoctorReport;

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

/// What the holder's socket and lock showed (`terminal-holder.md` 2.2).
#[derive(Debug, PartialEq, Eq)]
enum HolderState {
    /// It answered a ping.
    Answers { version: String, instance: String },
    /// No socket, and no process holds the lock.
    Absent,
    /// A socket file is left that nothing listens on, and no process holds
    /// the lock: a holder that crashed.
    Stale,
    /// A process holds the lock, but the socket does not answer.
    Silent { pid: u32, error: String },
    /// Something listens but is no holder this build can talk to: another
    /// program, or a holder on a protocol version this tiller does not
    /// speak.
    Broken(String),
}

/// Looks at the holder of `home`. Read only: it connects, pings and reads
/// the lock file's pid, and takes no lock.
fn probe_holder(home: &Path) -> HolderState {
    use std::io::ErrorKind;
    let socket = socket_path(home);
    let pid = live_pid(&lock_path(&run_dir(home)));
    match std::os::unix::net::UnixStream::connect(&socket) {
        Ok(_) => {}
        Err(error) => {
            return match (pid, error.kind()) {
                (Some(pid), _) => HolderState::Silent {
                    pid,
                    error: error.to_string(),
                },
                (None, ErrorKind::NotFound) => HolderState::Absent,
                (None, ErrorKind::ConnectionRefused) => HolderState::Stale,
                (None, _) => HolderState::Broken(error.to_string()),
            };
        }
    }
    match HolderClient::connect(&socket, "cli") {
        Ok(client) => match client.ping() {
            Ok(()) => HolderState::Answers {
                version: client.welcome().holder.clone(),
                instance: client.welcome().instance.clone(),
            },
            Err(error) => HolderState::Broken(error.to_string()),
        },
        Err(error) => match pid {
            Some(pid) => HolderState::Silent {
                pid,
                error: error.to_string(),
            },
            None => HolderState::Broken(error.to_string()),
        },
    }
}

/// The `holder` check. The holder is off until cutover (`terminal-holder.md`
/// 11.2), so none running is a warning; a holder that is there and does not
/// answer is a failure.
fn holder(state: HolderState) -> Check {
    const NAME: &str = "holder";
    match state {
        HolderState::Answers { version, instance } => Check::new(
            NAME,
            true,
            Status::Ok,
            format!("answers a ping; instance {instance}"),
        )
        .version(Some(version))
        .code("answers"),
        HolderState::Absent => Check::new(
            NAME,
            true,
            Status::Warn,
            "no holder runs for this home, so no agent has a terminal; \
             start one with: tiller holder --detach",
        )
        .code("not_running"),
        HolderState::Stale => Check::new(
            NAME,
            true,
            Status::Warn,
            "a holder socket is left but no holder runs (it crashed or was killed); \
             the next holder removes it. Start one with: tiller holder --detach",
        )
        .code("stale_socket"),
        HolderState::Silent { pid, error } => Check::new(
            NAME,
            true,
            Status::Fail,
            format!(
                "pid {pid} holds the holder lock, but the socket does not answer: {error}"
            ),
        )
        .code("not_answering"),
        HolderState::Broken(error) => Check::new(
            NAME,
            true,
            Status::Fail,
            format!("something listens on the holder socket but is no holder this tiller can use: {error}"),
        )
        .code("broken"),
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
/// talking to an old daemon. `cli` is the CLI's version line, `status` what
/// `daemon status` printed and `daemon` the daemon binary's version line.
fn no_mistakes_daemon(
    cli: Option<&str>,
    status: Result<Option<String>, String>,
    daemon: Option<String>,
) -> Check {
    let running = match status {
        Ok(line) => line.unwrap_or_else(|| "running".into()),
        Err(error) => return Check::missing("no-mistakes daemon", false, error),
    };
    no_mistakes_skew(&running, cli, daemon)
}

/// The no-mistakes CLI and its daemon, each put as soon as it is known. The
/// daemon's reads start with the CLI's, so a slow daemon never holds the CLI
/// line, and a missing CLI is put at once without waiting for them. Its
/// binary's version is read after `ps` names it: when both are slow, the
/// daemon line can miss the report's deadline and reads as late.
fn no_mistakes_pair(put: &mut dyn FnMut(Check)) {
    std::thread::scope(|scope| {
        let status =
            scope.spawn(|| tools::run_first_line("no-mistakes".as_ref(), &[], "daemon status"));
        let daemon = scope.spawn(tools::running_daemon_version);
        let cli = tool("no-mistakes", false, &["no-mistakes".into()]);
        // The slots fill in order: the CLI's line, then its daemon's.
        let found = cli.status == Status::Ok;
        let version = cli.version.clone();
        put(cli);
        if !found {
            put(Check::new(
                "no-mistakes daemon",
                false,
                Status::Warn,
                "the no-mistakes CLI is missing",
            ));
            return;
        }
        let status = status
            .join()
            .unwrap_or_else(|_| Err("daemon status stopped before it answered".into()));
        let daemon = daemon.join().ok().flatten();
        put(no_mistakes_daemon(version.as_deref(), status, daemon));
    });
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

/// `[notify] command`, the owner's notification hook (`terminal-holder.md`
/// 7). Optional: with none set, notifications are off once herdr is gone,
/// and herdr shows them until then.
fn notify(table: Option<&toml::Table>) -> Check {
    const NAME: &str = "notify";
    let argv: Vec<&str> = table
        .and_then(|table| table.get("notify"))
        .and_then(|notify| notify.get("command"))
        .and_then(toml::Value::as_array)
        .map(|argv| argv.iter().filter_map(toml::Value::as_str).collect())
        .unwrap_or_default();
    match argv.first() {
        Some(program) if !program.is_empty() => Check::new(
            NAME,
            false,
            Status::Ok,
            format!("owner notifications run {}", argv.join(" ")),
        ),
        _ => Check::new(
            NAME,
            false,
            Status::Warn,
            "owner notifications are off: no [notify] command is set (herdr shows them until the cutover)",
        ),
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
/// `TILLER_PS` (the way the tests inject a fixed list). A list that hangs is
/// killed at the report's `deadline`, so its probe does not outlive it.
fn process_list(deadline: Instant) -> Result<String, String> {
    let program = env_command("TILLER_PS").unwrap_or_else(|| vec!["ps".into()]);
    let (first, rest) = program.split_first().ok_or("no command is configured")?;
    let left = deadline.saturating_duration_since(Instant::now());
    tools::run_within(first, rest, ["-axo", "args="], left).map_err(|error| {
        // Killed at the deadline: worded as every other late probe.
        if Instant::now() >= deadline {
            late()
        } else {
            error
        }
    })
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
        )
        .code("unset"),
        Some("firstmate") if daemon => Check::missing(
            NAME,
            true,
            "the owner marker says firstmate owns this host, but the tiller daemon is running; \
             tiller must not dispatch here. Stop the daemon, or hand the host over with: tiller owner set tiller",
        )
        .code("other_system_daemon_running"),
        Some("firstmate") => Check::new(
            NAME,
            true,
            Status::Ok,
            "firstmate owns this host and the tiller daemon is not running",
        )
        .code("other_system"),
        Some("tiller") => Check::new(NAME, true, Status::Ok, "tiller owns this host").code("tiller"),
        Some(other) => Check::new(
            NAME,
            true,
            Status::Ok,
            format!("the owner marker names {other:?}; tiller is read only here"),
        )
        .code("named_other"),
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
            )
            .code("processes_unreadable");
        }
    };
    let found = firstmate_scripts(&list);
    if found.is_empty() {
        return Check::new(NAME, false, Status::Ok, "no Firstmate process is running")
            .code("not_running");
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
        .code("both_dispatch")
    } else {
        Check::new(
            NAME,
            false,
            Status::Ok,
            format!("{running}, and the owner marker keeps tiller from dispatching"),
        )
        .code("kept_read_only")
    }
}

/// The longest the report waits for its probes, counted from its start. A
/// board host waits 10 s for a peer's answer (`Timing::query`, held to it by
/// a test), so a peer's report must be sent well inside that, however many of
/// its tools hang.
pub const REPORT_DEADLINE: Duration = Duration::from_secs(7);

// A tool that answers inside its own timeout answers inside the report's;
// only a probe that chains two reads can be late.
const _: () = assert!(PROBE_TIMEOUT.as_millis() < REPORT_DEADLINE.as_millis());

/// What a probe still running at [`REPORT_DEADLINE`] reports.
fn late() -> String {
    format!("did not answer within {REPORT_DEADLINE:?}")
}

/// The report's checks in their order: some known at once, the rest made by
/// probes that run on threads of their own.
struct Probes {
    /// Each slot's check: the answer when known at once, else what the slot
    /// reports if its probe is still running at the deadline.
    slots: Vec<Check>,
    /// How many slots a probe still has to fill.
    pending: usize,
    /// When the report stops waiting.
    deadline: Instant,
    tx: mpsc::Sender<(usize, Check)>,
    rx: mpsc::Receiver<(usize, Check)>,
}

impl Probes {
    /// Probes for a report that waits for them until `within` from now.
    fn new(within: Duration) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            slots: Vec::new(),
            pending: 0,
            deadline: Instant::now() + within,
            tx,
            rx,
        }
    }

    /// A check known at once.
    fn ready(&mut self, check: Check) {
        self.slots.push(check);
    }

    /// Starts `probe` on a thread of its own. It fills the slots `late`
    /// holds, in order, through its argument.
    fn start(
        &mut self,
        late: Vec<Check>,
        probe: impl FnOnce(&mut dyn FnMut(Check)) + Send + 'static,
    ) {
        let mut slot = self.slots.len();
        self.pending += late.len();
        self.slots.extend(late);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            probe(&mut |check| {
                // A report past its deadline has gone: the answer is dropped.
                let _ = tx.send((slot, check));
                slot += 1;
            });
        });
    }

    /// [`Probes::start`] for a probe that makes one check.
    fn one(&mut self, late: Check, probe: impl FnOnce() -> Check + Send + 'static) {
        self.start(vec![late], move |put| put(probe()));
    }

    /// A tool that must (or may) answer `--version`.
    fn tool(&mut self, name: &'static str, required: bool, program: Vec<OsString>) {
        self.one(Check::missing(name, required, late()), move || {
            tool(name, required, &program)
        });
    }

    /// The checks, once every probe has answered or the deadline has passed.
    fn finish(self) -> Vec<Check> {
        let Self {
            mut slots,
            mut pending,
            deadline,
            tx,
            rx,
        } = self;
        drop(tx);
        while pending > 0 {
            let Ok((slot, check)) =
                rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            else {
                break;
            };
            if let Some(place) = slots.get_mut(slot) {
                *place = check;
                pending -= 1;
            }
        }
        slots
    }
}

/// Every check, run now: the report `tiller doctor` prints and the board's
/// host card shows (`GET /api/v1/doctor`).
pub fn report(app: &App) -> Report {
    // Every probe that starts a process runs on a thread of its own, and the
    // report waits for them together, at most REPORT_DEADLINE from here: a
    // hung tool costs its own timeout, never the sum of them all.
    let mut probes = Probes::new(REPORT_DEADLINE);
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

    probes.tool("git", true, vec!["git".into()]);
    probes.tool("treehouse", true, words(&spawn.treehouse));
    let program = herdr_program(spawn);
    probes.one(Check::missing("herdr", true, late()), move || {
        herdr(program)
    });
    let program = [OsString::from(&spawn.claude)];
    probes.one(Check::missing("claude", true, late()), move || {
        claude(&program)
    });
    probes.tool(
        "gh",
        true,
        env_command("TILLER_GH").unwrap_or_else(|| vec!["gh".into()]),
    );
    probes.tool("gh-axi", false, vec!["gh-axi".into()]);
    probes.tool(
        "quota-axi",
        config.quota.enabled,
        env_command("TILLER_QUOTA_AXI").unwrap_or_else(|| vec!["quota-axi".into()]),
    );
    probes.start(
        vec![
            Check::missing("no-mistakes", false, late()),
            Check::missing("no-mistakes daemon", false, late()),
        ],
        no_mistakes_pair,
    );
    probes.ready(hooks());
    let holder_home = app.home.clone();
    probes.one(Check::missing("holder", true, late()), move || {
        holder(probe_holder(&holder_home))
    });
    let marker = app.owner_marker();
    let daemon = daemon_running(&app.home);
    probes.ready(owner_marker(marker.as_deref(), daemon));
    let late_list = firstmate(Err(late()), marker.as_deref(), daemon);
    let deadline = probes.deadline;
    probes.one(late_list, move || {
        firstmate(process_list(deadline), marker.as_deref(), daemon)
    });
    probes.ready(merge_app(&app.home));
    probes.ready(notify(table.as_ref().ok()));
    checks.extend(probes.finish());
    let merge_gates = stored(app, &mut checks);

    Report {
        ok: !checks.iter().any(Check::failed),
        checks,
        merge_gates,
    }
}

pub fn run(app: &App) -> Done {
    let report = report(app);
    if report.ok {
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

    /// A peer's report goes out well inside the time its board host waits
    /// for it, with room for the checks around the probes and the link.
    #[test]
    fn the_report_deadline_fits_inside_a_peer_query() {
        let query = tiller_daemon::peer::Timing::default().query;
        assert!(
            super::REPORT_DEADLINE + std::time::Duration::from_secs(2) <= query,
            "{:?} against {query:?}",
            super::REPORT_DEADLINE
        );
    }

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
