//! The memory view's routes (T7.2; `memory.md` sections 6 and 7,
//! `protocol.md` section 7.3): the documents, owner edits, Keep and Discard,
//! Undo, and the revision behind a provenance hint.
//!
//! Every change runs under the memory repo's lock: the file is read, the
//! core records the owner-decided proposal and its approval, and `mem.apply`
//! commits it at once. None of these routes is on the forwarding allowlist:
//! memory changes only on the host that owns it.

pub mod apply;
pub mod dto;
pub mod scrub;

use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_agents::land::scrub::ScrubList;
use tiller_core::domain::{Approval, ApprovalState};
use tiller_core::memory::{document_path, unified_diff};
use tiller_core::state::get;
use tiller_core::state::memory::{Begin, EditOp, Outcome, Pending};
use tiller_core::{Actor, Applied, Ctx, Error, Store, Timestamp, ids};
use utoipa::{IntoParams, ToSchema};

use self::apply::{Lock, Repo};
use self::dto::{MemoryLanded, MemoryResponse, MemoryRevision};
use crate::api::{
    ApiError, ApiResult, AppState, body, guard, key, not_forwarded, read, read_only, uninitialized,
};
use crate::dto::{ErrorBody, MutationResponse, Receipt};

/// The most decisions one batch carries.
const BATCH: usize = 50;

#[derive(Clone, Debug, Deserialize, IntoParams)]
pub struct MemoryQuery {
    /// The document to show: a project id, or `fleet` (the default) for
    /// "All projects (shared)".
    #[serde(default)]
    pub scope: Option<String>,
}

/// `POST /api/v1/memory/edits`: one owner edit, committed at once.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryEdit {
    pub idempotency_key: String,
    /// Refused unless absent or this host: memory is never forwarded.
    #[serde(default)]
    pub host: Option<String>,
    pub project: String,
    /// `add`, `rewrite` or `remove`.
    pub op: String,
    pub section: String,
    /// The line's position in its section, for `rewrite` and `remove`.
    #[serde(default)]
    pub index: Option<usize>,
    /// The new text, for `add` and `rewrite`.
    #[serde(default)]
    pub text: Option<String>,
    /// The blob the board showed; another is 422.
    pub base_blob: String,
}

/// One Keep or Discard.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryDecision {
    pub proposal: String,
    /// `keep` or `discard`.
    pub decision: String,
    /// The blob the board showed the proposal against.
    pub base_blob: String,
}

/// `POST /api/v1/memory/proposals/decide`: Keep or Discard on agent
/// proposals, each with its own approval row, applied in order.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryDecide {
    pub idempotency_key: String,
    #[serde(default)]
    pub host: Option<String>,
    pub decisions: Vec<MemoryDecision>,
}

/// `POST /api/v1/memory/changes/{proposal}/undo`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryUndo {
    pub idempotency_key: String,
    #[serde(default)]
    pub host: Option<String>,
    /// The blob the board showed.
    pub base_blob: String,
}

/// Everything a memory write runs with.
struct Writer {
    store: Store,
    repo: Repo,
    lock: Lock,
    list: ScrubList,
    events: Vec<i64>,
    replayed: bool,
}

impl Writer {
    fn track<T>(&mut self, applied: Applied<T>) -> T {
        self.events.extend(applied.events);
        self.replayed &= applied.replayed;
        applied.value
    }

    /// `mem.apply` for an accepted approval: the base check, the commit and
    /// the rows, all under the lock. A commit that fails (the scrub, a diff
    /// that does not apply) marks the approval and proposal `failed` and is
    /// refused with the finding. A retry of a write that did not land (a
    /// replayed key) is refused the same way, never answered as landed.
    fn apply(&mut self, approval: &str, path: &str) -> tiller_core::Result<()> {
        let row: Approval = get(self.store.conn(), "approval", approval)?;
        match row.state {
            ApprovalState::Accepted => {}
            ApprovalState::Succeeded => return Ok(()),
            state => {
                let reason = row
                    .result
                    .as_ref()
                    .and_then(|r| r.get("error"))
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("its approval is {state}"), str::to_owned);
                return Err(refused("memory document", path, reason));
            }
        }
        let daemon = Ctx::new(Actor::Daemon);
        let file = self.repo.state(path, &self.list)?;
        let begin = self.store.mem_apply_begin(&daemon, approval, &file)?;
        let (path, diff, message) = match self.track(begin) {
            Begin::Done => return Ok(()),
            Begin::Stale { reason } => return Err(refused("memory document", path, reason)),
            Begin::Go {
                path,
                diff,
                message,
                ..
            } => (path, diff, message),
        };
        let committed = self
            .repo
            .commit(&self.lock, &path, &diff, &message, &self.list);
        let outcome = match &committed {
            Ok(done) => Outcome::Applied {
                blob: done.blob.clone(),
                commit: done.commit.clone(),
            },
            Err(Error::Refused { fact, .. }) => Outcome::Failed {
                reason: fact.clone(),
            },
            Err(other) => Outcome::Failed {
                reason: other.to_string(),
            },
        };
        let finished = self.store.mem_apply_finish(&daemon, approval, &outcome)?;
        self.track(finished);
        committed.map(|_| ())
    }

    fn landed(&self, pending: Pending, path: &str) -> tiller_core::Result<MemoryLanded> {
        Ok(MemoryLanded {
            proposal_id: pending.proposal_id,
            approval_id: pending.approval_id,
            blob: self.repo.state(path, &self.list)?.head_blob,
        })
    }
}

/// A memory change on the blocking pool, under the repo lock, with the
/// board's actor.
async fn write(
    state: &AppState,
    change: impl FnOnce(&mut Writer, &str) -> tiller_core::Result<Value> + Send + 'static,
    key: String,
) -> ApiResult<MutationResponse> {
    if let Some(reason) = state.0.damage.reason() {
        return Err(read_only(&reason));
    }
    let db = state.db();
    let home = state.0.home.clone();
    let damage = state.0.damage.clone();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let done = Store::open_current(&db).and_then(|store| {
            let repo = Repo::at(&home);
            let lock = repo.lock()?;
            let mut writer = Writer {
                store,
                repo,
                lock,
                list: scrub::terms(&home),
                events: Vec::new(),
                replayed: true,
            };
            let data = change(&mut writer, &key)?;
            Ok((data, writer.events, writer.replayed))
        });
        if let Err(error) = &done {
            damage.suspect(&db, error);
        }
        Ok(done?)
    })
    .await;
    let (data, events, replayed) = match joined {
        Ok(result) => result?,
        Err(error) => return Err(Error::Internal(error.to_string()).into()),
    };
    Ok(Json(MutationResponse {
        ok: true,
        replayed,
        events,
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: None,
            approval_id: None,
        },
        data,
        host: None,
    }))
}

pub(crate) fn refused(entity: &'static str, id: &str, fact: impl Into<String>) -> Error {
    Error::Refused {
        entity,
        id: id.to_owned(),
        fact: fact.into(),
    }
}

fn board(key: &str) -> Ctx {
    Ctx::new(Actor::Board).with_key(key.to_owned())
}

fn home(state: &AppState) -> PathBuf {
    state.0.home.clone()
}

#[utoipa::path(get, path = "/api/v1/memory", tag = "reads", params(MemoryQuery),
    responses((status = 200, body = MemoryResponse),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn view(
    State(state): State<AppState>,
    Query(query): Query<MemoryQuery>,
) -> ApiResult<MemoryResponse> {
    let home = home(&state);
    let project = query.scope.unwrap_or_else(|| ids::FLEET_PROJECT.to_owned());
    let view = read(&state, move |store| {
        let repo = Repo::at(&home);
        let list = scrub::terms(&home);
        let view = store.memory_view(
            &project,
            |path| repo.state(path, &list),
            |paths| repo.head_texts(paths),
        )?;
        Ok(serde_json::from_value(serde_json::to_value(view)?)?)
    })
    .await?;
    Ok(Json(view))
}

#[utoipa::path(get, path = "/api/v1/memory/revisions/{id}", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = MemoryRevision, description = "The revision behind a provenance hint"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn revision(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<MemoryRevision> {
    let home = home(&state);
    let detail = read(&state, move |store| {
        let detail = store.memory_revision(&id)?;
        let text = Repo::at(&home).blob(&detail.blob_sha)?;
        let mut value = serde_json::to_value(detail)?;
        value["text"] = json!(text);
        Ok(serde_json::from_value(value)?)
    })
    .await?;
    Ok(Json(detail))
}

#[utoipa::path(post, path = "/api/v1/memory/edits", tag = "mutations", request_body = MemoryEdit,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "the document changed since the board showed it, or the scrub flagged the line"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
pub(crate) async fn edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: MemoryEdit = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a memory edit").await?;
    let key = key(&request.idempotency_key)?;
    let needs = |what: &str| ApiError::usage(format!("{} needs {what}", request.op));
    let op = match request.op.as_str() {
        "add" => EditOp::Add {
            section: request.section.clone(),
            text: request.text.clone().ok_or_else(|| needs("text"))?,
        },
        "rewrite" => EditOp::Rewrite {
            section: request.section.clone(),
            index: request.index.ok_or_else(|| needs("index"))?,
            text: request.text.clone().ok_or_else(|| needs("text"))?,
        },
        "remove" => EditOp::Remove {
            section: request.section.clone(),
            index: request.index.ok_or_else(|| needs("index"))?,
        },
        other => {
            return Err(ApiError::usage(format!(
                "op is add, rewrite or remove, not {other}"
            )));
        }
    };
    write(
        &state,
        move |w, key| {
            let path = document_path(&request.project);
            let file = w.repo.state(&path, &w.list)?;
            let edited = w.store.mem_edit(
                &board(key),
                &request.project,
                &op,
                &request.base_blob,
                &file,
            )?;
            let pending = w.track(edited);
            if let Some(approval) = &pending.approval_id {
                w.apply(approval, &path)?;
            }
            Ok(serde_json::to_value(w.landed(pending, &path)?)?)
        },
        key,
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/memory/proposals/decide", tag = "mutations", request_body = MemoryDecide,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "a proposal is not open, the document changed, or a Keep is over the startup budget; earlier decisions in the batch stand"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
pub(crate) async fn decide(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: MemoryDecide = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a memory decision").await?;
    let key = key(&request.idempotency_key)?;
    if request.decisions.is_empty() || request.decisions.len() > BATCH {
        return Err(ApiError::usage(format!(
            "decisions holds 1 to {BATCH} entries"
        )));
    }
    let mut decisions = Vec::new();
    for decision in request.decisions {
        let keep = match decision.decision.as_str() {
            "keep" => true,
            "discard" => false,
            other => {
                return Err(ApiError::usage(format!(
                    "decision is keep or discard, not {other}"
                )));
            }
        };
        decisions.push((decision.proposal, keep, decision.base_blob));
    }
    write(
        &state,
        move |w, key| {
            // Per document, the blob it had when the batch began and what it
            // is after this batch's own commits: a later decision shown
            // against the first is decided against the second.
            let mut moved: Vec<(String, String, String)> = Vec::new();
            // Per document, the proposals this batch kept, in order.
            let mut kept: Vec<(String, String)> = Vec::new();
            let mut out = Vec::new();
            for (at, (proposal, keep, shown)) in decisions.into_iter().enumerate() {
                let change = w.store.memory_change(&proposal)?;
                let path = document_path(&change.project_id);
                let file = w.repo.state(&path, &w.list)?;
                let base = moved
                    .iter()
                    .find(|(doc, from, _)| *doc == path && *from == shown)
                    .map_or(shown.clone(), |(_, _, to)| to.clone());
                let ctx = board(&format!("{key}:{at}"));
                let after: Vec<String> = kept
                    .iter()
                    .filter(|(doc, _)| *doc == path)
                    .map(|(_, id)| id.clone())
                    .collect();
                let decided = w
                    .store
                    .mem_decide(&ctx, &proposal, keep, &base, &file, &after)?;
                let pending = w.track(decided);
                if let Some(approval) = &pending.approval_id {
                    w.apply(approval, &path)?;
                }
                let landed = w.landed(pending, &path)?;
                if keep {
                    kept.push((path.clone(), proposal));
                }
                match moved.iter_mut().find(|(doc, _, _)| *doc == path) {
                    Some(entry) => entry.2 = landed.blob.clone(),
                    None => moved.push((path, shown, landed.blob.clone())),
                }
                out.push(landed);
            }
            Ok(json!({"decisions": out}))
        },
        key,
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/memory/changes/{proposal}/undo", tag = "mutations", request_body = MemoryUndo,
    params(("proposal" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 422, body = ErrorBody, description = "already undone, the document changed, or the revert conflicts with a later edit"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
pub(crate) async fn undo(
    State(state): State<AppState>,
    Path(proposal): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: MemoryUndo = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a memory undo").await?;
    let key = key(&request.idempotency_key)?;
    write(
        &state,
        move |w, key| {
            let change = w.store.memory_change(&proposal)?;
            let path = document_path(&change.project_id);
            let file = w.repo.state(&path, &w.list)?;
            // The commit reverted onto the current text, as `git revert`
            // would; on a conflict the core undoes the line itself, or
            // refuses when the line was changed since.
            let reverse = match (change.state.as_str(), &change.commit) {
                ("applied", Some(commit)) => w
                    .repo
                    .revert(&path, commit, &file.text)?
                    .map(|text| unified_diff(&path, &file.text, &text)),
                _ => None,
            };
            let undone = w.store.mem_undo(
                &board(key),
                &proposal,
                &request.base_blob,
                &file,
                reverse.as_deref(),
            )?;
            let pending = w.track(undone);
            if let Some(approval) = &pending.approval_id {
                w.apply(approval, &path)?;
            }
            Ok(serde_json::to_value(w.landed(pending, &path)?)?)
        },
        key,
    )
    .await
}

#[cfg(test)]
mod tests;
