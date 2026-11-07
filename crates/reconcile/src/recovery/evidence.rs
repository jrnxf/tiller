//! Step 5: records against evidence. Every active run has a live or
//! explained session, a known worktree and an open reservation; every pane,
//! worktree and reservation has an active run or is handled as an orphan
//! (R15); runs are owned by the project's live orchestrator; a done status
//! with no merge is noted (R13); a cut the harness recorded ends the run
//! (R14); live panes a restore or an owner flip left unbound are adopted
//! back by their metadata (R10, R20).

use std::path::{Path, PathBuf};

use serde_json::json;
use tiller_core::domain::{
    Card, CardState, Delivery, OutboxMessage, Pane, Project, Reservation, Run, RunOutcome, RunRole,
    Session,
};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::caches::PaneReading;
use tiller_core::state::outbox::live_orchestrator;
use tiller_core::state::publishes::{publish, published_head};
use tiller_core::state::runs::{Bind, EndRun, lease_holder, live_session};
use tiller_core::state::{find, get, list};
use tiller_core::{Result, Timestamp};
use tiller_herdr::client::Snapshot;
use tiller_herdr::layout::Binding;
use tiller_observe::liveness::Probe;

use crate::pass::{Reconciler, Report, canonical, minutes_since};

/// The pane cache rows for one snapshot, as the herdr poller builds them.
pub fn pane_readings(snapshot: &Snapshot, bindings: &[Binding]) -> Vec<PaneReading> {
    snapshot
        .panes
        .iter()
        .map(|pane| PaneReading {
            pane_id: pane.pane_id.clone(),
            workspace_label: snapshot
                .workspaces
                .iter()
                .find(|w| w.workspace_id == pane.workspace_id)
                .map(|w| w.label.clone()),
            agent_name: pane.agent.clone(),
            agent_status: pane.agent_status.clone(),
            session_id: bindings
                .iter()
                .find(|b| b.pane_id == pane.pane_id)
                .map(|b| b.session_id.clone()),
            metadata: json!(pane.tokens),
        })
        .collect()
}

impl Reconciler {
    pub(crate) fn evidence(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        self.leaked_reservations(now, report)?;
        self.reown_runs(now, report)?;
        self.retarget_orphaned_messages(now, report)?;
        self.orphan_panes(now, report)?;
        self.run_worktrees(now, report)?;
        self.done_without_evidence(now, report)?;
        self.cuts(now, report)?;
        Ok(())
    }

    /// R15: a reservation whose run ended or is missing is released.
    fn leaked_reservations(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let leaked: Vec<Reservation> = list(
            self.conn(),
            "released_at IS NULL AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.id = reservations.run_id AND r.ended_at IS NULL)
             ORDER BY reserved_at",
            &[],
        )?;
        for reservation in leaked {
            self.store
                .reservation_release(&self.ctx(now), &reservation.id)?;
            report.repair(format!(
                "R15 reservation {} released: run {} is not active",
                reservation.id, reservation.run_id
            ));
        }
        Ok(())
    }

    /// R02: every active run is owned by its project's live orchestrator.
    fn reown_runs(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let runs: Vec<Run> = list(
            self.conn(),
            "ended_at IS NULL AND supervisor_session_id NOT IN (SELECT id FROM sessions WHERE ended_at IS NULL)
             ORDER BY started_at",
            &[],
        )?;
        for run in runs {
            let card: Card = get(self.conn(), "card", &run.card_id)?;
            if live_orchestrator(self.conn(), &card.project_id)?.is_none() {
                continue;
            }
            let applied = self.store.run_reown(&self.ctx(now), &run.id)?;
            if !applied.replayed {
                report.repair(format!(
                    "R02 run {} reowned to the live orchestrator",
                    run.id
                ));
            }
        }
        Ok(())
    }

    /// R02, R09: a pending message to an ended session goes to its
    /// successor: the live session of the same run, else the project's live
    /// orchestrator. With no successor it waits.
    fn retarget_orphaned_messages(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let waiting: Vec<OutboxMessage> = list(
            self.conn(),
            "state IN ('pending', 'delivered')
             AND target_session_id IN (SELECT id FROM sessions WHERE ended_at IS NOT NULL)
             ORDER BY created_at",
            &[],
        )?;
        for message in waiting {
            let old: Session = get(self.conn(), "session", &message.target_session_id)?;
            let successor = match &old.run_id {
                Some(run) => live_session(self.conn(), run)?,
                None => live_orchestrator(self.conn(), &old.project_id)?,
            };
            let Some(successor) = successor.filter(|s| s.id != old.id) else {
                continue;
            };
            self.store
                .outbox_retarget(&self.ctx(now), &message.id, &successor.id)?;
            report.repair(format!(
                "R02 message {} retargeted from {} to {}",
                message.id, old.id, successor.id
            ));
        }
        Ok(())
    }

    /// R10, R20: a live pane whose `session` token names a live session
    /// bound to another pane (or none) is the session's pane again; the
    /// records were older than the world.
    pub(crate) fn adopt_panes(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let Some(snapshot) = self.snapshot.clone() else {
            return Ok(());
        };
        for pane in &snapshot.panes {
            let Some(named) = pane.tokens.get("session") else {
                continue;
            };
            if pane.agent.is_none() {
                continue;
            }
            let Some(session) =
                find::<Session>(self.conn(), "id = ?1 AND ended_at IS NULL", &[named])?
            else {
                continue;
            };
            if session.pane_id.as_deref() == Some(pane.pane_id.as_str())
                || session.host_id != self.config.host_id
            {
                continue;
            }
            let Some(claude_id) = session.claude_session_id.clone() else {
                continue;
            };
            self.store.run_bind_session(
                &self.ctx(now),
                Bind {
                    session_id: session.id.clone(),
                    claude_session_id: claude_id,
                    pane_id: Some(pane.pane_id.clone()),
                    transcript_path: None,
                    source: "adopt".to_owned(),
                },
            )?;
            report.repair(format!(
                "R10 session {} rebound to pane {} from its metadata (was {:?})",
                session.id, pane.pane_id, session.pane_id
            ));
        }
        Ok(())
    }

    /// R15: a pane whose metadata names a run or session that is over.
    /// A dead agent in it is closed; a live one is never closed, the
    /// orchestrator is asked, and the developer after half an hour. Two
    /// live panes naming one session go to the developer at once.
    fn orphan_panes(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let Some(snapshot) = self.snapshot.clone() else {
            return Ok(());
        };
        let host = self.config.host_id.clone();
        // Two panes for one session.
        let mut by_session: std::collections::HashMap<&str, Vec<&str>> =
            std::collections::HashMap::new();
        for pane in snapshot.panes.iter().filter(|p| p.agent.is_some()) {
            if let Some(named) = pane.tokens.get("session") {
                by_session
                    .entry(named.as_str())
                    .or_default()
                    .push(pane.pane_id.as_str());
            }
        }
        for (session_id, panes) in by_session.iter().filter(|(_, p)| p.len() > 1) {
            let Some(session) = find::<Session>(self.conn(), "id = ?1", &[session_id])? else {
                continue;
            };
            let (card, run_id) = match &session.run_id {
                Some(run) => {
                    let run: Run = get(self.conn(), "run", run)?;
                    (run.card_id, Some(run.id))
                }
                None => (String::new(), None),
            };
            let key = format!("reconcile:R15:two-panes:{session_id}");
            if self.recovery(
                now,
                &key,
                NewAttention {
                    card,
                    run_id,
                    question: format!(
                        "Two live panes ({}) claim session {session_id}. Which one is it? The other should be closed by hand.",
                        panes.join(", ")
                    ),
                    evidence: Self::json_text(json!({"session_id": session_id, "panes": panes})),
                    ..NewAttention::default()
                },
            )? {
                report.repair(format!("R15 two panes for session {session_id}: recovery item"));
            }
        }

        for pane in &snapshot.panes {
            let named_session = pane.tokens.get("session");
            let named_run = pane.tokens.get("run");
            if named_session.is_none() && named_run.is_none() {
                continue;
            }
            let session: Option<Session> = match named_session {
                Some(id) => find(self.conn(), "id = ?1", &[id])?,
                None => None,
            };
            let run: Option<Run> =
                match named_run.or(session.as_ref().and_then(|s| s.run_id.as_ref())) {
                    Some(id) => find(self.conn(), "id = ?1", &[id])?,
                    None => None,
                };
            let live_session_here = session.as_ref().is_some_and(|s| s.ended_at.is_none());
            let run_active = run.as_ref().is_some_and(|r| r.ended_at.is_none());
            // An orchestrator's pane names a live session and no run; a
            // worker's names its run, which must be active.
            if run_active || (run.is_none() && live_session_here) {
                continue;
            }
            // Orphan: its run has ended or does not exist.
            let probe = self.sources.probe.agent(pane);
            let card: Option<Card> = match &run {
                Some(run) => Some(get(self.conn(), "card", &run.card_id)?),
                None => None,
            };
            match probe {
                Probe::Dead => match self.sources.launcher.close_pane(&pane.pane_id) {
                    Ok(()) => report.repair(format!(
                        "R15 pane {} closed: its run {} ended and its agent is dead",
                        pane.pane_id,
                        run.as_ref().map(|r| r.id.as_str()).unwrap_or("(none)")
                    )),
                    Err(error) => report.error(&format!("close pane {}", pane.pane_id), &error),
                },
                Probe::Alive => {
                    let project = card
                        .as_ref()
                        .map(|c| c.project_id.clone())
                        .or_else(|| pane.tokens.get("project").cloned());
                    let Some(project) = project else { continue };
                    let key = format!("reconcile:R15:orphan:{}:{}", host, pane.pane_id);
                    // A worker's agent is the orchestrator's to stop, named by
                    // its session: a run's own session may have ended. An
                    // orchestrator's is the owner's.
                    let target = match (&run, &session) {
                        (None, _) => None,
                        (Some(_), Some(session)) => Some(session.id.clone()),
                        (Some(run), None) => find::<Session>(
                            self.conn(),
                            "run_id = ?1 ORDER BY started_at DESC, id DESC",
                            &[&run.id],
                        )?
                        .map(|s| s.id),
                    };
                    let stop = match target {
                        Some(target) => format!("stop its agent with tiller agent stop {target}"),
                        None => "close it".into(),
                    };
                    let body = format!(
                        "pane {} runs {} with no active run ({}); {stop}, or adopt it as a run?",
                        pane.pane_id,
                        pane.agent.as_deref().unwrap_or("an agent"),
                        run.as_ref()
                            .map(|r| format!("run {} ended {:?}", r.id, r.outcome))
                            .unwrap_or_else(|| "no run".into())
                    );
                    if self.note(
                        now,
                        &project,
                        &key,
                        body,
                        card.as_ref().map(|c| c.id.clone()),
                    )? {
                        report.repair(format!(
                            "R15 live pane {} with no active run: note to the orchestrator",
                            pane.pane_id
                        ));
                    }
                    if let Some(at) = self.message_at(&key)?
                        && at.plus_minutes(self.config.thresholds.orphan_note_developer) <= now
                    {
                        let item =
                            format!("reconcile:R15:orphan-recovery:{}:{}", host, pane.pane_id);
                        if self.recovery(
                            now,
                            &item,
                            NewAttention {
                                card: card.as_ref().map(|c| c.id.clone()).unwrap_or_default(),
                                question: format!(
                                    "Pane {} runs {} with no active run, and the orchestrator has not acted for {} minutes. Adopt it, or close it?",
                                    pane.pane_id,
                                    pane.agent.as_deref().unwrap_or("an agent"),
                                    minutes_since(at, now)
                                ),
                                evidence: Self::json_text(json!({"pane_id": pane.pane_id, "note_at": at.to_string()})),
                                options: Some(json!(["Adopt", "Close"])),
                                ..NewAttention::default()
                            },
                        )? {
                            report.repair(format!("R15 orphan pane {}: recovery item", pane.pane_id));
                        }
                    }
                }
                Probe::Unknown => {}
            }
        }
        Ok(())
    }

    /// R15: worktrees against runs. An active run whose worktree is missing
    /// is lost work when the last scan showed anything unlanded (a rollback
    /// trigger, G19), else a question for the orchestrator. An ended run's
    /// worktree is returned only on five positive facts; one holding work is
    /// the orchestrator's; one unreturned for a week is the developer's.
    fn run_worktrees(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let host = self.config.host_id.clone();
        let runs: Vec<Run> = list(
            self.conn(),
            "host_id = ?1 AND worktree_path IS NOT NULL ORDER BY started_at",
            &[&host],
        )?;
        // Read once, and only when an ended run needs it. Compared by where
        // each path leads, so a symlink in one spelling (`/var` against
        // `/private/var`) still matches.
        let mut listed: Option<Option<Vec<PathBuf>>> = None;
        let mut rescanned = false;
        for run in runs {
            let Some(path) = run.worktree_path.clone() else {
                continue;
            };
            let card: Card = get(self.conn(), "card", &run.card_id)?;
            let project = self.project(&card.project_id)?;
            let cached = find::<tiller_core::domain::Worktree>(
                self.conn(),
                "host_id = ?1 AND path = ?2",
                &[&host, &path],
            )?;
            let scan = self.scan_of(Path::new(&path), &project);
            if run.ended_at.is_none() {
                if scan.exists {
                    continue;
                }
                // Missing: lost work, or a question.
                let last_known_work = cached.as_ref().is_some_and(|w| {
                    w.dirty == Some(true)
                        || w.unlanded_commits.is_some_and(|n| n > 0)
                        || w.unlanded_commits.is_none()
                });
                let had_cleaned = self.last_event("run", &run.id, "run.cleaned")?.is_some();
                if last_known_work && !had_cleaned {
                    let breach_key = format!("reconcile:G19:{}", run.id);
                    if !self.key_used(&breach_key)? {
                        self.store.guard_breach(
                            &self.ctx(now).with_key(breach_key),
                            "G19",
                            Some(&card.id),
                            json!({"run_id": run.id, "worktree_path": path, "last_scan": cached.as_ref().map(|w| json!({"dirty": w.dirty, "unlanded_commits": w.unlanded_commits}))}),
                        )?;
                    }
                    let key = format!("reconcile:R15:lost:{}", run.id);
                    if self.recovery(
                        now,
                        &key,
                        NewAttention {
                            card: card.id.clone(),
                            run_id: Some(run.id.clone()),
                            question: format!(
                                "{} {} run {}: worktree {path} is gone and its last scan showed unlanded or uncommitted work ({:?} unlanded, dirty {:?}). This is lost work. Abandon the run, or recreate it from the branch?",
                                card.display_id,
                                run.role.short_name(),
                                run.id,
                                cached.as_ref().and_then(|w| w.unlanded_commits),
                                cached.as_ref().and_then(|w| w.dirty)
                            ),
                            evidence: Self::json_text(json!({"run_id": run.id, "worktree_path": path, "branch": run.branch})),
                            options: Some(json!(["Abandon", "Recreate from the branch"])),
                            ..NewAttention::default()
                        },
                    )? {
                        report.repair(format!("R15 run {} worktree gone with unlanded work: recovery item and G19 breach", run.id));
                    }
                } else {
                    let key = format!("reconcile:R15:missing:{}", run.id);
                    let body = format!(
                        "[{}] {} run {}: worktree {path} is missing (last scan: clean). Abandon the run, or recreate it from branch {}?",
                        card.display_id,
                        run.role.short_name(),
                        run.id,
                        run.branch.as_deref().unwrap_or("(unknown)")
                    );
                    if self.note(now, &card.project_id, &key, body, Some(card.id.clone()))? {
                        report.repair(format!(
                            "R15 run {} worktree missing: note to the orchestrator",
                            run.id
                        ));
                    }
                    if let Some(at) = self.message_at(&key)?
                        && at.plus_minutes(self.config.thresholds.orphan_note_developer) <= now
                    {
                        let item = format!("reconcile:R15:missing-recovery:{}", run.id);
                        if self.recovery(
                            now,
                            &item,
                            NewAttention {
                                card: card.id.clone(),
                                run_id: Some(run.id.clone()),
                                question: format!(
                                    "{} {} run {}: worktree {path} is missing and the orchestrator has not acted for {} minutes. Abandon, or recreate from the branch?",
                                    card.display_id,
                                    run.role.short_name(),
                                    run.id,
                                    minutes_since(at, now)
                                ),
                                evidence: Self::json_text(json!({"run_id": run.id, "worktree_path": path})),
                                options: Some(json!(["Abandon", "Recreate from the branch"])),
                                ..NewAttention::default()
                            },
                        )? {
                            report.repair(format!("R15 run {} worktree missing: recovery item", run.id));
                        }
                    }
                }
                continue;
            }
            // An ended run.
            let Some(ended_at) = run.ended_at else {
                continue;
            };
            if cached.as_ref().is_some_and(|w| w.gone_at.is_some()) || !scan.exists {
                continue;
            }
            if self.last_event("run", &run.id, "run.cleaned")?.is_some() {
                // Returned already; treehouse may still be removing it.
                continue;
            }
            // A new attempt works on in its predecessor's worktree: the
            // newest run on the path is the one that returns it.
            let succeeded: bool = self.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM runs WHERE host_id = ?1 AND worktree_path = ?2
                                AND id != ?3 AND (ended_at IS NULL OR started_at > ?4))",
                rusqlite::params![host, path, run.id, run.started_at],
                |row| row.get(0),
            )?;
            if succeeded {
                continue;
            }
            let pane_closed = self.run_pane_closed(&run);
            let listed = listed.get_or_insert_with(|| match self.sources.scanner.listed() {
                Ok(paths) => Some(paths.iter().map(|p| canonical(p)).collect()),
                Err(error) => {
                    // Nothing is returned this pass; say why.
                    report.error("worktree list", &error);
                    None
                }
            });
            let listed_here = listed
                .as_ref()
                .is_some_and(|l| l.contains(&canonical(Path::new(&path))));
            if scan.provably_clean() && listed_here && pane_closed {
                // The reading `run.clean` checks is the cache row, which must
                // be newer than the run's end: scan now when this pass has
                // not yet.
                if !rescanned && cached.as_ref().is_none_or(|w| w.fetched_at < ended_at) {
                    self.scan_worktrees(now)?;
                    rescanned = true;
                }
                let holder = match lease_holder(self.conn(), &run) {
                    Ok(holder) => holder,
                    Err(error) => {
                        report.error(&format!("lease holder of run {}", run.id), &error);
                        continue;
                    }
                };
                match self.store.run_clean(&self.ctx(now), &run.id, true) {
                    Ok(applied) if !applied.replayed => {
                        report.repair(format!("R15 run {} worktree {path} returned: ended, clean, nothing unlanded, pane closed", run.id));
                        if let Err(error) = self.sources.scanner.release(Path::new(&path), &holder)
                        {
                            report.error(&format!("treehouse return of {path}"), &error);
                            // The records say returned, so no later pass
                            // tries again: hand the return to the developer.
                            let key = format!("reconcile:R15:return-failed:{}", run.id);
                            if self.recovery(
                                now,
                                &key,
                                NewAttention {
                                    card: if card.state == CardState::Open { card.id.clone() } else { String::new() },
                                    run_id: Some(run.id.clone()),
                                    question: format!(
                                        "{} {} run {}: worktree {path} is clean and recorded as returned, but treehouse did not take it back ({error}). Return it with: treehouse return {path} --if-lease-holder {holder}",
                                        card.display_id,
                                        run.role.short_name(),
                                        run.id,
                                    ),
                                    evidence: Self::json_text(json!({"run_id": run.id, "worktree_path": path, "holder": holder, "error": error})),
                                    ..NewAttention::default()
                                },
                            )? {
                                report.repair(format!("R15 run {} worktree return failed: recovery item", run.id));
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error) => report.error(&format!("run.clean {}", run.id), &error),
                }
                continue;
            }
            let holds_work = scan.dirty != Some(false) || scan.unlanded_commits != Some(0);
            // A builder that ended `reported` with its head on an open PR
            // left the work to the PR, which the card tracks to landing.
            let on_pr = run.outcome == Some(RunOutcome::Reported)
                && run.role == RunRole::Builder
                && published_head(self.conn(), &run)?.is_some();
            if holds_work && !on_pr {
                let key = format!(
                    "reconcile:R15:unlanded:{}:{}",
                    run.id,
                    scan.head_sha.as_deref().unwrap_or("unknown")
                );
                let body = format!(
                    "[{}] {} run {} ended {:?} but worktree {path} still holds work: {:?} unlanded commits, dirty {:?}. Land it, relaunch, or abandon it?",
                    card.display_id,
                    run.role.short_name(),
                    run.id,
                    run.outcome,
                    scan.unlanded_commits,
                    scan.dirty
                );
                if self.note(now, &card.project_id, &key, body, Some(card.id.clone()))? {
                    report.repair(format!(
                        "R15 run {} ended with unlanded work: note to the orchestrator",
                        run.id
                    ));
                }
            }
            if ended_at.plus_minutes(self.config.thresholds.unreturned_days * 24 * 60) <= now {
                let key = format!("reconcile:R15:unreturned:{}", run.id);
                if self.recovery(
                    now,
                    &key,
                    NewAttention {
                        card: card.id.clone(),
                        run_id: Some(run.id.clone()),
                        question: format!(
                            "{} {} run {} ended {} days ago and worktree {path} is still there ({:?} unlanded commits, dirty {:?}). Keep it, or return it?",
                            card.display_id,
                            run.role.short_name(),
                            run.id,
                            minutes_since(ended_at, now) / (24 * 60),
                            scan.unlanded_commits,
                            scan.dirty
                        ),
                        evidence: Self::json_text(json!({"run_id": run.id, "worktree_path": path})),
                        options: Some(json!(["Keep", "Return"])),
                        ..NewAttention::default()
                    },
                )? {
                    report.repair(format!("R15 run {} worktree unreturned for a week: recovery item", run.id));
                }
            }
        }
        Ok(())
    }

    /// Whether the run's pane is gone from this pass's snapshot, or there
    /// with its agent dead. Without a snapshot nothing is known.
    fn run_pane_closed(&self, run: &Run) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        let panes: Vec<Pane> = list(
            self.conn(),
            &format!(
                "host_id = ?2 AND {}",
                tiller_core::state::runs::run_panes(1)
            ),
            &[&run.id, &self.config.host_id],
        )
        .unwrap_or_default();
        if panes.is_empty() {
            return false;
        }
        panes.iter().all(|pane| match snapshot.pane(&pane.pane_id) {
            None => true,
            Some(info) => self.sources.probe.agent(info) == Probe::Dead,
        })
    }

    /// R13: a builder on a ship card whose status says done while no merge
    /// is recorded gets a note; the column stays Working (the view) and
    /// `run.end landed` stays refused (the core). A direct-pr run the daemon
    /// has not yet published gets none: the PR is the daemon's to open.
    fn done_without_evidence(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let runs: Vec<Run> = list(
            self.conn(),
            "ended_at IS NULL AND role = 'builder' AND last_status_line LIKE 'done:%' ORDER BY started_at",
            &[],
        )?;
        for run in runs {
            let card: Card = get(self.conn(), "card", &run.card_id)?;
            if card.kind != tiller_core::domain::CardKind::Ship {
                continue;
            }
            let merged: bool = self.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM approvals WHERE card_id = ?1 AND kind IN ('merge', 'land_direct') AND state = 'succeeded')
                     OR EXISTS (SELECT 1 FROM prs WHERE card_id = ?1 AND state = 'merged')",
                [&card.id],
                |row| row.get(0),
            )?;
            if merged {
                continue;
            }
            let open_pr: Option<tiller_core::domain::Pr> =
                find(self.conn(), "card_id = ?1 AND state = 'open'", &[&card.id])?;
            if open_pr.is_none() && self.publishing(&run, &card)? {
                continue;
            }
            let record = match &open_pr {
                Some(pr) => format!(
                    "PR #{} is open, not merged (checks {}, pipeline {})",
                    pr.number, pr.checks, pr.pipeline_state
                ),
                None => "no PR is linked".to_owned(),
            };
            let stamp = run
                .last_status_at
                .map(|t| t.as_millis())
                .unwrap_or_default();
            let key = format!("reconcile:R13:{}:{stamp}", run.id);
            let body = format!(
                "[{}] build run {} reports \"{}\" but {record}; the card stays in its column. Steer the worker to the pipeline or the PR, or relaunch with a note.",
                card.display_id,
                run.id,
                run.last_status_line.as_deref().unwrap_or_default()
            );
            if self.note(now, &card.project_id, &key, body, Some(card.id.clone()))? {
                report.repair(format!(
                    "R13 run {} says done without a merge: note to the orchestrator",
                    run.id
                ));
            }
        }
        Ok(())
    }

    /// Whether the daemon still owes a direct-pr run its PR: the run's done
    /// report is what starts the push and the PR, so "no PR is linked" is
    /// the expected state until then, and a failed publish is already an
    /// owner item (`authority.md`, "Publishing a direct-pr run").
    fn publishing(&self, run: &Run, card: &Card) -> Result<bool> {
        let project: Project = get(self.conn(), "project", &card.project_id)?;
        if project.effective_delivery() != Delivery::DirectPr {
            return Ok(false);
        }
        Ok(publish(self.conn(), &run.id)?.is_some_and(|record| record.pr_id.is_none()))
    }

    /// R14: a run the harness itself recorded as cut ends `cut` with a note
    /// of what was done and what remains; the worktree stays. The same cut
    /// twice on one card goes to the developer.
    fn cuts(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let runs: Vec<Run> = list(self.conn(), "ended_at IS NULL ORDER BY started_at", &[])?;
        for run in runs {
            let Some(cut) = self.sources.harness.cut(&run) else {
                continue;
            };
            let card: Card = get(self.conn(), "card", &run.card_id)?;
            let last_text: Option<String> = self
                .conn()
                .query_row(
                    "SELECT json_extract(payload, '$.last_assistant_text') FROM observations
                     WHERE run_id = ?1 AND source = 'transcript' AND json_extract(payload, '$.last_assistant_text') IS NOT NULL
                     ORDER BY observed_at DESC LIMIT 1",
                    [&run.id],
                    |row| row.get(0),
                )
                .ok();
            let note = format!(
                "cut: {}. Done: {}. Remains: {}. Last status: {}.",
                cut.reason,
                cut.done
                    .as_deref()
                    .or(last_text.as_deref())
                    .unwrap_or("unknown"),
                cut.remains.as_deref().unwrap_or("unknown"),
                run.last_status_line.as_deref().unwrap_or("none")
            );
            self.store.run_end(
                &self.ctx(now),
                &run.id,
                EndRun {
                    outcome: RunOutcome::Cut,
                    note: Some(note.clone()),
                },
            )?;
            report.repair(format!("R14 run {} ended cut: {}", run.id, cut.reason));
            let key = format!("reconcile:R14:note:{}", run.id);
            self.note(
                now,
                &card.project_id,
                &key,
                format!("[{}] {} run {} was cut by the harness. {note} Relaunch as a new attempt with this note, or hand the rest to a builder.", card.display_id, run.role.short_name(), run.id),
                Some(card.id.clone()),
            )?;
            let cuts: i64 = self.conn().query_row(
                "SELECT count(*) FROM runs WHERE card_id = ?1 AND outcome = 'cut'",
                [&card.id],
                |row| row.get(0),
            )?;
            if cuts >= 2 {
                let item = format!("reconcile:R14:recovery:{}:{}", card.id, run.id);
                if self.recovery(
                    now,
                    &item,
                    NewAttention {
                        card: card.id.clone(),
                        run_id: Some(run.id.clone()),
                        question: format!(
                            "{} has been cut {cuts} times ({}). Split the work, raise the limit, or abandon?",
                            card.display_id, cut.reason
                        ),
                        evidence: Self::json_text(json!({"run_id": run.id, "cuts": cuts})),
                        ..NewAttention::default()
                    },
                )? {
                    report.repair(format!("R14 {} cut twice: recovery item", card.display_id));
                }
            }
        }
        Ok(())
    }
}
