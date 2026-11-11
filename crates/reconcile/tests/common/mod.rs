//! The fakes and the test world, compiled from `src/testing.rs`, plus the
//! small builders the end-to-end tests share.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

#[path = "../../src/testing.rs"]
pub mod testing;

pub use testing::*;
pub use tiller_core::Timestamp;
use tiller_core::domain::{
    AnsweredVia, ApprovalAction, ApprovalKind, Checks, Mergeable, PipelineState, PrState,
    ReviewDecision,
};
use tiller_core::state::approvals::{Decision, NewApproval, NewPr, PrFetch};
use tiller_core::{Actor, Ctx};
use tiller_reconcile::sources::PrRead;

pub const REPO: &str = "example/alpha";

/// A forge read of an open PR at `head`, checks green, body clean.
pub fn open_pr(head: &str) -> PrRead {
    PrRead {
        fetch: PrFetch {
            state: PrState::Open,
            head_sha: Some(head.into()),
            base_ref: Some("main".into()),
            checks: Checks::Success,
            review_decision: ReviewDecision::None,
            mergeable: Mergeable::Clean,
            merged_at: None,
            merge_sha: None,
            pipeline_state: PipelineState::Passed,
            fetch_error: None,
        },
        body_leaks: Vec::new(),
        repo_public: Some(false),
        merged_by: None,
        head_ref: Some("fm/work".into()),
        changed_paths: Some(vec!["src/lib.rs".into()]),
    }
}

/// A forge read of a PR merged at `head` by `by`.
pub fn merged_pr(head: &str, merge_sha: &str, at: Timestamp) -> PrRead {
    let mut read = open_pr(head);
    read.merged_by = Some("someone-else".into());
    read.fetch.state = PrState::Merged;
    read.fetch.merged_at = Some(at);
    read.fetch.merge_sha = Some(merge_sha.into());
    read
}

pub fn closed_pr(head: &str) -> PrRead {
    let mut read = open_pr(head);
    read.fetch.state = PrState::Closed;
    read
}

pub fn daemon() -> Ctx {
    Ctx::new(Actor::Daemon)
}

pub fn session_ctx(id: &str) -> Ctx {
    Ctx::new(Actor::Session(id.to_owned()))
}

/// Stores `key` in `idempotency_keys` as `actor` stored it (`None`: a key
/// from before schema 15, which records no actor), as a session that ran
/// any mutation under the pass's key first would.
pub fn plant_key(w: &World, key: &str, actor: Option<&str>) {
    w.store()
        .conn()
        .execute(
            "INSERT INTO idempotency_keys (key, command, entity, entity_id, response, created_at, actor)
             VALUES (?1, 'guard.breach', 'card', 'x', '{}', ?2, ?3)",
            rusqlite::params![key, now(), actor],
        )
        .unwrap();
}

impl World {
    /// Links PR `number` at `head` to the worker's card and run. Returns the
    /// PR id.
    pub fn link_pr(&mut self, worker: &str, number: i64, head: &str) -> String {
        let run = self.run_of(worker);
        self.store_mut()
            .pr_link(
                &owner(),
                NewPr {
                    card: run.card_id.clone(),
                    run_id: Some(run.id.clone()),
                    number,
                    url: format!("https://github.com/{REPO}/pull/{number}"),
                    head_sha: Some(head.into()),
                    base_ref: Some("main".into()),
                },
            )
            .unwrap()
            .value
            .id
    }

    /// A merge approval for the PR at `head`, requested by the orchestrator;
    /// accepted by the owner when `accept`. Returns the approval id.
    pub fn approve(&mut self, worker: &str, pr_id: &str, head: &str, accept: bool) -> String {
        let run = self.run_of(worker);
        let orchestrator = self.fleet.orchestrator_session.clone();
        let approval = self
            .store_mut()
            .approval_request(
                &session_ctx(&orchestrator),
                NewApproval {
                    kind: ApprovalKind::Merge,
                    card: run.card_id.clone(),
                    run_id: Some(run.id.clone()),
                    pr_id: Some(pr_id.to_owned()),
                    branch: None,
                    head_sha: head.into(),
                    action: ApprovalAction::Squash,
                    memory_paths: None,
                    grant_id: None,
                },
            )
            .unwrap()
            .value;
        if accept {
            self.store_mut()
                .approval_decide(
                    &owner(),
                    None,
                    &approval.id,
                    Decision {
                        accept: true,
                        via: AnsweredVia::Cli,
                        memory: Vec::new(),
                        ask_id: None,
                    },
                )
                .unwrap();
        }
        approval.id
    }

    /// The PR cache row.
    pub fn pr(&self, id: &str) -> tiller_core::domain::Pr {
        tiller_core::state::get(self.store().conn(), "pr", id).unwrap()
    }

    pub fn approval(&self, id: &str) -> tiller_core::domain::Approval {
        tiller_core::state::get(self.store().conn(), "approval", id).unwrap()
    }

    pub fn card(&self, id: &str) -> tiller_core::domain::Card {
        tiller_core::state::get(self.store().conn(), "card", id).unwrap()
    }

    /// `guard.breached` events for `guard`, oldest first, as payloads.
    pub fn breaches(&self, guard: &str) -> Vec<serde_json::Value> {
        let mut stmt = self
            .store()
            .conn()
            .prepare("SELECT payload FROM events WHERE kind = 'guard.breached' AND entity_id = ?1 ORDER BY seq")
            .unwrap();
        stmt.query_map([guard], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}
