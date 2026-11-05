//! Landing (`authority.md` section 6, T4.3): only the daemon merges a PR or
//! moves a default branch, and only by executing an `accepted` approval.
//!
//! Every function here that can change a remote takes a [`DaemonLock`], and
//! the only way to get one is the daemon's single-instance lock, so no CLI
//! path can call them (G01). `tiller land <approval>` run by the owner waits
//! for the daemon to do it.
//!
//! The merge sequence re-reads the PR (step 2), stales a moved head, a closed
//! PR, failed checks or a grant that stopped holding (step 3), scrubs the body
//! (step 4), moves the approval to `executing` (step 5), merges with the
//! bound SHA as the forge's precondition (step 6) and records the result
//! (step 7).

pub mod direct;
pub mod forge;
pub mod grants;
pub mod publish;
pub mod reconcile;
pub mod scrub;

#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use serde_json::json;
use tiller_core::domain::{
    Approval, ApprovalKind, ApprovalState, AttentionKind, CheckPlan, Checks, Delivery, Finish,
    Grant, MergeGate, Pr, PrState, Project, Run,
};
use tiller_core::state::approvals::{Execution, NewApproval};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};

use self::direct::Pushed;
use self::forge::{AppTokens, Forge, ForgeError, Identity, MergeOutcome, MergeRequest, PullView};
use self::scrub::ScrubList;

/// The file under the Tiller home that the daemon holds locked while it runs.
pub const LOCK_FILE: &str = "daemon.lock";

#[derive(Debug, thiserror::Error)]
pub enum LandError {
    #[error("another daemon holds {0}")]
    Held(PathBuf),
    #[error("lock {path}: {error}")]
    Lock {
        path: PathBuf,
        error: std::io::Error,
    },
    /// The approval cannot be landed as it is.
    #[error("approval {id}: {why}")]
    Refused { id: String, why: String },
    #[error(transparent)]
    State(#[from] tiller_core::Error),
}

/// Proof that this process is the host's one daemon. It holds an exclusive
/// lock on `<home>/daemon.lock` for as long as it lives; only
/// [`DaemonLock::acquire`] makes one.
#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
    path: PathBuf,
}

impl DaemonLock {
    /// Takes the single-instance lock, or fails when another daemon holds it.
    pub fn acquire(home: &Path) -> Result<Self> {
        let path = home.join(LOCK_FILE);
        let lock_error = |error| LandError::Lock {
            path: path.clone(),
            error,
        };
        std::fs::create_dir_all(home).map_err(lock_error)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(lock_error)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(LandError::Held(path)),
            Err(TryLockError::Error(error)) => Err(lock_error(error)),
        }
    }

    /// Whether a daemon holds the lock now. A probe takes and drops the lock
    /// when it is free, so a daemon starting at that instant may lose one
    /// attempt; it never runs two daemons.
    pub fn held(home: &Path) -> bool {
        matches!(Self::acquire(home), Err(LandError::Held(_)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The daemon's write context, for the daemon-only operations.
    pub fn ctx(&self) -> Ctx {
        Ctx::new(Actor::Daemon)
    }
}

/// What the land sequence needs besides the store: the forge, the merge App
/// and the scrub list.
pub struct Lander<'a> {
    pub forge: &'a dyn Forge,
    pub app: &'a dyn AppTokens,
    pub scrub: ScrubList,
}

/// What one land attempt came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Landed {
    Succeeded {
        approval: Approval,
        sha: String,
    },
    Failed {
        approval: Approval,
        error: String,
    },
    /// The call was sent and its result could not be read; reconcile
    /// resolves it from the forge, and it is never retried blind.
    Uncertain {
        approval: Approval,
        error: String,
    },
    /// The binding no longer holds; the approval is `stale`.
    Stale {
        approval: Approval,
        why: String,
    },
    /// Not now: checks pending, the forge unreadable, the App missing, the
    /// body refused. The approval stays `accepted` for the next pass.
    Waiting {
        approval: Approval,
        why: String,
    },
    /// The attempt stopped on an error, such as the state store refusing a
    /// write. The pass goes on with the next approval, and the next pass
    /// finds this one in whatever state the error left it.
    Errored {
        approval: Approval,
        error: String,
    },
}

/// What one grant pass came to: the approvals it created, and each PR it
/// skipped after an error, for the next pass to try again.
#[derive(Debug, Default)]
pub struct Matched {
    pub approvals: Vec<Approval>,
    /// The PR id and the error.
    pub errors: Vec<(String, LandError)>,
}

impl Landed {
    pub fn approval(&self) -> &Approval {
        match self {
            Landed::Succeeded { approval, .. }
            | Landed::Failed { approval, .. }
            | Landed::Uncertain { approval, .. }
            | Landed::Stale { approval, .. }
            | Landed::Waiting { approval, .. }
            | Landed::Errored { approval, .. } => approval,
        }
    }
}

type Result<T> = std::result::Result<T, LandError>;

impl Lander<'_> {
    /// One pass: every `accepted` merge or land-direct approval, oldest
    /// first, skipping a PR another approval is executing on (step 1).
    pub fn land_ready(&self, lock: &DaemonLock, store: &mut Store) -> Result<Vec<Landed>> {
        let ready: Vec<Approval> = list(
            store.conn(),
            "state = 'accepted' AND kind IN ('merge', 'land_direct')
             AND (pr_id IS NULL OR pr_id NOT IN (SELECT pr_id FROM approvals WHERE state = 'executing' AND pr_id IS NOT NULL))
             ORDER BY decided_at, id",
            &[],
        )?;
        let mut landed = Vec::new();
        for approval in ready {
            landed.push(match self.land(lock, store, &approval.id) {
                Ok(done) => done,
                Err(error) => Landed::Errored {
                    approval,
                    error: error.to_string(),
                },
            });
        }
        Ok(landed)
    }

    /// The land sequence for one approval.
    pub fn land(&self, lock: &DaemonLock, store: &mut Store, approval: &str) -> Result<Landed> {
        let approval: Approval = get(store.conn(), "approval", approval)?;
        if approval.state != ApprovalState::Accepted {
            return Err(LandError::Refused {
                id: approval.id,
                why: format!("it is {}, not accepted", approval.state),
            });
        }
        match approval.kind {
            ApprovalKind::Merge => self.merge(lock, store, approval),
            ApprovalKind::LandDirect => self.direct(lock, store, approval),
            other => Err(LandError::Refused {
                id: approval.id,
                why: format!("{other} approvals are not landed"),
            }),
        }
    }

    fn merge(&self, lock: &DaemonLock, store: &mut Store, approval: Approval) -> Result<Landed> {
        let ctx = lock.ctx();
        let project: Project = get(store.conn(), "project", &approval.project_id)?;
        let Some(pr_id) = &approval.pr_id else {
            return self.stale(lock, store, approval, "the approval binds no PR");
        };
        let pr: Pr = get(store.conn(), "pr", pr_id)?;
        let repo = approval
            .repo
            .clone()
            .or(project.repo.clone())
            .unwrap_or_default();
        // Step 2: the forge as it is now.
        let view = match self.forge.pull(&repo, pr.number) {
            Ok(view) => view,
            Err(error) => return Ok(waiting(approval, error.to_string())),
        };
        // Step 3.
        if view.state != PrState::Open {
            return self.stale(
                lock,
                store,
                approval,
                &format!("PR #{} is {}", pr.number, view.state),
            );
        }
        if approval.head_sha.as_deref() != Some(view.head_sha.as_str()) {
            // `approval.execute` stales it and asks again at the new head.
            return match store
                .approval_execute(&ctx, &approval.id, &view.head_sha)?
                .value
            {
                Execution::Stale(stale) => Ok(Landed::Stale {
                    approval: stale,
                    why: format!("head moved to {}", view.head_sha),
                }),
                Execution::Executing(executing) => self.finish(
                    lock,
                    store,
                    executing,
                    Finish::Failed,
                    "head check disagreed with the forge",
                ),
            };
        }
        match view.checks {
            Checks::Pending => return Ok(waiting(approval, "checks are pending".into())),
            Checks::Failure => {
                let card = approval.card_id.clone();
                let landed = self.stale(lock, store, approval, "checks failed")?;
                if let Some(card) = card {
                    let why = format!(
                        "PR #{} checks failed at {}; the merge approval is stale",
                        pr.number, view.head_sha
                    );
                    self.notice(lock, store, &card, landed.approval(), "checks", why)?;
                }
                return Ok(landed);
            }
            Checks::Success | Checks::None => {}
        }
        if let Some(grant_id) = &approval.grant_id {
            let grant: Grant = get(store.conn(), "grant", grant_id)?;
            if let Err(miss) = grants::evaluate(
                store.conn(),
                &grant,
                &approval,
                &pr,
                &view,
                Timestamp::now(),
            )? {
                return self.stale(
                    lock,
                    store,
                    approval,
                    &format!("grant no longer holds: {miss}"),
                );
            }
        }
        // Step 4.
        let asks = project_asks(store, &approval.project_id)?;
        if let Some(why) = scrub::scrub(&view.body, &asks, &self.scrub, pr.pipeline_state) {
            if let Some(card) = approval.card_id.clone() {
                let question = format!(
                    "PR #{} cannot land: {why}. Edit the PR body, then it lands.",
                    pr.number
                );
                self.notice(lock, store, &card, &approval, "scrub", question)?;
            }
            return Ok(waiting(approval, why));
        }
        let identity = match self.identity(&project, &repo) {
            Ok(identity) => identity,
            Err(error) => return self.no_identity(lock, store, approval, &error),
        };
        // Step 5.
        let executing = match store
            .approval_execute(&ctx, &approval.id, &view.head_sha)?
            .value
        {
            Execution::Executing(executing) => executing,
            Execution::Stale(stale) => {
                return Ok(Landed::Stale {
                    approval: stale,
                    why: "the binding moved".into(),
                });
            }
        };
        // Step 6.
        let request = MergeRequest {
            repo,
            number: pr.number,
            sha: view.head_sha.clone(),
            method: executing.action,
        };
        // Step 7.
        match self.forge.merge(&request, &identity) {
            MergeOutcome::Merged { sha } => self.succeed(lock, store, executing, sha),
            MergeOutcome::Refused(error) => {
                self.finish(lock, store, executing, Finish::Failed, &error)
            }
            MergeOutcome::Unknown(error) => {
                self.finish(lock, store, executing, Finish::Uncertain, &error)
            }
        }
    }

    fn direct(&self, lock: &DaemonLock, store: &mut Store, approval: Approval) -> Result<Landed> {
        let ctx = lock.ctx();
        let project: Project = get(store.conn(), "project", &approval.project_id)?;
        let (Some(branch), Some(bound)) = (approval.branch.clone(), approval.head_sha.clone())
        else {
            return self.stale(lock, store, approval, "the approval binds no branch");
        };
        let run: Option<Run> = match &approval.run_id {
            Some(id) => find(store.conn(), "id = ?1", &[id])?,
            None => None,
        };
        let Some(checkout) = run.and_then(|run| run.worktree_path).map(PathBuf::from) else {
            return Ok(waiting(approval, "no worktree to push from".into()));
        };
        if project.delivery == Delivery::LocalOnly {
            return self.land_local(lock, store, approval, &project, &checkout);
        }
        let remote = project.remote.clone().unwrap_or_else(|| "origin".into());
        let repo = approval
            .repo
            .clone()
            .or(project.repo.clone())
            .unwrap_or_default();
        let identity = match self.identity(&project, &repo) {
            Ok(identity) => identity,
            Err(error) => return self.no_identity(lock, store, approval, &error),
        };
        let head = match direct::branch_head(&checkout, &branch) {
            Ok(head) => head,
            Err(error) => return Ok(waiting(approval, format!("read {branch}: {error}"))),
        };
        if head != bound {
            return match store.approval_execute(&ctx, &approval.id, &head)?.value {
                Execution::Stale(stale) => Ok(Landed::Stale {
                    approval: stale,
                    why: format!("{branch} moved to {head}"),
                }),
                Execution::Executing(executing) => self.finish(
                    lock,
                    store,
                    executing,
                    Finish::Failed,
                    "head check disagreed with the branch",
                ),
            };
        }
        let default =
            match direct::fetch_default(&checkout, &remote, &project.default_branch, &identity) {
                Ok(default) => default,
                Err(error) => {
                    return Ok(waiting(
                        approval,
                        format!("fetch {}: {error}", project.default_branch),
                    ));
                }
            };
        if !direct::descends(&checkout, &default, &bound) {
            let why = format!(
                "{bound} does not descend from {} at {default}",
                project.default_branch
            );
            return self.stale(lock, store, approval, &why);
        }
        if needs_checks(&project) {
            match self.forge.checks(&repo, &bound) {
                Ok(Checks::Success) => {}
                Ok(Checks::Pending) => return Ok(waiting(approval, "checks are pending".into())),
                Ok(other) => {
                    return self.stale(lock, store, approval, &format!("checks are {other}"));
                }
                Err(error) => return Ok(waiting(approval, error.to_string())),
            }
        }
        if let Some(grant_id) = &approval.grant_id {
            let grant: Grant = get(store.conn(), "grant", grant_id)?;
            if let Err(miss) = grants::direct_holds(&grant, Timestamp::now()) {
                return self.stale(
                    lock,
                    store,
                    approval,
                    &format!("grant no longer holds: {miss}"),
                );
            }
        }
        let executing = match store.approval_execute(&ctx, &approval.id, &bound)?.value {
            Execution::Executing(executing) => executing,
            Execution::Stale(stale) => {
                return Ok(Landed::Stale {
                    approval: stale,
                    why: "the binding moved".into(),
                });
            }
        };
        match direct::push(
            &checkout,
            &remote,
            &bound,
            &project.default_branch,
            &identity,
        ) {
            Pushed::Done => self.succeed(lock, store, executing, bound),
            Pushed::Refused(error) => self.finish(lock, store, executing, Finish::Failed, &error),
            Pushed::Unknown(error) => {
                self.finish(lock, store, executing, Finish::Uncertain, &error)
            }
        }
    }

    /// `local-only` (P12): the `land_direct` checks against the local default
    /// branch, then a fast-forward in the repository, with no forge and no
    /// remote. The caller checked that the approval binds a branch and a SHA.
    fn land_local(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        approval: Approval,
        project: &Project,
        checkout: &Path,
    ) -> Result<Landed> {
        let ctx = lock.ctx();
        let branch = approval.branch.clone().unwrap_or_default();
        let bound = approval.head_sha.clone().unwrap_or_default();
        let (branch, bound) = (branch.as_str(), bound.as_str());
        let head = match direct::branch_head(checkout, branch) {
            Ok(head) => head,
            Err(error) => return Ok(waiting(approval, format!("read {branch}: {error}"))),
        };
        if head != bound {
            return match store.approval_execute(&ctx, &approval.id, &head)?.value {
                Execution::Stale(stale) => Ok(Landed::Stale {
                    approval: stale,
                    why: format!("{branch} moved to {head}"),
                }),
                Execution::Executing(executing) => self.finish(
                    lock,
                    store,
                    executing,
                    Finish::Failed,
                    "head check disagreed with the branch",
                ),
            };
        }
        let default = match direct::local_default(checkout, &project.default_branch) {
            Ok(default) => default,
            Err(error) => {
                return Ok(waiting(
                    approval,
                    format!("read {}: {error}", project.default_branch),
                ));
            }
        };
        if !direct::descends(checkout, &default, bound) {
            let why = format!(
                "{bound} does not descend from {} at {default}",
                project.default_branch
            );
            return self.stale(lock, store, approval, &why);
        }
        if let Some(grant_id) = &approval.grant_id {
            let grant: Grant = get(store.conn(), "grant", grant_id)?;
            if let Err(miss) = grants::direct_holds(&grant, Timestamp::now()) {
                return self.stale(
                    lock,
                    store,
                    approval,
                    &format!("grant no longer holds: {miss}"),
                );
            }
        }
        let executing = match store.approval_execute(&ctx, &approval.id, bound)?.value {
            Execution::Executing(executing) => executing,
            Execution::Stale(stale) => {
                return Ok(Landed::Stale {
                    approval: stale,
                    why: "the binding moved".into(),
                });
            }
        };
        match direct::advance(checkout, &project.default_branch, &default, bound) {
            Pushed::Done => self.succeed(lock, store, executing, bound.to_owned()),
            Pushed::Refused(error) => self.finish(lock, store, executing, Finish::Failed, &error),
            Pushed::Unknown(error) => {
                self.finish(lock, store, executing, Finish::Uncertain, &error)
            }
        }
    }

    /// A gated project whose App gave no token: the approval stays accepted
    /// and the owner is told once, never falling back to the owner's login.
    fn no_identity(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        approval: Approval,
        error: &ForgeError,
    ) -> Result<Landed> {
        let why = format!("the merge App gave no token: {error}");
        if let Some(card) = approval.card_id.clone() {
            let question = format!(
                "Approval {} cannot land: {why}. It stays accepted and lands once the App works.",
                approval.id
            );
            self.notice(lock, store, &card, &approval, "identity", question)?;
        }
        Ok(waiting(approval, why))
    }

    /// Who the forge write acts as: the merge App on a gated project, the
    /// owner's login on a `detect` project.
    fn identity(&self, project: &Project, repo: &str) -> std::result::Result<Identity, ForgeError> {
        match project.merge_gate {
            MergeGate::Enforced => self.app.mint(repo).map(Identity::App),
            MergeGate::Detect => Ok(Identity::Owner),
        }
    }

    fn stale(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        approval: Approval,
        why: &str,
    ) -> Result<Landed> {
        let stale = store.approval_stale(&lock.ctx(), &approval.id, why)?.value;
        Ok(Landed::Stale {
            approval: stale,
            why: why.into(),
        })
    }

    fn succeed(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        approval: Approval,
        sha: String,
    ) -> Result<Landed> {
        let finished = store.approval_finish(
            &lock.ctx(),
            &approval.id,
            Finish::Succeeded,
            json!({"sha": sha}),
        )?;
        Ok(Landed::Succeeded {
            approval: finished.value.approval,
            sha,
        })
    }

    fn finish(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        approval: Approval,
        finish: Finish,
        error: &str,
    ) -> Result<Landed> {
        let finished =
            store.approval_finish(&lock.ctx(), &approval.id, finish, json!({"error": error}))?;
        let approval = finished.value.approval;
        let error = error.to_owned();
        Ok(match finish {
            Finish::Uncertain => Landed::Uncertain { approval, error },
            _ => Landed::Failed { approval, error },
        })
    }

    /// One `merge` attention item per approval and reason, however many
    /// passes see it: the key replays the first.
    fn notice(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        card: &str,
        approval: &Approval,
        reason: &str,
        question: String,
    ) -> Result<()> {
        let key = format!("land:{}:{reason}", approval.id);
        store.attention_open(
            &lock.ctx(),
            Some(&key),
            NewAttention {
                card: card.into(),
                kind: AttentionKind::Merge,
                question,
                evidence: Some(
                    json!({"approval_id": approval.id, "head_sha": approval.head_sha}).to_string(),
                ),
                run_id: approval.run_id.clone(),
                ..NewAttention::default()
            },
        )?;
        Ok(())
    }

    /// The grant matcher: every open PR whose checks are done and that has
    /// no open approval gets one `accepted` approval from the first live
    /// grant whose conditions all hold, bound to the head the forge reports
    /// now (P13, G05).
    pub fn match_grants(&self, lock: &DaemonLock, store: &mut Store) -> Result<Matched> {
        let now = Timestamp::now();
        let grants: Vec<Grant> = list(
            store.conn(),
            "kind = 'merge' AND revoked_at IS NULL ORDER BY granted_at, id",
            &[],
        )?;
        let mut matched = Matched::default();
        if grants.is_empty() {
            return Ok(matched);
        }
        let prs: Vec<Pr> = list(
            store.conn(),
            "state = 'open' AND head_sha IS NOT NULL
             AND id NOT IN (SELECT pr_id FROM approvals WHERE pr_id IS NOT NULL
                            AND state IN ('requested', 'accepted', 'executing', 'uncertain'))
             AND id NOT IN (SELECT pr_id FROM approvals WHERE pr_id IS NOT NULL AND state = 'stale'
                            AND head_sha = prs.head_sha)
             ORDER BY linked_at, id",
            &[],
        )?;
        for pr in prs {
            match self.match_pr(lock, store, &grants, &pr, now) {
                Ok(Some(approval)) => matched.approvals.push(approval),
                Ok(None) => {}
                Err(error) => matched.errors.push((pr.id.clone(), error)),
            }
        }
        Ok(matched)
    }

    /// The first live grant whose conditions all hold for `pr`, as a new
    /// `accepted` approval.
    fn match_pr(
        &self,
        lock: &DaemonLock,
        store: &mut Store,
        grants: &[Grant],
        pr: &Pr,
        now: Timestamp,
    ) -> Result<Option<Approval>> {
        let project: Project = get(store.conn(), "project", &pr.project_id)?;
        let candidates: Vec<&Grant> = grants
            .iter()
            .filter(|grant| grant.project_id == project.id)
            .collect();
        // The poller's cache first: a PR whose checks or pipeline it last saw
        // unfinished costs no forge read this pass.
        if candidates.is_empty()
            || !grants::checks_done(pr.checks)
            || !grants::pipeline_done(pr.pipeline_state)
        {
            return Ok(None);
        }
        let repo = project.repo.clone().unwrap_or_default();
        let Ok(view) = self.forge.pull(&repo, pr.number) else {
            return Ok(None);
        };
        if view.state != PrState::Open
            || pr.head_sha.as_deref() != Some(view.head_sha.as_str())
            || !grants::checks_done(view.checks)
            || !grants::pipeline_done(pr.pipeline_state)
        {
            return Ok(None);
        }
        let asks = project_asks(store, &project.id)?;
        if scrub::scrub(&view.body, &asks, &self.scrub, pr.pipeline_state).is_some() {
            return Ok(None);
        }
        for grant in candidates {
            let Some(action) = grants::action(grant) else {
                continue;
            };
            let draft = draft(pr, &project, action, &view);
            if grants::evaluate(store.conn(), grant, &draft, pr, &view, now)?.is_err() {
                continue;
            }
            let approval = store
                .approval_request(
                    &lock.ctx(),
                    NewApproval {
                        kind: ApprovalKind::Merge,
                        card: pr.card_id.clone(),
                        run_id: pr.run_id.clone(),
                        pr_id: Some(pr.id.clone()),
                        branch: None,
                        head_sha: view.head_sha.clone(),
                        action,
                        memory_paths: None,
                        grant_id: Some(grant.id.clone()),
                    },
                )?
                .value;
            return Ok(Some(approval));
        }
        Ok(None)
    }
}

fn waiting(approval: Approval, why: String) -> Landed {
    Landed::Waiting { approval, why }
}

/// A `direct-push` project whose `checks` names any check needs the forge's
/// result for the pushed SHA. A shape `CheckPlan` does not read counts as
/// naming one, so a bad value never lets a push skip the gate.
fn needs_checks(project: &Project) -> bool {
    match CheckPlan::parse(&project.checks) {
        Ok(plan) => !plan.is_empty(),
        Err(_) => true,
    }
}

fn project_asks(store: &Store, project: &str) -> Result<Vec<String>> {
    let mut statement = store
        .conn()
        .prepare("SELECT text FROM asks WHERE project_id = ?1 AND length(text) > 40")
        .map_err(tiller_core::Error::from)?;
    let rows = statement
        .query_map([project], |row| row.get::<_, String>(0))
        .map_err(tiller_core::Error::from)?;
    Ok(rows
        .collect::<std::result::Result<_, _>>()
        .map_err(tiller_core::Error::from)?)
}

/// The approval a grant would create, for evaluating it before it exists.
fn draft(
    pr: &Pr,
    project: &Project,
    action: tiller_core::domain::ApprovalAction,
    view: &PullView,
) -> Approval {
    let now = Timestamp::now();
    Approval {
        id: String::new(),
        kind: ApprovalKind::Merge,
        project_id: project.id.clone(),
        card_id: Some(pr.card_id.clone()),
        run_id: pr.run_id.clone(),
        repo: project.repo.clone(),
        pr_id: Some(pr.id.clone()),
        branch: None,
        head_sha: Some(view.head_sha.clone()),
        diff_sha256: None,
        base_rev: None,
        scope: None,
        memory_paths: None,
        linked_approval_id: None,
        action,
        state: ApprovalState::Requested,
        requested_at: now,
        requested_by: "daemon".into(),
        decided_at: None,
        decided_by: None,
        decision_idempotency_key: None,
        grant_id: None,
        executed_at: None,
        finished_at: None,
        result: None,
        rev: 0,
    }
}
