//! The return brief: what happened while the owner was away (P34).
//!
//! Read from the records for the window between `away.start` and `away.end`
//! (or now, while the contract still runs), so it is the same whether the
//! owner asks the moment they return or a week later. It states what the
//! contract allowed, what it spent, what shipped, and what waited for the
//! owner. Away mode never answers attention items or decides approvals, so
//! the waiting lists are the owner's whole backlog.

use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use tiller_core::domain::{
    Approval, ApprovalAction, ApprovalKind, ApprovalState, Attention, Card, CardOutcome, Pr, Run,
    RunOutcome, RunRole,
};
use tiller_core::state::approvals::AwayContract;
use tiller_core::state::{find, get, list, resolve_id};
use tiller_core::{Error, Result, Timestamp};

use super::resume::{CardLine, DecisionLine, card_line, decision_line};

/// What the owner reads on returning from away mode.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReturnBrief {
    pub approval_id: String,
    /// `true` while the contract still binds; `false` once it ended.
    pub away: bool,
    pub started_at: Timestamp,
    pub until_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    /// Why it ended: the owner's word, or that the window passed.
    pub ended_reason: Option<String>,
    pub orders: Option<String>,
    pub projects: Vec<String>,
    pub spend: Spend,
    /// Every run that started in the window.
    pub runs: Vec<RunLine>,
    /// Cards that landed in the window.
    pub landed: Vec<CardLine>,
    /// Merges and direct landings a standing grant made in the window.
    pub merged_by_grant: Vec<MergeLine>,
    /// Attention items still waiting for the owner.
    pub waiting_decisions: Vec<DecisionLine>,
    /// Approvals still waiting for the owner's word.
    pub waiting_approvals: Vec<WaitingApproval>,
}

/// What the cap allowed against what started.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Spend {
    pub max_new_runs: Option<i64>,
    pub new_runs: i64,
    pub quota_floor_percent: Option<f64>,
    /// `true` when the run cap was reached: more work was refused.
    pub cap_reached: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunLine {
    pub run_id: String,
    pub card: String,
    pub role: RunRole,
    pub started_at: Timestamp,
    pub outcome: Option<RunOutcome>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MergeLine {
    pub card: String,
    pub action: ApprovalAction,
    pub pr_url: Option<String>,
    pub grant_id: Option<String>,
    pub state: ApprovalState,
    pub at: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WaitingApproval {
    pub approval_id: String,
    pub card: Option<String>,
    pub action: ApprovalAction,
    pub requested_at: Timestamp,
}

impl ReturnBrief {
    /// The brief for the away contract `approval` (an id or short id), or
    /// for the newest contract when `None`. `Ok(None)` when the owner has
    /// never been away.
    pub fn load(conn: &Connection, approval: Option<&str>, now: Timestamp) -> Result<Option<Self>> {
        let away: Approval = match approval {
            Some(id) => {
                let full = resolve_id(conn, "approvals", "approval", id)?;
                let row = get::<Approval>(conn, "approval", &full)?;
                if row.kind != ApprovalKind::Away {
                    return Err(Error::Usage(format!(
                        "approval {id} is not an away contract"
                    )));
                }
                row
            }
            None => match find::<Approval>(
                conn,
                "kind = 'away' ORDER BY requested_at DESC, id DESC",
                &[],
            )? {
                Some(row) => row,
                None => return Ok(None),
            },
        };
        let contract = AwayContract::from_approval(&away)?;
        let running = away.state == ApprovalState::Accepted;
        let ended_at = away.finished_at;
        let ended_reason = away
            .result
            .as_ref()
            .and_then(|result| result.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        // The window closes when the contract ended; while it runs, now.
        let end = ended_at.unwrap_or(now);
        let projects = serde_json::to_string(&contract.projects)?;

        let runs: Vec<Run> = list(
            conn,
            "started_at >= ?1 AND started_at <= ?2 ORDER BY started_at, id",
            &[&contract.started_at, &end],
        )?;
        let mut run_lines = Vec::new();
        for run in &runs {
            run_lines.push(RunLine {
                run_id: run.id.clone(),
                card: get::<Card>(conn, "card", &run.card_id)?.display_id,
                role: run.role,
                started_at: run.started_at,
                outcome: run.outcome,
                status: run.last_status_line.clone(),
            });
        }

        let landed = list::<Card>(
            conn,
            "outcome = ?3 AND closed_at >= ?1 AND closed_at <= ?2
             AND project_id IN (SELECT value FROM json_each(?4)) ORDER BY closed_at, id",
            &[&contract.started_at, &end, &CardOutcome::Landed, &projects],
        )?
        .iter()
        .map(card_line)
        .collect();

        let mut merged_by_grant = Vec::new();
        for approval in list::<Approval>(
            conn,
            "decided_by LIKE 'grant:%' AND kind IN ('merge', 'land_direct')
             AND requested_at >= ?1 AND requested_at <= ?2 ORDER BY requested_at, id",
            &[&contract.started_at, &end],
        )? {
            let pr_url = match &approval.pr_id {
                Some(id) => find::<Pr>(conn, "id = ?1", &[id])?.map(|pr| pr.url),
                None => None,
            };
            merged_by_grant.push(MergeLine {
                card: match &approval.card_id {
                    Some(id) => get::<Card>(conn, "card", id)?.display_id,
                    None => String::new(),
                },
                action: approval.action,
                pr_url,
                grant_id: approval.grant_id.clone(),
                state: approval.state,
                at: approval.requested_at,
            });
        }

        let mut waiting_decisions = Vec::new();
        for item in list::<Attention>(
            conn,
            "answered_at IS NULL AND withdrawn_at IS NULL
             AND card_id IN (SELECT id FROM cards WHERE project_id IN (SELECT value FROM json_each(?1)))
             ORDER BY opened_at, id",
            &[&projects],
        )? {
            waiting_decisions.push(decision_line(conn, &item)?);
        }

        let mut waiting_approvals = Vec::new();
        for approval in list::<Approval>(
            conn,
            "state = 'requested' AND kind IN ('merge', 'land_direct', 'memory')
             AND project_id IN (SELECT value FROM json_each(?1)) ORDER BY requested_at, id",
            &[&projects],
        )? {
            waiting_approvals.push(WaitingApproval {
                approval_id: approval.id.clone(),
                card: match &approval.card_id {
                    Some(id) => Some(get::<Card>(conn, "card", id)?.display_id),
                    None => None,
                },
                action: approval.action,
                requested_at: approval.requested_at,
            });
        }

        let new_runs = i64::try_from(runs.len()).unwrap_or(i64::MAX);
        Ok(Some(Self {
            approval_id: away.id,
            away: running && contract.until_at > now,
            started_at: contract.started_at,
            until_at: contract.until_at,
            ended_at,
            ended_reason,
            orders: contract.orders,
            projects: contract.projects,
            spend: Spend {
                max_new_runs: contract.max_new_runs,
                new_runs,
                quota_floor_percent: contract.quota_floor_percent,
                cap_reached: contract.max_new_runs.is_some_and(|cap| new_runs >= cap),
            },
            runs: run_lines,
            landed,
            merged_by_grant,
            waiting_decisions,
            waiting_approvals,
        }))
    }
}
