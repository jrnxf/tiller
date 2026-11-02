//! Step 4: the ladder for every live session, and what each verdict
//! triggers: R02 (orchestrator crash), R03 (worker crash), R04 (the reboot
//! restore) and R12 (worker silent).

use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use tiller_core::domain::{Activity, Card, ObservationSource, Role, Run, Session};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::runs::{Bind, NewObservation, live_session};
use tiller_core::state::{find, get, list};
use tiller_core::{Result, Timestamp};
use tiller_observe::liveness::Probe;

use crate::ladder::{self, Death, Verdict};
use crate::pass::{Reconciler, Report, minutes_since, ts};

/// The `source` a resume launch binds the new pane under, so the proof of
/// life is a hook's bind, never the launcher's own.
pub const LAUNCH_BIND_SOURCE: &str = "resume_launch";

/// One `session.resume_attempted` event.
#[derive(Clone, Debug)]
pub(crate) struct Attempt {
    pub at: Timestamp,
    pub payload: Value,
}

impl Reconciler {
    pub(crate) fn sessions(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        // Live panes the records lost track of (a restore, an owner flip)
        // are rebound first, so the ladder never reads them as gone.
        self.adopt_panes(now, report)?;
        if self.restore_pending()? {
            // The restore owns every live session until the boot id is
            // written; the ladder would call its dead sessions crashes.
            self.reboot_restore(now, report)?;
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
        let verdict = ladder::climb(
            session,
            &effective,
            errored,
            self.snapshot.as_ref(),
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
        if !self.key_used(&note_key)? {
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
        let note_at = self.message_at(&note_key)?;
        let earlier_silences: i64 = self.conn().query_row(
            "SELECT count(*) FROM outbox WHERE idempotency_key LIKE ?1 AND idempotency_key != ?2 AND created_at >= ?3",
            rusqlite::params![format!("reconcile:R12:note:{run_id}:%"), note_key, now.plus_minutes(-t.repeat_window_hours * 60)],
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
        if self.key_used(&failed_key)? {
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
        if attempts.len() < 2 {
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
                        evidence: Self::json_text(json!({"session_id": session.id, "attempts": attempts.len()})),
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
                        evidence: Self::json_text(json!({"session_id": session.id, "attempts": attempts.len()})),
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

    // ----- R04 -----

    /// The reboot restore: orchestrators of projects with work in flight,
    /// then each active run in its own worktree, one launch per ten seconds,
    /// each step checking that the session is not already alive. The boot
    /// id is written last, when every session is alive, settled or
    /// reported, so a daemon that dies mid-restore repeats it safely.
    pub(crate) fn reboot_restore(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let Some(snapshot) = self.snapshot.clone() else {
            return Ok(());
        };
        let Some(boot_id) = self.config.boot_id.clone() else {
            return Ok(());
        };
        let started = self.started_at.unwrap_or(now);
        let mut pending = false;
        // `(project, line)` for the summary: what was not simply resumed.
        let mut summary: Vec<(String, String, String)> = Vec::new();
        for session in self.live_sessions()? {
            let probe =
                ladder::bound_pane(&snapshot, &session).map(|p| self.sources.probe.agent(p));
            if probe == Some(Probe::Unknown) && !self.hook_seen_after(&session.id, started)? {
                // Present, unproven: never a second agent on one run.
                pending = true;
                continue;
            }
            let alive =
                probe == Some(Probe::Alive) || self.hook_seen_after(&session.id, started)?;
            if alive {
                // A session this restore launched that sits at a trust or
                // permission prompt: nothing is typed, the summary names it.
                if self.at_prompt(&session)
                    && !self.hook_seen_after(&session.id, started)?
                    && !self.attempts(&session.id, started)?.is_empty()
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
                    .session_end(&self.ctx(now), &session.id, "reboot")?;
                report.repair(format!(
                    "R04 {} orchestrator had nothing in flight: session ended",
                    session.project_id
                ));
                continue;
            }
            let project = session.project_id.clone();
            let skipped_key = format!("reconcile:R04:skipped:{}", session.id);
            let floor_key = format!("reconcile:resume:skipped:{}", session.id);
            let failed_key = format!("reconcile:R04:failed:{}", session.id);
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
            let attempts = self.attempts(&session.id, started)?;
            match attempts.last() {
                None => match self.restore_target(&session, run.as_ref())? {
                    Ok(cwd) => {
                        match self.attempt_resume(&session, &cwd, true, "reboot", now, report)? {
                            Launch::Launched | Launch::RateLimited => pending = true,
                            Launch::Skipped => summary.push((
                                project,
                                session.id.clone(),
                                "skipped for a floor".into(),
                            )),
                        }
                    }
                    Err(why) => {
                        self.restore_skipped(&session, run.as_ref(), &why, now, report)?;
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
                    if attempts.len() < 2 {
                        if let Ok(cwd) = self.restore_target(&session, run.as_ref())? {
                            self.attempt_resume(&session, &cwd, false, "reboot", now, report)?;
                        }
                        pending = true;
                        continue;
                    }
                    self.restore_failed(&session, run.as_ref(), now, report)?;
                    summary.push((
                        project,
                        session.id.clone(),
                        "resumed twice, bound nothing".into(),
                    ));
                }
            }
        }
        if pending {
            return Ok(());
        }
        // Complete: the summary per project, one notification, boot id last.
        let projects: Vec<String> = {
            let mut stmt = self
                .conn()
                .prepare("SELECT id FROM projects WHERE retired_at IS NULL ORDER BY id")?;
            let rows = stmt.query_map([], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut resumed_total = 0;
        let mut workers_total = 0;
        for project in projects {
            let runs: Vec<Run> = list(
                self.conn(),
                "ended_at IS NULL AND card_id IN (SELECT id FROM cards WHERE project_id = ?1) ORDER BY started_at",
                &[&project],
            )?;
            if runs.is_empty() {
                continue;
            }
            let mut lines = Vec::new();
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
                            let resumed = self.attempts(&s.id, started)?.last().is_some_and(|a| {
                                self.hook_seen_after(&s.id, a.at).unwrap_or(false)
                            });
                            if resumed {
                                resumed_total += 1;
                                "resumed".to_owned()
                            } else {
                                resumed_total += 1;
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
            let key = format!("reconcile:R04:summary:{boot_id}:{project}");
            let body = format!("host rebooted; restore of {project}:\n{}", lines.join("\n"));
            if self.note(now, &project, &key, body, None)? {
                report.repair(format!("R04 {project}: reboot summary note"));
            }
        }
        let host: tiller_core::domain::Host = get(self.conn(), "host", &self.config.host_id)?;
        self.sources.launcher.notify(
            &format!("{} rebooted", host.label),
            &format!("{resumed_total} of {workers_total} workers resumed"),
        );
        self.store.daemon_boot_id(&self.ctx(now), &boot_id)?;
        report.repair(format!("R04 restore complete: boot id {boot_id} written"));
        Ok(())
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
        let project = session.project_id.clone();
        self.note(
            now,
            &project,
            &format!("reconcile:R04:skipped-note:{}", session.id),
            format!("after the reboot, {label} was not resumed: {why}"),
            run.map(|r| r.card_id.clone()),
        )?;
        if self.recovery(
            now,
            &format!("reconcile:R04:skipped:{}", session.id),
            NewAttention {
                card,
                run_id,
                question: format!("After the reboot, {label} was not resumed: {why}. Relaunch it by hand, abandon it, or wait?"),
                evidence: Self::json_text(json!({"session_id": session.id})),
                options: Some(json!(["Relaunch", "Abandon", "Wait"])),
                ..NewAttention::default()
            },
        )? {
            report.repair(format!("R04 {label} skipped ({why}): recovery item"));
        }
        Ok(())
    }

    fn restore_failed(
        &mut self,
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
        if self.recovery(
            now,
            &format!("reconcile:R04:failed:{}", session.id),
            NewAttention {
                card,
                run_id,
                question: format!("After the reboot, {label} was resumed twice and bound no session; its pane is left for you. Relaunch, abandon, or wait?"),
                evidence: Self::json_text(json!({"session_id": session.id})),
                options: Some(json!(["Relaunch", "Abandon", "Wait"])),
                ..NewAttention::default()
            },
        )? {
            report.repair(format!("R04 {label} resume failed twice: recovery item"));
        }
        Ok(())
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
