//! The memory view's wire shapes, for the contract (`memory.md` section 4,
//! API).

use serde::{Deserialize, Serialize};
use tiller_core::domain::PendingNote;
use tiller_core::kb::{Entry, KbState};
use utoipa::ToSchema;

/// The kb store as the view read it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryKb {
    /// `ok`, `missing` or `unreadable`.
    pub state: String,
    /// Where the store is: `KB_HOME`, else `.kb` in the home folder; absent when neither is
    /// known.
    pub home: Option<String>,
    /// The kb scopes a launch in the project loads, in load order; empty
    /// without a project.
    pub scopes: Vec<String>,
}

/// The role's startup budget: what the role file and the loaded notes take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryBudget {
    pub used: i64,
    pub limit: i64,
}

/// One kb note a launch would load now, or skip and why.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryLive {
    pub id: String,
    pub title: String,
    /// Absent for a note that did not parse.
    pub kind: Option<String>,
    pub scope: String,
    pub tokens: i64,
    pub loaded: bool,
    /// `over budget` or `unreadable`, for a skipped note.
    pub reason: Option<String>,
}

/// One note an agent suggested, waiting for the owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryPending {
    pub id: String,
    pub project_id: String,
    /// `convention`, `decision`, `gotcha` or `how-to`.
    pub kind: String,
    pub title: String,
    pub body: String,
    /// `fleet`, `curator` or `migration`.
    pub source: String,
    /// The fleet role that suggested it.
    pub role: Option<String>,
    pub card_id: Option<String>,
    pub run_id: Option<String>,
    pub retro_id: Option<String>,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub rev: i64,
}

impl From<PendingNote> for MemoryPending {
    fn from(note: PendingNote) -> Self {
        Self {
            id: note.id,
            project_id: note.project_id,
            kind: note.kind,
            title: note.title,
            body: note.body,
            source: note.source,
            role: note.role,
            card_id: note.card_id,
            run_id: note.run_id,
            retro_id: note.retro_id,
            created_at: note.created_at.to_string(),
            edited_at: note.edited_at.map(|at| at.to_string()),
            rev: note.rev,
        }
    }
}

impl From<&Entry> for MemoryLive {
    fn from(entry: &Entry) -> Self {
        Self {
            id: entry.id.clone(),
            title: entry.title.clone(),
            kind: entry.kind.clone(),
            scope: entry.scope.clone(),
            tokens: entry.tokens,
            loaded: entry.loaded,
            reason: entry.reason.clone(),
        }
    }
}

/// `GET /api/v1/memory`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryResponse {
    /// Absent when no project was asked for.
    pub project_id: Option<String>,
    pub role: String,
    pub kb: MemoryKb,
    pub budget: MemoryBudget,
    /// What a launch of `role` in the project would load now, skipped notes
    /// with `loaded: false` and the reason; empty without a project.
    pub live: Vec<MemoryLive>,
    /// The project's pending notes, or every project's without one, oldest
    /// first.
    pub pending: Vec<MemoryPending>,
}

/// What Keep wrote to kb.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryKept {
    pub note_id: String,
    pub scope: String,
}

/// The kb state's wire name.
pub fn state_name(state: KbState) -> String {
    state.as_str().to_owned()
}
