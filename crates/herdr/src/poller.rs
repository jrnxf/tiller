//! The herdr pollers (T2.5; `recovery.md` R05, R18). The generic poller
//! lives in `tiller-poll`, re-exported here so its users need no change.
//!
//! A herdr source backs off as every source does, but the local one holds at
//! 15 s: it runs on this host, so a late herdr start shows in seconds.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;
use tiller_core::domain::{Activity, ObservationSource, Session};
use tiller_core::state::caches::PaneReading;
use tiller_core::state::get;
use tiller_core::state::runs::NewObservation;
use tiller_core::{Actor, Ctx, Store, Timestamp};
pub use tiller_poll::{BACKOFF, Duplicate, Health, PollError, Poller, Registry, Source, backoff};

use crate::client::{Herdr, HerdrError, Snapshot};
use crate::layout::{self, Binding};

impl From<HerdrError> for PollError {
    fn from(error: HerdrError) -> Self {
        Self::from_source(error)
    }
}

/// The longest wait for the local herdr source: it runs on this host, so a
/// late herdr start should be noticed in seconds, not minutes.
pub const LOCAL_BACKOFF_CAP: Duration = Duration::from_secs(15);

/// Failures in a row before a herdr source marks its host stale.
pub const STALE_AFTER: u32 = 3;

pub const LOCAL_INTERVAL: Duration = Duration::from_secs(2);
pub const REMOTE_INTERVAL: Duration = Duration::from_secs(5);

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

/// A pane and the session bound to it.
type Bound = (String, String);

/// One herdr server's snapshot source: the local server, which also gets its
/// layout and row tokens repaired, or a saved machine, which is read only.
pub struct HerdrSource<H: Herdr> {
    herdr: H,
    store: Store,
    host_id: String,
    machine: Option<String>,
    /// The status last recorded per bound pane, so an observation is written
    /// only when herdr's reading changes. Each map is keyed by pane and
    /// session: a pane that another session binds starts afresh.
    statuses: HashMap<Bound, Option<String>>,
    /// Bound panes whose last `herdr:idle` a fresh hook `working` held
    /// (`protocol.md` 4.5), with when the hold ends: the reading is sent
    /// again then, though it has not changed, while the session is still
    /// hook `working`.
    held: HashMap<Bound, Timestamp>,
    /// Bound panes already reported gone.
    gone: HashSet<Bound>,
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
            held: HashMap::new(),
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
        // Only bound panes are remembered: a pane whose session ended, or
        // that another session took, starts afresh.
        let bound: HashSet<Bound> = bindings
            .iter()
            .map(|b| (b.pane_id.clone(), b.session_id.clone()))
            .collect();
        self.statuses.retain(|key, _| bound.contains(key));
        self.held.retain(|key, _| bound.contains(key));
        self.gone.retain(|key| bound.contains(key));
        for binding in bindings {
            let key: Bound = (binding.pane_id.clone(), binding.session_id.clone());
            let pane = snapshot.pane(&binding.pane_id);
            // The caches move only once the observation is recorded, so a
            // failed write is retried on the next poll.
            let (event, verdict) = match pane {
                None => {
                    if self.gone.contains(&key) {
                        continue;
                    }
                    ("pane_gone".to_owned(), Some(Activity::Ended))
                }
                Some(pane) => {
                    self.gone.remove(&key);
                    let due = self.held.get(&key).is_some_and(|until| ctx.now >= *until);
                    let unchanged = self.statuses.get(&key) == Some(&pane.agent_status);
                    if unchanged && !due {
                        continue;
                    }
                    let Some(status) = pane.agent_status.as_deref() else {
                        self.held.remove(&key);
                        self.statuses.insert(key, None);
                        continue;
                    };
                    let session: Session = get(self.store.conn(), "session", &binding.session_id)?;
                    // The resend is owed only while a hook `working` still
                    // holds the session: a newer hook verdict (a permission
                    // prompt, an error, a Stop) has replaced it otherwise.
                    if unchanged
                        && !(session.activity == Activity::Working
                            && session.activity_source == Some(ObservationSource::Hook))
                    {
                        self.held.remove(&key);
                        continue;
                    }
                    (format!("herdr:{status}"), verdict(status, &session))
                }
            };
            let recorded = self.store.observation_record(
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
                    // A pane that comes back is read afresh.
                    self.statuses.remove(&key);
                    self.held.remove(&key);
                    self.gone.insert(key);
                }
                Some(pane) => {
                    self.statuses.insert(key.clone(), pane.agent_status.clone());
                    // herdr keeps reading idle, so a held idle is sent
                    // again when its hold ends; without that it would
                    // never land.
                    match recorded.value.idle_held_until {
                        Some(until) => self.held.insert(key, until),
                        None => self.held.remove(&key),
                    };
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
            error if matches!(error.source_as(), Some(HerdrError::Parse { .. })) => "poll_failed",
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
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use tiller_core::domain::{Host, Pane};
    use tiller_core::state::list;

    use super::*;
    use crate::fake::{FakeHerdr, Screen};
    use crate::testing::{Fleet, at};
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

    fn herdr_events(fleet: &Fleet, session: &str) -> Vec<String> {
        fleet
            .store
            .conn()
            .prepare(
                "SELECT event FROM observations WHERE session_id = ?1 AND source = 'herdr'
                 ORDER BY observed_at",
            )
            .unwrap()
            .query_map([session], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// `protocol.md` 4.5: a `herdr:idle` is held while a hook `working` is
    /// under 30 s old, and lands once it is older. herdr keeps reading idle,
    /// so the poller sends the held reading again when the hold ends: a lost
    /// Stop hook never leaves the session busy forever.
    #[test]
    fn a_held_herdr_idle_lands_once_the_hold_ends() {
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
        // The turn's hooks say working; its Stop hook is lost.
        fleet
            .store
            .observation_record(
                &Ctx::new(Actor::Hook),
                NewObservation {
                    session_id: Some(worker.clone()),
                    run_id: None,
                    source: ObservationSource::Hook,
                    event: "PreToolUse".into(),
                    verdict: Some(Activity::Working),
                    observed_at: at(1),
                    payload: json!({}),
                },
            )
            .unwrap();
        herdr.set_screen("w2:p1", Screen::Idle);
        for n in [5, 10, 20, 30] {
            poller.poll_once(at(n));
            assert_eq!(activity(&fleet), Activity::Working, "held at {n} s");
        }
        poller.poll_once(at(32));
        assert_eq!(activity(&fleet), Activity::Idle, "the hold ended at 31 s");
        for n in [40, 120, 600] {
            poller.poll_once(at(n));
        }
        assert_eq!(activity(&fleet), Activity::Idle);
        assert_eq!(
            herdr_events(&fleet, &worker),
            ["herdr:working", "herdr:idle", "herdr:idle"],
            "the held reading is sent again once, when the hold ends"
        );
    }

    /// A hook `working` that keeps coming holds the idle again; once the
    /// hooks go quiet for 30 s the idle lands. A reading that changes in
    /// the meantime drops the resend.
    #[test]
    fn a_held_herdr_idle_waits_for_each_newer_hook_working() {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        let hook = |fleet: &mut Fleet, seconds: i64| {
            fleet
                .store
                .observation_record(
                    &Ctx::new(Actor::Hook),
                    NewObservation {
                        session_id: Some(worker.clone()),
                        run_id: None,
                        source: ObservationSource::Hook,
                        event: "PreToolUse".into(),
                        verdict: Some(Activity::Working),
                        observed_at: at(seconds),
                        payload: json!({}),
                    },
                )
                .unwrap();
        };
        let activity = |fleet: &Fleet| {
            get::<Session>(fleet.store.conn(), "session", &worker)
                .unwrap()
                .activity
        };
        hook(&mut fleet, 0);
        herdr.set_screen("w2:p1", Screen::Idle);
        poller.poll_once(at(2));
        hook(&mut fleet, 20);
        poller.poll_once(at(31));
        assert_eq!(
            activity(&fleet),
            Activity::Working,
            "held by the hook at 20 s"
        );
        poller.poll_once(at(51));
        assert_eq!(activity(&fleet), Activity::Idle);

        // A new turn: held again, then herdr reads working before the
        // hold ends, and nothing is resent.
        hook(&mut fleet, 60);
        herdr.set_screen("w2:p1", Screen::Working);
        poller.poll_once(at(61));
        hook(&mut fleet, 62);
        herdr.set_screen("w2:p1", Screen::Idle);
        poller.poll_once(at(63));
        assert_eq!(
            activity(&fleet),
            Activity::Working,
            "held by the hook at 62 s"
        );
        herdr.set_screen("w2:p1", Screen::Working);
        poller.poll_once(at(64));
        for n in [100, 200] {
            poller.poll_once(at(n));
        }
        assert_eq!(activity(&fleet), Activity::Working);
        assert_eq!(
            herdr_events(&fleet, &worker),
            [
                "herdr:idle",
                "herdr:idle",
                "herdr:idle",
                "herdr:working",
                "herdr:idle",
                "herdr:working"
            ]
        );
    }

    /// The resend is owed only while the hook `working` that held the idle
    /// is still the session's activity. A permission prompt or an error the
    /// hooks report during the hold is newer than herdr's idle: the resend
    /// at the end of the hold must not replace it.
    #[test]
    fn a_held_herdr_idle_is_not_resent_over_a_newer_hook_verdict() {
        for (event, newer) in [
            ("PermissionRequest", Activity::WaitingPermission),
            ("StopFailure", Activity::Errored),
        ] {
            let mut fleet = Fleet::new();
            let worker = fleet.worker("w2:p1");
            let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
            fleet.place(&herdr);
            let registry = Registry::new();
            let mut poller = registry
                .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
                .unwrap();
            let hook = |fleet: &mut Fleet, event: &str, verdict, seconds| {
                fleet
                    .store
                    .observation_record(
                        &Ctx::new(Actor::Hook),
                        NewObservation {
                            session_id: Some(worker.clone()),
                            run_id: None,
                            source: ObservationSource::Hook,
                            event: event.into(),
                            verdict: Some(verdict),
                            observed_at: at(seconds),
                            payload: json!({}),
                        },
                    )
                    .unwrap();
            };
            let activity = |fleet: &Fleet| {
                get::<Session>(fleet.store.conn(), "session", &worker)
                    .unwrap()
                    .activity
            };
            hook(&mut fleet, "PreToolUse", Activity::Working, 1);
            herdr.set_screen("w2:p1", Screen::Idle);
            poller.poll_once(at(5));
            assert_eq!(activity(&fleet), Activity::Working, "held");
            hook(&mut fleet, event, newer, 10);
            for n in [32, 40, 120] {
                poller.poll_once(at(n));
                assert_eq!(activity(&fleet), newer, "{event} kept at {n} s");
            }
            assert_eq!(herdr_events(&fleet, &worker), ["herdr:idle"], "{event}");
            assert!(poller.source().held.is_empty(), "{event}");
        }
    }

    /// A pane that another session binds between two polls starts afresh:
    /// the new session's first reading is recorded though herdr's status
    /// did not change, and its pane going away is reported.
    #[test]
    fn a_pane_bound_to_a_new_session_between_polls_starts_afresh() {
        let mut fleet = Fleet::new();
        let first = fleet.worker("w2:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        poller.poll_once(at(0));
        assert_eq!(herdr_events(&fleet, &first), ["herdr:idle"]);

        fleet
            .store
            .session_end(&Ctx::new(Actor::Daemon), &first, "test")
            .unwrap();
        let second = fleet.worker("w2:p1");
        poller.poll_once(at(2));
        assert_eq!(
            herdr_events(&fleet, &second),
            ["herdr:idle"],
            "the new session's first reading"
        );
        herdr.remove_pane("w2:p1");
        poller.poll_once(at(4));
        assert_eq!(herdr_events(&fleet, &second), ["herdr:idle", "pane_gone"]);
    }

    /// A pane that herdr reports gone and then again, with the status it
    /// had, is observed again: the session does not stay `ended`.
    #[test]
    fn a_pane_that_returns_after_gone_is_observed_again() {
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
        poller.poll_once(at(0));
        assert_eq!(activity(&fleet), Activity::Idle);
        herdr.remove_pane("w2:p1");
        poller.poll_once(at(2));
        assert_eq!(activity(&fleet), Activity::Ended);
        herdr.add_pane("w2", "w2:p1", Screen::Idle);
        poller.poll_once(at(4));
        assert_eq!(activity(&fleet), Activity::Idle, "the pane is back");
        assert_eq!(
            herdr_events(&fleet, &worker),
            ["herdr:idle", "pane_gone", "herdr:idle"]
        );
    }

    /// The per-pane memory covers only the panes bound now: a session that
    /// ended leaves nothing behind.
    #[test]
    fn pane_memory_is_pruned_to_the_bound_panes() {
        let mut fleet = Fleet::new();
        let gone = fleet.worker("w2:p1");
        let held = fleet.worker("w4:p1");
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let registry = Registry::new();
        let mut poller = registry
            .register(HerdrSource::local(herdr.clone(), fleet.open(), &fleet.host))
            .unwrap();
        fleet
            .store
            .observation_record(
                &Ctx::new(Actor::Hook),
                NewObservation {
                    session_id: Some(held.clone()),
                    run_id: None,
                    source: ObservationSource::Hook,
                    event: "PreToolUse".into(),
                    verdict: Some(Activity::Working),
                    observed_at: at(0),
                    payload: json!({}),
                },
            )
            .unwrap();
        poller.poll_once(at(1));
        herdr.remove_pane("w2:p1");
        poller.poll_once(at(2));
        let source = poller.source();
        assert!(source.gone.contains(&("w2:p1".to_owned(), gone.clone())));
        assert!(
            source
                .statuses
                .contains_key(&("w4:p1".to_owned(), held.clone()))
        );
        assert!(
            source
                .held
                .contains_key(&("w4:p1".to_owned(), held.clone()))
        );

        for session in [&gone, &held] {
            fleet
                .store
                .session_end(&Ctx::new(Actor::Daemon), session, "test")
                .unwrap();
        }
        poller.poll_once(at(3));
        let source = poller.source();
        assert!(source.gone.is_empty(), "{:?}", source.gone);
        let panes: Vec<&str> = source
            .statuses
            .keys()
            .map(|(pane, _)| pane.as_str())
            .collect();
        assert_eq!(panes, ["w1:p1"], "only the orchestrator's pane is left");
        assert!(source.held.is_empty(), "{:?}", source.held);
    }
}
