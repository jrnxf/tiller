//! The board API over axum (`protocol.md` section 7): reads, mutations
//! through the core's writer functions, the change stream, the login link
//! and the app shell with its boot snapshot.
//!
//! Every request passes the `Host` check first (421 otherwise, against DNS
//! rebinding). Mutations also need the owner cookie, a JSON body,
//! `Sec-Fetch-Site: same-origin` and the daemon's own `Origin`; no CORS
//! header is ever sent.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_agents::spawn::config::SpawnConfig;
use tiller_core::domain::Stage;
use tiller_core::domain::{
    AnsweredVia, Approval, CardKind, CardOutcome, HoldReleaseReason, HostKind, Session,
};
use tiller_core::state::approvals::Decision;
use tiller_core::state::cards::{CardEdit, NewCard};
use tiller_core::state::{find, get as get_row, resolve_id};
use tiller_core::{Actor, Applied, Ctx, Error, Store, Timestamp};
use tiller_herdr::client::{Cli, Herdr};
use tiller_herdr::poller::Registry;
use tokio::sync::watch;

use crate::assets::{self, Assets};
use crate::dto::*;
use crate::readonly::Damage;
use crate::views::{self, Filter, LocalHealth};
use crate::{auth, sse};

/// Lines of a pane the run detail shows.
const PANE_TAIL: usize = 40;

/// What every handler shares.
#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    /// `~/.tiller`: `state.db` and the board credential.
    pub home: PathBuf,
    /// The port the daemon listens on, for the `Host` and `Origin` checks.
    pub port: u16,
    /// Extra `Host` values served reads, such as a reverse proxy's name
    /// (`tiller daemon --allow-host`). Lowercase.
    pub allowed_hosts: Vec<String>,
    pub assets: Assets,
    /// The newest `seq`, published by the change feed.
    pub newest: watch::Receiver<i64>,
    /// Turns true when the daemon stops; open streams end on it.
    pub stop: watch::Receiver<bool>,
    pub pollers: Registry,
    /// The local herdr server, for focus and pane tails.
    pub herdr: Cli,
    pub started_at: Timestamp,
    /// Tool versions, filled in once after start.
    pub tools: Arc<Mutex<Value>>,
    /// Set when `state.db` fails its integrity check (`recovery.md` R10):
    /// reads go on, every mutation is refused.
    pub damage: Damage,
}

impl AppState {
    pub fn new(inner: Inner) -> Self {
        Self(Arc::new(inner))
    }

    fn db(&self) -> PathBuf {
        self.0.home.join("state.db")
    }

    fn own_health(&self) -> LocalHealth {
        local_health(&self.0.pollers)
    }
}

/// The pollers whose last poll failed.
pub fn local_health(pollers: &Registry) -> LocalHealth {
    LocalHealth {
        failing_pollers: pollers
            .health()
            .into_iter()
            .filter(|health| health.consecutive_failures > 0)
            .map(|health| health.source)
            .collect(),
    }
}

// -- errors -------------------------------------------------------------------

/// An error answer: the CLI's error shape with the matching HTTP status.
/// The body is boxed so every handler's `Result` stays small.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: Box<ErrorBody>,
}

impl ApiError {
    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: Box::new(ErrorBody {
                ok: false,
                error: ErrorDetail {
                    code: code.to_owned(),
                    message: message.into(),
                    entity: None,
                    id: None,
                    current: None,
                    hint: None,
                },
            }),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    fn usage(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "usage", message)
    }

    fn hint(mut self, hint: &str) -> Self {
        self.body.error.hint = Some(hint.to_owned());
        self
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        let status = match error.exit_code() {
            2 => StatusCode::BAD_REQUEST,
            3 => StatusCode::NOT_FOUND,
            4 => StatusCode::CONFLICT,
            5 => StatusCode::UNPROCESSABLE_ENTITY,
            6 => StatusCode::FORBIDDEN,
            7 => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let mut api = ApiError::new(status, error.code(), error.to_string());
        match &error {
            Error::NotFound {
                entity,
                id,
                candidates,
            } => {
                api.body.error.entity = Some((*entity).to_owned());
                api.body.error.id = Some(id.clone());
                if !candidates.is_empty() {
                    api.body.error.current = Some(json!({"candidates": candidates}));
                }
            }
            Error::Conflict {
                entity,
                id,
                current_rev,
                field,
                ..
            } => {
                api.body.error.entity = Some((*entity).to_owned());
                api.body.error.id = Some(id.clone());
                let mut current = json!({"rev": current_rev});
                if let Some((name, value)) = field {
                    current[name] = value.clone();
                }
                api.body.error.current = Some(current);
            }
            Error::Refused { entity, id, .. } => {
                api.body.error.entity = Some((*entity).to_owned());
                api.body.error.id = Some(id.clone());
            }
            Error::Internal(_) | Error::SchemaTooNew { .. } => {
                tracing::error!(%error, "request failed");
            }
            _ => {}
        }
        api
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(*self.body)).into_response()
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// The command that makes `state.db`, named in every answer before it exists.
pub const INIT_HINT: &str = "tiller init --host-label <label> --lane-capacity <n>";

/// The 503 every read and write answers before `tiller init`.
fn uninitialized() -> ApiError {
    ApiError::from(Error::Unavailable(
        "state.db is not initialized; run tiller init".into(),
    ))
    .hint(INIT_HINT)
}

/// The 503 every mutation answers while `state.db` is damaged.
fn read_only(reason: &str) -> ApiError {
    ApiError::from(Error::Unavailable(format!(
        "read only: state.db failed its integrity check ({reason}); mutations are refused until the owner restores it"
    )))
    .hint("stop the daemon, then run tiller restore <file> --dry-run and again without the flag")
}

/// Runs `read` on the blocking pool against a fresh read connection.
async fn read<T: Send + 'static>(
    state: &AppState,
    read: impl FnOnce(&Store) -> tiller_core::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let db = state.db();
    let damage = state.0.damage.clone();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let done = Store::open_current(&db).and_then(|store| read(&store));
        if let Err(error) = &done {
            damage.suspect(&db, error);
        }
        Ok(done?)
    })
    .await;
    match joined {
        Ok(result) => result,
        Err(error) => Err(Error::Internal(error.to_string()).into()),
    }
}

// -- the Host check and the mutation guard ---------------------------------------

fn loopback_hosts(port: u16) -> [String; 2] {
    [format!("127.0.0.1:{port}"), format!("localhost:{port}")]
}

/// 421 unless `Host` is exactly this daemon's loopback address, or one the
/// owner allowed with `--allow-host`.
///
/// A page opened on `localhost` is sent to `127.0.0.1`, the host the login
/// link names: the owner cookie belongs to one host, so a board on the other
/// could read but never write. API calls are answered on either. An allowed
/// host is served reads only: `guard` refuses its mutations.
async fn host_check(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let [own, alias] = loopback_hosts(state.0.port);
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|host| host.to_str().ok());
    let page = matches!(*request.method(), Method::GET | Method::HEAD)
        && !request.uri().path().starts_with("/api/");
    if host == Some(alias.as_str()) && page {
        let path = request
            .uri()
            .path_and_query()
            .map_or("/", |path| path.as_str());
        Redirect::permanent(&format!("http://{own}{path}")).into_response()
    } else if host.is_some_and(|host| {
        host == own
            || host == alias
            // The login link mints the owner cookie, so it stays on loopback.
            || (!request.uri().path().starts_with("/login/")
                && state.0.allowed_hosts.contains(&host.to_ascii_lowercase()))
    }) {
        next.run(request).await
    } else {
        ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "forbidden",
            "the board answers only on 127.0.0.1, localhost or an --allow-host name",
        )
        .into_response()
    }
}

/// The checks every mutation passes before its body is read.
fn guard(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let text = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    let json_body = text(header::CONTENT_TYPE).is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
    });
    if !json_body {
        return Err(ApiError::usage(
            "a mutation sends Content-Type: application/json",
        ));
    }
    if headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) != Some("same-origin") {
        return Err(ApiError::forbidden(
            "a mutation must come from the board itself",
        ));
    }
    let host = text(header::HOST).unwrap_or_default();
    if !loopback_hosts(state.0.port).iter().any(|own| own == host) {
        return Err(ApiError::forbidden(
            "a mutation must come from the board on 127.0.0.1",
        ));
    }
    if text(header::ORIGIN) != Some(format!("http://{host}").as_str()) {
        return Err(ApiError::forbidden(
            "a mutation must come from the board's own origin",
        ));
    }
    let owner = text(header::COOKIE)
        .and_then(auth::cookie_value)
        .is_some_and(|cookie| auth::is_owner(&state.0.home, cookie, Timestamp::now()));
    if !owner {
        return Err(
            ApiError::forbidden("the board is not signed in as the owner")
                .hint("run tiller board --login in a terminal and open the link"),
        );
    }
    Ok(())
}

/// A JSON body, refused as 400 when it does not parse.
fn body<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|error| ApiError::usage(format!("bad body: {error}")))
}

fn key(idempotency_key: &str) -> Result<String, ApiError> {
    let key = idempotency_key.trim();
    if key.is_empty() {
        return Err(ApiError::usage("idempotency_key is required"));
    }
    Ok(key.to_owned())
}

/// One mutation on the blocking pool with the board's actor and the key.
async fn mutate(
    state: &AppState,
    key: String,
    mutation: impl FnOnce(&mut Store, &Ctx, &str) -> tiller_core::Result<Done> + Send + 'static,
) -> ApiResult<MutationResponse> {
    if let Some(reason) = state.0.damage.reason() {
        return Err(read_only(&reason));
    }
    let db = state.db();
    let damage = state.0.damage.clone();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let done = Store::open_current(&db).and_then(|mut store| {
            let ctx = Ctx::new(Actor::Board).with_key(key.clone());
            mutation(&mut store, &ctx, &key)
        });
        if let Err(error) = &done {
            damage.suspect(&db, error);
        }
        Ok(done?)
    })
    .await;
    let done = match joined {
        Ok(result) => result?,
        Err(error) => return Err(Error::Internal(error.to_string()).into()),
    };
    Ok(Json(MutationResponse {
        ok: true,
        replayed: done.replayed,
        events: done.events,
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: done.outbox_id,
            approval_id: done.approval_id,
        },
        data: done.data,
    }))
}

/// What a mutation hands back to [`mutate`].
struct Done {
    data: Value,
    events: Vec<i64>,
    replayed: bool,
    outbox_id: Option<String>,
    approval_id: Option<String>,
}

impl Done {
    fn of<T: Serialize>(applied: Applied<T>) -> tiller_core::Result<Self> {
        Ok(Self {
            data: serde_json::to_value(&applied.value)?,
            events: applied.events,
            replayed: applied.replayed,
            outbox_id: None,
            approval_id: None,
        })
    }
}

// -- the router -------------------------------------------------------------------

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/snapshot", get(snapshot))
        .route("/api/v1/board", get(board))
        .route("/api/v1/cards", post(create_card))
        .route("/api/v1/cards/{id}", get(card).patch(edit_card))
        .route("/api/v1/cards/{id}/timeline", get(timeline))
        .route("/api/v1/cards/{id}/comment", post(comment))
        .route("/api/v1/cards/{id}/hold", post(hold).delete(release_hold))
        .route("/api/v1/cards/{id}/close", post(close))
        .route("/api/v1/cards/{id}/cancel", post(cancel))
        .route("/api/v1/cards/{id}/reopen", post(reopen))
        .route("/api/v1/attention", get(attention))
        .route("/api/v1/attention/{id}/answer", post(answer))
        .route("/api/v1/approvals/{id}/decide", post(decide))
        .route("/api/v1/agents", get(agents))
        .route("/api/v1/agents/{session}/focus", post(focus))
        .route("/api/v1/runs/{id}", get(run))
        .route("/api/v1/machines", get(machines))
        .route("/api/v1/quota", get(quota))
        .route("/api/v1/health", get(health))
        .route("/api/v1/projects", get(projects))
        .route("/api/v1/projects/{id}/stage", post(stage_project))
        .route("/api/v1/signals", get(signals))
        .route("/api/v1/events", get(events))
        .route("/login/{token}", get(login))
        .fallback(app_shell)
        .layer(middleware::from_fn_with_state(state.clone(), host_check))
        .with_state(state)
}

// -- reads ------------------------------------------------------------------------

/// The boot snapshot, also served alone for a silent refetch after `reset`.
#[utoipa::path(get, path = "/api/v1/snapshot", tag = "reads",
    responses((status = 200, body = Snapshot), (status = 503, body = ErrorBody)))]
async fn snapshot(State(state): State<AppState>) -> ApiResult<Snapshot> {
    let local = state.own_health();
    read(&state, move |store| {
        views::snapshot(store, &local, Timestamp::now())
    })
    .await
    .map(Json)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct BoardQuery {
    pub project: Option<String>,
    /// `all` (default), `active` or `needs`.
    pub filter: Option<String>,
    /// A host id or label.
    pub host: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/board", tag = "reads", params(BoardQuery),
    responses((status = 200, body = BoardResponse), (status = 400, body = ErrorBody)))]
async fn board(
    State(state): State<AppState>,
    Query(query): Query<BoardQuery>,
) -> ApiResult<BoardResponse> {
    let filter: Filter = query.filter.as_deref().unwrap_or("all").parse()?;
    read(&state, move |store| {
        views::board(
            store,
            query.project.as_deref(),
            filter,
            query.host.as_deref(),
        )
    })
    .await
    .map(Json)
}

#[utoipa::path(get, path = "/api/v1/cards/{id}", tag = "reads",
    params(("id" = String, Path, description = "ULID, short form or display id")),
    responses((status = 200, body = CardDetail), (status = 404, body = ErrorBody)))]
async fn card(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<CardDetail> {
    read(&state, move |store| views::card_detail(store, &id))
        .await
        .map(Json)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct TimelineQuery {
    /// Only events older than this `seq`.
    pub before: Option<i64>,
}

#[utoipa::path(get, path = "/api/v1/cards/{id}/timeline", tag = "reads",
    params(("id" = String, Path), TimelineQuery),
    responses((status = 200, body = TimelineResponse), (status = 404, body = ErrorBody)))]
async fn timeline(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<TimelineResponse> {
    read(&state, move |store| {
        views::timeline(store, &id, query.before)
    })
    .await
    .map(Json)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AttentionQuery {
    /// Only `open` is served.
    pub state: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/attention", tag = "reads", params(AttentionQuery),
    responses((status = 200, body = AttentionResponse), (status = 400, body = ErrorBody)))]
async fn attention(
    State(state): State<AppState>,
    Query(query): Query<AttentionQuery>,
) -> ApiResult<AttentionResponse> {
    if query.state.as_deref().is_some_and(|s| s != "open") {
        return Err(ApiError::usage("attention is served with state=open only"));
    }
    read(&state, views::attention).await.map(Json)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct HostQuery {
    /// A host id or label.
    pub host: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/agents", tag = "reads", params(HostQuery),
    responses((status = 200, body = AgentsResponse)))]
async fn agents(
    State(state): State<AppState>,
    Query(query): Query<HostQuery>,
) -> ApiResult<AgentsResponse> {
    read(&state, move |store| {
        views::agents(store, query.host.as_deref())
    })
    .await
    .map(Json)
}

#[utoipa::path(get, path = "/api/v1/runs/{id}", tag = "reads", params(("id" = String, Path)),
    responses((status = 200, body = RunDetail), (status = 404, body = ErrorBody)))]
async fn run(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<RunDetail> {
    let (mut detail, pane) = read(&state, move |store| views::run_detail(store, &id)).await?;
    if let Some(target) = pane {
        let herdr = match target.machine {
            Some(machine) => state.0.herdr.clone().on_machine(machine),
            None => state.0.herdr.clone(),
        };
        let tail = tokio::task::spawn_blocking(move || herdr.read_visible(&target.pane).ok()).await;
        if let Ok(Some(screen)) = tail {
            detail.pane_tail = pane_tail(&screen);
        }
    }
    Ok(Json(detail))
}

/// The last non-empty lines of a pane read, without terminal escapes.
fn pane_tail(screen: &str) -> Vec<String> {
    let plain = strip_ansi(screen);
    let lines: Vec<String> = plain
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect();
    let end = lines
        .iter()
        .rposition(|line| !line.is_empty())
        .map_or(0, |i| i + 1);
    lines[end.saturating_sub(PANE_TAIL)..end].to_vec()
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() || c == '\n' || c == '\t' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ST.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[utoipa::path(get, path = "/api/v1/machines", tag = "reads",
    responses((status = 200, body = MachinesResponse)))]
async fn machines(State(state): State<AppState>) -> ApiResult<MachinesResponse> {
    let local = state.own_health();
    read(&state, move |store| {
        views::machines(store, &local, Timestamp::now())
    })
    .await
    .map(Json)
}

#[utoipa::path(get, path = "/api/v1/quota", tag = "reads",
    responses((status = 200, body = QuotaResponse)))]
async fn quota(State(state): State<AppState>) -> ApiResult<QuotaResponse> {
    read(&state, |store| views::quota(store, Timestamp::now()))
        .await
        .map(Json)
}

#[utoipa::path(get, path = "/api/v1/projects", tag = "reads",
    responses((status = 200, body = ProjectsResponse)))]
async fn projects(State(state): State<AppState>) -> ApiResult<ProjectsResponse> {
    read(&state, |store| views::projects(store, Timestamp::now()))
        .await
        .map(Json)
}

#[utoipa::path(get, path = "/api/v1/signals", tag = "reads",
    responses((status = 200, body = SignalsResponse)))]
async fn signals(State(state): State<AppState>) -> ApiResult<SignalsResponse> {
    read(&state, |store| views::signals(store, Timestamp::now()))
        .await
        .map(Json)
}

/// The daemon's own health: versions, pollers and tools, no work data. It
/// answers before `tiller init` too.
#[utoipa::path(get, path = "/api/v1/health", tag = "reads",
    responses((status = 200, body = Health)))]
async fn health(State(state): State<AppState>) -> Json<Health> {
    let db = state.db();
    let seq = tokio::task::spawn_blocking(move || {
        db.exists()
            .then(|| Store::open_current(&db).and_then(|store| store.last_seq()))
    })
    .await
    .ok()
    .flatten();
    let pollers = state.0.pollers.health();
    let failing = pollers.iter().any(|p| p.consecutive_failures > 0);
    let tools = state
        .0
        .tools
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    Json(Health {
        ok: matches!(seq, Some(Ok(_))) && !failing,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        schema: tiller_core::migrate::LATEST,
        initialized: seq.is_some(),
        pid: std::process::id(),
        started_at: state.0.started_at.to_string(),
        server_time: Timestamp::now().to_string(),
        last_event_seq: seq.and_then(Result::ok).unwrap_or(0),
        pollers: views::poller_health(pollers),
        tools,
    })
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct EventsQuery {
    /// Resume after this `seq` when the request has no `Last-Event-ID`:
    /// the boot snapshot's `last_event_seq` on the first connect.
    pub since: Option<i64>,
}

/// The change stream: `event: change` with a [`ChangeEvent`] per `events`
/// row, `event: reset` with a [`ResetEvent`] when a resume is impossible.
#[utoipa::path(get, path = "/api/v1/events", tag = "stream", params(EventsQuery,
        ("Last-Event-ID" = Option<i64>, Header, description = "the last seq the client applied")),
    responses(
        (status = 200, content_type = "text/event-stream", body = ChangeEvent,
         description = "`change` messages carry a ChangeEvent; `reset` messages a ResetEvent"),
        (status = 503, body = ErrorBody)))]
async fn events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    if !state.db().exists() {
        return uninitialized().into_response();
    }
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok());
    let after = sse::start(last, query.since);
    let pollers = state.0.pollers.clone();
    sse::stream(
        state.db(),
        after,
        state.0.newest.clone(),
        state.0.stop.clone(),
        move || local_health(&pollers),
    )
    .into_response()
}

// -- mutations --------------------------------------------------------------------

#[utoipa::path(post, path = "/api/v1/cards", tag = "mutations", request_body = CreateCard,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 422, body = ErrorBody)))]
async fn create_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: CreateCard = body(&bytes)?;
    let kind: CardKind = request.kind.parse()?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, key| {
            let added = store.board_add(
                ctx,
                Some(key),
                NewCard {
                    project_id: request.project,
                    kind,
                    title: request.title,
                    body: request.body,
                    priority: request.priority,
                    ..NewCard::default()
                },
            )?;
            let outbox_id = added.value.message.as_ref().map(|m| m.id.clone());
            let mut done = Done::of(added)?;
            done.outbox_id = outbox_id;
            Ok(done)
        },
    )
    .await
}

#[utoipa::path(patch, path = "/api/v1/cards/{id}", tag = "mutations", request_body = EditCard,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 409, body = ErrorBody)))]
async fn edit_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: EditCard = body(&bytes)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, key| {
            let edit = CardEdit {
                title: request.title,
                body: request.body.map(Some),
                priority: request.priority,
                ..CardEdit::default()
            };
            Done::of(store.card_edit(ctx, Some(key), &id, request.rev, edit)?)
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/comment", tag = "mutations", request_body = Comment,
    params(("id" = String, Path)), responses((status = 200, body = MutationResponse)))]
async fn comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: Comment = body(&bytes)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, key| {
            let applied = store.card_comment(ctx, Some(key), &id, &request.text)?;
            let outbox_id = Some(applied.value.id.clone());
            let mut done = Done::of(applied)?;
            done.outbox_id = outbox_id;
            Ok(done)
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/hold", tag = "mutations", request_body = HoldBody,
    params(("id" = String, Path)), responses((status = 200, body = MutationResponse)))]
async fn hold(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: HoldBody = body(&bytes)?;
    let until: Timestamp = request.until.parse()?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.card_hold(ctx, &id, until, &request.reason)?),
    )
    .await
}

#[utoipa::path(delete, path = "/api/v1/cards/{id}/hold", tag = "mutations", request_body = Transition,
    params(("id" = String, Path)), responses((status = 200, body = MutationResponse)))]
async fn release_hold(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: Transition = body(&bytes)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.hold_release(ctx, &id, HoldReleaseReason::Removed)?),
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/close", tag = "mutations", request_body = Transition,
    params(("id" = String, Path)), responses((status = 200, body = MutationResponse)))]
async fn close(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: Transition = body(&bytes)?;
    let outcome: CardOutcome = request.outcome.as_deref().unwrap_or("completed").parse()?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            Done::of(store.card_close(ctx, &id, outcome, request.reason.as_deref(), None)?)
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/stage", tag = "mutations", request_body = StageProject,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 409, body = ErrorBody)))]
async fn stage_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: StageProject = body(&bytes)?;
    let stage: Stage = request.stage.parse()?;
    let home = state.0.home.clone();
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            // To pre-launch, the reviewed commit is the default-branch head
            // read now, as `tiller project stage` reads it. A replay, a stale
            // rev or the current stage fetches nothing.
            let head = if store.stage_reads_head(ctx, &id, request.rev, stage)? {
                let project: tiller_core::domain::Project = get_row(store.conn(), "project", &id)?;
                Some(prelaunch_head(&home, &project)?)
            } else {
                None
            };
            Done::of(store.project_stage(ctx, &id, request.rev, stage, head.as_deref())?)
        },
    )
    .await
}

fn prelaunch_head(
    home: &std::path::Path,
    project: &tiller_core::domain::Project,
) -> tiller_core::Result<String> {
    let table = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map_err(|error| Error::Usage(format!("config.toml does not parse: {error}")))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(error) => return Err(Error::Internal(format!("read config.toml: {error}"))),
    };
    let config = SpawnConfig::from_table(&table)
        .map_err(|error| Error::Usage(format!("config.toml: {error}")))?;
    let (_, head) =
        tiller_agents::review::checkout_head(&config, project).map_err(|error| Error::Refused {
            entity: "project",
            id: project.id.clone(),
            fact: error.to_string(),
        })?;
    Ok(head)
}

fn reason(request: &Transition) -> Result<String, ApiError> {
    request
        .reason
        .clone()
        .filter(|reason| !reason.trim().is_empty())
        .ok_or_else(|| ApiError::usage("reason is required"))
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/cancel", tag = "mutations", request_body = Transition,
    params(("id" = String, Path)), responses((status = 200, body = MutationResponse)))]
async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: Transition = body(&bytes)?;
    let reason = reason(&request)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.card_cancel(ctx, &id, &reason)?),
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/reopen", tag = "mutations", request_body = Transition,
    params(("id" = String, Path)), responses((status = 200, body = MutationResponse)))]
async fn reopen(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: Transition = body(&bytes)?;
    let reason = reason(&request)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.card_reopen(ctx, &id, &reason)?),
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/attention/{id}/answer", tag = "mutations", request_body = AnswerBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 422, body = ErrorBody)))]
async fn answer(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: AnswerBody = body(&bytes)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, key| {
            let applied =
                store.attention_answer(ctx, Some(key), &id, &request.answer, AnsweredVia::Board)?;
            let outbox_id = applied.value.message.as_ref().map(|m| m.id.clone());
            let item = views::attention_item(store, &applied.value.attention)?;
            Ok(Done {
                data: serde_json::to_value(item)?,
                events: applied.events,
                replayed: applied.replayed,
                outbox_id,
                approval_id: None,
            })
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/approvals/{id}/decide", tag = "mutations", request_body = DecideBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 422, body = ErrorBody, description = "the head moved: `current.head_sha` is the bound head")))]
async fn decide(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: DecideBody = body(&bytes)?;
    let accept = match request.decision.as_str() {
        "accept" => true,
        "reject" => false,
        other => {
            return Err(ApiError::usage(format!(
                "decision is accept or reject, not {other:?}"
            )));
        }
    };
    let shown = request.head_sha.clone();
    let lookup = id.clone();
    let decided = mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, key| {
            // The head check runs in the decision's own transaction.
            let applied = store.approval_decide_shown(
                ctx,
                Some(key),
                &id,
                Decision {
                    accept,
                    via: AnsweredVia::Board,
                    memory: Vec::new(),
                    ask_id: None,
                },
                &shown,
            )?;
            let approval_id = Some(applied.value.approval.id.clone());
            let mut done = Done::of(applied)?;
            done.approval_id = approval_id;
            Ok(done)
        },
    )
    .await;
    match decided {
        Err(mut error) if error.body.error.message.contains(HEAD_MOVED) => {
            // Tell the board which head the approval binds now.
            let head = read(&state, move |store| {
                let id = resolve_id(store.conn(), "approvals", "approval", &lookup)?;
                let approval: Approval = get_row(store.conn(), "approval", &id)?;
                Ok(approval.head_sha)
            })
            .await
            .ok()
            .flatten();
            error.body.error.current = Some(json!({"head_sha": head}));
            Err(error)
        }
        other => other,
    }
}

/// The core's refusal when the approval binds another head than the one
/// the board showed.
const HEAD_MOVED: &str = "the head moved since it was shown";

#[utoipa::path(post, path = "/api/v1/agents/{session}/focus", tag = "mutations", request_body = FocusBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 503, body = ErrorBody)))]
async fn focus(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let _: FocusBody = body(&bytes)?;
    let target = read(&state, move |store| {
        let id = resolve_id(store.conn(), "sessions", "session", &session)?;
        let session: Session = get_row(store.conn(), "session", &id)?;
        let host: tiller_core::domain::Host = get_row(store.conn(), "host", &session.host_id)?;
        let pane = find::<tiller_core::domain::Pane>(
            store.conn(),
            "session_id = ?1 AND gone_at IS NULL",
            &[&session.id],
        )?
        .map(|pane| pane.pane_id)
        .or(session.pane_id.clone())
        .ok_or_else(|| Error::Refused {
            entity: "session",
            id: session.id.clone(),
            fact: "the session has no pane to focus".into(),
        })?;
        let machine = (host.kind == HostKind::Remote)
            .then_some(host.herdr_machine)
            .flatten();
        Ok((session.id, pane, machine))
    })
    .await?;
    let (session_id, pane, machine) = target;
    let herdr = match machine {
        Some(machine) => state.0.herdr.clone().on_machine(machine),
        None => state.0.herdr.clone(),
    };
    let focused = tokio::task::spawn_blocking(move || herdr.focus_agent(&pane)).await;
    match focused {
        Ok(Ok(())) => Ok(Json(MutationResponse {
            ok: true,
            replayed: false,
            events: Vec::new(),
            receipt: Receipt {
                recorded_at: Timestamp::now().to_string(),
                outbox_id: None,
                approval_id: None,
            },
            data: json!({"session_id": session_id}),
        })),
        Ok(Err(error)) => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            format!("herdr did not focus the agent: {error}"),
        )),
        Err(error) => Err(Error::Internal(error.to_string()).into()),
    }
}

// -- login and the app shell --------------------------------------------------------

/// The one-time login link: sets the owner cookie and goes to the board.
async fn login(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let home = state.0.home.clone();
    let redeemed =
        tokio::task::spawn_blocking(move || auth::redeem_login(&home, &token, Timestamp::now()))
            .await;
    match redeemed {
        Ok(Ok(Some(cookie))) => (
            [(header::SET_COOKIE, auth::set_cookie(&cookie))],
            Redirect::to("/"),
        )
            .into_response(),
        Ok(Ok(None)) => (
            StatusCode::FORBIDDEN,
            "This login link was used already or has expired. Run `tiller board --login` again.",
        )
            .into_response(),
        Ok(Err(error)) => {
            tracing::error!(%error, "login failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Built files, and for every other path the app shell with the boot
/// snapshot inlined. An unknown API path is a JSON 404.
async fn app_shell(
    State(state): State<AppState>,
    method: Method,
    request: Request<Body>,
) -> Response {
    let path = request.uri().path().to_owned();
    if path.starts_with("/api/") {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no endpoint {path}"),
        )
        .into_response();
    }
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    match state.0.assets.file(&path) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(status) => return status.into_response(),
    }
    let Some(index) = state.0.assets.index() else {
        return assets::unbuilt();
    };
    let local = state.own_health();
    let snapshot = read(&state, move |store| {
        let snapshot = views::snapshot(store, &local, Timestamp::now())?;
        Ok(serde_json::to_string(&snapshot)?)
    })
    .await;
    let html = match snapshot {
        Ok(json) => assets::with_boot(&index, &json),
        // Before `tiller init` the board boots empty and fetches.
        Err(_) => index,
    };
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        html,
    )
        .into_response()
}

// -- the contract ---------------------------------------------------------------------

/// The OpenAPI document `contract/openapi.json` is generated from: every
/// route above and the SSE message schemas (`protocol.md` 7.1).
#[derive(utoipa::OpenApi)]
#[openapi(
    info(title = "Tiller board API", version = "1", description = "The daemon's board API (docs/spec/protocol.md section 7). Every path answers only on 127.0.0.1 or localhost; mutations need the owner cookie."),
    paths(
        snapshot, board, card, timeline, attention, agents, run, machines, quota, health,
        projects, signals, events, create_card, edit_card, comment, hold, release_hold, close,
        cancel, reopen, answer, decide, focus, stage_project
    ),
    components(schemas(ChangeEvent, ResetEvent, ErrorBody, ErrorDetail)),
    tags(
        (name = "reads", description = "Reads, served to any request that passes the Host check"),
        (name = "mutations", description = "Mutations: owner cookie, JSON body, same origin"),
        (name = "stream", description = "The one SSE stream"),
    )
)]
pub struct ApiDoc;

/// The contract as committed: pretty JSON with a final newline.
pub fn contract() -> String {
    use utoipa::OpenApi;
    let mut json = ApiDoc::openapi().to_pretty_json().unwrap_or_default();
    json.push('\n');
    json
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::body::to_bytes;
    use rust_embed::RustEmbed;
    use tiller_herdr::testing::Fleet;
    use tower::ServiceExt;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/dist"]
    pub(crate) struct Fixture;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/empty"]
    #[allow_missing = true]
    struct Missing;

    const PORT: u16 = 7421;
    const HOST: &str = "127.0.0.1:7421";

    fn app(home: &std::path::Path, assets: Assets) -> Router {
        app_with(home, assets, Damage::default())
    }

    pub(crate) fn app_with(home: &std::path::Path, assets: Assets, damage: Damage) -> Router {
        let (_newest_tx, newest) = watch::channel(0);
        let (_stop_tx, stop) = watch::channel(false);
        router(AppState::new(Inner {
            home: home.to_owned(),
            port: PORT,
            allowed_hosts: vec!["board.example.com".into()],
            assets,
            newest,
            stop,
            pollers: Registry::new(),
            herdr: Cli::local(),
            started_at: Timestamp::now(),
            tools: Arc::new(Mutex::new(Value::Null)),
            damage,
        }))
    }

    pub(crate) struct Answer {
        pub(crate) status: StatusCode,
        headers: HeaderMap,
        pub(crate) body: String,
    }

    pub(crate) async fn send(app: &Router, request: Request<Body>) -> Answer {
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        Answer {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        }
    }

    pub(crate) async fn get(app: &Router, path: &str) -> Answer {
        send(
            app,
            Request::get(path)
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    fn cache(answer: &Answer) -> Option<&str> {
        answer
            .headers
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok())
    }

    /// The owner cookie, minted and redeemed as the login link does.
    pub(crate) fn owner_cookie(home: &std::path::Path) -> String {
        let token = auth::mint_login(home, Timestamp::now()).unwrap();
        let cookie = auth::redeem_login(home, &token, Timestamp::now())
            .unwrap()
            .unwrap();
        format!("tiller_owner={cookie}")
    }

    /// `POST /api/v1/cards` as the board sends it, with `headers` set over
    /// the board's own.
    pub(crate) fn create(headers: &[(&str, &str)]) -> Request<Body> {
        let mut request = Request::post("/api/v1/cards").body(Body::from(
            json!({"idempotency_key": "k-1", "project": "alpha", "kind": "ship", "title": "Undo strokes"})
                .to_string(),
        ))
        .unwrap();
        let origin = format!("http://{HOST}");
        let board = [
            ("host", HOST),
            ("content-type", "application/json"),
            ("sec-fetch-site", "same-origin"),
            ("origin", origin.as_str()),
        ];
        for (name, value) in board.iter().chain(headers) {
            let name: header::HeaderName = name.parse().unwrap();
            request.headers_mut().insert(name, value.parse().unwrap());
        }
        request
    }

    pub(crate) fn cards(fleet: &Fleet) -> i64 {
        fleet
            .open()
            .conn()
            .query_row("SELECT count(*) FROM cards", [], |row| row.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn root_serves_the_app_shell_with_the_boot_snapshot() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let root = get(&app, "/").await;
        assert_eq!(root.status, StatusCode::OK);
        assert_eq!(cache(&root), Some("no-cache"));
        assert!(
            root.body
                .contains(r#"<script type="application/json" id="tiller-boot">"#)
        );
        assert!(root.body.contains("app-abc123.js"));
    }

    #[tokio::test]
    async fn the_boot_snapshot_holds_exactly_what_each_get_returns() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let root = get(&app, "/").await;
        let boot = root
            .body
            .split_once(r#"id="tiller-boot">"#)
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        let boot: Value = serde_json::from_str(boot).unwrap();
        assert_eq!(boot["v"], 1);
        let queries = boot["queries"].as_object().unwrap();
        assert_eq!(queries.len(), views::SNAPSHOT_PATHS.len());
        for path in views::SNAPSHOT_PATHS {
            let answer = get(&app, path).await;
            assert_eq!(answer.status, StatusCode::OK, "{path}");
            let mut live: Value = serde_json::from_str(&answer.body).unwrap();
            let mut inlined = queries[path].clone();
            // Ages, and the local host's last-seen time (now), move between
            // the two reads.
            for value in [&mut live, &mut inlined] {
                drop_times(value);
            }
            assert_eq!(inlined, live, "{path}");
        }
    }

    fn drop_times(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.retain(|key, _| {
                    !(key.ends_with("_time") || key.starts_with("age") || key == "last_seen_at")
                });
                map.values_mut().for_each(drop_times);
            }
            Value::Array(items) => items.iter_mut().for_each(drop_times),
            _ => {}
        }
    }

    #[tokio::test]
    async fn hashed_assets_are_immutable() {
        let home = tempfile::tempdir().unwrap();
        let app = app(home.path(), Assets::of::<Fixture>());
        let asset = get(&app, "/assets/app-abc123.js").await;
        assert_eq!(asset.status, StatusCode::OK);
        assert_eq!(cache(&asset), Some(assets::IMMUTABLE));
        assert!(
            asset.headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .contains("javascript")
        );
    }

    #[tokio::test]
    async fn client_routes_fall_back_to_the_app_shell() {
        let home = tempfile::tempdir().unwrap();
        let app = app(home.path(), Assets::of::<Fixture>());
        for path in ["/cards/alpha-1", "/cards/alpha-1.2", "/health"] {
            let shell = get(&app, path).await;
            assert_eq!(shell.status, StatusCode::OK, "{path}");
            assert!(shell.body.contains("app-abc123.js"), "{path}");
        }
    }

    #[tokio::test]
    async fn a_missing_asset_or_api_path_is_not_found_not_html() {
        let home = tempfile::tempdir().unwrap();
        let app = app(home.path(), Assets::of::<Fixture>());
        let asset = get(&app, "/assets/app-gone.js").await;
        assert_eq!(asset.status, StatusCode::NOT_FOUND);
        assert!(!asset.body.contains("<html"));
        let api = get(&app, "/api/v1/nope").await;
        assert_eq!(api.status, StatusCode::NOT_FOUND);
        let error: Value = serde_json::from_str(&api.body).unwrap();
        assert_eq!(error["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn only_get_and_head_reach_the_shell() {
        let home = tempfile::tempdir().unwrap();
        let app = app(home.path(), Assets::of::<Fixture>());
        let post = send(
            &app,
            Request::post("/")
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
        let head = send(
            &app,
            Request::head("/")
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(head.status, StatusCode::OK);
    }

    #[tokio::test]
    async fn an_unbuilt_board_says_so() {
        let home = tempfile::tempdir().unwrap();
        let app = app(home.path(), Assets::of::<Missing>());
        let root = get(&app, "/").await;
        assert_eq!(root.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(root.body.contains("bun run build"));
    }

    #[tokio::test]
    async fn health_answers_before_init_and_other_reads_say_why_not() {
        let home = tempfile::tempdir().unwrap();
        let app = app(home.path(), Assets::of::<Fixture>());
        let health = get(&app, "/api/v1/health").await;
        assert_eq!(health.status, StatusCode::OK);
        let health: Value = serde_json::from_str(&health.body).unwrap();
        assert_eq!(health["initialized"], false);
        assert_eq!(health["ok"], false);
        // Every read says how to make the database, so whoever meets the 503
        // (the board's setup screen, a curl) knows the next step.
        for path in ["/api/v1/board", "/api/v1/agents", "/api/v1/events"] {
            let read = get(&app, path).await;
            assert_eq!(read.status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
            let body: Value = serde_json::from_str(&read.body).unwrap();
            assert_eq!(body["error"]["code"], "unavailable", "{path}");
            assert_eq!(body["error"]["hint"], INIT_HINT, "{path}");
        }
        // The shell still boots, without a snapshot.
        let root = get(&app, "/").await;
        assert_eq!(root.status, StatusCode::OK);
        assert!(!root.body.contains("tiller-boot"));
    }

    #[tokio::test]
    async fn foreign_host_header_is_refused() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        for host in [
            "evil.example.com",
            "evil.example.com:7421",
            "127.0.0.1:7422",
            "192.0.2.1:7421",
        ] {
            for path in ["/", "/api/v1/board", "/assets/app-abc123.js"] {
                let request = Request::get(path)
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap();
                let answer = send(&app, request).await;
                assert_eq!(
                    answer.status,
                    StatusCode::MISDIRECTED_REQUEST,
                    "{host} {path}"
                );
                assert!(!answer.body.contains("tiller-boot"), "{host} {path}");
            }
        }
        let missing = send(&app, Request::get("/").body(Body::empty()).unwrap()).await;
        assert_eq!(missing.status, StatusCode::MISDIRECTED_REQUEST);
        // A page on localhost moves to 127.0.0.1, where the owner cookie is.
        for path in ["/", "/cards/c-1?tab=runs", "/login/abc"] {
            let request = Request::get(path)
                .header(header::HOST, "localhost:7421")
                .body(Body::empty())
                .unwrap();
            let answer = send(&app, request).await;
            assert_eq!(answer.status, StatusCode::PERMANENT_REDIRECT, "{path}");
            assert_eq!(
                answer.headers[header::LOCATION],
                format!("http://127.0.0.1:7421{path}").as_str()
            );
        }
        for host in ["127.0.0.1:7421", "localhost:7421"] {
            let request = Request::get("/api/v1/board")
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap();
            let answer = send(&app, request).await;
            assert_eq!(answer.status, StatusCode::OK, "{host} {}", answer.body);
        }
    }

    #[tokio::test]
    async fn allowed_host_is_served_reads_only() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        for host in ["board.example.com", "Board.Example.com"] {
            for path in ["/", "/api/v1/board", "/assets/app-abc123.js"] {
                let request = Request::get(path)
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .unwrap();
                let answer = send(&app, request).await;
                assert_eq!(answer.status, StatusCode::OK, "{host} {path}");
            }
        }
        // The allow-list names exact hosts: no suffix or other port matches,
        // and the login link, which mints the owner cookie, stays on loopback.
        for (host, path) in [
            ("evil.board.example.com", "/"),
            ("board.example.com:7421", "/"),
            ("", "/"),
            ("board.example.com", "/login/abc"),
        ] {
            let request = Request::get(path)
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap();
            let answer = send(&app, request).await;
            assert_eq!(
                answer.status,
                StatusCode::MISDIRECTED_REQUEST,
                "{host} {path}"
            );
        }
        // A mutation through it is refused even with the owner's cookie.
        let before = cards(&fleet);
        let cookie = owner_cookie(fleet.dir.path());
        let refused = send(
            &app,
            create(&[
                ("host", "board.example.com"),
                ("origin", "http://board.example.com"),
                ("cookie", &cookie),
            ]),
        )
        .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
        assert_eq!(cards(&fleet), before);
    }

    #[tokio::test]
    async fn mutation_without_owner_cookie_is_refused() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let before = cards(&fleet);
        let refused = send(&app, create(&[])).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert!(refused.body.contains("tiller board --login"));
        let forged = send(&app, create(&[("cookie", "tiller_owner=forged")])).await;
        assert_eq!(forged.status, StatusCode::FORBIDDEN);
        assert_eq!(cards(&fleet), before);

        let cookie = owner_cookie(fleet.dir.path());
        let created = send(&app, create(&[("cookie", &cookie)])).await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        let created: Value = serde_json::from_str(&created.body).unwrap();
        assert_eq!(created["ok"], true);
        assert_eq!(cards(&fleet), before + 1);
    }

    #[tokio::test]
    async fn cross_origin_mutation_is_refused() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let before = cards(&fleet);
        let cases = [
            (
                "foreign origin",
                ("origin", "http://evil.example.com"),
                StatusCode::FORBIDDEN,
            ),
            (
                "cross-site fetch",
                ("sec-fetch-site", "cross-site"),
                StatusCode::FORBIDDEN,
            ),
            (
                "form post",
                ("content-type", "text/plain"),
                StatusCode::BAD_REQUEST,
            ),
            (
                "the other loopback origin",
                ("origin", "http://localhost:7421"),
                StatusCode::FORBIDDEN,
            ),
        ];
        for (name, hostile, status) in cases {
            let request = create(&[("cookie", &cookie), hostile]);
            assert_eq!(send(&app, request).await.status, status, "{name}");
        }
        assert_eq!(cards(&fleet), before);
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// The board's stage switch: to pre-launch it reads the default-branch
    /// head from the project's checkout, then launch opens the final pass.
    #[tokio::test]
    async fn board_stage_switch_reads_the_head_then_launches() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let (origin, checkout) = (home.join("origin.git"), home.join("checkout"));
        git(
            home,
            &["init", "-q", "-b", "main", origin.to_str().unwrap()],
        );
        git(
            &origin,
            &[
                "-c",
                "user.email=dev@example.com",
                "-c",
                "user.name=Dev",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        let head = git(&origin, &["rev-parse", "HEAD"]);
        git(
            home,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        std::fs::write(
            home.join("config.toml"),
            format!("[checkouts]\nalpha = {:?}\n", checkout.to_str().unwrap()),
        )
        .unwrap();
        let app = app(home, Assets::of::<Fixture>());
        let cookie = owner_cookie(home);
        let origin_header = format!("http://{HOST}");
        let stage = |key: &str, rev: i64, stage: &str| {
            let mut request = Request::post("/api/v1/projects/alpha/stage")
                .body(Body::from(
                    json!({"idempotency_key": key, "rev": rev, "stage": stage}).to_string(),
                ))
                .unwrap();
            for (name, value) in [
                ("host", HOST),
                ("content-type", "application/json"),
                ("sec-fetch-site", "same-origin"),
                ("origin", origin_header.as_str()),
                ("cookie", cookie.as_str()),
            ] {
                let name: header::HeaderName = name.parse().unwrap();
                request.headers_mut().insert(name, value.parse().unwrap());
            }
            request
        };
        let project = |app: Router| async move {
            let answer = get(&app, "/api/v1/projects").await;
            let body: Value = serde_json::from_str(&answer.body).unwrap();
            body["projects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == "alpha")
                .unwrap()
                .clone()
        };

        let before = project(app.clone()).await;
        assert_eq!(before["stage"], "launched");
        let rev = before["rev"].as_i64().unwrap();
        let staged = send(&app, stage("k-stage-1", rev, "pre-launch")).await;
        assert_eq!(staged.status, StatusCode::OK, "{}", staged.body);
        let now = project(app.clone()).await;
        assert_eq!(now["stage"], "pre-launch");
        assert_eq!(now["reviewed_through_sha"], head.as_str());
        assert!(now["next_review_due"].is_string(), "{now}");
        assert_eq!(now["review_open"], Value::Null);

        let launched = send(
            &app,
            stage("k-stage-2", now["rev"].as_i64().unwrap(), "launched"),
        )
        .await;
        assert_eq!(launched.status, StatusCode::OK, "{}", launched.body);
        let body: Value = serde_json::from_str(&launched.body).unwrap();
        assert_eq!(body["data"]["final_pass"]["state"], "pending");
        let now = project(app.clone()).await;
        assert_eq!(
            (now["stage"].as_str(), now["review_open"].as_str()),
            (Some("launched"), Some("final"))
        );
        assert_eq!(now["next_review_due"], Value::Null);

        // With the remote gone, a retry of the recorded switch replays and a
        // stale rev is still a conflict: neither fetches.
        std::fs::remove_dir_all(&origin).unwrap();
        let retry = send(&app, stage("k-stage-1", rev, "pre-launch")).await;
        assert_eq!(retry.status, StatusCode::OK, "{}", retry.body);
        let body: Value = serde_json::from_str(&retry.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");
        let stale = send(&app, stage("k-stage-3", rev, "pre-launch")).await;
        assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    }

    #[test]
    fn pane_tail_drops_escapes_and_trailing_blank_lines() {
        let screen = "\u{1b}[1mbold\u{1b}[0m line\n\u{1b}]0;title\u{7}second\n\n   \n";
        assert_eq!(pane_tail(screen), vec!["bold line", "second"]);
    }

    /// CI fails when `contract/openapi.json` is stale. Regenerate it with
    /// `TILLER_WRITE_CONTRACT=1 cargo nextest run -p tiller-daemon contract`.
    #[test]
    fn contract_is_current() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/openapi.json");
        let generated = contract();
        if std::env::var_os("TILLER_WRITE_CONTRACT").is_some() {
            std::fs::write(&path, &generated).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == generated,
            "contract/openapi.json is stale: run TILLER_WRITE_CONTRACT=1 cargo nextest run -p tiller-daemon contract"
        );
    }
}
