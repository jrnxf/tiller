//! The terminal holder as the observation source of this host's panes
//! (`terminal-holder.md` 4.6 and 6.1, `recovery.md` R18 and R22).
//!
//! [`HolderSource`] is a `tiller_poll` source. It connects with `hello`,
//! records the `welcome` against `meta.holder_instance`, starts a `watch`
//! and runs `list` into the `panes` cache. Events drive it: each `quiet`
//! reads the pane's visible screen and classifies it, and a `list` runs
//! every 30 s as a check and after each reconnect.
//!
//! A holder that does not answer is unknown, never gone: the poll fails,
//! backs off 5 s and then 15 s, and after three failures marks the host
//! `holder_down`. The cache keeps every pane.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::json;
use tiller_core::domain::{Activity, ObservationSource, PaneState, Session};
use tiller_core::state::caches::{HolderRecord, HolderSeen, PaneHolder, PaneReading};
use tiller_core::state::runs::NewObservation;
use tiller_core::state::{get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_observe::merge::holder_verdict;
use tiller_panes::composer::{Composer, classify};
use tiller_panes::handshake::Welcome;
use tiller_panes::message::{Event, Format, Listing, Pane, PaneId, Read};
use tiller_panes::{EventsEnd, Live, Panes, PanesError, Upgrade};
use tiller_poll::{PollError, Source, backoff};

/// The longest wait between two connects: the holder runs on this host, so
/// a late start shows in seconds (`terminal-holder.md` 6.1).
pub const BACKOFF_CAP: Duration = Duration::from_secs(15);

/// Failed connects in a row before the host is marked `holder_down` (R18).
pub const STALE_AFTER: u32 = 3;

/// How often the events are drained.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// How often a `list` runs as a check between events.
pub const LIST_EVERY: Duration = Duration::from_secs(30);

/// The host check that carries a refused upgrade (R22).
pub const UPGRADE_CHECK: &str = "holder_upgrade";

/// The stale reason of a host whose holder does not answer (R18).
pub const HOLDER_DOWN: &str = "holder_down";

/// What the daemon asks of the local holder: the panes, their events, and
/// the connection's own facts.
pub trait Link: Panes + Live {
    /// The `welcome` of a live connection, connecting when there is none.
    fn welcome(&self) -> tiller_panes::Result<Welcome>;
    /// The binary the holder process runs now, when it can be read.
    fn running_binary(&self) -> Option<PathBuf>;
    /// Asks the holder to re-exec into `binary` (`terminal-holder.md` 6.2).
    fn upgrade(&self, binary: &Path) -> tiller_panes::Result<Upgrade>;
}

fn polled(error: PanesError) -> PollError {
    PollError::from_source(error)
}

/// The `panes` cache rows for one `list`. A pane's key is the session it
/// was spawned for, and names one only when that session is on record.
pub fn readings(store: &Store, listing: &Listing) -> tiller_core::Result<Vec<PaneReading>> {
    let mut out = Vec::with_capacity(listing.panes.len());
    for pane in &listing.panes {
        let known: Vec<Session> = list(store.conn(), "id = ?1", &[&pane.key])?;
        out.push(PaneReading {
            pane_id: pane.pane.as_str().to_owned(),
            holder_instance: Some(listing.instance.clone()),
            session_id: known.first().map(|s| s.id.clone()),
            pid: Some(i64::from(pane.pid)),
            label: pane.meta.get("label").cloned(),
            state: match pane.state {
                tiller_panes::message::PaneState::Running => PaneState::Running,
                tiller_panes::message::PaneState::Exited => PaneState::Exited,
            },
            exit_code: pane.exit.and_then(|e| e.code).map(i64::from),
            exit_signal: pane.exit.and_then(|e| e.signal).map(i64::from),
            exited_at: pane.exit.map(|e| Timestamp::from_jiff(e.at)),
            metadata: json!(pane.meta),
        });
    }
    Ok(out)
}

/// The status a pane's visible screen stands for: `working`, `idle` or
/// `blocked`, or `None` when the screen says nothing (`terminal-holder.md`
/// 4.6).
pub fn screen_status(ansi: &str) -> Option<&'static str> {
    match classify(ansi) {
        Composer::Busy => Some("working"),
        Composer::Empty => Some("idle"),
        Composer::Dialog => Some("blocked"),
        Composer::Holds(_) | Composer::Unknown => None,
    }
}

/// A pane and the session bound to it.
pub type Bound = (String, String);

/// What the source remembers per bound pane, for tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub statuses: Vec<Bound>,
    pub held: Vec<Bound>,
    pub gone: Vec<Bound>,
}

enum Item {
    Event(Event),
    /// The watch ended or failed: the connection is gone.
    Closed,
}

/// One live `watch`, drained by a thread into a channel the poll reads.
struct Watch {
    rx: mpsc::Receiver<Item>,
    end: EventsEnd,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.end.end();
    }
}

struct Binding {
    session_id: String,
    run_id: Option<String>,
    pane_id: String,
}

/// The local terminal holder's source.
pub struct HolderSource {
    link: Arc<dyn Link>,
    store: Store,
    host_id: String,
    /// The stable profile path the holder should run (R22), or `None` to
    /// never ask it to upgrade.
    stable: Option<PathBuf>,
    /// The instance and version of the last `welcome`.
    seen: Option<(String, String)>,
    watch: Option<Watch>,
    /// Events the watch thread has passed on, for tests to wait on.
    received: Arc<AtomicU64>,
    /// The last `list`: its instance and panes by id.
    instance: Option<String>,
    panes: HashMap<PaneId, Pane>,
    last_list: Option<Timestamp>,
    /// The `config.changed` time the last upgrade check ran against;
    /// `None` until the first check.
    upgrade_checked: Option<Option<Timestamp>>,
    /// The status last recorded per bound pane. Each map is keyed by pane
    /// and session: a pane that another session binds starts afresh.
    statuses: HashMap<Bound, Option<String>>,
    /// Bound panes whose last `holder:idle` a fresh hook `working` held
    /// (`protocol.md` 4.5), with when the hold ends: the screen is read
    /// again then, and its idle sent again while the hook still holds.
    held: HashMap<Bound, Timestamp>,
    /// Bound panes already reported exited or gone.
    gone: HashSet<Bound>,
}

impl HolderSource {
    pub fn new(
        link: Arc<dyn Link>,
        store: Store,
        host_id: impl Into<String>,
        stable: Option<PathBuf>,
    ) -> Self {
        Self {
            link,
            store,
            host_id: host_id.into(),
            stable,
            seen: None,
            watch: None,
            received: Arc::new(AtomicU64::new(0)),
            instance: None,
            panes: HashMap::new(),
            last_list: None,
            upgrade_checked: None,
            statuses: HashMap::new(),
            held: HashMap::new(),
            gone: HashSet::new(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// How many events the watch has passed on so far.
    pub fn received(&self) -> u64 {
        self.received.load(Ordering::SeqCst)
    }

    /// Whether a watch is open.
    pub fn connected(&self) -> bool {
        self.watch.is_some()
    }

    pub fn memory(&self) -> Memory {
        let mut memory = Memory {
            statuses: self.statuses.keys().cloned().collect(),
            held: self.held.keys().cloned().collect(),
            gone: self.gone.iter().cloned().collect(),
        };
        memory.statuses.sort();
        memory.held.sort();
        memory.gone.sort();
        memory
    }

    /// `hello`, `holder.record`, `watch`, then `list`. The watch starts
    /// before the list, so no event between the two is lost.
    fn connect(&mut self, ctx: &Ctx) -> Result<(), PollError> {
        let welcome = self.link.welcome().map_err(polled)?;
        let previous_version = self
            .seen
            .as_ref()
            .filter(|(instance, _)| *instance == welcome.instance)
            .map(|(_, version)| version.clone());
        let recorded = self.store.holder_record(
            ctx,
            &HolderSeen {
                instance: welcome.instance.clone(),
                version: welcome.holder.clone(),
                boot_id: welcome.boot_id.clone(),
                previous_version,
            },
        )?;
        if let HolderRecord::Upgraded { from } = &recorded.value {
            tracing::info!(%from, to = %welcome.holder, "terminal holder upgraded");
            self.store.host_health(ctx, UPGRADE_CHECK, None)?;
        } else if self
            .binaries()
            .is_some_and(|(running, stable)| running == stable)
        {
            // A new instance or a re-exec that kept its version, on the
            // stable binary: nothing is left to upgrade.
            self.store.host_health(ctx, UPGRADE_CHECK, None)?;
        }
        self.seen = Some((welcome.instance.clone(), welcome.holder.clone()));

        let events = self.link.watch().map_err(polled)?;
        let end = events.ender();
        let (tx, rx) = mpsc::channel();
        let received = Arc::clone(&self.received);
        std::thread::Builder::new()
            .name("holder-watch".into())
            .spawn(move || {
                let mut events = events;
                loop {
                    let item = match events.recv() {
                        Ok(Some(event)) => Item::Event(event),
                        Ok(None) | Err(_) => Item::Closed,
                    };
                    let closed = matches!(item, Item::Closed);
                    if tx.send(item).is_err() {
                        return;
                    }
                    received.fetch_add(1, Ordering::SeqCst);
                    if closed {
                        return;
                    }
                }
            })
            .map_err(PollError::from_source)?;
        self.watch = Some(Watch { rx, end });
        // A new connection reads every bound pane afresh.
        self.statuses.clear();
        self.held.clear();
        self.list(ctx)?;
        self.store.host_seen(ctx, &self.host_id, None)?;
        Ok(())
    }

    /// `list` into the cache, and the pane map the events update.
    fn list(&mut self, ctx: &Ctx) -> Result<(), PollError> {
        let listing = self.link.list().map_err(polled)?;
        let rows = readings(&self.store, &listing)?;
        self.store
            .cache_panes(ctx, &self.host_id, PaneHolder::Holder, rows)?;
        self.panes = listing
            .panes
            .into_iter()
            .map(|pane| (pane.pane.clone(), pane))
            .collect();
        self.instance = Some(listing.instance);
        self.last_list = Some(ctx.now);
        Ok(())
    }

    /// Whether the last list came from the recorded instance. A new
    /// instance is R21's to restore: until then no pane of the old one is
    /// reported gone.
    fn listed_recorded(&self) -> Result<bool, PollError> {
        let recorded = tiller_core::state::projects::meta(self.store.conn(), "holder_instance")?;
        Ok(recorded.is_some() && recorded == self.instance)
    }

    fn bindings(&self) -> Result<Vec<Binding>, PollError> {
        let sessions: Vec<Session> = list(
            self.store.conn(),
            "host_id = ?1 AND ended_at IS NULL AND pane_id IS NOT NULL ORDER BY started_at, id",
            &[&self.host_id],
        )?;
        Ok(sessions
            .into_iter()
            .filter_map(|s| {
                let pane_id = s.pane_id.filter(|p| tiller_core::ids::is_holder_pane(p))?;
                Some(Binding {
                    session_id: s.id,
                    run_id: s.run_id,
                    pane_id,
                })
            })
            .collect())
    }

    fn record(
        &mut self,
        ctx: &Ctx,
        binding: &Binding,
        event: String,
        verdict: Option<Activity>,
        payload: serde_json::Value,
    ) -> Result<Option<Timestamp>, PollError> {
        let mut payload = payload;
        if let Some(map) = payload.as_object_mut() {
            map.insert("pane_id".into(), json!(binding.pane_id));
            map.insert("host_id".into(), json!(self.host_id));
        }
        let recorded = self.store.observation_record(
            ctx,
            NewObservation {
                session_id: Some(binding.session_id.clone()),
                run_id: binding.run_id.clone(),
                source: ObservationSource::Holder,
                event,
                verdict,
                observed_at: ctx.now,
                payload,
            },
        )?;
        Ok(recorded.value.idle_held_until)
    }

    /// The bound panes' observations: exits and gone panes once, screen
    /// readings on `quiet`, on first sight and when a hold ends, and the
    /// holder's `notify` and `model_reset` as they come.
    fn observe(
        &mut self,
        ctx: &Ctx,
        quiet: &HashSet<PaneId>,
        marks: &[(PaneId, &'static str, serde_json::Value)],
    ) -> Result<(), PollError> {
        let bindings = self.bindings()?;
        let bound: HashSet<Bound> = bindings
            .iter()
            .map(|b| (b.pane_id.clone(), b.session_id.clone()))
            .collect();
        self.statuses.retain(|key, _| bound.contains(key));
        self.held.retain(|key, _| bound.contains(key));
        self.gone.retain(|key| bound.contains(key));
        let recorded = self.listed_recorded()?;
        for binding in &bindings {
            let key: Bound = (binding.pane_id.clone(), binding.session_id.clone());
            let id = PaneId::from(binding.pane_id.clone());
            for (_, event, payload) in marks.iter().filter(|(pane, _, _)| *pane == id) {
                self.record(ctx, binding, (*event).to_owned(), None, payload.clone())?;
            }
            let pane = self.panes.get(&id).cloned();
            match pane {
                None if !recorded => continue,
                None => {
                    if self.gone.contains(&key) {
                        continue;
                    }
                    self.record(
                        ctx,
                        binding,
                        "pane_gone".into(),
                        Some(Activity::Ended),
                        json!({}),
                    )?;
                    self.forget(key);
                }
                Some(pane) if pane.state == tiller_panes::message::PaneState::Exited => {
                    if self.gone.contains(&key) {
                        continue;
                    }
                    let exit = pane.exit;
                    self.record(
                        ctx,
                        binding,
                        "holder:exited".into(),
                        Some(Activity::Ended),
                        json!({
                            "exit_code": exit.and_then(|e| e.code),
                            "exit_signal": exit.and_then(|e| e.signal),
                        }),
                    )?;
                    self.forget(key);
                }
                Some(_) => {
                    self.gone.remove(&key);
                    let due = self.held.get(&key).is_some_and(|until| ctx.now >= *until);
                    let first = !self.statuses.contains_key(&key);
                    if !(first || due || quiet.contains(&id)) {
                        continue;
                    }
                    let screen = match self.link.screen(&id, Read::Visible, Format::Ansi) {
                        Ok(screen) => screen,
                        // Gone or exited since the list: the next list says.
                        Err(PanesError::NotFound(_) | PanesError::Exited(_)) => continue,
                        Err(error) => return Err(polled(error)),
                    };
                    let status = screen_status(&screen.text()).map(str::to_owned);
                    let unchanged = self.statuses.get(&key) == Some(&status);
                    if unchanged && !due {
                        continue;
                    }
                    let Some(text) = status.clone() else {
                        self.held.remove(&key);
                        self.statuses.insert(key, None);
                        continue;
                    };
                    let session: Session = get(self.store.conn(), "session", &binding.session_id)?;
                    let hook_working = session.activity == Activity::Working
                        && session.activity_source == Some(ObservationSource::Hook);
                    // The resend is owed only while a hook `working` still
                    // holds the session: a newer hook verdict has replaced
                    // it otherwise.
                    if unchanged && !hook_working {
                        self.held.remove(&key);
                        continue;
                    }
                    let newest_hook = (session.activity_source == Some(ObservationSource::Hook))
                        .then_some(session.activity);
                    let held = self.record(
                        ctx,
                        binding,
                        format!("holder:{text}"),
                        holder_verdict(&text, newest_hook),
                        json!({"status": text}),
                    )?;
                    self.statuses.insert(key.clone(), status);
                    match held {
                        Some(until) => self.held.insert(key, until),
                        None => self.held.remove(&key),
                    };
                }
            }
        }
        Ok(())
    }

    fn forget(&mut self, key: Bound) {
        self.statuses.remove(&key);
        self.held.remove(&key);
        self.gone.insert(key);
    }

    /// The binary the holder runs and the canonical stable profile path,
    /// when both can be read.
    fn binaries(&self) -> Option<(PathBuf, PathBuf)> {
        let stable = std::fs::canonicalize(self.stable.as_ref()?).ok()?;
        Some((self.link.running_binary()?, stable))
    }

    /// R22's trigger: at the first connect and after each `config.changed`,
    /// a holder that runs another binary than the stable profile path is
    /// asked to upgrade, once. A holder on the stable binary clears a
    /// refusal left from before.
    fn check_upgrade(&mut self, ctx: &Ctx) -> Result<(), PollError> {
        let changed = self.store.config_changed_at()?;
        if self.upgrade_checked == Some(changed) {
            return Ok(());
        }
        self.upgrade_checked = Some(changed);
        let Some((running, stable)) = self.binaries() else {
            return Ok(());
        };
        if running == stable {
            self.store.host_health(ctx, UPGRADE_CHECK, None)?;
            return Ok(());
        }
        match self.link.upgrade(&stable) {
            Ok(Upgrade::Upgrading) => {
                tracing::info!(running = %running.display(), stable = %stable.display(), "terminal holder upgrading");
            }
            Ok(Upgrade::Refused(reason)) => {
                tracing::warn!(%reason, "terminal holder upgrade refused");
                self.store.host_health(
                    ctx,
                    UPGRADE_CHECK,
                    Some(&format!("holder upgrade refused: {reason}")),
                )?;
            }
            Err(error) => {
                tracing::warn!(%error, "terminal holder upgrade not asked");
            }
        }
        Ok(())
    }
}

impl Source for HolderSource {
    fn name(&self) -> String {
        "holder".to_owned()
    }

    fn interval(&self) -> Duration {
        INTERVAL
    }

    fn retry_after(&self, failures: u32) -> Duration {
        backoff(failures).min(BACKOFF_CAP)
    }

    /// Drains the watch, reconnecting when it closed, lists when an event
    /// or the 30 s check asks, then records the bound panes'
    /// observations.
    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        let ctx = Ctx::at(Actor::Daemon, now);
        let mut quiet = HashSet::new();
        let mut marks = Vec::new();
        let mut relist = false;
        let mut closed = false;
        if let Some(watch) = &self.watch {
            while let Ok(item) = watch.rx.try_recv() {
                let event = match item {
                    Item::Event(event) => event,
                    Item::Closed => {
                        closed = true;
                        break;
                    }
                };
                match event {
                    Event::Quiet { pane, .. } => {
                        quiet.insert(pane);
                    }
                    Event::Spawned { .. } | Event::Forgotten { .. } => relist = true,
                    Event::Exited { pane, exit } => {
                        if let Some(known) = self.panes.get_mut(&pane) {
                            known.state = tiller_panes::message::PaneState::Exited;
                            known.exit = Some(exit);
                        }
                        relist = true;
                    }
                    Event::ModelReset { pane } => {
                        marks.push((pane, "holder:model_reset", json!({})))
                    }
                    Event::Notify { pane, title, body } => {
                        marks.push((pane, "holder:notify", json!({"title": title, "body": body})));
                    }
                    // A live upgrade ends every connection: connect again.
                    Event::Upgraded { .. } => closed = true,
                    _ => {}
                }
            }
        }
        if closed {
            self.watch = None;
        }
        if self.watch.is_none() {
            self.connect(&ctx)?;
        } else if relist
            || self
                .last_list
                .is_none_or(|at| now.as_millis() - at.as_millis() >= LIST_EVERY.as_millis() as i64)
        {
            self.list(&ctx)?;
        }
        self.observe(&ctx, &quiet, &marks)?;
        self.check_upgrade(&ctx)
    }

    /// R18: three failures in a row mark the host `holder_down`. The cache
    /// keeps every pane: a failed connect is no evidence that one is gone.
    fn failed(&mut self, now: Timestamp, failures: u32, _error: &PollError) {
        self.watch = None;
        if failures < STALE_AFTER {
            return;
        }
        let ctx = Ctx::at(Actor::Daemon, now);
        if let Err(error) = self.store.host_seen(&ctx, &self.host_id, Some(HOLDER_DOWN)) {
            tracing::warn!(%error, host = %self.host_id, "could not mark the host stale");
        }
    }
}
