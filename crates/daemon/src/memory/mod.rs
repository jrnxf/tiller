//! The memory view's routes (`memory.md` section 4, API): what a launch
//! loads from kb now, and the pending queue with Edit, Keep and Discard.
//!
//! Every route is the owner's. Keep runs `kb add` before the row goes, so a
//! failed add leaves the note pending. None of these routes is on the
//! forwarding allowlist: the queue and the kb store are this host's.

pub mod dto;

use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_agents::roles::Role;
use tiller_agents::spawn::config::SpawnConfig;
use tiller_agents::spawn::memory::{self as launch, STARTUP_BUDGET};
use tiller_agents::spawn::trust::ClaudeConfig;
use tiller_core::kb::{self, KbConfig};
use tiller_core::state::memory::NoteEdit;
use tiller_core::{Error, Store, Timestamp};
use utoipa::{IntoParams, ToSchema};

use self::dto::{MemoryBudget, MemoryKb, MemoryKept, MemoryLive, MemoryPending, MemoryResponse};
use crate::api::{
    ApiError, ApiResult, AppState, board_ctx, body, guard, key, read, request_args, uninitialized,
    writable,
};
use crate::dto::{ErrorBody, MutationResponse, Receipt};

#[derive(Clone, Debug, Deserialize, IntoParams)]
pub struct MemoryQuery {
    /// The project whose launch to show. Without one, `live` is empty and
    /// `pending` lists every project's notes.
    #[serde(default)]
    pub project: Option<String>,
    /// The fleet role whose launch to show; `builder` by default.
    #[serde(default)]
    pub role: Option<String>,
}

/// `PATCH /api/v1/memory/pending/{id}`: the owner's edit of a pending note.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PendingEdit {
    pub idempotency_key: String,
    /// The row's rev the board showed; another is 409.
    pub rev: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// `convention`, `decision`, `gotcha` or `how-to`.
    #[serde(default)]
    pub kind: Option<String>,
}

/// `POST /api/v1/memory/pending/{id}/keep` and `.../discard`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PendingDecision {
    pub idempotency_key: String,
    /// The row's rev the board showed; another is 409.
    pub rev: i64,
}

/// The parsed `config.toml`, or an empty table when there is none.
fn config_table(home: &FsPath) -> tiller_core::Result<toml::Table> {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text
            .parse()
            .map_err(|error| Error::Usage(format!("config.toml does not parse: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(error) => Err(Error::Internal(format!("cannot read config.toml: {error}"))),
    }
}

fn kb_config(table: &toml::Table) -> tiller_core::Result<KbConfig> {
    KbConfig::from_table(table).map_err(|error| Error::Usage(format!("config.toml: {error}")))
}

#[utoipa::path(get, path = "/api/v1/memory", tag = "reads", params(MemoryQuery),
    responses((status = 200, body = MemoryResponse),
              (status = 400, body = ErrorBody, description = "an unknown role"),
              (status = 404, body = ErrorBody)))]
pub(crate) async fn view(
    State(state): State<AppState>,
    Query(query): Query<MemoryQuery>,
) -> ApiResult<MemoryResponse> {
    let role_name = query
        .role
        .unwrap_or_else(|| Role::Builder.name().to_owned());
    let role = Role::parse(&role_name)
        .ok_or_else(|| ApiError::usage(format!("{role_name} is not a fleet role")))?;
    let home = state.0.home.clone();
    let kb_home = state.0.kb_home.clone();
    let view = read(&state, move |store| {
        let pending = store
            .pending_notes(query.project.as_deref())?
            .into_iter()
            .map(MemoryPending::from)
            .collect();
        let shown = kb_home.as_ref().map(|path| path.display().to_string());
        let Some(project_id) = query.project else {
            let read = kb::read(kb_home.as_deref());
            return Ok(MemoryResponse {
                project_id: None,
                role: role.name().to_owned(),
                kb: MemoryKb {
                    state: dto::state_name(read.state),
                    home: shown,
                    scopes: Vec::new(),
                },
                budget: MemoryBudget {
                    used: 0,
                    limit: STARTUP_BUDGET,
                },
                live: Vec::new(),
                pending,
            });
        };
        let project = store.project(&project_id)?;
        let config = kb_config(&config_table(&home)?)?;
        let rendered = launch::render(&config, kb_home.as_deref(), &project, role)
            .map_err(|error| Error::Internal(error.to_string()))?;
        Ok(MemoryResponse {
            project_id: Some(project.id.clone()),
            role: role.name().to_owned(),
            kb: MemoryKb {
                state: dto::state_name(rendered.kb),
                home: shown,
                scopes: launch::scopes(&config, &project),
            },
            budget: MemoryBudget {
                used: rendered.used,
                limit: STARTUP_BUDGET,
            },
            live: rendered.entries.iter().map(MemoryLive::from).collect(),
            pending,
        })
    })
    .await?;
    Ok(Json(view))
}

/// A memory change on the blocking pool, with the board's actor.
async fn write(
    state: &AppState,
    change: impl FnOnce(
        &mut Store,
        &str,
        Option<&Value>,
    ) -> tiller_core::Result<(Value, Vec<i64>, bool)>
    + Send
    + 'static,
    key: String,
) -> ApiResult<MutationResponse> {
    writable(state)?;
    let db = state.db();
    let damage = state.0.damage.clone();
    let args = request_args();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let done =
            Store::open_current(&db).and_then(|mut store| change(&mut store, &key, args.as_ref()));
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
        warning: None,
    }))
}

#[utoipa::path(patch, path = "/api/v1/memory/pending/{id}", tag = "mutations", request_body = PendingEdit,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the edited pending note"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "the row changed since the board showed it")))]
pub(crate) async fn edit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PendingEdit = body(&bytes)?;
    let key = key(&request.idempotency_key)?;
    let edit = NoteEdit {
        title: request.title,
        body: request.body,
        kind: request.kind,
    };
    write(
        &state,
        move |store, key, args| {
            let done = store.mem_edit(&board_ctx(key, args), Some(key), &id, request.rev, &edit)?;
            let row = serde_json::to_value(MemoryPending::from(done.value))?;
            Ok((row, done.events, done.replayed))
        },
        key,
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/memory/pending/{id}/keep", tag = "mutations", request_body = PendingDecision,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is `MemoryKept`: the kb note id and its scope"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "the row changed since the board showed it"),
              (status = 422, body = ErrorBody, description = "kb refused the note or could not run; the note stays pending")))]
pub(crate) async fn keep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PendingDecision = body(&bytes)?;
    let key = key(&request.idempotency_key)?;
    let home = state.0.home.clone();
    write(
        &state,
        move |store, key, args| {
            let table = config_table(&home)?;
            let command =
                kb_config(&table)?.command(ClaudeConfig::from_env().home_dir().as_deref());
            // A replayed Keep finds no row and runs no add; the core says
            // which it is.
            let project = store.pending_note(&id).ok().map(|note| note.project_id);
            let checkout: Option<PathBuf> = project.and_then(|project| {
                SpawnConfig::from_table(&table)
                    .ok()
                    .and_then(|config| config.checkouts.get(&project).cloned())
            });
            let done = store.mem_keep_with(&board_ctx(key, args), &id, request.rev, |input| {
                let command = command.ok_or_else(|| kb::NO_COMMAND.to_owned())?;
                kb::add(&command, checkout.as_deref(), input)
            })?;
            let kept = MemoryKept {
                note_id: done.value.note_id,
                scope: done.value.scope,
            };
            Ok((serde_json::to_value(kept)?, done.events, done.replayed))
        },
        key,
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/memory/pending/{id}/discard", tag = "mutations", request_body = PendingDecision,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is `{}`"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "the row changed since the board showed it")))]
pub(crate) async fn discard(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PendingDecision = body(&bytes)?;
    let key = key(&request.idempotency_key)?;
    write(
        &state,
        move |store, key, args| {
            let done = store.mem_discard(&board_ctx(key, args), Some(key), &id, request.rev)?;
            Ok((json!({}), done.events, done.replayed))
        },
        key,
    )
    .await
}

#[cfg(test)]
mod tests;
