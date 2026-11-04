//! Step 6: approvals and PRs, in this order: `uncertain` executions first
//! (R08), then stale bindings (R08, inside `pr.refresh`), then PRs merged
//! or closed with no execution of Tiller's own (R07, R19). An execution the
//! daemon lost track of is resolved before anything calls it an outside
//! merge. Then the owner's answers to R07's question are carried out. Last,
//! decisions by the wrong principal raise the unauthorized-action trigger
//! (G04, `authority.md` section 11).

use std::path::Path;

use serde_json::{Value, json};
use tiller_core::domain::lifecycle::Finish;
use tiller_core::domain::{
    Approval, ApprovalKind, Card, CardOutcome, CardState, Delivery, GrantKind, Pr, PrState,
    Project, Run, RunOutcome,
};
use tiller_core::state::approvals::{NewApproval, PrFetch};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::MARK_LANDED;
use tiller_core::state::runs::EndRun;
use tiller_core::state::{find, get, list};
use tiller_core::{Result, Timestamp};

use crate::grants;
use crate::pass::{Reconciler, Report};
use crate::sources::PrRead;

/// The answers R07's item offers. The two landed ones start with
/// [`MARK_LANDED`], which `run.end landed` and `card.close landed` accept as
/// the owner's confirmation.
pub const LANDED_BY_ME: &str = "Mark landed, it was me";
pub const LANDED_NOT_ME: &str = "Mark landed, not me";
pub const KEEP_OPEN: &str = "Keep open";
pub const SUPERSEDED: &str = "Superseded";

impl Reconciler {
    pub(crate) fn approvals(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let projects: Vec<Project> = list(self.conn(), "retired_at IS NULL ORDER BY id", &[])?;
        for project in &projects {
            if let Err(error) = self.project_prs(project, now, report) {
                report.error(&format!("PRs of {}", project.id), &error);
            }
            if let Err(error) = self.default_branch(project, now, report) {
                report.error(&format!("default branch of {}", project.id), &error);
            }
        }
        // The detector runs even when an answer fails to apply: it raises
        // the rollback trigger, so nothing else may hold it back.
        let answers = self.outside_merge_answers(now, report);
        let raised = self.store.raise_wrong_principal_decisions(&self.ctx(now))?;
        for found in &raised.value {
            report.repair(format!(
                "G04 approval {} decided by {}: {}: unauthorized-action trigger",
                found.approval_id, found.actor, found.why
            ));
        }
        answers
    }

    /// Every PR of the project that still matters: open, or carrying an
    /// approval whose outcome is unknown.
    fn project_prs(
        &mut self,
        project: &Project,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let Some(repo) = project.repo.clone() else {
            return Ok(());
        };
        let prs: Vec<Pr> = list(
            self.conn(),
            "project_id = ?1 AND (state = 'open'
               OR EXISTS (SELECT 1 FROM approvals a WHERE a.pr_id = prs.id AND a.state = 'uncertain'))
             ORDER BY number",
            &[&project.id],
        )?;
        for pr in prs {
            let read = self.sources.forge.pr(&repo, pr.number);
            let fetch = match &read {
                Ok(read) => read.fetch.clone(),
                Err(error) => PrFetch {
                    state: pr.state,
                    head_sha: pr.head_sha.clone(),
                    base_ref: pr.base_ref.clone(),
                    checks: pr.checks,
                    review_decision: pr.review_decision,
                    mergeable: pr.mergeable,
                    merged_at: pr.merged_at,
                    merge_sha: pr.merge_sha.clone(),
                    pipeline_state: pr.pipeline_state,
                    fetch_error: Some(error.clone()),
                },
            };
            // Uncertain executions first, from the same read.
            if let Ok(read) = &read {
                self.resolve_uncertain_merges(&pr, read, project, now, report)?;
            } else {
                self.uncertain_unreadable(&pr, now, report)?;
            }
            let refreshed = self.store.pr_refresh(&self.ctx(now), &pr.id, fetch)?.value;
            let Ok(read) = read else {
                continue;
            };
            self.scrub(&pr, &refreshed, &read, project, now, report)?;
            // Both transitions are read from the records as they stand, not
            // from this pass's memory of the row before the refresh: a pass
            // that dies between the refresh and the repair, or a forge read
            // that fails once, loses nothing. Every repair is keyed.
            if refreshed.state != PrState::Open {
                self.outside_merge(&refreshed, &read, project, now, report)?;
            }
            if refreshed.state == PrState::Open
                && let Some(new) = refreshed.head_sha.as_deref()
                && let Some(moved) = self.moved_from(&refreshed, new)?
            {
                self.head_moved(&pr, &refreshed, &read, &moved, project, new, now, report)?;
            }
        }
        Ok(())
    }

    /// The approvals a head move left behind: the PR's newest `stale`
    /// approvals bound to another head, when no approval (open, executing or
    /// finished) sits at the current head. Empty when nothing is owed.
    fn moved_from(&self, pr: &Pr, head: &str) -> Result<Option<Vec<Approval>>> {
        let at_head: bool = self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM approvals WHERE pr_id = ?1 AND head_sha = ?2 AND state != 'stale')",
            rusqlite::params![pr.id, head],
            |row| row.get(0),
        )?;
        if at_head {
            return Ok(None);
        }
        let stale: Vec<Approval> = list(
            self.conn(),
            "pr_id = ?1 AND state = 'stale' AND head_sha IS NOT NULL AND head_sha != ?2 ORDER BY requested_at DESC, id DESC",
            &[&pr.id, &head],
        )?;
        let Some(newest) = stale.first() else {
            return Ok(None);
        };
        // Only the approvals bound to the head the move came from.
        let from = newest.head_sha.clone();
        Ok(Some(
            stale.into_iter().filter(|a| a.head_sha == from).collect(),
        ))
    }

    /// R08: an `uncertain` merge is `succeeded` when the forge shows the PR
    /// merged at the bound head, else `failed` with the forge's answer.
    fn resolve_uncertain_merges(
        &mut self,
        pr: &Pr,
        read: &PrRead,
        _project: &Project,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let uncertain: Vec<Approval> = list(
            self.conn(),
            "pr_id = ?1 AND state = 'uncertain' AND kind = 'merge' ORDER BY id",
            &[&pr.id],
        )?;
        for approval in uncertain {
            let merged_at_bound = read.fetch.state == PrState::Merged
                && read.fetch.head_sha.as_deref() == approval.head_sha.as_deref();
            let (finish, result) = if merged_at_bound {
                (
                    Finish::Succeeded,
                    json!({"sha": read.fetch.merge_sha.clone().or(approval.head_sha.clone()), "resolved_by": "reconcile"}),
                )
            } else {
                (
                    Finish::Failed,
                    json!({"error": format!("the forge shows PR #{} {} at head {}", pr.number, read.fetch.state, read.fetch.head_sha.as_deref().unwrap_or("unknown")), "resolved_by": "reconcile"}),
                )
            };
            let finished =
                self.store
                    .approval_finish(&self.ctx(now), &approval.id, finish, result)?;
            report.repair(format!(
                "R08 uncertain approval {} resolved from the forge: {}",
                approval.id, finished.value.approval.state
            ));
            if finish == Finish::Failed {
                self.failed_execution_item(&approval, "the merge did not happen", now, report)?;
            }
        }
        Ok(())
    }

    /// An `uncertain` execution the forge would not answer for ten minutes
    /// goes to the developer, and stays `uncertain`.
    fn uncertain_unreadable(&mut self, pr: &Pr, now: Timestamp, report: &mut Report) -> Result<()> {
        let uncertain: Vec<Approval> = list(
            self.conn(),
            "pr_id = ?1 AND state = 'uncertain' ORDER BY id",
            &[&pr.id],
        )?;
        for approval in uncertain {
            let Some(executed_at) = approval.executed_at else {
                continue;
            };
            if executed_at.plus_minutes(self.config.thresholds.uncertain_merge) <= now {
                self.failed_execution_item(
                    &approval,
                    "the forge has been unreadable since it executed",
                    now,
                    report,
                )?;
            }
        }
        Ok(())
    }

    fn failed_execution_item(
        &mut self,
        approval: &Approval,
        why: &str,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let Some(card_id) = approval.card_id.clone() else {
            return Ok(());
        };
        let card: Card = get(self.conn(), "card", &card_id)?;
        let key = format!("reconcile:R08:execution:{}", approval.id);
        if self.recovery(
            now,
            &key,
            NewAttention {
                card: card.id.clone(),
                run_id: approval.run_id.clone(),
                question: format!(
                    "The {} of {} at {} is {}: {why}. Approve again at the current head, or stop?",
                    approval.action,
                    card.display_id,
                    approval.head_sha.as_deref().unwrap_or("unknown"),
                    approval.state
                ),
                evidence: Self::json_text(json!({"approval_id": approval.id, "state": approval.state, "result": approval.result})),
                options: Some(json!(["Approve again", "Stop"])),
                ..NewAttention::default()
            },
        )? {
            report.repair(format!("R08 approval {} {}: recovery item on {}", approval.id, approval.state, card.display_id));
        }
        Ok(())
    }

    /// R19 (G30): a body that fails the scrub is a breach once per head,
    /// a note with the lines, and on a public repo the developer at once.
    /// While the newest read fails, this pass matches no grant and requests
    /// no approval for the PR.
    fn scrub(
        &mut self,
        pr: &Pr,
        refreshed: &Pr,
        read: &PrRead,
        project: &Project,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        if read.body_leaks.is_empty() || refreshed.state != PrState::Open {
            return Ok(());
        }
        let head = refreshed.head_sha.clone().unwrap_or_default();
        let card: Card = get(self.conn(), "card", &pr.card_id)?;
        let breach_key = format!("reconcile:G30:{}:{head}", pr.id);
        if !self.key_used(&breach_key)? {
            self.store.guard_breach(
                &self.ctx(now).with_key(breach_key),
                "G30",
                Some(&card.id),
                json!({"pr_id": pr.id, "number": pr.number, "head_sha": head, "lines": read.body_leaks, "repo_public": read.repo_public}),
            )?;
            report.repair(format!(
                "R19 PR #{} body fails the scrub at {head}: G30 breach",
                pr.number
            ));
        }
        let note_key = format!("reconcile:R19:scrub-note:{}:{head}", pr.id);
        self.note(
            now,
            &project.id,
            &note_key,
            format!(
                "[{}] PR #{} body fails the scrub; the merge is blocked until it is fixed through gh-axi. Lines:\n{}",
                card.display_id,
                pr.number,
                read.body_leaks.join("\n")
            ),
            Some(card.id.clone()),
        )?;
        if read.repo_public == Some(true) {
            let key = format!("reconcile:R19:leak:{}:{head}", pr.id);
            if self.recovery(
                now,
                &key,
                NewAttention {
                    card: card.id.clone(),
                    run_id: pr.run_id.clone(),
                    question: format!(
                        "PR #{} on public repo {} has a body that leaks ({} lines); edit history cannot be scrubbed. The merge stays blocked until you answer. Fixed it, or close the PR?",
                        pr.number,
                        project.repo.as_deref().unwrap_or(""),
                        read.body_leaks.len()
                    ),
                    evidence: Self::json_text(json!({"url": pr.url, "lines": read.body_leaks})),
                    consequence: Some("edit history on the forge keeps the leaked text".into()),
                    ..NewAttention::default()
                },
            )? {
                report.repair(format!("R19 PR #{} leaks on a public repo: recovery item on {}", pr.number, card.display_id));
            }
        }
        Ok(())
    }

    /// R07: a PR merged or closed with no execution of Tiller's own. The
    /// approvals went stale and the `merge` item was withdrawn inside
    /// `pr.refresh`; here the worktree is rescanned, the orchestrator is
    /// told, and the owner is asked whether it was them.
    fn outside_merge(
        &mut self,
        pr: &Pr,
        read: &PrRead,
        project: &Project,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let own_attempt: bool = self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM approvals WHERE pr_id = ?1 AND executed_at IS NOT NULL)",
            [&pr.id],
            |row| row.get(0),
        )?;
        if own_attempt {
            return Ok(());
        }
        let card: Card = get(self.conn(), "card", &pr.card_id)?;
        if let Some(run_id) = &pr.run_id
            && let Some(run) = find::<Run>(self.conn(), "id = ?1", &[run_id])?
            && let Some(path) = &run.worktree_path
        {
            self.scans.remove(Path::new(path));
            self.scan_of(Path::new(path), project);
        }
        let merged_by = read.merged_by.clone();
        let by_owner = match (&merged_by, &self.config.forge_login) {
            (Some(by), Some(login)) => Some(by == login),
            _ => None,
        };
        let last_head_landed = pr
            .run_id
            .as_ref()
            .and_then(|run| find::<Run>(self.conn(), "id = ?1", &[run]).ok().flatten())
            .and_then(|run| run.worktree_path)
            .and_then(|path| self.scans.get(Path::new(&path)).cloned())
            .is_some_and(|scan| scan.head_sha == pr.head_sha);
        if pr.state == PrState::Merged {
            let who = merged_by.clone().unwrap_or_else(|| "someone".into());
            let login_note = match by_owner {
                Some(true) => " (your forge login)",
                Some(false) => " (not your forge login)",
                None => "",
            };
            let gate = if project.merge_gate == tiller_core::domain::MergeGate::Enforced {
                " The project's merge gate is enforced, so the ruleset was bypassed or changed."
            } else {
                ""
            };
            let note_key = format!("reconcile:R07:note:{}", pr.id);
            self.note(
                now,
                &project.id,
                &note_key,
                format!(
                    "[{}] PR #{} was merged outside Tiller by {who}{login_note} at {} (merge {}). The owner decides whether the card closes; do not close it.",
                    card.display_id,
                    pr.number,
                    pr.merged_at.map(|t| t.to_string()).unwrap_or_else(|| "an unknown time".into()),
                    pr.merge_sha.as_deref().unwrap_or("unknown")
                ),
                Some(card.id.clone()),
            )?;
            let key = format!("reconcile:R07:{}", pr.id);
            if self.recovery(
                now,
                &key,
                NewAttention {
                    card: card.id.clone(),
                    run_id: pr.run_id.clone(),
                    question: format!(
                        "PR #{} was merged outside Tiller by {who}{login_note} at {} (merge {}).{gate} Was that you? Close {} as landed?",
                        pr.number,
                        pr.merged_at.map(|t| t.to_string()).unwrap_or_else(|| "an unknown time".into()),
                        pr.merge_sha.as_deref().unwrap_or("unknown"),
                        card.display_id
                    ),
                    evidence: Self::json_text(json!({"r07_pr": pr.id, "url": pr.url, "merged_by": merged_by, "merge_sha": pr.merge_sha, "by_forge_login": by_owner})),
                    recommendation: Some(if last_head_landed {
                        format!("{LANDED_BY_ME}: the merged head equals the run's last known head")
                    } else {
                        KEEP_OPEN.to_owned()
                    }),
                    options: Some(json!([LANDED_BY_ME, LANDED_NOT_ME, KEEP_OPEN, SUPERSEDED])),
                    ..NewAttention::default()
                },
            )? {
                report.repair(format!("R07 PR #{} merged outside Tiller: recovery item on {}", pr.number, card.display_id));
            }
        } else {
            let key = format!("reconcile:R07:closed:{}", pr.id);
            if self.recovery(
                now,
                &key,
                NewAttention {
                    card: card.id.clone(),
                    run_id: pr.run_id.clone(),
                    question: format!(
                        "PR #{} was closed without merging. Cancel {}, or relaunch?",
                        pr.number, card.display_id
                    ),
                    evidence: Self::json_text(json!({"url": pr.url, "r07_closed": pr.id})),
                    options: Some(json!(["Cancel", "Relaunch", KEEP_OPEN])),
                    ..NewAttention::default()
                },
            )? {
                report.repair(format!(
                    "R07 PR #{} closed without merging: recovery item on {}",
                    pr.number, card.display_id
                ));
            }
        }
        let _ = read;
        Ok(())
    }

    /// R08 and R19 on a moved head: a force-push (the new head does not
    /// descend from the old) is a breach and the developer's at once, never
    /// rematched. A fast-forward the run itself pushed is rematched against
    /// the standing grants; anything else asks the owner again.
    #[allow(clippy::too_many_arguments)]
    fn head_moved(
        &mut self,
        pr: &Pr,
        refreshed: &Pr,
        read: &PrRead,
        open_before: &[Approval],
        project: &Project,
        new: &str,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        let Some(old) = open_before.first().and_then(|a| a.head_sha.as_deref()) else {
            return Ok(());
        };
        let card: Card = get(self.conn(), "card", &pr.card_id)?;
        let descends = self.sources.forge.is_ancestor(project, old, new);
        match descends {
            Ok(false) => {
                let breach_key = format!("reconcile:G11:{}:{new}", pr.id);
                if !self.key_used(&breach_key)? {
                    self.store.guard_breach(
                        &self.ctx(now).with_key(breach_key),
                        "G11",
                        Some(&card.id),
                        json!({"pr_id": pr.id, "number": pr.number, "from": old, "to": new, "approvals_staled": open_before.iter().map(|a| a.id.clone()).collect::<Vec<_>>()}),
                    )?;
                    report.repair(format!(
                        "R19 PR #{} force-pushed from {old} to {new}: G11 breach",
                        pr.number
                    ));
                }
                let key = format!("reconcile:R19:force:{}:{new}", pr.id);
                if self.recovery(
                    now,
                    &key,
                    NewAttention {
                        card: card.id.clone(),
                        run_id: pr.run_id.clone(),
                        question: format!(
                            "PR #{} was force-pushed: {new} does not descend from {old}. {} approval(s) went stale and will not be rematched. Approve the new head by hand, or stop?",
                            pr.number,
                            open_before.len()
                        ),
                        evidence: Self::json_text(json!({"url": pr.url, "from": old, "to": new})),
                        options: Some(json!(["Approve again", "Stop"])),
                        ..NewAttention::default()
                    },
                )? {
                    report.repair(format!("R19 PR #{} force-push: recovery item on {}", pr.number, card.display_id));
                }
                return Ok(());
            }
            Err(error) => {
                report.error(&format!("ancestry of PR #{}", pr.number), &error);
                return Ok(());
            }
            Ok(true) => {}
        }
        if open_before.is_empty() || !read.body_leaks.is_empty() {
            return Ok(());
        }
        let Some(stale) = open_before.first() else {
            return Ok(());
        };
        let run: Option<Run> = match &pr.run_id {
            Some(id) => find(self.conn(), "id = ?1", &[id])?,
            None => None,
        };
        let pushed_by_run = run
            .as_ref()
            .and_then(|run| run.worktree_path.clone())
            .map(|path| {
                self.scans.remove(Path::new(&path));
                self.scan_of(Path::new(&path), project)
            })
            .is_some_and(|scan| scan.head_sha.as_deref() == Some(new));
        let grant = if pushed_by_run {
            grants::matching(
                self.conn(),
                &project.id,
                GrantKind::Merge,
                refreshed,
                Some(read),
                now,
            )?
        } else {
            None
        };
        let (grant_id, action) = match &grant {
            Some((grant, action)) => (Some(grant.id.clone()), *action),
            None => (None, stale.action),
        };
        let approval = self.store.approval_request(
            &self.ctx(now),
            NewApproval {
                kind: ApprovalKind::Merge,
                card: card.id.clone(),
                run_id: pr.run_id.clone(),
                pr_id: Some(pr.id.clone()),
                branch: None,
                head_sha: new.to_owned(),
                action,
                memory_paths: stale
                    .memory_paths
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_str().map(str::to_owned))
                            .collect()
                    }),
                grant_id,
            },
        )?;
        let how = if grant.is_some() {
            "rematched to a standing grant (accepted)"
        } else if pushed_by_run {
            "no grant matched: requested again"
        } else {
            "head not pushed by the run: requested again"
        };
        report.repair(format!(
            "R08 PR #{} head moved {old} -> {new}: approval {} {how}",
            pr.number, approval.value.id
        ));
        Ok(())
    }

    /// R07 (direct projects) and R08 (`uncertain` `land_direct`): the
    /// default branch against the records. A run's branch head on the
    /// default branch with no `succeeded` `land_direct` is a landing outside
    /// `tiller land`: a G10 breach and the developer's at once.
    fn default_branch(
        &mut self,
        project: &Project,
        now: Timestamp,
        report: &mut Report,
    ) -> Result<()> {
        if !matches!(project.delivery, Delivery::DirectPush | Delivery::LocalOnly) {
            return Ok(());
        }
        let uncertain: Vec<Approval> = list(
            self.conn(),
            "project_id = ?1 AND kind = 'land_direct' AND state = 'uncertain' ORDER BY id",
            &[&project.id],
        )?;
        let runs: Vec<Run> = list(
            self.conn(),
            "ended_at IS NULL AND card_id IN (SELECT id FROM cards WHERE project_id = ?1) AND branch IS NOT NULL ORDER BY started_at",
            &[&project.id],
        )?;
        if uncertain.is_empty() && runs.is_empty() {
            return Ok(());
        }
        let head = match self.sources.forge.default_branch_head(project) {
            Ok(head) => head,
            Err(error) => {
                for approval in &uncertain {
                    if approval.executed_at.is_some_and(|at| {
                        at.plus_minutes(self.config.thresholds.uncertain_merge) <= now
                    }) {
                        self.failed_execution_item(
                            approval,
                            "the remote has been unreadable since it executed",
                            now,
                            report,
                        )?;
                    }
                }
                tracing::debug!(%error, project = %project.id, "default branch head unreadable");
                return Ok(());
            }
        };
        for approval in uncertain {
            let Some(bound) = approval.head_sha.clone() else {
                continue;
            };
            let landed = self.sources.forge.is_ancestor(project, &bound, &head);
            let (finish, result) = match landed {
                Ok(true) => (
                    Finish::Succeeded,
                    json!({"sha": bound, "default_head": head, "resolved_by": "reconcile"}),
                ),
                Ok(false) => (
                    Finish::Failed,
                    json!({"error": format!("{bound} is not on {} ({head})", project.default_branch), "resolved_by": "reconcile"}),
                ),
                Err(error) => {
                    report.error(&format!("ancestry on {}", project.id), &error);
                    continue;
                }
            };
            self.store
                .approval_finish(&self.ctx(now), &approval.id, finish, result)?;
            report.repair(format!(
                "R08 uncertain land_direct {} resolved from the default branch: {finish:?}",
                approval.id
            ));
            if finish == Finish::Failed {
                self.failed_execution_item(&approval, "the push did not land", now, report)?;
            }
        }
        for run in runs {
            let Some(path) = run.worktree_path.clone() else {
                continue;
            };
            let scan = self.scan_of(Path::new(&path), project);
            let Some(run_head) = scan.head_sha.clone() else {
                continue;
            };
            // A worktree still at its base has landed nothing, and one at the
            // default head with no known base is a fresh checkout, not a
            // landing: only a head the run moved past its base counts.
            if run.base_sha.as_deref() == Some(run_head.as_str()) {
                continue;
            }
            if run_head == head && run.base_sha.is_none() {
                continue;
            }
            if run_head != head
                && !self
                    .sources
                    .forge
                    .is_ancestor(project, &run_head, &head)
                    .unwrap_or(false)
            {
                continue;
            }
            let covered: bool = self.conn().query_row(
                "SELECT EXISTS (SELECT 1 FROM approvals WHERE card_id = ?1 AND kind = 'land_direct' AND state IN ('succeeded', 'executing', 'uncertain'))",
                [&run.card_id],
                |row| row.get(0),
            )?;
            if covered {
                continue;
            }
            let card: Card = get(self.conn(), "card", &run.card_id)?;
            let breach_key = format!("reconcile:G10:{}:{run_head}", run.id);
            if !self.key_used(&breach_key)? {
                self.store.guard_breach(
                    &self.ctx(now).with_key(breach_key),
                    "G10",
                    Some(&card.id),
                    json!({"run_id": run.id, "branch": run.branch, "head": run_head, "default_head": head}),
                )?;
                report.repair(format!(
                    "R19 run {} landed on {} without tiller land: G10 breach",
                    run.id, project.default_branch
                ));
            }
            let key = format!("reconcile:R07:direct:{}:{run_head}", run.id);
            if self.recovery(
                now,
                &key,
                NewAttention {
                    card: card.id.clone(),
                    run_id: Some(run.id.clone()),
                    question: format!(
                        "{} {} run {}: its head {run_head} is on {} ({head}) with no land_direct approval. Was that you? Close {} as landed?",
                        card.display_id,
                        run.role.short_name(),
                        run.id,
                        project.default_branch,
                        card.display_id
                    ),
                    evidence: Self::json_text(json!({"r07_direct_run": run.id, "head": run_head, "default_head": head})),
                    options: Some(json!([LANDED_BY_ME, LANDED_NOT_ME, KEEP_OPEN])),
                    ..NewAttention::default()
                },
            )? {
                report.repair(format!("R07 run {} landed outside tiller land: recovery item on {}", run.id, card.display_id));
            }
        }
        Ok(())
    }

    /// The owner's answers to R07: either landed answer ends the card's
    /// active runs `landed` and closes the card `landed` (`closed_by =
    /// daemon`, the answer as evidence); "not me" is also a breach and the
    /// unauthorized-action trigger; "superseded" closes the card so.
    fn outside_merge_answers(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let answered: Vec<tiller_core::domain::Attention> = list(
            self.conn(),
            "kind = 'recovery' AND answered_at IS NOT NULL AND json_valid(evidence)
             AND (json_extract(evidence, '$.r07_pr') IS NOT NULL OR json_extract(evidence, '$.r07_direct_run') IS NOT NULL)
             ORDER BY answered_at",
            &[],
        )?;
        for item in answered {
            let card: Card = get(self.conn(), "card", &item.card_id)?;
            if card.state != CardState::Open {
                continue;
            }
            let answer = item.answer.clone().unwrap_or_default();
            let lower = answer.trim().to_lowercase();
            let landed = lower.starts_with(&MARK_LANDED.to_lowercase());
            let evidence: Value = serde_json::from_str(item.evidence.as_deref().unwrap_or("{}"))
                .unwrap_or(Value::Null);
            if landed && lower.contains("not me") {
                let breach_key = format!("reconcile:R07:not-me:{}", item.id);
                if !self.key_used(&breach_key)? {
                    self.store.guard_breach(
                        &self.ctx(now).with_key(breach_key),
                        "G10",
                        Some(&card.id),
                        json!({"attention_id": item.id, "answer": answer, "evidence": evidence}),
                    )?;
                    report.repair(format!("R07 {}: the owner says the merge was not theirs: unauthorized-action trigger", card.display_id));
                }
            }
            let outcome = if landed {
                CardOutcome::Landed
            } else if lower == SUPERSEDED.to_lowercase() {
                CardOutcome::Superseded
            } else {
                continue;
            };
            // Another open item holds the close: the answer waits for it,
            // and no run is ended ahead of a close that would be refused.
            let other_open: i64 = self.conn().query_row(
                "SELECT count(*) FROM attention WHERE card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL",
                [&card.id],
                |row| row.get(0),
            )?;
            if other_open > 0 {
                continue;
            }
            let runs: Vec<Run> = list(
                self.conn(),
                "card_id = ?1 AND ended_at IS NULL ORDER BY started_at",
                &[&card.id],
            )?;
            for run in runs {
                let run_outcome = if landed {
                    RunOutcome::Landed
                } else {
                    RunOutcome::Abandoned
                };
                self.store.run_end(
                    &self.ctx(now),
                    &run.id,
                    EndRun {
                        outcome: run_outcome,
                        note: Some(format!(
                            "the owner answered \"{answer}\" to the outside-merge question"
                        )),
                    },
                )?;
            }
            // Other open items on the card hold the close; this one is answered.
            let closed = self.store.card_close(
                &self.ctx(now),
                &card.id,
                outcome,
                Some(&format!("the owner answered \"{answer}\"")),
                Some(&format!("attention {}", item.id)),
            );
            match closed {
                Ok(_) => report.repair(format!(
                    "R07 {} closed {outcome} on the owner's answer \"{answer}\"",
                    card.display_id
                )),
                Err(error) => report.error(
                    &format!("close {} on the R07 answer", card.display_id),
                    &error,
                ),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landed_answers_start_with_the_marker_the_core_accepts() {
        assert!(
            LANDED_BY_ME
                .to_lowercase()
                .starts_with(&MARK_LANDED.to_lowercase())
        );
        assert!(
            LANDED_NOT_ME
                .to_lowercase()
                .starts_with(&MARK_LANDED.to_lowercase())
        );
    }
}
