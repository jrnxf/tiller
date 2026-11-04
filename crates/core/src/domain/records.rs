//! One struct per `state.db` table, field for field (`schema.sql`). These are
//! what reads return and what mutations hand back, and they serialize to the
//! JSON contract the CLI and the board API print.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::enums::*;
use crate::time::Timestamp;

/// A row of one table, read by column name.
pub trait Record: Sized {
    const TABLE: &'static str;
    const FIELDS: &'static [&'static str];

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self>;

    /// `SELECT <every field> FROM <table>`, ready for a `WHERE`.
    fn select() -> String {
        format!("SELECT {} FROM {}", Self::FIELDS.join(", "), Self::TABLE)
    }
}

macro_rules! record {
    (
        $(#[$meta:meta])*
        $name:ident in $table:literal {
            $($(#[$field_meta:meta])* $field:ident: $ty:ty),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            $($(#[$field_meta])* pub $field: $ty),+
        }

        impl Record for $name {
            const TABLE: &'static str = $table;
            const FIELDS: &'static [&'static str] = &[$(stringify!($field)),+];

            fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
                Ok(Self {
                    $($field: row.get(stringify!($field))?),+
                })
            }
        }
    };
}

record!(Host in "hosts" {
    id: String,
    label: String,
    kind: HostKind,
    herdr_machine: Option<String>,
    lane_capacity: Option<i64>,
    ram_floor_mb: Option<i64>,
    disk_floor_mb: Option<i64>,
    last_seen_at: Option<Timestamp>,
    stale_reason: Option<String>,
    peer_scope: Option<PeerScope>,
    peer_host_id: Option<String>,
    rev: i64,
});

record!(
    /// On a peer, one board host it accepted (ADR 0003). The public half
    /// only; the private half is a file on the board host.
    PeerKey in "peer_keys" {
        id: String,
        viewer_label: String,
        viewer_host_id: String,
        public_key: String,
        scope: PeerScope,
        accepted_at: Timestamp,
        revoked_at: Option<Timestamp>,
        rev: i64,
    }
);

record!(Project in "projects" {
    id: String,
    name: String,
    repo: Option<String>,
    remote: Option<String>,
    default_branch: String,
    delivery: Delivery,
    checks: Value,
    capacity: i64,
    deploy_workflow: Option<String>,
    deploy_environment: Option<String>,
    deploy_timeout_minutes: i64,
    merge_gate: MergeGate,
    owner_host_id: String,
    added_at: Timestamp,
    next_card_seq: i64,
    retired_at: Option<Timestamp>,
    rev: i64,
    stage: Stage,
    reviewed_through_sha: Option<String>,
    stage_changed_at: Option<Timestamp>,
});

impl Project {
    /// The delivery mode builders and the land loop follow
    /// ([`Stage::effective_delivery`]).
    pub fn effective_delivery(&self) -> Delivery {
        self.stage.effective_delivery(self.delivery)
    }
}

record!(Card in "cards" {
    id: String,
    display_id: String,
    legacy_id: Option<String>,
    project_id: String,
    kind: CardKind,
    title: String,
    body: Option<String>,
    priority: i64,
    category: Option<String>,
    size: Option<Size>,
    acceptance: Option<String>,
    fixes_card_id: Option<String>,
    dispatch_override: Option<Value>,
    state: CardState,
    outcome: Option<CardOutcome>,
    closed_at: Option<Timestamp>,
    closed_by: Option<String>,
    created_at: Timestamp,
    created_by_ask_id: Option<String>,
    rev: i64,
});

record!(Session in "sessions" {
    id: String,
    claude_session_id: Option<String>,
    role: Role,
    project_id: String,
    run_id: Option<String>,
    host_id: String,
    pane_id: Option<String>,
    transcript_path: Option<String>,
    harness: String,
    config_hash: Option<String>,
    started_at: Timestamp,
    ended_at: Option<Timestamp>,
    end_reason: Option<String>,
    activity: Activity,
    activity_observed_at: Option<Timestamp>,
    activity_source: Option<ObservationSource>,
    rev: i64,
});

record!(Ask in "asks" {
    id: String,
    project_id: String,
    session_id: Option<String>,
    source: AskSource,
    text: String,
    received_at: Timestamp,
    disposition: Disposition,
    resolved_at: Option<Timestamp>,
    resolved_by_session_id: Option<String>,
});

record!(AskCard in "ask_cards" {
    ask_id: String,
    card_id: String,
    relation: LinkRelation,
    linked_at: Timestamp,
    linked_by_session_id: Option<String>,
});

record!(CardDep in "card_deps" {
    card_id: String,
    depends_on_card_id: String,
    added_at: Timestamp,
    added_by: String,
    released_at: Option<Timestamp>,
    release_reason: Option<DepReleaseReason>,
});

record!(Hold in "holds" {
    id: String,
    card_id: String,
    until_at: Timestamp,
    reason: String,
    set_at: Timestamp,
    set_by: String,
    released_at: Option<Timestamp>,
    release_reason: Option<HoldReleaseReason>,
});

record!(Attention in "attention" {
    id: String,
    card_id: String,
    kind: AttentionKind,
    question: String,
    evidence: Option<String>,
    recommendation: Option<String>,
    consequence: Option<String>,
    options: Option<Value>,
    opened_at: Timestamp,
    opened_by_session_id: Option<String>,
    opened_by_run_id: Option<String>,
    answered_at: Option<Timestamp>,
    answer: Option<String>,
    answered_via: Option<AnsweredVia>,
    answer_idempotency_key: Option<String>,
    withdrawn_at: Option<Timestamp>,
    withdraw_reason: Option<String>,
    rev: i64,
});

record!(Run in "runs" {
    id: String,
    card_id: String,
    role: RunRole,
    attempt: i64,
    predecessor_run_id: Option<String>,
    model: String,
    effort: Effort,
    host_id: String,
    worktree_path: Option<String>,
    branch: Option<String>,
    base_branch: Option<String>,
    base_sha: Option<String>,
    supervisor_session_id: String,
    started_at: Timestamp,
    ended_at: Option<Timestamp>,
    outcome: Option<RunOutcome>,
    outcome_note: Option<String>,
    pause_until: Option<Timestamp>,
    last_status_line: Option<String>,
    last_status_at: Option<Timestamp>,
    config_hash: Option<String>,
    idempotency_key: Option<String>,
    rev: i64,
});

record!(Pr in "prs" {
    id: String,
    project_id: String,
    card_id: String,
    run_id: Option<String>,
    number: i64,
    url: String,
    linked_at: Timestamp,
    linked_by: String,
    state: PrState,
    head_sha: Option<String>,
    base_ref: Option<String>,
    checks: Checks,
    review_decision: ReviewDecision,
    mergeable: Mergeable,
    merged_at: Option<Timestamp>,
    merge_sha: Option<String>,
    fetched_at: Option<Timestamp>,
    fetch_error: Option<String>,
    pipeline_state: PipelineState,
});

record!(Grant in "grants" {
    id: String,
    project_id: String,
    kind: GrantKind,
    conditions: Value,
    granted_at: Timestamp,
    granted_by: String,
    revoked_at: Option<Timestamp>,
    revoke_reason: Option<String>,
});

record!(Approval in "approvals" {
    id: String,
    kind: ApprovalKind,
    project_id: String,
    card_id: Option<String>,
    run_id: Option<String>,
    repo: Option<String>,
    pr_id: Option<String>,
    branch: Option<String>,
    head_sha: Option<String>,
    diff_sha256: Option<String>,
    base_rev: Option<String>,
    scope: Option<Value>,
    memory_paths: Option<Value>,
    linked_approval_id: Option<String>,
    action: ApprovalAction,
    state: ApprovalState,
    requested_at: Timestamp,
    requested_by: String,
    decided_at: Option<Timestamp>,
    decided_by: Option<String>,
    decision_idempotency_key: Option<String>,
    grant_id: Option<String>,
    executed_at: Option<Timestamp>,
    finished_at: Option<Timestamp>,
    result: Option<Value>,
    rev: i64,
});

record!(Reservation in "reservations" {
    id: String,
    host_id: String,
    project_id: String,
    run_id: String,
    reserved_at: Timestamp,
    reserved_by: String,
    released_at: Option<Timestamp>,
    release_reason: Option<ReservationRelease>,
});

record!(OutboxMessage in "outbox" {
    id: String,
    target_session_id: String,
    target_pane_id: Option<String>,
    kind: OutboxKind,
    body: String,
    card_id: Option<String>,
    reply_to: Option<String>,
    idempotency_key: Option<String>,
    state: OutboxState,
    attempts: i64,
    next_attempt_at: Option<Timestamp>,
    created_at: Timestamp,
    created_by: String,
    delivered_at: Option<Timestamp>,
    acked_at: Option<Timestamp>,
    failed_at: Option<Timestamp>,
    failure_reason: Option<String>,
    rev: i64,
});

record!(Observation in "observations" {
    id: String,
    session_id: Option<String>,
    run_id: Option<String>,
    source: ObservationSource,
    event: String,
    verdict: Option<Activity>,
    observed_at: Timestamp,
    recorded_at: Timestamp,
    payload: Value,
});

record!(Event in "events" {
    seq: i64,
    id: String,
    at: Timestamp,
    txn: String,
    actor: String,
    entity: String,
    entity_id: String,
    kind: String,
    rev_after: Option<i64>,
    card_id: Option<String>,
    payload: Value,
    idempotency_key: Option<String>,
});

record!(Obligation in "obligations" {
    id: String,
    project_id: String,
    card_id: Option<String>,
    what: String,
    trigger_kind: TriggerKind,
    trigger_event: Option<Value>,
    due_at: Option<Timestamp>,
    state: ObligationState,
    opened_at: Timestamp,
    opened_by: String,
    met_at: Option<Timestamp>,
    met_by_event_seq: Option<i64>,
});

record!(Artifact in "artifacts" {
    id: String,
    card_id: String,
    run_id: Option<String>,
    kind: ArtifactKind,
    title: String,
    url: Option<String>,
    path: Option<String>,
    created_at: Timestamp,
    created_by_session_id: Option<String>,
    retired_at: Option<Timestamp>,
});

record!(Review in "reviews" {
    id: String,
    run_id: String,
    card_id: String,
    reviewer: Reviewer,
    coverage: Coverage,
    findings_high: i64,
    findings_medium: i64,
    findings_low: i64,
    verdict: Verdict,
    recorded_at: Timestamp,
    source_url: Option<String>,
});

record!(ReviewPass in "review_passes" {
    id: String,
    project_id: String,
    card_id: String,
    kind: ReviewPassKind,
    base_sha: String,
    head_sha: Option<String>,
    chunks: Value,
    state: ReviewPassState,
    report_artifact_id: Option<String>,
    fix_card_ids: Value,
    obligation_id: Option<String>,
    created_at: Timestamp,
    started_at: Option<Timestamp>,
    finished_at: Option<Timestamp>,
    rev: i64,
});

record!(Handoff in "handoffs" {
    id: String,
    project_id: String,
    session_id: String,
    written_at: Timestamp,
    active_cards: Value,
    pending_attention: Value,
    workers: Value,
    conclusions: String,
    next_action: String,
});

record!(Pane in "panes" {
    pane_id: String,
    host_id: String,
    workspace_label: Option<String>,
    agent_name: Option<String>,
    agent_status: Option<String>,
    session_id: Option<String>,
    metadata: Value,
    fetched_at: Timestamp,
    gone_at: Option<Timestamp>,
});

record!(Worktree in "worktrees" {
    path: String,
    host_id: String,
    project_id: Option<String>,
    run_id: Option<String>,
    branch: Option<String>,
    head_sha: Option<String>,
    dirty: Option<bool>,
    unlanded_commits: Option<i64>,
    fetched_at: Timestamp,
    gone_at: Option<Timestamp>,
});

record!(QuotaWindow in "quota_windows" {
    label: String,
    percent_remaining: Option<f64>,
    resets_at: Option<Timestamp>,
    pace: Option<String>,
    reserve: Option<f64>,
    burn_multiple: Option<f64>,
    elapsed_percent: Option<f64>,
    projected_exhaustion_at: Option<Timestamp>,
    title: Option<String>,
    kind: Option<String>,
    fetched_at: Timestamp,
    fetch_error: Option<String>,
});

/// One row of the `card_column` view: a card's derived column and badge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardColumn {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub kind: CardKind,
    pub column: Column,
    pub badge: String,
    pub parked_until: Option<Timestamp>,
    pub parked_reason: Option<String>,
    pub blocked_by: Option<String>,
    pub closed_at: Option<Timestamp>,
}

impl CardColumn {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            display_id: row.get("display_id")?,
            project_id: row.get("project_id")?,
            kind: row.get("kind")?,
            column: row.get("column")?,
            badge: row.get("badge")?,
            parked_until: row.get("parked_until")?,
            parked_reason: row.get("parked_reason")?,
            blocked_by: row.get("blocked_by")?,
            closed_at: row.get("closed_at")?,
        })
    }
}

record!(
    /// One retro of one closed card (docs/design/learning-loop.md, section 3).
    Retro in "retros" {
        id: String,
        card_id: String,
        project_id: String,
        closed_at: Timestamp,
        trigger: Option<String>,
        depth: RetroDepth,
        reason: RetroReason,
        counts: Value,
        config_hashes: Value,
        state: RetroState,
        outcome: Option<RetroOutcome>,
        learning: Option<String>,
        evidence_id: Option<String>,
        proposal_id: Option<String>,
        curator_session: Option<String>,
        curator_started_at: Option<Timestamp>,
        finished_at: Option<Timestamp>,
        finished_by: Option<String>,
        created_at: Timestamp,
        rev: i64,
    }
);

record!(
    /// One change, or a named bundle, measured as one (learning-loop.md,
    /// section 6).
    ChangeSet in "change_sets" {
        id: String,
        name: String,
        state: ChangeSetState,
        window_cards: i64,
        opened_at: Timestamp,
        opened_by: String,
        measuring_from: Option<Timestamp>,
        closed_at: Option<Timestamp>,
        close_reason: Option<ChangeSetClose>,
        rev: i64,
    }
);

record!(ChangeSetMember in "change_set_members" {
    change_set_id: String,
    proposal_id: String,
    added_at: Timestamp,
});

record!(
    /// One holdout run recorded for a change set (learning-loop.md, section 7).
    EvalRun in "eval_runs" {
        id: String,
        change_set_id: String,
        split: String,
        members_sha256: String,
        candidate_sha256: Option<String>,
        cases_total: i64,
        cases_passed: i64,
        overall_score: Option<f64>,
        cost_usd: Option<f64>,
        partial: i64,
        passed: i64,
        claude_version: Option<String>,
        judge_model: Option<String>,
        result_path: Option<String>,
        ran_at: Timestamp,
        recorded_by: String,
    }
);

record!(
    /// The parts behind one `config_hash` (learning-loop.md, section 5).
    ConfigManifest in "config_manifests" {
        config_hash: String,
        parts: Value,
        first_seen_at: Timestamp,
    }
);

/// One row of the `capacity` view: free run slots on a local host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostCapacity {
    pub host_id: String,
    pub label: String,
    pub lane_capacity: Option<i64>,
    pub reserved: i64,
    pub free: Option<i64>,
}

impl HostCapacity {
    pub fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            host_id: row.get("host_id")?,
            label: row.get("label")?,
            lane_capacity: row.get("lane_capacity")?,
            reserved: row.get("reserved")?,
            free: row.get("free")?,
        })
    }
}
