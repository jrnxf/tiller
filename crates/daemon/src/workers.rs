//! The daemon's background work, each piece a poller in one registry so
//! `/api/v1/health` shows its last success and last error: the herdr cache
//! (local and each saved machine), delivery, reconcile, the land loop, the
//! transcript tailer and the quota feed.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tiller_agents::intake::{HostConfig, IntakeSource};
use tiller_agents::land::forge::{GhForge, NoApp};
use tiller_agents::land::publish::Published;
use tiller_agents::land::scrub::ScrubList;
use tiller_agents::land::{DaemonLock, Landed, Lander};
use tiller_agents::spawn::readings;
use tiller_core::domain::{Host, HostKind, Session};
use tiller_core::state::list;
use tiller_core::{Store, Timestamp};
use tiller_delivery::outbox::Deliverer;
use tiller_herdr::client::{Cli, Herdr};
use tiller_herdr::poller::{HerdrSource, PollError, Registry, Source};
use tiller_observe::tailer::Tailer;
use tiller_reconcile::git::GitScanner;
use tiller_reconcile::sources::{
    HerdrLauncher, HerdrStatusProbe, HostReadings, NoForge, NoHarness, Readings,
};
use tiller_reconcile::{Config as ReconcileConfig, Reconciler, Sources};
use tokio::sync::watch;
use tokio::task::JoinHandle;

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

/// Free RAM and disk read live for the host check (R01, R16).
struct LiveReadings {
    home: PathBuf,
}

impl Readings for LiveReadings {
    fn read(&self) -> HostReadings {
        HostReadings {
            ram_free_mb: readings::ram_free_mb(),
            disk_free_mb: readings::disk_free_mb(&self.home),
            version_skew: None,
            versions: serde_json::json!({"tiller": env!("CARGO_PKG_VERSION")}),
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
}

impl Land {
    /// The host's private names as SHA-256 hashes, plus the optional
    /// plain-text list `scripts/scrub-check.sh` also reads, kept outside any
    /// repo (`memory.md` section 8).
    fn scrub(&self) -> ScrubList {
        let mut terms =
            std::fs::read_to_string(self.home.join("scrub-terms.sha256")).unwrap_or_default();
        if let Some(extra) = std::env::var_os("TILLER_SCRUB_TERMS") {
            terms.push('\n');
            terms.push_str(&std::fs::read_to_string(extra).unwrap_or_default());
        }
        ScrubList::parse(&terms)
    }
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
            app: &NoApp,
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
    store: Store,
    lock: Arc<DaemonLock>,
    forge: GhForge,
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
            app: &NoApp,
            scrub: ScrubList::default(),
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

// -- transcripts ------------------------------------------------------------------

/// Tails the transcript of every live local session that has one
/// (`protocol.md` section 5). It polls each file every two seconds; the
/// offset lives in the observations, so a restart repeats nothing.
pub struct Transcripts {
    store: Store,
    host_id: String,
    intake: bool,
    tailers: HashMap<String, Tailer>,
}

impl Source for Transcripts {
    fn name(&self) -> String {
        "transcript".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(2)
    }

    fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
        let live: Vec<Session> = list(
            self.store.conn(),
            "ended_at IS NULL AND host_id = ?1 AND transcript_path IS NOT NULL",
            &[&self.host_id],
        )?;
        self.tailers
            .retain(|id, _| live.iter().any(|session| &session.id == id));
        for session in &live {
            if !self.tailers.contains_key(&session.id)
                && let Some(path) = &session.transcript_path
            {
                let tailer = Tailer::resume(&self.store, session, path, self.intake)?;
                self.tailers.insert(session.id.clone(), tailer);
            }
        }
        for tailer in self.tailers.values_mut() {
            tailer.poll(&mut self.store)?;
        }
        Ok(())
    }
}

// -- remote machines ----------------------------------------------------------------

/// Gives every saved remote machine its own cache poller, including one the
/// owner adds while the daemon runs. Checks the host list every 30 s.
pub struct Machines {
    store: Store,
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
            if let Ok(poller) = self.registry.register(source) {
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
            tiller_core::Error::Unavailable("state.db has no local host; run tiller init".into())
        })?;
    let settings = Settings::load(home);

    // Every source is built, and every connection opened, before any
    // starts: a failure here leaves nothing running, and the next attempt
    // registers each name afresh.
    let herdr = HerdrSource::local(tools.herdr.clone(), open()?, &local);
    let machines = Machines {
        store,
        db: db.clone(),
        herdr: tools.herdr.clone(),
        registry: registry.clone(),
        stop: stop.clone(),
        runtime: tokio::runtime::Handle::current(),
    };
    let delivery = Deliverer::new(open()?, tools.herdr.clone(), &local);
    let mut config = ReconcileConfig::new(&local, home);
    config.boot_id = boot_id();
    config.forge_login = settings.forge_login.clone();
    config.learning = settings.learning.clone();
    let sources = Sources {
        herdr: Box::new(tools.herdr.clone()),
        probe: Box::new(HerdrStatusProbe),
        scanner: Box::new(GitScanner::new()),
        forge: Box::new(NoForge),
        // Reconcile closes a dead pane through herdr (R15).
        launcher: Box::new(HerdrLauncher::new(tools.herdr.clone(), {
            let herdr = tools.herdr.clone();
            move |pane: &str| herdr.close_pane(pane).map_err(|error| error.to_string())
        })),
        readings: Box::new(LiveReadings {
            home: home.to_owned(),
        }),
        harness: Box::new(NoHarness),
    };
    let reconcile = Reconcile(Reconciler::new(open()?, config, sources));
    let publish = Publish {
        store: open()?,
        lock: lock.clone(),
        forge: GhForge::new(tools.gh.clone()),
    };
    let land = Land {
        home: home.to_owned(),
        store: open()?,
        lock,
        forge: GhForge::new(tools.gh.clone()),
    };
    let transcripts = Transcripts {
        store: open()?,
        host_id: local,
        intake: HostConfig::load(home).intake_source == IntakeSource::Transcript,
        tailers: HashMap::new(),
    };
    let quota = crate::quota::Feed::new(open()?, tools.quota.clone(), settings.quota_providers);

    let mut handles = Vec::new();
    spawn(registry, herdr, stop, &mut handles);
    spawn(registry, machines, stop, &mut handles);
    spawn(registry, delivery, stop, &mut handles);
    spawn(registry, reconcile, stop, &mut handles);
    spawn(registry, publish, stop, &mut handles);
    spawn(registry, land, stop, &mut handles);
    spawn(registry, transcripts, stop, &mut handles);
    spawn(registry, quota, stop, &mut handles);
    Ok(handles)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
