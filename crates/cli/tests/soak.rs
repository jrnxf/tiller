//! The terminal holder's end-to-end soak (`terminal-holder.md` 9.2, 9.3 and
//! 13, row H14): a test home with agent sessions in holder panes, a daemon
//! that delivers to them, and over a set time a schedule of daemon
//! restarts, three live upgrades (`tiller holder upgrade`) and one holder
//! kill. After every event, and on a timer, each worker's counter is read
//! back from the holder's screen and scrollback and must have no gap and no
//! repeated number, and the hooks below read what the daemon recorded.
//!
//! # The sessions
//!
//! `fake-agent` stands in for Claude Code. The project's orchestrator runs
//! it with its composer, and takes a numbered message through the outbox
//! at every event. Each worker run runs `fake-agent --counter`, which
//! writes `count <n>`, from 0 and up by one, every 50 ms. The `claude` the
//! launch line names fires the `SessionStart` hook and picks the mode by
//! its working directory, so the daemon's resume (R21) starts the same
//! mode again.
//!
//! # The counters
//!
//! The harness keeps, per worker, the last number it verified. A read of
//! the pane's scrollback must then hold every number from that one on
//! (from 0 in a new epoch), once and in order. A number that is missing,
//! repeated or older than the last one is a gap, and the soak stops with
//! it. The newest line of a read is left for the next read, because a read
//! may land inside a line. A pane that makes no progress within `STALL`
//! after an event is a gap too. The holder kill is the one event that ends
//! an epoch: its panes are gone (spec 6.4), and the daemon resumes each
//! session in a pane of the new holder (R21), whose counter starts again
//! from 0. The numbers the old epoch wrote after the last read are lost
//! with the holder, and the report says so.
//!
//! # The hooks
//!
//! Each entry of [`HOOKS`] runs after the events it names and reads what
//! the daemon recorded: the outbox and the stand-in's log, the events
//! table, the `panes` cache, `meta`, health and the Machines row. Every
//! report lists each hook with the number of times it passed.
//!
//! # How to run
//!
//! The short mode is the test `a_short_soak_has_no_gap`. It runs in CI, in
//! the `wall-clock` nextest group. The long mode is `the_long_soak`, an
//! ignored test. H14 runs it for 4 h:
//!
//! ```text
//! scripts/soak.sh              # 4 h, the report in target/tmp/soak/
//! scripts/soak.sh 600          # 10 min, the same schedule scaled down
//! ```
//!
//! The script builds `fake-agent`, then runs `cargo nextest run -p
//! tiller-cli --test soak --run-ignored only the_long_soak`, with
//! `TILLER_SOAK_SECS` for the length and `TILLER_SOAK_OUT` for the report
//! directory. It writes `soak-result.json` (machine readable, rewritten
//! after every event so a stopped run leaves its progress) and
//! `soak-report.md`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::holder::{cookie, fake_agent, get, script};
use common::{Running, World, git_only_daemon_logging};
use jiff::Timestamp;
use serde_json::{Value, json};
use tiller_core::state::runs::{NewRun, SpawnChecks};
use tiller_core::{Actor, Ctx};
use tiller_panes::{HolderClient, Pane, PaneState, Panes, Read, socket_path};

const TILLER: &str = env!("CARGO_BIN_EXE_tiller");

/// How long a step waits for the holder or the daemon to answer.
const WAIT: Duration = Duration::from_secs(20);

/// How long a pane may go without a new number after an event.
const STALL: Duration = Duration::from_secs(15);

/// How long the daemon may take to resume every session after the holder
/// kill: R21 launches one session per reconcile pass (30 s apart, at most
/// one per `launch_gap_seconds`), and writes `holder.lost` on the pass
/// after the last one binds.
const RESTORE: Duration = Duration::from_secs(300);

/// How long a message may wait for delivery: one deferral for a busy pane
/// (`BUSY_DEFER_SECS`, 30 s, in `tiller_delivery::outbox`) and the wait.
const DELIVER: Duration = Duration::from_secs(50);

/// How long a line the daemon typed before an ack can take to reach the
/// stand-in's log: the 500 ms `Pacing` settle before Enter
/// (`crates/panes/src/submit.rs`), plus the serve pass and the pty write,
/// with room to spare.
const ACK_SETTLE: Duration = Duration::from_secs(3);

/// The scrollback a read takes: all the holder keeps (spec 2.8).
const WINDOW: u16 = 10_000;

/// A check the soak makes after some events.
struct Hook {
    name: &'static str,
    after: &'static str,
    checks: &'static str,
}

/// Every hook. Each one is called from `Soak::hooks`, and each report lists
/// how often it passed.
const HOOKS: &[Hook] = &[
    Hook {
        name: "delivery_counts",
        after: "every event, and the end",
        checks: "every message to the orchestrator so far, the soak's and the daemon's own notes, is `acked` in the outbox after exactly one typed wake line covered it (`tiller inbox` drains a batch), with no line naming an acked message that reached the log more than a settle after its ack (the daemon may type a batch's head again while it is unacked); then the next message goes out",
    },
    Hook {
        name: "daemon_reconnect",
        after: "each daemon restart",
        checks: "`meta.holder_instance` unchanged, no new `holder.lost` or `session.resume_attempted`, the holder poller succeeding after the restart with no failure, and the live `panes` rows the holder's panes on its instance",
    },
    Hook {
        name: "upgrade_observed_by_daemon",
        after: "each live upgrade",
        checks: "the instance unchanged in the holder and in `meta`, no `holder.upgraded` (the soak upgrades to the same binary, so the version does not change), the holder poller succeeding after the re-exec, the live `panes` rows the same panes, and no \"holder upgrade refused\" in health's warnings",
    },
    Hook {
        name: "holder_kill_ladder",
        after: "the holder kill",
        checks: "one `holder.lost` from the old instance to the new one, a `session.resume_attempted` and a pane on the new instance for every session the kill took down, the new `panes` rows, the Machines row's `holder_restarted_at`, and one owner notification through `[notify] command` that all sessions resumed",
    },
];

// -- the plan ------------------------------------------------------------------

/// What one soak run does.
#[derive(Clone)]
struct Plan {
    duration: Duration,
    /// Worker runs, each a counter.
    workers: usize,
    /// How often the counters are read between events.
    poll: Duration,
    /// Daemon restarts, spread over the run.
    restarts: usize,
    /// A test fault: the first worker skips a number.
    skip_at: Option<u64>,
}

impl Plan {
    /// The plan for a run of `secs` seconds: polls and restarts scale with
    /// it, and the events always include 3 upgrades and 1 kill. Every run
    /// has two workers: a worker costs two spawns (its start and its
    /// relaunch), and a project allows 5 in 10 minutes, the orchestrator's
    /// start among them.
    fn of(secs: u64) -> Self {
        Self {
            duration: Duration::from_secs(secs),
            workers: 2,
            poll: Duration::from_secs((secs / 60).clamp(2, 30)),
            restarts: ((secs / 900) as usize).max(4),
            skip_at: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    DaemonRestart,
    Upgrade,
    HolderKill,
}

/// The events and when each one fires, in order: the upgrades at 20, 45 and
/// 70% of the run, the kill at 85%, and the restarts spread over the run,
/// moved off any other event.
fn schedule(plan: &Plan) -> Vec<(Duration, Event)> {
    let at = |fraction: f64| plan.duration.mul_f64(fraction);
    let mut events = vec![
        (at(0.20), Event::Upgrade),
        (at(0.45), Event::Upgrade),
        (at(0.70), Event::Upgrade),
        (at(0.85), Event::HolderKill),
    ];
    for k in 0..plan.restarts {
        let mut fraction = (k as f64 + 0.5) / plan.restarts as f64 * 0.95;
        while events.iter().any(|(when, _)| {
            (when.as_secs_f64() / plan.duration.as_secs_f64() - fraction).abs() < 0.02
        }) {
            fraction += 0.03;
        }
        events.push((at(fraction.min(0.97)), Event::DaemonRestart));
    }
    events.sort_by_key(|(when, _)| *when);
    events
}

// -- the counters --------------------------------------------------------------

/// One worker's counter, as the harness has verified it.
#[derive(Clone, Debug)]
struct Counter {
    key: String,
    /// The worker's session, which keeps its id across a resume.
    session: String,
    /// The holder pane the session runs in now.
    pane: String,
    pid: u32,
    epoch: u32,
    /// The last number verified in this epoch.
    last: Option<u64>,
    /// The numbers verified in all epochs.
    verified: u64,
    /// The reads that verified something.
    reads: u64,
}

impl Counter {
    fn new(key: &str) -> Self {
        Self {
            key: key.to_owned(),
            session: String::new(),
            pane: String::new(),
            pid: 0,
            epoch: 1,
            last: None,
            verified: 0,
            reads: 0,
        }
    }

    /// The numbers the pane's counter wrote in `text`, in the order they
    /// stand. A pane holds one epoch: a resume runs in a new pane.
    fn numbers(text: &str) -> Vec<u64> {
        text.lines()
            .filter_map(|line| line.trim().strip_prefix("count ")?.parse().ok())
            .collect()
    }

    /// Checks a read of the pane's scrollback against what was verified
    /// before, and takes it in. The newest line of text is not taken: a
    /// read may land inside it. An epoch's first read starts at 0.
    fn verify(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim_end();
        let complete = text.rfind('\n').map_or("", |end| &text[..end]);
        let numbers = Self::numbers(complete);
        let Some((&first, &last)) = numbers.first().zip(numbers.last()) else {
            return Ok(());
        };
        for pair in numbers.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b != a + 1 {
                return Err(format!(
                    "gap in {} e{}: {a} is followed by {b}{}",
                    self.key,
                    self.epoch,
                    if b <= a { " (repeat or reset)" } else { "" }
                ));
            }
        }
        if self.last.is_none() && first != 0 {
            return Err(format!(
                "gap in {} e{}: the first read starts at {first}, not 0",
                self.key, self.epoch
            ));
        }
        if let Some(before) = self.last {
            if first > before + 1 {
                return Err(format!(
                    "gap in {} e{}: verified up to {before}, the next read starts at {first}",
                    self.key, self.epoch
                ));
            }
            if last < before {
                return Err(format!(
                    "gap in {} e{}: verified up to {before}, the next read ends at {last} (a reset)",
                    self.key, self.epoch
                ));
            }
        }
        self.verified += match self.last {
            Some(before) => last - before,
            None => last + 1,
        };
        self.reads += 1;
        self.last = Some(last);
        Ok(())
    }
}

// -- the home, the holder and the sessions -------------------------------------

/// The `claude` every launch runs: it fires the `SessionStart` hook Claude
/// Code would, under the Claude session id `claude-<session>`, then becomes
/// `fake-agent` logging to a file per session. A worker, whose working
/// directory is under the soak's own `work/`, runs the counter. The match is
/// on that whole path, as a temporary directory can sit under another
/// `work/` (a CI runner's is). With a skip, the first worker runs a shell
/// counter that skips that number.
fn claude_stub(root: &Path, skip_at: Option<u64>) -> String {
    let work = std::fs::canonicalize(root).unwrap().join("work");
    let work = work.display();
    let skip = skip_at.map_or(String::new(), |skip| {
        format!(
            "  \"{work}\"/soak-1) i=0; while :; do echo \"count $i\"; i=$((i + 1)); \
             [ \"$i\" = {skip} ] && i=$((i + 1)); sleep 0.05; done ;;\n"
        )
    });
    format!(
        "if [ \"$1\" = --version ]; then exec {agent} --version; fi\n\
         printf '{{\"session_id\":\"claude-%s\",\"source\":\"startup\"}}' \"$TILLER_SESSION_ID\" \
         | {TILLER} hook SessionStart >/dev/null 2>>{logs}/hook.err\n\
         case \"$(pwd -P)\" in\n{skip}  \"{work}\"/*) set -- --counter \"$@\" ;;\nesac\n\
         exec env FAKE_AGENT_LOG={logs}/\"$TILLER_SESSION_ID\".jsonl {agent} \"$@\"\n",
        agent = fake_agent().display(),
        logs = root.join("fake-agent").display(),
    )
}

/// Runs `git` in `dir`, which must succeed.
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// A home for the soak: one project with room for every worker, launches
/// that run the stub, a `[notify] command` that appends each note to
/// `notified`, and the daemon's git-only `PATH` with `env` and `claude`
/// beside git, so its resume (R21) can start the stub.
fn home(plan: &Plan) -> World {
    let world = World::new();
    let capacity = (plan.workers + 2).to_string();
    world.must(&[
        "init",
        "--host-label",
        "host-a",
        "--lane-capacity",
        &capacity,
    ]);
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
        &capacity,
    ]);
    let root = world.dir.path().to_owned();
    let checkout = root.join("checkouts/alpha");
    std::fs::create_dir_all(&checkout).unwrap();
    world.must(&[
        "config",
        "set",
        "checkouts.alpha",
        checkout.to_str().unwrap(),
    ]);
    for (key, dir) in [
        ("spawn.pool_root", "pool"),
        ("spawn.scratch_root", "scratch"),
        ("spawn.orchestrator_home", "orchestrators"),
        ("spawn.checkouts_root", "checkouts"),
        ("", "fake-agent"),
    ] {
        let dir = root.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        if !key.is_empty() {
            world.must(&["config", "set", key, dir.to_str().unwrap()]);
        }
    }
    let claude = root.join("claude-stub");
    script(&claude, &claude_stub(&root, plan.skip_at));
    world.must(&["config", "set", "spawn.claude", claude.to_str().unwrap()]);

    let notify = root.join("notify");
    script(
        &notify,
        &format!(
            // The daemon's PATH holds only git: shell builtins copy stdin.
            "while IFS= read -r line || [ -n \"$line\" ]; do printf '%s' \"$line\"; done >> {out}\necho >> {out}\n",
            out = root.join("notified").display()
        ),
    );
    let config = world.home().join("config.toml");
    let mut table = std::fs::read_to_string(&config).unwrap_or_default();
    table.push_str(&format!(
        "\n[notify]\ncommand = [\"{}\"]\n",
        notify.display()
    ));
    std::fs::write(&config, table).unwrap();

    let bin = root.join("git-only");
    std::fs::create_dir_all(&bin).unwrap();
    let found = std::process::Command::new("sh")
        .args(["-c", "command -v env"])
        .output()
        .unwrap();
    std::os::unix::fs::symlink(
        String::from_utf8(found.stdout).unwrap().trim(),
        bin.join("env"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&claude, bin.join("claude")).unwrap();
    world
}

/// A worker run on a new card in its own repository, on its branch, with
/// its brief, started in a holder pane. Returns the run.
fn worker(world: &World, n: usize) -> String {
    let root = world.dir.path();
    world.must(&[
        "card",
        "create",
        "alpha",
        "--kind",
        "ship",
        "--title",
        &format!("Soak {n}"),
    ]);
    let card = format!("alpha-{n}");
    let branch = format!("{card}-build");
    let worktree = root.join(format!("work/soak-{n}"));
    std::fs::create_dir_all(&worktree).unwrap();
    git(&worktree, &["init", "-q", "-b", &branch]);
    git(
        &worktree,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "start",
        ],
    );
    let run = world
        .store()
        .run_start(
            &Ctx::new(Actor::Owner),
            None,
            NewRun {
                card,
                role: tiller_core::domain::RunRole::Builder,
                model: "test-model".into(),
                effort: tiller_core::domain::Effort::Medium,
                host_id: None,
                worktree_path: Some(worktree.to_str().unwrap().to_owned()),
                branch: Some(branch),
                base_branch: Some("main".into()),
                base_sha: None,
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value
        .run
        .id;
    let scratch = root.join("scratch").join(&run);
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("brief.md"), "Count.\n").unwrap();
    let done = world.must(&["agent", "relaunch", &run, "--note", "soak start"]);
    assert_eq!(done.data()["started"], "confirmed", "{:?}", done.data());
    run
}

struct Holder {
    running: Running,
}

impl Holder {
    fn start(world: &World) -> Self {
        let running = Running::new(
            world
                .bare_command()
                .arg("holder")
                .env("TILLER_LOG", "info")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(log_path(world)).unwrap())
                .spawn()
                .unwrap(),
        );
        Self { running }
    }
}

fn log_path(world: &World) -> PathBuf {
    world.dir.path().join("holder.log")
}

/// The daemon, every one of them logging into one file the report keeps.
fn start_daemon(world: &World) -> (Running, u16) {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log_path(world))
        .unwrap();
    git_only_daemon_logging(world, log.into(), "info")
}

fn daemon_log_path(world: &World) -> PathBuf {
    world.dir.path().join("daemon.log")
}

/// A new client, once the holder listens.
fn client(world: &World) -> Result<HolderClient, String> {
    let socket = socket_path(&world.home());
    let deadline = Instant::now() + WAIT;
    loop {
        match HolderClient::connect(&socket, "soak") {
            Ok(client) => return Ok(client),
            Err(error) if Instant::now() > deadline => {
                return Err(format!("the holder did not listen: {error}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn holder_log(world: &World) -> String {
    std::fs::read_to_string(log_path(world)).unwrap_or_default()
}

/// Polls `done` until it returns `Some`, or fails with what `done` said
/// last after `wait`.
fn until<T>(wait: Duration, mut done: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    let deadline = Instant::now() + wait;
    loop {
        match done() {
            Ok(value) => return Ok(value),
            Err(error) if Instant::now() > deadline => {
                return Err(format!("after {} s: {error}", wait.as_secs()));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn timestamp(value: &Value) -> Option<Timestamp> {
    value.as_str()?.parse().ok()
}

// -- the report ------------------------------------------------------------------

struct Logged {
    at: Duration,
    kind: &'static str,
    note: String,
}

/// A message to the orchestrator: its outbox id, the submitted line (by
/// its place in the stand-in's log) whose `tiller inbox` took it, and how
/// many lines the log held a settle after the take that acked it.
struct Message {
    id: String,
    acked_by: Option<usize>,
    typed_before: usize,
}

struct Soak {
    world: World,
    plan: Plan,
    out: PathBuf,
    holder: Holder,
    daemon: Option<(Running, u16)>,
    cookie: String,
    orchestrator: String,
    counters: Vec<Counter>,
    messages: Vec<Message>,
    /// The messages the soak sent itself.
    sent: usize,
    /// The submitted lines already run.
    submits_taken: usize,
    /// The holder instance, as the last event left it.
    instance: String,
    /// The instance the last holder kill took down.
    killed: Option<String>,
    upgrades: usize,
    restarts: usize,
    kills: usize,
    checks: u64,
    /// How often each hook passed.
    passed: BTreeMap<&'static str, u64>,
    started: Instant,
    started_at: Timestamp,
    log: Vec<Logged>,
    failure: Option<String>,
}

impl Soak {
    fn new(plan: Plan, out: PathBuf) -> Result<Self, String> {
        let world = home(&plan);
        let holder = Holder::start(&world);
        let instance = client(&world)?.list().map_err(|e| e.to_string())?.instance;
        let started = world.must(&["orchestrator", "start", "alpha"]);
        let orchestrator = started.data()["session"]["id"].as_str().unwrap().to_owned();
        let mut counters = Vec::new();
        for n in 1..=plan.workers {
            let run = worker(&world, n);
            let mut counter = Counter::new(&format!("soak-{n}"));
            counter.session = world
                .store()
                .conn()
                .query_row(
                    "SELECT id FROM sessions WHERE run_id = ?1 AND ended_at IS NULL",
                    [&run],
                    |row| row.get(0),
                )
                .map_err(|e| format!("the session of {run}: {e}"))?;
            counters.push(counter);
        }
        let daemon = start_daemon(&world);
        let cookie = cookie(&world);
        let mut soak = Self {
            world,
            plan,
            out,
            holder,
            daemon: Some(daemon),
            cookie,
            orchestrator,
            counters,
            messages: Vec::new(),
            sent: 0,
            submits_taken: 0,
            instance,
            killed: None,
            upgrades: 0,
            restarts: 0,
            kills: 0,
            checks: 0,
            passed: BTreeMap::new(),
            started: Instant::now(),
            started_at: Timestamp::now(),
            log: Vec::new(),
            failure: None,
        };
        soak.bind(WAIT)?;
        // The daemon's holder source lists the panes before the first event
        // compares against them.
        until(WAIT, || soak.rows_match())?;
        Ok(soak)
    }

    fn since(&self) -> Duration {
        self.started.elapsed()
    }

    fn note(&mut self, kind: &'static str, note: impl Into<String>) {
        let note = note.into();
        eprintln!("[soak {:>6.0}s] {kind}: {note}", self.since().as_secs_f64());
        self.log.push(Logged {
            at: self.since(),
            kind,
            note,
        });
    }

    fn port(&self) -> Result<u16, String> {
        self.daemon
            .as_ref()
            .map(|(_, port)| *port)
            .ok_or_else(|| "no daemon".to_owned())
    }

    fn count(&self, sql: &str, params: impl rusqlite::Params) -> i64 {
        self.world
            .store()
            .conn()
            .query_row(sql, params, |row| row.get(0))
            .unwrap()
    }

    fn last_seq(&self) -> i64 {
        self.count("SELECT coalesce(max(seq), 0) FROM events", [])
    }

    fn events_since(&self, kind: &str, seq: i64) -> Vec<Value> {
        let store = self.world.store();
        let mut statement = store
            .conn()
            .prepare(
                "SELECT entity_id, payload FROM events WHERE kind = ?1 AND seq > ?2 ORDER BY seq",
            )
            .unwrap();
        statement
            .query_map(rusqlite::params![kind, seq], |row| {
                let entity: String = row.get(0)?;
                let payload: String = row.get(1)?;
                Ok(json!({"entity": entity, "payload": serde_json::from_str::<Value>(&payload).unwrap_or(Value::Null)}))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    fn meta(&self, key: &str) -> Option<String> {
        self.world
            .store()
            .conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .ok()
    }

    fn pane_of(&self, session: &str) -> Option<String> {
        self.world
            .store()
            .conn()
            .query_row(
                "SELECT pane_id FROM sessions WHERE id = ?1",
                [session],
                |row| row.get(0),
            )
            .ok()
            .flatten()
    }

    /// Every session's pane, read from the database and found running in
    /// the holder. Takes in each worker's pane and pid.
    fn bind(&mut self, wait: Duration) -> Result<(), String> {
        let client = client(&self.world)?;
        let mut sessions = vec![self.orchestrator.clone()];
        sessions.extend(self.counters.iter().map(|c| c.session.clone()));
        let panes = until(wait, || {
            let listing = client.list().map_err(|e| e.to_string())?;
            let mut found = BTreeMap::new();
            for session in &sessions {
                let pane = self
                    .pane_of(session)
                    .ok_or_else(|| format!("session {session} has no pane"))?;
                let held = listing
                    .panes
                    .iter()
                    .find(|p| p.pane.as_str() == pane && p.state == PaneState::Running)
                    .ok_or_else(|| {
                        format!("session {session}'s pane {pane} is not running in the holder")
                    })?;
                found.insert(session.clone(), held.clone());
            }
            Ok(found)
        })?;
        for counter in &mut self.counters {
            let pane = &panes[&counter.session];
            counter.pane = pane.pane.as_str().to_owned();
            counter.pid = pane.pid;
        }
        Ok(())
    }

    /// The panes of the holder, by worker key.
    fn panes(&self, client: &HolderClient) -> Result<BTreeMap<String, Pane>, String> {
        let listing = client.list().map_err(|e| e.to_string())?;
        if listing.instance != self.instance {
            return Err(format!(
                "the holder instance changed from {} to {}",
                self.instance, listing.instance
            ));
        }
        let mut panes = BTreeMap::new();
        for counter in &self.counters {
            let pane = listing
                .panes
                .iter()
                .find(|pane| pane.pane.as_str() == counter.pane)
                .ok_or_else(|| {
                    format!(
                        "pane {} of {} is gone from the holder",
                        counter.pane, counter.key
                    )
                })?;
            panes.insert(counter.key.clone(), pane.clone());
        }
        Ok(panes)
    }

    /// Reads every counter back and checks it: the same panes, the same
    /// pids, still running, and no gap.
    fn check(&mut self) -> Result<(), String> {
        let client = client(&self.world)?;
        let panes = self.panes(&client)?;
        for counter in &mut self.counters {
            let pane = &panes[&counter.key];
            if pane.state != PaneState::Running {
                return Err(format!(
                    "pane {} is not running: {:?}",
                    counter.key, pane.state
                ));
            }
            if counter.pid != pane.pid {
                return Err(format!(
                    "pane {} changed pid from {} to {}",
                    counter.key, counter.pid, pane.pid
                ));
            }
            let text = client
                .screen(&pane.pane, Read::Recent(WINDOW), tiller_panes::Format::Text)
                .map_err(|e| format!("screen {}: {e}", counter.key))?
                .text();
            counter.verify(&text)?;
        }
        // Acks before a delivered message rings again (2 min).
        self.take_submits()?;
        self.checks += 1;
        Ok(())
    }

    /// Waits until every pane has written past what was verified before the
    /// event, then checks them.
    fn settle(&mut self) -> Result<(), String> {
        let before: Vec<Option<u64>> = self.counters.iter().map(|c| c.last).collect();
        let deadline = Instant::now() + STALL;
        loop {
            self.check()?;
            let moved = self.counters.iter().zip(&before).all(|(counter, before)| {
                match (counter.last, before) {
                    (Some(now), Some(before)) => now > *before,
                    (Some(_), None) => true,
                    _ => false,
                }
            });
            if moved {
                return Ok(());
            }
            if Instant::now() > deadline {
                let stalled: Vec<_> = self
                    .counters
                    .iter()
                    .zip(&before)
                    .filter(|(c, b)| c.last <= **b)
                    .map(|(c, _)| c.key.as_str())
                    .collect();
                return Err(format!(
                    "stalled: {stalled:?} wrote no new number in {}s",
                    STALL.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn event(&mut self, event: Event) -> Result<(), String> {
        // The state just before, so a number written during the event is
        // read after it.
        self.check()?;
        let seq = self.last_seq();
        let at = match event {
            Event::DaemonRestart => self.daemon_restart()?,
            Event::Upgrade => self.upgrade()?,
            Event::HolderKill => self.holder_kill()?,
        };
        self.settle()?;
        self.hooks(event, seq, at)
    }

    /// Restarts the daemon. Returns when the new one started.
    fn daemon_restart(&mut self) -> Result<Timestamp, String> {
        let (mut daemon, _) = self.daemon.take().ok_or("no daemon to restart")?;
        // Every other restart is a crash.
        let hard = self.restarts % 2 == 1;
        if hard {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        drop(daemon);
        let at = Timestamp::now();
        self.daemon = Some(start_daemon(&self.world));
        self.restarts += 1;
        self.note(
            "daemon_restart",
            if hard {
                "SIGKILL, then a new daemon"
            } else {
                "SIGTERM, then a new daemon"
            },
        );
        Ok(at)
    }

    /// A live upgrade to the same binary. Returns when the holder
    /// re-executed.
    fn upgrade(&mut self) -> Result<Timestamp, String> {
        let out = self.world.run(&["holder", "upgrade", "--binary", TILLER]);
        if out.data()["upgrade"] != "started" {
            return Err(format!(
                "the upgrade was not started: {}\n{}",
                out.json,
                holder_log(&self.world)
            ));
        }
        self.upgrades += 1;
        let deadline = Instant::now() + WAIT;
        loop {
            let reexecs = holder_log(&self.world)
                .matches("the holder re-executed")
                .count();
            if reexecs == self.upgrades {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "{} upgrades ran, the holder log shows {reexecs} re-executions",
                    self.upgrades
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let at = Timestamp::now();
        if self.holder.running.try_wait().ok().flatten().is_some() {
            return Err("the holder process exited in the upgrade".into());
        }
        let listing = client(&self.world)?.list().map_err(|e| e.to_string())?;
        if listing.instance != self.instance {
            return Err(format!(
                "the upgrade changed the instance from {} to {}",
                self.instance, listing.instance
            ));
        }
        self.note(
            "upgrade",
            format!("live upgrade {} of 3, same pid and instance", self.upgrades),
        );
        Ok(at)
    }

    /// SIGKILL to the holder and a new one. The daemon sees the new
    /// instance and resumes every session in it (R21); the harness waits
    /// for that and takes in the new panes as the next epoch. Returns when
    /// the holder was killed.
    fn holder_kill(&mut self) -> Result<Timestamp, String> {
        let at = Timestamp::now();
        let killed = Instant::now();
        let _ = self.holder.running.kill();
        let _ = self.holder.running.wait();
        self.holder = Holder::start(&self.world);
        let listing = client(&self.world)?.list().map_err(|e| e.to_string())?;
        if listing.instance == self.instance {
            return Err("the holder came back with the same instance after a kill".into());
        }
        if !listing.panes.is_empty() {
            return Err("a new holder instance holds panes".into());
        }
        self.killed = Some(std::mem::replace(&mut self.instance, listing.instance));
        self.kills += 1;
        for counter in &mut self.counters {
            counter.epoch += 1;
            counter.last = None;
        }
        self.bind(RESTORE)
            .map_err(|e| format!("the daemon did not resume every session: {e}"))?;
        let took = killed.elapsed().as_secs();
        self.note(
            "holder_kill",
            format!(
                "SIGKILL, a new instance with no panes; the daemon resumed {} sessions in it in {took} s, the counters in the next epoch",
                self.counters.len() + 1
            ),
        );
        Ok(at)
    }

    // -- the hooks -----------------------------------------------------------

    /// The hooks after `event`, which started after event `seq` at `at`.
    fn hooks(&mut self, event: Event, seq: i64, at: Timestamp) -> Result<(), String> {
        match event {
            Event::DaemonRestart => self.daemon_reconnect(seq, at)?,
            Event::Upgrade => self.upgrade_observed_by_daemon(seq, at)?,
            Event::HolderKill => self.holder_kill_ladder(seq, at)?,
        }
        self.delivery_counts(true)
    }

    fn passed(&mut self, hook: &'static str) {
        *self.passed.entry(hook).or_default() += 1;
    }

    /// The stand-in's submitted lines, in order.
    fn submits(&self) -> Vec<String> {
        let log = self
            .world
            .dir
            .path()
            .join("fake-agent")
            .join(format!("{}.jsonl", self.orchestrator));
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|e| e["event"] == "submit")
            .filter_map(|e| e["text"].as_str().map(str::to_owned))
            .collect()
    }

    /// Takes in every message the outbox holds for the orchestrator (the
    /// daemon sends some of its own: a note when a worker starts), then
    /// does what Claude Code does with each new submitted line: runs it,
    /// `tiller inbox <id>`, which acks that message and drains the rest of
    /// its batch (protocol 3.3). The daemon types a batch's head again when
    /// a message joins it before the ack, so a line that names an acked
    /// message is skipped when the log held it a settle after the take
    /// that acked it. A line that is not one wake line, or that names an
    /// acked message and came later, is a delivery that typed twice.
    fn take_submits(&mut self) -> Result<(), String> {
        let ids: Vec<String> = {
            let store = self.world.store();
            let mut statement = store
                .conn()
                .prepare(
                    "SELECT id FROM outbox WHERE target_session_id = ?1 ORDER BY created_at, id",
                )
                .unwrap();
            statement
                .query_map([&self.orchestrator], |row| row.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        for id in ids {
            if !self.messages.iter().any(|m| m.id == id) {
                self.messages.push(Message {
                    id,
                    acked_by: None,
                    typed_before: 0,
                });
            }
        }
        let submits = self.submits();
        let mut acked = Vec::new();
        for (n, submit) in submits.iter().enumerate().skip(self.submits_taken) {
            let id = submit
                .strip_prefix("tiller inbox ")
                .and_then(|rest| rest.split_whitespace().next())
                .filter(|_| submit.matches("tiller inbox").count() == 1 && !submit.contains('\n'))
                .ok_or_else(|| format!("a submit is not one wake line: {submit:?}"))?;
            if let Some(Message {
                acked_by: Some(before),
                typed_before,
                ..
            }) = self.messages.iter().find(|m| m.id == id)
            {
                if n >= *typed_before {
                    return Err(format!(
                        "message {id} was typed again after submit {before} took it: {submit:?}"
                    ));
                }
                self.submits_taken = n + 1;
                continue;
            }
            let read = self.world.must_as(&self.orchestrator, &["inbox", id]);
            let batch: Vec<String> = read.data()["messages"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .filter_map(|m| m["id"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            for id in batch {
                match self.messages.iter_mut().find(|m| m.id == id) {
                    Some(Message {
                        acked_by: Some(before),
                        ..
                    }) => {
                        return Err(format!(
                            "message {id} came back in submit {n} after submit {before}"
                        ));
                    }
                    Some(message) => {
                        message.acked_by = Some(n);
                        message.typed_before = usize::MAX;
                    }
                    None => self.messages.push(Message {
                        id: id.clone(),
                        acked_by: Some(n),
                        typed_before: usize::MAX,
                    }),
                }
                acked.push(id);
            }
            self.submits_taken = n + 1;
        }
        if !acked.is_empty() {
            std::thread::sleep(ACK_SETTLE);
            let typed_before = self.submits().len();
            for message in &mut self.messages {
                if acked.contains(&message.id) {
                    message.typed_before = typed_before;
                }
            }
        }
        Ok(())
    }

    /// Why `waiting` is still unacked, as the store tells it: each
    /// message's state and every deferral, the orchestrator's session row
    /// and the end of the daemon's log.
    fn undelivered(&self, waiting: &[&str]) -> String {
        let store = self.world.store();
        let mut out = String::new();
        for id in waiting {
            let (state, next): (String, Option<String>) = store
                .conn()
                .query_row(
                    "SELECT state, next_attempt_at FROM outbox WHERE id = ?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or_default();
            out.push_str(&format!("\n  {id} {state}, next attempt {next:?}"));
            let mut statement = store
                .conn()
                .prepare(
                    "SELECT at, kind, payload FROM events \
                     WHERE entity = 'outbox' AND entity_id = ?1 ORDER BY seq",
                )
                .unwrap();
            let events = statement
                .query_map([id], |row| {
                    Ok(format!(
                        "{} {} {}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?
                    ))
                })
                .unwrap()
                .flatten();
            for event in events {
                out.push_str(&format!("\n    {event}"));
            }
        }
        let session: String = store
            .conn()
            .query_row(
                "SELECT coalesce(pane_id, '-') || ' ' || activity || ' ' || \
                 coalesce(activity_observed_at, '-') FROM sessions WHERE id = ?1",
                [&self.orchestrator],
                |row| row.get(0),
            )
            .unwrap_or_default();
        out.push_str(&format!("\n  orchestrator pane, activity: {session}"));
        let log = std::fs::read_to_string(daemon_log_path(&self.world)).unwrap_or_default();
        let lines: Vec<&str> = log.lines().collect();
        out.push_str("\n  daemon log:");
        for line in &lines[lines.len().saturating_sub(40)..] {
            out.push_str(&format!("\n    {line}"));
        }
        out
    }

    /// Every message to the orchestrator so far was delivered and taken by
    /// exactly one submitted line, and no line named an acked message
    /// later than a settle after its ack. With
    /// `next`, sends the next message.
    fn delivery_counts(&mut self, next: bool) -> Result<(), String> {
        let deadline = Instant::now() + DELIVER;
        loop {
            self.take_submits()?;
            let waiting: Vec<&str> = self
                .messages
                .iter()
                .filter(|m| m.acked_by.is_none())
                .map(|m| m.id.as_str())
                .collect();
            if waiting.is_empty() {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "after {} s no submit took {}",
                    DELIVER.as_secs(),
                    self.undelivered(&waiting)
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if let Some(message) = self.messages.iter().find(|m| {
            self.count(
                "SELECT count(*) FROM outbox WHERE id = ?1 AND state = 'acked'",
                [&m.id],
            ) != 1
        }) {
            return Err(format!("message {} is not acked", message.id));
        }
        if next {
            let n = self.sent + 1;
            let sent = self.world.must(&[
                "send",
                &self.orchestrator,
                "--kind",
                "steer",
                "--text",
                &format!("soak message {n}"),
            ]);
            let id = sent.data()["id"].as_str().unwrap().to_owned();
            self.sent = n;
            self.messages.push(Message {
                id,
                acked_by: None,
                typed_before: 0,
            });
        }
        self.passed("delivery_counts");
        Ok(())
    }

    /// The holder poller's health row, once it has succeeded after `at`
    /// with no failure.
    fn poller_after(&self, at: Timestamp) -> Result<(), String> {
        let port = self.port()?;
        until(WAIT, || {
            let health = get(port, &self.cookie, "/api/v1/health");
            let poller = health["pollers"]
                .as_array()
                .and_then(|p| p.iter().find(|p| p["source"] == "holder"))
                .ok_or_else(|| format!("no holder poller: {health}"))?;
            let success = timestamp(&poller["last_success_at"]);
            if success.is_some_and(|s| s > at) && poller["consecutive_failures"] == 0 {
                Ok(())
            } else {
                Err(format!(
                    "the holder poller has not succeeded since {at}: {poller}"
                ))
            }
        })
    }

    /// The live `panes` rows of the current instance are the holder's
    /// panes, each with its pid.
    fn rows_match(&self) -> Result<(), String> {
        let listing = client(&self.world)?.list().map_err(|e| e.to_string())?;
        let held: BTreeSet<String> = listing
            .panes
            .iter()
            .map(|p| p.pane.as_str().to_owned())
            .collect();
        let store = self.world.store();
        let mut statement = store
            .conn()
            .prepare(
                "SELECT pane_id, pid FROM panes WHERE holder_instance = ?1 AND gone_at IS NULL",
            )
            .unwrap();
        let rows: BTreeMap<String, Option<i64>> = statement
            .query_map([&self.instance], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let listed: BTreeSet<String> = rows.keys().cloned().collect();
        if listed != held || rows.values().any(Option::is_none) {
            return Err(format!(
                "the panes rows {rows:?} are not the holder's panes {held:?}"
            ));
        }
        Ok(())
    }

    fn same_instance(&self) -> Result<(), String> {
        match self.meta("holder_instance") {
            Some(recorded) if recorded == self.instance => Ok(()),
            other => Err(format!(
                "meta.holder_instance is {other:?}, the holder's is {}",
                self.instance
            )),
        }
    }

    /// After a daemon restart: the same holder, the same panes, no R21.
    fn daemon_reconnect(&mut self, seq: i64, at: Timestamp) -> Result<(), String> {
        self.poller_after(at)?;
        self.same_instance()?;
        for kind in ["holder.lost", "session.resume_attempted"] {
            let new = self.events_since(kind, seq);
            if !new.is_empty() {
                return Err(format!("a daemon restart recorded {kind}: {new:?}"));
            }
        }
        until(WAIT, || self.rows_match())?;
        self.passed("daemon_reconnect");
        Ok(())
    }

    /// After a live upgrade: the daemon kept the instance and its panes,
    /// and refused nothing. The binary is the same, so the version is too,
    /// and `holder.upgraded` is not recorded (`holder.record`).
    fn upgrade_observed_by_daemon(&mut self, seq: i64, at: Timestamp) -> Result<(), String> {
        self.poller_after(at)?;
        self.same_instance()?;
        let upgraded = self.events_since("holder.upgraded", seq);
        if !upgraded.is_empty() {
            return Err(format!(
                "an upgrade to the same binary recorded holder.upgraded: {upgraded:?}"
            ));
        }
        let lost = self.events_since("holder.lost", seq);
        if !lost.is_empty() {
            return Err(format!("a live upgrade recorded holder.lost: {lost:?}"));
        }
        until(WAIT, || self.rows_match())?;
        let health = get(self.port()?, &self.cookie, "/api/v1/health");
        let refused: Vec<&Value> = health["warnings"]
            .as_array()
            .map(|w| {
                w.iter()
                    .filter(|w| {
                        w.as_str()
                            .is_some_and(|w| w.contains("holder upgrade refused"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !refused.is_empty() {
            return Err(format!("health warns: {refused:?}"));
        }
        self.passed("upgrade_observed_by_daemon");
        Ok(())
    }

    /// After the holder kill: R21 ran once, resumed every session and told
    /// the owner once.
    fn holder_kill_ladder(&mut self, seq: i64, at: Timestamp) -> Result<(), String> {
        let previous = self.meta("holder_instance");
        let mut sessions = vec![self.orchestrator.clone()];
        sessions.extend(self.counters.iter().map(|c| c.session.clone()));
        // R21 writes `holder.lost` last, on the pass after every session
        // bound.
        let lost = until(RESTORE, || {
            let lost = self.events_since("holder.lost", seq);
            if lost.is_empty() {
                Err("no holder.lost".to_owned())
            } else {
                Ok(lost)
            }
        })?;
        if lost.len() != 1 {
            return Err(format!(
                "the kill recorded {} holder.lost: {lost:?}",
                lost.len()
            ));
        }
        let payload = &lost[0]["payload"];
        if payload["instance"] != self.instance.as_str()
            || payload["previous"].as_str() != self.killed.as_deref()
        {
            return Err(format!(
                "holder.lost names {payload}, the kill went from {:?} to {} (meta was {previous:?})",
                self.killed, self.instance
            ));
        }
        self.same_instance()?;
        let attempts = self.events_since("session.resume_attempted", seq);
        for session in &sessions {
            if !attempts.iter().any(|a| a["entity"] == session.as_str()) {
                return Err(format!(
                    "no session.resume_attempted for {session}: {attempts:?}"
                ));
            }
        }
        until(WAIT, || self.rows_match())?;
        for session in &sessions {
            let pane = self.pane_of(session).unwrap_or_default();
            let on: Option<String> = self
                .world
                .store()
                .conn()
                .query_row(
                    "SELECT holder_instance FROM panes WHERE pane_id = ?1 AND gone_at IS NULL",
                    [&pane],
                    |row| row.get(0),
                )
                .ok()
                .flatten();
            if on.as_deref() != Some(self.instance.as_str()) {
                return Err(format!(
                    "session {session}'s pane {pane} has no live row on the new instance: {on:?}"
                ));
            }
        }
        let machines = get(self.port()?, &self.cookie, "/api/v1/machines");
        let restarted = timestamp(&machines["machines"][0]["holder_restarted_at"]);
        if !restarted.is_some_and(|r| r.as_second() >= at.as_second() - 1) {
            return Err(format!(
                "the Machines row does not show the restart after {at}: {machines}"
            ));
        }
        let notes: Vec<Value> = std::fs::read_to_string(self.world.dir.path().join("notified"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|note| {
                note["title"]
                    .as_str()
                    .is_some_and(|t| t.contains("terminal holder"))
            })
            .collect();
        let resumed = format!("{0} of {0} agents resumed", sessions.len());
        if notes.len() != 1 || notes[0]["message"] != resumed.as_str() {
            return Err(format!(
                "the owner was told {notes:?}, not once that {resumed}"
            ));
        }
        self.passed("holder_kill_ladder");
        Ok(())
    }

    /// The whole run: the timer, the schedule, the counters. Every event
    /// fires, even when one ran past the planned end.
    fn run(&mut self) {
        let events = schedule(&self.plan);
        self.note(
            "start",
            format!(
                "{} s, an orchestrator and {} workers, {} events",
                self.plan.duration.as_secs(),
                self.plan.workers,
                events.len()
            ),
        );
        let mut next_event = 0;
        let mut next_poll = self.plan.poll;
        self.started = Instant::now();
        let outcome = loop {
            let now = self.since();
            if now >= self.plan.duration && next_event == events.len() {
                break self.settle().and_then(|()| self.delivery_counts(false));
            }
            let result = match events.get(next_event) {
                Some((at, event)) if now >= *at => {
                    next_event += 1;
                    self.event(*event)
                }
                _ if now >= next_poll => {
                    next_poll = now + self.plan.poll;
                    self.check()
                }
                _ => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            self.save("running");
            if let Err(error) = result {
                break Err(error);
            }
        };
        match outcome {
            Ok(()) => self.note("end", "no gap, every hook passed"),
            Err(error) => {
                self.note("failed", error.clone());
                self.failure = Some(error);
            }
        }
        self.keep_home();
        self.save(if self.failure.is_some() {
            "failed"
        } else {
            "passed"
        });
    }

    /// At the end, copies what a diagnosis reads into `home/` of the
    /// report directory: the state database, the holder's log and the
    /// stand-in's logs. The test home itself is removed with the test.
    fn keep_home(&self) {
        let kept = self.out.join("home");
        let _ = std::fs::create_dir_all(kept.join("fake-agent"));
        let db = self.world.db();
        for suffix in ["", "-wal", "-shm"] {
            let from = PathBuf::from(format!("{}{suffix}", db.display()));
            let _ = std::fs::copy(&from, kept.join(format!("state.db{suffix}")));
        }
        let _ = std::fs::copy(log_path(&self.world), kept.join("holder.log"));
        let _ = std::fs::copy(daemon_log_path(&self.world), kept.join("daemon.log"));
        let home = self.world.home();
        for name in ["log/hook.log", "config.toml"] {
            let _ = std::fs::copy(home.join(name), kept.join(name.replace('/', "-")));
        }
        for entry in std::fs::read_dir(home.join("hook-spool"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let _ = std::fs::copy(entry.path(), kept.join(entry.file_name()));
        }
        let logs = self.world.dir.path().join("fake-agent");
        for entry in std::fs::read_dir(logs).into_iter().flatten().flatten() {
            let _ = std::fs::copy(
                entry.path(),
                kept.join("fake-agent").join(entry.file_name()),
            );
        }
    }

    // -- the result ----------------------------------------------------------

    fn result(&self, status: &str) -> Value {
        let lisbon = |ts: Timestamp| tiller_core::time::lisbon::display(ts);
        json!({
            "status": status,
            "started": lisbon(self.started_at),
            "now": lisbon(Timestamp::now()),
            "elapsed_secs": self.since().as_secs(),
            "planned_secs": self.plan.duration.as_secs(),
            "events": {
                "daemon_restarts": self.restarts,
                "upgrades": self.upgrades,
                "holder_kills": self.kills,
            },
            "checks": self.checks,
            "messages": {
                "sent": self.messages.len(),
                "delivered_once": self.messages.iter().filter(|m| m.acked_by.is_some()).count(),
            },
            "gaps": u64::from(self.failure.as_deref().is_some_and(|f| f.starts_with("gap"))),
            "failure": self.failure,
            "panes": self.counters.iter().map(|c| json!({
                "key": c.key,
                "session": c.session,
                "epoch": c.epoch,
                "last": c.last,
                "numbers_verified": c.verified,
                "reads": c.reads,
            })).collect::<Vec<_>>(),
            "log": self.log.iter().map(|l| json!({
                "at_secs": l.at.as_secs(), "kind": l.kind, "note": l.note,
            })).collect::<Vec<_>>(),
            "hooks": HOOKS.iter().map(|h| json!({
                "name": h.name,
                "after": h.after,
                "checks": h.checks,
                "passed": self.passed.get(h.name).copied().unwrap_or(0),
            })).collect::<Vec<_>>(),
        })
    }

    /// Writes the JSON result and the Markdown report.
    fn save(&self, status: &str) {
        let result = self.result(status);
        if std::fs::create_dir_all(&self.out).is_err() {
            return;
        }
        let _ = std::fs::write(
            self.out.join("soak-result.json"),
            serde_json::to_string_pretty(&result).unwrap(),
        );
        let _ = std::fs::write(self.out.join("soak-report.md"), self.markdown(&result));
    }

    fn markdown(&self, result: &Value) -> String {
        let mut md = format!(
            "# Terminal holder soak\n\nResult: **{}**. Started {}, ran {} s of {} s.\n\n",
            result["status"].as_str().unwrap_or(""),
            result["started"].as_str().unwrap_or(""),
            result["elapsed_secs"],
            result["planned_secs"],
        );
        if let Some(failure) = &self.failure {
            md.push_str(&format!("Failure: `{failure}`\n\n"));
        }
        md.push_str(&format!(
            "Events: {} daemon restarts, {} live upgrades, {} holder kill(s). {} counter checks. {} messages sent, {} delivered once.\n\n",
            self.restarts,
            self.upgrades,
            self.kills,
            self.checks,
            self.messages.len(),
            self.messages.iter().filter(|m| m.acked_by.is_some()).count()
        ));
        md.push_str("| Worker | Epoch | Last number | Verified | Reads |\n|---|---|---|---|---|\n");
        for c in &self.counters {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                c.key,
                c.epoch,
                c.last.map_or("-".into(), |n| n.to_string()),
                c.verified,
                c.reads
            ));
        }
        md.push_str(
            "\nThe holder kill ends an epoch: its panes are gone, and numbers written after the last read are lost with them (spec 6.4). The daemon resumes each session in a new pane (R21).",
        );
        if self.failure.is_none() {
            md.push_str(" Inside an epoch, no number is missing or repeated.");
        }
        md.push_str("\n\n");
        md.push_str("## Events\n\n");
        for l in &self.log {
            md.push_str(&format!(
                "- {:>6} s - {}: {}\n",
                l.at.as_secs(),
                l.kind,
                l.note
            ));
        }
        md.push_str("\n## Hooks\n\n");
        for h in HOOKS {
            md.push_str(&format!(
                "- `{}` - passed {} times, after {}. Checks {}.\n",
                h.name,
                self.passed.get(h.name).copied().unwrap_or(0),
                h.after,
                h.checks
            ));
        }
        md
    }
}

/// Runs one soak and returns its failure, if any.
fn soak(plan: Plan, out: &Path) -> Result<Value, String> {
    let mut soak = Soak::new(plan, out.to_owned())?;
    soak.run();
    let result = soak.result(if soak.failure.is_some() {
        "failed"
    } else {
        "passed"
    });
    match soak.failure {
        Some(error) => Err(error),
        None => Ok(result),
    }
}

/// Where a run writes its report: `TILLER_SOAK_OUT`, else under the test
/// target directory.
fn out_dir(name: &str) -> PathBuf {
    std::env::var_os("TILLER_SOAK_OUT").map_or_else(
        || {
            Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join("soak")
                .join(name)
        },
        PathBuf::from,
    )
}

/// The `wall-clock` group runs a soak alone. Under nextest the test refuses
/// to run outside it.
fn require_wall_clock_group() {
    if std::env::var_os("NEXTEST_RUN_ID").is_some() {
        assert_eq!(
            std::env::var("NEXTEST_TEST_GROUP").as_deref(),
            Ok("wall-clock"),
            "the nextest override that runs this test alone no longer matches it"
        );
    }
}

/// Every hook passed as often as its events fired.
fn assert_hooks(result: &Value) {
    let events = &result["events"];
    let fired = events["daemon_restarts"].as_u64().unwrap()
        + events["upgrades"].as_u64().unwrap()
        + events["holder_kills"].as_u64().unwrap();
    for hook in result["hooks"].as_array().unwrap() {
        let want = match hook["name"].as_str().unwrap() {
            "delivery_counts" => fired + 1,
            "daemon_reconnect" => events["daemon_restarts"].as_u64().unwrap(),
            "upgrade_observed_by_daemon" => events["upgrades"].as_u64().unwrap(),
            "holder_kill_ladder" => events["holder_kills"].as_u64().unwrap(),
            other => panic!("an unknown hook {other}"),
        };
        assert_eq!(hook["passed"], want, "{hook}");
    }
    assert_eq!(
        result["messages"]["sent"], result["messages"]["delivered_once"],
        "{}",
        result["messages"]
    );
}

// -- the tests -------------------------------------------------------------------

/// Section 13, row H14, scaled to about two minutes plus the resume after
/// the kill: daemon restarts, 3 live upgrades and a holder kill over agent
/// sessions, with no gap and every hook passed. CI runs it.
#[test]
fn a_short_soak_has_no_gap() {
    require_wall_clock_group();
    let secs = std::env::var("TILLER_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let result = soak(Plan::of(secs), &out_dir("short")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(result["events"]["upgrades"], 3);
    assert_eq!(result["events"]["holder_kills"], 1);
    assert!(result["events"]["daemon_restarts"].as_u64().unwrap() >= 4);
    assert_eq!(result["gaps"], 0);
    assert_hooks(&result);
    for pane in result["panes"].as_array().unwrap() {
        assert!(
            pane["numbers_verified"].as_u64().unwrap() > 50,
            "pane read too little: {pane}"
        );
    }
}

/// Section 13, row H14, for the owner's 4 h run. Ignored: `scripts/soak.sh`
/// runs it, and `TILLER_SOAK_SECS` sets the length (default 4 h).
#[test]
#[ignore = "the 4 h soak of H14: scripts/soak.sh"]
fn the_long_soak() {
    require_wall_clock_group();
    let secs = std::env::var("TILLER_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4 * 3600);
    let result = soak(Plan::of(secs), &out_dir("long")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(result["gaps"], 0);
    assert_hooks(&result);
}

/// The gap check works: a worker that skips one number fails the soak, and
/// the failure names the gap.
#[test]
fn a_skipped_number_fails_the_soak() {
    require_wall_clock_group();
    let mut plan = Plan::of(20);
    plan.skip_at = Some(10);
    plan.poll = Duration::from_secs(2);
    plan.restarts = 0;
    let error = soak(plan, &out_dir("injected")).expect_err("a skipped number must fail the soak");
    assert!(
        error.contains("gap in soak-1 e1: 9 is followed by 11"),
        "{error}"
    );
}

#[test]
fn the_counter_check_finds_a_missing_repeated_and_reset_number() {
    let lines = |numbers: &[u64]| {
        numbers
            .iter()
            .map(|n| format!("count {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // The newest line is left for the next read.
    let mut counter = Counter::new("pane");
    counter.verify(&lines(&[0, 1, 2, 3])).unwrap();
    assert_eq!((counter.last, counter.verified), (Some(2), 3));
    // The next read overlaps and goes on.
    counter.verify(&lines(&[1, 2, 3, 4, 5])).unwrap();
    assert_eq!(counter.last, Some(4));
    // A read that lands inside the newest line sets aside only that line.
    counter
        .verify(&format!("{}\ncount", lines(&[3, 4, 5])))
        .unwrap();
    assert_eq!((counter.last, counter.verified), (Some(5), 6));

    for (numbers, wants) in [
        (vec![0, 1, 3, 4], "1 is followed by 3"),
        (vec![0, 1, 1, 2], "repeat"),
        (vec![8, 9, 10], "starts at 8"),
        (vec![1, 2, 3], "ends at 2"),
        (vec![3, 4, 5], "first read starts at 3"),
    ] {
        let mut counter = Counter::new("pane");
        counter.last = match wants {
            w if w.starts_with("first") => None,
            w if w.contains("starts") => Some(5),
            _ => Some(4),
        };
        let error = counter
            .verify(&lines(&numbers))
            .expect_err(&format!("{numbers:?}"));
        assert!(error.contains(wants), "{numbers:?}: {error}");
    }
    // Other text in the pane is not the counter's.
    let mut counter = Counter::new("pane");
    counter
        .verify("fake-agent --counter\ncounted 7\n  count 0\ncount 1\ncount 2")
        .unwrap();
    assert_eq!(counter.last, Some(1));
}

#[test]
fn the_schedule_holds_three_upgrades_and_a_kill_and_keeps_events_apart() {
    for secs in [20, 120, 720, 4 * 3600] {
        let plan = Plan::of(secs);
        let events = schedule(&plan);
        let count = |e| events.iter().filter(|(_, event)| *event == e).count();
        assert_eq!(count(Event::Upgrade), 3, "{secs}");
        assert_eq!(count(Event::HolderKill), 1, "{secs}");
        assert_eq!(count(Event::DaemonRestart), plan.restarts, "{secs}");
        assert!(events.windows(2).all(|w| w[0].0 < w[1].0), "{secs}");
        assert!(events.iter().all(|(at, _)| *at < plan.duration), "{secs}");
    }
}
