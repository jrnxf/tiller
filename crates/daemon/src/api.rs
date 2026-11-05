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
use tiller_core::domain::{
    AnsweredVia, Approval, AskSource, CardKind, CardOutcome, HoldReleaseReason, Host, PeerScope,
    Session,
};
use tiller_core::domain::{InterventionKind, Stage};
use tiller_core::state::approvals::Decision;
use tiller_core::state::asks::NewAsk;
use tiller_core::state::cards::{CardEdit, NewCard};
use tiller_core::state::{get as get_row, resolve_id};
use tiller_core::{Actor, Applied, Ctx, Error, Store, Timestamp, ids};
use tiller_herdr::client::{Cli, Herdr};
use tiller_herdr::front::{OpenError, PROBE_TIMEOUT, Terminal, Wait, open_agent};
use tiller_herdr::poller::Registry;
use tokio::sync::watch;

use crate::assets::{self, Assets};
use crate::dto::*;
use crate::peer::forward::{Op, Order};
use crate::peer::link::{Forwarded, Peers};
use crate::peer::{Federation, PeerView};
use crate::readonly::Damage;
use crate::views::{self, Filter, LocalHealth, PaneTarget};
use crate::{auth, memory, sse};

/// Lines of a pane the run detail shows.
const PANE_TAIL: usize = 40;

/// What every handler shares.
#[derive(Clone)]
pub struct AppState(pub(crate) Arc<Inner>);

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
    /// Every paired peer's link (`protocol.md` 10.2).
    pub peers: Arc<Peers>,
    /// The local herdr server, for focus and pane tails.
    pub herdr: Cli,
    /// The terminal app "Open in herdr" brings to the front, from
    /// `[herdr] terminal`; none means a focus only.
    pub terminal: Option<Arc<dyn Terminal>>,
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

    pub(crate) fn db(&self) -> PathBuf {
        self.0.home.join("state.db")
    }

    fn own_health(&self) -> LocalHealth {
        local_health(&self.0.pollers, &self.0.peers)
    }
}

/// The pollers whose last poll failed, and every peer as it is now.
pub fn local_health(pollers: &Registry, peers: &Peers) -> LocalHealth {
    LocalHealth {
        failing_pollers: pollers
            .health()
            .into_iter()
            .filter(|health| health.consecutive_failures > 0)
            .map(|health| health.source)
            .collect(),
        peers: peers.federation(),
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
                    reason: None,
                    host: None,
                },
            }),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "usage", message)
    }

    fn hint(mut self, hint: &str) -> Self {
        self.body.error.hint = Some(hint.to_owned());
        self
    }

    /// The status and body, for a peer's `result` frame.
    pub(crate) fn into_parts(self) -> (StatusCode, ErrorBody) {
        (self.status, *self.body)
    }

    /// The status and JSON body, for a peer's `result` frame.
    pub(crate) fn into_result(self) -> (u16, Value) {
        let (status, body) = self.into_parts();
        let body = serde_json::to_value(body).unwrap_or(Value::Null);
        (status.as_u16(), body)
    }

    /// Names the peer that answered or refused (`protocol.md` 10.3).
    fn host(mut self, host: &str) -> Self {
        self.body.error.host = Some(host.to_owned());
        self
    }

    /// A 502 `peer_refused`: the forward was refused before anything was
    /// written, for `reason` (`protocol.md` 10.3).
    pub(crate) fn peer_refused(reason: &str, message: impl Into<String>) -> Self {
        let mut error = Self::new(StatusCode::BAD_GATEWAY, "peer_refused", message);
        error.body.error.reason = Some(reason.to_owned());
        error
    }

    /// A 503 `peer_unreachable` for a mutation: refused at once, with
    /// nothing sent or queued (ADR 0003).
    fn unreachable(host: &str) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "peer_unreachable",
            format!("{host} is unreachable; nothing was sent or queued"),
        )
        .hint("retry from the control once the link is live")
        .host(host)
    }

    /// A 504 `forward_uncertain`: the frame was sent and no result came
    /// back in time, so it may or may not have applied.
    fn forward_uncertain(host: &str) -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "forward_uncertain",
            format!("{host} did not confirm the mutation in time; it may have applied"),
        )
        .hint("retry with the same idempotency key: it applies once")
        .host(host)
    }

    /// True for the core's refusal of a decision on a head that moved.
    fn head_moved(&self) -> bool {
        self.body.error.message.contains(HEAD_MOVED)
    }

    /// Adds the head an approval binds now to a head-moved refusal.
    fn with_bound_head(mut self, head: Option<String>) -> Self {
        self.body.error.current = Some(json!({"head_sha": head}));
        self
    }

    /// A 503 `peer_unreachable` for a single-entity read of a peer's record
    /// (`protocol.md` 7.3), with the cached board row when there is one.
    fn peer_unreachable(host: &str, current: Option<Value>) -> Self {
        let mut error = Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "peer_unreachable",
            format!("{host} is unreachable; its link is not live"),
        );
        error.body.error.current = current;
        error.hint("the board shows its cached row; try again once the link is live")
    }
}

impl From<(u16, Value)> for ApiError {
    /// A peer's error `result`, passed on as it came.
    fn from((status, body): (u16, Value)) -> Self {
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        match serde_json::from_value::<ErrorBody>(body) {
            Ok(body) => Self {
                status,
                body: Box::new(body),
            },
            Err(error) => Self::new(
                StatusCode::BAD_GATEWAY,
                "internal",
                format!("the peer answered a malformed error: {error}"),
            ),
        }
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

pub(crate) type ApiResult<T> = Result<Json<T>, ApiError>;

/// The command that makes `state.db`, named in every answer before it exists.
pub const INIT_HINT: &str = "tiller init --host-label <label> --lane-capacity <n>";

/// The 503 every read and write answers before `tiller init`.
pub(crate) fn uninitialized() -> ApiError {
    ApiError::from(Error::Unavailable(
        "state.db is not initialized; run tiller init".into(),
    ))
    .hint(INIT_HINT)
}

/// The 503 every mutation answers while `state.db` is damaged.
pub(crate) fn read_only(reason: &str) -> ApiError {
    ApiError::from(Error::Unavailable(format!(
        "read only: state.db failed its integrity check ({reason}); mutations are refused until the owner restores it"
    )))
    .hint("stop the daemon, then run tiller restore <file> --dry-run and again without the flag")
}

/// Runs `read` on the blocking pool against a fresh read connection.
pub(crate) async fn read<T: Send + 'static>(
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
pub(crate) fn guard(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
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
pub(crate) fn body<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|error| ApiError::usage(format!("bad body: {error}")))
}

pub(crate) fn key(idempotency_key: &str) -> Result<String, ApiError> {
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
    Ok(Json(done.response()))
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

    fn response(self) -> MutationResponse {
        MutationResponse {
            ok: true,
            replayed: self.replayed,
            events: self.events,
            receipt: Receipt {
                recorded_at: Timestamp::now().to_string(),
                outbox_id: self.outbox_id,
                approval_id: self.approval_id,
            },
            data: self.data,
            host: None,
        }
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
        .route("/api/v1/attention/{id}/withdraw", post(withdraw_attention))
        .route("/api/v1/approvals/{id}/decide", post(decide))
        .route("/api/v1/agents", get(agents))
        .route("/api/v1/agents/{session}/focus", post(focus))
        .route("/api/v1/runs/{id}", get(run))
        .route("/api/v1/machines", get(machines))
        .route("/api/v1/quota", get(quota))
        .route("/api/v1/health", get(health))
        .route("/api/v1/projects", get(projects))
        .route("/api/v1/contributions", get(contributions))
        .route("/api/v1/projects/{id}/stage", post(stage_project))
        .route("/api/v1/projects/{id}/asks", post(ask_note))
        .route(
            "/api/v1/projects/{id}/delete",
            get(project_delete_preview).post(delete_project),
        )
        .route(
            "/api/v1/interventions/{id}/label",
            post(relabel_intervention),
        )
        .route("/api/v1/memory", get(memory::view))
        .route("/api/v1/memory/edits", post(memory::edit))
        .route("/api/v1/memory/proposals/decide", post(memory::decide))
        .route("/api/v1/memory/changes/{proposal}/undo", post(memory::undo))
        .route("/api/v1/memory/revisions/{id}", get(memory::revision))
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
    let peers = state.0.peers.federation();
    read(&state, move |store| {
        views::federated_board(
            store,
            &peers,
            query.project.as_deref(),
            filter,
            query.host.as_deref(),
        )
    })
    .await
    .map(Json)
}

#[utoipa::path(get, path = "/api/v1/cards/{id}", tag = "reads",
    params(("id" = String, Path, description = "ULID, short form or display id"), HostQuery),
    responses((status = 200, body = CardDetail), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "`peer_unreachable`: the cached card row is in `current`")))]
async fn card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HostQuery>,
) -> ApiResult<CardDetail> {
    let peers = state.0.peers.federation();
    if let Some(view) = query.host.as_deref().and_then(|host| peers.peer(host)) {
        return peer_read(&state, view, format!("cards/{id}"), &id)
            .await
            .map(Json);
    }
    unique(
        &state,
        &peers,
        query.host.as_deref(),
        id,
        views::card_detail,
    )
    .await
    .map(Json)
}

/// A single-entity read of a peer's record (`protocol.md` 7.3): sent over
/// the link as a `query` and answered from the peer's `state.db`. With no
/// answer it is 503 `peer_unreachable`, with the cached row of `card` (an
/// id or display id) in `current`.
async fn peer_read<T: for<'de> Deserialize<'de>>(
    state: &AppState,
    view: &PeerView,
    path: String,
    card: &str,
) -> Result<T, ApiError> {
    let Some((status, body)) = state.0.peers.query(&view.host_id, path).await else {
        let cached = views::peer_board(view, None, Filter::All)
            .rows
            .into_iter()
            .find(|row| names(row, card));
        let current = cached.and_then(|row| serde_json::to_value(row).ok());
        return Err(ApiError::peer_unreachable(&view.label, current));
    };
    if status != StatusCode::OK.as_u16() {
        return Err(ApiError::from((status, body)));
    }
    serde_json::from_value(body).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "internal",
            format!("{} answered a malformed record: {error}", view.label),
        )
    })
}

/// Whether a card reference (ULID, short form or display id) names `row`.
fn names(row: &BoardRow, reference: &str) -> bool {
    let reference = reference.trim();
    row.display_id == reference
        || row.id.eq_ignore_ascii_case(reference)
        || (ids::is_short(reference) && ids::short(&row.id).eq_ignore_ascii_case(reference))
}

/// A card read with no `?host=`: this host's record, unless the reference
/// also names a card in a peer's copy. Then it is 404 listing every host's
/// match, host-qualified (`protocol.md` 7.3).
async fn unique<T: Send + 'static>(
    state: &AppState,
    peers: &Federation,
    host: Option<&str>,
    reference: String,
    read_one: impl FnOnce(&Store, &str) -> tiller_core::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    if let Some(host) = host {
        // A `host` that names no peer must name a host of this one: a
        // mistyped label is 404, never this host's card of the same id.
        let host = host.to_owned();
        read(state, move |store| views::host_id(store, &host)).await?;
    }
    let elsewhere: Vec<String> = match host {
        Some(_) => Vec::new(),
        None => peers
            .peers
            .iter()
            .filter_map(|view| {
                let rows = &view.queries.as_ref()?.board.rows;
                let row = rows.iter().find(|row| names(row, &reference))?;
                Some(format!("{} · {}", view.label, row.display_id))
            })
            .collect(),
    };
    let id = reference.clone();
    let local = read(state, move |store| read_one(store, &id)).await;
    if elsewhere.is_empty() {
        return local;
    }
    if let Err(error) = &local
        && error.status != StatusCode::NOT_FOUND
    {
        return local;
    }
    let mut candidates = Vec::new();
    if local.is_ok() {
        let label = read(state, |store| store.this_host().map(|host| host.label)).await?;
        candidates.push(format!("{label} · {reference}"));
    }
    candidates.extend(elsewhere);
    Err(ApiError::from(Error::NotFound {
        entity: "card",
        id: reference,
        candidates,
    })
    .hint("name the host with ?host=<label>"))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct TimelineQuery {
    /// Only events older than this `seq`.
    pub before: Option<i64>,
    /// A peer's label for a peer's card; absent means this host.
    pub host: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/cards/{id}/timeline", tag = "reads",
    params(("id" = String, Path), TimelineQuery),
    responses((status = 200, body = TimelineResponse), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "`peer_unreachable`: the cached card row is in `current`")))]
async fn timeline(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<TimelineQuery>,
) -> ApiResult<TimelineResponse> {
    let peers = state.0.peers.federation();
    if let Some(view) = query.host.as_deref().and_then(|host| peers.peer(host)) {
        let path = match query.before {
            Some(before) => format!("cards/{id}/timeline?before={before}"),
            None => format!("cards/{id}/timeline"),
        };
        return peer_read(&state, view, path, &id).await.map(Json);
    }
    let before = query.before;
    unique(
        &state,
        &peers,
        query.host.as_deref(),
        id,
        move |store, id| views::timeline(store, id, before),
    )
    .await
    .map(Json)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct AttentionQuery {
    /// Only `open` is served.
    pub state: Option<String>,
    /// A host id or label.
    pub host: Option<String>,
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
    let peers = state.0.peers.federation();
    read(&state, move |store| {
        views::federated_attention(store, &peers, query.host.as_deref())
    })
    .await
    .map(Json)
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
    let peers = state.0.peers.federation();
    read(&state, move |store| {
        views::federated_agents(store, &peers, query.host.as_deref())
    })
    .await
    .map(Json)
}

#[utoipa::path(get, path = "/api/v1/runs/{id}", tag = "reads",
    params(("id" = String, Path), HostQuery),
    responses((status = 200, body = RunDetail), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "`peer_unreachable`: the cached card row is in `current`")))]
async fn run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HostQuery>,
) -> ApiResult<RunDetail> {
    let peers = state.0.peers.federation();
    if let Some(view) = query.host.as_deref().and_then(|host| peers.peer(host)) {
        // `alpha-42 build`: the card is the part before the role.
        let card = id.split(' ').next().unwrap_or_default();
        return peer_read(&state, view, format!("runs/{id}"), card)
            .await
            .map(Json);
    }
    let (mut detail, pane) = read(&state, move |store| views::run_detail(store, &id)).await?;
    if let Some(target) = pane {
        let herdr = state.0.herdr.clone();
        if let Ok(tail) = tokio::task::spawn_blocking(move || pane_tail_of(herdr, target)).await {
            detail.pane_tail = tail;
        }
    }
    Ok(Json(detail))
}

/// A pane's tail read through herdr, or nothing when herdr cannot read it.
/// It blocks.
pub(crate) fn pane_tail_of(herdr: Cli, target: PaneTarget) -> Vec<String> {
    let herdr = match target.machine {
        Some(machine) => herdr.on_machine(machine),
        None => herdr,
    };
    herdr
        .read_visible(&target.pane)
        .map(|screen| pane_tail(&screen, PANE_TAIL))
        .unwrap_or_default()
}

/// The last `keep` lines of a pane read, without terminal escapes and
/// without the blank lines below the last text (`tiller run show --tail`
/// shares it).
pub fn pane_tail(screen: &str, keep: usize) -> Vec<String> {
    let plain = strip_ansi(screen);
    let lines: Vec<String> = plain
        .lines()
        .map(|line| line.trim_end().to_owned())
        .collect();
    let end = lines
        .iter()
        .rposition(|line| !line.is_empty())
        .map_or(0, |i| i + 1);
    lines[end.saturating_sub(keep)..end].to_vec()
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

#[utoipa::path(get, path = "/api/v1/projects", tag = "reads", params(HostQuery),
    responses((status = 200, body = ProjectsResponse)))]
async fn projects(
    State(state): State<AppState>,
    Query(query): Query<HostQuery>,
) -> ApiResult<ProjectsResponse> {
    let peers = state.0.peers.federation();
    read(&state, move |store| {
        views::federated_projects(store, &peers, query.host.as_deref(), Timestamp::now())
    })
    .await
    .map(Json)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ContributionsQuery {
    /// Only this project's PRs.
    pub project: Option<String>,
}

#[utoipa::path(get, path = "/api/v1/contributions", tag = "reads", params(ContributionsQuery),
    responses((status = 200, body = ContributionsResponse)))]
async fn contributions(
    State(state): State<AppState>,
    Query(query): Query<ContributionsQuery>,
) -> ApiResult<ContributionsResponse> {
    read(&state, move |store| {
        views::contributions(store, query.project.as_deref())
    })
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
        links: state.0.peers.links(),
    })
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct EventsQuery {
    /// Resume after this `seq` when the request has no `Last-Event-ID`:
    /// the boot snapshot's `last_event_seq` on the first connect.
    pub since: Option<i64>,
}

/// The change stream: `event: change` with a [`ChangeEvent`] per `events`
/// row, `event: reset` with a [`ResetEvent`] when a resume is impossible,
/// and, once a peer is paired, `event: peer`, `event: link` and
/// `event: peer_reset` from the peer links (`protocol.md` 7.6), with no id.
#[utoipa::path(get, path = "/api/v1/events", tag = "stream", params(EventsQuery,
        ("Last-Event-ID" = Option<i64>, Header, description = "the last seq the client applied")),
    responses(
        (status = 200, content_type = "text/event-stream", body = ChangeEvent,
         description = "`change` messages carry a ChangeEvent; `reset` messages a ResetEvent; `peer` messages a PeerEvent, `link` messages a LinkEvent and `peer_reset` messages a PeerResetEvent"),
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
    let peers = state.0.peers.clone();
    sse::stream(
        state.db(),
        after,
        state.0.newest.clone(),
        state.0.stop.clone(),
        peers.clone(),
        move || local_health(&pollers, &peers),
    )
    .into_response()
}

// -- mutations --------------------------------------------------------------------

/// One parsed mutation on the forwarding allowlist (`protocol.md` 7.4):
/// what this host's board applies for its own records, and what `peer
/// serve` applies for a forwarded one, through the same core function.
pub(crate) struct Mutation {
    op: Op,
    /// The card, attention item or approval the path names; `None` for a
    /// new card.
    target: Option<String>,
    key: String,
    host: Option<String>,
    parsed: Parsed,
}

/// A body, with the fields its handler parses before any write.
enum Parsed {
    CreateCard(CreateCard, CardKind),
    EditCard(EditCard),
    Comment(Comment),
    Hold(HoldBody, Timestamp),
    ReleaseHold,
    Close(Transition, CardOutcome),
    Cancel(String),
    Reopen(String),
    Answer(AnswerBody),
    Decide(DecideBody, bool),
}

impl Mutation {
    /// Reads `bytes` as `op`'s body: 400 when it does not parse, and the
    /// handler's own field checks.
    pub(crate) fn parse(op: Op, target: Option<String>, bytes: &[u8]) -> Result<Self, ApiError> {
        let (idempotency_key, host, parsed) = match op {
            Op::CreateCard => {
                let request: CreateCard = body(bytes)?;
                let kind: CardKind = request.kind.parse()?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::CreateCard(request, kind),
                )
            }
            Op::EditCard => {
                let request: EditCard = body(bytes)?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::EditCard(request),
                )
            }
            Op::Comment => {
                let request: Comment = body(bytes)?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::Comment(request),
                )
            }
            Op::Hold => {
                let request: HoldBody = body(bytes)?;
                let until: Timestamp = request.until.parse()?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::Hold(request, until),
                )
            }
            Op::ReleaseHold => {
                let request: Transition = body(bytes)?;
                (request.idempotency_key, request.host, Parsed::ReleaseHold)
            }
            Op::Close => {
                let request: Transition = body(bytes)?;
                let outcome: CardOutcome =
                    request.outcome.as_deref().unwrap_or("completed").parse()?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::Close(request, outcome),
                )
            }
            Op::Cancel | Op::Reopen => {
                let request: Transition = body(bytes)?;
                let reason = reason(&request)?;
                let parsed = if op == Op::Cancel {
                    Parsed::Cancel(reason)
                } else {
                    Parsed::Reopen(reason)
                };
                (request.idempotency_key, request.host, parsed)
            }
            Op::Answer => {
                let request: AnswerBody = body(bytes)?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::Answer(request),
                )
            }
            Op::Decide => {
                let request: DecideBody = body(bytes)?;
                let accept = match request.decision.as_str() {
                    "accept" => true,
                    "reject" => false,
                    other => {
                        return Err(ApiError::usage(format!(
                            "decision is accept or reject, not {other:?}"
                        )));
                    }
                };
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::Decide(request, accept),
                )
            }
        };
        Ok(Self {
            op,
            target,
            key: key(&idempotency_key)?,
            host,
            parsed,
        })
    }

    /// The body's `host`: a paired peer to forward to, or this host.
    pub(crate) fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// Runs the core function, in one transaction with its own checks.
    fn apply(self, store: &mut Store, ctx: &Ctx) -> tiller_core::Result<Done> {
        let key = self.key.as_str();
        let id = self.target.as_deref().unwrap_or_default();
        match self.parsed {
            Parsed::CreateCard(request, kind) => {
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
            }
            Parsed::EditCard(request) => {
                let edit = CardEdit {
                    title: request.title,
                    body: request.body.map(Some),
                    priority: request.priority,
                    ..CardEdit::default()
                };
                Done::of(store.card_edit(ctx, Some(key), id, request.rev, edit)?)
            }
            Parsed::Comment(request) => {
                let applied = store.card_comment(ctx, Some(key), id, &request.text)?;
                let outbox_id = Some(applied.value.id.clone());
                let mut done = Done::of(applied)?;
                done.outbox_id = outbox_id;
                Ok(done)
            }
            Parsed::Hold(request, until) => {
                Done::of(store.card_hold(ctx, id, until, &request.reason)?)
            }
            Parsed::ReleaseHold => {
                Done::of(store.hold_release(ctx, id, HoldReleaseReason::Removed)?)
            }
            Parsed::Close(request, outcome) => {
                Done::of(store.card_close(ctx, id, outcome, request.reason.as_deref(), None)?)
            }
            Parsed::Cancel(reason) => Done::of(store.card_cancel(ctx, id, &reason)?),
            Parsed::Reopen(reason) => Done::of(store.card_reopen(ctx, id, &reason)?),
            Parsed::Answer(request) => {
                let applied = store.attention_answer(
                    ctx,
                    Some(key),
                    id,
                    &request.answer,
                    AnsweredVia::Board,
                )?;
                let outbox_id = applied.value.message.as_ref().map(|m| m.id.clone());
                let item = views::attention_item(store, &applied.value.attention)?;
                Ok(Done {
                    data: serde_json::to_value(item)?,
                    events: applied.events,
                    replayed: applied.replayed,
                    outbox_id,
                    approval_id: None,
                })
            }
            Parsed::Decide(request, accept) => {
                // The head check runs in the decision's own transaction.
                let applied = store.approval_decide_shown(
                    ctx,
                    Some(key),
                    id,
                    Decision {
                        accept,
                        via: AnsweredVia::Board,
                        memory: Vec::new(),
                        ask_id: None,
                    },
                    &request.head_sha,
                )?;
                let approval_id = Some(applied.value.approval.id.clone());
                let mut done = Done::of(applied)?;
                done.approval_id = approval_id;
                Ok(done)
            }
        }
    }

    /// Applies on `store` at once, as `peer serve` does for a verified
    /// frame: the response, or the refusal with what the board needs to
    /// recover from it.
    pub(crate) fn apply_here(
        self,
        store: &mut Store,
        ctx: &Ctx,
    ) -> Result<MutationResponse, ApiError> {
        let (op, target) = (self.op, self.target.clone());
        match self.apply(store, ctx) {
            Ok(done) => Ok(done.response()),
            Err(error) => {
                let error = ApiError::from(error);
                if op == Op::Decide && error.head_moved() {
                    let head = bound_head(store, target.as_deref().unwrap_or_default());
                    return Err(error.with_bound_head(head.ok().flatten()));
                }
                Err(error)
            }
        }
    }
}

/// The head an approval binds now, for a 422 after the head moved.
fn bound_head(store: &Store, approval: &str) -> tiller_core::Result<Option<String>> {
    let id = resolve_id(store.conn(), "approvals", "approval", approval)?;
    let approval: Approval = get_row(store.conn(), "approval", &id)?;
    Ok(approval.head_sha)
}

/// A mutation on the forwarding allowlist (`protocol.md` 7.4): applied
/// here, or with a peer `host`, forwarded to it (10.3).
async fn submit(
    state: &AppState,
    headers: &HeaderMap,
    op: Op,
    target: Option<String>,
    bytes: &[u8],
) -> ApiResult<MutationResponse> {
    guard(state, headers)?;
    let mutation = Mutation::parse(op, target, bytes)?;
    if let Some(view) = peer_for(state, mutation.host()).await? {
        return forward(state, &view, mutation, bytes).await;
    }
    let lookup = mutation.target.clone().unwrap_or_default();
    let key = mutation.key.clone();
    let applied = mutate(state, key, move |store, ctx, _| mutation.apply(store, ctx)).await;
    match applied {
        Err(error) if op == Op::Decide && error.head_moved() => {
            // Tell the board which head the approval binds now.
            let head = read(state, move |store| bound_head(store, &lookup))
                .await
                .ok()
                .flatten();
            Err(error.with_bound_head(head))
        }
        other => other,
    }
}

/// The paired peer a mutation's `host` names, or `None` for this host
/// (absent, or its own label or id). A host that is not paired is refused:
/// its records change only on that host (G25).
async fn peer_for(state: &AppState, host: Option<&str>) -> Result<Option<PeerView>, ApiError> {
    match addressed(state, host).await? {
        Addressed::Here => Ok(None),
        Addressed::Peer(view) => Ok(Some(view)),
        // Paired, with no link task yet: nothing to send it over.
        Addressed::Unlinked(label) => Err(ApiError::unreachable(&label)),
    }
}

/// Where a mutation's `host` points.
enum Addressed {
    Here,
    Peer(PeerView),
    /// A paired peer with no link task yet, by label.
    Unlinked(String),
}

async fn addressed(state: &AppState, host: Option<&str>) -> Result<Addressed, ApiError> {
    let Some(host) = host else {
        return Ok(Addressed::Here);
    };
    if let Some(view) = state.0.peers.federation().peer(host) {
        return Ok(Addressed::Peer(view.clone()));
    }
    let named = host.to_owned();
    let (row, local) = read(state, move |store| {
        let id = views::host_id(store, &named)?;
        let row: Host = get_row(store.conn(), "host", &id)?;
        Ok((row, store.this_host()?.id))
    })
    .await?;
    if row.id == local {
        return Ok(Addressed::Here);
    }
    if row.peer_scope.is_some() {
        return Ok(Addressed::Unlinked(row.label));
    }
    Err(ApiError::from(Error::Refused {
        entity: "host",
        id: row.id,
        fact: format!(
            "{} is not a paired peer; its records change only on that host",
            row.label
        ),
    }))
}

/// Sends a mutation for a peer's record over its link (`protocol.md` 10.3)
/// and returns the peer's answer with `host` added. Nothing is held or
/// queued here: a peer whose link is not `live` is 503 at once, and one
/// that does not confirm in time is 504.
async fn forward(
    state: &AppState,
    view: &PeerView,
    mutation: Mutation,
    bytes: &[u8],
) -> ApiResult<MutationResponse> {
    let host = view.label.as_str();
    if view.link != LinkState::Live {
        return Err(ApiError::unreachable(host));
    }
    if view.scope != PeerScope::Act {
        return Err(ApiError::peer_refused(
            "read_scope",
            format!("{host} is paired with a read scope: view only pairing"),
        )
        .host(host));
    }
    let mut body: Value = body(bytes)?;
    if let Some(fields) = body.as_object_mut() {
        fields.remove("host");
    }
    let order = Order {
        op: mutation.op,
        target: mutation.target,
        body,
        idempotency_key: mutation.key,
    };
    match state.0.peers.forward(&view.host_id, order).await {
        Forwarded::Unreachable => Err(ApiError::unreachable(host)),
        Forwarded::Uncertain => Err(ApiError::forward_uncertain(host)),
        Forwarded::Answered(200, body) => {
            let mut response: MutationResponse = serde_json::from_value(body).map_err(|error| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "internal",
                    format!("{host} answered a malformed response: {error}"),
                )
                .host(host)
            })?;
            response.host = Some(host.to_owned());
            Ok(Json(response))
        }
        Forwarded::Answered(status, body) => Err(ApiError::from((status, body)).host(host)),
    }
}

/// An op off the forwarding allowlist, with a peer `host`: 502
/// `peer_refused` before anything else (`protocol.md` 7.4).
pub(crate) async fn not_forwarded(
    state: &AppState,
    host: Option<&str>,
    what: &str,
) -> Result<(), ApiError> {
    let label = match addressed(state, host).await? {
        Addressed::Here => return Ok(()),
        Addressed::Peer(view) => view.label,
        Addressed::Unlinked(label) => label,
    };
    Err(ApiError::peer_refused(
        "not_allowlisted",
        format!("{what} is not forwarded to {label}"),
    )
    .host(&label))
}

#[utoipa::path(post, path = "/api/v1/cards", tag = "mutations", request_body = CreateCard,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 422, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`"),
              (status = 503, body = ErrorBody, description = "`peer_unreachable`: nothing was sent or queued"),
              (status = 504, body = ErrorBody, description = "`forward_uncertain`: retry with the same key")))]
async fn create_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::CreateCard, None, &bytes).await
}

#[utoipa::path(patch, path = "/api/v1/cards/{id}", tag = "mutations", request_body = EditCard,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 409, body = ErrorBody),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn edit_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::EditCard, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/comment", tag = "mutations", request_body = Comment,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::Comment, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/hold", tag = "mutations", request_body = HoldBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn hold(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::Hold, Some(id), &bytes).await
}

#[utoipa::path(delete, path = "/api/v1/cards/{id}/hold", tag = "mutations", request_body = Transition,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn release_hold(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::ReleaseHold, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/close", tag = "mutations", request_body = Transition,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn close(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::Close, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/stage", tag = "mutations", request_body = StageProject,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 409, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn stage_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: StageProject = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a stage switch").await?;
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

#[utoipa::path(post, path = "/api/v1/projects/{id}/asks", tag = "mutations", request_body = AskNote,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn ask_note(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: AskNote = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "an ask").await?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, key| {
            let text = request.text.trim().to_owned();
            if text.chars().count() > ASK_NOTE_MAX_CHARS {
                return Err(Error::Usage(format!(
                    "an ask is at most {ASK_NOTE_MAX_CHARS} characters"
                )));
            }
            // The same core call as `tiller ask note`, from the board's ask box.
            let applied = store.ask_record(
                ctx,
                Some(key),
                NewAsk {
                    project_id: id,
                    session_id: None,
                    source: AskSource::Board,
                    text,
                },
            )?;
            let outbox_id = applied.value.message.as_ref().map(|m| m.id.clone());
            let mut done = Done::of(applied)?;
            done.outbox_id = outbox_id;
            Ok(done)
        },
    )
    .await
}

/// The longest ask the board's box records: it is noted whole to the
/// orchestrator, and again at every orchestrator start until it is resolved.
const ASK_NOTE_MAX_CHARS: usize = 8000;

#[utoipa::path(get, path = "/api/v1/projects/{id}/delete", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = ProjectDeletePreview), (status = 404, body = ErrorBody)))]
async fn project_delete_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ProjectDeletePreview> {
    let preview = read(&state, move |store| store.project_delete_preview(&id)).await?;
    Ok(Json(ProjectDeletePreview {
        project: preview.deletion.project,
        removed: preview.deletion.removed,
        unlinked: preview.deletion.unlinked,
        refused: preview.refused,
        unblock: preview.unblock.map(|unblock| DeleteUnblock {
            step: unblock.step,
            command: unblock.command,
        }),
    }))
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/delete", tag = "mutations", request_body = DeleteProject,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: a live run or session, the fleet project, or another host's project"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: DeleteProject = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a project delete").await?;
    if request.confirm.trim() != id {
        return Err(ApiError::usage(format!("type {id} to delete the project")));
    }
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.project_delete(ctx, &id)?),
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/interventions/{id}/label", tag = "mutations", request_body = RelabelBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "the question still applies"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn relabel_intervention(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: RelabelBody = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a relabel").await?;
    let label: Option<InterventionKind> = request.label.as_deref().map(str::parse).transpose()?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.intervention_relabel(ctx, None, &id, label)?),
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
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::Cancel, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/cards/{id}/reopen", tag = "mutations", request_body = Transition,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn reopen(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::Reopen, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/attention/{id}/answer", tag = "mutations", request_body = AnswerBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 422, body = ErrorBody),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn answer(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::Answer, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/attention/{id}/withdraw", tag = "mutations", request_body = WithdrawBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "the question still applies"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn withdraw_attention(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: WithdrawBody = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a withdraw").await?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            Done::of(store.attention_withdraw_stale(
                ctx,
                &id,
                "withdrawn from the board by the owner",
            )?)
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/approvals/{id}/decide", tag = "mutations", request_body = DecideBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 422, body = ErrorBody, description = "the head moved: `current.head_sha` is the bound head"),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn decide(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::Decide, Some(id), &bytes).await
}

/// The core's refusal when the approval binds another head than the one
/// the board showed.
const HEAD_MOVED: &str = "the head moved since it was shown";

#[utoipa::path(post, path = "/api/v1/agents/{session}/focus", tag = "mutations", request_body = FocusBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 503, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn focus(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers)?;
    let request: FocusBody = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a focus").await?;
    let target = read(&state, move |store| {
        let id = resolve_id(store.conn(), "sessions", "session", &session)?;
        let session: Session = get_row(store.conn(), "session", &id)?;
        let (pane, machine) = tiller_core::state::runs::focus_target(store.conn(), &session)?;
        Ok((session.id, pane, machine))
    })
    .await?;
    let (session_id, pane, machine) = target;
    let herdr = match machine {
        Some(machine) => state.0.herdr.clone().on_machine(machine),
        None => state.0.herdr.clone(),
    };
    let screen = state.0.herdr.clone().timeout(PROBE_TIMEOUT);
    let terminal = state.0.terminal.clone();
    let app = terminal.as_ref().map(|terminal| terminal.name().to_owned());
    let opened = tokio::task::spawn_blocking(move || {
        open_agent(&screen, terminal.as_deref(), Wait::default(), || {
            herdr.focus_agent(&pane)
        })
    })
    .await;
    let data = match opened {
        Ok(Ok(opened)) => json!({"session_id": session_id, "opened": opened, "terminal": app}),
        Ok(Err(OpenError::Herdr(error))) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                format!("herdr did not focus the agent: {error}"),
            ));
        }
        // The focus landed, so this is no failed request: the answer says
        // what went wrong with the window.
        Ok(Err(error)) => json!({
            "session_id": session_id,
            "opened": "failed",
            "terminal": app,
            "problem": error.to_string(),
        }),
        Err(error) => return Err(Error::Internal(error.to_string()).into()),
    };
    Ok(Json(MutationResponse {
        ok: true,
        replayed: false,
        events: Vec::new(),
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: None,
            approval_id: None,
        },
        data,
        host: None,
    }))
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
        projects, contributions, signals, events, create_card, edit_card, comment, hold, release_hold, close,
        cancel, reopen, answer, withdraw_attention, decide, focus, stage_project, ask_note, project_delete_preview, delete_project,
        relabel_intervention, memory::view, memory::revision,
        memory::edit, memory::decide, memory::undo
    ),
    components(schemas(ChangeEvent, ResetEvent, PeerEvent, LinkEvent, PeerResetEvent, ErrorBody, ErrorDetail)),
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
        app_full(home, assets, damage, Peers::idle(), Cli::local(), None)
    }

    /// The board API with these peer links.
    pub(crate) fn app_with_peers(home: &std::path::Path, peers: Arc<Peers>) -> Router {
        app_full(
            home,
            Assets::of::<Fixture>(),
            Damage::default(),
            peers,
            Cli::local(),
            None,
        )
    }

    fn app_full(
        home: &std::path::Path,
        assets: Assets,
        damage: Damage,
        peers: Arc<Peers>,
        herdr: Cli,
        terminal: Option<Arc<dyn Terminal>>,
    ) -> Router {
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
            peers,
            herdr,
            terminal,
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
        mutation(
            "POST",
            "/api/v1/cards",
            json!({"idempotency_key": "k-1", "project": "alpha", "kind": "ship", "title": "Undo strokes"}),
            headers,
        )
    }

    /// A mutation as the board sends it, with `headers` set over the
    /// board's own.
    pub(crate) fn mutation(
        method: &str,
        path: &str,
        body: Value,
        headers: &[(&str, &str)],
    ) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::from(body.to_string()))
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

    fn ask_request(key: &str, text: &str, headers: &[(&str, &str)]) -> Request<Body> {
        mutation(
            "POST",
            "/api/v1/projects/alpha/asks",
            json!({"idempotency_key": key, "text": text}),
            headers,
        )
    }

    fn ask_texts(fleet: &Fleet) -> Vec<(String, String)> {
        let store = fleet.open();
        let mut statement = store
            .conn()
            .prepare("SELECT source, text FROM asks ORDER BY received_at, id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// The board's ask box: owner only, and the same `ask.record` and
    /// orchestrator note as `tiller ask note`.
    #[tokio::test]
    async fn board_ask_records_the_ask_and_wakes_the_orchestrator() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];

        let refused = send(&app, ask_request("a-1", "ship it", &[])).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert!(ask_texts(&fleet).is_empty());

        // The orchestrator is live: the ask is recorded and noted to it.
        let woken = send(&app, ask_request("a-1", "  first\nsecond line  ", &owner)).await;
        assert_eq!(woken.status, StatusCode::OK, "{}", woken.body);
        let woken: Value = serde_json::from_str(&woken.body).unwrap();
        assert_eq!(woken["data"]["warning"], Value::Null);
        assert_eq!(woken["data"]["ask"]["source"], "board");
        assert_eq!(woken["data"]["ask"]["project_id"], "alpha");
        assert_eq!(
            woken["data"]["message"]["target_session_id"],
            fleet.orchestrator_session
        );
        assert_eq!(woken["data"]["message"]["kind"], "note");
        assert_eq!(
            woken["receipt"]["outbox_id"],
            woken["data"]["message"]["id"]
        );
        assert_eq!(
            ask_texts(&fleet),
            [("board".to_owned(), "first\nsecond line".to_owned())]
        );

        // The same key replays the first answer and records nothing more.
        let replay = send(&app, ask_request("a-1", "  first\nsecond line  ", &owner)).await;
        assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
        assert_eq!(ask_texts(&fleet).len(), 1);

        // With no orchestrator the ask is still recorded, and the response says
        // nobody was told, as `tiller ask note` does.
        fleet
            .open()
            .session_end(&Ctx::new(Actor::Owner), &fleet.orchestrator_session, "exit")
            .unwrap();
        let alone = send(&app, ask_request("a-2", "anyone there?", &owner)).await;
        assert_eq!(alone.status, StatusCode::OK, "{}", alone.body);
        let alone: Value = serde_json::from_str(&alone.body).unwrap();
        assert_eq!(alone["data"]["warning"], "no_orchestrator");
        assert_eq!(alone["data"]["message"], Value::Null);
        assert_eq!(alone["receipt"]["outbox_id"], Value::Null);
        assert_eq!(ask_texts(&fleet).len(), 2);
    }

    #[tokio::test]
    async fn board_ask_refuses_empty_text_and_an_unknown_project() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let empty = send(&app, ask_request("a-1", "   ", &owner)).await;
        assert_eq!(empty.status, StatusCode::BAD_REQUEST, "{}", empty.body);
        let long = send(&app, ask_request("a-3", &"x".repeat(8001), &owner)).await;
        assert_eq!(long.status, StatusCode::BAD_REQUEST, "{}", long.body);
        let unknown = send(
            &app,
            mutation(
                "POST",
                "/api/v1/projects/nope/asks",
                json!({"idempotency_key": "a-2", "text": "hi"}),
                &owner,
            ),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
        assert!(ask_texts(&fleet).is_empty());
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

    /// The Health page's relabel: an unplanned escalation relabeled `planned`
    /// moves the week's counts, the same label again changes nothing, and a
    /// label naming a peer is refused rather than forwarded.
    #[tokio::test]
    async fn board_relabels_an_intervention_and_the_week_counts_it() {
        let fleet = Fleet::new();
        let store = fleet.open();
        let now = Timestamp::now();
        store
            .conn()
            .execute_batch(&format!(
                "INSERT INTO cards (id, display_id, project_id, kind, title, created_at)
                 VALUES ('c-x', 'alpha-x', 'alpha', 'ship', 't', '{now}');
                 INSERT INTO attention (id, card_id, kind, question, opened_at)
                 VALUES ('a-x', 'c-x', 'credential', 'Token for the registry?', '{now}')"
            ))
            .unwrap();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let relabel = |key: &str, extra: Value| {
            let mut body = json!({"idempotency_key": key});
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            mutation(
                "POST",
                "/api/v1/interventions/a-x/label",
                body,
                &[("cookie", cookie.as_str())],
            )
        };
        let week = |answer: Answer| {
            let body: Value = serde_json::from_str(&answer.body).unwrap();
            body["weeks"].as_array().unwrap().last().unwrap().clone()
        };

        let before = week(get(&app, "/api/v1/signals").await);
        assert_eq!(before["interventions"]["unplanned_escalation"], 1);
        assert_eq!(before["interventions"]["planned"], 0);

        let done = send(&app, relabel("k-1", json!({"label": "planned"}))).await;
        assert_eq!(done.status, StatusCode::OK, "{}", done.body);
        let after = get(&app, "/api/v1/signals").await;
        let listed: Value = serde_json::from_str(&after.body).unwrap();
        assert_eq!(listed["interventions"][0]["kind"], "planned");
        assert_eq!(
            listed["interventions"][0]["classified"],
            "unplanned_escalation"
        );
        assert_eq!(listed["interventions"][0]["relabeled"], true);
        let after = week(after);
        assert_eq!(after["interventions"]["unplanned_escalation"], 0);
        assert_eq!(after["interventions"]["planned"], 1);

        let again = send(&app, relabel("k-2", json!({"label": "planned"}))).await;
        let body: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(
            (again.status, &body["replayed"]),
            (StatusCode::OK, &json!(true))
        );

        let cleared = send(&app, relabel("k-3", json!({"label": null}))).await;
        assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
        let now = week(get(&app, "/api/v1/signals").await);
        assert_eq!(now["interventions"]["unplanned_escalation"], 1);

        let bad = send(&app, relabel("k-4", json!({"label": "bogus"}))).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
        // An unknown host label is not a pairing; the refusal for a paired peer
        // is in `peer::tests::spawn_not_forwardable_from_the_board`.
        let peer = send(
            &app,
            relabel("k-5", json!({"label": "rework", "host": "laptop"})),
        )
        .await;
        assert_eq!(peer.status, StatusCode::NOT_FOUND, "{}", peer.body);
        let unknown = send(
            &app,
            mutation(
                "POST",
                "/api/v1/interventions/nope/label",
                json!({"idempotency_key": "k-6", "label": "rework"}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
        let anonymous = send(
            &app,
            mutation(
                "POST",
                "/api/v1/interventions/a-x/label",
                json!({"idempotency_key": "k-7", "label": "rework"}),
                &[],
            ),
        )
        .await;
        assert_eq!(anonymous.status, StatusCode::FORBIDDEN);
    }

    /// A requested merge approval renders on the card page and in the inbox,
    /// and the board's Approve records an accepted approval bound to the head.
    #[tokio::test]
    async fn board_renders_and_approves_a_requested_merge() {
        let fleet = Fleet::new();
        let store = fleet.open();
        let now = Timestamp::now();
        store
            .conn()
            .execute_batch(&format!(
                "INSERT INTO cards (id, display_id, project_id, kind, title, created_at)
                 VALUES ('c-m', 'alpha-9', 'alpha', 'ship', 'Ship it', '{now}');
                 INSERT INTO prs (id, project_id, card_id, number, url, linked_at, linked_by, head_sha)
                 VALUES ('p-m', 'alpha', 'c-m', 7, 'https://github.com/example/app/pull/7', '{now}', 'owner', 'abc1234');
                 INSERT INTO approvals (id, kind, project_id, card_id, repo, pr_id, head_sha, action, requested_at, requested_by)
                 VALUES ('01M6MH5FH5B0EWK3S8NQ4BARPT', 'merge', 'alpha', 'c-m', 'example/app', 'p-m', 'abc1234', 'squash', '{now}', 'tiller');
                 INSERT INTO attention (id, card_id, kind, question, opened_at)
                 VALUES ('a-m', 'c-m', 'merge', 'Merge the PR?', '{now}')"
            ))
            .unwrap();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());

        let card = get(&app, "/api/v1/cards/alpha-9").await;
        assert_eq!(card.status, StatusCode::OK, "{}", card.body);
        let inbox = get(&app, "/api/v1/attention?state=open").await;
        assert_eq!(inbox.status, StatusCode::OK, "{}", inbox.body);
        let inbox: Value = serde_json::from_str(&inbox.body).unwrap();
        let item = &inbox["items"][0];
        assert_eq!(item["id"], "a-m");
        assert_eq!(item["approval"]["id"], "01M6MH5FH5B0EWK3S8NQ4BARPT");
        assert_eq!(item["approval"]["head_sha"], "abc1234");

        let decided = send(
            &app,
            mutation(
                "POST",
                "/api/v1/approvals/01M6MH5FH5B0EWK3S8NQ4BARPT/decide",
                json!({"idempotency_key": "k-1", "decision": "accept", "head_sha": "abc1234"}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        assert_eq!(decided.status, StatusCode::OK, "{}", decided.body);
        let (state, head): (String, String) = store
            .conn()
            .query_row(
                "SELECT state, head_sha FROM approvals WHERE id = '01M6MH5FH5B0EWK3S8NQ4BARPT'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((state.as_str(), head.as_str()), ("accepted", "abc1234"));
        let card = get(&app, "/api/v1/cards/alpha-9").await;
        assert_eq!(card.status, StatusCode::OK, "{}", card.body);
    }

    /// The contributions view lists the PRs published from cards, newest first,
    /// narrowed by project, and the card page carries its pages with their URLs.
    #[tokio::test]
    async fn contributions_list_published_prs_and_cards_list_their_pages() {
        let fleet = Fleet::new();
        let store = fleet.open();
        store
            .conn()
            .execute_batch(
                "INSERT INTO cards (id, display_id, project_id, kind, title, created_at)
                 VALUES ('c-1', 'alpha-1', 'alpha', 'ship', 'First', '2026-11-01T10:00:00Z'),
                        ('c-2', 'alpha-2', 'alpha', 'ship', 'Second', '2026-11-02T10:00:00Z');
                 INSERT INTO prs (id, project_id, card_id, number, url, linked_at, linked_by)
                 VALUES ('p-1', 'alpha', 'c-1', 1, 'https://github.com/example/app/pull/1', '2026-11-01T11:00:00Z', 'owner'),
                        ('p-2', 'alpha', 'c-2', 2, 'https://github.com/example/app/pull/2', '2026-11-02T11:00:00Z', 'owner');
                 INSERT INTO artifacts (id, card_id, kind, title, url, path, created_at)
                 VALUES ('a-1', 'c-1', 'page', 'Mockup', 'https://pages.example.com/mockup.html', NULL, '2026-11-01T12:00:00Z'),
                        ('a-2', 'c-1', 'report', 'Local report', NULL, '/home/user/report.md', '2026-11-01T13:00:00Z')",
            )
            .unwrap();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());

        let all = get(&app, "/api/v1/contributions").await;
        assert_eq!(all.status, StatusCode::OK, "{}", all.body);
        let all: Value = serde_json::from_str(&all.body).unwrap();
        let ids: Vec<&str> = all["contributions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["p-2", "p-1"]);
        assert_eq!(all["contributions"][0]["repo"], "example/app");
        assert_eq!(all["contributions"][0]["card_display_id"], "alpha-2");

        let none = get(&app, "/api/v1/contributions?project=elsewhere").await;
        let none: Value = serde_json::from_str(&none.body).unwrap();
        assert_eq!(none["contributions"], json!([]));

        let card = get(&app, "/api/v1/cards/alpha-1").await;
        let card: Value = serde_json::from_str(&card.body).unwrap();
        assert_eq!(
            card["artifacts"][0]["url"],
            "https://pages.example.com/mockup.html"
        );
        assert_eq!(card["artifacts"][1]["url"], Value::Null);
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

    /// The board's project delete: the preview counts what goes, the delete
    /// needs the id typed again, a live session refuses it, and only the
    /// owner may send it.
    #[tokio::test]
    async fn board_project_delete_previews_confirms_and_frees_the_id() {
        let mut fleet = Fleet::new();
        fleet
            .store
            .project_add(
                &Ctx::new(Actor::Owner),
                tiller_core::state::projects::NewProject {
                    id: "gamma".into(),
                    name: "Gamma".into(),
                    repo: None,
                    remote: None,
                    default_branch: None,
                    delivery: tiller_core::domain::Delivery::DirectPr,
                    checks: None,
                    capacity: None,
                    deploy_workflow: None,
                    deploy_environment: None,
                    deploy_timeout_minutes: None,
                },
            )
            .unwrap();
        fleet
            .store
            .card_create(
                &Ctx::new(Actor::Owner),
                None,
                tiller_core::state::cards::NewCard {
                    project_id: "gamma".into(),
                    kind: tiller_core::domain::CardKind::Ship,
                    title: "doomed".into(),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let cookie = owner_cookie(home);
        let delete = |id: &str, key: &str, confirm: &str, cookie: &str| {
            let headers: Vec<(&str, &str)> = if cookie.is_empty() {
                vec![]
            } else {
                vec![("cookie", cookie)]
            };
            mutation(
                "POST",
                &format!("/api/v1/projects/{id}/delete"),
                json!({"idempotency_key": key, "confirm": confirm}),
                &headers,
            )
        };

        let preview = get(&app, "/api/v1/projects/gamma/delete").await;
        assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
        let body: Value = serde_json::from_str(&preview.body).unwrap();
        assert_eq!(body["project"], "gamma");
        assert_eq!(body["removed"]["projects"], 1);
        assert_eq!(body["removed"]["cards"], 1);
        assert_eq!(body["refused"], Value::Null, "{body}");
        assert_eq!(body["unblock"], Value::Null, "{body}");

        // alpha's orchestrator is live: the preview names it, the delete
        // refuses and removes nothing.
        let live = get(&app, "/api/v1/projects/alpha/delete").await;
        let body: Value = serde_json::from_str(&live.body).unwrap();
        assert!(
            body["refused"].as_str().unwrap().contains("session"),
            "{body}"
        );
        assert_eq!(body["unblock"]["step"], "stop its agent", "{body}");
        assert!(
            body["unblock"]["command"]
                .as_str()
                .unwrap()
                .starts_with("tiller agent stop "),
            "{body}"
        );
        let refused = send(&app, delete("alpha", "k-del-1", "alpha", &cookie)).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );

        let anonymous = send(&app, delete("gamma", "k-del-2", "gamma", "")).await;
        assert_eq!(anonymous.status, StatusCode::FORBIDDEN);
        let mistyped = send(&app, delete("gamma", "k-del-3", "gama", &cookie)).await;
        assert_eq!(
            mistyped.status,
            StatusCode::BAD_REQUEST,
            "{}",
            mistyped.body
        );
        let peer = send(
            &app,
            mutation(
                "POST",
                "/api/v1/projects/gamma/delete",
                json!({"idempotency_key": "k-del-4", "confirm": "gamma", "host": "laptop"}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        assert_ne!(peer.status, StatusCode::OK, "{}", peer.body);

        let deleted = send(&app, delete("gamma", "k-del-5", "gamma", &cookie)).await;
        assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
        let body: Value = serde_json::from_str(&deleted.body).unwrap();
        assert_eq!(body["data"]["removed"]["cards"], 1, "{body}");
        let gone = get(&app, "/api/v1/projects/gamma/delete").await;
        assert_eq!(gone.status, StatusCode::NOT_FOUND, "{}", gone.body);
        let replay = send(&app, delete("gamma", "k-del-5", "gamma", &cookie)).await;
        assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
        let body: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");
        let alpha = fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*) FROM projects WHERE id = 'alpha'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(alpha, 1);
    }

    /// A herdr that logs each call and answers the attach probe from a
    /// file the fake terminal app creates when it opens.
    fn scripted_herdr(dir: &std::path::Path) -> Cli {
        let script = format!(
            r#"echo "$*" >> '{dir}/calls'
case "$*" in *"terminal title clear"*)
  if [ -e '{dir}/attached' ]; then r=cleared; else r=no_foreground_client; fi
  printf '{{"result":{{"type":"client_window_title","reason":"%s"}}}}\n' "$r";;
esac"#,
            dir = dir.display()
        );
        Cli::local().program(vec![
            "sh".into(),
            "-c".into(),
            script.into(),
            "herdr".into(),
        ])
    }

    /// A terminal app that records what it was asked and attaches a client
    /// when it opens.
    struct ScriptedApp(std::path::PathBuf);

    impl Terminal for ScriptedApp {
        fn name(&self) -> &str {
            "Ghostty"
        }

        fn raise(&self) -> std::io::Result<()> {
            std::fs::write(self.0.join("app"), "raise")
        }

        fn open(&self, command: &[std::ffi::OsString]) -> std::io::Result<()> {
            let command: Vec<_> = command.iter().map(|w| w.to_string_lossy()).collect();
            std::fs::write(self.0.join("app"), format!("open {}", command.join(" ")))?;
            std::fs::write(self.0.join("attached"), "")
        }
    }

    /// A terminal app that is not installed.
    struct MissingApp;

    impl Terminal for MissingApp {
        fn name(&self) -> &str {
            "Ghostty"
        }

        fn raise(&self) -> std::io::Result<()> {
            Err(std::io::Error::other(
                "Unable to find application named 'Ghostty'",
            ))
        }

        fn open(&self, _: &[std::ffi::OsString]) -> std::io::Result<()> {
            self.raise()
        }
    }

    #[tokio::test]
    async fn focus_says_what_it_did_on_this_machine() {
        let mut fleet = Fleet::new();
        let local = fleet.worker("w2:p1");
        let remote = fleet.worker("w3:p1");
        fleet
            .open()
            .conn()
            .execute_batch(&format!(
                "INSERT INTO hosts (id, label, kind, herdr_machine) VALUES ('h-r', 'remote-a', 'remote', 'm-1');
                 UPDATE sessions SET host_id = 'h-r' WHERE id = '{remote}'"
            ))
            .unwrap();
        let cookie = owner_cookie(fleet.dir.path());
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = |terminal: Option<Arc<dyn Terminal>>| {
            app_full(
                fleet.dir.path(),
                Assets::of::<Fixture>(),
                Damage::default(),
                Peers::idle(),
                scripted_herdr(dir),
                terminal,
            )
        };
        let focus = |app: Router, session: String, key: &'static str| {
            let cookie = cookie.clone();
            async move {
                let answer = send(
                    &app,
                    mutation(
                        "POST",
                        &format!("/api/v1/agents/{session}/focus"),
                        json!({"idempotency_key": key}),
                        &[("cookie", cookie.as_str())],
                    ),
                )
                .await;
                assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
                let body: Value = serde_json::from_str(&answer.body).unwrap();
                body["data"].clone()
            }
        };
        let calls = || std::fs::read_to_string(dir.join("calls")).unwrap_or_default();
        let app_did = || std::fs::read_to_string(dir.join("app")).unwrap_or_default();
        let ghostty = || Some(Arc::new(ScriptedApp(dir.to_owned())) as Arc<dyn Terminal>);

        // No terminal configured and no client: focused, but nothing shows it.
        let data = focus(app(None), local.clone(), "k-1").await;
        assert_eq!(data["opened"], "unseen");
        assert_eq!(data["terminal"], Value::Null);
        assert!(calls().contains("agent focus w2:p1"), "{}", calls());

        // No client: the app opens on herdr, then the focus lands again.
        let data = focus(app(ghostty()), local.clone(), "k-2").await;
        assert_eq!(data["opened"], "launched");
        assert_eq!(data["terminal"], "Ghostty");
        // The program words, the bare `sh` resolved to a full path.
        assert!(app_did().starts_with("open /"), "{}", app_did());
        assert!(app_did().ends_with("esac herdr"), "{}", app_did());

        // A client is attached: the app comes to the front.
        let data = focus(app(ghostty()), local.clone(), "k-3").await;
        assert_eq!(data["opened"], "raised");
        assert_eq!(app_did(), "raise");

        // The app will not open: the focus still landed, so the answer is
        // 200 and names the problem.
        let data = focus(app(Some(Arc::new(MissingApp))), local.clone(), "k-5").await;
        assert_eq!(data["opened"], "failed");
        assert_eq!(
            data["problem"],
            "Ghostty did not open: Unable to find application named 'Ghostty'"
        );

        // A remote agent: the probe asks the local server, the focus goes
        // to the saved machine.
        std::fs::remove_file(dir.join("calls")).unwrap();
        let data = focus(app(None), remote, "k-4").await;
        assert_eq!(data["opened"], "focused");
        assert_eq!(
            calls().lines().collect::<Vec<_>>(),
            ["terminal title clear", "--machine m-1 agent focus w3:p1"]
        );
    }

    #[test]
    fn pane_tail_drops_escapes_and_trailing_blank_lines() {
        let screen = "\u{1b}[1mbold\u{1b}[0m line\n\u{1b}]0;title\u{7}second\n\n   \n";
        assert_eq!(pane_tail(screen, PANE_TAIL), vec!["bold line", "second"]);
        assert_eq!(pane_tail(screen, 1), vec!["second"]);
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
