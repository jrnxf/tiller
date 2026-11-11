//! A throwaway Tiller home and a way to run the real `tiller` binary against
//! it, for the command snapshots and the idempotency tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use tiller_core::domain::{Effort, RunRole};
use tiller_core::state::runs::{NewOrchestrator, NewRun, SpawnChecks};
use tiller_core::{Actor, Ctx, Store};

/// What a caller's shell may set that changes what `tiller` does, removed
/// from every command a test runs. Run from a Claude Code shell, over ssh or
/// neither, the owner checks see the same caller: no `CLAUDECODE`, no
/// `SSH_CONNECTION`, and the test stand-in for a terminal with no Claude Code
/// behind it only where a test asks for it (`authority::principal`).
pub const CALLER_ENV: &[&str] = &[
    "TILLER_DB",
    "TILLER_SESSION_ID",
    "TILLER_RUN_ID",
    "TILLER_SSH",
    "TILLER_GH",
    "TILLER_TEST_OWNER",
    "CLAUDECODE",
    "SSH_CONNECTION",
];

/// What one command printed and how it exited.
pub struct Out {
    pub code: i32,
    pub json: Value,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.json["ok"] == true
    }

    pub fn data(&self) -> &Value {
        &self.json["data"]
    }

    pub fn error_code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }

    pub fn replayed(&self) -> bool {
        self.json["replayed"] == true
    }

    pub fn events(&self) -> Vec<i64> {
        self.json["events"]
            .as_array()
            .map(|events| events.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default()
    }
}

pub struct World {
    pub dir: TempDir,
    names: RefCell<Names>,
    /// Extra environment for every command, like a fake forge client.
    env: RefCell<Vec<(String, String)>>,
    /// Whether a command without a session stands in for the owner at a
    /// terminal; off, it is a script reading a pipe.
    terminal: Cell<bool>,
}

#[derive(Default)]
struct Names {
    ulids: HashMap<String, String>,
}

impl World {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            names: RefCell::default(),
            env: RefCell::default(),
            terminal: Cell::new(true),
        }
    }

    /// A world that has had `tiller init` and `project add alpha`.
    pub fn initialized() -> Self {
        let world = Self::new();
        world.must(&["init", "--host-label", "host-a", "--lane-capacity", "2"]);
        world.must(&[
            "project",
            "add",
            "alpha",
            "--name",
            "Alpha",
            "--repo",
            "example/alpha",
            "--delivery",
            "direct-pr",
            "--capacity",
            "3",
        ]);
        world
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn db(&self) -> PathBuf {
        self.home().join("state.db")
    }

    /// `tiller` against this home and nothing the caller's shell carries:
    /// none of [`CALLER_ENV`], so a developer's `TILLER_DB` or session never
    /// reaches the command.
    pub fn bare_command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tiller"));
        command.env("TILLER_HOME", self.home());
        for name in CALLER_ENV {
            command.env_remove(name);
        }
        command
    }

    fn command(&self, session: Option<&str>) -> Command {
        let mut command = self.bare_command();
        // A launch records workspace trust in Claude's config; keep the
        // owner's own config out of reach.
        command.env("CLAUDE_CONFIG_DIR", self.dir.path().join("claude"));
        if self.terminal.get() {
            command.env("TILLER_TEST_OWNER", "1");
        }
        if let Some(session) = session {
            command.env("TILLER_SESSION_ID", session);
        }
        command.envs(self.env.borrow().iter().cloned());
        command
    }

    /// Runs every later command at the stand-in terminal (the default), or
    /// with stdin a pipe and no terminal, as a cron job or `ssh host tiller`.
    pub fn set_terminal(&self, on: bool) {
        self.terminal.set(on);
    }

    /// Sets `key` for every later command.
    pub fn set_env(&self, key: &str, value: &str) {
        self.env.borrow_mut().push((key.into(), value.into()));
    }

    /// Starts `tiller --json <args>` as the owner and returns at once, for a
    /// run the test acts on while it runs. Its stdout is piped.
    pub fn spawn(&self, args: &[&str]) -> std::process::Child {
        use std::process::Stdio;
        self.command(None)
            .arg("--json")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Runs `tiller --json <args>` as the owner.
    pub fn run(&self, args: &[&str]) -> Out {
        self.exec(None, args, None)
    }

    /// Runs as a Tiller session (`TILLER_SESSION_ID` set).
    pub fn run_as(&self, session: &str, args: &[&str]) -> Out {
        self.exec(Some(session), args, None)
    }

    /// Runs `tiller hook <event>` the way Claude Code does, with `input` on
    /// stdin, and requires it to succeed.
    pub fn hook(&self, session: Option<&str>, event: &str, input: &str) {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = self
            .command(session)
            .args(["hook", event])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "hook {event}: {output:?}");
    }

    pub fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Out {
        self.exec(None, args, Some(stdin))
    }

    /// Runs as the owner with arguments that need not be UTF-8, as a shell
    /// passes a path.
    pub fn run_os(&self, args: &[&OsStr]) -> Out {
        self.exec(None, args, None)
    }

    fn exec<A: AsRef<OsStr> + std::fmt::Debug>(
        &self,
        session: Option<&str>,
        args: &[A],
        stdin: Option<&str>,
    ) -> Out {
        use std::io::Write;
        use std::process::Stdio;
        let mut command = self.command(session);
        command
            .arg("--json")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn().unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let json = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{args:?} printed no JSON ({error}): {text:?}"));
        Out {
            code: output.status.code().unwrap(),
            json,
        }
    }

    /// Runs and requires success, returning the response.
    pub fn must(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert!(out.ok() && out.code == 0, "{args:?} failed: {}", out.json);
        out
    }

    pub fn must_as(&self, session: &str, args: &[&str]) -> Out {
        let out = self.run_as(session, args);
        assert!(out.ok() && out.code == 0, "{args:?} failed: {}", out.json);
        out
    }

    pub fn store(&self) -> Store {
        Store::open(self.db()).unwrap()
    }

    pub fn owner_ctx(&self) -> Ctx {
        Ctx::new(Actor::Owner)
    }

    /// A live orchestrator for `alpha`; returns its session id.
    pub fn orchestrator(&self) -> String {
        self.store()
            .session_start(
                &self.owner_ctx(),
                NewOrchestrator {
                    project_id: "alpha".into(),
                    claude_session_id: Some("claude-orchestrator".into()),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap()
            .value
            .session
            .id
    }

    /// Starts a builder run on `card` (a display id); returns the run id and
    /// the worker session id.
    pub fn builder_run(&self, card: &str) -> (String, String) {
        let started = self
            .store()
            .run_start(
                &self.owner_ctx(),
                None,
                NewRun {
                    card: card.into(),
                    role: RunRole::Builder,
                    model: "test-model".into(),
                    effort: Effort::Medium,
                    host_id: None,
                    worktree_path: Some("/home/user/work/alpha-1".into()),
                    branch: Some("alpha-1-build".into()),
                    base_branch: Some("main".into()),
                    base_sha: None,
                    config_hash: None,
                    checks: SpawnChecks::default(),
                },
            )
            .unwrap()
            .value;
        (started.run.id, started.session.id)
    }

    // -- normalizing for snapshots --------------------------------------------

    /// The response as a snapshot: ULIDs, their short forms and times replaced
    /// by stable names in order of first appearance, with the exit code.
    pub fn snapshot(&self, out: &Out) -> String {
        let mut json = out.json.clone();
        self.normalize(&mut json);
        format!(
            "exit: {}\n{}",
            out.code,
            serde_json::to_string_pretty(&json).unwrap()
        )
    }

    fn normalize(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.normalize_text(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.normalize(item)),
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if matches!(key.as_str(), "age_seconds" | "age") {
                        *item = Value::String("<age>".into());
                    } else if matches!(key.as_str(), "seq" | "last_seq" | "last_event_seq") {
                        *item = Value::String("<seq>".into());
                    } else if key == "via_terminal" && item.get("chain").is_some() {
                        // The parent chain is the test runner's: its pids
                        // and names change from run to run.
                        item["chain"] = Value::String("<chain>".into());
                        self.normalize(item);
                    } else if key == "events"
                        && item
                            .as_array()
                            .is_some_and(|a| a.iter().all(Value::is_number))
                    {
                        let count = item.as_array().map_or(0, Vec::len);
                        *item = Value::Array(vec![Value::String("<seq>".into()); count]);
                    } else {
                        self.normalize(item);
                    }
                }
            }
            _ => {}
        }
    }

    fn normalize_text(&self, text: &str) -> String {
        if is_time(text) {
            return "<time>".into();
        }
        let mut names = self.names.borrow_mut();
        let mut out = String::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if i + 26 <= bytes.len()
                && bytes[i..i + 26].iter().all(|b| is_crockford(*b))
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
                && (i + 26 == bytes.len() || !bytes[i + 26].is_ascii_alphanumeric())
            {
                let ulid = &text[i..i + 26];
                let next = names.ulids.len() + 1;
                let name = names
                    .ulids
                    .entry(ulid.to_owned())
                    .or_insert_with(|| format!("<id{next}>"))
                    .clone();
                // The short form is the ULID's last seven characters.
                names
                    .ulids
                    .entry(ulid[19..].to_owned())
                    .or_insert_with(|| format!("{}.short", name.trim_end_matches('>')) + ">");
                out.push_str(&name);
                i += 26;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        // Short forms (the last seven characters of a ULID seen above).
        let mut result = String::new();
        let mut token = String::new();
        for c in out.chars().chain(std::iter::once('\0')) {
            if c.is_ascii_alphanumeric() {
                token.push(c);
                continue;
            }
            match names.ulids.get(&token) {
                Some(known) if token.len() == 7 => result.push_str(known),
                _ => result.push_str(&token),
            }
            token.clear();
            if c != '\0' {
                result.push(c);
            }
        }
        result
    }
}

fn is_crockford(byte: u8) -> bool {
    byte.is_ascii_digit()
        || (byte.is_ascii_uppercase() && !matches!(byte, b'I' | b'L' | b'O' | b'U'))
}

fn is_time(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 24
        && bytes[4] == b'-'
        && bytes[10] == b'T'
        && bytes[19] == b'.'
        && bytes[23] == b'Z'
}

/// Asserts one named snapshot of a command's response and exit code.
pub fn snap(world: &World, name: &str, out: &Out) {
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_snapshot_path("../snapshots");
    settings.set_omit_expression(true);
    settings.bind(|| {
        insta::assert_snapshot!(name, world.snapshot(out));
    });
}

/// CPU time (user plus system) of every child this process has reaped. The
/// difference across one call is that call's own work: unlike wall time it
/// does not grow while the call waits for a core. It still grows when the
/// call runs on a slower core, so a test that gates it first calls
/// [`pin_to_performance_cores`].
pub fn children_cpu() -> std::time::Duration {
    use nix::sys::resource::{UsageWho, getrusage};
    use std::time::Duration;
    let usage = getrusage(UsageWho::RUSAGE_CHILDREN).unwrap();
    let timeval =
        |t: nix::sys::time::TimeVal| Duration::new(t.tv_sec() as u64, t.tv_usec() as u32 * 1000);
    timeval(usage.user_time()) + timeval(usage.system_time())
}

/// Pins this thread, and so every child it starts from here on, to the
/// host's performance cores when its CPU mixes two kinds of core, and
/// returns them. A budget on CPU time needs it: an Intel hybrid CPU's
/// efficient cores do the same work in about 1.5 times the CPU time (`tiller
/// card show` measured 6.9 ms pinned to a performance core and 10.6 ms pinned
/// to an efficient one, with the same page faults), and the scheduler moves a
/// child onto them as soon as load fills the performance cores. Linux names
/// the performance cores in `/sys/devices/cpu_core/cpus`. On any other CPU,
/// or when none of them is allowed, nothing changes and the answer is `None`.
pub fn pin_to_performance_cores() -> Option<Vec<usize>> {
    #[cfg(target_os = "linux")]
    {
        use nix::sched::{CpuSet, sched_getaffinity, sched_setaffinity};
        use nix::unistd::Pid;
        let listed = std::fs::read_to_string("/sys/devices/cpu_core/cpus").ok()?;
        let allowed = sched_getaffinity(Pid::from_raw(0)).ok()?;
        let fast: Vec<usize> = parse_cpu_list(&listed)
            .into_iter()
            .filter(|&cpu| allowed.is_set(cpu).unwrap_or(false))
            .collect();
        if fast.is_empty() {
            return None;
        }
        let mut set = CpuSet::new();
        for &cpu in &fast {
            set.set(cpu).ok()?;
        }
        sched_setaffinity(Pid::from_raw(0), &set).ok()?;
        Some(fast)
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// The CPUs a sysfs CPU list names, such as `0-2,6-13,15`.
pub fn parse_cpu_list(text: &str) -> Vec<usize> {
    text.trim()
        .split(',')
        .filter(|part| !part.is_empty())
        .flat_map(|part| match part.split_once('-') {
            Some((first, last)) => match (first.parse::<usize>(), last.parse::<usize>()) {
                (Ok(first), Ok(last)) => (first..=last).collect(),
                _ => Vec::new(),
            },
            None => part.parse().into_iter().collect(),
        })
        .collect()
}

/// A child, such as a daemon, that is stopped with everything it started
/// when it goes out of scope, so a test that fails before it stops the
/// child leaves no process behind.
///
/// The child first gets SIGTERM and `grace` to stop the way it does for
/// the owner: a daemon then stops its curators and herdr calls itself.
/// When it is still running after that, it and every process under it are
/// frozen with SIGSTOP, so none starts another, then killed with SIGKILL,
/// with the whole process group of each one that leads a group of its own
/// (a curator does). A process seen under the child while it stopped that
/// still runs after a clean stop is killed the same way.
pub struct Running {
    child: std::process::Child,
    grace: std::time::Duration,
}

impl Running {
    /// The grace a daemon gets to stop on SIGTERM.
    pub const GRACE: std::time::Duration = std::time::Duration::from_secs(10);

    pub fn new(child: std::process::Child) -> Self {
        Self::with_grace(child, Self::GRACE)
    }

    pub fn with_grace(child: std::process::Child, grace: std::time::Duration) -> Self {
        Self { child, grace }
    }

    fn runs(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl std::ops::Deref for Running {
    type Target = std::process::Child;
    fn deref(&self) -> &std::process::Child {
        &self.child
    }
}

impl std::ops::DerefMut for Running {
    fn deref_mut(&mut self) -> &mut std::process::Child {
        &mut self.child
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        // A reaped child's pid may belong to another process by now.
        if !self.runs() {
            return;
        }
        let Ok(raw) = i32::try_from(self.child.id()) else {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return;
        };
        let root = Pid::from_raw(raw);
        let mut seen = descendants(raw);
        let _ = kill(root, Signal::SIGTERM);
        let deadline = std::time::Instant::now() + self.grace;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Read only while the child is unreaped: until then its pid is
            // its own, and so are the children `ps` names under it.
            if !self.runs() {
                break;
            }
            add_new(&mut seen, descendants(raw));
        }
        if self.runs() {
            let _ = kill(root, Signal::SIGSTOP);
            let mut doomed = freeze(raw);
            // A process seen earlier whose parent exited since is no longer
            // under the child, so `freeze` cannot reach it.
            add_new(&mut doomed, still_running(&seen));
            kill_all(&doomed);
            let _ = self.child.kill();
            let _ = self.child.wait();
            return;
        }
        // Stopped by itself. Whatever it started that outlived it now has
        // another parent.
        kill_all(&still_running(&seen));
    }
}

/// One row of the process table.
#[derive(Clone)]
struct Process {
    pid: i32,
    ppid: i32,
    pgid: i32,
    /// When it started, as `ps` prints it, to the second. A pid that is
    /// reused has another start time, while `exec`, a new parent and a new
    /// group keep it.
    started: String,
}

impl Process {
    /// The same process: its parent, group and command may have changed.
    fn same(&self, other: &Process) -> bool {
        self.pid == other.pid && self.started == other.started
    }
}

/// Every process on the host, from `ps`, which Linux and macOS share.
fn processes() -> Vec<Process> {
    let Ok(out) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,pgid=,lstart="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let pgid = fields.next()?.parse().ok()?;
            let started = fields.collect::<Vec<_>>().join(" ");
            Some(Process {
                pid,
                ppid,
                pgid,
                started,
            })
        })
        .collect()
}

/// Every process under `root`.
fn descendants(root: i32) -> Vec<Process> {
    let table = processes();
    let mut found: Vec<Process> = Vec::new();
    let mut parents = vec![root];
    while let Some(parent) = parents.pop() {
        for process in table.iter().filter(|p| p.ppid == parent) {
            if !found.iter().any(|seen| seen.pid == process.pid) {
                parents.push(process.pid);
                found.push(process.clone());
            }
        }
    }
    found
}

/// The processes of `seen` that still run, as the process table shows them
/// now, with the group they lead by now.
fn still_running(seen: &[Process]) -> Vec<Process> {
    processes()
        .into_iter()
        .filter(|now| seen.iter().any(|p| p.same(now)))
        .collect()
}

/// Adds to `seen` each process of `found` it does not hold yet, and
/// returns how many it added.
fn add_new(seen: &mut Vec<Process>, found: Vec<Process>) -> usize {
    let before = seen.len();
    for process in found {
        if !seen.iter().any(|p| p.same(&process)) {
            seen.push(process);
        }
    }
    seen.len() - before
}

/// SIGSTOP to every process under the stopped `root`, read again until a
/// read finds none it had not stopped, so none of them starts another.
/// Returns them all.
fn freeze(root: i32) -> Vec<Process> {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let mut tree = Vec::new();
    for _ in 0..20 {
        let before = tree.len();
        if add_new(&mut tree, descendants(root)) == 0 {
            break;
        }
        for process in &tree[before..] {
            let _ = kill(Pid::from_raw(process.pid), Signal::SIGSTOP);
        }
    }
    tree
}

/// SIGKILL to each process, and to the group of each one that leads its
/// own. The test's own group is never signalled.
fn kill_all(processes: &[Process]) {
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::{Pid, getpgrp};
    let own = getpgrp().as_raw();
    for process in processes {
        if process.pgid == process.pid && process.pgid != own {
            let _ = killpg(Pid::from_raw(process.pgid), Signal::SIGKILL);
        }
        let _ = kill(Pid::from_raw(process.pid), Signal::SIGKILL);
    }
}

/// The first line a child writes to its piped stdout, or `None` when stdout
/// closes first. Panics when nothing comes within `wait`.
pub fn first_line(child: &mut std::process::Child, wait: std::time::Duration) -> Option<String> {
    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .unwrap_or(0);
        let _ = tx.send((read > 0).then_some(line));
    });
    rx.recv_timeout(wait).expect("the daemon said nothing")
}

/// The port a `tiller daemon --port 0` child listens on, from the first line
/// it writes.
pub fn listening_port(child: &mut std::process::Child) -> u16 {
    let line = first_line(child, std::time::Duration::from_secs(20))
        .expect("the daemon exited before listening");
    line.trim()
        .strip_prefix("listening on http://127.0.0.1:")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .parse()
        .unwrap()
}

/// The real daemon on a free port, with a `PATH` that holds `git` and
/// nothing else, so no poller reaches a real herdr, `gh` or `quota-axi`.
/// Returns its guard and its port.
pub fn git_only_daemon(world: &World) -> (Running, u16) {
    use std::process::Stdio;
    let bin = world.dir.path().join("git-only");
    if !bin.join("git").exists() {
        std::fs::create_dir_all(&bin).unwrap();
        let found = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        let git = String::from_utf8(found.stdout).unwrap();
        std::os::unix::fs::symlink(git.trim(), bin.join("git")).unwrap();
    }
    let mut child = Running::new(
        world
            .bare_command()
            .args(["daemon", "--port", "0"])
            .env("PATH", &bin)
            .env("TILLER_HERDR", "/nonexistent/herdr")
            .env("TILLER_QUOTA_AXI", "/nonexistent/quota-axi")
            .env("TILLER_LOG", "warn")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let port = listening_port(&mut child);
    (child, port)
}
