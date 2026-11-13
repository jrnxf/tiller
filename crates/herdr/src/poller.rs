//! The herdr poller (T2.5; `recovery.md` R05, R18) of this host's herdr
//! server, until the terminal holder replaces it (`terminal-holder.md` 13,
//! H13). The generic poller lives in `tiller-poll`, re-exported here so its
//! users need no change.
//!
//! It backs off as every source does, but holds at 15 s: it runs on this
//! host, so a late herdr start shows in seconds. Saved herdr machines went
//! with schema 20 (`terminal-holder.md` 11.1): a remote host is a peer.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::json;
use tiller_core::domain::PaneState;
use tiller_core::domain::{Activity, ObservationSource, Session};
use tiller_core::state::caches::{PaneHolder, PaneReading};
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

/// The pane cache rows for one herdr snapshot: herdr panes carry no holder
/// instance. `metadata` holds the pane's row tokens, as a holder pane's
/// holds its meta, plus herdr's `agent` and `agent_status`.
pub fn readings(snapshot: &Snapshot, bindings: &[Binding]) -> Vec<PaneReading> {
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
            holder_instance: None,
            session_id: sessions.get(pane.pane_id.as_str()).map(|s| (*s).to_owned()),
            pid: None,
            label: labels
                .get(pane.workspace_id.as_str())
                .map(|l| (*l).to_owned()),
            state: PaneState::Running,
            exit_code: None,
            exit_signal: None,
            exited_at: None,
            metadata: {
                let mut metadata: serde_json::Map<String, serde_json::Value> = pane
                    .tokens
                    .iter()
                    .map(|(name, value)| (name.clone(), json!(value)))
                    .collect();
                metadata.insert("agent".into(), json!(pane.agent));
                metadata.insert("agent_status".into(), json!(pane.agent_status));
                serde_json::Value::Object(metadata)
            },
        })
        .collect()
}

/// A pane and the session bound to it.
type Bound = (String, String);

/// The local herdr server's snapshot source, which also gets its layout and
/// row tokens repaired.
pub struct HerdrSource<H: Herdr> {
    herdr: H,
    store: Store,
    host_id: String,
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
        Self {
            herdr,
            store,
            host_id: host_id.into(),
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

    /// Whether a terminal holder was seen on this host: its poller then owns
    /// the host's reachability, and herdr's reading of it would flap
    /// against it.
    fn holder_recorded(&self) -> Result<bool, PollError> {
        Ok(tiller_core::state::projects::meta(self.store.conn(), "holder_instance")?.is_some())
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn herdr(&self) -> &H {
        &self.herdr
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
        "herdr:local".to_owned()
    }

    fn interval(&self) -> Duration {
        LOCAL_INTERVAL
    }

    fn retry_after(&self, failures: u32) -> Duration {
        backoff(failures).min(LOCAL_BACKOFF_CAP)
    }

    /// One full snapshot replaces the host's herdr panes in one transaction;
    /// panes it positively lacks are gone. The server then gets its tokens
    /// re-reported and its layout repaired, which is a no-op when nothing
    /// drifted and the whole of R18's recovery after a restart.
    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        let snapshot = self.herdr.snapshot()?;
        let ctx = Ctx::at(Actor::Daemon, now);
        let bindings = layout::bindings(self.store.conn(), &self.host_id)?;
        self.store.cache_panes(
            &ctx,
            &self.host_id,
            PaneHolder::Herdr,
            readings(&snapshot, &bindings),
        )?;
        if !self.holder_recorded()? {
            self.store.host_seen(&ctx, &self.host_id, None)?;
        }
        self.observe(&ctx, &snapshot, &bindings)?;
        self.repair_layout(&snapshot, &bindings);
        Ok(())
    }

    /// Three failures in a row mark the host stale. The cache keeps every
    /// pane: a failed poll is no evidence that one is gone.
    fn failed(&mut self, now: Timestamp, failures: u32, error: &PollError) {
        if failures < STALE_AFTER || self.holder_recorded().unwrap_or(false) {
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

    use tiller_core::domain::Host;

    use super::*;
    use crate::fake::{FakeHerdr, Screen};
    use crate::testing::{Fleet, at};

    fn host(fleet: &Fleet, id: &str) -> Host {
        get(fleet.store.conn(), "host", id).unwrap()
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
}
