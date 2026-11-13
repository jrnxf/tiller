//! Step 4: the ladder for every live session, and what each verdict
//! triggers: R02 (orchestrator crash), R03 (worker crash), R04 (the reboot
//! restore), R12 (worker silent) and R21 (the restore after a lost
//! terminal holder).

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tiller_core::domain::{Activity, Card, ObservationSource, Role, Run, Session};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::outbox::live_orchestrator;
use tiller_core::state::runs::{Bind, NewObservation, live_session};
use tiller_core::state::{find, get, list, projects};
use tiller_core::{Actor, Result, Timestamp};
use tiller_observe::liveness::Probe;

use crate::ladder::{self, Death, Verdict};
use crate::pass::{Reconciler, Report, minutes_since, ts};

/// The `source` a resume launch binds the new pane under, so the proof of
/// life is a hook's bind, never the launcher's own.
pub const LAUNCH_BIND_SOURCE: &str = "resume_launch";

/// The `checks.reason` of a resume `tiller agent resume` attempted. The
/// automatic path never wakes, retries or escalates one: the caller reports
/// its own result. The ladder leaves the session alone for the bind window,
/// then judges it as usual (R03).
pub const MANUAL_RESUME_REASON: &str = "manual";

/// One `session.resume_attempted` event.
#[derive(Clone, Debug)]
pub(crate) struct Attempt {
    pub at: Timestamp,
    pub payload: Value,
}

impl Attempt {
    /// Whether `tiller agent resume` made it, not the daemon.
    fn manual(&self) -> bool {
        self.payload
            .get("checks")
            .and_then(|c| c.get("reason"))
            .and_then(Value::as_str)
            == Some(MANUAL_RESUME_REASON)
    }
}

/// The daemon's own attempts: a manual resume never uses up its two.
fn automatic(attempts: &[Attempt]) -> usize {
    attempts.iter().filter(|a| !a.manual()).count()
}

impl Reconciler {
    pub(crate) fn sessions(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        // Live panes the records lost track of (a restore, an owner flip)
        // are rebound first, so the ladder never reads them as gone.
        self.adopt_panes(now, report)?;
        if let Some(restore) = self.open_restore()? {
            // The restore owns every live session until the boot id or the
            // holder instance is written; the ladder would call its dead
            // sessions crashes.
            self.restore(&restore, now, report)?;
            if self.restore_pending()? {
                return Ok(());
            }
        }
        for session in self.live_sessions()? {
            if let Err(error) = self.session_step(&session, now, report) {
                report.error(&format!("session {}", session.id), &error);
            }
        }
        self.crashed_runs_to_developer(now, report)?;
        Ok(())
    }

    fn session_step(
        &mut self,
        session: &Session,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        // A resume in flight is judged by its own clock first.
        if self.resume_follow_up(session, now, report)? {
            return Ok(());
        }
        let effective =
            ladder::effective_activity(self.conn(), session, &self.config.thresholds, now)?;
        let errored = ladder::errored_long(session, &self.config.thresholds, now);
        let recorded_pid = self.recorded_pid(session)?;
        let verdict = ladder::climb(
            session,
            &effective,
            errored,
            self.snapshot.as_ref(),
            self.holder_reading(recorded_pid)?,
            self.sources.probe.as_ref(),
        );
        match verdict {
            Verdict::Alive | Verdict::Unknown => Ok(()),
            Verdict::Silent => self.silent(session, now, report),
            Verdict::Dead(how) => {
                self.record_death(session, how, now)?;
                match session.role {
                    Role::Orchestrator => self.orchestrator_crash(session, how, now, report),
                    _ => self.worker_crash(session, how, now, report),
                }
            }
        }
    }

    /// The pid the panes cache last saw in this session's pane: the agent
    /// probe's target once the holder no longer lists the pane (R18).
    fn recorded_pid(&self, session: &Session) -> Result<Option<u32>> {
        let Some(pane) = &session.pane_id else {
            return Ok(None);
        };
        let pid: Option<Option<i64>> = self
            .conn()
            .query_row(
                "SELECT pid FROM panes WHERE host_id = ?1 AND pane_id = ?2",
                rusqlite::params![session.host_id, pane],
                |row| row.get(0),
            )
            .optional()?;
        Ok(pid.flatten().and_then(|pid| u32::try_from(pid).ok()))
    }

    /// This pass's holder `list`, for the ladder, only when it comes from
    /// the instance the records know. A new instance is R21's to judge:
    /// until it has written `meta.holder_instance` the ladder reads every
    /// holder pane as unknown.
    pub(crate) fn holder_reading(
        &self,
        recorded_pid: Option<u32>,
    ) -> Result<Option<ladder::HolderReading<'_>>> {
        let Some(listing) = &self.listing else {
            return Ok(None);
        };
        let known = projects::meta(self.conn(), "holder_instance")?;
        Ok(
            (known.as_deref() == Some(listing.instance.as_str())).then_some(
                ladder::HolderReading {
                    listing,
                    recorded_pid,
                },
            ),
        )
    }

    /// The ladder's dead verdict as a `liveness` observation (verdict
    /// `unknown`), once per death: the row stays open.
    fn record_death(&mut self, session: &Session, how: Death, now: Timestamp) -> Result<()> {
        let since = session.activity_observed_at.unwrap_or(session.started_at);
        let already: bool = self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM observations WHERE session_id = ?1 AND source = 'liveness'
                            AND event = ?2 AND recorded_at > ?3)",
            rusqlite::params![session.id, how.event(), since],
            |row| row.get(0),
        )?;
        if already {
            return Ok(());
        }
        self.store.observation_record(
            &self.ctx(now),
            NewObservation {
                session_id: Some(session.id.clone()),
                run_id: session.run_id.clone(),
                source: ObservationSource::Liveness,
                event: how.event().to_owned(),
                verdict: Some(Activity::Unknown),
                observed_at: now,
                payload: json!({"pane_id": session.pane_id}),
            },
        )?;
        Ok(())
    }

    // ----- R12 -----

    fn silent(&mut self, session: &Session, now: Timestamp, report: &mut Report) -> Result<()> {
        let observed = session.activity_observed_at.unwrap_or(session.started_at);
        let silent_for = minutes_since(observed, now);
        let stamp = observed.as_millis();
        // The evidence first: one `agent_alive` observation per silence.
        let recorded: bool = self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM observations WHERE session_id = ?1 AND source = 'liveness'
                            AND event = 'agent_alive' AND recorded_at > ?2)",
            rusqlite::params![session.id, observed],
            |row| row.get(0),
        )?;
        if !recorded {
            let ctx = self.ctx(now);
            tiller_observe::liveness::record(&mut self.store, &ctx, session, Probe::Alive, None)?;
            report.repair(format!(
                "R12 session {} silent for {silent_for} min: agent alive, recorded",
                session.id
            ));
        }
        let t = self.config.thresholds.clone();
        let Some(run_id) = session.run_id.clone() else {
            // An orchestrator: no note (nobody to note), the developer after
            // an hour with work in flight.
            if silent_for >= t.orchestrator_silent_developer && self.work_in_flight(session)?.any()
            {
                let key = format!("reconcile:R12:orchestrator:{}:{stamp}", session.id);
                let opened = self.recovery(
                    now,
                    &key,
                    NewAttention {
                        question: format!(
                            "The {} orchestrator has been silent for {silent_for} minutes with work in flight. Interrupt it, relaunch it, or wait?",
                            session.project_id
                        ),
                        evidence: Self::json_text(json!({"session_id": session.id, "last_observation_at": observed.to_string(), "pane_tail": session.pane_id.as_deref().map(|p| self.pane_tail(p))})),
                        subject_session_id: Some(session.id.clone()),
                        subject_project_id: Some(session.project_id.clone()),
                        options: Some(json!(["Interrupt", "Relaunch", "Wait"])),
                        ..NewAttention::default()
                    },
                )?;
                if opened {
                    report.repair(format!(
                        "R12 orchestrator {} silent {silent_for} min: recovery item on fleet-1",
                        session.id
                    ));
                }
            }
            return Ok(());
        };
        let run: Run = get(self.conn(), "run", &run_id)?;
        let card: Card = get(self.conn(), "card", &run.card_id)?;
        if silent_for < t.silent_note {
            return Ok(());
        }
        let note_key = format!("reconcile:R12:note:{run_id}:{}:{stamp}", session.id);
        let last_tool = self.last_tool_call(&session.id, now)?;
        // A key another actor holds is refused again, so the pane is not
        // read for it either.
        if !self.key_used(&note_key)? && !self.still_planted(&note_key)? {
            let tail = session
                .pane_id
                .as_deref()
                .map(|p| self.pane_tail(p))
                .unwrap_or_default();
            let body = format!(
                "[{}] {} run {} has been silent for {silent_for} min (agent alive, last observation {}). Last tool call: {last_tool}. Interrupt, relaunch with a note, or pause it until a date.\n--- pane tail ---\n{tail}",
                card.display_id,
                run.role.short_name(),
                run.id,
                observed
            );
            if self.note(
                now,
                &card.project_id,
                &note_key,
                body,
                Some(card.id.clone()),
            )? {
                report.repair(format!(
                    "R12 run {} silent {silent_for} min: note to the orchestrator",
                    run.id
                ));
            }
        }
        // The developer: an hour after the note with no action, or the second
        // silence of the same run in the window.
        // Only the daemon's own notes count: any actor may send under a
        // key of this shape (`protocol.md` 6.3).
        let note_at = self.message_at(&note_key)?;
        let earlier_silences: i64 = self.conn().query_row(
            "SELECT count(*) FROM outbox WHERE idempotency_key LIKE ?1 AND idempotency_key != ?2 AND created_at >= ?3 AND created_by = ?4",
            rusqlite::params![format!("reconcile:R12:note:{run_id}:%"), note_key, now.plus_minutes(-t.repeat_window_hours * 60), Actor::Daemon.to_string()],
            |row| row.get(0),
        )?;
        let overdue = note_at.is_some_and(|at| at.plus_minutes(t.silent_developer) <= now);
        if overdue || earlier_silences > 0 {
            let key = format!("reconcile:R12:recovery:{run_id}:{}:{stamp}", session.id);
            let why = if earlier_silences > 0 {
                format!("its second silence in {} hours", t.repeat_window_hours)
            } else {
                format!(
                    "no orchestrator action {} minutes after the note",
                    t.silent_developer
                )
            };
            let opened = self.recovery(
                now,
                &key,
                NewAttention {
                    card: card.id.clone(),
                    run_id: Some(run.id.clone()),
                    question: format!(
                        "{} {} has been silent for {silent_for} minutes ({why}). Last tool call: {last_tool}. Interrupt, relaunch, or wait?",
                        card.display_id,
                        run.role.short_name()
                    ),
                    evidence: Self::json_text(json!({"session_id": session.id, "run_id": run.id, "last_observation_at": observed.to_string(), "note_at": ts(note_at)})),
                    subject_session_id: Some(session.id.clone()),
                    subject_project_id: Some(session.project_id.clone()),
                    options: Some(json!(["Interrupt", "Relaunch", "Wait"])),
                    ..NewAttention::default()
                },
            )?;
            if opened {
                report.repair(format!(
                    "R12 run {} silent {silent_for} min: recovery item on {}",
                    run.id, card.display_id
                ));
            }
        }
        Ok(())
    }

    /// The newest tool call the hooks or the transcript saw, as "<event>, started N min ago".
    fn last_tool_call(&self, session: &str, now: Timestamp) -> Result<String> {
        let row: Option<(String, Timestamp, Value)> = self
            .conn()
            .query_row(
                "SELECT event, observed_at, payload FROM observations
                 WHERE session_id = ?1 AND source IN ('hook', 'transcript')
                   AND (event LIKE 'PreToolUse%' OR event LIKE 'PostToolUse%' OR event LIKE 'tool%')
                 ORDER BY observed_at DESC LIMIT 1",
                [session],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            Some((event, at, payload)) => {
                let tool = payload
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or(&event);
                format!("{tool}, started {} min ago", minutes_since(at, now))
            }
            None => "none recorded".to_owned(),
        })
    }

    // ----- R03 -----

    fn worker_crash(
        &mut self,
        session: &Session,
        how: Death,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let Some(run_id) = session.run_id.clone() else {
            return Ok(());
        };
        let run: Run = get(self.conn(), "run", &run_id)?;
        let card: Card = get(self.conn(), "card", &run.card_id)?;
        let project = self.project(&card.project_id)?;
        let reason = match how {
            Death::PaneGone => "crash: pane gone",
            Death::AgentDead => "crash: agent dead",
        };
        self.store
            .session_end(&self.ctx(now), &session.id, reason)?;
        report.repair(format!(
            "R03 worker session {} on {} ended ({reason}); run {} keeps its worktree and reservation",
            session.id, card.display_id, run.id
        ));
        if run.ended_at.is_some() {
            return Ok(());
        }
        let scan = run
            .worktree_path
            .as_deref()
            .map(|path| self.scan_of(Path::new(path), &project));
        let status_age = run
            .last_status_at
            .map(|at| format!("{} min ago", minutes_since(at, now)))
            .unwrap_or_else(|| "never".into());
        let (dirty, unlanded) = match &scan {
            Some(scan) => (scan.dirty, scan.unlanded_commits),
            None => (None, None),
        };
        let holds_work = dirty == Some(true) || unlanded.is_some_and(|n| n > 0);
        let recent = run
            .last_status_at
            .is_some_and(|at| at.plus_minutes(60) > now);
        let suggestion = if holds_work || recent {
            "relaunch with a note (tiller agent relaunch)"
        } else {
            "abandon (tiller done --outcome abandoned)"
        };
        let note = format!(
            "[{}] {} run {} (attempt {}) died at {now}: {reason}. Last status: {} ({status_age}). Worktree: dirty {dirty:?}, unlanded commits {unlanded:?}. Suggested: {suggestion}.",
            card.display_id,
            run.role.short_name(),
            run.id,
            run.attempt,
            run.last_status_line.as_deref().unwrap_or("none"),
        );
        let key = format!("reconcile:R03:note:{}", session.id);
        if self.note(now, &card.project_id, &key, note, Some(card.id.clone()))? {
            report.repair(format!("R03 run {}: note to the orchestrator", run.id));
        }
        // Straight to the developer: a missing worktree, or the second death
        // of this run in the window.
        let deaths: i64 = self.conn().query_row(
            "SELECT count(*) FROM sessions WHERE run_id = ?1 AND end_reason LIKE 'crash%' AND ended_at >= ?2",
            rusqlite::params![run.id, now.plus_minutes(-self.config.thresholds.repeat_window_hours * 60)],
            |row| row.get(0),
        )?;
        let missing = scan.as_ref().is_some_and(|s| !s.exists);
        if missing || deaths >= 2 {
            let why = if missing {
                "its worktree is missing".to_owned()
            } else {
                format!(
                    "it died {deaths} times in {} hours",
                    self.config.thresholds.repeat_window_hours
                )
            };
            self.worker_crash_item(&session.id, &run, &card, &why, now, report)?;
        }
        Ok(())
    }

    fn worker_crash_item(
        &mut self,
        session: &str,
        run: &Run,
        card: &Card,
        why: &str,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let key = format!("reconcile:R03:recovery:{session}");
        let opened = self.recovery(
            now,
            &key,
            NewAttention {
                card: card.id.clone(),
                run_id: Some(run.id.clone()),
                question: format!(
                    "{} {} run {} crashed and {why}. Last status: {}. Relaunch it, abandon the run, or wait?",
                    card.display_id,
                    run.role.short_name(),
                    run.id,
                    run.last_status_line.as_deref().unwrap_or("none")
                ),
                evidence: Self::json_text(json!({"session_id": session, "run_id": run.id, "worktree_path": run.worktree_path})),
                options: Some(json!(["Relaunch", "Abandon", "Wait"])),
                ..NewAttention::default()
            },
        )?;
        if opened {
            report.repair(format!(
                "R03 run {}: recovery item on {} ({why})",
                run.id, card.display_id
            ));
        }
        Ok(())
    }

    /// Active runs whose last session crashed with no orchestrator action
    /// (a relaunch starts a live session; `done` or `cancel` ends the run)
    /// for the crash threshold go to the developer.
    fn crashed_runs_to_developer(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let runs: Vec<Run> = list(
            self.conn(),
            "host_id = ?1 AND ended_at IS NULL
             AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.run_id = runs.id AND s.ended_at IS NULL)
             ORDER BY started_at",
            &[&self.config.host_id],
        )?;
        for run in runs {
            let last: Option<Session> = find(
                self.conn(),
                "run_id = ?1 AND ended_at IS NOT NULL ORDER BY ended_at DESC",
                &[&run.id],
            )?;
            let Some(last) = last else { continue };
            let crashed = last
                .end_reason
                .as_deref()
                .is_some_and(|r| r.starts_with("crash"));
            let Some(ended_at) = last.ended_at.filter(|_| crashed) else {
                continue;
            };
            if ended_at.plus_minutes(self.config.thresholds.worker_crash_developer) > now {
                continue;
            }
            let card: Card = get(self.conn(), "card", &run.card_id)?;
            let why = format!(
                "the orchestrator took no action for {} minutes",
                self.config.thresholds.worker_crash_developer
            );
            self.worker_crash_item(&last.id, &run, &card, &why, now, report)?;
        }
        Ok(())
    }

    // ----- R02 -----

    pub(crate) fn work_in_flight(&self, session: &Session) -> Result<InFlight> {
        let runs: i64 = self.conn().query_row(
            "SELECT count(*) FROM runs r JOIN cards c ON c.id = r.card_id WHERE c.project_id = ?1 AND r.ended_at IS NULL",
            [&session.project_id],
            |row| row.get(0),
        )?;
        let messages: i64 = self.conn().query_row(
            "SELECT count(*) FROM outbox WHERE target_session_id = ?1 AND state IN ('pending', 'delivered')",
            [&session.id],
            |row| row.get(0),
        )?;
        let asks: i64 = self.conn().query_row(
            "SELECT count(*) FROM asks WHERE project_id = ?1 AND disposition = 'unresolved'",
            [&session.project_id],
            |row| row.get(0),
        )?;
        Ok(InFlight {
            runs,
            messages,
            asks,
        })
    }

    fn orchestrator_crash(
        &mut self,
        session: &Session,
        how: Death,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let flight = self.work_in_flight(session)?;
        let reason = match how {
            Death::PaneGone => "crash: pane gone",
            Death::AgentDead => "crash: agent dead",
        };
        if !flight.any() {
            self.store
                .session_end(&self.ctx(now), &session.id, reason)?;
            report.repair(format!(
                "R02 {} orchestrator {} died with nothing in flight: session ended, workspace stays closed",
                session.project_id, session.id
            ));
            return Ok(());
        }
        let t = self.config.thresholds.clone();
        let hour_ago = now.plus_minutes(-60);
        let crashes: i64 = self.conn().query_row(
            "SELECT count(*) FROM sessions WHERE project_id = ?1 AND role = 'orchestrator' AND end_reason LIKE 'crash%' AND ended_at >= ?2",
            rusqlite::params![session.project_id, hour_ago],
            |row| row.get(0),
        )?;
        let resumes = self.project_resumes_since(&session.project_id, hour_ago)?;
        if crashes + 1 >= t.orchestrator_crashes_per_hour || resumes >= t.resumes_per_project_hour {
            self.store
                .session_end(&self.ctx(now), &session.id, reason)?;
            let key = format!("reconcile:R02:crashes:{}", session.id);
            let opened = self.recovery(
                now,
                &key,
                NewAttention {
                    question: format!(
                        "The {} orchestrator crashed {} times since {hour_ago} (last: {reason}); {} runs active, {} messages waiting. Relaunch it, or hold the project?",
                        session.project_id,
                        crashes + 1,
                        flight.runs,
                        flight.messages
                    ),
                    evidence: Self::json_text(json!({"session_id": session.id, "crashes_in_hour": crashes + 1, "resumes_in_hour": resumes})),
                    subject_project_id: Some(session.project_id.clone()),
                    options: Some(json!(["Relaunch", "Hold the project"])),
                    ..NewAttention::default()
                },
            )?;
            if opened {
                report.repair(format!(
                    "R02 {} orchestrator crashed {} times in an hour: recovery item on fleet-1",
                    session.project_id,
                    crashes + 1
                ));
            }
            return Ok(());
        }
        let Some(cwd) = self.config.project_paths.get(&session.project_id).cloned() else {
            self.store
                .session_end(&self.ctx(now), &session.id, reason)?;
            let key = format!("reconcile:R02:nopath:{}", session.id);
            if self.recovery(
                now,
                &key,
                NewAttention {
                    question: format!(
                        "The {} orchestrator died ({reason}) with {} runs active and no checkout path in the config to resume in. Relaunch it by hand, or add the path?",
                        session.project_id, flight.runs
                    ),
                    evidence: Self::json_text(json!({"session_id": session.id})),
                    subject_project_id: Some(session.project_id.clone()),
                    ..NewAttention::default()
                },
            )? {
                report.repair(format!("R02 {} orchestrator died; no checkout path: recovery item on fleet-1", session.project_id));
            }
            return Ok(());
        };
        self.attempt_resume(session, &cwd, true, "crash", now, report)?;
        Ok(())
    }

    fn project_resumes_since(&self, project_id: &str, since: Timestamp) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM events e JOIN sessions s ON s.id = e.entity_id
             WHERE e.kind = 'session.resume_attempted' AND s.project_id = ?1 AND s.role = 'orchestrator'
               AND e.at >= ?2 AND json_extract(e.payload, '$.checks.skipped') IS NULL",
            rusqlite::params![project_id, since],
            |row| row.get(0),
        )?)
    }

    // ----- session.resume (R02, R04) -----

    /// The resume attempts recorded for a session at or after `since`,
    /// oldest first, skipping the ones a floor refused.
    pub(crate) fn attempts(&self, session: &str, since: Timestamp) -> Result<Vec<Attempt>> {
        let mut stmt = self.conn().prepare(
            "SELECT at, payload FROM events WHERE entity = 'session' AND entity_id = ?1
               AND kind = 'session.resume_attempted' AND at >= ?2
               AND json_extract(payload, '$.checks.skipped') IS NULL
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![session, since], |row| {
            Ok(Attempt {
                at: row.get(0)?,
                payload: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Launches `claude --resume` (or a fresh session on the second try)
    /// for `session` in `cwd`, after the floors and the launch rate.
    pub(crate) fn attempt_resume(
        &mut self,
        session: &Session,
        cwd: &Path,
        with_resume: bool,
        reason: &str,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<Launch> {
        if self.last_launch.is_some_and(|at| {
            at.as_millis() + self.config.thresholds.launch_gap_seconds * 1000 > now.as_millis()
        }) {
            return Ok(Launch::RateLimited);
        }
        let readings = self.sources.readings.read();
        let host: tiller_core::domain::Host = get(self.conn(), "host", &self.config.host_id)?;
        let floor = |name: &str, floor: Option<i64>, free: Option<i64>| -> Option<String> {
            match (floor, free) {
                (Some(floor), Some(free)) if free < floor => Some(format!(
                    "{name} free is {free} MB, under the {floor} MB floor"
                )),
                (Some(floor), None) => Some(format!(
                    "{name} free is unknown; the {floor} MB floor fails closed"
                )),
                _ => None,
            }
        };
        let skipped = floor("RAM", host.ram_floor_mb, readings.ram_free_mb)
            .or_else(|| floor("disk", host.disk_floor_mb, readings.disk_free_mb));
        let resume_id = session.claude_session_id.clone().filter(|_| with_resume);
        let checks = json!({
            "ram_free_mb": readings.ram_free_mb,
            "disk_free_mb": readings.disk_free_mb,
            "cwd": cwd.display().to_string(),
            "resume": resume_id,
            "reason": reason,
            "skipped": skipped,
        });
        self.store
            .session_resume_attempted(&self.ctx(now), &session.id, checks)?;
        if let Some(why) = skipped {
            report.repair(format!("session {} resume skipped: {why}", session.id));
            self.resume_skipped_item(session, &why, now, report)?;
            return Ok(Launch::Skipped);
        }
        self.last_launch = Some(now);
        match self
            .sources
            .launcher
            .launch(session, cwd, resume_id.as_deref())
        {
            Ok(launched) => {
                if let Some(claude_id) = &session.claude_session_id {
                    self.store.run_bind_session(
                        &self.ctx(now),
                        Bind {
                            session_id: session.id.clone(),
                            claude_session_id: claude_id.clone(),
                            pane_id: Some(launched.pane_id.clone()),
                            transcript_path: None,
                            source: LAUNCH_BIND_SOURCE.to_owned(),
                        },
                    )?;
                }
                report.repair(format!(
                    "session {} ({} {}) launched in pane {} after a {reason}{}",
                    session.id,
                    session.project_id,
                    session.role,
                    launched.pane_id,
                    if with_resume {
                        " (--resume)"
                    } else {
                        " (fresh)"
                    }
                ));
                Ok(Launch::Launched)
            }
            Err(error) => {
                // The attempt stands: the bind window decides what it was.
                report.error(&format!("launch of session {}", session.id), &error);
                Ok(Launch::Launched)
            }
        }
    }

    fn resume_skipped_item(
        &mut self,
        session: &Session,
        why: &str,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let (card, run_id) = match &session.run_id {
            Some(run) => {
                let run: Run = get(self.conn(), "run", run)?;
                (run.card_id, Some(run.id))
            }
            None => (String::new(), None),
        };
        let key = format!("reconcile:resume:skipped:{}", session.id);
        if self.recovery(
            now,
            &key,
            NewAttention {
                card,
                run_id,
                question: format!("Session {} could not be resumed: {why}. Free the resource, or relaunch by hand?", session.id),
                evidence: Self::json_text(json!({"session_id": session.id})),
                subject_project_id: Some(session.project_id.clone()),
                ..NewAttention::default()
            },
        )? {
            report.repair(format!("session {}: recovery item for the skipped resume", session.id));
        }
        Ok(())
    }

    /// Whether the session's pane shows a prompt (a trust or permission
    /// dialog) in this pass's snapshot: a resumed session there is left
    /// alone, and never answered (R04, R17).
    fn at_prompt(&self, session: &Session) -> bool {
        // Only this pass's snapshot: the stored activity of a dead session
        // keeps its last prompt forever, since the view never ages a prompt.
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        ladder::bound_pane(snapshot, session)
            .is_some_and(|p| p.agent.is_some() && p.agent_status.as_deref() == Some("blocked"))
    }

    /// Settles a session whose newest resume attempt is still open. Returns
    /// `true` when the attempt owns the session this pass (the ladder is not
    /// run on it). What settled an attempt is read from the records: the
    /// wake sent on a bind, or the item opened when two attempts failed.
    fn resume_follow_up(
        &mut self,
        session: &Session,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<bool> {
        // Attempts since the daemon started, or over the last day when that
        // is earlier: a previous daemon's attempt still counts.
        let since = self
            .started_at
            .unwrap_or(now)
            .min(now.plus_minutes(-24 * 60));
        let attempts = self.attempts(&session.id, since)?;
        let Some(newest) = attempts.last().cloned() else {
            return Ok(false);
        };
        let wake_key = format!(
            "reconcile:resume:wake:{}:{}",
            session.id,
            newest.at.as_millis()
        );
        if self.key_used(&wake_key)? {
            return Ok(false);
        }
        let failed_key = match session.run_id {
            None => format!("reconcile:R02:failed:{}", session.id),
            Some(_) => format!("reconcile:R04:failed:{}", session.id),
        };
        if self.key_used(&failed_key)?
            || self.key_used(&format!("reconcile:R21:failed:{}", session.id))?
        {
            // Left for the owner: the pane stays, the row stays `unknown`.
            return Ok(true);
        }
        let reason = newest
            .payload
            .get("checks")
            .and_then(|c| c.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("crash")
            .to_owned();
        if reason == MANUAL_RESUME_REASON {
            // Its bind window is the launch's grace; after it, or once a
            // hook shows up, the ladder judges the session.
            return Ok(!self.hook_seen_after(&session.id, newest.at)?
                && newest.at.plus_minutes(self.config.thresholds.bind_window) > now);
        }
        if self.hook_seen_after(&session.id, newest.at)? {
            self.resumed(session, &reason, &wake_key, now, report)?;
            return Ok(false);
        }
        if self.at_prompt(session) {
            // The owner answers it; the card shows it. Nothing is typed.
            return Ok(true);
        }
        if newest.at.plus_minutes(self.config.thresholds.bind_window) > now {
            return Ok(true);
        }
        if automatic(&attempts) < 2 {
            if let Some(cwd) = self.resume_cwd(session)? {
                self.attempt_resume(session, &cwd, false, &reason, now, report)?;
            }
            return Ok(true);
        }
        // Two attempts bound nothing: the automatic path stops.
        match session.run_id.clone() {
            None => {
                self.store.session_end(
                    &self.ctx(now),
                    &session.id,
                    "crash: resume failed twice",
                )?;
                if self.recovery(
                    now,
                    &failed_key,
                    NewAttention {
                        question: format!(
                            "The {} orchestrator could not be resumed after a {reason}: two attempts bound no session. Relaunch it, or hold the project?",
                            session.project_id
                        ),
                        evidence: Self::json_text(json!({"session_id": session.id, "attempts": automatic(&attempts)})),
                        subject_project_id: Some(session.project_id.clone()),
                        options: Some(json!(["Relaunch", "Hold the project"])),
                        ..NewAttention::default()
                    },
                )? {
                    report.repair(format!("R02 {} orchestrator resume failed twice: session ended, recovery item on fleet-1", session.project_id));
                }
            }
            Some(run_id) => {
                let run: Run = get(self.conn(), "run", &run_id)?;
                let card: Card = get(self.conn(), "card", &run.card_id)?;
                if self.recovery(
                    now,
                    &failed_key,
                    NewAttention {
                        card: card.id.clone(),
                        run_id: Some(run.id.clone()),
                        question: format!(
                            "{} {} run {} could not be resumed after a {reason}: two attempts bound no session; its pane is left for you. Relaunch, abandon, or wait?",
                            card.display_id,
                            run.role.short_name(),
                            run.id
                        ),
                        evidence: Self::json_text(json!({"session_id": session.id, "attempts": automatic(&attempts)})),
                        subject_project_id: Some(session.project_id.clone()),
                        options: Some(json!(["Relaunch", "Abandon", "Wait"])),
                        ..NewAttention::default()
                    },
                )? {
                    report.repair(format!("R04 run {} resume failed twice: recovery item on {}", run.id, card.display_id));
                }
            }
        }
        Ok(true)
    }

    /// Where a session resumes: a worker in its worktree, an orchestrator in
    /// the project's checkout from the config.
    pub(crate) fn resume_cwd(&self, session: &Session) -> Result<Option<PathBuf>> {
        Ok(match &session.run_id {
            Some(run) => get::<Run>(self.conn(), "run", run)?
                .worktree_path
                .map(PathBuf::from),
            None => self.config.project_paths.get(&session.project_id).cloned(),
        })
    }

    /// The hook bound the resumed session: reown the project's runs, and
    /// wake it with what it missed.
    fn resumed(
        &mut self,
        session: &Session,
        reason: &str,
        wake_key: &str,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let body = match &session.run_id {
            None => {
                let runs: Vec<Run> = list(
                    self.conn(),
                    "ended_at IS NULL AND card_id IN (SELECT id FROM cards WHERE project_id = ?1) ORDER BY started_at",
                    &[&session.project_id],
                )?;
                for run in &runs {
                    let applied = self.store.run_reown(&self.ctx(now), &run.id)?;
                    if !applied.replayed {
                        report.repair(format!("R02 run {} reowned to {}", run.id, session.id));
                    }
                }
                let flight = self.work_in_flight(session)?;
                format!(
                    "resumed after a {reason} at {now}: {} runs active, {} messages waiting",
                    runs.len(),
                    flight.messages
                )
            }
            Some(run_id) => {
                let run: Run = get(self.conn(), "run", run_id)?;
                format!(
                    "resumed after a {reason} at {now}; your last status: {}",
                    run.last_status_line.as_deref().unwrap_or("none")
                )
            }
        };
        self.wake(now, &session.id, wake_key, body)?;
        report.repair(format!(
            "session {} resumed after a {reason}: woken",
            session.id
        ));
        Ok(())
    }

    // ----- R04 and R21 -----

    /// The restore after a reboot (R04) or a lost terminal holder (R21):
    /// orchestrators of projects with work in flight, then each active run
    /// in its own worktree, one launch per ten seconds, each step checking
    /// that the session is not already alive. The boot id, or the holder
    /// instance, is written last, when every session is alive, settled or
    /// reported, so a daemon that dies mid-restore repeats it safely. R21
    /// restores only the sessions of holder panes: the herdr panes did not
    /// go with the holder.
    pub(crate) fn restore(
        &mut self,
        restore: &Restore,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let row = restore.row();
        let reason = restore.reason();
        let started = self.started_at.unwrap_or(now);
        // What the restore counts from: the daemon's start after a reboot,
        // the new holder's start after a lost holder, so a daemon that
        // restarts mid-restore still sees the attempts it made.
        let since = match restore {
            Restore::Reboot { .. } => started,
            Restore::HolderLost { instance } => match self.holder_started(instance) {
                Some(at) => at,
                None => return Ok(()),
            },
        };
        let mut pending = false;
        // `(project, session, line)` for the summary: what was not simply
        // resumed.
        let mut summary: Vec<(String, String, String)> = Vec::new();
        for session in self.live_sessions()? {
            let probe = if session
                .pane_id
                .as_deref()
                .is_some_and(tiller_core::ids::is_holder_pane)
            {
                match self.restore_probe(restore, &session)? {
                    Some(probe) => probe,
                    None => {
                        // No holder list this pass: unknown, never gone.
                        pending = true;
                        continue;
                    }
                }
            } else {
                if matches!(restore, Restore::HolderLost { .. }) {
                    continue;
                }
                let Some(snapshot) = &self.snapshot else {
                    pending = true;
                    continue;
                };
                ladder::bound_pane(snapshot, &session).map(|p| self.sources.probe.agent(p))
            };
            if probe == Some(Probe::Unknown) && !self.hook_seen_after(&session.id, since)? {
                // Present, unproven: never a second agent on one run.
                pending = true;
                continue;
            }
            let alive = probe == Some(Probe::Alive) || self.hook_seen_after(&session.id, since)?;
            if alive {
                // A session this restore launched that sits at a trust or
                // permission prompt: nothing is typed, the summary names it.
                if self.at_prompt(&session)
                    && !self.hook_seen_after(&session.id, since)?
                    && !self.attempts(&session.id, since)?.is_empty()
                {
                    summary.push((
                        session.project_id.clone(),
                        session.id.clone(),
                        "waits at a trust prompt; nothing typed".into(),
                    ));
                }
                continue;
            }
            let run: Option<Run> = match &session.run_id {
                Some(id) => Some(get(self.conn(), "run", id)?),
                None => None,
            };
            // A worker whose run ended, or a session that never had a pane:
            // nothing to restore.
            if run.as_ref().is_some_and(|r| r.ended_at.is_some()) || session.pane_id.is_none() {
                continue;
            }
            if run.is_none() && !self.work_in_flight(&session)?.any() {
                self.store
                    .session_end(&self.ctx(now), &session.id, reason)?;
                report.repair(format!(
                    "{row} {} orchestrator had nothing in flight: session ended",
                    session.project_id
                ));
                continue;
            }
            let project = session.project_id.clone();
            let skipped_key = format!("reconcile:{row}:skipped:{}", session.id);
            let floor_key = format!("reconcile:resume:skipped:{}", session.id);
            let failed_key = format!("reconcile:{row}:failed:{}", session.id);
            if self.key_used(&skipped_key)? {
                summary.push((project, session.id.clone(), "skipped".into()));
                continue;
            }
            if self.key_used(&floor_key)? {
                summary.push((project, session.id.clone(), "skipped for a floor".into()));
                continue;
            }
            if self.key_used(&failed_key)? {
                summary.push((
                    project,
                    session.id.clone(),
                    "resumed twice, bound nothing".into(),
                ));
                continue;
            }
            let attempts = self.attempts(&session.id, since)?;
            match attempts.last() {
                None => match self.restore_target(&session, run.as_ref())? {
                    Ok(cwd) => {
                        match self.attempt_resume(&session, &cwd, true, reason, now, report)? {
                            Launch::Launched | Launch::RateLimited => pending = true,
                            Launch::Skipped => summary.push((
                                project,
                                session.id.clone(),
                                "skipped for a floor".into(),
                            )),
                        }
                    }
                    Err(why) => {
                        self.restore_skipped(restore, &session, run.as_ref(), &why, now, report)?;
                        summary.push((project, session.id.clone(), format!("skipped: {why}")));
                    }
                },
                Some(newest) => {
                    if self.hook_seen_after(&session.id, newest.at)? {
                        continue;
                    }
                    if self.at_prompt(&session) {
                        summary.push((
                            project,
                            session.id.clone(),
                            "waits at a trust prompt; nothing typed".into(),
                        ));
                        continue;
                    }
                    if newest.at.plus_minutes(self.config.thresholds.bind_window) > now {
                        pending = true;
                        continue;
                    }
                    if automatic(&attempts) < 2 {
                        if let Ok(cwd) = self.restore_target(&session, run.as_ref())? {
                            self.attempt_resume(&session, &cwd, false, reason, now, report)?;
                        }
                        pending = true;
                        continue;
                    }
                    self.restore_failed(restore, &session, run.as_ref(), now, report)?;
                    summary.push((
                        project,
                        session.id.clone(),
                        "resumed twice, bound nothing".into(),
                    ));
                }
            }
        }
        // The new instance goes with the boot id (R04 step 5), so a holder
        // wired here must have listed this pass.
        let instance = match (&self.sources.holder, &self.listing) {
            (None, _) => None,
            (Some(_), Some(listing)) => Some(listing.instance.clone()),
            (Some(_), None) => {
                pending = true;
                None
            }
        };
        if pending {
            return Ok(());
        }
        // Complete: the summary per project, one notification, the boot id
        // or the holder instance last.
        let projects: Vec<String> = {
            let mut stmt = self
                .conn()
                .prepare("SELECT id FROM projects WHERE retired_at IS NULL ORDER BY id")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let lost = match restore {
            Restore::Reboot { .. } => None,
            Restore::HolderLost { .. } => Some(self.lost_agents(since)?),
        };
        let mut resumed_total = 0;
        let mut workers_total = 0;
        for project in projects {
            let runs: Vec<Run> = list(
                self.conn(),
                "ended_at IS NULL AND card_id IN (SELECT id FROM cards WHERE project_id = ?1) ORDER BY started_at",
                &[&project],
            )?;
            let mut lines = Vec::new();
            if let Some(lost) = &lost {
                // R21 names only the agents the holder took down.
                for agent in lost.iter().filter(|a| a.project_id == project) {
                    let state = match self.restored_as(agent)? {
                        Some(live) => match summary.iter().find(|(_, id, _)| *id == live) {
                            Some((_, _, line)) => line.clone(),
                            None => "resumed".to_owned(),
                        },
                        None => "no live session".to_owned(),
                    };
                    lines.push(match &agent.run_id {
                        Some(run) => format!("run {run} ({}): {state}", agent.role.as_str()),
                        None => format!("orchestrator: {state}"),
                    });
                }
            } else {
                for run in &runs {
                    workers_total += 1;
                    let live = live_session(self.conn(), &run.id)?;
                    let state = match &live {
                        Some(s) => match summary
                            .iter()
                            .find(|(p, id, _)| *p == project && *id == s.id)
                        {
                            Some((_, _, line)) => line.clone(),
                            None => {
                                resumed_total += 1;
                                let resumed =
                                    self.attempts(&s.id, started)?.last().is_some_and(|a| {
                                        self.hook_seen_after(&s.id, a.at).unwrap_or(false)
                                    });
                                if resumed {
                                    "resumed".to_owned()
                                } else {
                                    "alive through the reboot check".to_owned()
                                }
                            }
                        },
                        None => "no live session".to_owned(),
                    };
                    lines.push(format!(
                        "run {} ({}): {state}",
                        run.id,
                        run.role.short_name()
                    ));
                }
            }
            if lines.is_empty() {
                continue;
            }
            let (key, body) = match restore {
                Restore::Reboot { boot_id } => (
                    format!("reconcile:R04:summary:{boot_id}:{project}"),
                    format!("host rebooted; restore of {project}:\n{}", lines.join("\n")),
                ),
                Restore::HolderLost { instance } => (
                    format!("reconcile:R21:summary:{instance}:{project}"),
                    format!(
                        "the terminal holder restarted; restore of {project}:\n{}",
                        lines.join("\n")
                    ),
                ),
            };
            if self.note(now, &project, &key, body, None)? {
                report.repair(format!("{row} {project}: restore summary note"));
            }
        }
        let host: tiller_core::domain::Host = get(self.conn(), "host", &self.config.host_id)?;
        match restore {
            Restore::Reboot { boot_id } => {
                self.sources.launcher.notify(
                    &format!("{} rebooted", host.label),
                    &format!("{resumed_total} of {workers_total} workers resumed"),
                );
                self.store
                    .daemon_boot_id(&self.ctx(now), boot_id, instance.as_deref())?;
                report.repair(format!("R04 restore complete: boot id {boot_id} written"));
            }
            Restore::HolderLost { instance } => {
                let lost = lost.unwrap_or_default();
                let mut resumed = 0;
                for agent in &lost {
                    if self
                        .restored_as(agent)?
                        .is_some_and(|live| !summary.iter().any(|(_, id, _)| *id == live))
                    {
                        resumed += 1;
                    }
                }
                self.sources.launcher.notify(
                    &format!("the terminal holder on {} restarted", host.label),
                    &format!("{resumed} of {} agents resumed", lost.len()),
                );
                self.store.holder_restored(&self.ctx(now), instance)?;
                report.repair(format!(
                    "R21 restore complete: holder instance {instance} written"
                ));
            }
        }
        Ok(())
    }

    /// Step 2 of the ladder for a holder pane during a restore: `None`
    /// when this pass has no holder list. A pane of the list whose key
    /// names the session is alive until it exits. A pane the list lacks is
    /// dead after a reboot, which killed every process; after a lost
    /// holder (R21) the recorded pid decides (`kill(pid, 0)`), since a
    /// child that ignored `SIGHUP` may still run.
    fn restore_probe(&self, restore: &Restore, session: &Session) -> Result<Option<Option<Probe>>> {
        let Some(listing) = &self.listing else {
            return Ok(None);
        };
        let listed = listing
            .panes
            .iter()
            .find(|p| session.pane_id.as_deref() == Some(p.pane.as_str()) && p.key == session.id);
        if let Some(pane) = listed {
            return Ok(Some(Some(
                if pane.state == tiller_panes::message::PaneState::Exited {
                    Probe::Dead
                } else {
                    Probe::Alive
                },
            )));
        }
        Ok(Some(match restore {
            Restore::Reboot { .. } => None,
            Restore::HolderLost { .. } => self
                .recorded_pid(session)?
                .map(|pid| self.sources.probe.process(pid))
                .filter(|probe| *probe != Probe::Dead),
        }))
    }

    /// When the holder of `instance` started, from its `welcome`: `None`
    /// when it does not answer or another instance does.
    fn holder_started(&self, instance: &str) -> Option<Timestamp> {
        let welcome = self.sources.holder.as_ref()?.welcome().ok()?;
        (welcome.instance == instance).then(|| Timestamp::from_jiff(welcome.started_at))
    }

    /// The agents the lost holder ran (R21): the sessions bound to a pane
    /// of the instance on record, live when the restore began.
    fn lost_agents(&self, since: Timestamp) -> Result<Vec<Session>> {
        let previous = projects::meta(self.conn(), "holder_instance")?;
        list(
            self.conn(),
            "id IN (SELECT session_id FROM panes WHERE host_id = ?1 AND holder_instance = ?2)
               AND (ended_at IS NULL OR ended_at >= ?3)
             ORDER BY CASE role WHEN 'orchestrator' THEN 0 ELSE 1 END, started_at, id",
            &[&self.config.host_id, &previous, &since],
        )
    }

    /// The live session that carries `agent` on after a restore: the row
    /// itself, resumed with `--resume`, or the fresh row of its run or of
    /// its project's orchestrator.
    fn restored_as(&self, agent: &Session) -> Result<Option<String>> {
        if agent.ended_at.is_none() {
            let row: Session = get(self.conn(), "session", &agent.id)?;
            if row.ended_at.is_none() {
                return Ok(Some(row.id));
            }
        }
        Ok(match &agent.run_id {
            Some(run) => live_session(self.conn(), run)?.map(|s| s.id),
            None => live_orchestrator(self.conn(), &agent.project_id)?.map(|s| s.id),
        })
    }

    /// Where a restored session launches, or why it cannot: a worker needs
    /// its worktree on its branch; an orchestrator needs a checkout path.
    fn restore_target(
        &mut self,
        session: &Session,
        run: Option<&Run>,
    ) -> Result<std::result::Result<PathBuf, String>> {
        let Some(run) = run else {
            return Ok(self
                .config
                .project_paths
                .get(&session.project_id)
                .cloned()
                .ok_or_else(|| "no checkout path in the config".to_owned()));
        };
        let Some(path) = run.worktree_path.clone() else {
            return Ok(Err("run has no worktree".into()));
        };
        let card: Card = get(self.conn(), "card", &run.card_id)?;
        let project = self.project(&card.project_id)?;
        let scan = self.scan_of(Path::new(&path), &project);
        if !scan.exists {
            return Ok(Err(format!("worktree {path} is missing")));
        }
        if !scan.is_worktree {
            return Ok(Err(format!("{path} is not a git worktree")));
        }
        if let (Some(want), Some(have)) = (&run.branch, &scan.branch)
            && want != have
        {
            return Ok(Err(format!("{path} is on {have}, not {want}")));
        }
        Ok(Ok(PathBuf::from(path)))
    }

    fn restore_skipped(
        &mut self,
        restore: &Restore,
        session: &Session,
        run: Option<&Run>,
        why: &str,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let (card, run_id, label) = match run {
            Some(run) => {
                let card: Card = get(self.conn(), "card", &run.card_id)?;
                (
                    card.id.clone(),
                    Some(run.id.clone()),
                    format!(
                        "{} {} run {}",
                        card.display_id,
                        run.role.short_name(),
                        run.id
                    ),
                )
            }
            None => (
                String::new(),
                None,
                format!("the {} orchestrator", session.project_id),
            ),
        };
        let (row, after) = (restore.row(), restore.after());
        let project = session.project_id.clone();
        self.note(
            now,
            &project,
            &format!("reconcile:{row}:skipped-note:{}", session.id),
            format!("after {after}, {label} was not resumed: {why}"),
            run.map(|r| r.card_id.clone()),
        )?;
        if self.recovery(
            now,
            &format!("reconcile:{row}:skipped:{}", session.id),
            NewAttention {
                card,
                run_id,
                question: format!("After {after}, {label} was not resumed: {why}. Relaunch it by hand, abandon it, or wait?"),
                evidence: Self::json_text(json!({"session_id": session.id})),
                subject_project_id: Some(session.project_id.clone()),
                options: Some(json!(["Relaunch", "Abandon", "Wait"])),
                ..NewAttention::default()
            },
        )? {
            report.repair(format!("{row} {label} skipped ({why}): recovery item"));
        }
        Ok(())
    }

    fn restore_failed(
        &mut self,
        restore: &Restore,
        session: &Session,
        run: Option<&Run>,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let (card, run_id, label) = match run {
            Some(run) => {
                let card: Card = get(self.conn(), "card", &run.card_id)?;
                (
                    card.id.clone(),
                    Some(run.id.clone()),
                    format!(
                        "{} {} run {}",
                        card.display_id,
                        run.role.short_name(),
                        run.id
                    ),
                )
            }
            None => (
                String::new(),
                None,
                format!("the {} orchestrator", session.project_id),
            ),
        };
        let (row, after) = (restore.row(), restore.after());
        if self.recovery(
            now,
            &format!("reconcile:{row}:failed:{}", session.id),
            NewAttention {
                card,
                run_id,
                question: format!("After {after}, {label} was resumed twice and bound no session; its pane is left for you. Relaunch, abandon, or wait?"),
                evidence: Self::json_text(json!({"session_id": session.id})),
                subject_project_id: Some(session.project_id.clone()),
                options: Some(json!(["Relaunch", "Abandon", "Wait"])),
                ..NewAttention::default()
            },
        )? {
            report.repair(format!("{row} {label} resume failed twice: recovery item"));
        }
        Ok(())
    }
}

/// The restore that owns the live sessions until its id is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restore {
    /// R04: `meta.boot_id` differs from this boot's id.
    Reboot { boot_id: String },
    /// R21: the holder lists another instance than `meta.holder_instance`
    /// on the same boot.
    HolderLost { instance: String },
}

impl Restore {
    /// The `recovery.md` row: the prefix of every idempotency key.
    pub fn row(&self) -> &'static str {
        match self {
            Self::Reboot { .. } => "R04",
            Self::HolderLost { .. } => "R21",
        }
    }

    /// The resume attempts' `checks.reason`, and the end reason of a
    /// session with nothing to restore.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Reboot { .. } => "reboot",
            Self::HolderLost { .. } => "holder restart",
        }
    }

    /// The event in the owner's words: "after the reboot".
    pub fn after(&self) -> &'static str {
        match self {
            Self::Reboot { .. } => "the reboot",
            Self::HolderLost { .. } => "the holder restart",
        }
    }
}

/// What one resume attempt did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    /// A launch was made (the bind window judges it).
    Launched,
    /// Another launch happened within the launch gap: next pass.
    RateLimited,
    /// A floor refused it; the attempt event carries the reason.
    Skipped,
}

/// What a project has in flight when its orchestrator dies (R02).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InFlight {
    pub runs: i64,
    pub messages: i64,
    pub asks: i64,
}

impl InFlight {
    pub fn any(self) -> bool {
        self.runs > 0 || self.messages > 0 || self.asks > 0
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_manual_resume_never_uses_up_the_daemons_attempts() {
        let attempt = |reason: &str| Attempt {
            at: Timestamp::now(),
            payload: json!({ "checks": { "reason": reason } }),
        };
        let attempts = [
            attempt("crash"),
            attempt(MANUAL_RESUME_REASON),
            attempt(MANUAL_RESUME_REASON),
        ];
        assert_eq!(automatic(&attempts), 1);
        assert_eq!(
            automatic(&[Attempt {
                at: Timestamp::now(),
                payload: json!({})
            }]),
            1
        );
    }
}
