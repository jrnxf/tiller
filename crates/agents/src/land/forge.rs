//! The forge seam of the land sequence (`authority.md` section 6): the PR
//! re-read of step 2, the commit checks a grant or a direct push needs, and
//! the one merge call of step 6, made with the bound SHA as a precondition.
//! The publish of a direct-pr run uses it too, to find, open, read and edit
//! the run's PR.
//!
//! [`GhForge`] talks to GitHub through `gh api`: the land sequence needs the
//! REST merge call's `sha` precondition and machine-readable replies, which
//! `gh-axi`'s merge and its TOON output do not give. It is the only forge
//! writer in Tiller.

use std::fmt;
use std::process::{Command, Output};

use serde_json::Value;
use tiller_core::domain::{ApprovalAction, Checks, Mergeable, PrState};

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("forge read failed: {0}")]
    Read(String),
    #[error("forge write failed: {0}")]
    Write(String),
    /// The merge App is not set up for this repo (an owner setup step on the
    /// cutover checklist), so an enforced project cannot land yet.
    #[error("the merge App cannot act on {repo}: {why}")]
    NoApp { repo: String, why: String },
}

/// One PR as the forge reports it at this moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullView {
    pub state: PrState,
    pub head_sha: String,
    pub head_ref: String,
    pub base_ref: String,
    pub checks: Checks,
    pub mergeable: Mergeable,
    pub body: String,
    /// Every path the PR changes, for grant `exclude_paths`.
    pub files: Vec<String>,
}

/// A PR the publish opened or found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRef {
    pub number: i64,
    pub url: String,
    pub head_sha: String,
    pub base_ref: String,
}

/// What a publish reads of a PR it recorded before it pushes to it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullText {
    pub state: PrState,
    pub title: String,
    pub body: String,
}

/// The PR a direct-pr publish opens: `head` is a branch of `repo` itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPull {
    pub repo: String,
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeRequest {
    pub repo: String,
    pub number: i64,
    /// The bound head: the forge refuses the merge when the PR head differs.
    pub sha: String,
    pub method: ApprovalAction,
}

/// A short-lived installation token of the daemon's merge App. Never
/// printed: `Debug` shows only that it is one.
#[derive(Clone, PartialEq, Eq)]
pub struct AppToken(String);

impl AppToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub(crate) fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AppToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AppToken(..)")
    }
}

/// Who the merge call acts as. On a project with `merge_gate = enforced` the
/// ruleset lets only the daemon's App through; elsewhere the owner's login
/// lands, the labelled `detect` fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Identity {
    Owner,
    App(AppToken),
}

/// What the merge call returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged {
        sha: String,
    },
    /// The forge answered and refused: the head moved, the PR is not
    /// mergeable, a rule blocked it.
    Refused(String),
    /// The call was sent and its result could not be read.
    Unknown(String),
}

pub trait Forge {
    fn pull(&self, repo: &str, number: i64) -> Result<PullView, ForgeError>;
    /// The combined checks of one commit.
    fn checks(&self, repo: &str, sha: &str) -> Result<Checks, ForgeError>;
    fn merge(&self, request: &MergeRequest, identity: &Identity) -> MergeOutcome;
    /// The open PR whose head is `branch` of `repo`, if there is one.
    fn find_pull(&self, repo: &str, branch: &str) -> Result<Option<PullRef>, ForgeError>;
    /// Opens a PR as the owner's login.
    fn open_pull(&self, pull: &NewPull) -> Result<PullRef, ForgeError>;
    /// One PR's state, title and body: a single call, unlike [`Forge::pull`].
    fn pull_text(&self, repo: &str, number: i64) -> Result<PullText, ForgeError>;
    /// Sets a PR's title and body as the owner's login.
    fn edit_pull(&self, repo: &str, number: i64, title: &str, body: &str)
    -> Result<(), ForgeError>;
}

/// The daemon's merge App: mints its installation token for one repo, and
/// tells the ruleset check who the App is and where it is installed.
pub trait AppTokens {
    fn mint(&self, repo: &str) -> Result<AppToken, ForgeError>;
    /// The App's id, the `actor_id` a ruleset names it by in its bypass list.
    fn app_id(&self) -> Result<u64, ForgeError>;
    /// Ok when the App is installed on `repo`.
    fn installed(&self, repo: &str) -> Result<(), ForgeError>;
}

/// The host has no merge App yet: every mint fails, so an enforced project
/// waits instead of landing as the owner. The ruleset check only records
/// `enforced` once the App exists, so this is the state of every host until
/// that setup step.
pub struct NoApp;

impl NoApp {
    fn missing(repo: &str) -> ForgeError {
        ForgeError::NoApp {
            repo: repo.into(),
            why: "no merge App is installed on this host".into(),
        }
    }
}

impl AppTokens for NoApp {
    fn mint(&self, repo: &str) -> Result<AppToken, ForgeError> {
        Err(Self::missing(repo))
    }

    fn app_id(&self) -> Result<u64, ForgeError> {
        Err(Self::missing("any repo"))
    }

    fn installed(&self, repo: &str) -> Result<(), ForgeError> {
        Err(Self::missing(repo))
    }
}

/// GitHub through `gh api`, as the owner's login unless an App token is given.
pub struct GhForge {
    program: String,
}

impl Default for GhForge {
    fn default() -> Self {
        Self {
            program: "gh".into(),
        }
    }
}

impl GhForge {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn api(&self, args: &[&str], identity: &Identity) -> std::io::Result<Output> {
        let mut command = Command::new(&self.program);
        command.arg("api").args(args);
        if let Identity::App(token) = identity {
            command.env("GH_TOKEN", token.secret());
        }
        command.env("GH_PROMPT_DISABLED", "1").output()
    }

    pub(crate) fn get(&self, args: &[&str]) -> Result<String, ForgeError> {
        let output = self
            .api(args, &Identity::Owner)
            .map_err(|error| ForgeError::Read(format!("run gh: {error}")))?;
        if !output.status.success() {
            return Err(ForgeError::Read(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// A write as the owner's login: `-f key=value` fields, so a value is
    /// always sent as a string.
    fn send(
        &self,
        method: &str,
        path: &str,
        fields: &[(&str, &str)],
    ) -> Result<Output, ForgeError> {
        let fields: Vec<String> = fields
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let mut args = vec!["-X", method, path];
        for field in &fields {
            args.extend(["-f", field.as_str()]);
        }
        let output = self
            .api(&args, &Identity::Owner)
            .map_err(|error| ForgeError::Write(format!("run gh: {error}")))?;
        if !output.status.success() {
            return Err(ForgeError::Write(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        Ok(output)
    }

    pub(crate) fn json(&self, path: &str) -> Result<Value, ForgeError> {
        serde_json::from_str(&self.get(&[path])?)
            .map_err(|error| ForgeError::Read(format!("{path}: {error}")))
    }

    /// Every page of a paginated endpoint, so a long list is never read as
    /// its first page only.
    pub(crate) fn pages(&self, path: &str) -> Result<Vec<Value>, ForgeError> {
        let text = self.get(&["--paginate", "--slurp", path])?;
        serde_json::from_str(&text).map_err(|error| ForgeError::Read(format!("{path}: {error}")))
    }
}

fn pull_ref(pull: &Value) -> Option<PullRef> {
    Some(PullRef {
        number: pull.get("number")?.as_i64()?,
        url: pull.get("html_url")?.as_str()?.to_owned(),
        head_sha: text(pull, "/head/sha"),
        base_ref: text(pull, "/base/ref"),
    })
}

fn pull_state(pull: &Value) -> PrState {
    if pull.get("merged").and_then(Value::as_bool) == Some(true) {
        PrState::Merged
    } else if text(pull, "/state") == "open" {
        PrState::Open
    } else {
        PrState::Closed
    }
}

fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// GitHub's `mergeable_state` in the cache's terms.
fn mergeable(state: &str) -> Mergeable {
    match state {
        "clean" | "unstable" | "has_hooks" => Mergeable::Clean,
        "dirty" => Mergeable::Dirty,
        "blocked" | "behind" | "draft" => Mergeable::Blocked,
        _ => Mergeable::Unknown,
    }
}

/// Folds check runs and commit statuses into one result: any failure fails,
/// then anything unfinished is pending, and `none` means the commit has no
/// checks at all.
pub fn combine(results: impl IntoIterator<Item = Checks>) -> Checks {
    let mut seen = Checks::None;
    for result in results {
        seen = match (seen, result) {
            (Checks::Failure, _) | (_, Checks::Failure) => Checks::Failure,
            (Checks::Pending, _) | (_, Checks::Pending) => Checks::Pending,
            (Checks::Success, _) | (_, Checks::Success) => Checks::Success,
            _ => Checks::None,
        };
    }
    seen
}

fn run_result(run: &Value) -> Checks {
    if run.get("status").and_then(Value::as_str) != Some("completed") {
        return Checks::Pending;
    }
    match run.get("conclusion").and_then(Value::as_str) {
        Some("success" | "neutral" | "skipped") => Checks::Success,
        _ => Checks::Failure,
    }
}

/// The checks on a commit from every page of its check runs and of its
/// combined status. A page count short of the reported total reads as
/// pending, never as a pass on what was seen.
fn page_checks(runs: &[Value], statuses: &[Value]) -> Checks {
    let items = |pages: &[Value], key: &str| -> Vec<Value> {
        pages
            .iter()
            .filter_map(|page| page.get(key).and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect()
    };
    let check_runs = items(runs, "check_runs");
    let reported = runs
        .iter()
        .filter_map(|page| page.get("total_count").and_then(Value::as_u64))
        .max()
        .unwrap_or(0);
    if (check_runs.len() as u64) < reported {
        return Checks::Pending;
    }
    let results = check_runs
        .iter()
        .map(run_result)
        .chain(items(statuses, "statuses").iter().map(status_result))
        .collect::<Vec<_>>();
    combine(results)
}

fn status_result(status: &Value) -> Checks {
    match status.get("state").and_then(Value::as_str) {
        Some("success") => Checks::Success,
        Some("pending") => Checks::Pending,
        _ => Checks::Failure,
    }
}

impl GhForge {
    /// One PR as [`Forge::pull`] reads it, with the PR's raw JSON for the
    /// fields a [`PullView`] does not carry (merge time, merger, visibility).
    pub(crate) fn pull_with_raw(
        &self,
        repo: &str,
        number: i64,
    ) -> Result<(PullView, Value), ForgeError> {
        let pull = self.json(&format!("repos/{repo}/pulls/{number}"))?;
        let state = pull_state(&pull);
        let head_sha = text(&pull, "/head/sha");
        let files = self
            .get(&[
                "--paginate",
                &format!("repos/{repo}/pulls/{number}/files?per_page=100"),
                "--jq",
                ".[].filename",
            ])?
            .lines()
            .map(str::to_owned)
            .collect();
        let view = PullView {
            state,
            checks: self.checks(repo, &head_sha)?,
            head_ref: text(&pull, "/head/ref"),
            base_ref: text(&pull, "/base/ref"),
            mergeable: mergeable(&text(&pull, "/mergeable_state")),
            body: text(&pull, "/body"),
            files,
            head_sha,
        };
        Ok((view, pull))
    }
}

impl Forge for GhForge {
    fn pull(&self, repo: &str, number: i64) -> Result<PullView, ForgeError> {
        self.pull_with_raw(repo, number).map(|(view, _)| view)
    }

    fn checks(&self, repo: &str, sha: &str) -> Result<Checks, ForgeError> {
        let runs = self.pages(&format!(
            "repos/{repo}/commits/{sha}/check-runs?per_page=100"
        ))?;
        let statuses = self.pages(&format!("repos/{repo}/commits/{sha}/status?per_page=100"))?;
        Ok(page_checks(&runs, &statuses))
    }

    fn merge(&self, request: &MergeRequest, identity: &Identity) -> MergeOutcome {
        let path = format!("repos/{}/pulls/{}/merge", request.repo, request.number);
        let sha = format!("sha={}", request.sha);
        let method = format!("merge_method={}", request.method);
        let output = match self.api(&["-X", "PUT", &path, "-f", &sha, "-f", &method], identity) {
            Ok(output) => output,
            // Nothing was sent.
            Err(error) => return MergeOutcome::Refused(format!("run gh: {error}")),
        };
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !output.status.success() {
            // GitHub answered with a refusal: 405 not mergeable, 409 head
            // moved, 422 invalid, 403 a rule. Anything else (a timeout, a 5xx,
            // no answer) leaves the result unknown.
            let refused = [
                "(HTTP 403)",
                "(HTTP 404)",
                "(HTTP 405)",
                "(HTTP 409)",
                "(HTTP 422)",
            ];
            return if refused.iter().any(|code| stderr.contains(code)) {
                MergeOutcome::Refused(stderr)
            } else {
                MergeOutcome::Unknown(stderr)
            };
        }
        let reply: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        match reply {
            Some(reply) if reply.get("merged").and_then(Value::as_bool) == Some(true) => {
                MergeOutcome::Merged {
                    sha: text(&reply, "/sha"),
                }
            }
            _ => MergeOutcome::Unknown(format!(
                "unreadable merge reply: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            )),
        }
    }

    fn find_pull(&self, repo: &str, branch: &str) -> Result<Option<PullRef>, ForgeError> {
        let owner = repo.split_once('/').map_or(repo, |(owner, _)| owner);
        let pages = self.pages(&format!(
            "repos/{repo}/pulls?state=open&head={owner}:{branch}&per_page=100"
        ))?;
        let pulls: Vec<&Value> = pages
            .iter()
            .filter_map(Value::as_array)
            .flatten()
            .filter(|pull| text(pull, "/head/ref") == branch)
            .collect();
        match pulls.as_slice() {
            [] => Ok(None),
            [pull] => pull_ref(pull)
                .map(Some)
                .ok_or_else(|| ForgeError::Read(format!("unreadable PR of {repo}:{branch}"))),
            more => Err(ForgeError::Read(format!(
                "{} open PRs have head {repo}:{branch}",
                more.len()
            ))),
        }
    }

    fn open_pull(&self, pull: &NewPull) -> Result<PullRef, ForgeError> {
        let output = self.send(
            "POST",
            &format!("repos/{}/pulls", pull.repo),
            &[
                ("title", &pull.title),
                ("body", &pull.body),
                ("head", &pull.head),
                ("base", &pull.base),
            ],
        )?;
        serde_json::from_slice::<Value>(&output.stdout)
            .ok()
            .as_ref()
            .and_then(pull_ref)
            .ok_or_else(|| {
                ForgeError::Write(format!(
                    "unreadable PR reply: {}",
                    String::from_utf8_lossy(&output.stdout).trim()
                ))
            })
    }
    fn pull_text(&self, repo: &str, number: i64) -> Result<PullText, ForgeError> {
        let pull = self.json(&format!("repos/{repo}/pulls/{number}"))?;
        Ok(PullText {
            state: pull_state(&pull),
            title: text(&pull, "/title"),
            body: text(&pull, "/body"),
        })
    }

    fn edit_pull(
        &self,
        repo: &str,
        number: i64,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.send(
            "PATCH",
            &format!("repos/{repo}/pulls/{number}"),
            &[("title", title), ("body", body)],
        )
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn run(conclusion: &str) -> Value {
        json!({"status": "completed", "conclusion": conclusion})
    }

    #[test]
    fn checks_read_every_page() {
        let first: Vec<Value> = (0..100).map(|_| run("success")).collect();
        let runs = [
            json!({"total_count": 101, "check_runs": first}),
            json!({"total_count": 101, "check_runs": [run("failure")]}),
        ];
        let statuses = [json!({"statuses": []})];
        // The failure is on the second page.
        assert_eq!(page_checks(&runs, &statuses), Checks::Failure);
        // A missing page is pending, not a pass on the first.
        assert_eq!(page_checks(&runs[..1], &statuses), Checks::Pending);
        let statuses = [
            json!({"statuses": [{"state": "success"}]}),
            json!({"statuses": [{"state": "pending"}]}),
        ];
        let green = [json!({"total_count": 1, "check_runs": [run("success")]})];
        assert_eq!(page_checks(&green, &statuses), Checks::Pending);
        assert_eq!(page_checks(&green, &statuses[..1]), Checks::Success);
        assert_eq!(
            page_checks(&[json!({"total_count": 0, "check_runs": []})], &[]),
            Checks::None
        );
    }
}
