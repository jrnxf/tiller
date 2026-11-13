//! The deploy poller (P11; `protocol.md` section 4.6): while a deploy-watch
//! obligation is open, reads the runs of the project's `deploy_workflow` on
//! its default branch and records the result with `deploy.record`.
//!
//! A run counts for a watch only when it matches the commit (its `head_sha`
//! is the pushed SHA, or descends from it) and, when the project names a
//! `deploy_environment`, a job of the run deployed to that environment. The
//! decision is [`evaluate`], pure over a [`DeployForge`], so the tests drive
//! it with a fake; [`GhDeploys`] is the real forge, `gh api` as the owner's
//! login. Nothing here writes to the forge.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tiller_agents::land::DaemonLock;
use tiller_core::domain::{DeployResult, Obligation, Project};
use tiller_core::state::{get, list};
use tiller_core::{Store, Timestamp};
use tiller_poll::{PollError, Source};

pub mod gate;

/// Minutes after the push with no matching run before the watch records
/// `missing`.
pub const MISSING_AFTER_MINUTES: i64 = 10;

#[derive(Debug, thiserror::Error)]
#[error("deploy forge read failed: {0}")]
pub struct DeployError(pub String);

/// One run of the deploy workflow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeployRun {
    pub id: u64,
    pub head_sha: String,
    /// `queued`, `in_progress` or `completed`.
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
    pub created_at: Option<Timestamp>,
}

/// What the deploy watch reads of the forge.
pub trait DeployForge {
    /// The recent runs of `workflow` on `branch`, newest first.
    fn runs(&self, repo: &str, workflow: &str, branch: &str)
    -> Result<Vec<DeployRun>, DeployError>;
    /// The environments a job of `run` deployed to.
    fn deployed_environments(
        &self,
        repo: &str,
        run: &DeployRun,
    ) -> Result<Vec<String>, DeployError>;
    /// Whether `head` is `base` or a descendant of it.
    fn descends(&self, repo: &str, base: &str, head: &str) -> Result<bool, DeployError>;
}

/// An open deploy watch and what it needs to be judged.
#[derive(Clone, Debug)]
pub struct Watch {
    pub obligation_id: String,
    pub card_id: String,
    pub repo: String,
    pub workflow: String,
    pub branch: String,
    pub environment: Option<String>,
    /// The commit the daemon pushed.
    pub sha: String,
    pub pushed_at: Timestamp,
    pub due_at: Timestamp,
}

/// What the forge says about a watch right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub result: DeployResult,
    /// The run the result is about; `None` for `missing` and a `timed_out`
    /// that no run caused.
    pub run: Option<DeployRun>,
}

impl Verdict {
    /// The `deploy.record` key: the obligation, the run and the result, so a
    /// re-poll writes nothing new.
    pub fn key(&self, watch: &Watch) -> String {
        let run = self
            .run
            .as_ref()
            .map_or_else(|| "none".to_owned(), |run| run.id.to_string());
        format!("deploy:{}:{run}:{}", watch.obligation_id, self.result)
    }

    fn details(&self, watch: &Watch) -> Value {
        json!({
            "sha": watch.sha,
            "environment": watch.environment,
            "run_id": self.run.as_ref().map(|run| run.id),
            "run_sha": self.run.as_ref().map(|run| run.head_sha.as_str()),
            "url": self.run.as_ref().map(|run| run.url.as_str()),
        })
    }
}

/// A run that matched the watch's commit and environment.
struct Matched {
    run: DeployRun,
    /// The run built the pushed SHA itself, not a descendant.
    exact: bool,
}

enum Outcome {
    Success,
    Ignored,
    Pending,
    Ended(DeployResult),
}

fn outcome(run: &DeployRun) -> Outcome {
    if run.status != "completed" {
        return Outcome::Pending;
    }
    match run.conclusion.as_deref() {
        Some("success") => Outcome::Success,
        // A skipped run deployed nothing: it is neither a deploy nor a failure.
        Some("skipped" | "neutral") => Outcome::Ignored,
        Some("cancelled") => Outcome::Ended(DeployResult::Cancelled),
        _ => Outcome::Ended(DeployResult::Failed),
    }
}

fn matching(
    forge: &dyn DeployForge,
    watch: &Watch,
    runs: Vec<DeployRun>,
) -> Result<Vec<Matched>, DeployError> {
    let mut matched = Vec::new();
    for run in runs {
        let exact = run.head_sha == watch.sha;
        // A later commit's run can only be a run created after the push.
        if !exact
            && (run.created_at.is_none_or(|at| at < watch.pushed_at)
                || !forge.descends(&watch.repo, &watch.sha, &run.head_sha)?)
        {
            continue;
        }
        if let Some(environment) = &watch.environment
            && !forge
                .deployed_environments(&watch.repo, &run)?
                .contains(environment)
        {
            continue;
        }
        matched.push(Matched { run, exact });
    }
    Ok(matched)
}

/// Judges one watch. `None` means nothing to record yet.
///
/// A success beats everything: the pushed SHA's own run is `success`, or a
/// later run for a descendant is `superseded` (the pushed run was skipped,
/// cancelled by concurrency or never queued). Otherwise the pushed SHA's own
/// failure is recorded, then a run still going waits for `due_at`
/// (`timed_out`), then a descendant's failure is recorded, and with no run at
/// all the watch records `missing` once ten minutes have passed.
pub fn evaluate(
    forge: &dyn DeployForge,
    watch: &Watch,
    now: Timestamp,
) -> Result<Option<Verdict>, DeployError> {
    let runs = forge.runs(&watch.repo, &watch.workflow, &watch.branch)?;
    let matched = matching(forge, watch, runs)?;
    let first = |exact: bool, wanted: &dyn Fn(&Outcome) -> bool| {
        matched
            .iter()
            .find(|m| m.exact == exact && wanted(&outcome(&m.run)))
    };
    let succeeded = |m: &Matched| Verdict {
        result: if m.exact {
            DeployResult::Success
        } else {
            DeployResult::Superseded
        },
        run: Some(m.run.clone()),
    };
    if let Some(m) = first(true, &|o| matches!(o, Outcome::Success))
        .or_else(|| first(false, &|o| matches!(o, Outcome::Success)))
    {
        return Ok(Some(succeeded(m)));
    }
    let ended = |m: &Matched| match outcome(&m.run) {
        Outcome::Ended(result) => Some(Verdict {
            result,
            run: Some(m.run.clone()),
        }),
        _ => None,
    };
    if let Some(verdict) = matched.iter().filter(|m| m.exact).find_map(ended) {
        return Ok(Some(verdict));
    }
    if matched
        .iter()
        .any(|m| matches!(outcome(&m.run), Outcome::Pending))
    {
        return Ok((now >= watch.due_at).then(|| Verdict {
            result: DeployResult::TimedOut,
            run: matched
                .iter()
                .find(|m| matches!(outcome(&m.run), Outcome::Pending))
                .map(|m| m.run.clone()),
        }));
    }
    if let Some(verdict) = matched.iter().find_map(ended) {
        return Ok(Some(verdict));
    }
    Ok(
        (now >= watch.pushed_at.plus_minutes(MISSING_AFTER_MINUTES)).then_some(Verdict {
            result: DeployResult::Missing,
            run: None,
        }),
    )
}

/// The open deploy watches whose card is still open, with the project facts
/// each needs. A watch that cannot be judged (a project with no repo or no
/// workflow, an obligation that names no SHA) is left out.
pub fn open_watches(store: &Store) -> tiller_core::Result<Vec<Watch>> {
    let obligations: Vec<Obligation> = list(
        store.conn(),
        "state = 'open' AND trigger_kind = 'event'
         AND json_extract(trigger_event, '$.kind') = 'deploy.finished'
         AND card_id IN (SELECT id FROM cards WHERE state = 'open')",
        &[],
    )?;
    let mut watches = Vec::new();
    for obligation in obligations {
        let (Some(card_id), Some(event)) = (&obligation.card_id, &obligation.trigger_event) else {
            continue;
        };
        let project: Project = get(store.conn(), "project", &obligation.project_id)?;
        let (Some(repo), Some(workflow), Some(sha), Some(due_at)) = (
            project.repo.clone(),
            project.deploy_workflow.clone(),
            event.get("sha").and_then(Value::as_str),
            obligation.due_at,
        ) else {
            continue;
        };
        watches.push(Watch {
            obligation_id: obligation.id.clone(),
            card_id: card_id.clone(),
            repo,
            workflow,
            branch: project.default_branch.clone(),
            environment: project.deploy_environment.clone(),
            sha: sha.to_owned(),
            pushed_at: obligation.opened_at,
            due_at,
        });
    }
    Ok(watches)
}

/// One pass: judges every open watch and records what the forge says. A
/// watch the forge cannot be read for is logged and tried again next pass.
pub fn poll_watches(
    store: &mut Store,
    forge: &dyn DeployForge,
    ctx: &tiller_core::Ctx,
    now: Timestamp,
) -> tiller_core::Result<Vec<(Watch, Verdict)>> {
    let mut recorded = Vec::new();
    for watch in open_watches(store)? {
        let verdict = match evaluate(forge, &watch, now) {
            Ok(Some(verdict)) => verdict,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(obligation = %watch.obligation_id, %error, "deploy watch not read");
                continue;
            }
        };
        let key = verdict.key(&watch);
        match store.deploy_record(
            ctx,
            Some(&key),
            &watch.card_id,
            verdict.result,
            verdict.details(&watch),
        ) {
            Ok(_) => recorded.push((watch, verdict)),
            Err(error) => {
                tracing::warn!(obligation = %watch.obligation_id, %error, "deploy not recorded");
            }
        }
    }
    Ok(recorded)
}

/// The deploy poller: 60 s, the cadence the spec gives a watch. With no open
/// watch a pass is one query.
pub struct Deploys {
    pub store: Store,
    pub lock: Arc<DaemonLock>,
    pub forge: GhDeploys,
}

impl Source for Deploys {
    fn name(&self) -> String {
        "deploy".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        let ctx = self.lock.ctx();
        for (watch, verdict) in poll_watches(&mut self.store, &self.forge, &ctx, now)? {
            tracing::info!(
                obligation = %watch.obligation_id,
                result = %verdict.result,
                "deploy recorded"
            );
        }
        Ok(())
    }
}

/// GitHub through `gh api`, as the owner's login.
pub struct GhDeploys {
    program: String,
}

impl GhDeploys {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn get(&self, path: &str) -> Result<Value, DeployError> {
        let output = Command::new(&self.program)
            .args(["api", path])
            .env("GH_PROMPT_DISABLED", "1")
            .output()
            .map_err(|error| DeployError(format!("run gh: {error}")))?;
        if !output.status.success() {
            return Err(DeployError(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| DeployError(format!("{path}: {error}")))
    }
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

impl DeployForge for GhDeploys {
    fn runs(
        &self,
        repo: &str,
        workflow: &str,
        branch: &str,
    ) -> Result<Vec<DeployRun>, DeployError> {
        let reply = self.get(&format!(
            "repos/{repo}/actions/workflows/{workflow}/runs?branch={branch}&per_page=50"
        ))?;
        let runs = reply
            .get("workflow_runs")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        Ok(runs
            .iter()
            .filter_map(|run| {
                Some(DeployRun {
                    id: run.get("id")?.as_u64()?,
                    head_sha: text(run, "head_sha"),
                    status: text(run, "status"),
                    conclusion: run
                        .get("conclusion")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    url: text(run, "html_url"),
                    created_at: text(run, "created_at").parse().ok(),
                })
            })
            .collect())
    }

    /// GitHub Actions records each environment deploy as a deployment whose
    /// status links to the run (`/actions/runs/<id>`), so a run's
    /// environments are the deployments of its commit with such a status.
    fn deployed_environments(
        &self,
        repo: &str,
        run: &DeployRun,
    ) -> Result<Vec<String>, DeployError> {
        let deployments = self.get(&format!(
            "repos/{repo}/deployments?sha={}&per_page=100",
            run.head_sha
        ))?;
        let marker = format!("/actions/runs/{}", run.id);
        let links_to_run = |url: &str| {
            url.find(&marker).is_some_and(|at| {
                url[at + marker.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_ascii_digit())
            })
        };
        let mut environments = Vec::new();
        for deployment in deployments
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let (Some(id), environment) = (
                deployment.get("id").and_then(Value::as_u64),
                text(deployment, "environment"),
            ) else {
                continue;
            };
            let statuses = self.get(&format!("repos/{repo}/deployments/{id}/statuses"))?;
            let linked = statuses
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .any(|status| {
                    links_to_run(&text(status, "log_url"))
                        || links_to_run(&text(status, "target_url"))
                });
            if linked && !environments.contains(&environment) {
                environments.push(environment);
            }
        }
        Ok(environments)
    }

    fn descends(&self, repo: &str, base: &str, head: &str) -> Result<bool, DeployError> {
        let reply = self.get(&format!("repos/{repo}/compare/{base}...{head}"))?;
        Ok(matches!(
            reply.get("status").and_then(Value::as_str),
            Some("ahead" | "identical")
        ))
    }
}

#[cfg(test)]
mod tests;
