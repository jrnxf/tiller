//! The ruleset check (`authority.md` section 6, "Enforced merge gate";
//! `protocol.md`, Ruleset check): whether a project's forge refuses every
//! merge and default-branch update that does not come from the daemon's
//! merge App, recorded as `projects.merge_gate` through `project.merge_gate`.
//!
//! A project is `enforced` only when the rules GitHub applies to its default
//! branch require a pull request, require at least one status check,
//! restrict updates and block force pushes and deletion; every ruleset
//! behind those rules has the App, always, as its one bypass actor; and the
//! App is installed on the repo. Anything less is `detect`, the labelled
//! fallback. A ruleset on the repo is how a project opts in, whatever its
//! delivery: a `no-mistakes` project is meant to have one, and a `direct-pr`
//! or `direct-push` project gets one when the owner wants it gated.
//!
//! The rules are read as the owner's login (`gh api`), which sees the bypass
//! list as the repo's admin; the App answers only whether it is installed.
//! A read that fails changes nothing: the check tries again next pass.

use std::collections::{BTreeSet, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use tiller_agents::land::DaemonLock;
use tiller_agents::land::app::{APP_ID_FILE, KEY_FILE};
use tiller_agents::land::forge::{AppTokens, ForgeError};
use tiller_core::domain::{Delivery, MergeGate, Project};
use tiller_core::{Ctx, Store, Timestamp};
use tiller_herdr::poller::{PollError, Source};

/// How often each project is checked again.
pub const RECHECK: Duration = Duration::from_secs(60 * 60);

/// How soon a check that could not read the forge or the App runs again.
pub const RETRY: Duration = Duration::from_secs(10 * 60);

/// The rule types an enforced default branch needs, as GitHub names them.
const REQUIRED_RULES: [&str; 5] = [
    "pull_request",
    "required_status_checks",
    "update",
    "non_fast_forward",
    "deletion",
];

#[derive(Debug, thiserror::Error)]
#[error("ruleset read failed: {0}")]
pub struct GateError(pub String);

/// What the check reads of the forge, as the owner's login.
pub trait RulesetForge {
    /// Every active rule GitHub applies to `branch`, each carrying the
    /// `ruleset_id` it comes from.
    fn branch_rules(&self, repo: &str, branch: &str) -> Result<Vec<Value>, GateError>;
    /// One ruleset, with its `bypass_actors`.
    fn ruleset(&self, repo: &str, id: u64) -> Result<Value, GateError>;
}

/// The check's answer for one project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Enforced,
    /// The forge does not hold the gate, and why.
    Detect(String),
}

/// Judges one project's default branch. `Err` means the forge could not be
/// read, which says nothing about the gate.
pub fn judge(
    forge: &dyn RulesetForge,
    app: &dyn AppTokens,
    repo: &str,
    branch: &str,
) -> Result<Verdict, GateError> {
    let app_id = match app.app_id() {
        Ok(id) => id,
        Err(error) => return app_verdict(error),
    };
    let rules = forge.branch_rules(repo, branch)?;
    let kinds: BTreeSet<&str> = rules
        .iter()
        .filter_map(|rule| rule.get("type").and_then(Value::as_str))
        .collect();
    let missing: Vec<&str> = REQUIRED_RULES
        .iter()
        .copied()
        .filter(|kind| !kinds.contains(kind))
        .collect();
    if !missing.is_empty() {
        return Ok(Verdict::Detect(format!(
            "the rules on {branch} lack {}",
            missing.join(", ")
        )));
    }
    let names_a_check = rules.iter().any(|rule| {
        rule.get("type").and_then(Value::as_str) == Some("required_status_checks")
            && rule
                .pointer("/parameters/required_status_checks")
                .and_then(Value::as_array)
                .is_some_and(|checks| !checks.is_empty())
    });
    if !names_a_check {
        return Ok(Verdict::Detect(format!(
            "the rules on {branch} require no status check"
        )));
    }
    let ids: BTreeSet<u64> = rules
        .iter()
        .filter_map(|rule| rule.get("ruleset_id").and_then(Value::as_u64))
        .collect();
    for id in ids {
        let ruleset = forge.ruleset(repo, id)?;
        let name = ruleset.get("name").and_then(Value::as_str).map_or_else(
            || format!("ruleset {id}"),
            |name| format!("ruleset \"{name}\""),
        );
        let Some(actors) = ruleset.get("bypass_actors").and_then(Value::as_array) else {
            return Ok(Verdict::Detect(format!(
                "the bypass list of {name} is not readable as the owner's login"
            )));
        };
        let only_the_app = matches!(actors.as_slice(), [actor]
            if actor.get("actor_type").and_then(Value::as_str) == Some("Integration")
                && actor.get("actor_id").and_then(Value::as_u64) == Some(app_id)
                && actor.get("bypass_mode").and_then(Value::as_str) == Some("always"));
        if !only_the_app {
            return Ok(Verdict::Detect(format!(
                "{name} must have the merge App (id {app_id}, mode always) as its only bypass actor"
            )));
        }
    }
    if let Err(error) = app.installed(repo) {
        return app_verdict(error);
    }
    Ok(Verdict::Enforced)
}

/// An App that cannot act is `detect`. Anything else (GitHub not reached, a
/// key file being replaced) says nothing about the gate, so it is an `Err`.
fn app_verdict(error: ForgeError) -> Result<Verdict, GateError> {
    match error {
        ForgeError::NoApp { why, .. } => {
            Ok(Verdict::Detect(format!("the merge App cannot act: {why}")))
        }
        other => Err(GateError(other.to_string())),
    }
}

/// Whether the check looks at `project`: it is live and has a forge repo.
fn checked(project: &Project) -> bool {
    project.retired_at.is_none()
        && project.repo.is_some()
        && project.delivery != Delivery::LocalOnly
}

/// One pass over the projects whose check is due: each never checked by this
/// daemon (a project just added) or last checked an hour ago. A check that
/// cannot read the forge or the App changes nothing and runs again after
/// [`RETRY`]; one whose write the store refuses (a project retired
/// meanwhile) is skipped, so neither stops the pass. Writes the
/// verdict through `project.merge_gate`, which changes nothing when the
/// value holds and raises the degraded event and the recovery item on a move
/// to `detect`.
pub fn check_due(
    store: &mut Store,
    forge: &dyn RulesetForge,
    app: &dyn AppTokens,
    ctx: &Ctx,
    last: &mut HashMap<String, Instant>,
    now: Instant,
) -> tiller_core::Result<Vec<(String, Verdict)>> {
    let mut judged = Vec::new();
    for project in store.projects()?.into_iter().filter(checked) {
        if last
            .get(&project.id)
            .is_some_and(|at| now.duration_since(*at) < RECHECK)
        {
            continue;
        }
        let Some(repo) = project.repo.as_deref() else {
            continue;
        };
        let verdict = match judge(forge, app, repo, &project.default_branch) {
            Ok(verdict) => verdict,
            Err(error) => {
                tracing::warn!(project = %project.id, %error, "ruleset check not run");
                last.insert(project.id, now.checked_sub(RECHECK - RETRY).unwrap_or(now));
                continue;
            }
        };
        let (gate, reason) = match &verdict {
            Verdict::Enforced => (MergeGate::Enforced, "the ruleset check holds"),
            Verdict::Detect(why) => (MergeGate::Detect, why.as_str()),
        };
        last.insert(project.id.clone(), now);
        if let Err(error) = store.project_set_merge_gate(ctx, &project.id, gate, reason) {
            tracing::warn!(project = %project.id, %error, "merge gate not recorded");
            continue;
        }
        judged.push((project.id, verdict));
    }
    Ok(judged)
}

/// What the check knows of the App directory's files without reading them:
/// each one's size, mode and modification time.
pub type AppFiles = [Option<(u64, u32, SystemTime)>; 2];

/// The App directory's [`AppFiles`]. A key or id the owner adds, replaces
/// or `chmod`s changes it.
pub fn app_files(dir: &Path) -> AppFiles {
    [KEY_FILE, APP_ID_FILE].map(|name| {
        let meta = std::fs::metadata(dir.join(name)).ok()?;
        Some((meta.len(), meta.permissions().mode(), meta.modified().ok()?))
    })
}

/// The ruleset check as a poller. It wakes every minute so a project added
/// with `tiller project add` is checked within one, and checks each project
/// once an hour. A change to the App's files checks every project again at
/// once, so the owner's last setup step (placing the key) shows in
/// `tiller doctor` within a minute.
pub struct Gate {
    pub store: Store,
    pub lock: Arc<DaemonLock>,
    pub forge: GhRulesets,
    pub app: Box<dyn AppTokens + Send>,
    /// The App directory, watched through [`app_files`].
    pub app_dir: PathBuf,
    pub app_files: AppFiles,
    pub last: HashMap<String, Instant>,
}

impl Source for Gate {
    fn name(&self) -> String {
        "merge-gate".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }

    fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
        let files = app_files(&self.app_dir);
        if files != self.app_files {
            self.app_files = files;
            self.last.clear();
        }
        let ctx = self.lock.ctx();
        let judged = check_due(
            &mut self.store,
            &self.forge,
            self.app.as_ref(),
            &ctx,
            &mut self.last,
            Instant::now(),
        )?;
        for (project, verdict) in judged {
            tracing::info!(project, ?verdict, "merge gate checked");
        }
        Ok(())
    }
}

/// The rulesets through `gh api`, as the owner's login.
pub struct GhRulesets {
    program: String,
}

impl GhRulesets {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn get(&self, args: &[&str]) -> Result<Value, GateError> {
        let output = Command::new(&self.program)
            .arg("api")
            .args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .output()
            .map_err(|error| GateError(format!("run gh: {error}")))?;
        if !output.status.success() {
            return Err(GateError(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        serde_json::from_slice(&output.stdout).map_err(|error| GateError(error.to_string()))
    }
}

impl RulesetForge for GhRulesets {
    fn branch_rules(&self, repo: &str, branch: &str) -> Result<Vec<Value>, GateError> {
        let pages = self.get(&[
            "--paginate",
            "--slurp",
            &format!(
                "repos/{repo}/rules/branches/{}?per_page=100",
                encode_path(branch)
            ),
        ])?;
        Ok(pages
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_array)
            .flatten()
            .cloned()
            .collect())
    }

    fn ruleset(&self, repo: &str, id: u64) -> Result<Value, GateError> {
        self.get(&[&format!("repos/{repo}/rulesets/{id}")])
    }
}

/// `segment` percent-encoded for a URL path, so a branch such as
/// `release/1.0` or one with `#` stays one segment.
fn encode_path(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests;
