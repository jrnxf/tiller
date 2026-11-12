//! The terminal holder's end-to-end soak (`terminal-holder.md` 9.2, 9.3 and
//! 13, row H14): a test home with counter panes, and over a set time a
//! schedule of daemon restarts, three live upgrades (`tiller holder
//! upgrade`) and one holder kill. After every event, and on a timer, each
//! pane's counter is read back from the holder's screen and scrollback and
//! must have no gap and no repeated number.
//!
//! # The counters
//!
//! Each pane runs a shell loop that writes `SEQ <key> e<epoch> <n>`, with
//! `n` up by one each time. The harness keeps, per pane, the last number it
//! verified. A read of the pane's scrollback must then hold every number
//! from that one on (from 1 in a new epoch), once and in order. A number
//! that is missing, repeated or older than the last one is a gap, and the
//! soak stops with it. The newest line of a read is left for the next read,
//! because a read may land inside a line. A pane that makes no progress
//! within `STALL` after an event is a gap too. The holder kill is the one
//! event that ends an epoch: its panes are gone (spec 6.4), so the harness
//! starts new panes with the next epoch, as the ladder resumes a session.
//! The numbers the old epoch wrote after the last read are lost with the
//! holder, and the report says so.
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
//! The script runs `cargo nextest run -p tiller-cli --test soak
//! --run-ignored only the_long_soak`, with `TILLER_SOAK_SECS` for the length
//! and `TILLER_SOAK_OUT` for the report directory. It writes
//! `soak-result.json` (machine readable, rewritten after every event so a
//! stopped run leaves its progress) and `soak-report.md`.
//!
//! # Hooks for H14
//!
//! A part that needs work not yet on `main` has a marked hook (`H14 HOOK`),
//! listed in [`HOOKS`] and in every report as `pending`. The harness does
//! not fake a result for one. Until H09 and H10 land, the daemon of this
//! build does not talk to the holder, so a daemon restart proves that the
//! holder and its panes ignore a daemon that comes and goes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{Running, World, git_only_daemon};
use serde_json::{Value, json};
use tiller_panes::{HolderClient, Meta, Pane, PaneState, Panes, Read, Spawn, socket_path};

const TILLER: &str = env!("CARGO_BIN_EXE_tiller");

/// How long a step waits for the holder to answer.
const WAIT: Duration = Duration::from_secs(20);

/// How long a pane may go without a new number after an event.
const STALL: Duration = Duration::from_secs(15);

/// The scrollback a read takes: all the holder keeps (spec 2.8).
const WINDOW: u16 = 10_000;

/// A part of the soak that waits for work not on `main` yet.
struct Hook {
    name: &'static str,
    waits_for: &'static str,
    h14_fills_in: &'static str,
}

/// Every hook H14 fills in. Each one is called from `Soak::hooks`, and each
/// report lists them as `pending`.
const HOOKS: &[Hook] = &[
    Hook {
        name: "delivery_counts",
        waits_for: "H09 (agent delivery on holder panes)",
        h14_fills_in: "send numbered messages to each pane through the outbox, and after every event compare the messages sent, the lines the pane echoed and the outbox's delivered count: each message once",
    },
    Hook {
        name: "daemon_reconnect",
        waits_for: "H10 (HolderSource in the daemon)",
        h14_fills_in: "after a daemon restart, check that the daemon reconnected to the same holder instance, listed the same panes, recorded no R21 and typed nothing twice",
    },
    Hook {
        name: "upgrade_observed_by_daemon",
        waits_for: "H10 (HolderSource in the daemon)",
        h14_fills_in: "after an upgrade, check that the daemon recorded holder.upgraded with both versions and kept its panes cache",
    },
    Hook {
        name: "holder_kill_ladder",
        waits_for: "H10 (reconcile, ladder, R21)",
        h14_fills_in: "after the holder kill, let the daemon see the new instance, run the ladder and resume the sessions (R21) in place of Soak::respawn, and check the one notification",
    },
];

// -- the plan ------------------------------------------------------------------

/// What one soak run does.
#[derive(Clone)]
struct Plan {
    duration: Duration,
    panes: usize,
    /// The pause between two numbers a pane writes.
    rate: Duration,
    /// How often the counters are read between events.
    poll: Duration,
    /// Daemon restarts, spread over the run.
    restarts: usize,
    /// A test fault: the first pane skips a number.
    skip_at: Option<u64>,
}

impl Plan {
    /// The plan for a run of `secs` seconds: panes, rate and polls scale
    /// with it, and the events always include 3 upgrades and 1 kill.
    fn of(secs: u64) -> Self {
        let long = secs >= 600;
        Self {
            duration: Duration::from_secs(secs),
            panes: if long { 4 } else { 3 },
            rate: Duration::from_millis(if long { 1_000 } else { 200 }),
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

/// One pane's counter, as the harness has verified it.
#[derive(Clone, Debug)]
struct Counter {
    key: String,
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
            epoch: 1,
            last: None,
            verified: 0,
            reads: 0,
        }
    }

    /// The numbers this counter's pane wrote in `text`, in the order they
    /// stand, from the lines of this epoch.
    fn numbers(&self, text: &str) -> Vec<u64> {
        let epoch = format!("e{}", self.epoch);
        text.lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                (fields.next()? == "SEQ" && fields.next()? == self.key && fields.next()? == epoch)
                    .then(|| fields.next()?.parse().ok())
                    .flatten()
            })
            .collect()
    }

    /// Checks a read of the pane's scrollback against what was verified
    /// before, and takes it in. The newest line of text is not taken: a
    /// read may land inside it. An epoch's first read starts at 1.
    fn verify(&mut self, text: &str) -> Result<(), String> {
        let text = text.trim_end();
        let complete = text.rfind('\n').map_or("", |end| &text[..end]);
        let numbers = self.numbers(complete);
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
        if self.last.is_none() && first != 1 {
            return Err(format!(
                "gap in {} e{}: the first read starts at {first}, not 1",
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
        self.verified += last - self.last.unwrap_or(0);
        self.reads += 1;
        self.last = Some(last);
        Ok(())
    }
}

// -- the holder and the panes ------------------------------------------------------

/// The shell loop of one counter pane.
fn counter_script(key: &str, epoch: u32, rate: Duration, skip_at: Option<u64>) -> String {
    format!(
        r#"i=1; while :; do echo "SEQ {key} e{epoch} $i"; i=$((i + 1)); [ "$i" = "{skip}" ] && i=$((i + 1)); sleep {rate}; done"#,
        skip = skip_at.unwrap_or(0),
        rate = rate.as_secs_f64()
    )
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

/// A new client, once the holder listens.
fn client(world: &World) -> Result<HolderClient, String> {
    let socket = socket_path(&world.home());
    let deadline = Instant::now() + WAIT;
    loop {
        match HolderClient::connect(&socket, "daemon") {
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

// -- the report ------------------------------------------------------------------

struct Logged {
    at: Duration,
    kind: &'static str,
    note: String,
}

struct Soak {
    world: World,
    plan: Plan,
    out: PathBuf,
    holder: Holder,
    daemon: Option<(Running, u16)>,
    counters: Vec<Counter>,
    /// The holder instance and each pane's pid, as the last event left them.
    instance: String,
    pids: BTreeMap<String, u32>,
    upgrades: usize,
    restarts: usize,
    kills: usize,
    checks: u64,
    started: Instant,
    started_at: jiff::Timestamp,
    log: Vec<Logged>,
    failure: Option<String>,
}

impl Soak {
    fn new(plan: Plan, out: PathBuf) -> Result<Self, String> {
        let world = World::initialized();
        let holder = Holder::start(&world);
        let client = client(&world)?;
        let instance = client.list().map_err(|e| e.to_string())?.instance;
        let daemon = Some(git_only_daemon(&world));
        let mut soak = Self {
            world,
            plan,
            out,
            holder,
            daemon,
            counters: Vec::new(),
            instance,
            pids: BTreeMap::new(),
            upgrades: 0,
            restarts: 0,
            kills: 0,
            checks: 0,
            started: Instant::now(),
            started_at: jiff::Timestamp::now(),
            log: Vec::new(),
            failure: None,
        };
        for n in 1..=soak.plan.panes {
            soak.counters.push(Counter::new(&format!("soak-{n}")));
        }
        soak.respawn(&client)?;
        Ok(soak)
    }

    /// Starts every counter pane in its current epoch. In place of the
    /// ladder's resume after a holder loss (spec 6.4).
    // H14 HOOK holder_kill_ladder: after the kill, the daemon resumes the
    // sessions. Until H10 lands, the harness starts the panes itself.
    fn respawn(&mut self, client: &HolderClient) -> Result<(), String> {
        for (n, counter) in self.counters.iter_mut().enumerate() {
            counter.last = None;
            let skip = (n == 0).then_some(self.plan.skip_at).flatten();
            let pane = client
                .spawn(&Spawn {
                    key: format!("{}-e{}", counter.key, counter.epoch),
                    argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        counter_script(&counter.key, counter.epoch, self.plan.rate, skip),
                    ],
                    cwd: self.world.dir.path().display().to_string(),
                    env: BTreeMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
                    size: None,
                    meta: Meta::default(),
                })
                .map_err(|e| format!("spawn {}: {e}", counter.key))?;
            self.pids.insert(counter.key.clone(), pane.pid);
        }
        Ok(())
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

    /// The panes of the holder, by counter key.
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
            let key = format!("{}-e{}", counter.key, counter.epoch);
            let pane = listing
                .panes
                .iter()
                .find(|pane| pane.key == key)
                .ok_or_else(|| format!("pane {key} is gone from the holder"))?;
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
            if self.pids.get(&counter.key) != Some(&pane.pid) {
                return Err(format!(
                    "pane {} changed pid from {:?} to {}",
                    counter.key,
                    self.pids.get(&counter.key),
                    pane.pid
                ));
            }
            let text = client
                .screen(&pane.pane, Read::Recent(WINDOW), tiller_panes::Format::Text)
                .map_err(|e| format!("screen {}: {e}", counter.key))?
                .text();
            counter.verify(&text)?;
        }
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
            std::thread::sleep(self.plan.rate.max(Duration::from_millis(250)));
        }
    }

    fn event(&mut self, event: Event) -> Result<(), String> {
        // The state just before, so a number written during the event is
        // read after it.
        self.check()?;
        match event {
            Event::DaemonRestart => self.daemon_restart()?,
            Event::Upgrade => self.upgrade()?,
            Event::HolderKill => self.holder_kill()?,
        }
        self.settle()?;
        self.hooks(event)
    }

    fn daemon_restart(&mut self) -> Result<(), String> {
        let (mut daemon, _) = self.daemon.take().ok_or("no daemon to restart")?;
        // Every other restart is a crash.
        let hard = self.restarts % 2 == 1;
        if hard {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        drop(daemon);
        self.daemon = Some(git_only_daemon(&self.world));
        self.restarts += 1;
        self.note(
            "daemon_restart",
            if hard {
                "SIGKILL, then a new daemon"
            } else {
                "SIGTERM, then a new daemon"
            },
        );
        Ok(())
    }

    fn upgrade(&mut self) -> Result<(), String> {
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
        if self.holder.running.try_wait().ok().flatten().is_some() {
            return Err("the holder process exited in the upgrade".into());
        }
        self.note(
            "upgrade",
            format!("live upgrade {} of 3, same pid and instance", self.upgrades),
        );
        Ok(())
    }

    fn holder_kill(&mut self) -> Result<(), String> {
        let _ = self.holder.running.kill();
        let _ = self.holder.running.wait();
        self.holder = Holder::start(&self.world);
        let client = client(&self.world)?;
        let listing = client.list().map_err(|e| e.to_string())?;
        if listing.instance == self.instance {
            return Err("the holder came back with the same instance after a kill".into());
        }
        if !listing.panes.is_empty() {
            return Err("a new holder instance holds panes".into());
        }
        self.instance = listing.instance;
        self.kills += 1;
        for counter in &mut self.counters {
            counter.epoch += 1;
        }
        self.respawn(&client)?;
        self.note(
            "holder_kill",
            "SIGKILL, a new instance with no panes, the counter panes started again in the next epoch",
        );
        Ok(())
    }

    /// The marked hooks. Each one is pending until its work is on `main`.
    fn hooks(&mut self, event: Event) -> Result<(), String> {
        // H14 HOOK delivery_counts (H09): after every event, each message
        // sent to a pane was delivered once.
        // H14 HOOK daemon_reconnect (H10): after Event::DaemonRestart.
        // H14 HOOK upgrade_observed_by_daemon (H10): after Event::Upgrade.
        // H14 HOOK holder_kill_ladder (H10): after Event::HolderKill.
        let _ = event;
        Ok(())
    }

    /// The whole run: the timer, the schedule, the counters.
    fn run(&mut self) {
        let events = schedule(&self.plan);
        self.note(
            "start",
            format!(
                "{} s, {} panes, {} events",
                self.plan.duration.as_secs(),
                self.plan.panes,
                events.len()
            ),
        );
        let mut next_event = 0;
        let mut next_poll = self.plan.poll;
        self.started = Instant::now();
        let outcome = loop {
            let now = self.since();
            if now >= self.plan.duration {
                break self.settle();
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
            Ok(()) => self.note("end", "no gap"),
            Err(error) => {
                self.note("failed", error.clone());
                self.failure = Some(error);
            }
        }
        self.save(if self.failure.is_some() {
            "failed"
        } else {
            "passed"
        });
    }

    // -- the result ----------------------------------------------------------

    fn result(&self, status: &str) -> Value {
        let lisbon = |ts: jiff::Timestamp| tiller_core::time::lisbon::display(ts);
        json!({
            "status": status,
            "started": lisbon(self.started_at),
            "now": lisbon(jiff::Timestamp::now()),
            "elapsed_secs": self.since().as_secs(),
            "planned_secs": self.plan.duration.as_secs(),
            "events": {
                "daemon_restarts": self.restarts,
                "upgrades": self.upgrades,
                "holder_kills": self.kills,
            },
            "checks": self.checks,
            "gaps": u64::from(self.failure.as_deref().is_some_and(|f| f.starts_with("gap"))),
            "failure": self.failure,
            "panes": self.counters.iter().map(|c| json!({
                "key": c.key,
                "epoch": c.epoch,
                "last": c.last,
                "numbers_verified": c.verified,
                "reads": c.reads,
            })).collect::<Vec<_>>(),
            "log": self.log.iter().map(|l| json!({
                "at_secs": l.at.as_secs(), "kind": l.kind, "note": l.note,
            })).collect::<Vec<_>>(),
            "hooks": HOOKS.iter().map(|h| json!({
                "name": h.name, "status": "pending", "waits_for": h.waits_for,
                "h14_fills_in": h.h14_fills_in,
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
            "Events: {} daemon restarts, {} live upgrades, {} holder kill(s). {} counter checks.\n\n",
            self.restarts, self.upgrades, self.kills, self.checks
        ));
        md.push_str("| Pane | Epoch | Last number | Verified | Reads |\n|---|---|---|---|---|\n");
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
            "\nThe holder kill ends an epoch: its panes are gone, and numbers written after the last read are lost with them (spec 6.4).",
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
        md.push_str("\n## Hooks pending for H14\n\n");
        for h in HOOKS {
            md.push_str(&format!(
                "- `{}` - waits for {}. H14: {}\n",
                h.name, h.waits_for, h.h14_fills_in
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

// -- the tests -------------------------------------------------------------------

/// Section 13, row H14, scaled to about two minutes: daemon restarts, 3 live
/// upgrades and a holder kill over counter panes, with no gap. CI runs it.
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
}

/// The gap check works: a pane that skips one number fails the soak, and
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
            .map(|n| format!("SEQ pane e1 {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    // The newest line is left for the next read.
    let mut counter = Counter::new("pane");
    counter.verify(&lines(&[1, 2, 3, 4])).unwrap();
    assert_eq!((counter.last, counter.verified), (Some(3), 3));
    // The next read overlaps and goes on.
    counter.verify(&lines(&[2, 3, 4, 5, 6])).unwrap();
    assert_eq!(counter.last, Some(5));
    // A read that lands inside the newest line sets aside only that line.
    counter
        .verify(&format!("{}\nSEQ pane e", lines(&[4, 5, 6])))
        .unwrap();
    assert_eq!((counter.last, counter.verified), (Some(6), 6));

    for (numbers, wants) in [
        (vec![1, 2, 4, 5], "2 is followed by 4"),
        (vec![1, 2, 2, 3], "repeat"),
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
    // Other panes' and other epochs' lines are not this counter's.
    let mut counter = Counter::new("pane");
    counter
        .verify("SEQ other e1 1\nSEQ other e1 7\nSEQ pane e2 40\nSEQ pane e1 1\nSEQ pane e1 2\nSEQ pane e1 3")
        .unwrap();
    assert_eq!(counter.last, Some(2));
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
