//! A project's stage and its batched reviews: `project.stage`,
//! `review_pass.start`, `review_pass.finish` and `review_pass.abandon`
//! (`docs/design/pre-launch-validation.md`).
//!
//! A pre-launch project skips the `no-mistakes` pipeline and gets one review
//! a week of everything merged since `reviewed_through_sha`. The due date is
//! an ordinary `date` obligation at the next Saturday 06:00 Lisbon slot, which
//! the reconcile timer meets; the pass itself is a scout card plus one
//! `review_passes` row. Switching to launched turns validation back on at
//! once and opens one final pass.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::attention::{NewAttention, open_attention};
use super::cards::{NewCard, close_card, create_card};
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::projects::require_owner;
use super::records::{NewObligation, cancel_obligation, open_obligation};
use super::{check_rev, find, get, list, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, FLEET_PROJECT};
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::{Timestamp, lisbon};

/// The `what` of a project's weekly review obligation.
pub const REVIEW_WHAT: &str = "pre-launch review";

/// The category of pass cards and of the fix cards a pass files.
pub const REVIEW_CATEGORY: &str = "pre-launch-review";

/// How long a met weekly obligation may wait for its pass before the board
/// shows the review as overdue.
pub const OVERDUE_MINUTES: i64 = 24 * 60;

/// What a stage change did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Staged {
    pub project: Project,
    /// The first weekly due date, on a switch to pre-launch.
    pub obligation: Option<Obligation>,
    /// The weekly due dates a switch to launched cancelled.
    pub cancelled_obligations: Vec<String>,
    /// The final pass a switch to launched opened. `None` while a weekly
    /// pass is still running: its finish opens the final pass.
    pub final_pass: Option<ReviewPass>,
    /// Open PRs with no pipeline run, which must go through the gate before
    /// they merge now that the project is launched.
    pub unvalidated_prs: Vec<Pr>,
}

/// What a finished pass did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finished {
    pub pass: ReviewPass,
    pub project: Project,
    pub attention: Option<Attention>,
    pub next_obligation: Option<Obligation>,
    pub final_pass: Option<ReviewPass>,
}

/// What an abandoned pass did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Abandoned {
    pub pass: ReviewPass,
    pub card: Card,
    pub next_obligation: Option<Obligation>,
    pub final_pass: Option<ReviewPass>,
}

/// A weekly review that is due: its obligation was met and no pass started
/// from it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DueReview {
    pub project_id: String,
    pub obligation: Obligation,
    /// Due for more than [`OVERDUE_MINUTES`].
    pub overdue: bool,
}

/// Whether the project is pre-launch. An unknown project is launched: rigor
/// is never lowered by a missing row.
pub fn prelaunch(conn: &rusqlite::Connection, project_id: &str) -> Result<bool> {
    let stage: Option<Stage> = conn
        .query_row(
            "SELECT stage FROM projects WHERE id = ?1",
            [project_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stage == Some(Stage::PreLaunch))
}

/// The project's pending or running pass, if any (at most one).
pub fn live_pass(conn: &rusqlite::Connection, project_id: &str) -> Result<Option<ReviewPass>> {
    find(
        conn,
        "project_id = ?1 AND state IN ('pending', 'running')",
        &[&project_id],
    )
}

/// The project's open weekly due date, if any.
pub fn open_weekly(conn: &rusqlite::Connection, project_id: &str) -> Result<Option<Obligation>> {
    find(
        conn,
        "project_id = ?1 AND card_id IS NULL AND what = ?2 AND trigger_kind = 'date' AND state = 'open'
         ORDER BY due_at",
        &[&project_id, &REVIEW_WHAT],
    )
}

/// Weekly reviews that are due: met obligations with no pass started from
/// them, oldest first, for one project or all. Only obligations met since the
/// project last became pre-launch count: one met before a launch is covered
/// by the final pass.
pub fn due_reviews(
    conn: &rusqlite::Connection,
    project_id: Option<&str>,
    now: Timestamp,
) -> Result<Vec<DueReview>> {
    let met: Vec<Obligation> = list(
        conn,
        "card_id IS NULL AND what = ?1 AND trigger_kind = 'date' AND state = 'met'
         AND (?2 IS NULL OR project_id = ?2)
         AND NOT EXISTS (SELECT 1 FROM review_passes p WHERE p.obligation_id = obligations.id)
         AND EXISTS (SELECT 1 FROM projects WHERE id = obligations.project_id AND stage = 'pre-launch'
                     AND retired_at IS NULL AND met_at >= coalesce(stage_changed_at, ''))
         ORDER BY met_at, id",
        &[&REVIEW_WHAT, &project_id],
    )?;
    let overdue_before = now.plus_minutes(-OVERDUE_MINUTES);
    Ok(met
        .into_iter()
        .map(|obligation| DueReview {
            project_id: obligation.project_id.clone(),
            overdue: obligation.met_at.is_some_and(|at| at <= overdue_before),
            obligation,
        })
        .collect())
}

/// The weekly review a new pass starts from, for a project with no live
/// pass: refused when the project is launched or nothing is due yet. The CLI
/// asks before it reads the range, so a refusal costs no fetch.
pub fn due_to_start(
    conn: &rusqlite::Connection,
    project: &Project,
    now: Timestamp,
) -> Result<DueReview> {
    if project.stage != Stage::PreLaunch {
        return Err(Error::refused(
            "project",
            &project.id,
            "the project is launched; no weekly review is due",
        ));
    }
    match due_reviews(conn, Some(&project.id), now)?
        .into_iter()
        .next()
    {
        Some(due) => Ok(due),
        None => {
            let next = open_weekly(conn, &project.id)?
                .and_then(|o| o.due_at)
                .map_or_else(
                    || "none is scheduled".to_owned(),
                    |at| format!("the next is due {at}"),
                );
            Err(Error::refused(
                "project",
                &project.id,
                format!("no weekly review is due; {next}"),
            ))
        }
    }
}

/// The chunks of a pass that are covered: a live `report` artifact titled
/// `chunk <n> of <total>` on the pass card, from a run that also recorded a
/// review there, both since the pass started. A re-run of a cut chunk covers
/// the same chunk again, and the summary run's review covers none.
fn covered_chunks(
    conn: &rusqlite::Connection,
    pass: &ReviewPass,
    chunks: usize,
) -> Result<std::collections::BTreeSet<usize>> {
    let mut statement = conn.prepare(
        "SELECT DISTINCT a.title FROM artifacts a
         JOIN reviews r ON r.run_id = a.run_id AND r.card_id = a.card_id AND r.recorded_at >= ?2
         WHERE a.card_id = ?1 AND a.kind = 'report' AND a.retired_at IS NULL AND a.created_at >= ?2",
    )?;
    let titles = statement
        .query_map(params![pass.card_id, pass.started_at], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(titles
        .iter()
        .filter_map(|title| {
            let rest = title.trim().strip_prefix("chunk ")?;
            let (chunk, total) = rest.split_once(" of ")?;
            (total.trim() == chunks.to_string()).then(|| chunk.trim().parse().ok())?
        })
        .filter(|chunk| (1..=chunks).contains(chunk))
        .collect())
}

/// How many chunks a stored plan has: a bare array of chunks, or the plan
/// object `tiller review plan` prints, whose `chunks` is that array.
pub fn chunk_count(plan: &Value) -> Option<usize> {
    plan.as_array()
        .or_else(|| plan.get("chunks").and_then(Value::as_array))
        .map(Vec::len)
}

/// Resolves a pass reference: its id, short form, or its card.
pub fn resolve_pass(conn: &rusqlite::Connection, input: &str) -> Result<ReviewPass> {
    if let Ok(id) = resolve_id(conn, "review_passes", "review pass", input) {
        return get(conn, "review pass", &id);
    }
    let card =
        resolve_card(conn, input).map_err(|_| Error::not_found("review pass", input.trim()))?;
    find(conn, "card_id = ?1", &[&card.id])?
        .ok_or_else(|| Error::not_found("review pass", input.trim()))
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn require_sha(name: &str, sha: &str) -> Result<()> {
    if (7..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "{name} must be a commit SHA, got {sha:?}"
        )))
    }
}

fn open_next_weekly(tx: &mut Tx<'_>, project_id: &str) -> Result<Option<Obligation>> {
    if open_weekly(tx.conn(), project_id)?.is_some() {
        return Ok(None);
    }
    open_obligation(
        tx,
        &NewObligation {
            project_id: project_id.to_owned(),
            card_id: None,
            what: REVIEW_WHAT.to_owned(),
            trigger_kind: TriggerKind::Date,
            trigger_event: None,
            due_at: Some(lisbon::next_review_slot(tx.now())),
        },
    )
    .map(Some)
}

fn note_orchestrator(tx: &mut Tx<'_>, project_id: &str, card: &Card, body: String) -> Result<()> {
    if let Some(orchestrator) = live_orchestrator(tx.conn(), project_id)? {
        enqueue(
            tx,
            &NewMessage {
                target_session_id: orchestrator.id,
                kind: OutboxKind::Note,
                body,
                card_id: Some(card.id.clone()),
                reply_to: None,
            },
        )?;
    }
    Ok(())
}

fn pass_card(tx: &mut Tx<'_>, project: &Project, kind: ReviewPassKind, base: &str) -> Result<Card> {
    let (title, body) = match kind {
        ReviewPassKind::Weekly => (
            "Weekly pre-launch review".to_owned(),
            format!(
                "The batched review of everything merged to {} since {}: correctness bugs, security issues and regressions only.",
                project.default_branch,
                short(base)
            ),
        ),
        ReviewPassKind::Final => (
            "Final pre-launch review".to_owned(),
            format!(
                "The last batched review before launch, of everything merged to {} since {}. Validation is already back on.",
                project.default_branch,
                short(base)
            ),
        ),
    };
    create_card(
        tx,
        &NewCard {
            project_id: project.id.clone(),
            kind: CardKind::Scout,
            title,
            body: Some(body),
            priority: Some(1),
            category: Some(REVIEW_CATEGORY.to_owned()),
            ..NewCard::default()
        },
        None,
    )
}

fn insert_pass(
    tx: &mut Tx<'_>,
    project: &Project,
    card: &Card,
    kind: ReviewPassKind,
    base: &str,
    obligation_id: Option<&str>,
) -> Result<ReviewPass> {
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO review_passes (id, project_id, card_id, kind, base_sha, obligation_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, project.id, card.id, kind, base, obligation_id, tx.now()],
    )?;
    tx.event(
        NewEvent::new("review_pass", &id, "review_pass.created")
            .rev(1)
            .card(&card.id)
            .payload(json!({"kind": kind, "base_sha": base, "obligation_id": obligation_id})),
    )?;
    get(tx.conn(), "review pass", &id)
}

/// Opens the final pass from the project's last reviewed commit, and wakes
/// the orchestrator to start it.
fn open_final_pass(tx: &mut Tx<'_>, project_id: &str) -> Result<ReviewPass> {
    let project: Project = get(tx.conn(), "project", project_id)?;
    let base = project.reviewed_through_sha.clone().ok_or_else(|| {
        Error::refused(
            "project",
            &project.id,
            "no reviewed commit is recorded to start the final review from",
        )
    })?;
    let card = pass_card(tx, &project, ReviewPassKind::Final, &base)?;
    let pass = insert_pass(tx, &project, &card, ReviewPassKind::Final, &base, None)?;
    note_orchestrator(
        tx,
        &project.id,
        &card,
        format!(
            "[{}] final pre-launch review is pending: run tiller review start {}",
            card.display_id, project.id
        ),
    )?;
    Ok(pass)
}

fn set_pass_state(
    tx: &mut Tx<'_>,
    pass: &ReviewPass,
    state: ReviewPassState,
    kind: &'static str,
    payload: Value,
) -> Result<()> {
    tx.conn().execute(
        "UPDATE review_passes SET state = ?2, finished_at = ?3, rev = rev + 1 WHERE id = ?1",
        params![pass.id, state, tx.now()],
    )?;
    tx.event(
        NewEvent::new("review_pass", &pass.id, kind)
            .rev(pass.rev + 1)
            .card(&pass.card_id)
            .payload(payload),
    )?;
    Ok(())
}

impl Store {
    /// Whether [`Store::project_stage`] with these arguments needs the
    /// default-branch head: only a switch to pre-launch that applies. A
    /// replayed key, a stale rev or the current stage need no fetch, and
    /// `project_stage` answers them without a head.
    pub fn stage_reads_head(
        &self,
        ctx: &Ctx,
        project_id: &str,
        expected_rev: i64,
        stage: Stage,
    ) -> Result<bool> {
        if stage != Stage::PreLaunch || self.key_recorded(ctx)? {
            return Ok(false);
        }
        let project: Project = get(self.conn(), "project", project_id)?;
        Ok(project.rev == expected_rev && project.stage != stage)
    }

    /// `project.stage`, owner only, against `expected_rev`. To pre-launch it
    /// records `head_sha` (the default-branch head the caller read) as the
    /// last reviewed commit and opens the first weekly due date. To launched
    /// it cancels the weekly due date and opens the final pass. A stale
    /// `rev` is a conflict; the current stage at the current `rev` is a
    /// no-op.
    pub fn project_stage(
        &mut self,
        ctx: &Ctx,
        project_id: &str,
        expected_rev: i64,
        stage: Stage,
        head_sha: Option<&str>,
    ) -> Result<Applied<Staged>> {
        require_owner(&ctx.actor, "project.stage")?;
        if let Some(sha) = head_sha {
            require_sha("the default-branch head", sha)?;
        }
        self.write(ctx, "project.stage", None, |tx| {
            let project: Project = get(tx.conn(), "project", project_id)?;
            check_rev(
                "project",
                &project.id,
                project.rev,
                expected_rev,
                Some(("stage", json!(project.stage))),
                Some(project.edited_at),
            )?;
            let mut staged = Staged {
                project: project.clone(),
                obligation: None,
                cancelled_obligations: Vec::new(),
                final_pass: None,
                unvalidated_prs: Vec::new(),
            };
            if project.stage == stage {
                return Ok(staged);
            }
            if project.id == FLEET_PROJECT {
                return Err(Error::refused("project", &project.id, "the fleet project has no stage"));
            }
            if project.retired_at.is_some() {
                return Err(Error::refused("project", &project.id, "project is retired"));
            }
            match stage {
                Stage::PreLaunch => {
                    let head = head_sha.ok_or_else(|| {
                        Error::Usage("pre-launch starts from the default-branch head; pass its SHA".into())
                    })?;
                    if let Some(pass) = live_pass(tx.conn(), &project.id)? {
                        let card: Card = get(tx.conn(), "card", &pass.card_id)?;
                        return Err(Error::refused(
                            "project",
                            &project.id,
                            format!("the {} review on {} is still {}; finish or abandon it first", pass.kind, card.display_id, pass.state),
                        ));
                    }
                    tx.conn().execute(
                        "UPDATE projects SET stage = ?2, reviewed_through_sha = ?3, stage_changed_at = ?4, rev = rev + 1,
                                             edited_at = ?4
                         WHERE id = ?1",
                        params![project.id, stage, head, tx.now()],
                    )?;
                    tx.event(
                        NewEvent::new("project", &project.id, "project.staged")
                            .rev(project.rev + 1)
                            .payload(json!({
                                "from": project.stage,
                                "to": stage,
                                "reviewed_through_sha": head,
                                "expected_rev": expected_rev,
                            })),
                    )?;
                    staged.obligation = open_next_weekly(tx, &project.id)?;
                }
                Stage::Launched => {
                    tx.conn().execute(
                        "UPDATE projects SET stage = ?2, stage_changed_at = ?3, rev = rev + 1, edited_at = ?3
                         WHERE id = ?1",
                        params![project.id, stage, tx.now()],
                    )?;
                    tx.event(
                        NewEvent::new("project", &project.id, "project.staged")
                            .rev(project.rev + 1)
                            .payload(json!({
                                "from": project.stage,
                                "to": stage,
                                "reviewed_through_sha": project.reviewed_through_sha,
                                "expected_rev": expected_rev,
                            })),
                    )?;
                    while let Some(weekly) = open_weekly(tx.conn(), &project.id)? {
                        cancel_obligation(tx, &weekly, "project launched")?;
                        staged.cancelled_obligations.push(weekly.id);
                    }
                    staged.final_pass = match live_pass(tx.conn(), &project.id)? {
                        // A weekly pass not yet started has nothing to keep;
                        // the final pass covers its range.
                        Some(pass) if pass.state == ReviewPassState::Pending => {
                            set_pass_state(
                                tx,
                                &pass,
                                ReviewPassState::Abandoned,
                                "review_pass.abandoned",
                                json!({"reason": "project launched"}),
                            )?;
                            let card: Card = get(tx.conn(), "card", &pass.card_id)?;
                            close_card(tx, &card, CardOutcome::Superseded, json!({"reason": "project launched"}))?;
                            Some(open_final_pass(tx, &project.id)?)
                        }
                        // A running weekly pass continues and its result
                        // counts; its finish opens the final pass.
                        Some(_) => None,
                        None => Some(open_final_pass(tx, &project.id)?),
                    };
                    staged.unvalidated_prs = list(
                        tx.conn(),
                        "project_id = ?1 AND state = 'open' AND pipeline_state = 'none' ORDER BY number",
                        &[&project.id],
                    )?;
                }
            }
            staged.project = get(tx.conn(), "project", &project.id)?;
            Ok(staged)
        })
    }

    /// `review_pass.start`: fixes the pass head and stores its chunk plan.
    /// Starts the project's pending final pass if there is one; otherwise
    /// opens a weekly pass from the oldest due weekly obligation. Natural:
    /// the running pass with the same head and plan is returned.
    pub fn review_start(
        &mut self,
        ctx: &Ctx,
        project_id: &str,
        head_sha: &str,
        chunks: Value,
    ) -> Result<Applied<ReviewPass>> {
        require_sha("the pass head", head_sha)?;
        if chunk_count(&chunks).is_none() {
            return Err(Error::Usage(
                "the chunk plan must be a JSON array of chunks or an object with a `chunks` array"
                    .into(),
            ));
        }
        self.write(ctx, "review_pass.start", None, |tx| {
            let project: Project = get(tx.conn(), "project", project_id)?;
            let pass = match live_pass(tx.conn(), &project.id)? {
                Some(pass) if pass.state == ReviewPassState::Running => {
                    if pass.head_sha.as_deref() == Some(head_sha) && pass.chunks == chunks {
                        return Ok(pass);
                    }
                    let card: Card = get(tx.conn(), "card", &pass.card_id)?;
                    return Err(Error::refused(
                        "review pass",
                        &pass.id,
                        format!("the {} review on {} is already running", pass.kind, card.display_id),
                    ));
                }
                Some(pending) => pending,
                None => {
                    let due = due_to_start(tx.conn(), &project, tx.now())?;
                    let base = project.reviewed_through_sha.clone().ok_or_else(|| {
                        Error::refused("project", &project.id, "no reviewed commit is recorded")
                    })?;
                    let card = pass_card(tx, &project, ReviewPassKind::Weekly, &base)?;
                    insert_pass(tx, &project, &card, ReviewPassKind::Weekly, &base, Some(&due.obligation.id))?
                }
            };
            tx.conn().execute(
                "UPDATE review_passes SET state = 'running', head_sha = ?2, chunks = ?3, started_at = ?4, rev = rev + 1
                 WHERE id = ?1",
                params![pass.id, head_sha, chunks, tx.now()],
            )?;
            tx.event(
                NewEvent::new("review_pass", &pass.id, "review_pass.started")
                    .rev(pass.rev + 1)
                    .card(&pass.card_id)
                    .payload(json!({
                        "base_sha": pass.base_sha,
                        "head_sha": head_sha,
                        "chunks": chunk_count(&chunks),
                    })),
            )?;
            get(tx.conn(), "review pass", &pass.id)
        })
    }

    /// `review_pass.finish`: every chunk has its review row and the report
    /// page exists. Advances `reviewed_through_sha` from the pass base to its
    /// head (compare-and-set), records the report and fix cards, opens the
    /// `review` item that points the owner at the report, and opens the next
    /// weekly due date while the project is pre-launch. A weekly pass that
    /// finishes after launch opens the final pass.
    pub fn review_finish(
        &mut self,
        ctx: &Ctx,
        pass: &str,
        report: Option<&str>,
        fix_cards: &[String],
    ) -> Result<Applied<Finished>> {
        self.write(ctx, "review_pass.finish", None, |tx| {
            let pass = resolve_pass(tx.conn(), pass)?;
            let project: Project = get(tx.conn(), "project", &pass.project_id)?;
            if pass.state == ReviewPassState::Finished {
                return Ok(Finished { pass, project, attention: None, next_obligation: None, final_pass: None });
            }
            if pass.state != ReviewPassState::Running {
                return Err(Error::refused("review pass", &pass.id, format!("pass is {}", pass.state)));
            }
            let head = pass.head_sha.clone().ok_or_else(|| {
                Error::Internal(format!("running pass {} has no head", pass.id))
            })?;
            let card: Card = get(tx.conn(), "card", &pass.card_id)?;
            let chunks = chunk_count(&pass.chunks).unwrap_or(0);
            let covered = covered_chunks(tx.conn(), &pass, chunks)?;
            if let Some(missing) = (1..=chunks).find(|chunk| !covered.contains(chunk)) {
                return Err(Error::refused(
                    "review pass",
                    &pass.id,
                    format!(
                        "{} of {chunks} chunks have a review and a chunk report on {}; chunk {missing} has none",
                        covered.len(),
                        card.display_id
                    ),
                ));
            }
            let (high, medium, low): (i64, i64, i64) = tx.conn().query_row(
                "SELECT coalesce(sum(findings_high), 0), coalesce(sum(findings_medium), 0),
                        coalesce(sum(findings_low), 0)
                 FROM reviews WHERE card_id = ?1 AND recorded_at >= ?2",
                params![pass.card_id, pass.started_at],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let report: Artifact = match report {
                Some(reference) => {
                    let id = resolve_id(tx.conn(), "artifacts", "artifact", reference)?;
                    get(tx.conn(), "artifact", &id)?
                }
                None => find(
                    tx.conn(),
                    "card_id = ?1 AND kind = 'page' AND retired_at IS NULL ORDER BY created_at DESC, id DESC",
                    &[&pass.card_id],
                )?
                .ok_or_else(|| {
                    Error::refused("review pass", &pass.id, format!("no report page is recorded on {}", card.display_id))
                })?,
            };
            if report.card_id != pass.card_id || report.kind != ArtifactKind::Page || report.retired_at.is_some() {
                return Err(Error::refused(
                    "artifact",
                    &report.id,
                    format!("the report must be a live page artifact on {}", card.display_id),
                ));
            }
            let mut fixes = Vec::new();
            for reference in fix_cards {
                let fix = resolve_card(tx.conn(), reference)?;
                if fix.project_id != project.id {
                    return Err(Error::refused("card", &fix.display_id, format!("card belongs to project {}", fix.project_id)));
                }
                if !fixes.contains(&fix.id) {
                    fixes.push(fix.id);
                }
            }
            if project.reviewed_through_sha.as_deref() != Some(pass.base_sha.as_str()) {
                return Err(Error::refused(
                    "project",
                    &project.id,
                    format!(
                        "the last reviewed commit is {}, not the pass base {}",
                        project.reviewed_through_sha.as_deref().unwrap_or("none"),
                        pass.base_sha
                    ),
                ));
            }
            tx.conn().execute(
                "UPDATE projects SET reviewed_through_sha = ?2, rev = rev + 1, edited_at = ?3 WHERE id = ?1",
                params![project.id, head, tx.now()],
            )?;
            tx.event(
                NewEvent::new("project", &project.id, "project.reviewed")
                    .rev(project.rev + 1)
                    .payload(json!({"from": pass.base_sha, "to": head, "pass_id": pass.id})),
            )?;
            tx.conn().execute(
                "UPDATE review_passes SET state = 'finished', finished_at = ?2, report_artifact_id = ?3, fix_card_ids = ?4,
                        rev = rev + 1
                 WHERE id = ?1",
                params![pass.id, tx.now(), report.id, json!(fixes)],
            )?;
            tx.event(
                NewEvent::new("review_pass", &pass.id, "review_pass.finished")
                    .rev(pass.rev + 1)
                    .card(&pass.card_id)
                    .payload(json!({
                        "head_sha": head,
                        "report_artifact_id": report.id,
                        "fix_card_ids": fixes,
                        "findings": [high, medium, low],
                    })),
            )?;
            let attention = open_attention(
                tx,
                &NewAttention {
                    card: card.id.clone(),
                    kind: AttentionKind::Review,
                    question: format!(
                        "The {} pre-launch review of {} through {} is ready: {high} high, {medium} medium and {low} low findings across {chunks} chunks, {} fix cards filed.",
                        pass.kind,
                        project.id,
                        short(&head),
                        fixes.len()
                    ),
                    evidence: Some(report.url.clone().or(report.path.clone()).unwrap_or_default()),
                    recommendation: Some("Read the report; answering closes the review card.".into()),
                    ..NewAttention::default()
                },
            )?;
            let project: Project = get(tx.conn(), "project", &project.id)?;
            let (next_obligation, final_pass) = match (pass.kind, project.stage) {
                (ReviewPassKind::Weekly, Stage::PreLaunch) => (open_next_weekly(tx, &project.id)?, None),
                (ReviewPassKind::Weekly, Stage::Launched) => (None, Some(open_final_pass(tx, &project.id)?)),
                (ReviewPassKind::Final, _) => (None, None),
            };
            Ok(Finished {
                pass: get(tx.conn(), "review pass", &pass.id)?,
                project: get(tx.conn(), "project", &project.id)?,
                attention: Some(attention),
                next_obligation,
                final_pass,
            })
        })
    }

    /// `review_pass.abandon`: closes the pass card `abandoned` and leaves
    /// `reviewed_through_sha` alone, so the next pass covers the same commits
    /// again. A weekly pass opens the next due date while pre-launch, or the
    /// final pass after launch; an abandoned final pass is replaced by a new
    /// pending one, so launch never skips its review.
    pub fn review_abandon(
        &mut self,
        ctx: &Ctx,
        pass: &str,
        reason: &str,
    ) -> Result<Applied<Abandoned>> {
        required("reason", reason)?;
        self.write(ctx, "review_pass.abandon", None, |tx| {
            let pass = resolve_pass(tx.conn(), pass)?;
            let card: Card = get(tx.conn(), "card", &pass.card_id)?;
            if pass.state == ReviewPassState::Abandoned {
                return Ok(Abandoned {
                    pass,
                    card,
                    next_obligation: None,
                    final_pass: None,
                });
            }
            if pass.state == ReviewPassState::Finished {
                return Err(Error::refused("review pass", &pass.id, "pass is finished"));
            }
            set_pass_state(
                tx,
                &pass,
                ReviewPassState::Abandoned,
                "review_pass.abandoned",
                json!({"reason": reason}),
            )?;
            close_card(tx, &card, CardOutcome::Abandoned, json!({"reason": reason}))?;
            let project: Project = get(tx.conn(), "project", &pass.project_id)?;
            let (next_obligation, final_pass) = match (pass.kind, project.stage) {
                (ReviewPassKind::Weekly, Stage::PreLaunch) => {
                    (open_next_weekly(tx, &project.id)?, None)
                }
                (ReviewPassKind::Weekly, Stage::Launched) | (ReviewPassKind::Final, _) => {
                    (None, Some(open_final_pass(tx, &project.id)?))
                }
            };
            Ok(Abandoned {
                pass: get(tx.conn(), "review pass", &pass.id)?,
                card: get(tx.conn(), "card", &card.id)?,
                next_obligation,
                final_pass,
            })
        })
    }
}

#[cfg(test)]
mod tests;
