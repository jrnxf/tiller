//! The memory view's wire shapes, for the contract. The core builds them;
//! the routes pass its output through these types (unknown fields refused),
//! so the contract cannot drift from what is served.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

/// One document in the list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryDocSummary {
    /// `fleet` for "All projects (shared)".
    pub project_id: String,
    pub name: String,
    pub lines: usize,
    /// Open agent proposals against it.
    pub suggestions: usize,
}

/// Where a line came from: the revision that last changed it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryProvenance {
    pub revision_id: String,
    /// The card's display id, when a card is behind it.
    pub card: Option<String>,
    /// `you`, `carried over`, `outside Tiller` or the proposing session.
    pub note: Option<String>,
    pub at: String,
}

/// An agent proposal shown inline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemorySuggestion {
    pub proposal_id: String,
    /// `add`, `rewrite` or `retire`.
    pub op: String,
    pub text: Option<String>,
    pub by: String,
    pub card: Option<String>,
    pub at: String,
    pub why: Option<String>,
    #[schema(value_type = Object)]
    pub token_budget_after: Option<Value>,
    /// The holdout gate's verdict on keeping it; Keep waits until it passes.
    pub gate: MemoryGate,
    /// The current text of the line a rewrite or removal names, while the
    /// file has it.
    pub target: Option<String>,
}

/// `learning.gate` on a proposal (learning-loop.md, section 7), with a
/// proposal made against another text refused as stale.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryGate {
    pub proposal: String,
    pub ok: bool,
    /// The failing step, when not ok.
    pub reason: Option<String>,
    /// The command that clears it.
    pub hint: Option<String>,
    pub change_set: Option<String>,
    pub eval_run: Option<String>,
    /// The set that is measuring and blocks every other, with cards to go.
    pub measuring: Option<MemoryMeasuring>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryMeasuring {
    pub change_set: String,
    pub name: String,
    pub cards_closed: i64,
    pub window_cards: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryLine {
    /// Its position in the section, as edits name it.
    pub index: usize,
    pub text: String,
    pub memory_id: Option<String>,
    pub provenance: Option<MemoryProvenance>,
    /// A rewrite or removal of this line.
    pub suggestion: Option<MemorySuggestion>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemorySection {
    pub title: String,
    /// `rule`, `note` or `preference`.
    pub class: String,
    pub lines: Vec<MemoryLine>,
    /// New lines suggested for the section, and suggestions whose line is
    /// gone.
    pub suggestions: Vec<MemorySuggestion>,
}

/// One entry in "Recent changes".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryChange {
    pub proposal_id: String,
    /// `added`, `kept`, `edit`, `remove` or `discard`.
    pub kind: String,
    pub text: Option<String>,
    pub from: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryDocument {
    pub project_id: String,
    pub name: String,
    pub path: String,
    /// The blob at `HEAD`; every change sends it back as `base_blob`.
    pub blob: String,
    /// The working tree differs from `HEAD`: changes are refused until it is
    /// committed or reset.
    pub dirty: bool,
    pub sections: Vec<MemorySection>,
    pub changes: Vec<MemoryChange>,
}

/// `GET /api/v1/memory`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryResponse {
    /// Fleet first.
    pub docs: Vec<MemoryDocSummary>,
    pub doc: MemoryDocument,
}

/// A session that loaded a revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryLoad {
    pub session_id: String,
    pub role: String,
    pub project_id: String,
    pub run_id: Option<String>,
    pub card: Option<String>,
    pub trigger: String,
    pub loaded_at: String,
}

/// `GET /api/v1/memory/revisions/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryRevision {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub blob_sha: String,
    pub commit_sha: Option<String>,
    /// `approved`, `unreviewed` or `revoked`.
    pub review: String,
    pub provenance: String,
    pub recorded_at: String,
    /// The approval row that applied it.
    #[schema(value_type = Object)]
    pub approval: Option<Value>,
    pub proposal_id: Option<String>,
    pub card: Option<String>,
    pub sessions: Vec<MemoryLoad>,
    /// The file's text at this revision; absent once the blob is gone.
    pub text: Option<String>,
}

/// What one change came to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MemoryLanded {
    pub proposal_id: String,
    /// Absent for a Discard.
    pub approval_id: Option<String>,
    /// The blob at `HEAD` after it.
    pub blob: String,
}
