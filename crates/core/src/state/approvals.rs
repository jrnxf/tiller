//! Pull requests, approvals, grants, away mode and the deploy watch:
//! `pr.*`, `approval.*`, `grant.*`, `away.*`, `deploy.record` (`state.md`,
//! Pull request, Approval and section 7; `authority.md` section 6).

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::attention::{NewAttention, answer_attention, open_attention, withdraw_attention};
use super::cards::{MARK_LANDED, REOPEN_WORK, active_runs, close_card, open_attention_id};
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::projects::{require_daemon, require_owner};
use super::records::{NewObligation, meet_obligation, open_obligation};
use super::runs::{EndRun, end_run};
use super::{find, get, list, refuse, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// A PR to link to a card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewPr {
    pub card: String,
    pub run_id: Option<String>,
    pub number: i64,
    pub url: String,
    pub head_sha: Option<String>,
    pub base_ref: Option<String>,
}

/// One `wake` to the project's live orchestrator for a PR event that is the
/// orchestrator's to act on: checks turning green or red, or a review decision
/// landing, while the PR is open. A merge or close outside Tiller is reconcile's
/// note (R07). It runs only on a change, so a transition wakes once; with no live
/// orchestrator nothing is enqueued, and the next session reads the PR itself.
fn wake_for_pr_event(tx: &mut Tx<'_>, old: &Pr, new: &Pr) -> Result<()> {
    if new.state != PrState::Open {
        return Ok(());
    }
    let mut events = Vec::new();
    if old.checks != new.checks {
        match new.checks {
            Checks::Success => events.push("checks passed".to_string()),
            Checks::Failure => events.push("checks failed".to_string()),
            _ => {}
        }
    }
    if old.review_decision != new.review_decision {
        match new.review_decision {
            ReviewDecision::Approved => events.push("review approved".to_string()),
            ReviewDecision::ChangesRequested => events.push("review requested changes".to_string()),
            ReviewDecision::None => {}
        }
    }
    if events.is_empty() {
        return Ok(());
    }
    let card: Card = get(tx.conn(), "card", &new.card_id)?;
    let Some(orchestrator) = live_orchestrator(tx.conn(), &card.project_id)? else {
        return Ok(());
    };
    enqueue(
        tx,
        &NewMessage {
            target_session_id: orchestrator.id,
            kind: OutboxKind::Wake,
            body: format!(
                "[{}] PR #{}: {}. {}",
                card.display_id,
                new.number,
                events.join(", "),
                new.url
            ),
            card_id: Some(card.id.clone()),
            reply_to: None,
        },
    )?;
    Ok(())
}

/// What the forge poller read for one PR.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrFetch {
    pub state: PrState,
    pub head_sha: Option<String>,
    pub base_ref: Option<String>,
    pub checks: Checks,
    pub review_decision: ReviewDecision,
    pub mergeable: Mergeable,
    pub merged_at: Option<Timestamp>,
    pub merge_sha: Option<String>,
    pub pipeline_state: PipelineState,
    pub fetch_error: Option<String>,
}

/// A merge or land-direct approval to request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewApproval {
    pub kind: ApprovalKind,
    pub card: String,
    pub run_id: Option<String>,
    /// `merge`: the PR id.
    pub pr_id: Option<String>,
    /// `land_direct`: the branch to push.
    pub branch: Option<String>,
    pub head_sha: String,
    pub action: ApprovalAction,
    /// The source-map paths the diff touches, if any (`memory.md` section 3).
    pub memory_paths: Option<Vec<String>>,
    /// A standing grant the daemon matched; the approval is then `accepted`.
    pub grant_id: Option<String>,
}

/// One memory source a merge's diff touches, bound for "Approve memory
/// change and merge".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryBinding {
    pub path: String,
    pub diff_sha256: String,
    pub base_rev: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub accept: bool,
    pub via: AnsweredVia,
    /// Required to accept an approval with `memory_paths`, one per source.
    pub memory: Vec<MemoryBinding>,
    /// The hook-recorded ask a chat decision cites (G06), recorded in the
    /// `approval.decided` event.
    pub ask_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Decided {
    pub approval: Approval,
    pub memory_approvals: Vec<Approval>,
}

/// What `approval.execute` found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "approval", rename_all = "snake_case")]
pub enum Execution {
    /// The head matched: the daemon may run the action.
    Executing(Approval),
    /// The head moved or the card closed: the approval is `stale`. On a moved
    /// head a new `requested` approval at the forge head asks again.
    Stale(Approval),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finished {
    pub approval: Approval,
    pub ended_runs: Vec<String>,
    pub closed_card: Option<Card>,
    pub deploy_watch: Option<Obligation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewGrant {
    pub project_id: String,
    pub kind: GrantKind,
    pub conditions: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeployRecorded {
    pub obligation: Obligation,
    pub closed_card: Option<Card>,
    pub attention: Option<Attention>,
}

fn load(tx: &Tx<'_>, approval: &str) -> Result<Approval> {
    let id = resolve_id(tx.conn(), "approvals", "approval", approval)?;
    get(tx.conn(), "approval", &id)
}

fn set_state(
    tx: &mut Tx<'_>,
    approval: &Approval,
    to: ApprovalState,
    kind: &'static str,
    payload: Value,
) -> Result<Approval> {
    tx.conn().execute(
        "UPDATE approvals SET state = ?2, rev = rev + 1 WHERE id = ?1",
        params![approval.id, to],
    )?;
    tx.event(
        NewEvent::new("approval", &approval.id, kind)
            .rev(approval.rev + 1)
            .card_opt(approval.card_id.clone())
            .payload(payload),
    )?;
    get(tx.conn(), "approval", &approval.id)
}

/// The open `merge` attention item that asks about `approval`.
fn approval_item(conn: &Connection, approval: &Approval) -> Result<Option<Attention>> {
    find(
        conn,
        "kind = 'merge' AND answered_at IS NULL AND withdrawn_at IS NULL
         AND json_valid(evidence) AND json_extract(evidence, '$.approval_id') = ?1",
        &[&approval.id],
    )
}

/// Marks an approval `stale`, with the memory approvals accepted with it,
/// and withdraws the question it was waiting on.
pub(crate) fn mark_stale(tx: &mut Tx<'_>, approval: &Approval, why: &str) -> Result<Approval> {
    approval
        .state
        .mark_stale()
        .map_err(refuse("approval", &approval.id))?;
    let stale = set_state(
        tx,
        approval,
        ApprovalState::Stale,
        "approval.stale",
        json!({"reason": why, "was": approval.state}),
    )?;
    let linked: Vec<Approval> = list(
        tx.conn(),
        "linked_approval_id = ?1 AND state IN ('requested', 'accepted') ORDER BY id",
        &[&approval.id],
    )?;
    for memory in linked {
        set_state(
            tx,
            &memory,
            ApprovalState::Stale,
            "approval.stale",
            json!({"reason": why, "linked_to": approval.id}),
        )?;
    }
    if let Some(item) = approval_item(tx.conn(), approval)? {
        withdraw_attention(tx, &item, &format!("approval went stale: {why}"))?;
    }
    Ok(stale)
}

fn ask_merge(
    tx: &mut Tx<'_>,
    approval: &Approval,
    card: &Card,
    question: String,
) -> Result<Attention> {
    open_attention(
        tx,
        &NewAttention {
            card: card.id.clone(),
            kind: AttentionKind::Merge,
            question,
            evidence: Some(
                json!({"approval_id": approval.id, "head_sha": approval.head_sha}).to_string(),
            ),
            // Not tied to the asking run: the daemon publishes the PR after
            // the builder's run ends, and the ask follows the card, the PR
            // and the bound head. The approval going stale withdraws it.
            run_id: None,
            ..NewAttention::default()
        },
    )
}

/// Asks the owner again for an approval that went stale because the head
/// moved: a new `requested` approval with the same binding at `head`, and its
/// `merge` item. The old item pointed at the stale approval, so answering it
/// could never execute.
fn reapprove(tx: &mut Tx<'_>, stale: &Approval, card: &Card, head: &str) -> Result<Approval> {
    let head = check_sha(head)?;
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO approvals (id, kind, project_id, card_id, run_id, repo, pr_id, branch, head_sha, memory_paths,
                                action, state, requested_at, requested_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'requested', ?12, ?13)",
        params![
            id,
            stale.kind,
            stale.project_id,
            card.id,
            stale.run_id,
            stale.repo,
            stale.pr_id,
            stale.branch,
            head,
            stale.memory_paths,
            stale.action,
            tx.now(),
            tx.actor().to_string(),
        ],
    )?;
    tx.event(
        NewEvent::new("approval", &id, "approval.requested")
            .rev(1)
            .card(&card.id)
            .payload(json!({
                "kind": stale.kind,
                "head_sha": head,
                "action": stale.action,
                "memory_paths": stale.memory_paths,
                "supersedes": stale.id,
            })),
    )?;
    let approval: Approval = get(tx.conn(), "approval", &id)?;
    let question = format!(
        "The head of {} moved to {} after approval. Approve the new head?",
        card.display_id,
        short_sha(head)
    );
    ask_merge(tx, &approval, card, question)?;
    Ok(approval)
}

/// A commit sha as git prints it: hex, 4 to 64 characters.
fn check_sha(sha: &str) -> Result<&str> {
    if (4..=64).contains(&sha.len()) && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        Err(Error::Usage(format!("{sha:?} is not a commit sha")))
    }
}

/// The first 12 characters of a checked sha, for questions.
fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// A grant that may accept an approval now.
fn live_grant(
    conn: &Connection,
    grant_id: &str,
    project_id: &str,
    kind: ApprovalKind,
    action: ApprovalAction,
    now: Timestamp,
) -> Result<Grant> {
    let grant: Grant = get(conn, "grant", grant_id)?;
    let fail = |fact: String| Err(Error::refused("grant", grant_id, fact));
    if grant.revoked_at.is_some() {
        return fail("grant is revoked".into());
    }
    if grant.project_id != project_id || grant.kind.as_str() != kind.as_str() {
        return fail(format!("grant is for {} {}", grant.project_id, grant.kind));
    }
    if let Some(expires) = grant.conditions.get("expires_at").and_then(Value::as_str)
        && expires.parse::<Timestamp>().is_ok_and(|at| at <= now)
    {
        return fail(format!("grant expired at {expires}"));
    }
    let actions = grant.conditions.get("actions").and_then(Value::as_array);
    if !actions.is_some_and(|list| list.iter().any(|a| a.as_str() == Some(action.as_str()))) {
        return fail(format!("grant does not allow {action}"));
    }
    Ok(grant)
}

/// Ends the card's active runs `landed` and closes it when nothing else
/// holds it open; for a `land_direct` with a deploy workflow, opens the
/// deploy watch instead of the close.
fn land(
    tx: &mut Tx<'_>,
    approval: &Approval,
    card: &Card,
    sha: Option<&str>,
) -> Result<(Vec<String>, Option<Card>, Option<Obligation>)> {
    let mut ended = Vec::new();
    for run in active_runs(tx.conn(), &card.id)? {
        end_run(
            tx,
            &run,
            EndRun {
                outcome: RunOutcome::Landed,
                note: Some(format!("approval {} succeeded", approval.id)),
            },
        )?;
        ended.push(run.id);
    }
    let project: Project = get(tx.conn(), "project", &card.project_id)?;
    if approval.kind == ApprovalKind::LandDirect
        && let Some(_workflow) = &project.deploy_workflow
    {
        let watch = open_obligation(
            tx,
            &NewObligation {
                project_id: project.id.clone(),
                card_id: Some(card.id.clone()),
                what: format!(
                    "watch the deploy of {} to success",
                    sha.unwrap_or("the pushed commit")
                ),
                trigger_kind: TriggerKind::Event,
                trigger_event: Some(json!({
                    "kind": "deploy.finished",
                    "sha": sha,
                    "environment": project.deploy_environment,
                })),
                due_at: Some(tx.now().plus_minutes(project.deploy_timeout_minutes)),
            },
        )?;
        return Ok((ended, None, Some(watch)));
    }
    let card: Card = get(tx.conn(), "card", &card.id)?;
    if card.state == CardState::Open && open_attention_id(tx.conn(), &card.id)?.is_none() {
        let closed = close_card(
            tx,
            &card,
            CardOutcome::Landed,
            json!({"approval_id": approval.id}),
        )?;
        return Ok((ended, Some(closed.card), None));
    }
    Ok((ended, None, None))
}

impl Store {
    /// `pr.link` (`tiller pr link`, reconcile). Natural: the same PR linked to
    /// the same card is returned.
    pub fn pr_link(&mut self, ctx: &Ctx, new: NewPr) -> Result<Applied<Pr>> {
        required("url", &new.url)?;
        self.write(ctx, "pr.link", None, |tx| {
            let card = resolve_card(tx.conn(), &new.card)?;
            if let Some(existing) = find::<Pr>(
                tx.conn(),
                "project_id = ?1 AND number = ?2",
                &[&card.project_id, &new.number],
            )? {
                if existing.card_id == card.id {
                    return Ok(existing);
                }
                return Err(Error::refused("pr", &existing.url, format!("PR is linked to card {}", existing.card_id)));
            }
            let run_active = match &new.run_id {
                Some(run) => get::<Run>(tx.conn(), "run", run)?.ended_at.is_none(),
                None => false,
            };
            if !run_active && card.state != CardState::Open {
                return Err(Error::refused("card", &card.display_id, "card is closed and the run is not active"));
            }
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO prs (id, project_id, card_id, run_id, number, url, linked_at, linked_by, head_sha, base_ref)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id,
                    card.project_id,
                    card.id,
                    new.run_id,
                    new.number,
                    new.url,
                    tx.now(),
                    tx.actor().to_string(),
                    new.head_sha,
                    new.base_ref,
                ],
            )?;
            tx.event(
                NewEvent::new("pr", &id, "pr.linked")
                    .card(&card.id)
                    .payload(json!({"number": new.number, "url": new.url, "run_id": new.run_id})),
            )?;
            get(tx.conn(), "pr", &id)
        })
    }

    /// `pr.refresh` (daemon poller). Writes `pr.changed` only on a change, and
    /// stales every open approval on the PR when its head moved or it closed
    /// or merged outside Tiller.
    pub fn pr_refresh(&mut self, ctx: &Ctx, pr: &str, fetch: PrFetch) -> Result<Applied<Pr>> {
        require_daemon(&ctx.actor, "pr.refresh")?;
        self.write(ctx, "pr.refresh", None, |tx| {
            tx.rebuildable();
            let id = resolve_id(tx.conn(), "prs", "pr", pr)?;
            let old: Pr = get(tx.conn(), "pr", &id)?;
            tx.conn().execute(
                "UPDATE prs SET state = ?2, head_sha = ?3, base_ref = ?4, checks = ?5, review_decision = ?6,
                        mergeable = ?7, merged_at = ?8, merge_sha = ?9, pipeline_state = ?10, fetched_at = ?11,
                        fetch_error = ?12
                 WHERE id = ?1",
                params![
                    old.id,
                    fetch.state,
                    fetch.head_sha,
                    fetch.base_ref,
                    fetch.checks,
                    fetch.review_decision,
                    fetch.mergeable,
                    fetch.merged_at,
                    fetch.merge_sha,
                    fetch.pipeline_state,
                    tx.now(),
                    fetch.fetch_error,
                ],
            )?;
            let new: Pr = get(tx.conn(), "pr", &old.id)?;
            let mut changed = serde_json::Map::new();
            macro_rules! diff {
                ($($field:ident),+) => {$(
                    if old.$field != new.$field {
                        changed.insert(stringify!($field).into(), json!({"from": old.$field, "to": new.$field}));
                    }
                )+};
            }
            diff!(state, head_sha, base_ref, checks, review_decision, mergeable, merged_at, merge_sha, pipeline_state, fetch_error);
            if changed.is_empty() {
                return Ok(new);
            }
            tx.event(
                NewEvent::new("pr", &new.id, "pr.changed")
                    .card(&new.card_id)
                    .payload(Value::Object(changed)),
            )?;
            wake_for_pr_event(tx, &old, &new)?;
            // Compare each open approval with the fetched head, not with the
            // previous fetch: the head may have moved before the PR's first
            // recorded head, or between an approval and a missed fetch.
            let open: Vec<Approval> = list(
                tx.conn(),
                "pr_id = ?1 AND state IN ('requested', 'accepted') ORDER BY id",
                &[&new.id],
            )?;
            for approval in open {
                let why = match &new.head_sha {
                    _ if new.state != PrState::Open => format!("PR is {}", new.state),
                    Some(head) if approval.head_sha.as_ref() != Some(head) => format!("head moved to {head}"),
                    _ => continue,
                };
                mark_stale(tx, &approval, &why)?;
            }
            Ok(new)
        })
    }

    /// `approval.request`. Natural: an open approval with the same binding is
    /// returned. With a matched grant the approval is `accepted` at once;
    /// otherwise it is `requested` with a `merge` attention item.
    pub fn approval_request(&mut self, ctx: &Ctx, new: NewApproval) -> Result<Applied<Approval>> {
        check_sha(&new.head_sha)?;
        self.write(ctx, "approval.request", None, |tx| {
            let card = resolve_card(tx.conn(), &new.card)?;
            if card.state != CardState::Open {
                return Err(Error::refused("card", &card.display_id, "card is closed"));
            }
            let project: Project = get(tx.conn(), "project", &card.project_id)?;
            let Some(repo) = project.repo.clone() else {
                return Err(Error::refused("project", &project.id, "project has no repo to bind"));
            };
            match new.kind {
                ApprovalKind::Merge => {
                    let Some(pr_id) = &new.pr_id else {
                        return Err(Error::Usage("a merge approval binds a PR".into()));
                    };
                    let pr: Pr = get(tx.conn(), "pr", pr_id)?;
                    if pr.card_id != card.id || pr.state != PrState::Open {
                        return Err(Error::refused("pr", &pr.url, "PR is not open on this card"));
                    }
                    if pr.head_sha.as_deref() != Some(new.head_sha.as_str()) {
                        return Err(Error::refused(
                            "pr",
                            &pr.url,
                            format!("PR head is {}, not {}", pr.head_sha.as_deref().unwrap_or("unknown"), new.head_sha),
                        ));
                    }
                }
                ApprovalKind::LandDirect if new.branch.is_none() => {
                    return Err(Error::Usage("a land-direct approval binds a branch".into()));
                }
                ApprovalKind::LandDirect => {}
                other => return Err(Error::Usage(format!("{other} approvals are not requested here"))),
            }
            let same: Option<Approval> = find(
                tx.conn(),
                "kind = ?1 AND card_id = ?2 AND head_sha = ?3 AND pr_id IS ?4 AND branch IS ?5
                 AND state IN ('requested', 'accepted', 'executing')",
                &[&new.kind, &card.id, &new.head_sha, &new.pr_id, &new.branch],
            )?;
            if let Some(same) = same {
                return Ok(same);
            }
            let memory_paths = new.memory_paths.as_ref().filter(|paths| !paths.is_empty()).map(|paths| json!(paths));
            let grant = match &new.grant_id {
                Some(_) if memory_paths.is_some() => {
                    return Err(Error::refused("approval", &card.display_id, "a memory change is never accepted by a grant"));
                }
                Some(grant) => Some(live_grant(tx.conn(), grant, &project.id, new.kind, new.action, tx.now())?),
                None => None,
            };
            let id = ids::new_ulid();
            let (state, decided_by) = match &grant {
                Some(grant) => (ApprovalState::Accepted, Some(format!("grant:{}", grant.id))),
                None => (ApprovalState::Requested, None),
            };
            tx.conn().execute(
                "INSERT INTO approvals (id, kind, project_id, card_id, run_id, repo, pr_id, branch, head_sha, memory_paths,
                                        action, state, requested_at, requested_by, decided_at, decided_by, grant_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    id,
                    new.kind,
                    project.id,
                    card.id,
                    new.run_id,
                    repo,
                    new.pr_id,
                    new.branch,
                    new.head_sha,
                    memory_paths,
                    new.action,
                    state,
                    tx.now(),
                    tx.actor().to_string(),
                    grant.as_ref().map(|_| tx.now()),
                    decided_by,
                    grant.as_ref().map(|g| g.id.clone()),
                ],
            )?;
            let approval: Approval = get(tx.conn(), "approval", &id)?;
            tx.event(
                NewEvent::new("approval", &id, "approval.requested")
                    .rev(1)
                    .card(&card.id)
                    .payload(json!({"kind": new.kind, "head_sha": new.head_sha, "action": new.action, "memory_paths": memory_paths})),
            )?;
            match &grant {
                Some(grant) => {
                    tx.event(
                        NewEvent::new("approval", &id, "approval.decided")
                            .rev(1)
                            .card(&card.id)
                            .payload(json!({"accepted": true, "decided_by": format!("grant:{}", grant.id)})),
                    )?;
                }
                None => {
                    let what = match new.kind {
                        ApprovalKind::Merge => "Merge",
                        _ => "Push",
                    };
                    let question = format!(
                        "{what} {} at {} ({})?",
                        card.display_id,
                        short_sha(&new.head_sha),
                        new.action
                    );
                    ask_merge(tx, &approval, &card, question)?;
                }
            }
            Ok(approval)
        })
    }

    /// `approval.decide` (board, `tiller approve`). Owner only. Keyed: the key
    /// is the row's `decision_idempotency_key`, so a second click returns the
    /// first outcome. Refuses when the binding no longer matches the cache.
    pub fn approval_decide(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        approval: &str,
        decision: Decision,
    ) -> Result<Applied<Decided>> {
        self.decide_inner(ctx, key, approval, decision, None)
    }

    /// [`Store::approval_decide`] for a decision made on a shown head (the
    /// board): refused, in the same transaction, when the approval binds
    /// another head by now.
    pub fn approval_decide_shown(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        approval: &str,
        decision: Decision,
        shown_head: &str,
    ) -> Result<Applied<Decided>> {
        self.decide_inner(ctx, key, approval, decision, Some(shown_head))
    }

    fn decide_inner(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        approval: &str,
        decision: Decision,
        shown_head: Option<&str>,
    ) -> Result<Applied<Decided>> {
        require_owner(&ctx.actor, "approval.decide")?;
        self.write(ctx, "approval.decide", key, |tx| {
            let approval = load(tx, approval)?;
            if let Some(shown) = shown_head
                && approval.head_sha.as_deref().is_some_and(|head| head != shown)
            {
                return Err(Error::refused(
                    "approval",
                    &approval.id,
                    "the head moved since it was shown",
                ));
            }
            let to = approval.state.decide(decision.accept).map_err(refuse("approval", &approval.id))?;
            if let Some(pr_id) = &approval.pr_id {
                let pr: Pr = get(tx.conn(), "pr", pr_id)?;
                if pr.state != PrState::Open || pr.head_sha != approval.head_sha {
                    let why = format!(
                        "PR is {} at {}",
                        pr.state,
                        pr.head_sha.as_deref().unwrap_or("unknown")
                    );
                    return Err(Error::refused("approval", &approval.id, format!("binding no longer matches: {why}")));
                }
            }
            let paths: Vec<String> = approval
                .memory_paths
                .as_ref()
                .map(|paths| serde_json::from_value(paths.clone()))
                .transpose()?
                .unwrap_or_default();
            if paths.is_empty() && !decision.memory.is_empty() {
                return Err(Error::refused(
                    "approval",
                    &approval.id,
                    "approval touches no memory source, so it takes no memory binding",
                ));
            }
            if decision.accept && !paths.is_empty() {
                let mut bound: Vec<&str> = decision.memory.iter().map(|m| m.path.as_str()).collect();
                bound.sort_unstable();
                let mut wanted: Vec<&str> = paths.iter().map(String::as_str).collect();
                wanted.sort_unstable();
                if bound != wanted {
                    return Err(Error::refused(
                        "approval",
                        &approval.id,
                        format!("a memory change needs one binding per source: {}", wanted.join(", ")),
                    ));
                }
            }
            let by = tx.actor().to_string();
            tx.conn().execute(
                "UPDATE approvals SET state = ?2, decided_at = ?3, decided_by = 'owner', decision_idempotency_key = ?4,
                        rev = rev + 1
                 WHERE id = ?1",
                params![approval.id, to, tx.now(), tx.key()],
            )?;
            tx.event(
                NewEvent::new("approval", &approval.id, "approval.decided")
                    .rev(approval.rev + 1)
                    .card_opt(approval.card_id.clone())
                    .payload(json!({
                        "accepted": decision.accept,
                        "decided_by": "owner",
                        "via": by,
                        "ask_id": decision.ask_id,
                    })),
            )?;
            let mut memory_approvals = Vec::new();
            if decision.accept {
                for binding in &decision.memory {
                    let id = ids::new_ulid();
                    tx.conn().execute(
                        "INSERT INTO approvals (id, kind, project_id, card_id, run_id, repo, head_sha, diff_sha256, base_rev,
                                                linked_approval_id, action, state, requested_at, requested_by,
                                                decided_at, decided_by)
                         VALUES (?1, 'memory', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'apply', 'accepted', ?10, ?11, ?10, 'owner')",
                        params![
                            id,
                            approval.project_id,
                            approval.card_id,
                            approval.run_id,
                            approval.repo,
                            approval.head_sha,
                            binding.diff_sha256,
                            binding.base_rev,
                            approval.id,
                            tx.now(),
                            by,
                        ],
                    )?;
                    tx.event(
                        NewEvent::new("approval", &id, "approval.decided")
                            .rev(1)
                            .card_opt(approval.card_id.clone())
                            .payload(json!({"kind": "memory", "path": binding.path, "linked_approval_id": approval.id})),
                    )?;
                    memory_approvals.push(get(tx.conn(), "approval", &id)?);
                }
            }
            if let Some(item) = approval_item(tx.conn(), &approval)? {
                let words = if decision.accept { "approve" } else { "reject" };
                answer_attention(tx, &item, words, decision.via)?;
            }
            Ok(Decided {
                approval: get(tx.conn(), "approval", &approval.id)?,
                memory_approvals,
            })
        })
    }

    /// `approval.execute` (daemon only), with the head just re-read from the
    /// forge. A moved head marks the approval `stale` and asks again.
    pub fn approval_execute(
        &mut self,
        ctx: &Ctx,
        approval: &str,
        forge_head: &str,
    ) -> Result<Applied<Execution>> {
        require_daemon(&ctx.actor, "approval.execute")?;
        check_sha(forge_head)?;
        self.write(ctx, "approval.execute", None, |tx| {
            let approval = load(tx, approval)?;
            if approval.state == ApprovalState::Executing {
                return Ok(Execution::Executing(approval));
            }
            approval.state.execute().map_err(refuse("approval", &approval.id))?;
            let card: Option<Card> = approval
                .card_id
                .as_deref()
                .map(|id| get(tx.conn(), "card", id))
                .transpose()?;
            if let Some(card) = card.as_ref().filter(|card| card.state != CardState::Open) {
                let stale = mark_stale(tx, &approval, &format!("card {} is closed", card.display_id))?;
                return Ok(Execution::Stale(stale));
            }
            if approval.head_sha.as_deref() != Some(forge_head) {
                let stale = mark_stale(tx, &approval, &format!("head moved to {forge_head}"))?;
                if let Some(card) = &card {
                    reapprove(tx, &stale, card, forge_head)?;
                }
                return Ok(Execution::Stale(stale));
            }
            if let Some(pr_id) = &approval.pr_id
                && let Some(other) = find::<Approval>(tx.conn(), "pr_id = ?1 AND state = 'executing'", &[pr_id])? {
                    return Err(Error::refused("pr", pr_id, format!("approval {} is executing", other.id)));
                }
            tx.conn().execute(
                "UPDATE approvals SET state = 'executing', executed_at = ?2, rev = rev + 1 WHERE id = ?1",
                params![approval.id, tx.now()],
            )?;
            tx.event(
                NewEvent::new("approval", &approval.id, "approval.executing")
                    .rev(approval.rev + 1)
                    .card_opt(approval.card_id.clone())
                    .payload(json!({"head_sha": forge_head})),
            )?;
            Ok(Execution::Executing(get(tx.conn(), "approval", &approval.id)?))
        })
    }

    /// `approval.stale` (daemon only): the land sequence re-read the forge and
    /// the binding no longer holds for a reason `pr.refresh` does not see:
    /// checks failed, the PR closed, a grant condition stopped holding
    /// (`authority.md` section 6, steps 2 and 3). Natural: a stale approval
    /// is returned.
    pub fn approval_stale(
        &mut self,
        ctx: &Ctx,
        approval: &str,
        why: &str,
    ) -> Result<Applied<Approval>> {
        require_daemon(&ctx.actor, "approval.stale")?;
        required("reason", why)?;
        self.write(ctx, "approval.stale", None, |tx| {
            let approval = load(tx, approval)?;
            if approval.state == ApprovalState::Stale {
                return Ok(approval);
            }
            mark_stale(tx, &approval, why)
        })
    }

    /// `approval.finish` (daemon only). On success: the card's active runs end
    /// `landed`, then the card closes `landed` (releasing dependants), or a
    /// `land_direct` with a deploy workflow opens the deploy watch. Linked
    /// memory approvals finish with it.
    pub fn approval_finish(
        &mut self,
        ctx: &Ctx,
        approval: &str,
        finish: Finish,
        result: Value,
    ) -> Result<Applied<Finished>> {
        require_daemon(&ctx.actor, "approval.finish")?;
        self.write(ctx, "approval.finish", None, |tx| {
            let approval = load(tx, approval)?;
            let to = approval.state.finish(finish).map_err(refuse("approval", &approval.id))?;
            let finished_at = (to != ApprovalState::Uncertain).then(|| tx.now());
            tx.conn().execute(
                "UPDATE approvals SET state = ?2, finished_at = ?3, result = ?4, rev = rev + 1 WHERE id = ?1",
                params![approval.id, to, finished_at, result],
            )?;
            tx.event(
                NewEvent::new("approval", &approval.id, "approval.finished")
                    .rev(approval.rev + 1)
                    .card_opt(approval.card_id.clone())
                    .payload(json!({"state": to, "result": result})),
            )?;
            let linked: Vec<Approval> = list(
                tx.conn(),
                "linked_approval_id = ?1 AND state = 'accepted' ORDER BY id",
                &[&approval.id],
            )?;
            if to != ApprovalState::Uncertain {
                for memory in linked {
                    tx.conn().execute(
                        "UPDATE approvals SET state = ?2, executed_at = ?3, finished_at = ?3, result = ?4, rev = rev + 1
                         WHERE id = ?1",
                        params![memory.id, to, tx.now(), result],
                    )?;
                    tx.event(
                        NewEvent::new("approval", &memory.id, "approval.finished")
                            .rev(memory.rev + 1)
                            .card_opt(memory.card_id.clone())
                            .payload(json!({"state": to, "linked_approval_id": approval.id})),
                    )?;
                }
            }
            let mut finished = Finished {
                approval: get(tx.conn(), "approval", &approval.id)?,
                ended_runs: Vec::new(),
                closed_card: None,
                deploy_watch: None,
            };
            if to == ApprovalState::Succeeded
                && let Some(card_id) = &approval.card_id {
                    let card: Card = get(tx.conn(), "card", card_id)?;
                    let sha = result
                        .get("sha")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or(approval.head_sha.clone());
                    let (ended, closed, watch) = land(tx, &approval, &card, sha.as_deref())?;
                    finished.ended_runs = ended;
                    finished.closed_card = closed;
                    finished.deploy_watch = watch;
                }
            Ok(finished)
        })
    }

    /// `grant.create`. Owner only.
    pub fn grant_create(&mut self, ctx: &Ctx, new: NewGrant) -> Result<Applied<Grant>> {
        require_owner(&ctx.actor, "grant.create")?;
        self.write(ctx, "grant.create", None, |tx| insert_grant(tx, &new))
    }

    /// `grant.revoke` (owner; daemon on expiry or `away.end`). Accepted
    /// approvals from it that have not started go `stale`. Natural: a revoked
    /// grant is returned.
    pub fn grant_revoke(&mut self, ctx: &Ctx, grant: &str, reason: &str) -> Result<Applied<Grant>> {
        if !ctx.actor.is_owner() && ctx.actor != Actor::Daemon {
            return Err(Error::Forbidden(format!(
                "grant.revoke is owner only; {} may not",
                ctx.actor
            )));
        }
        required("reason", reason)?;
        self.write(ctx, "grant.revoke", None, |tx| {
            let id = resolve_id(tx.conn(), "grants", "grant", grant)?;
            revoke_grant(tx, &id, reason)
        })
    }

    /// `away.start`. Owner only. Writes the `away` approval, accepted, and any
    /// temporary grants the scope asks for.
    pub fn away_start(&mut self, ctx: &Ctx, scope: Value) -> Result<Applied<Approval>> {
        require_owner(&ctx.actor, "away.start")?;
        validate_away_scope(&scope)?;
        self.write(ctx, "away.start", None, |tx| {
            if let Some(active) = find::<Approval>(tx.conn(), "kind = 'away' AND state = 'accepted'", &[])? {
                return Err(Error::refused("approval", &active.id, "away mode is already on"));
            }
            let until = scope.get("until_at").and_then(Value::as_str).map(str::parse::<Timestamp>);
            let Some(Ok(until)) = until else {
                return Err(Error::Usage("away scope needs until_at".into()));
            };
            if until <= tx.now() {
                return Err(Error::Usage(format!("away until_at {until} is not in the future")));
            }
            let projects: Vec<String> = serde_json::from_value(scope.get("projects").cloned().unwrap_or(json!([])))?;
            if projects.is_empty() {
                return Err(Error::Usage("away scope names no projects".into()));
            }
            let first: Project = get(tx.conn(), "project", &projects[0])?;
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO approvals (id, kind, project_id, scope, action, state, requested_at, requested_by,
                                        decided_at, decided_by)
                 VALUES (?1, 'away', ?2, ?3, 'away', 'accepted', ?4, ?5, ?4, 'owner')",
                params![id, first.id, scope, tx.now(), tx.actor().to_string()],
            )?;
            tx.event(
                NewEvent::new("approval", &id, "approval.decided")
                    .rev(1)
                    .payload(json!({"kind": "away", "until_at": until, "projects": projects})),
            )?;
            if let Some(conditions) = scope.get("merge").filter(|m| m.is_object()) {
                for project in &projects {
                    let mut conditions = conditions.clone();
                    conditions["expires_at"] = json!(until);
                    conditions["away_approval_id"] = json!(id);
                    insert_grant(
                        tx,
                        &NewGrant {
                            project_id: project.clone(),
                            kind: GrantKind::Merge,
                            conditions,
                        },
                    )?;
                }
            }
            get(tx.conn(), "approval", &id)
        })
    }

    /// `away.end` (owner; reconcile when `until_at` passes): revokes the
    /// temporary grants and moves the approval to `succeeded`. Natural: no
    /// away mode on returns `None`.
    pub fn away_end(&mut self, ctx: &Ctx, reason: &str) -> Result<Applied<Option<Approval>>> {
        if !ctx.actor.is_owner() && ctx.actor != Actor::Daemon {
            return Err(Error::Forbidden(format!(
                "away.end is owner only; {} may not",
                ctx.actor
            )));
        }
        self.write(ctx, "away.end", None, |tx| {
            let Some(away) = find::<Approval>(tx.conn(), "kind = 'away' AND state = 'accepted'", &[])? else {
                return Ok(None);
            };
            tx.conn().execute(
                "UPDATE approvals SET state = 'succeeded', executed_at = ?2, finished_at = ?2, result = ?3, rev = rev + 1
                 WHERE id = ?1",
                params![away.id, tx.now(), json!({"reason": reason})],
            )?;
            tx.event(
                NewEvent::new("approval", &away.id, "approval.finished")
                    .rev(away.rev + 1)
                    .payload(json!({"kind": "away", "reason": reason})),
            )?;
            let grants: Vec<Grant> = list(
                tx.conn(),
                "revoked_at IS NULL AND json_extract(conditions, '$.away_approval_id') = ?1 ORDER BY id",
                &[&away.id],
            )?;
            for grant in grants {
                revoke_grant(tx, &grant.id, "away mode ended")?;
            }
            Ok(Some(get(tx.conn(), "approval", &away.id)?))
        })
    }

    /// `deploy.record` (daemon forge poller): resolves a deploy watch. A
    /// `success` or `superseded` meets the obligation and closes the card
    /// `landed`; anything else opens a `recovery` item and leaves the watch
    /// open. Keyed by the caller with the obligation, run and result.
    pub fn deploy_record(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        card: &str,
        result: DeployResult,
        details: Value,
    ) -> Result<Applied<DeployRecorded>> {
        require_daemon(&ctx.actor, "deploy.record")?;
        self.write(ctx, "deploy.record", key, |tx| {
            let card = resolve_card(tx.conn(), card)?;
            let watch: Obligation = find(
                tx.conn(),
                "card_id = ?1 AND state = 'open' AND trigger_kind = 'event'
                 AND json_extract(trigger_event, '$.kind') = 'deploy.finished'",
                &[&card.id],
            )?
            .ok_or_else(|| Error::refused("card", &card.display_id, "no open deploy watch"))?;
            let seq = tx.event(
                NewEvent::new("obligation", &watch.id, "deploy.finished")
                    .card(&card.id)
                    .payload(json!({"result": result, "details": details})),
            )?;
            let mut recorded = DeployRecorded {
                obligation: watch.clone(),
                closed_card: None,
                attention: None,
            };
            match result {
                DeployResult::Success | DeployResult::Superseded => {
                    recorded.obligation = meet_obligation(tx, &watch, seq)?;
                    // A run that shows up after `missing`, `timed_out` or a
                    // failed run still resolves the watch (`state.md`
                    // section 7), so the recovery items this watch opened
                    // no longer apply and must not keep the card open.
                    let own: Vec<Attention> = list(
                        tx.conn(),
                        "card_id = ?1 AND kind = 'recovery' AND answered_at IS NULL
                         AND withdrawn_at IS NULL
                         AND json_extract(evidence, '$.obligation_id') = ?2",
                        &[&card.id, &watch.id],
                    )?;
                    for item in &own {
                        withdraw_attention(tx, item, &format!("the deploy later ended {result}"))?;
                    }
                    if open_attention_id(tx.conn(), &card.id)?.is_none()
                        && active_runs(tx.conn(), &card.id)?.is_empty()
                    {
                        let closed = close_card(
                            tx,
                            &card,
                            CardOutcome::Landed,
                            json!({"deploy": result, "obligation_id": watch.id}),
                        )?;
                        recorded.closed_card = Some(closed.card);
                    }
                }
                _ => {
                    recorded.attention = Some(open_attention(
                        tx,
                        &NewAttention {
                            card: card.id.clone(),
                            kind: AttentionKind::Recovery,
                            question: format!(
                                "The deploy of {} ended {result}. Mark landed or reopen work?",
                                card.display_id
                            ),
                            evidence: Some(
                                json!({"obligation_id": watch.id, "details": details}).to_string(),
                            ),
                            options: Some(json!([MARK_LANDED, REOPEN_WORK])),
                            ..NewAttention::default()
                        },
                    )?);
                }
            }
            Ok(recorded)
        })
    }
}

/// The running away contract: the `away` approval and what its scope limits
/// (`authority.md`, section 6, "Away mode"). Only an `accepted` approval whose
/// `until_at` has not passed counts, so a contract nobody ended yet stops
/// binding at the time the owner gave.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AwayContract {
    pub approval_id: String,
    pub started_at: Timestamp,
    pub until_at: Timestamp,
    pub projects: Vec<String>,
    /// Standing orders for the orchestrators to read.
    pub orders: Option<String>,
    /// No run starts once this many have started since the contract began.
    pub max_new_runs: Option<i64>,
    /// A weekly quota floor that applies when stricter than the standing one.
    pub quota_floor_percent: Option<f64>,
    /// `merge` is `"none"`: no merges while away, whatever grants exist.
    pub no_merges: bool,
}

impl AwayContract {
    pub fn from_approval(approval: &Approval) -> Result<Self> {
        let scope = approval.scope.clone().unwrap_or(Value::Null);
        let until_at = scope
            .get("until_at")
            .and_then(Value::as_str)
            .and_then(|at| at.parse::<Timestamp>().ok())
            .ok_or_else(|| Error::Usage("away scope needs until_at".into()))?;
        let spend = scope.get("spend");
        Ok(Self {
            approval_id: approval.id.clone(),
            started_at: approval.requested_at,
            until_at,
            projects: serde_json::from_value(scope.get("projects").cloned().unwrap_or(json!([])))?,
            orders: scope
                .get("orders")
                .and_then(Value::as_str)
                .map(str::to_owned),
            max_new_runs: spend
                .and_then(|s| s.get("max_new_runs"))
                .and_then(Value::as_i64),
            quota_floor_percent: spend
                .and_then(|s| s.get("quota_floor_percent"))
                .and_then(Value::as_f64),
            no_merges: scope.get("merge").and_then(Value::as_str) == Some("none"),
        })
    }

    /// Runs that started since the contract began, whoever started them.
    pub fn new_runs(&self, conn: &Connection) -> Result<i64> {
        Ok(conn.query_row(
            &format!("SELECT count(*) FROM runs WHERE {}", Self::started_since(1)),
            params![self.approval_id],
            |row| row.get(0),
        )?)
    }

    /// A `runs` filter for the runs started after the contract began, with
    /// the away approval's id bound at `?{param}`: a run whose first
    /// `run.started` or `run.relaunched` event (a new attempt is a new run
    /// with only the latter) follows the contract's first event. It reads the
    /// event order, not `started_at`: a timestamp has millisecond grain, so a
    /// run started just before the owner left can share the contract's
    /// millisecond. A contract with no event counts every run, so the cap
    /// errs closed.
    pub fn started_since(param: usize) -> String {
        format!(
            "id IN (SELECT entity_id FROM events
                    WHERE entity = 'run' AND kind IN ('run.started', 'run.relaunched')
                    GROUP BY entity_id
                    HAVING min(seq) > (SELECT coalesce(min(seq), 0) FROM events
                                       WHERE entity = 'approval' AND entity_id = ?{param}))"
        )
    }
}

/// The away contract binding at `now`, if the owner is away.
pub fn active_away(conn: &Connection, now: Timestamp) -> Result<Option<AwayContract>> {
    let Some(approval) = find::<Approval>(conn, "kind = 'away' AND state = 'accepted'", &[])?
    else {
        return Ok(None);
    };
    let contract = AwayContract::from_approval(&approval)?;
    Ok((contract.until_at > now).then_some(contract))
}

/// Refuses an away scope whose parts cannot be enforced as written.
fn validate_away_scope(scope: &Value) -> Result<()> {
    if let Some(orders) = scope.get("orders")
        && !orders.as_str().is_some_and(|text| !text.trim().is_empty())
    {
        return Err(Error::Usage("away orders must be non-empty text".into()));
    }
    if let Some(spend) = scope.get("spend") {
        if !spend.is_object() {
            return Err(Error::Usage("away spend must be an object".into()));
        }
        if let Some(cap) = spend.get("max_new_runs")
            && !cap.as_i64().is_some_and(|cap| cap >= 0)
        {
            return Err(Error::Usage(
                "away spend max_new_runs must be a whole number, 0 or more".into(),
            ));
        }
        if let Some(floor) = spend.get("quota_floor_percent")
            && !floor
                .as_f64()
                .is_some_and(|floor| (0.0..=100.0).contains(&floor))
        {
            return Err(Error::Usage(
                "away spend quota_floor_percent must be 0 to 100".into(),
            ));
        }
    }
    match scope.get("merge") {
        None => Ok(()),
        Some(merge) if merge.is_object() => Ok(()),
        Some(Value::String(mode)) if mode == "none" || mode == "grants" => Ok(()),
        Some(_) => Err(Error::Usage(
            "away merge must be \"none\", \"grants\" or a grant conditions object".into(),
        )),
    }
}

fn insert_grant(tx: &mut Tx<'_>, new: &NewGrant) -> Result<Grant> {
    let project: Project = get(tx.conn(), "project", &new.project_id)?;
    if new.conditions.get("v").and_then(Value::as_i64) != Some(1) {
        return Err(Error::Usage(
            "grant conditions must be version 1 (\"v\": 1)".into(),
        ));
    }
    let actions = new.conditions.get("actions").and_then(Value::as_array);
    let allowed: &[&str] = match new.kind {
        GrantKind::Merge => &["merge", "squash", "rebase"],
        GrantKind::LandDirect => &["push"],
    };
    if !actions.is_some_and(|list| {
        !list.is_empty()
            && list
                .iter()
                .all(|a| a.as_str().is_some_and(|a| allowed.contains(&a)))
    }) {
        return Err(Error::Usage(format!(
            "a {} grant's actions must be some of {}",
            new.kind,
            allowed.join(", ")
        )));
    }
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO grants (id, project_id, kind, conditions, granted_at, granted_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, project.id, new.kind, new.conditions, tx.now(), tx.actor().to_string()],
    )?;
    tx.event(NewEvent::new("grant", &id, "grant.created").payload(
        json!({"project_id": project.id, "kind": new.kind, "conditions": new.conditions}),
    ))?;
    get(tx.conn(), "grant", &id)
}

fn revoke_grant(tx: &mut Tx<'_>, id: &str, reason: &str) -> Result<Grant> {
    let grant: Grant = get(tx.conn(), "grant", id)?;
    if grant.revoked_at.is_some() {
        return Ok(grant);
    }
    tx.conn().execute(
        "UPDATE grants SET revoked_at = ?2, revoke_reason = ?3 WHERE id = ?1",
        params![grant.id, tx.now(), reason],
    )?;
    tx.event(
        NewEvent::new("grant", &grant.id, "grant.revoked").payload(json!({"reason": reason})),
    )?;
    let accepted: Vec<Approval> = list(
        tx.conn(),
        "grant_id = ?1 AND state = 'accepted' ORDER BY id",
        &[&grant.id],
    )?;
    for approval in accepted {
        mark_stale(tx, &approval, &format!("grant {} was revoked", grant.id))?;
    }
    get(tx.conn(), "grant", &grant.id)
}
