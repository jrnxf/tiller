//! Pollers: one per source, never overlapping, with backoff and a recorded
//! last success (T2.5; `recovery.md` R05, R18).
//!
//! A [`Poller`] owns its source and runs it on one task, so a slow poll
//! delays the next one instead of overlapping it. A failed poll backs off
//! 5 s, 15 s, 60 s, then every 5 minutes (the local source holds at 15 s),
//! and changes nothing a successful
//! read would: absence of evidence is never `gone` (`recovery.md`, principle
//! 3). [`Registry`] refuses a second poller for a source and serves each
//! poller's [`Health`] to `/health` and the board.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tiller_core::domain::{Activity, ObservationSource, Session};
use tiller_core::state::caches::PaneReading;
use tiller_core::state::get;
use tiller_core::state::runs::NewObservation;
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tokio::sync::watch;

use crate::client::{Herdr, HerdrError, Snapshot};
use crate::layout::{self, Binding};

/// How long to wait after the first, second and third failure in a row; every
/// later failure waits the last step.
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(300),
];

/// The longest wait for the local herdr source: it runs on this host, so a
/// late herdr start should be noticed in seconds, not minutes.
pub const LOCAL_BACKOFF_CAP: Duration = Duration::from_secs(15);

/// Failures in a row before a herdr source marks its host stale.
pub const STALE_AFTER: u32 = 3;

pub const LOCAL_INTERVAL: Duration = Duration::from_secs(2);
pub const REMOTE_INTERVAL: Duration = Duration::from_secs(5);

/// The wait after `failures` failed polls in a row.
pub fn backoff(failures: u32) -> Duration {
    let step = usize::try_from(failures.saturating_sub(1)).unwrap_or(usize::MAX);
    BACKOFF[step.min(BACKOFF.len() - 1)]
}

#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error(transparent)]
    Herdr(#[from] HerdrError),
    #[error(transparent)]
    State(#[from] tiller_core::Error),
}

/// Something a poller reads on a schedule.
pub trait Source: Send + 'static {
    /// Unique among the daemon's pollers, such as `herdr:local`.
    fn name(&self) -> String;
    fn interval(&self) -> Duration;
    /// The wait after `failures` failed polls in a row.
    fn retry_after(&self, failures: u32) -> Duration {
        backoff(failures)
    }
    fn poll(&mut self, now: Timestamp) -> Result<(), PollError>;
    /// Told after every failed poll, with the count of failures in a row.
    fn failed(&mut self, _now: Timestamp, _failures: u32, _error: &PollError) {}
}

/// One poller's record, as `/health` shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Health {
    pub source: String,
    pub last_attempt_at: Option<Timestamp>,
    pub last_success_at: Option<Timestamp>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

type Shared = Arc<Mutex<BTreeMap<String, Arc<Mutex<Health>>>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The daemon's pollers, by source name.
#[derive(Clone, Default)]
pub struct Registry {
    pollers: Shared,
}

#[derive(Debug, thiserror::Error)]
#[error("a poller for {0} is already running")]
pub struct Duplicate(pub String);

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A poller for `source`, refused while another poller holds the name.
    pub fn register<S: Source>(&self, source: S) -> Result<Poller<S>, Duplicate> {
        let name = source.name();
        let mut pollers = lock(&self.pollers);
        if pollers.contains_key(&name) {
            return Err(Duplicate(name));
        }
        let health = Arc::new(Mutex::new(Health {
            source: name.clone(),
            ..Health::default()
        }));
        pollers.insert(name.clone(), Arc::clone(&health));
        Ok(Poller {
            source,
            name,
            health,
            registry: Arc::clone(&self.pollers),
        })
    }

    /// Every poller's health, by source name.
    pub fn health(&self) -> Vec<Health> {
        lock(&self.pollers)
            .values()
            .map(|h| lock(h).clone())
            .collect()
    }
}

/// One source and its schedule. Dropping it frees the name.
pub struct Poller<S: Source> {
    source: S,
    name: String,
    health: Arc<Mutex<Health>>,
    registry: Shared,
}

impl<S: Source> Drop for Poller<S> {
    fn drop(&mut self) {
        lock(&self.registry).remove(&self.name);
    }
}

impl<S: Source> Poller<S> {
    pub fn health(&self) -> Health {
        lock(&self.health).clone()
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// Polls once and returns how long to wait before the next poll.
    pub fn poll_once(&mut self, now: Timestamp) -> Duration {
        let outcome = self.source.poll(now);
        let mut health = lock(&self.health);
        health.last_attempt_at = Some(now);
        match outcome {
            Ok(()) => {
                health.last_success_at = Some(now);
                health.consecutive_failures = 0;
                health.last_error = None;
                self.source.interval()
            }
            Err(error) => {
                health.consecutive_failures = health.consecutive_failures.saturating_add(1);
                health.last_error = Some(error.to_string());
                let failures = health.consecutive_failures;
                drop(health);
                tracing::warn!(source = %self.name, failures, %error, "poll failed");
                self.source.failed(now, failures, &error);
                self.source.retry_after(failures)
            }
        }
    }

    /// Polls until `stop` turns true. Each poll runs on the blocking pool and
    /// the next is scheduled only after it returns, so polls never overlap.
    pub async fn run(mut self, mut stop: watch::Receiver<bool>) {
        loop {
            if *stop.borrow() {
                return;
            }
            let polled = tokio::task::spawn_blocking(move || {
                let wait = self.poll_once(Timestamp::now());
                (self, wait)
            })
            .await;
            let wait = match polled {
                Ok((poller, wait)) => {
                    self = poller;
                    wait
                }
                Err(error) => {
                    tracing::error!(%error, "poller task panicked; stopping it");
                    return;
                }
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                changed = stop.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

/// The verdict a herdr `agent_status` stands for (`protocol.md` 4.5). `None`
/// for a status that says nothing about the session.
pub fn verdict(status: &str, newest: &Session) -> Option<Activity> {
    match status {
        "working" => Some(Activity::Working),
        "idle" | "done" => Some(Activity::Idle),
        "blocked"
            if newest.activity == Activity::WaitingPermission
                && newest.activity_source == Some(ObservationSource::Hook) =>
        {
            Some(Activity::WaitingPermission)
        }
        "blocked" => Some(Activity::WaitingInput),
        _ => None,
    }
}

/// One herdr server's snapshot source: the local server, which also gets its
/// layout and row tokens repaired, or a saved machine, which is read only.
pub struct HerdrSource<H: Herdr> {
    herdr: H,
    store: Store,
    host_id: String,
    machine: Option<String>,
    /// The status last recorded per pane, so an observation is written only
    /// when herdr's reading changes.
    statuses: HashMap<String, Option<String>>,
    /// Bound panes already reported gone.
    gone: HashSet<String>,
    /// What the last layout pass could not do, said once: the pass runs on
    /// every poll, and a complaint is logged again only when it changes.
    layout_complaint: Option<String>,
}

impl<H: Herdr + Send + 'static> HerdrSource<H> {
    /// The local server's source.
    pub fn local(herdr: H, store: Store, host_id: impl Into<String>) -> Self {
        Self::new(herdr, store, host_id.into(), None)
    }

    /// A saved machine's source, read only.
    pub fn remote(
        herdr: H,
        store: Store,
        host_id: impl Into<String>,
        machine: impl Into<String>,
    ) -> Self {
        Self::new(herdr, store, host_id.into(), Some(machine.into()))
    }

    fn new(herdr: H, store: Store, host_id: String, machine: Option<String>) -> Self {
        Self {
            herdr,
            store,
            host_id,
            machine,
            statuses: HashMap::new(),
            gone: HashSet::new(),
            layout_complaint: None,
        }
    }

    /// Cosmetic: a failed repair is retried on the next poll and is no
    /// evidence that the host is unreachable. What it could not do is logged
    /// when it first happens and again only when it changes, not every poll.
    fn repair_layout(&mut self, snapshot: &Snapshot, bindings: &[Binding]) {
        let complaint = match layout::repair(&self.herdr, snapshot, bindings) {
            Ok(repair) => {
                if repair.changed() {
                    tracing::info!(?repair, "herdr layout repaired");
                }
                (repair.unordered > 0).then(|| {
                    "workspace order drifted and workspace.move needs the herdr API socket \
                     (spawn.herdr_socket); labels are repaired, order is not"
                        .to_owned()
                })
            }
            Err(error) => Some(format!("herdr layout repair failed: {error}")),
        };
        if complaint == self.layout_complaint {
            return;
        }
        match &complaint {
            Some(complaint) => tracing::warn!(%complaint, "herdr layout"),
            None => tracing::info!("herdr layout settled"),
        }
        self.layout_complaint = complaint;
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn herdr(&self) -> &H {
        &self.herdr
    }

    fn readings(snapshot: &Snapshot, bindings: &[Binding]) -> Vec<PaneReading> {
        let labels: HashMap<&str, &str> = snapshot
            .workspaces
            .iter()
            .map(|w| (w.workspace_id.as_str(), w.label.as_str()))
            .collect();
        let sessions: HashMap<&str, &str> = bindings
            .iter()
            .map(|b| (b.pane_id.as_str(), b.session_id.as_str()))
            .collect();
        snapshot
            .panes
            .iter()
            .map(|pane| PaneReading {
                pane_id: pane.pane_id.clone(),
                workspace_label: labels
                    .get(pane.workspace_id.as_str())
                    .map(|l| (*l).to_owned()),
                agent_name: pane.agent.clone(),
                agent_status: pane.agent_status.clone(),
                session_id: sessions.get(pane.pane_id.as_str()).map(|s| (*s).to_owned()),
                metadata: json!(pane.tokens),
            })
            .collect()
    }

    fn observe(
        &mut self,
        ctx: &Ctx,
        snapshot: &Snapshot,
        bindings: &[Binding],
    ) -> Result<(), PollError> {
        for binding in bindings {
            let pane = snapshot.pane(&binding.pane_id);
            // The caches move only once the observation is recorded, so a
            // failed write is retried on the next poll.
            let (event, verdict) = match pane {
                None => {
                    if self.gone.contains(&binding.pane_id) {
                        continue;
                    }
                    ("pane_gone".to_owned(), Some(Activity::Ended))
                }
                Some(pane) => {
                    self.gone.remove(&binding.pane_id);
                    if self.statuses.get(&binding.pane_id) == Some(&pane.agent_status) {
                        continue;
                    }
                    let Some(status) = pane.agent_status.as_deref() else {
                        self.statuses.insert(binding.pane_id.clone(), None);
                        continue;
                    };
                    let session: Session = get(self.store.conn(), "session", &binding.session_id)?;
                    (format!("herdr:{status}"), verdict(status, &session))
                }
            };
            self.store.observation_record(
                ctx,
                NewObservation {
                    session_id: Some(binding.session_id.clone()),
                    run_id: binding.run_id.clone(),
                    source: ObservationSource::Herdr,
                    event,
                    verdict,
                    observed_at: ctx.now,
                    payload: json!({
                        "pane_id": binding.pane_id,
                        "host_id": self.host_id,
                        "agent_status": pane.and_then(|p| p.agent_status.clone()),
                    }),
                },
            )?;
            match pane {
                None => {
                    self.gone.insert(binding.pane_id.clone());
                }
                Some(pane) => {
                    self.statuses
                        .insert(binding.pane_id.clone(), pane.agent_status.clone());
                }
            }
        }
        Ok(())
    }
}

impl<H: Herdr + Send + 'static> Source for HerdrSource<H> {
    fn name(&self) -> String {
        match &self.machine {
            None => "herdr:local".to_owned(),
            Some(machine) => format!("herdr:{machine}"),
        }
    }

    fn interval(&self) -> Duration {
        match self.machine {
            None => LOCAL_INTERVAL,
            Some(_) => REMOTE_INTERVAL,
        }
    }

    fn retry_after(&self, failures: u32) -> Duration {
        let wait = backoff(failures);
        match self.machine {
            None => wait.min(LOCAL_BACKOFF_CAP),
            Some(_) => wait,
        }
    }

    /// One full snapshot replaces the host's pane cache in one transaction;
    /// panes it positively lacks are gone. The local server then gets its
    /// tokens re-reported and its layout repaired, which is a no-op when
    /// nothing drifted and the whole of R18's recovery after a restart.
    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        let snapshot = self.herdr.snapshot()?;
        let ctx = Ctx::at(Actor::Daemon, now);
        let bindings = layout::bindings(self.store.conn(), &self.host_id)?;
        self.store
            .cache_panes(&ctx, &self.host_id, Self::readings(&snapshot, &bindings))?;
        self.store.host_seen(&ctx, &self.host_id, None)?;
        self.observe(&ctx, &snapshot, &bindings)?;
        if self.machine.is_none() {
            self.repair_layout(&snapshot, &bindings);
        }
        Ok(())
    }

    /// Three failures in a row mark the host stale. The cache keeps every
    /// pane: a failed poll is no evidence that one is gone.
    fn failed(&mut self, now: Timestamp, failures: u32, error: &PollError) {
        if failures < STALE_AFTER {
            return;
        }
        let reason = match error {
            PollError::Herdr(HerdrError::Parse { .. }) => "poll_failed",
            _ => "herdr_link_down",
        };
        let ctx = Ctx::at(Actor::Daemon, now);
        if let Err(error) = self.store.host_seen(&ctx, &self.host_id, Some(reason)) {
            tracing::warn!(%error, host = %self.host_id, "could not mark the host stale");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tiller_core::domain::{Host, Pane};
    use tiller_core::state::list;

    use super::*;
    use crate::fake::{FakeHerdr, Screen};
    use crate::testing::{Fleet, at};

    struct Counting {
        name: &'static str,
        fail: bool,
        running: Arc<AtomicUsize>,
        overlapped: Arc<AtomicUsize>,
    }

    impl Source for Counting {
        fn name(&self) -> String {
            self.name.to_owned()
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(1)
        }
        fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
            if self.running.fetch_add(1, Ordering::SeqCst) > 0 {
                self.overlapped.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(5));
            self.running.fetch_sub(1, Ordering::SeqCst);
            if self.fail {
                Err(PollError::Herdr(HerdrError::Unreachable("down".into())))
            } else {
                Ok(())
            }
        }
    }

    fn counting(name: &'static str, fail: bool) -> Counting {
        Counting {
            name,
            fail,
            running: Arc::default(),
            overlapped: Arc::default(),
        }
    }

    #[test]
    fn backoff_steps_then_holds_at_five_minutes() {
        let waits: Vec<u64> = (1..=6).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [5, 15, 60, 300, 300, 300]);
    }

    #[test]
    fn local_source_backoff_holds_at_fifteen_seconds() {
        let fleet = Fleet::new();
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        herdr.set_unreachable(true);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr, fleet.open(), &fleet.host))
            .unwrap();
        let waits: Vec<u64> = (0..6).map(|n| poller.poll_once(at(n)).as_secs()).collect();
        assert_eq!(waits, [5, 15, 15, 15, 15, 15]);
    }

    #[test]
    fn remote_source_backoff_keeps_the_full_schedule() {
        let fleet = Fleet::new();
        let remote = fleet.remote_host("peer");
        let herdr = FakeHerdr::new(fleet.dir.path().join("remote.json"));
        herdr.set_unreachable(true);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::remote(herdr, fleet.open(), &remote, "peer"))
            .unwrap();
        let waits: Vec<u64> = (0..6).map(|n| poller.poll_once(at(n)).as_secs()).collect();
        assert_eq!(waits, [5, 15, 60, 300, 300, 300]);
    }

    #[test]
    fn one_poller_per_source_and_health_records_last_success() {
        let registry = Registry::new();
        let mut poller = registry.register(counting("herdr:local", false)).unwrap();
        assert!(registry.register(counting("herdr:local", false)).is_err());
        assert!(registry.register(counting("herdr:other", false)).is_ok());

        let wait = poller.poll_once(at(0));
        assert_eq!(wait, Duration::from_millis(1));
        poller.source_mut().fail = true;
        assert_eq!(poller.poll_once(at(1)), Duration::from_secs(5));
        assert_eq!(poller.poll_once(at(2)), Duration::from_secs(15));
        let health = registry
            .health()
            .into_iter()
            .find(|h| h.source == "herdr:local")
            .unwrap();
        assert_eq!(health.last_success_at, Some(at(0)));
        assert_eq!(health.last_attempt_at, Some(at(2)));
        assert_eq!(health.consecutive_failures, 2);
        assert!(health.last_error.unwrap().contains("down"));

        drop(poller);
        assert!(
            registry.register(counting("herdr:local", false)).is_ok(),
            "a stopped poller frees its source"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn polls_never_overlap() {
        let registry = Registry::new();
        let source = counting("slow", false);
        let overlapped = Arc::clone(&source.overlapped);
        let poller = registry.register(source).unwrap();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(poller.run(stopped));
        tokio::time::sleep(Duration::from_millis(80)).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let health = registry.health();
        assert!(health.is_empty(), "the finished poller freed its name");
        assert_eq!(overlapped.load(Ordering::SeqCst), 0);
    }

    fn panes(fleet: &Fleet) -> Vec<Pane> {
        list(fleet.store.conn(), "1 = 1 ORDER BY pane_id", &[]).unwrap()
    }

    fn host(fleet: &Fleet, id: &str) -> Host {
        get(fleet.store.conn(), "host", id).unwrap()
    }

    #[test]
    fn remote_poll_failure_marks_host_stale_not_panes_gone() {
        let fleet = Fleet::new();
        let remote = fleet.remote_host("peer");
        let herdr = FakeHerdr::new(fleet.dir.path().join("remote.json"));
        herdr.add_workspace("w1", "notes");
        herdr.add_pane("w1", "w1:p1", Screen::Idle);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::remote(
                herdr.clone(),
                fleet.open(),
                &remote,
                "peer",
            ))
            .unwrap();

        poller.poll_once(at(0));
        assert_eq!(panes(&fleet).len(), 1);
        assert_eq!(host(&fleet, &remote).last_seen_at, Some(at(0)));

        herdr.set_unreachable(true);
        let waits: Vec<_> = (1..=3).map(|n| poller.poll_once(at(n))).collect();
        assert_eq!(waits, [BACKOFF[0], BACKOFF[1], BACKOFF[2]]);
        let stale = host(&fleet, &remote);
        assert_eq!(stale.stale_reason.as_deref(), Some("herdr_link_down"));
        assert_eq!(
            stale.last_seen_at,
            Some(at(0)),
            "last seen keeps the last success"
        );
        let cached = panes(&fleet);
        assert_eq!(cached.len(), 1);
        assert!(
            cached[0].gone_at.is_none(),
            "a failed poll marks no pane gone"
        );
        assert!(
            herdr
                .calls()
                .iter()
                .all(|c| !c.starts_with("rename") && !c.starts_with("report")),
            "a saved machine is read only"
        );
    }

    #[test]
    fn poll_failure_never_marks_panes_gone() {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        poller.poll_once(at(0));

        herdr.set_unreachable(true);
        for n in 1..=6 {
            poller.poll_once(at(n));
        }
        assert!(panes(&fleet).iter().all(|p| p.gone_at.is_none()));
        let session: Session = get(fleet.store.conn(), "session", &worker).unwrap();
        assert_ne!(
            session.activity,
            Activity::Ended,
            "no pane_gone without a successful read"
        );
        assert_eq!(poller.health().consecutive_failures, 6);
    }

    #[test]
    fn reconnect_replaces_pane_cache_in_one_snapshot() {
        let fleet = Fleet::new();
        let remote = fleet.remote_host("peer");
        let herdr = FakeHerdr::new(fleet.dir.path().join("remote.json"));
        herdr.add_workspace("w1", "one");
        herdr.add_pane("w1", "w1:p1", Screen::Idle);
        herdr.add_pane("w1", "w1:p2", Screen::Idle);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::remote(
                herdr.clone(),
                fleet.open(),
                &remote,
                "peer",
            ))
            .unwrap();
        poller.poll_once(at(0));

        herdr.set_unreachable(true);
        for n in 1..=3 {
            poller.poll_once(at(n));
        }
        // While it was away, p2 closed and p3 opened.
        herdr.remove_pane("w1:p2");
        herdr.add_pane("w1", "w1:p3", Screen::Idle);
        herdr.set_unreachable(false);
        poller.poll_once(at(400));

        let cached = panes(&fleet);
        let live: Vec<&str> = cached
            .iter()
            .filter(|p| p.gone_at.is_none())
            .map(|p| p.pane_id.as_str())
            .collect();
        assert_eq!(live, ["w1:p1", "w1:p3"]);
        let gone = cached.iter().find(|p| p.pane_id == "w1:p2").unwrap();
        assert_eq!(gone.gone_at, Some(at(400)));
        // One write: both changes carry the reconnect's time and adjacent seqs.
        let seqs: Vec<i64> = fleet
            .store
            .conn()
            .prepare("SELECT seq FROM events WHERE entity = 'pane' AND at = ?1 ORDER BY seq")
            .unwrap()
            .query_map([at(400)], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(seqs.len(), 2, "p2 gone and p3 new");
        assert_eq!(seqs[1], seqs[0] + 1);
        let host = host(&fleet, &remote);
        assert_eq!(host.stale_reason, None);
        assert_eq!(host.last_seen_at, Some(at(400)));
    }

    #[test]
    fn local_restart_reapplies_layout_and_tokens() {
        let mut fleet = Fleet::new();
        let orchestrator = fleet.orchestrator_session.clone();
        let worker = fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        poller.poll_once(at(0));
        let settled = herdr.snapshot().unwrap();
        assert_eq!(
            settled
                .workspaces
                .iter()
                .map(|w| w.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "└ alpha-1 build", "notes"]
        );
        assert_eq!(settled.pane("w2:p1").unwrap().tokens["session"], worker);
        assert_eq!(
            settled.pane("w1:p1").unwrap().tokens["session"],
            orchestrator
        );

        // herdr restarts: tokens are gone and the restored order is flat.
        herdr.set_unreachable(true);
        poller.poll_once(at(1));
        herdr.restart(&["w3", "w2", "w1"]);
        herdr.set_unreachable(false);
        poller.poll_once(at(10));

        let repaired = herdr.snapshot().unwrap();
        assert_eq!(
            repaired
                .workspaces
                .iter()
                .map(|w| w.label.as_str())
                .collect::<Vec<_>>(),
            ["notes", "alpha", "└ alpha-1 build"]
        );
        let tokens = &repaired.pane("w2:p1").unwrap().tokens;
        assert_eq!(tokens["session"], worker);
        assert_eq!(tokens["card"], "alpha-1");
        assert_eq!(tokens["role"], "build");
        assert_eq!(tokens["project"], "alpha");

        let calls = herdr.calls().len();
        poller.poll_once(at(12));
        assert_eq!(
            herdr.calls().len(),
            calls + 1,
            "a settled layout costs one snapshot and no writes"
        );
    }

    #[test]
    fn a_refused_layout_move_is_not_a_failed_poll() {
        let mut fleet = Fleet::new();
        fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        herdr.refuse("move");
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        for n in 0..4 {
            poller.poll_once(at(n * 10));
        }
        assert_eq!(poller.health().consecutive_failures, 0);
        let host = host(&fleet, &fleet.host);
        assert_eq!(host.stale_reason, None);
        assert_eq!(host.last_seen_at, Some(at(30)));
        // Renames run first, so the labels are right in the wrong order.
        let labels: Vec<String> = herdr
            .snapshot()
            .unwrap()
            .workspaces
            .into_iter()
            .map(|w| w.label)
            .collect();
        assert_eq!(labels, ["alpha", "notes", "└ alpha-1 build"]);
    }

    /// What `f` logs, as plain text.
    fn logged(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
    }

    /// The layout pass runs on every two-second poll. Without the API socket
    /// it repairs the labels and says once that it cannot fix the order; a
    /// real failure after that is a new complaint, said once too.
    #[test]
    fn a_layout_complaint_is_logged_once_until_it_changes() {
        let mut fleet = Fleet::new();
        fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        herdr.set_no_socket(true);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        let logs = logged(|| {
            for n in 0..5 {
                poller.poll_once(at(n * 2));
            }
            herdr.set_no_socket(false);
            herdr.refuse("move");
            for n in 5..10 {
                poller.poll_once(at(n * 2));
            }
        });
        assert_eq!(
            logs.matches("needs the herdr API socket").count(),
            1,
            "{logs}"
        );
        assert_eq!(logs.matches("refused by the fake").count(), 1, "{logs}");
        assert_eq!(poller.health().consecutive_failures, 0);
        let labels: Vec<String> = herdr
            .snapshot()
            .unwrap()
            .workspaces
            .into_iter()
            .map(|w| w.label)
            .collect();
        assert_eq!(labels, ["alpha", "notes", "└ alpha-1 build"]);
    }

    #[test]
    fn a_pane_naming_another_session_keeps_its_tokens_and_row() {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let foreign = BTreeMap::from([("session".to_owned(), "01OTHERSESSION".to_owned())]);
        herdr.set_tokens("w2:p1", foreign.clone());
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        poller.poll_once(at(0));
        let snapshot = herdr.snapshot().unwrap();
        assert_eq!(snapshot.pane("w2:p1").unwrap().tokens, foreign);
        let labels: Vec<&str> = snapshot
            .workspaces
            .iter()
            .map(|w| w.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["alpha", "notes", "claude"],
            "the row is left alone"
        );

        // Reconcile clears the stale claim; the next poll takes the pane.
        herdr.set_tokens("w2:p1", BTreeMap::new());
        poller.poll_once(at(10));
        let snapshot = herdr.snapshot().unwrap();
        assert_eq!(snapshot.pane("w2:p1").unwrap().tokens["session"], worker);
    }

    #[test]
    fn status_changes_become_observations_and_a_gone_pane_ends() {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        let activity = |fleet: &Fleet| {
            get::<Session>(fleet.store.conn(), "session", &worker)
                .unwrap()
                .activity
        };

        herdr.set_screen("w2:p1", Screen::Working);
        poller.poll_once(at(0));
        assert_eq!(activity(&fleet), Activity::Working);
        poller.poll_once(at(1));
        herdr.set_screen("w2:p1", Screen::Permission);
        poller.poll_once(at(2));
        assert_eq!(activity(&fleet), Activity::WaitingInput);
        herdr.remove_pane("w2:p1");
        poller.poll_once(at(3));
        poller.poll_once(at(4));
        assert_eq!(activity(&fleet), Activity::Ended);
        let events: Vec<String> = fleet
            .store
            .conn()
            .prepare("SELECT event FROM observations WHERE session_id = ?1 ORDER BY observed_at")
            .unwrap()
            .query_map([&worker], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(events, ["herdr:working", "herdr:blocked", "pane_gone"]);
    }
}
