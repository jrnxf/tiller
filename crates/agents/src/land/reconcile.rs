//! The forge reconcile reads through (`recovery.md` R07, R08, R19): the same
//! GitHub the publish and land paths use, as the owner's `gh` login, so a PR
//! merged while the daemon was down is seen as merged after a restart.
//!
//! Reads only. The body scrub (R19) runs here against the host's scrub list
//! and the project's asks, the way land step 4 does.

use serde_json::Value;
use tiller_core::Timestamp;
use tiller_core::domain::{PipelineState, Project, ReviewDecision};
use tiller_core::state::approvals::PrFetch;
use tiller_reconcile::sources::{Forge, PrRead};

use super::forge::{GhForge, PullView};
use super::scrub::{ScrubList, scrub};

/// What the read needs from `state.db` and the host that GitHub does not
/// know.
pub trait Local: Send {
    /// The cached pipeline state of the PR: no forge reports it, so a read
    /// keeps what the record says.
    fn pipeline_state(&self, repo: &str, number: i64) -> PipelineState;
    /// The asks of the project that owns `repo` (R19's quote rule).
    fn asks(&self, repo: &str) -> Vec<String>;
    /// The host's scrub list, read fresh each time.
    fn scrub_list(&self) -> ScrubList;
}

pub struct ReconcileForge {
    gh: GhForge,
    local: Box<dyn Local>,
}

impl ReconcileForge {
    pub fn new(program: impl Into<String>, local: Box<dyn Local>) -> Self {
        Self {
            gh: GhForge::new(program),
            local,
        }
    }

    /// The lines of `body` that fail the scrub. A failure no single line
    /// shows, such as a quote that spans lines, comes back as its reason.
    fn leaks(&self, repo: &str, body: &str, pipeline: PipelineState) -> Vec<String> {
        let asks = self.local.asks(repo);
        let list = self.local.scrub_list();
        let Some(reason) = scrub(body, &asks, &list, pipeline) else {
            return Vec::new();
        };
        let lines: Vec<String> = body
            .lines()
            .filter(|line| scrub(line, &asks, &list, pipeline).is_some())
            .map(str::to_owned)
            .collect();
        if lines.is_empty() {
            vec![reason]
        } else {
            lines
        }
    }

    /// GitHub's review verdict from the latest review of each reviewer: any
    /// standing change request wins, then an approval.
    fn review_decision(&self, repo: &str, number: i64) -> Result<ReviewDecision, String> {
        let pages = self
            .gh
            .pages(&format!("repos/{repo}/pulls/{number}/reviews?per_page=100"))
            .map_err(|error| error.to_string())?;
        let mut latest: Vec<(String, String)> = Vec::new();
        for review in pages.iter().filter_map(Value::as_array).flatten() {
            let state = review.pointer("/state").and_then(Value::as_str);
            let user = review.pointer("/user/login").and_then(Value::as_str);
            let (Some(state), Some(user)) = (state, user) else {
                continue;
            };
            if !matches!(state, "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED") {
                continue;
            }
            latest.retain(|(who, _)| who != user);
            latest.push((user.to_owned(), state.to_owned()));
        }
        let has = |wanted: &str| latest.iter().any(|(_, state)| state == wanted);
        Ok(if has("CHANGES_REQUESTED") {
            ReviewDecision::ChangesRequested
        } else if has("APPROVED") {
            ReviewDecision::Approved
        } else {
            ReviewDecision::None
        })
    }

    fn repo_of(project: &Project) -> Result<&str, String> {
        project
            .repo
            .as_deref()
            .ok_or_else(|| format!("project {} has no repo", project.id))
    }
}

fn raw_text(raw: &Value, pointer: &str) -> Option<String> {
    raw.pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

impl Forge for ReconcileForge {
    fn pr(&self, repo: &str, number: i64) -> Result<PrRead, String> {
        let (view, raw): (PullView, Value) = self
            .gh
            .pull_with_raw(repo, number)
            .map_err(|error| error.to_string())?;
        let pipeline_state = self.local.pipeline_state(repo, number);
        let merged = view.state == tiller_core::domain::PrState::Merged;
        let fetch = PrFetch {
            state: view.state,
            head_sha: Some(view.head_sha.clone()).filter(|sha| !sha.is_empty()),
            base_ref: Some(view.base_ref.clone()).filter(|base| !base.is_empty()),
            checks: view.checks,
            review_decision: self.review_decision(repo, number)?,
            mergeable: view.mergeable,
            merged_at: raw_text(&raw, "/merged_at").and_then(|at| at.parse::<Timestamp>().ok()),
            merge_sha: raw_text(&raw, "/merge_commit_sha").filter(|_| merged),
            pipeline_state,
            fetch_error: None,
        };
        Ok(PrRead {
            fetch,
            body_leaks: self.leaks(repo, &view.body, pipeline_state),
            repo_public: raw
                .pointer("/base/repo/private")
                .and_then(Value::as_bool)
                .map(|private| !private),
            merged_by: raw_text(&raw, "/merged_by/login"),
            head_ref: Some(view.head_ref).filter(|name| !name.is_empty()),
            changed_paths: Some(view.files),
        })
    }

    fn default_branch_head(&self, project: &Project) -> Result<String, String> {
        let repo = Self::repo_of(project)?;
        let commit = self
            .gh
            .json(&format!("repos/{repo}/commits/{}", project.default_branch))
            .map_err(|error| error.to_string())?;
        raw_text(&commit, "/sha").ok_or_else(|| format!("no head for {repo}"))
    }

    fn is_ancestor(
        &self,
        project: &Project,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, String> {
        let repo = Self::repo_of(project)?;
        let compare = self
            .gh
            .json(&format!("repos/{repo}/compare/{ancestor}...{descendant}"))
            .map_err(|error| error.to_string())?;
        match compare.pointer("/status").and_then(Value::as_str) {
            Some("identical" | "ahead") => Ok(true),
            Some("behind" | "diverged") => Ok(false),
            other => Err(format!("unreadable comparison of {repo}: {other:?}")),
        }
    }
}
