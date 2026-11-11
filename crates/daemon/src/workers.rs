//! The daemon's background work, each piece a poller in one registry so
//! `/api/v1/health` shows its last success and last error: the herdr cache
//! (local and each saved machine), delivery, reconcile, the land loop, the
//! stop loop of finished runs, the transcript tailer and the quota feed.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use jiff::tz::TimeZone;
use tiller_agents::control::{self, Waits};
use tiller_agents::intake::{HostConfig, IntakeSource};
use tiller_agents::land::app::{APP_DIR, HostApp};
use tiller_agents::land::forge::GhForge;
use tiller_agents::land::publish::Published;
use tiller_agents::land::reconcile::{Local, ReconcileForge};
use tiller_agents::land::scrub::ScrubList;
use tiller_agents::land::{DaemonLock, Landed, Lander};
use tiller_agents::spawn::config::SpawnConfig;
use tiller_agents::spawn::pool::{Pool as _, Treehouse};
use tiller_agents::spawn::readings;
use tiller_core::backup::{self, Nightly};
use tiller_core::domain::{Host, HostKind, PipelineState, Session};
use tiller_core::state::list;
use tiller_core::state::memory_load::CopyMark;
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_delivery::notify::Notifier;
use tiller_delivery::outbox::Deliverer;
use tiller_delivery::sources::{Lavish, PageAnswers};
use tiller_herdr::client::{Cli, Herdr};
use tiller_herdr::poller::{HerdrSource, PollError, Registry, Source};
use tiller_observe::tailer::Tailer;
use tiller_reconcile::git::{GitScanner, WorktreePool};
use tiller_reconcile::sources::{
    Forge, HerdrLauncher, HerdrStatusProbe, HostReadings, NoHarness, Readings,
};
use tiller_reconcile::tools;
use tiller_reconcile::{Config as ReconcileConfig, Reconciler, Sources};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::owner::Gated;
use crate::readonly::Damage;

/// What the daemon reads from `<home>/config.toml` beyond intake.
#[derive(Clone, Debug)]
pub struct Settings {
    /// `quota_providers`, default `["claude"]` (`protocol.md` 8).
    pub quota_providers: Vec<String>,
    /// `forge_login` (CP2, C08).
    pub forge_login: Option<String>,
    /// `[learning]`, for the retro sweep (`docs/design/learning-loop.md`).
    pub learning: tiller_core::state::learning::Learning,
}

impl Settings {
    pub fn load(home: &Path) -> Self {
        let table = std::fs::read_to_string(home.join("config.toml"))
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
            .unwrap_or_default();
        let quota_providers = table
            .get("quota_providers")
            .and_then(toml::Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter(|list| !list.is_empty())
            .unwrap_or_else(|| vec!["claude".to_owned()]);
        let forge_login = table
            .get("forge_login")
            .and_then(toml::Value::as_str)
            .map(str::to_owned);
        let learning =
            tiller_core::state::learning::Learning::from_table(&table).unwrap_or_else(|error| {
                tracing::warn!(%error, "config.toml [learning] does not parse; using the defaults");
                tiller_core::state::learning::Learning::default()
            });
        Self {
            quota_providers,
            forge_login,
            learning,
        }
    }
}

/// How the workers reach the outside world.
#[derive(Clone, Debug)]
pub struct Tools {
    /// The local herdr server.
    pub herdr: Cli,
    /// `quota-axi`, with any leading arguments.
    pub quota: Vec<OsString>,
    /// The GitHub CLI program.
    pub gh: String,
    /// `lavish-axi`, with any leading arguments: the review page tool the
    /// page answers are read from (`protocol.md` 3.5).
    pub lavish: Vec<OsString>,
    /// The hostnames the review server answers to; every call to it carries
    /// them, since a call can be the one that starts it (P49).
    pub review_hosts: Vec<String>,
    /// The tool versions `GET /api/v1/health` serves: read once at start
    /// and again by the host check every five minutes (P43).
    pub versions: Arc<Mutex<serde_json::Value>>,
}

/// The host's boot id, when the OS reports one (R04).
pub fn boot_id() -> Option<String> {
    if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        return Some(id.trim().to_owned()).filter(|id| !id.is_empty());
    }
    let output = std::process::Command::new("sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .output()
        .ok()?;
    let id = String::from_utf8(output.stdout).ok()?;
    Some(id.trim().to_owned()).filter(|id| !id.is_empty())
}

// -- reconcile ------------------------------------------------------------------

/// The reconcile loop: one pass every 30 s (`recovery.md` section 3).
pub struct Reconcile(Reconciler);

impl Source for Reconcile {
    fn name(&self) -> String {
        "reconcile".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        let report = self.0.pass(now)?;
        if report.errors.is_empty() {
            Ok(())
        } else {
            Err(PollError::State(tiller_core::Error::Internal(format!(
                "{} reconcile steps failed; first: {}",
                report.errors.len(),
                report.errors[0]
            ))))
        }
    }
}

/// The treehouse pool as `config.toml` names it now: the `[checkouts]`
/// table and the `[spawn] treehouse` command, read again on every call so
/// an edit needs no restart. A file that does not parse lists nothing and
/// takes nothing back (`recovery.md` R15).
struct ConfiguredPool {
    config: PathBuf,
}

impl ConfiguredPool {
    fn read(&self) -> Result<SpawnConfig, String> {
        let table = match std::fs::read_to_string(&self.config) {
            Ok(text) => text
                .parse::<toml::Table>()
                .map_err(|error| format!("config.toml does not parse: {error}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(error) => return Err(format!("read config.toml: {error}")),
        };
        SpawnConfig::from_table(&table).map_err(|error| format!("config.toml: {error}"))
    }
}

impl WorktreePool for ConfiguredPool {
    fn checkouts(&self) -> Result<Vec<PathBuf>, String> {
        Ok(self.read()?.checkouts.into_values().collect())
    }

    fn give_back(&self, path: &Path, holder: &str) -> Result<(), String> {
        let treehouse = self.read()?.spawn.treehouse;
        Treehouse::new(treehouse.into_iter().map(OsString::from).collect()).give_back(path, holder)
    }
}

/// The worktree scanner reconcile reads through: git, plus the pool of
/// `<home>/config.toml` that R15 lists worktrees from and returns them to.
pub fn scanner(home: &Path) -> GitScanner {
    GitScanner::with_pool(ConfiguredPool {
        config: home.join("config.toml"),
    })
}

/// How long a read of the tool versions stands: the host check runs with
/// every reconcile pass, and each version read starts several processes
/// (`recovery.md` R01).
const VERSIONS_EVERY: Duration = Duration::from_secs(300);

/// Free RAM and disk read live for the host check (R01, R16), and the tool
/// versions every five minutes (P43). The versions also fill
/// `tools.versions`, which `GET /api/v1/health` serves.
struct LiveReadings {
    home: PathBuf,
    tools: Tools,
    last: Mutex<Option<(Instant, tools::Versions)>>,
}

impl LiveReadings {
    fn versions(&self) -> tools::Versions {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, versions)) = last.as_ref()
            && at.elapsed() < VERSIONS_EVERY
        {
            return versions.clone();
        }
        let versions = read_tool_versions(&self.tools);
        *self
            .tools
            .versions
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = versions.tools.clone();
        *last = Some((Instant::now(), versions.clone()));
        versions
    }
}

/// Every tool Health lists, read now.
pub fn read_tool_versions(tools: &Tools) -> tools::Versions {
    let mut versions = tools::read_all(
        &tools::Programs::standard(tools.quota.clone()),
        tools::ps_args,
    );
    let herdr = tools
        .herdr
        .snapshot()
        .ok()
        .and_then(|snapshot| snapshot.version);
    if let Some(object) = versions.tools.as_object_mut() {
        object.insert("herdr".to_owned(), herdr.into());
        object.insert("skew".to_owned(), versions.skew.clone().into());
    }
    versions
}

impl Readings for LiveReadings {
    fn read(&self) -> HostReadings {
        let versions = self.versions();
        HostReadings {
            ram_free_mb: readings::ram_free_mb(),
            disk_free_mb: readings::disk_free_mb(&self.home),
            version_skew: versions.skew,
            versions: versions.tools,
        }
    }
}

// -- landing ----------------------------------------------------------------------

/// The land loop (`authority.md` section 6): each pass matches standing
/// grants, then lands every accepted approval. Only the holder of the
/// single-instance lock lands (G01).
pub struct Land {
    home: PathBuf,
    store: Store,
    lock: Arc<DaemonLock>,
    forge: GhForge,
    app: HostApp,
}

impl Land {
    fn scrub(&self) -> ScrubList {
        scrub_list(&self.home)
    }
}

/// The host's private names as SHA-256 hashes, plus the optional plain-text
/// list `scripts/scrub-check.sh` also reads, kept outside any repo
/// (`memory.md` section 8).
fn scrub_list(home: &Path) -> ScrubList {
    let mut terms = std::fs::read_to_string(home.join("scrub-terms.sha256")).unwrap_or_default();
    if let Some(extra) = std::env::var_os("TILLER_SCRUB_TERMS") {
        terms.push('\n');
        terms.push_str(&std::fs::read_to_string(extra).unwrap_or_default());
    }
    ScrubList::parse(&terms)
}

/// What the reconcile forge reads from `state.db` and the host.
struct StoreLocal {
    home: PathBuf,
    store: Store,
}

impl Local for StoreLocal {
    fn pipeline_state(&self, repo: &str, number: i64) -> PipelineState {
        self.store
            .conn()
            .query_row(
                "SELECT pipeline_state FROM prs WHERE number = ?2
                   AND project_id IN (SELECT id FROM projects WHERE repo = ?1)",
                rusqlite::params![repo, number],
                |row| row.get(0),
            )
            .unwrap_or(PipelineState::None)
    }

    fn asks(&self, repo: &str) -> Vec<String> {
        let Ok(mut statement) = self.store.conn().prepare(
            "SELECT text FROM asks WHERE length(text) > 40
               AND project_id IN (SELECT id FROM projects WHERE repo = ?1)",
        ) else {
            return Vec::new();
        };
        statement
            .query_map([repo], |row| row.get::<_, String>(0))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    fn scrub_list(&self) -> ScrubList {
        scrub_list(&self.home)
    }
}

/// The forge reconcile reads through: GitHub as the owner's login, the same
/// `gh` the publish and land paths use.
fn reconcile_forge(home: &Path, store: Store, gh: &str) -> Box<dyn Forge> {
    Box::new(ReconcileForge::new(
        gh,
        Box::new(StoreLocal {
            home: home.to_owned(),
            store,
        }),
    ))
}

impl Source for Land {
    fn name(&self) -> String {
        "land".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
        let lander = Lander {
            forge: &self.forge,
            app: &self.app,
            scrub: self.scrub(),
        };
        // One PR or approval that errors is logged and skipped; a grant pass
        // that fails does not hold up landing.
        match lander.match_grants(&self.lock, &mut self.store) {
            Ok(matched) => {
                for (pr, error) in matched.errors {
                    tracing::warn!(pr, %error, "grant match failed");
                }
            }
            Err(error) => tracing::warn!(%error, "grant pass failed"),
        }
        let landed = lander
            .land_ready(&self.lock, &mut self.store)
            .map_err(|error| PollError::State(tiller_core::Error::Internal(error.to_string())))?;
        for landed in &landed {
            match landed {
                Landed::Waiting { approval, why } => {
                    tracing::info!(approval = %approval.id, %why, "approval waits");
                }
                Landed::Errored { approval, error } => {
                    tracing::warn!(approval = %approval.id, %error, "land failed");
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// The publish loop of direct-pr runs (`authority.md` section 6): pushes
/// the branch of each run that reports done and opens its PR. Its own
/// source, so a slow remote never holds up landing. Only the holder of the
/// single-instance lock publishes.
pub struct Publish {
    home: PathBuf,
    store: Store,
    lock: Arc<DaemonLock>,
    forge: GhForge,
    app: HostApp,
}

impl Source for Publish {
    fn name(&self) -> String {
        "publish".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
        let lander = Lander {
            forge: &self.forge,
            app: &self.app,
            scrub: scrub_list(&self.home),
        };
        // One run that errors is logged and skipped.
        let pass = lander
            .publish_ready(&self.lock, &mut self.store)
            .map_err(|error| PollError::State(tiller_core::Error::Internal(error.to_string())))?;
        for published in pass.published {
            match published {
                Published::Done {
                    run,
                    sha,
                    url,
                    opened,
                } => {
                    tracing::info!(run, sha, url, opened, "published");
                }
                Published::Failed { run, sha, error } => {
                    tracing::warn!(run, ?sha, %error, "publish failed");
                }
            }
        }
        for (run, error) in pass.errors {
            tracing::warn!(run, %error, "publish errored");
        }
        Ok(())
    }
}

// -- nightly backup ---------------------------------------------------------------

/// The nightly backup (`recovery.md` R10): every minute it asks
/// [`backup::nightly`] whether a backup is due, which takes it at
/// [`backup::NIGHT_HOUR`] host local time, retries a failed night an hour
/// later and raises the alarm from the second failed night. The manifest
/// keeps the schedule and the failures, so a restart or a sleeping host
/// misses nothing: a night that passed while the daemon was down is made up
/// by one backup at the next pass. A failed copy can be the first sign of a
/// damaged live database, so the live one is checked then. While the daemon
/// is read-only it takes nothing.
pub struct Backups {
    store: Store,
    paths: backup::Paths,
    zone: TimeZone,
    db: PathBuf,
    damage: Damage,
    /// The next time a pass is due, from the last pass. Polls before it do
    /// not take the manifest lock, so a manifest that cannot be read or a
    /// failed night is looked at again only when its retry is due.
    not_before: Option<Timestamp>,
}

impl Source for Backups {
    fn name(&self) -> String {
        "backup".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        if self.damage.reason().is_some() || self.not_before.is_some_and(|due| now < due) {
            return Ok(());
        }
        let pass = backup::nightly(&mut self.store, &self.paths, now, &self.zone)
            .inspect_err(|error| self.damage.suspect(&self.db, error))?;
        self.not_before = match &pass {
            Nightly::NotDue { next } | Nightly::Failed { next, .. } => Some(*next),
            Nightly::Done(_) => None,
        };
        match pass {
            Nightly::NotDue { .. } => {}
            Nightly::Done(backup::Taken { entry, .. }) => {
                tracing::info!(file = entry.file, "took the nightly backup");
            }
            Nightly::Failed { error, next, alarm } => {
                tracing::warn!(error, %next, alarm = alarm.is_some(), "the nightly backup failed");
                self.damage.check(&self.db);
            }
        }
        Ok(())
    }
}

// -- finished runs ----------------------------------------------------------------

/// Stops the agent of each local run that ended `landed` or `reported`, so
/// reconcile can close its pane and return its worktree (R15). An agent
/// mid-turn, with a draft or at a dialog waits for a later pass; one that
/// took `/exit` without leaving is not sent another, and is left to R15.
pub struct Finished {
    store: Store,
    herdr: Cli,
}

impl Source for Finished {
    fn name(&self) -> String {
        "stop".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(15)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        // Never an interrupt: the composer is looked at once, not waited on.
        let waits = Waits {
            interrupt: Duration::ZERO,
            ..Waits::default()
        };
        let swept = control::stop_finished(
            &mut self.store,
            &Ctx::at(Actor::Daemon, now),
            &self.herdr,
            &waits,
        )
        .map_err(|error| PollError::State(tiller_core::Error::Internal(error.to_string())))?;
        for swept in swept {
            if swept.stopped {
                tracing::info!(
                    session = swept.session,
                    run = swept.run,
                    evidence = swept.evidence,
                    "stopped the agent of a finished run"
                );
            } else if swept.failed {
                tracing::warn!(
                    session = swept.session,
                    run = swept.run,
                    evidence = swept.evidence,
                    "could not stop a finished run's agent"
                );
            } else if swept.exit_sent {
                tracing::warn!(
                    session = swept.session,
                    run = swept.run,
                    evidence = swept.evidence,
                    "a finished run's agent did not stop at /exit; left to reconcile"
                );
            } else {
                tracing::debug!(
                    session = swept.session,
                    run = swept.run,
                    evidence = swept.evidence,
                    "a finished run's agent waits"
                );
            }
        }
        Ok(())
    }
}

// -- transcripts ------------------------------------------------------------------

/// How long a session's transcript is still read after the session ends
/// (`protocol.md` section 5, Retention).
const TRANSCRIPT_RETENTION_MINUTES: i64 = 10;

/// Tails the transcript of every local session that has one, live or
/// ended less than [`TRANSCRIPT_RETENTION_MINUTES`] ago (`protocol.md`
/// section 5). It polls each file every two seconds; the offset lives in
/// the observations, so a restart repeats nothing.
///
/// A tailer belongs to one session and one file. `/clear` or a forked
/// resume binds the session to a new transcript: the old file's tailer
/// reads once more what came since its last poll, and is then let go.
///
/// The two rules meet like this. The retention window keeps only the file
/// the session was bound to when it ended, because Claude Code writes its
/// last lines there. A file the session had already left is read once
/// more and let go, whether the session is live or inside its window.
/// When the window closes, the last file is let go without a read, so a
/// line appended after the window is never recorded.
pub struct Transcripts {
    store: Store,
    host_id: String,
    intake: bool,
    /// Keyed by (session id, transcript path).
    tailers: HashMap<(String, String), Tailer>,
}

impl Source for Transcripts {
    fn name(&self) -> String {
        "transcript".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(2)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        // Live sessions, and ended ones still inside the retention window.
        let watched: Vec<Session> = list(
            self.store.conn(),
            "host_id = ?1 AND transcript_path IS NOT NULL
             AND (ended_at IS NULL OR ended_at > ?2)",
            &[
                &self.host_id,
                &now.plus_minutes(-TRANSCRIPT_RETENTION_MINUTES),
            ],
        )?;
        let current = |key: &(String, String)| {
            watched.iter().any(|session| {
                session.id == key.0 && session.transcript_path.as_deref() == Some(key.1.as_str())
            })
        };
        let old: Vec<(String, String)> = self
            .tailers
            .keys()
            .filter(|key| !current(key))
            .cloned()
            .collect();
        for key in old {
            // A watched session moved to a new file: read the rest of the
            // old one first. A session past its window is let go unread.
            let moved = watched.iter().any(|session| session.id == key.0);
            if moved && let Some(tailer) = self.tailers.get_mut(&key) {
                tailer.poll(&mut self.store)?;
            }
            self.tailers.remove(&key);
        }
        for session in &watched {
            if let Some(path) = &session.transcript_path {
                let key = (session.id.clone(), path.clone());
                if !self.tailers.contains_key(&key) {
                    let tailer = Tailer::resume(&self.store, session, path, self.intake)?;
                    self.tailers.insert(key, tailer);
                }
            }
        }
        for tailer in self.tailers.values_mut() {
            tailer.poll(&mut self.store)?;
        }
        Ok(())
    }
}

// -- memory loads -----------------------------------------------------------------

/// Copies a session's load set on each `SessionStart` observation for a
/// resume, `/clear` or `/compact` (`memory.md` section 7, `protocol.md`
/// 4.2). The hook only records the observation, so it stays in its budget.
/// Each pass reads only the observations recorded since the last one
/// (`mark`); the first pass after a start reads them all. A copy this pass
/// cannot write is logged, the pass goes on to the others, and the
/// poller's health shows the failure; the mark stays at or before it, so
/// the next pass tries it again, until an hour after its observation was
/// recorded or its session ended. The first pass past that hour logs that it
/// gave the copy up, and its health line counts it.
pub struct MemoryLoads {
    store: Store,
    mark: Option<CopyMark>,
}

impl Source for MemoryLoads {
    fn name(&self) -> String {
        "memory_loads".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        let pass = self
            .store
            .mem_load_owed(&Ctx::at(Actor::Daemon, now), self.mark)?;
        self.mark = Some(pass.mark);
        for loaded in pass.made {
            tracing::info!(
                session = loaded.session_id,
                trigger = loaded.trigger,
                revisions = loaded.revision_ids.len(),
                "copied a session's memory load"
            );
        }
        let failed = pass.failed.len();
        let given_up = pass.failed.iter().filter(|f| f.given_up).count();
        let Some(first) = pass.failed.first().map(|f| f.error.to_string()) else {
            return Ok(());
        };
        for copy in pass.failed {
            let what = if copy.given_up {
                "gave up copying a session's memory load: its window passed, \
                 so its Loaded by record lacks this load"
            } else {
                "could not copy a session's memory load"
            };
            tracing::warn!(
                session = copy.session_id,
                trigger = copy.trigger,
                observed_at = %copy.observed_at,
                given_up = copy.given_up,
                error = %copy.error,
                "{what}"
            );
        }
        let given_up = if given_up > 0 {
            format!(" ({given_up} given up, past their window)")
        } else {
            String::new()
        };
        Err(PollError::State(tiller_core::Error::Internal(format!(
            "{failed} memory load copies failed{given_up}; the first: {first}"
        ))))
    }
}

// -- remote machines ----------------------------------------------------------------

/// Gives every saved remote machine its own cache poller, including one the
/// owner adds while the daemon runs. Checks the host list every 30 s.
pub struct Machines {
    store: Store,
    home: PathBuf,
    db: PathBuf,
    herdr: Cli,
    registry: Registry,
    stop: watch::Receiver<bool>,
    runtime: tokio::runtime::Handle,
}

impl Source for Machines {
    fn name(&self) -> String {
        "machines".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
        let remotes: Vec<Host> = list(
            self.store.conn(),
            "kind = 'remote' AND herdr_machine IS NOT NULL ORDER BY id",
            &[],
        )?;
        let running: Vec<String> = self
            .registry
            .health()
            .into_iter()
            .map(|health| health.source)
            .collect();
        for host in remotes {
            let Some(machine) = host.herdr_machine else {
                continue;
            };
            if running.contains(&format!("herdr:{machine}")) {
                continue;
            }
            let herdr = self.herdr.clone().on_machine(machine.clone());
            let source = HerdrSource::remote(herdr, Store::open(&self.db)?, &host.id, machine);
            if let Ok(poller) = self.registry.register(Gated::new(&self.home, source)) {
                // Ends with the daemon, like every other poller.
                self.runtime.spawn(poller.run(self.stop.clone()));
            }
        }
        Ok(())
    }
}

// -- starting them --------------------------------------------------------------

/// Spawns `poller` on the runtime until `stop` turns true.
fn spawn<S: Source>(
    registry: &Registry,
    source: S,
    stop: &watch::Receiver<bool>,
    handles: &mut Vec<JoinHandle<()>>,
) {
    match registry.register(source) {
        Ok(poller) => handles.push(tokio::spawn(poller.run(stop.clone()))),
        Err(error) => tracing::error!(%error, "poller not started"),
    }
}

/// Starts every worker against an initialized `state.db`. Each opens its own
/// connection. Returns the tasks, which end once `stop` turns true.
pub fn start(
    home: &Path,
    lock: Arc<DaemonLock>,
    tools: &Tools,
    registry: &Registry,
    damage: &Damage,
    stop: &watch::Receiver<bool>,
) -> tiller_core::Result<Vec<JoinHandle<()>>> {
    let db = home.join("state.db");
    let open = || Store::open(&db);
    let store = open()?;
    let hosts: Vec<Host> = list(store.conn(), "1 = 1 ORDER BY id", &[])?;
    let local = hosts
        .iter()
        .find(|host| host.kind == HostKind::Local)
        .map(|host| host.id.clone())
        .ok_or_else(|| {
            tiller_core::Error::Unavailable(format!(
                "state.db has no local host; {}",
                tiller_core::RUN_INIT
            ))
        })?;
    let settings = Settings::load(home);

    // Every source is built, and every connection opened, before any
    // starts: a failure here leaves nothing running, and the next attempt
    // registers each name afresh.
    let herdr = HerdrSource::local(tools.herdr.clone(), open()?, &local);
    let machines = Machines {
        store,
        home: home.to_owned(),
        db: db.clone(),
        herdr: tools.herdr.clone(),
        registry: registry.clone(),
        stop: stop.clone(),
        runtime: tokio::runtime::Handle::current(),
    };
    let delivery = Deliverer::new(open()?, tools.herdr.clone(), &local);
    let notify = Notifier::new(open()?, tools.herdr.clone());
    let mut config = ReconcileConfig::new(&local, home);
    config.boot_id = boot_id();
    config.forge_login = settings.forge_login.clone();
    config.learning = settings.learning.clone();
    let sources = Sources {
        herdr: Box::new(tools.herdr.clone()),
        probe: Box::new(HerdrStatusProbe),
        scanner: Box::new(scanner(home)),
        forge: reconcile_forge(home, open()?, &tools.gh),
        // Reconcile closes a dead pane through herdr (R15).
        launcher: Box::new(HerdrLauncher::new(tools.herdr.clone(), {
            let herdr = tools.herdr.clone();
            move |pane: &str| herdr.close_pane(pane).map_err(|error| error.to_string())
        })),
        readings: Box::new(LiveReadings {
            home: home.to_owned(),
            tools: tools.clone(),
            last: Mutex::new(None),
        }),
        harness: Box::new(NoHarness),
    };
    let reconcile = Reconcile(Reconciler::new(open()?, config, sources));
    let publish = Publish {
        home: home.to_owned(),
        store: open()?,
        lock: lock.clone(),
        forge: GhForge::new(tools.gh.clone()),
        app: HostApp::new(home),
    };
    let deploys = crate::forge::Deploys {
        store: open()?,
        lock: lock.clone(),
        forge: crate::forge::GhDeploys::new(tools.gh.clone()),
    };
    let gate = crate::forge::gate::Gate {
        store: open()?,
        lock: lock.clone(),
        forge: crate::forge::gate::GhRulesets::new(tools.gh.clone()),
        app: Box::new(HostApp::new(home)),
        app_dir: home.join(APP_DIR),
        app_files: Default::default(),
        last: HashMap::new(),
    };
    let land = Land {
        home: home.to_owned(),
        store: open()?,
        lock,
        forge: GhForge::new(tools.gh.clone()),
        app: HostApp::new(home),
    };
    let transcripts = Transcripts {
        store: open()?,
        host_id: local,
        intake: HostConfig::load(home).intake_source == IntakeSource::Transcript,
        tailers: HashMap::new(),
    };
    let finished = Finished {
        store: open()?,
        herdr: tools.herdr.clone(),
    };
    let loads = MemoryLoads {
        store: open()?,
        mark: None,
    };
    let pages = PageAnswers::new(
        open()?,
        Lavish::new(tools.lavish.clone(), tools.review_hosts.clone()),
    );
    let quota = crate::quota::Feed::new(open()?, tools.quota.clone(), settings.quota_providers);
    let memory = crate::memory::watch::Watcher::new(home, open()?);
    let backups = Backups {
        store: open()?,
        paths: backup::Paths::in_home(home),
        zone: TimeZone::system(),
        db: db.clone(),
        damage: damage.clone(),
        not_before: None,
    };

    let mut handles = Vec::new();
    // Every worker but reconcile holds its polls while the owner marker
    // names another system (R20). A reconcile pass checks the marker
    // itself, so it sees the flip back and runs its full pass.
    spawn(registry, Gated::new(home, herdr), stop, &mut handles);
    spawn(registry, Gated::new(home, machines), stop, &mut handles);
    spawn(registry, Gated::new(home, delivery), stop, &mut handles);
    spawn(registry, Gated::new(home, notify), stop, &mut handles);
    spawn(registry, reconcile, stop, &mut handles);
    spawn(registry, Gated::new(home, publish), stop, &mut handles);
    spawn(registry, Gated::new(home, land), stop, &mut handles);
    spawn(registry, Gated::new(home, deploys), stop, &mut handles);
    spawn(registry, Gated::new(home, gate), stop, &mut handles);
    spawn(registry, Gated::new(home, finished), stop, &mut handles);
    spawn(registry, Gated::new(home, transcripts), stop, &mut handles);
    spawn(registry, Gated::new(home, loads), stop, &mut handles);
    spawn(registry, Gated::new(home, quota), stop, &mut handles);
    spawn(registry, Gated::new(home, pages), stop, &mut handles);
    spawn(registry, Gated::new(home, backups), stop, &mut handles);
    spawn(registry, Gated::new(home, memory), stop, &mut handles);
    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The forge reconcile reads through is the real one: with a `gh` that
    /// reports a merged PR, the read says merged and carries no
    /// "no forge is configured" error.
    #[test]
    fn reconcile_reads_the_forge_through_gh() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            r#"#!/bin/sh
case "$*" in
  *pulls/7/files*) echo src/lib.rs ;;
  *pulls/7/reviews*) echo '[[]]' ;;
  *pulls/7) echo '{"number":7,"state":"closed","merged":true,"merged_at":"2026-11-05T10:00:00Z","merge_commit_sha":"e1e1e1e1","merged_by":{"login":"owner"},"body":"","head":{"sha":"a1a1a1a1","ref":"fm/work"},"base":{"ref":"main","repo":{"private":true}},"mergeable_state":"clean"}' ;;
  *check-runs*) echo '[{"total_count":0,"check_runs":[]}]' ;;
  */status*) echo '[{"statuses":[]}]' ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = Store::open_in_memory().unwrap();
        let forge = reconcile_forge(dir.path(), store, &gh.to_string_lossy());
        let read = forge.pr("example/alpha", 7).unwrap();
        assert_eq!(read.fetch.state, tiller_core::domain::PrState::Merged);
        assert_eq!(read.fetch.merge_sha.as_deref(), Some("e1e1e1e1"));
        assert_eq!(read.merged_by.as_deref(), Some("owner"));
        assert_eq!(read.fetch.fetch_error, None);
    }

    /// R15 as the running daemon wires it: a real git checkout named in
    /// `config.toml`, a worktree of it the run worked in, the daemon's own
    /// scanner, and a `treehouse` that logs each call it gets.
    struct Pool {
        fleet: tiller_herdr::testing::Fleet,
        dir: tempfile::TempDir,
        herdr: tiller_herdr::fake::FakeHerdr,
        home: PathBuf,
        worktree: PathBuf,
        run_id: String,
        reconciler: Option<Reconciler>,
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn commit(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", text]);
    }

    impl Pool {
        /// Project `alpha`'s checkout with one commit on `main`, a worktree
        /// of it on the run's branch, and a builder run working there whose
        /// agent sits idle in pane `w2:p1`.
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            let mut fleet = tiller_herdr::testing::Fleet::new();
            let session = fleet.worker("w2:p1");
            let run_id = fleet
                .workers
                .iter()
                .find(|(s, _, _)| *s == session)
                .map(|(_, run, _)| run.clone())
                .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let checkout = dir.path().join("checkouts").join("alpha");
            std::fs::create_dir_all(&checkout).unwrap();
            git(&checkout, &["init", "-q", "-b", "main"]);
            git(&checkout, &["config", "user.email", "dev@example.com"]);
            git(&checkout, &["config", "user.name", "Dev"]);
            commit(&checkout, "base.txt", "base");
            let worktree = dir.path().join("pool").join("alpha-1");
            git(
                &checkout,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "tiller/alpha-1-build",
                    worktree.to_str().unwrap(),
                ],
            );
            fleet
                .store
                .conn()
                .execute(
                    "UPDATE runs SET worktree_path = ?2, branch = 'tiller/alpha-1-build' WHERE id = ?1",
                    rusqlite::params![run_id, worktree.to_string_lossy()],
                )
                .unwrap();

            let home = dir.path().join("home");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            let treehouse = dir.path().join("treehouse");
            std::fs::write(
                &treehouse,
                format!(
                    "#!/bin/sh\necho \"$*\" >> '{}'\n",
                    dir.path().join("treehouse.log").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&treehouse, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::write(
                home.join("config.toml"),
                format!(
                    "[spawn]\ntreehouse = [{:?}]\n\n[checkouts]\nalpha = {:?}\n",
                    treehouse.to_str().unwrap(),
                    checkout.to_str().unwrap()
                ),
            )
            .unwrap();

            let herdr = tiller_herdr::fake::FakeHerdr::new(dir.path().join("herdr.json"));
            fleet.place(&herdr);
            for binding in tiller_herdr::layout::bindings(fleet.store.conn(), &fleet.host).unwrap()
            {
                herdr.set_tokens(&binding.pane_id, binding.tokens());
            }
            let config = ReconcileConfig::new(&fleet.host, &home);
            let sources = Sources {
                herdr: Box::new(herdr.clone()),
                probe: Box::new(HerdrStatusProbe),
                scanner: Box::new(scanner(&home)),
                forge: Box::new(tiller_reconcile::sources::NoForge),
                launcher: Box::new(HerdrLauncher::new(herdr.clone(), {
                    let herdr = herdr.clone();
                    move |pane: &str| herdr.close_pane(pane).map_err(|error| error.to_string())
                })),
                readings: Box::new(tiller_reconcile::sources::NoReadings),
                harness: Box::new(NoHarness),
            };
            let reconciler = Reconciler::new(fleet.open(), config, sources);
            Self {
                fleet,
                dir,
                herdr,
                home,
                worktree,
                run_id,
                reconciler: Some(reconciler),
            }
        }

        fn pass(&mut self, now: Timestamp) -> tiller_reconcile::Report {
            self.reconciler.as_mut().unwrap().pass(now).unwrap()
        }

        /// Ends the run and lets its agent exit, then runs the passes that
        /// close its pane and judge its worktree.
        fn end_run(&mut self) -> tiller_reconcile::Report {
            let t0 = Timestamp::now();
            self.pass(t0);
            let mut store = Store::open(self.dir_db()).unwrap();
            store
                .run_end(
                    &Ctx::new(Actor::Owner),
                    &self.run_id,
                    tiller_core::state::runs::EndRun {
                        outcome: tiller_core::domain::RunOutcome::Abandoned,
                        note: Some("not needed".into()),
                    },
                )
                .unwrap();
            self.herdr
                .set_screen("w2:p1", tiller_herdr::fake::Screen::Shell);
            let first = self.pass(t0.plus_minutes(1));
            let second = self.pass(t0.plus_minutes(2));
            tiller_reconcile::Report {
                repairs: [first.repairs, second.repairs].concat(),
                errors: [first.errors, second.errors].concat(),
                ..first
            }
        }

        fn dir_db(&self) -> PathBuf {
            self.fleet.db.clone()
        }

        fn treehouse_calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("treehouse.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn cleaned(&self) -> bool {
            Store::open(self.dir_db())
                .unwrap()
                .conn()
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE entity = 'run' AND entity_id = ?1 AND kind = 'run.cleaned')",
                    [&self.run_id],
                    |row| row.get(0),
                )
                .unwrap()
        }

        /// Asserts the worktree was kept: no `run.clean`, no treehouse
        /// call, the files where they were.
        fn assert_kept(&self, report: &tiller_reconcile::Report) {
            assert!(!report.did("returned"), "{report:?}");
            assert!(!self.cleaned(), "{report:?}");
            assert_eq!(self.treehouse_calls(), Vec::<String>::new());
            assert!(self.worktree.join("base.txt").exists());
        }
    }

    /// The leak: an ended run whose worktree holds nothing goes back to the
    /// pool through `treehouse return`, under the run's own lease.
    #[test]
    fn daemon_returns_an_ended_runs_empty_worktree() {
        let mut pool = Pool::new();
        let report = pool.end_run();
        assert!(report.did("R15 pane w2:p1 closed"), "{report:?}");
        assert!(report.did("returned"), "{report:?}");
        assert!(pool.cleaned());
        assert_eq!(
            pool.treehouse_calls(),
            vec![format!(
                "return {} --if-lease-holder tiller:{}",
                pool.worktree.display(),
                pool.run_id
            )]
        );
    }

    /// A tracked file changed and never committed keeps the worktree.
    #[test]
    fn daemon_keeps_a_worktree_with_uncommitted_changes() {
        let mut pool = Pool::new();
        std::fs::write(pool.worktree.join("base.txt"), "edited").unwrap();
        let report = pool.end_run();
        pool.assert_kept(&report);
    }

    /// An untracked file keeps the worktree, even when the repository's own
    /// config hides untracked files from `git status`.
    #[test]
    fn daemon_keeps_a_worktree_with_untracked_files() {
        let mut pool = Pool::new();
        git(
            &pool.worktree,
            &["config", "status.showUntrackedFiles", "no"],
        );
        std::fs::write(pool.worktree.join("notes.md"), "draft").unwrap();
        let report = pool.end_run();
        pool.assert_kept(&report);
        assert!(pool.worktree.join("notes.md").exists());
    }

    /// A commit not on `main` keeps the worktree, and so does a stash.
    #[test]
    fn daemon_keeps_a_worktree_with_unlanded_commits_or_a_stash() {
        let mut pool = Pool::new();
        git(&pool.worktree, &["config", "user.email", "dev@example.com"]);
        git(&pool.worktree, &["config", "user.name", "Dev"]);
        commit(&pool.worktree, "feature.txt", "feature");
        let report = pool.end_run();
        pool.assert_kept(&report);

        let mut stashed = Pool::new();
        git(
            &stashed.worktree,
            &["config", "user.email", "dev@example.com"],
        );
        git(&stashed.worktree, &["config", "user.name", "Dev"]);
        std::fs::write(stashed.worktree.join("base.txt"), "edited").unwrap();
        git(&stashed.worktree, &["stash", "-q"]);
        let report = stashed.end_run();
        stashed.assert_kept(&report);
    }

    /// A lister that cannot read every checkout lists nothing, so nothing
    /// is returned: here a second configured checkout is missing, and then
    /// `config.toml` does not parse.
    #[test]
    fn daemon_keeps_every_worktree_when_the_lister_fails() {
        let mut pool = Pool::new();
        let config = pool.home.join("config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        let missing = pool.dir.path().join("checkouts").join("beta");
        std::fs::write(
            &config,
            format!("{text}beta = {:?}\n", missing.to_str().unwrap()),
        )
        .unwrap();
        let report = pool.end_run();
        pool.assert_kept(&report);

        let mut broken = Pool::new();
        std::fs::write(broken.home.join("config.toml"), "[checkouts\n").unwrap();
        let report = broken.end_run();
        broken.assert_kept(&report);
    }

    #[test]
    fn settings_default_to_claude() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(home.path()).quota_providers, vec!["claude"]);
        std::fs::write(
            home.path().join("config.toml"),
            "quota_providers = [\"claude\", \"codex\"]\nforge_login = \"owner\"\n",
        )
        .unwrap();
        let settings = Settings::load(home.path());
        assert_eq!(settings.quota_providers, vec!["claude", "codex"]);
        assert_eq!(settings.forge_login.as_deref(), Some("owner"));
    }

    /// The orchestrator's transcript, as the daemon's tailer source sees it.
    struct Transcript {
        fleet: tiller_herdr::testing::Fleet,
    }

    impl Transcript {
        fn new() -> Self {
            Self {
                fleet: tiller_herdr::testing::Fleet::new(),
            }
        }

        fn source(&self) -> Transcripts {
            Transcripts {
                store: self.fleet.open(),
                host_id: self.fleet.host.clone(),
                intake: false,
                tailers: HashMap::new(),
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.fleet.dir.path().join(name)
        }

        /// `SessionStart` with `source` (`startup`, `clear`, `resume`) binds
        /// the orchestrator to the Claude session and transcript.
        fn bind(&mut self, claude_session_id: &str, path: &Path, source: &str) {
            let session = self.fleet.orchestrator_session.clone();
            self.fleet
                .store
                .run_bind_session(
                    &Ctx::new(Actor::Hook),
                    tiller_core::state::runs::Bind {
                        session_id: session,
                        claude_session_id: claude_session_id.into(),
                        pane_id: None,
                        transcript_path: Some(path.to_string_lossy().into_owned()),
                        source: source.into(),
                    },
                )
                .unwrap();
        }

        /// Appends an interrupt entry, or `n` lines that mean nothing.
        fn interrupt(path: &Path, uuid: &str) {
            Self::append(
                path,
                &serde_json::json!({"type": "user", "uuid": uuid,
                    "timestamp": "2026-11-02T10:00:00Z",
                    "message": {"role": "user", "content": "[Request interrupted by user]"}}),
            );
        }

        fn filler(path: &Path, n: usize) {
            for i in 0..n {
                Self::append(
                    path,
                    &serde_json::json!({"type": "assistant", "uuid": format!("f{i}"),
                        "message": {"role": "assistant", "stop_reason": "tool_use"}}),
                );
            }
        }

        fn append(path: &Path, entry: &serde_json::Value) {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(file, "{entry}").unwrap();
        }

        /// `SessionEnd` at `at`.
        fn end(&mut self, at: Timestamp) {
            let session = self.fleet.orchestrator_session.clone();
            self.fleet
                .store
                .session_end(&Ctx::at(Actor::Hook, at), &session, "exit")
                .unwrap();
        }

        fn recorded(&self) -> Vec<String> {
            self.fleet
                .store
                .conn()
                .prepare(
                    "SELECT json_extract(payload, '$.uuid') FROM observations
                     WHERE source = 'transcript' ORDER BY id",
                )
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        }
    }

    /// `/clear` while the daemon runs: the session's transcript moves to a
    /// new file. The tailer reads what the old file got since its last poll,
    /// then follows the new one from its head.
    #[test]
    fn transcript_tailer_follows_a_cleared_session_to_its_new_file() {
        let mut t = Transcript::new();
        let (old, new) = (t.path("old.jsonl"), t.path("new.jsonl"));
        t.bind("claude-orchestrator", &old, "startup");
        Transcript::filler(&old, 3);
        let mut source = t.source();
        source.poll(Timestamp::now()).unwrap();
        assert!(t.recorded().is_empty());

        // An interrupt lands in the old file just before `/clear`.
        Transcript::interrupt(&old, "u1");
        t.bind("claude-cleared", &new, "clear");
        Transcript::interrupt(&new, "n1");
        source.poll(Timestamp::now()).unwrap();
        assert_eq!(t.recorded(), ["u1", "n1"]);

        Transcript::interrupt(&new, "n2");
        Transcript::interrupt(&old, "u2");
        source.poll(Timestamp::now()).unwrap();
        assert_eq!(t.recorded(), ["u1", "n1", "n2"], "the old file is let go");
    }

    /// `/clear` while the daemon is down: the restarted tailer reads the new
    /// file from its head, though the old file's offset is past it.
    #[test]
    fn transcript_tailer_after_a_restart_reads_a_new_file_from_its_head() {
        let mut t = Transcript::new();
        let (old, new) = (t.path("old.jsonl"), t.path("new.jsonl"));
        t.bind("claude-orchestrator", &old, "startup");
        Transcript::filler(&old, 20);
        Transcript::interrupt(&old, "u1");
        t.source().poll(Timestamp::now()).unwrap();
        assert_eq!(t.recorded(), ["u1"]);

        t.bind("claude-cleared", &new, "clear");
        Transcript::interrupt(&new, "n1");
        Transcript::filler(&new, 40);
        assert!(
            std::fs::metadata(&new).unwrap().len() > std::fs::metadata(&old).unwrap().len(),
            "the new file is past the old offset"
        );
        let mut restarted = t.source();
        restarted.poll(Timestamp::now()).unwrap();
        assert_eq!(t.recorded(), ["u1", "n1"]);
        restarted.poll(Timestamp::now()).unwrap();
        t.source().poll(Timestamp::now()).unwrap();
        assert_eq!(t.recorded(), ["u1", "n1"], "nothing twice");
    }

    /// Retention: a line appended after the session ended is recorded for
    /// 10 minutes, by the running tailer and by one a restart builds. A
    /// line appended after the window is never read.
    #[test]
    fn transcript_tailer_reads_an_ended_session_for_ten_minutes_then_stops() {
        let mut t = Transcript::new();
        let path = t.path("main.jsonl");
        t.bind("claude-orchestrator", &path, "startup");
        Transcript::filler(&path, 3);
        let ended = Timestamp::now();
        let mut source = t.source();
        source.poll(ended).unwrap();
        t.end(ended);

        Transcript::interrupt(&path, "u1");
        source.poll(ended.plus_minutes(1)).unwrap();
        assert_eq!(t.recorded(), ["u1"], "read after the end");
        // The daemon restarts inside the window.
        Transcript::interrupt(&path, "u2");
        let mut source = t.source();
        source.poll(ended.plus_minutes(9)).unwrap();
        assert_eq!(t.recorded(), ["u1", "u2"], "a restarted tailer too");

        Transcript::interrupt(&path, "u3");
        source.poll(ended.plus_minutes(10)).unwrap();
        Transcript::interrupt(&path, "u4");
        source.poll(ended.plus_minutes(11)).unwrap();
        t.source().poll(ended.plus_minutes(11)).unwrap();
        assert_eq!(t.recorded(), ["u1", "u2"], "nothing after the window");
    }

    /// Retention on a transcript-intake host: a line stamped after the
    /// session ended is kept as a row, but it moves nothing. The session
    /// stays `ended` and no ask is opened on it. A line stamped before the
    /// end, which Claude Code flushed late, still records its ask.
    #[test]
    fn transcript_lines_after_the_end_leave_an_ended_session_ended() {
        let mut t = Transcript::new();
        let path = t.path("main.jsonl");
        t.bind("claude-orchestrator", &path, "startup");
        let session = t.fleet.orchestrator_session.clone();
        let ended = Timestamp::now();
        let mut source = t.source();
        source.intake = true;
        source.poll(ended).unwrap();
        // The SessionEnd hook.
        t.fleet
            .store
            .observation_record(
                &Ctx::at(Actor::Hook, ended),
                tiller_core::state::runs::NewObservation {
                    session_id: Some(session.clone()),
                    run_id: None,
                    source: tiller_core::domain::ObservationSource::Hook,
                    event: "SessionEnd:exit".into(),
                    verdict: Some(tiller_core::domain::Activity::Ended),
                    observed_at: ended,
                    payload: serde_json::json!({}),
                },
            )
            .unwrap();
        t.end(ended);

        let before = ended.plus_minutes(-1).to_string();
        let after = ended.plus_minutes(1).to_string();
        let user = |uuid: &str, at: &str, text: &str| {
            serde_json::json!({"type": "user", "uuid": uuid, "timestamp": at,
                "message": {"role": "user", "content": text}})
        };
        Transcript::append(&path, &user("late", &before, "typed before the end"));
        Transcript::append(&path, &user("dead", &after, "typed after the end"));
        Transcript::append(
            &path,
            &serde_json::json!({"type": "assistant", "uuid": "turn", "timestamp": after,
                "message": {"role": "assistant", "stop_reason": "end_turn"}}),
        );
        source.poll(ended.plus_minutes(2)).unwrap();

        assert_eq!(t.recorded(), ["late", "dead", "turn"], "every row is kept");
        let row: Session =
            tiller_core::state::get(t.fleet.store.conn(), "session", &session).unwrap();
        assert_eq!(row.activity, tiller_core::domain::Activity::Ended);
        let conn = t.fleet.store.conn();
        let texts: Vec<String> = conn
            .prepare("SELECT text FROM asks WHERE session_id = ?1 ORDER BY id")
            .unwrap()
            .query_map([&session], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(texts, ["typed before the end"]);
        let moved: i64 = conn
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'observation.recorded'
                 AND json_extract(payload, '$.from') = 'ended'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(moved, 0, "the activity never left ended");
    }

    /// Retention and `/clear` together: the window keeps the file the
    /// session was bound to when it ended. A file the session left before
    /// is read once more and let go, as for a live session.
    #[test]
    fn transcript_tailer_keeps_only_an_ended_session_s_last_file() {
        let mut t = Transcript::new();
        let (old, new) = (t.path("old.jsonl"), t.path("new.jsonl"));
        t.bind("claude-orchestrator", &old, "startup");
        Transcript::filler(&old, 3);
        let ended = Timestamp::now();
        let mut source = t.source();
        source.poll(ended).unwrap();

        Transcript::interrupt(&old, "u1");
        t.bind("claude-cleared", &new, "clear");
        t.end(ended);
        Transcript::interrupt(&new, "n1");
        source.poll(ended.plus_minutes(1)).unwrap();
        assert_eq!(t.recorded(), ["u1", "n1"]);

        Transcript::interrupt(&old, "u2");
        Transcript::interrupt(&new, "n2");
        source.poll(ended.plus_minutes(2)).unwrap();
        assert_eq!(t.recorded(), ["u1", "n1", "n2"], "the old file is let go");
    }

    /// The nightly backup on an injected clock: the first pass schedules
    /// 03:00, the pass at 03:00 takes the backup, and two nights missed while
    /// the daemon was down are made up by one backup at the next pass.
    #[test]
    fn the_backup_worker_takes_the_night_on_the_clock() {
        let fleet = tiller_herdr::testing::Fleet::new();
        let home = fleet.dir.path();
        let paths = backup::Paths::in_home(home);
        let mut worker = Backups {
            store: fleet.open(),
            paths: paths.clone(),
            zone: TimeZone::UTC,
            db: fleet.db.clone(),
            damage: Damage::default(),
            not_before: None,
        };
        let at = |text: &str| text.parse::<Timestamp>().unwrap();
        let nightly = |manifest: &backup::Manifest| {
            manifest
                .live()
                .into_iter()
                .filter(|entry| entry.kind == backup::EntryKind::Nightly)
                .count()
        };

        worker.poll(at("2026-11-02T10:00:00Z")).unwrap();
        let manifest = backup::Manifest::load(&paths).unwrap();
        assert_eq!(manifest.next_night, Some(at("2026-11-03T03:00:00Z")));
        assert_eq!(nightly(&manifest), 0, "the first pass takes nothing");

        worker.poll(at("2026-11-03T02:59:00Z")).unwrap();
        assert_eq!(nightly(&backup::Manifest::load(&paths).unwrap()), 0);

        worker.poll(at("2026-11-03T03:00:00Z")).unwrap();
        let manifest = backup::Manifest::load(&paths).unwrap();
        assert_eq!(nightly(&manifest), 1, "03:00 takes the night");
        assert_eq!(manifest.next_night, Some(at("2026-11-04T03:00:00Z")));

        // Down from before 03:00 on the 4th until 09:00 on the 6th.
        worker.poll(at("2026-11-06T09:00:00Z")).unwrap();
        let manifest = backup::Manifest::load(&paths).unwrap();
        assert_eq!(nightly(&manifest), 2, "one backup makes up both nights");
        assert_eq!(manifest.next_night, Some(at("2026-11-07T03:00:00Z")));
        worker.poll(at("2026-11-06T09:01:00Z")).unwrap();
        assert_eq!(nightly(&backup::Manifest::load(&paths).unwrap()), 2);
    }

    #[test]
    fn the_backup_worker_waits_for_the_retry_after_a_failed_pass() {
        let fleet = tiller_herdr::testing::Fleet::new();
        let paths = backup::Paths::in_home(fleet.dir.path());
        let mut worker = Backups {
            store: fleet.open(),
            paths: paths.clone(),
            zone: TimeZone::UTC,
            db: fleet.db.clone(),
            damage: Damage::default(),
            not_before: None,
        };
        let at = |text: &str| text.parse::<Timestamp>().unwrap();
        std::fs::create_dir_all(&paths.backups).unwrap();
        std::fs::write(paths.manifest(), "not json").unwrap();

        worker.poll(at("2026-11-03T03:00:00Z")).unwrap();
        assert_eq!(worker.not_before, Some(at("2026-11-03T04:00:00Z")));

        // Polls before the retry do not look at the manifest again.
        std::fs::remove_file(paths.manifest()).unwrap();
        worker.poll(at("2026-11-03T03:30:00Z")).unwrap();
        assert!(!paths.manifest().exists());

        worker.poll(at("2026-11-03T04:00:00Z")).unwrap();
        assert!(paths.manifest().exists(), "the retry schedules the night");
    }
}
