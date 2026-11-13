//! The reconcile loop (`recovery.md` section 3): one pass every 30 seconds,
//! and one at once after any event, any poller change and the daemon's
//! start. The pass runs its steps in order, each repair in its own
//! transaction, and records nothing when it repairs nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_core::domain::{Card, OutboxKind, Project, Session};
use tiller_core::ids::HOST_HEALTH_CARD;
use tiller_core::state::attention::NewAttention;
use tiller_core::state::caches::{PaneHolder, WorktreeReading, landed_heads};
use tiller_core::state::outbox::{NewMessage, live_orchestrator};
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Ctx, Result, Store, Timestamp};
use tiller_herdr::client::{Herdr, Snapshot};
use tiller_herdr::layout;
use tiller_panes::message::Listing;

use crate::holder::Link;
use crate::recovery::sessions::Restore;
use crate::sources::{AgentProbe, Forge, Harness, Launcher, Readings, Scanner, WorktreeScan};
use crate::thresholds::Thresholds;

/// What the daemon tells the engine about this host.
#[derive(Clone, Debug)]
pub struct Config {
    /// The local host's id (`hosts.id`).
    pub host_id: String,
    /// `~/.tiller`: the owner marker, the hook spool, the state directory.
    pub tiller_home: PathBuf,
    /// The current boot id, when the host reports one. `None` never opens
    /// the reboot check (R04).
    pub boot_id: Option<String>,
    /// Each project's checkout path, from the config file: where an
    /// orchestrator resumes (R02). Workers resume in their own worktree.
    pub project_paths: HashMap<String, PathBuf>,
    /// The owner's forge login (CP2, C08), named on an outside merge.
    pub forge_login: Option<String>,
    pub thresholds: Thresholds,
    /// The learning loop's `[learning]` keys, for the retro sweep (step 5a).
    pub learning: tiller_core::state::learning::Learning,
}

impl Config {
    pub fn new(host_id: impl Into<String>, tiller_home: impl Into<PathBuf>) -> Self {
        Self {
            host_id: host_id.into(),
            tiller_home: tiller_home.into(),
            boot_id: None,
            project_paths: HashMap::new(),
            forge_login: None,
            thresholds: Thresholds::default(),
            learning: tiller_core::state::learning::Learning::default(),
        }
    }
}

/// The world the engine reads through (`crate::sources`).
pub struct Sources {
    pub herdr: Box<dyn Herdr + Send>,
    /// The local terminal holder, when this host runs one.
    pub holder: Option<Arc<dyn Link>>,
    pub probe: Box<dyn AgentProbe>,
    pub scanner: Box<dyn Scanner>,
    pub forge: Box<dyn Forge>,
    pub launcher: Box<dyn Launcher>,
    pub readings: Box<dyn Readings>,
    pub harness: Box<dyn Harness>,
}

/// What one pass did. Every line names the row and the entity, for the
/// daemon's log and for tests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// The owner marker names another system: nothing was written (R20).
    pub read_only: bool,
    /// This pass had a successful herdr snapshot.
    pub snapshot_ok: bool,
    /// Repairs, one line each.
    pub repairs: Vec<String>,
    /// Errors from single repairs; the pass went on past each.
    pub errors: Vec<String>,
}

impl Report {
    pub(crate) fn repair(&mut self, line: impl Into<String>) {
        let line = line.into();
        tracing::info!(%line, "reconcile repair");
        self.repairs.push(line);
    }

    pub(crate) fn error(&mut self, what: &str, error: &dyn std::fmt::Display) {
        let line = format!("{what}: {error}");
        tracing::warn!(%line, "reconcile step failed");
        self.errors.push(line);
    }

    /// Whether any repair line contains `needle`.
    pub fn did(&self, needle: &str) -> bool {
        self.repairs.iter().any(|line| line.contains(needle))
    }
}

/// What [`Reconciler::breach`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Breach {
    /// Recorded by this call: the step reports it.
    Written,
    /// Recorded by an earlier pass.
    Recorded,
    /// Not recorded, and reported: a later pass tries again.
    Failed,
}

impl Breach {
    pub(crate) fn written(self) -> bool {
        self == Breach::Written
    }
}

/// The engine: one `state.db` connection, the sources, and the little the
/// pass remembers between runs (cadences, never deadlines).
pub struct Reconciler {
    pub(crate) store: Store,
    pub(crate) config: Config,
    pub(crate) sources: Sources,
    pub(crate) started_at: Option<Timestamp>,
    pub(crate) last_host_check: Option<Timestamp>,
    pub(crate) last_scan: Option<Timestamp>,
    pub(crate) last_launch: Option<Timestamp>,
    /// Passes refused by the owner marker since the last full pass, so the
    /// flip back runs a full pass (R20).
    pub(crate) was_read_only: bool,
    /// This pass's herdr snapshot, terminal holder `list` and worktree
    /// scans.
    pub(crate) snapshot: Option<Snapshot>,
    pub(crate) listing: Option<Listing>,
    pub(crate) scans: HashMap<PathBuf, WorktreeScan>,
    /// Pass keys another actor stored first that this engine has reported,
    /// with that actor and when it stored the key, so each refusal is
    /// reported once, not on every pass.
    pub(crate) planted: HashMap<String, (String, Timestamp)>,
    /// Refusals the keyed-write helpers met and no returned report carried
    /// yet: the pass key and one line, at most one per key. A newer refusal
    /// of a key replaces the older one, so a hold that passed to another
    /// actor while passes failed is reported as it now stands.
    pub(crate) refusals: Vec<(String, String)>,
}

impl Reconciler {
    pub fn new(store: Store, config: Config, sources: Sources) -> Self {
        Self {
            store,
            config,
            sources,
            started_at: None,
            last_host_check: None,
            last_scan: None,
            last_launch: None,
            was_read_only: false,
            snapshot: None,
            listing: None,
            scans: HashMap::new(),
            planted: HashMap::new(),
            refusals: Vec::new(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub(crate) fn ctx(&self, now: Timestamp) -> Ctx {
        Ctx::at(Actor::Daemon, now)
    }

    /// One pass, every step in order.
    pub fn pass(&mut self, now: Timestamp) -> Result<Report> {
        let mut report = Report::default();
        // The keyed-write helpers take no report: their refusals join it
        // here. A pass that fails drops its report, so its refusals wait
        // for the next report a pass returns, and their keys stay planted.
        self.steps(now, &mut report)?;
        for (_, line) in std::mem::take(&mut self.refusals) {
            tracing::warn!(%line, "reconcile step failed");
            report.errors.push(line);
        }
        Ok(report)
    }

    fn steps(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        // The store refuses every write under the marker too (R20); the
        // pass stops here so it does no outside work either.
        if let Err(error) = tiller_core::owner::check(&self.config.tiller_home) {
            tracing::info!(%error, "reconcile pass skipped");
            report.read_only = true;
            self.was_read_only = true;
            return Ok(());
        }
        let flipped_back = std::mem::take(&mut self.was_read_only);
        self.snapshot = match self.sources.herdr.snapshot() {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                tracing::debug!(%error, "herdr snapshot failed; the ladder stops at unknown");
                None
            }
        };
        report.snapshot_ok = self.snapshot.is_some();
        self.listing = match &self.sources.holder {
            Some(holder) => match holder.list() {
                Ok(listing) => Some(listing),
                Err(error) => {
                    tracing::debug!(%error, "holder list failed; the ladder stops at unknown");
                    None
                }
            },
            None => None,
        };
        self.scans.clear();

        // Step 1: startup, once.
        if self.started_at.is_none() {
            if let Err(error) = self.startup(now, report) {
                report.error("startup", &error);
                return Ok(());
            }
            self.started_at = Some(now);
        }
        if flipped_back {
            report.repair(
                "R20 owner marker names tiller again: full pass with live evidence rebound",
            );
        }
        if let Some(snapshot) = self.snapshot.clone() {
            // The pane cache the pass reads is the one it just saw.
            match layout::bindings(self.store.conn(), &self.config.host_id) {
                Ok(bindings) => {
                    let readings = tiller_herdr::poller::readings(&snapshot, &bindings);
                    if let Err(error) = self.store.cache_panes(
                        &self.ctx(now),
                        &self.config.host_id,
                        PaneHolder::Herdr,
                        readings,
                    ) {
                        report.error("pane cache", &error);
                    }
                }
                Err(error) => report.error("pane bindings", &error),
            }
        }
        if let Some(listing) = &self.listing {
            let rows = crate::holder::readings(&self.store, listing);
            let cached = rows.and_then(|rows| {
                self.store.cache_panes(
                    &self.ctx(now),
                    &self.config.host_id,
                    PaneHolder::Holder,
                    rows,
                )
            });
            if let Err(error) = cached {
                report.error("holder pane cache", &error);
            }
        }
        if let Err(error) = self.import_spool() {
            report.error("hook spool", &error);
        }

        // Step 2: host checks, every five minutes.
        if self
            .last_host_check
            .is_none_or(|at| at.plus_minutes(self.config.thresholds.host_check) <= now)
        {
            self.last_host_check = Some(now);
            if let Err(error) = self.host_checks(now, report) {
                report.error("host checks", &error);
            }
        }
        // Step 3: timers.
        if let Err(error) = self.timers(now, report) {
            report.error("timers", &error);
        }
        // The worktree scan, every five minutes and whenever a resume or a
        // cleanup may happen this pass (they re-check from the scan taken
        // in this pass).
        if self
            .last_scan
            .is_none_or(|at| at.plus_minutes(self.config.thresholds.worktree_scan) <= now)
            || self.restore_pending()?
        {
            self.last_scan = Some(now);
            if let Err(error) = self.scan_worktrees(now) {
                report.error("worktree scan", &error);
            }
        }
        // Step 4: sessions.
        if let Err(error) = self.sessions(now, report) {
            report.error("sessions", &error);
        }
        // Step 5: runs and evidence.
        if let Err(error) = self.evidence(now, report) {
            report.error("evidence", &error);
        }
        // Step 5a: the learning loop's sweep: a retro per closed card and
        // the change set transitions (`docs/design/learning-loop.md`).
        match self
            .store
            .learning_sweep(&self.ctx(now), &self.config.learning)
        {
            Ok(swept) => {
                for retro in &swept.value.scheduled {
                    report.repair(format!(
                        "retro scheduled for card {}: {} ({})",
                        retro.card_id, retro.depth, retro.reason
                    ));
                }
                for set in &swept.value.measuring {
                    report.repair(format!("change set {set} is measuring"));
                }
                for set in &swept.value.closed {
                    report.repair(format!("change set {set} closed: window complete"));
                }
            }
            Err(error) => report.error("learning sweep", &error),
        }
        // Step 6: approvals and PRs.
        if let Err(error) = self.approvals(now, report) {
            report.error("approvals", &error);
        }
        // Step 6a: ask-user findings and event obligations, read from the
        // PR cache and review records this pass just refreshed.
        self.gates(now, report);
        // Step 7, delivery, is the delivery poller's pass (`tiller_delivery`),
        // which runs every two seconds beside this loop and owns R09.
        // Step 8: layout.
        if let Some(snapshot) = &self.snapshot {
            match layout::bindings(self.store.conn(), &self.config.host_id) {
                Ok(bindings) => {
                    match layout::repair(self.sources.herdr.as_ref(), snapshot, &bindings) {
                        Ok(repair) if repair.changed() => {
                            report.repair(format!("R18 layout repaired: {repair:?}"));
                        }
                        Ok(_) => {}
                        Err(error) => report.error("layout", &error),
                    }
                }
                Err(error) => report.error("layout bindings", &error),
            }
        }
        Ok(())
    }

    fn import_spool(&mut self) -> Result<()> {
        let env = tiller_observe::hooks::Env {
            tiller_home: self.config.tiller_home.clone(),
            user_home: None,
            tiller_session_id: None,
        };
        if !self.config.tiller_home.join("hook-spool").is_dir() {
            return Ok(());
        }
        tiller_observe::hooks::import_spool(&mut self.store, &env).map(drop)
    }

    // ----- shared helpers the row modules use -----

    pub(crate) fn conn(&self) -> &Connection {
        self.store.conn()
    }

    pub(crate) fn health_card(&self) -> Result<Card> {
        find(self.conn(), "display_id = ?1", &[&HOST_HEALTH_CARD])?.ok_or_else(|| {
            tiller_core::Error::NotFound {
                entity: "card",
                id: HOST_HEALTH_CARD.to_owned(),
                candidates: Vec::new(),
            }
        })
    }

    /// A keyed note to the project's live orchestrator. Returns whether one
    /// was written now (false: no orchestrator, already sent, or the key is
    /// another actor's, which [`Self::planted`] reports).
    pub(crate) fn note(
        &mut self,
        now: Timestamp,
        project_id: &str,
        key: &str,
        body: String,
        card_id: Option<String>,
    ) -> Result<bool> {
        if self.still_planted(key)? {
            return Ok(false);
        }
        let Some(orchestrator) = live_orchestrator(self.conn(), project_id)? else {
            return Ok(false);
        };
        let applied = self.store.outbox_enqueue(
            &self.ctx(now),
            Some(key),
            NewMessage {
                target_session_id: orchestrator.id,
                kind: OutboxKind::Note,
                body,
                card_id,
                reply_to: None,
            },
        );
        Ok(self.planted(key, applied)?.is_some_and(|a| !a.replayed))
    }

    /// A keyed `wake` to one live session. False when already sent, or when
    /// the key is another actor's ([`Self::planted`]).
    pub(crate) fn wake(
        &mut self,
        now: Timestamp,
        session: &str,
        key: &str,
        body: String,
    ) -> Result<bool> {
        if self.still_planted(key)? {
            return Ok(false);
        }
        let applied = self.store.outbox_enqueue(
            &self.ctx(now),
            Some(key),
            NewMessage {
                target_session_id: session.to_owned(),
                kind: OutboxKind::Wake,
                body,
                card_id: None,
                reply_to: None,
            },
        );
        Ok(self.planted(key, applied)?.is_some_and(|a| !a.replayed))
    }

    /// A keyed `recovery` item on a card (or on `fleet-1`). Returns whether
    /// it was opened now (false: the card is not open, the item is already
    /// open, or the key is another actor's, which [`Self::planted`]
    /// reports).
    pub(crate) fn recovery(
        &mut self,
        now: Timestamp,
        key: &str,
        mut item: NewAttention,
    ) -> Result<bool> {
        if self.still_planted(key)? {
            return Ok(false);
        }
        item.kind = tiller_core::domain::AttentionKind::Recovery;
        if item.card.is_empty() {
            item.card = HOST_HEALTH_CARD.to_owned();
        }
        let card = tiller_core::state::resolve_card(self.conn(), &item.card)?;
        if card.state != tiller_core::domain::CardState::Open {
            return Ok(false);
        }
        let applied = self.store.attention_open(&self.ctx(now), Some(key), item);
        Ok(self.planted(key, applied)?.is_some_and(|a| !a.replayed))
    }

    /// Whether this engine already met `key` held by another actor, and
    /// that same hold is still there: the write would be refused again, so
    /// the helpers skip it and the work before it. A key whose hold lapsed
    /// or passed to another actor is forgotten and tried again, so a later
    /// refusal of it is reported.
    pub(crate) fn still_planted(&mut self, key: &str) -> Result<bool> {
        let Some(met) = self.planted.get(key) else {
            return Ok(false);
        };
        if self.key_holder(key)?.as_ref() == Some(met) {
            return Ok(true);
        }
        self.planted.remove(key);
        Ok(false)
    }

    /// A keyed write's result, with the store's refusal of a pass key
    /// another actor stored first (`protocol.md` 6.3) turned into `None`.
    /// The refusal is reported once per key while this engine runs, and the
    /// pass goes on: the step's later writes still go out. Every other
    /// error is returned as it was.
    pub(crate) fn planted<T>(&mut self, key: &str, result: Result<T>) -> Result<Option<T>> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error @ tiller_core::Error::Refused { .. }) => {
                let Some(holder) = self.key_holder(key)? else {
                    return Err(error);
                };
                if self.planted.get(key) != Some(&holder) {
                    self.planted.insert(key.to_owned(), holder);
                    self.refusals.retain(|(queued, _)| queued != key);
                    self.refusals
                        .push((key.to_owned(), format!("pass key {key}: {error}")));
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Another actor than the pass that stored `key`, and when (`protocol.md`
    /// 6.3). None when the key is unused, the daemon's, or from before
    /// schema 15.
    pub(crate) fn key_holder(&self, key: &str) -> Result<Option<(String, Timestamp)>> {
        let actor = Actor::Daemon.to_string();
        Ok(self
            .conn()
            .query_row(
                "SELECT actor, created_at FROM idempotency_keys
                  WHERE key = ?1 AND actor IS NOT NULL AND actor != ?2",
                [key, actor.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// When the pass stored the keyed message, if it did. A row another
    /// actor stored under the key is not the pass's message (`protocol.md`
    /// 6.3). When the pass tried the message and the store refused it
    /// because another actor holds the key ([`Self::planted`]), the clock
    /// starts when that actor stored the key: a planted key can bring the
    /// next step forward, never hold it back. The planted time counts only
    /// while that hold is still there ([`Self::still_planted`]), so a hold
    /// that lapsed or passed to another actor starts no clock from its old
    /// time. A message the pass never tried (no live orchestrator) starts
    /// no clock. The try is known only while this engine runs: after a
    /// restart the clock waits for the next try.
    pub(crate) fn message_at(&mut self, key: &str) -> Result<Option<Timestamp>> {
        let actor = Actor::Daemon.to_string();
        let own = self
            .conn()
            .query_row(
                "SELECT created_at FROM outbox WHERE idempotency_key = ?1 AND created_by = ?2",
                [key, actor.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if own.is_some() || !self.still_planted(key)? {
            return Ok(own);
        }
        Ok(self.planted.get(key).map(|(_, at)| *at))
    }

    /// Records `guard`'s breach once, under the pass key `key`. When
    /// another actor stored the key first (`protocol.md` 6.3), the store
    /// refuses it: the breach is then recorded once without the key, its
    /// details naming the key (`pass_key`) and who held it (`key_held_by`),
    /// and the refusal is reported with it. Any other failure is reported,
    /// and the step goes on: its note and recovery item still run.
    pub(crate) fn breach(
        &mut self,
        now: Timestamp,
        key: String,
        guard: &str,
        card_id: &str,
        details: Value,
        report: &mut Report,
    ) -> Breach {
        match self.try_breach(now, &key, guard, card_id, details) {
            Ok(breach) => breach,
            Err(error) => {
                report.error(&format!("{guard} breach {key}"), &error);
                Breach::Failed
            }
        }
    }

    fn try_breach(
        &mut self,
        now: Timestamp,
        key: &str,
        guard: &str,
        card_id: &str,
        details: Value,
    ) -> Result<Breach> {
        if self.key_used(key)? || self.breach_recorded_without_key(guard, key)? {
            return Ok(Breach::Recorded);
        }
        let keyed = self.store.guard_breach(
            &self.ctx(now).with_key(key),
            guard,
            Some(card_id),
            details.clone(),
        );
        if self.planted(key, keyed)?.is_some() {
            return Ok(Breach::Written);
        }
        let held_by = self.key_holder(key)?.map(|(actor, _)| actor);
        let mut details = match details {
            Value::Object(map) => map,
            other => serde_json::Map::from_iter([("details".to_owned(), other)]),
        };
        details.insert("pass_key".into(), json!(key));
        details.insert("key_held_by".into(), json!(held_by));
        self.store
            .guard_breach(&self.ctx(now), guard, Some(card_id), Value::Object(details))?;
        Ok(Breach::Written)
    }

    /// Whether the pass already recorded `guard`'s breach without its key,
    /// because another actor held `key` (see [`Self::breach`]). Only the
    /// daemon writes `daemon` events, so no session can fake this record.
    fn breach_recorded_without_key(&self, guard: &str, key: &str) -> Result<bool> {
        let actor = Actor::Daemon.to_string();
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM events
                             WHERE entity = 'guard' AND entity_id = ?1
                               AND kind = 'guard.breached' AND actor = ?2
                               AND json_extract(payload, '$.pass_key') = ?3)",
            [guard, actor.as_str(), key],
            |row| row.get(0),
        )?)
    }

    /// Whether the pass's own mutation under `key` already ran: the key is
    /// remembered, stored by the daemon, or by no recorded actor (a key from
    /// before schema 15). A key another actor stored is not the pass's write
    /// (`protocol.md` 6.3): the pass writes, and the store refuses the key,
    /// so a session cannot silence a pass's breach by using its key first.
    pub(crate) fn key_used(&self, key: &str) -> Result<bool> {
        // The actor of every pass write (`Reconciler::ctx`).
        let actor = Actor::Daemon.to_string();
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM idempotency_keys
                             WHERE key = ?1 AND (actor IS NULL OR actor = ?2))",
            [key, actor.as_str()],
            |row| row.get(0),
        )?)
    }

    /// The newest event of `kind` on `entity`/`entity_id`, as `(at, payload)`.
    pub(crate) fn last_event(
        &self,
        entity: &str,
        entity_id: &str,
        kind: &str,
    ) -> Result<Option<(Timestamp, Value)>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT at, payload FROM events WHERE entity = ?1 AND entity_id = ?2 AND kind = ?3
                 ORDER BY seq DESC LIMIT 1",
                [entity, entity_id, kind],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Whether a hook observation for the session was recorded after `since`:
    /// the proof of life a resume waits for.
    pub(crate) fn hook_seen_after(&self, session: &str, since: Timestamp) -> Result<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM observations WHERE session_id = ?1 AND source = 'hook' AND recorded_at > ?2)",
            rusqlite::params![session, since],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn project(&self, id: &str) -> Result<Project> {
        get(self.conn(), "project", id)
    }

    /// Live sessions on the local host, orchestrators first, then oldest.
    pub(crate) fn live_sessions(&self) -> Result<Vec<Session>> {
        list(
            self.conn(),
            "host_id = ?1 AND ended_at IS NULL
             ORDER BY CASE role WHEN 'orchestrator' THEN 0 ELSE 1 END, started_at, id",
            &[&self.config.host_id],
        )
    }

    /// The restore still open, if any: a known boot id that
    /// `meta.boot_id` does not match yet (R04), else a holder `list` of
    /// another instance than `meta.holder_instance` (R21). A reboot covers
    /// a lost holder too and writes the new instance with the boot id.
    pub(crate) fn open_restore(&self) -> Result<Option<Restore>> {
        if let Some(current) = &self.config.boot_id {
            let recorded = tiller_core::state::projects::meta(self.conn(), "boot_id")?;
            if recorded.is_some_and(|recorded| recorded != *current) {
                return Ok(Some(Restore::Reboot {
                    boot_id: current.clone(),
                }));
            }
        }
        let Some(listing) = &self.listing else {
            return Ok(None);
        };
        let known = tiller_core::state::projects::meta(self.conn(), "holder_instance")?;
        Ok(known
            .filter(|known| *known != listing.instance)
            .map(|_| Restore::HolderLost {
                instance: listing.instance.clone(),
            }))
    }

    /// Whether a restore (R04 or R21) owns the live sessions this pass.
    pub(crate) fn restore_pending(&self) -> Result<bool> {
        Ok(self.open_restore()?.is_some())
    }

    /// The bounded pane tail a note carries (P22): the last 40 visible lines.
    pub(crate) fn pane_tail(&self, pane_id: &str) -> String {
        match self.sources.herdr.read_visible(pane_id) {
            Ok(screen) => {
                let lines: Vec<&str> = screen.lines().collect();
                let start = lines.len().saturating_sub(40);
                lines[start..].join("\n")
            }
            Err(error) => format!("(pane unreadable: {error})"),
        }
    }

    // ----- the worktree scan -----

    /// Scans every worktree the records or treehouse name and refreshes the
    /// cache in one snapshot: a path that no longer exists is positively
    /// gone, an unreadable one keeps unknowns, and a lister that failed
    /// drops nothing.
    pub(crate) fn scan_worktrees(&mut self, now: Timestamp) -> Result<()> {
        let host = self.config.host_id.clone();
        let mut paths: Vec<(PathBuf, Option<String>, Option<String>)> = Vec::new();
        let rows: Vec<(String, Option<String>, String)> = {
            let mut stmt = self.conn().prepare(
                "SELECT r.worktree_path, r.id, c.project_id FROM runs r JOIN cards c ON c.id = r.card_id
                 WHERE r.host_id = ?1 AND r.worktree_path IS NOT NULL ORDER BY r.started_at",
            )?;
            let rows =
                stmt.query_map([&host], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (path, run_id, project_id) in rows {
            paths.push((PathBuf::from(path), run_id, Some(project_id)));
        }
        let cached: Vec<(String, Option<String>, Option<String>)> = {
            let mut stmt = self
                .conn()
                .prepare("SELECT path, run_id, project_id FROM worktrees WHERE host_id = ?1 AND gone_at IS NULL")?;
            let rows =
                stmt.query_map([&host], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (path, run_id, project_id) in cached {
            let path = PathBuf::from(path);
            if !paths.iter().any(|(p, _, _)| *p == path) {
                paths.push((path, run_id, project_id));
            }
        }
        if let Ok(listed) = self.sources.scanner.listed() {
            let known: Vec<PathBuf> = paths.iter().map(|(p, _, _)| canonical(p)).collect();
            for path in listed {
                if !known.contains(&canonical(&path)) {
                    paths.push((path, None, None));
                }
            }
        }
        let projects: Vec<Project> = list(self.conn(), "1 = 1", &[])?;
        let mut readings = Vec::with_capacity(paths.len());
        for (path, run_id, project_id) in paths {
            // Resolve the run from the latest active run on the path, so a
            // treehouse-listed path still maps to its run.
            let run_id = match run_id {
                Some(id) => Some(id),
                None => self
                    .conn()
                    .query_row(
                        "SELECT id FROM runs WHERE host_id = ?1 AND worktree_path = ?2 ORDER BY ended_at IS NOT NULL, started_at DESC LIMIT 1",
                        rusqlite::params![host, path.to_string_lossy()],
                        |row| row.get(0),
                    )
                    .optional()?,
            };
            let project = project_id
                .as_deref()
                .and_then(|id| projects.iter().find(|p| p.id == id))
                .cloned();
            let scan = match &project {
                Some(project) => {
                    let landed = landed_heads(self.conn(), &host, &path.to_string_lossy())?;
                    self.sources.scanner.scan(&path, project, &landed)
                }
                None => WorktreeScan {
                    exists: path.exists(),
                    ..WorktreeScan::missing(&path)
                },
            };
            if scan.exists {
                readings.push(WorktreeReading {
                    path: path.to_string_lossy().into_owned(),
                    project_id: project.map(|p| p.id),
                    run_id,
                    branch: scan.branch.clone(),
                    head_sha: scan.head_sha.clone(),
                    dirty: scan.dirty,
                    unlanded_commits: scan.unlanded_commits,
                });
            }
            self.scans.insert(path, scan);
        }
        self.store
            .cache_worktrees(&self.ctx(now), &host, readings)?;
        Ok(())
    }

    /// This pass's scan of one path, scanning it now when the pass has not.
    pub(crate) fn scan_of(&mut self, path: &Path, project: &Project) -> WorktreeScan {
        if let Some(scan) = self.scans.get(path) {
            return scan.clone();
        }
        let landed = landed_heads(self.conn(), &self.config.host_id, &path.to_string_lossy())
            .unwrap_or_default();
        let scan = self.sources.scanner.scan(path, project, &landed);
        self.scans.insert(path.to_owned(), scan.clone());
        scan
    }

    /// Evidence JSON for an item or a note.
    pub(crate) fn json_text(value: Value) -> Option<String> {
        Some(value.to_string())
    }
}

/// Where `path` leads once every symlink is followed, or `path` itself
/// when it cannot be resolved (it is gone, say).
pub(crate) fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// The age of `at` in whole minutes at `now`.
pub(crate) fn minutes_since(at: Timestamp, now: Timestamp) -> i64 {
    (now.as_millis() - at.as_millis()) / 60_000
}

/// `json!` with a timestamp rendered as text.
pub(crate) fn ts(at: Option<Timestamp>) -> Value {
    match at {
        Some(at) => json!(at.to_string()),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::testing::World;

    /// Stores `key` as `actor` stored it, as a session that ran a mutation
    /// under the pass's key first would.
    fn hold(w: &World, key: &str, actor: &str) {
        w.store()
            .conn()
            .execute(
                "INSERT OR REPLACE INTO idempotency_keys
                     (key, command, entity, entity_id, response, created_at, actor)
                 VALUES (?1, 'note.send', 'card', 'x', '{}', ?2, ?3)",
                rusqlite::params![key, Timestamp::now(), actor],
            )
            .unwrap();
    }

    fn refused() -> Result<()> {
        Err(tiller_core::Error::Refused {
            entity: "key",
            id: "k".into(),
            fact: "held by another actor".into(),
        })
    }

    /// While no report carries the queued refusals, a newer refusal of the
    /// same key replaces the older one: a hold that passed from one actor
    /// to another is reported once, as it now stands.
    #[test]
    fn a_newer_refusal_of_a_key_replaces_the_queued_one() {
        let mut w = World::new();
        let key = "reconcile:test:held";
        hold(&w, key, "session:a");
        w.reconciler().planted(key, refused()).unwrap();
        hold(&w, key, "session:b");
        w.reconciler().planted(key, refused()).unwrap();
        hold(&w, "reconcile:test:other", "session:a");
        w.reconciler()
            .planted("reconcile:test:other", refused())
            .unwrap();
        let queued = &w.reconciler().refusals;
        assert_eq!(queued.len(), 2, "{queued:?}");
    }

    /// The escalation clock of a planted key runs only while the hold the
    /// pass met is still there: a hold that lapsed starts no clock.
    #[test]
    fn a_lapsed_hold_starts_no_clock() {
        let mut w = World::new();
        let key = "reconcile:test:lapsed";
        hold(&w, key, "session:a");
        w.reconciler().planted(key, refused()).unwrap();
        assert!(w.reconciler().message_at(key).unwrap().is_some());
        w.store()
            .conn()
            .execute("DELETE FROM idempotency_keys WHERE key = ?1", [key])
            .unwrap();
        assert_eq!(w.reconciler().message_at(key).unwrap(), None);
    }
}
