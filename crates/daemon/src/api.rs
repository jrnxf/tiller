//! The board API over axum (`protocol.md` section 7): reads, mutations
//! through the core's writer functions, the change stream, the login link
//! and the app shell with its boot snapshot.
//!
//! Every request passes the `Host` check first (421 otherwise, against DNS
//! rebinding). At an `--allow-host` name every request then needs the owner
//! cookie too, except the signed-out screen and the two ways to sign in.
//! Mutations also need the owner cookie, a JSON body,
//! `Sec-Fetch-Site: same-origin` and the board's own `Origin` (`http://` on
//! loopback, `https://` at an `--allow-host` name); no CORS header is ever
//! sent.

use std::ffi::OsString;
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
use tiller_agents::land::app::{APP_DIR, APP_ID_FILE, AppKey, KEY_FILE, KeyError};
use tiller_agents::spawn::config::SpawnConfig;
use tiller_agents::spawn::orchestrator::Orchestrator;
use tiller_agents::spawn::readings;
use tiller_core::config_file as config_toml;
use tiller_core::domain::{
    Activity, AnsweredVia, Approval, AskSource, CardKind, CardOutcome, HoldReleaseReason, Host,
    OutboxKind, PeerScope, Run, Session,
};
use tiller_core::domain::{Boundary, Delivery, GrantKind, InterventionKind, Stage};
use tiller_core::host_config;
// The store checks every cap. The API checks them again before a mutation
// is forwarded, so text over a cap is refused here, not after a trip over
// the link to a peer.
use tiller_core::limits::{self, ANSWER_MAX_CHARS, COMMENT_MAX_CHARS, STEER_MAX_CHARS, within};
use tiller_core::state::approvals::{Decision, NewGrant};
use tiller_core::state::asks::NewAsk;
use tiller_core::state::cards::{CardEdit, NewCard};
use tiller_core::state::peers::{
    self as pairing, KeyFile, NewPairing, check_alias, check_label, fingerprint,
};
use tiller_core::state::projects::meta;
use tiller_core::state::stop_hook;
use tiller_core::state::{get as get_row, resolve_id};
use tiller_core::{Actor, Applied, Ctx, Error, Store, Timestamp, ids};
use tiller_herdr::client::{Cli, Herdr, HerdrError};
use tiller_herdr::front::{OpenError, PROBE_TIMEOUT, Terminal, Wait, open_agent};
use tiller_herdr::poller::Registry;
use tokio::sync::watch;

use crate::assets::{self, Assets};
use crate::dto::*;
use crate::mirror::{self, MAX_VIEWERS, Mirrors};
use crate::peer::forward::{Op, Order};
use crate::peer::link::{Forwarded, PaneStream, Paned, Peers};
use crate::peer::pair::{self, Accept, Cause, NoAnswer, NotAccepted};
use crate::peer::pane::PaneKind;
use crate::peer::{Federation, PeerView};
use crate::readonly::Damage;
use crate::views::{self, Filter, LocalHealth, PaneTarget};
use crate::{auth, memory, prompt, sse};

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
    /// Extra `Host` values the board answers on, such as a TLS-terminating
    /// reverse proxy's name (`tiller daemon --allow-host`). Lowercase.
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
    /// The board's Start orchestrator; none answers it 503.
    pub orchestrators: Option<Arc<dyn OrchestratorStart>>,
    /// The terminal mirror's open views and input budget.
    pub mirrors: Mirrors,
    /// The limit on sign-in code tries, and sign-outs for open streams.
    pub sessions: auth::Sessions,
    /// The open change streams of each session, for their cap.
    pub streams: sse::Streams,
    /// `ssh`, or `TILLER_SSH` split on whitespace: what Add a host runs to
    /// reach the new host (`protocol.md` 10.1).
    pub ssh: Vec<OsString>,
    /// The holdout evals the board started, running or failed.
    pub evals: memory::eval::Evals,
    /// The backup the board started, if one runs, and the last that failed.
    pub backups: crate::backups::BackupJobs,
    /// The curator runs the board started (Run again), stopped with the
    /// daemon.
    pub retros: crate::retros::Retros,
}

/// Starts a project's orchestrator, or returns the live one, as `tiller
/// orchestrator start` does, and controls an agent's run as `tiller agent`
/// does. The binary provides it: the spawn tools are read from
/// `config.toml` there, and each call runs the command's own path.
pub trait OrchestratorStart: Send + Sync {
    /// With `restart`, the live orchestrator is stopped and replaced, as
    /// `--restart` does; without it, a live one is returned.
    fn start(
        &self,
        store: &mut Store,
        ctx: &Ctx,
        project: &str,
        restart: bool,
    ) -> Result<Applied<Orchestrator>, StartError>;

    /// `tiller agent stop` on `session`.
    fn stop_agent(
        &self,
        _store: &mut Store,
        _ctx: &Ctx,
        _session: &Session,
    ) -> Result<Controlled, StartError> {
        Err(cannot("stop agents"))
    }

    /// `tiller agent resume` on `run`.
    fn resume_run(
        &self,
        _store: &mut Store,
        _ctx: &Ctx,
        _run: &Run,
    ) -> Result<Controlled, StartError> {
        Err(cannot("resume agents"))
    }

    /// `tiller agent relaunch` on `run`, keyed by `ctx.key`. A new attempt
    /// takes the run's own brief.
    fn relaunch_run(
        &self,
        _store: &mut Store,
        _ctx: &Ctx,
        _run: &Run,
        _relaunch: BoardRelaunch,
    ) -> Result<Controlled, StartError> {
        Err(cannot("relaunch agents"))
    }

    /// The host-level settings a start reads, and what it would refuse
    /// about them.
    fn settings(&self) -> StartSettings {
        StartSettings {
            pool_root: None,
            checkouts_root: None,
            orchestrator_home: None,
            scratch_root: None,
            problems: Vec::new(),
            config_error: Some("this host cannot report its start settings".to_owned()),
        }
    }

    /// Checks `value` for `key` by the rules a start applies, before it is
    /// written. Returns what else the change leaves wrong, or the refusal
    /// in plain words.
    fn check_setting(&self, _key: &str, _value: &str) -> Result<Vec<String>, String> {
        Err("this host cannot check its start settings".to_owned())
    }

    /// Restarts a live session on this binary: the orchestrator start with
    /// `--restart`, or a relaunch of the worker's run. The error is what the
    /// command would say.
    fn restart(&self, _store: &mut Store, _ctx: &Ctx, _session: &Session) -> Result<(), String> {
        Err("this host cannot restart sessions".to_owned())
    }

    /// Runs the checks `tiller doctor` runs, on this host, now. `None` from a
    /// binary that has none.
    fn doctor(&self) -> Option<DoctorReport> {
        None
    }

    /// Claims `retro` with the board's `ctx` and writes its curator's
    /// launch, as `tiller retro run` does, refused by the same rules. The
    /// daemon does the returned run on a thread of its own.
    fn retro_run(
        &self,
        _store: &mut Store,
        _ctx: &Ctx,
        _retro: &str,
    ) -> Result<CuratorRun, StartError> {
        Err(StartError::Refused {
            exit: 7,
            code: "unavailable",
            message: "this host cannot run retros".to_owned(),
            hint: None,
            current: None,
        })
    }
}

/// A retro the board runs, claimed for its curator (`learning-loop.md`
/// section 4).
pub struct CuratorRun {
    /// The claim: the retro reset if it ended `unfinished`, then started.
    pub claimed: Applied<tiller_core::domain::Retro>,
    /// The run: launches the curator and waits for it until it exits, its
    /// timeout passes or the flag is set, which stops it and everything it
    /// started. A retro the curator did not finish ends `unfinished` with
    /// why. Returns the retro as the run left it, or what went wrong.
    pub run: Box<
        dyn FnOnce(Arc<std::sync::atomic::AtomicBool>) -> Result<tiller_core::domain::Retro, String>
            + Send,
    >,
}

/// What one of the binary's agent controls did, as the command prints it.
pub struct Controlled {
    pub applied: Applied<Value>,
    /// What the command would warn of: a stop or start it could not see.
    pub warning: Option<String>,
}

/// The board's Relaunch: the note the new session starts from, and whether
/// it counts as a new attempt.
pub struct BoardRelaunch {
    pub note: String,
    pub new_attempt: bool,
}

/// A host whose binary gave the daemon no agent controls.
fn cannot(what: &str) -> StartError {
    StartError::from(Error::Unavailable(format!("this host cannot {what}")))
}

/// A start that failed, as `tiller orchestrator start` reports it.
#[derive(Debug)]
pub enum StartError {
    /// The store failed, maybe after a start that was then undone (`notes`
    /// say what was put back). The daemon checks the database for damage.
    State {
        error: Box<Error>,
        notes: Option<String>,
    },
    /// Any other refusal, with the command's exit code (`protocol.md` 6.1).
    Refused {
        exit: u8,
        code: &'static str,
        message: String,
        hint: Option<String>,
        /// The facts of the refusal: a start with settings missing lists
        /// every one (`settings`).
        current: Option<Value>,
    },
}

impl From<Error> for StartError {
    fn from(error: Error) -> Self {
        Self::State {
            error: Box::new(error),
            notes: None,
        }
    }
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
    pub(crate) fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
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

/// The HTTP status for a CLI exit code (`protocol.md` 6.1).
fn status_of_exit(exit: impl Into<i32>) -> StatusCode {
    match exit.into() {
        2 => StatusCode::BAD_REQUEST,
        3 => StatusCode::NOT_FOUND,
        4 => StatusCode::CONFLICT,
        5 => StatusCode::UNPROCESSABLE_ENTITY,
        6 => StatusCode::FORBIDDEN,
        7 => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        let mut api = ApiError::new(
            status_of_exit(error.exit_code()),
            error.code(),
            error.to_string(),
        );
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
                edited_at,
                ..
            } => {
                api.body.error.entity = Some((*entity).to_owned());
                api.body.error.id = Some(id.clone());
                let mut current = json!({"rev": current_rev});
                if let Some((name, value)) = field {
                    current[name] = value.clone();
                }
                if let Some(at) = edited_at {
                    current["edited_at"] = json!(at);
                }
                api.body.error.current = Some(current);
            }
            Error::Refused { entity, id, .. } => {
                api.body.error.entity = Some((*entity).to_owned());
                api.body.error.id = Some(id.clone());
            }
            Error::OwnerMarker(_) => {
                api.body.error.hint = Some(tiller_core::owner::FLIP_BACK.to_owned());
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

pub use tiller_core::INIT_HINT;

/// The 503 every read and write answers before `tiller init`.
pub(crate) fn uninitialized() -> ApiError {
    ApiError::from(Error::Unavailable(tiller_core::not_initialized())).hint(INIT_HINT)
}

/// The 503 every mutation answers while `state.db` is damaged.
pub(crate) fn read_only(reason: &str) -> ApiError {
    ApiError::from(Error::Unavailable(format!(
        "read only: state.db failed its integrity check ({reason}); mutations are refused until the owner restores it"
    )))
    .hint("stop the daemon, then run tiller restore <file> --dry-run and again without the flag")
}

/// Refuses a mutation before any of its work while the daemon is read-only:
/// `state.db` is damaged (`recovery.md` R10), or the owner marker names
/// another system (R20; read on every request, since the owner can flip it
/// while the daemon runs). The store refuses the write itself under the
/// marker too; this check also stops the outside work some mutations do
/// first, such as a clone, an ssh pairing or an orchestrator start.
pub(crate) fn writable(state: &AppState) -> Result<(), ApiError> {
    if let Some(reason) = state.0.damage.reason() {
        return Err(read_only(&reason));
    }
    Ok(tiller_core::owner::check(&state.0.home)?)
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

/// The `--allow-host` entry `host` matches, if any.
fn allowed_host<'a>(state: &'a AppState, host: &str) -> Option<&'a str> {
    state
        .0
        .allowed_hosts
        .iter()
        .find(|allowed| allowed.eq_ignore_ascii_case(host))
        .map(String::as_str)
}

/// The origin the board has at `host`: `http://` on loopback, `https://` at
/// an allowed host, which a TLS-terminating proxy fronts. None for any other.
fn board_origin(state: &AppState, host: &str) -> Option<String> {
    if loopback_hosts(state.0.port).iter().any(|own| own == host) {
        Some(format!("http://{host}"))
    } else {
        allowed_host(state, host).map(|allowed| format!("https://{allowed}"))
    }
}

/// 421 unless `Host` is exactly this daemon's loopback address, or one the
/// owner allowed with `--allow-host`.
///
/// A page opened on `localhost` is sent to `127.0.0.1`, the host the login
/// link names: the owner cookie belongs to one host, so a board on the other
/// could read but never write. API calls are answered on either. An allowed
/// host gets the whole board, its login link included; `guard` takes its
/// mutations only from its `https://` origin.
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
    } else if host
        .is_some_and(|host| host == own || host == alias || allowed_host(&state, host).is_some())
    {
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
pub(crate) async fn guard(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    same_origin(state, headers)?;
    owner_only(state, headers).await
}

/// Refuses a read unless the request carries a live owner cookie. Most reads
/// answer any request that passes the Host check; the backups list is the
/// owner's.
pub(crate) async fn owner_only(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if owner_cookie(state, headers).await.is_none() {
        return Err(not_owner());
    }
    Ok(())
}

fn not_owner() -> ApiError {
    ApiError::forbidden("the board is not signed in as the owner")
        .hint("run tiller board --login in a terminal and enter the code or open the link")
}

/// The request's owner cookie, when it is a live one. The owner file is
/// read on the blocking pool, so a slow disk holds up this request only.
async fn owner_cookie(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let cookie = sent_cookie(headers)?;
    let home = state.0.home.clone();
    let asked = cookie.clone();
    tokio::task::spawn_blocking(move || auth::is_owner(&home, &asked, Timestamp::now()))
        .await
        .unwrap_or(false)
        .then_some(cookie)
}

/// The owner cookie the request carries, live or not.
fn sent_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(auth::cookie_value)
        .map(str::to_owned)
}

/// Whether `headers` name an `--allow-host` name rather than loopback.
fn at_allowed_host(state: &AppState, headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|host| host.to_str().ok())
        .is_some_and(|host| allowed_host(state, host).is_some())
}

/// Every mutation check but the owner cookie: a JSON body, sent by the
/// board's own page from its own origin. Signing in passes these too.
fn same_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
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
    let Some(origin) = board_origin(state, host) else {
        return Err(ApiError::forbidden(
            "a mutation must come from the board on 127.0.0.1 or an --allow-host name",
        ));
    };
    if text(header::ORIGIN) != Some(origin.as_str()) {
        return Err(ApiError::forbidden(format!(
            "a mutation must come from the board's own origin, {origin}"
        )));
    }
    Ok(())
}

/// Marks a request the owner gate let through without the owner cookie: the
/// app shell then serves the signed-out screen, with no boot snapshot.
#[derive(Clone, Copy)]
struct SignedOut;

/// At an `--allow-host` name, the owner cookie for everything but what the
/// signed-out screen needs: the app shell (without its boot snapshot), the
/// built files, and the two ways to sign in. Anything else is 403
/// `signed_out`, the board's cue to show that screen. Loopback is left as it
/// was: its reads need no cookie, so the CLI and a local board work as
/// before.
async fn owner_gate(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let headers = request.headers();
    if !at_allowed_host(&state, headers) || owner_cookie(&state, headers).await.is_some() {
        return next.run(request).await;
    }
    let path = request.uri().path();
    let open = match *request.method() {
        Method::GET | Method::HEAD => !path.starts_with("/api/"),
        Method::POST => path == LOGIN_PATH,
        _ => false,
    };
    if !open {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "signed_out",
            "this board is not signed in; sign in to see it",
        )
        .hint("run tiller board --login in a terminal and enter the code or open the link")
        .into_response();
    }
    request.extensions_mut().insert(SignedOut);
    next.run(request).await
}

tokio::task_local! {
    /// The normalized arguments of the API request this task serves
    /// ([`bind_args`]).
    static REQUEST_ARGS: Value;
}

/// The most body a request may send, as axum's default extractor limit.
const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Binds an API write's idempotency key to its arguments (`protocol.md`
/// 6.3): the method, path and body of every request that is not a read are
/// kept, in a normalized form, for [`board_ctx`] to record with the key. A
/// JSON body is normalized by parsing it, so key order and whitespace do not
/// count. Its top-level `rev` is left out: it is a precondition, not an
/// argument, and a retry after the write landed may carry the newer one. A
/// retry must resend the same request, and another request under the same
/// key is refused.
async fn bind_args(request: Request, next: Next) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD)
        || !request.uri().path().starts_with("/api/")
    {
        return next.run(request).await;
    }
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "usage",
                "the request body is unreadable or over 2 MiB",
            )
            .into_response();
        }
    };
    let mut sent = serde_json::from_slice::<Value>(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    if let Some(fields) = sent.as_object_mut() {
        fields.remove("rev");
    }
    let args = json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path(),
        "query": parts.uri.query(),
        "body": sent,
    });
    let request = Request::from_parts(parts, Body::from(bytes));
    REQUEST_ARGS.scope(args, next.run(request)).await
}

/// The normalized arguments of the request this task serves, when it is an
/// API write. Read it before moving work to the blocking pool.
pub(crate) fn request_args() -> Option<Value> {
    REQUEST_ARGS.try_with(Clone::clone).ok()
}

/// Runs `work` in its own task, so a browser that goes away does not stop
/// it halfway, with this request's arguments still bound ([`bind_args`]): a
/// new task does not inherit the task-local, and without it the key would
/// replay another request's answer.
pub(crate) async fn detached<T: Send + 'static>(
    work: impl Future<Output = ApiResult<T>> + Send + 'static,
) -> ApiResult<T> {
    let task = match request_args() {
        Some(args) => tokio::spawn(REQUEST_ARGS.scope(args, work)),
        None => tokio::spawn(work),
    };
    match task.await {
        Ok(result) => result,
        Err(error) => Err(Error::Internal(error.to_string()).into()),
    }
}

/// The board's context for a write under `key`, bound to `args`.
pub(crate) fn board_ctx(key: &str, args: Option<&Value>) -> Ctx {
    let ctx = Ctx::new(Actor::Board).with_key(key.to_owned());
    match args {
        Some(args) => ctx.with_args(key.to_owned(), args),
        None => ctx,
    }
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
pub(crate) async fn mutate(
    state: &AppState,
    key: String,
    mutation: impl FnOnce(&mut Store, &Ctx, &str) -> tiller_core::Result<Done> + Send + 'static,
) -> ApiResult<MutationResponse> {
    writable(state)?;
    let db = state.db();
    let damage = state.0.damage.clone();
    let args = request_args();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let done = Store::open_current(&db).and_then(|mut store| {
            let ctx = board_ctx(&key, args.as_ref());
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
pub(crate) struct Done {
    data: Value,
    events: Vec<i64>,
    replayed: bool,
    outbox_id: Option<String>,
    approval_id: Option<String>,
    warning: Option<String>,
}

impl Done {
    pub(crate) fn of<T: Serialize>(applied: Applied<T>) -> tiller_core::Result<Self> {
        Ok(Self {
            data: serde_json::to_value(&applied.value)?,
            events: applied.events,
            replayed: applied.replayed,
            outbox_id: None,
            approval_id: None,
            warning: None,
        })
    }

    pub(crate) fn with_warning(mut self, warning: Option<String>) -> Self {
        self.warning = warning;
        self
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
            warning: self.warning,
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
        .route("/api/v1/cards/{id}/deps", post(add_dep).delete(remove_dep))
        .route("/api/v1/cards/{id}/close", post(close))
        .route("/api/v1/cards/{id}/cancel", post(cancel))
        .route("/api/v1/cards/{id}/reopen", post(reopen))
        .route("/api/v1/attention", get(attention))
        .route("/api/v1/attention/{id}/answer", post(answer))
        .route("/api/v1/attention/{id}/withdraw", post(withdraw_attention))
        .route("/api/v1/approvals/{id}/decide", post(decide))
        .route(
            "/api/v1/away",
            post(away_start).put(away_edit).delete(away_end),
        )
        .route("/api/v1/away/brief", get(away_brief))
        .route("/api/v1/agents", get(agents))
        .route("/api/v1/agents/{session}/focus", post(focus))
        .route("/api/v1/agents/{session}/terminal", post(terminal))
        .route(
            "/api/v1/agents/{session}/terminal/input",
            post(terminal_input),
        )
        .route("/api/v1/agents/{session}/terminal/fit", post(terminal_fit))
        .route(
            "/api/v1/agents/{session}/terminal/history",
            post(terminal_history),
        )
        .route(
            "/api/v1/agents/{session}/terminal/tail",
            post(terminal_tail),
        )
        .route("/api/v1/agents/{session}/steer", post(steer_agent))
        .route("/api/v1/agents/{session}/interrupt", post(interrupt_agent))
        .route("/api/v1/agents/{session}/stop", post(stop_agent))
        .route("/api/v1/agents/{session}/pause", post(pause_agent))
        .route("/api/v1/agents/{session}/resume", post(resume_agent))
        .route("/api/v1/agents/{session}/unpause", post(unpause_agent))
        .route("/api/v1/agents/{session}/relaunch", post(relaunch_agent))
        .route("/api/v1/agents/{session}/prompt", post(prompt_read))
        .route(
            "/api/v1/agents/{session}/prompt/answer",
            post(prompt_answer),
        )
        .route("/api/v1/runs/{id}", get(run))
        .route("/api/v1/machines", get(machines))
        .route("/api/v1/quota", get(quota))
        .route("/api/v1/health", get(health))
        .route("/api/v1/doctor", get(doctor))
        .route("/api/v1/pollers/{source}/run", post(run_poller))
        .route("/api/v1/projects", get(projects).post(add_project))
        .route("/api/v1/projects/checkout", get(project_checkout))
        .route("/api/v1/start-settings", get(start_settings))
        .route("/api/v1/host-config", get(host_config_read))
        .route("/api/v1/config", post(set_config))
        .route("/api/v1/peers", get(crate::pairings::list).post(add_peer))
        .route(
            "/api/v1/peers/{label}",
            axum::routing::delete(crate::pairings::unpair),
        )
        .route(
            "/api/v1/peer-keys/{id}",
            axum::routing::delete(crate::pairings::revoke),
        )
        .route("/api/v1/host", get(host_fields).patch(edit_host))
        .route(
            "/api/v1/backups",
            get(crate::backups::list).post(crate::backups::take),
        )
        .route("/api/v1/bug-project", post(pick_bug_project))
        .route("/api/v1/contributions", get(contributions))
        .route("/api/v1/projects/{id}/stage", post(stage_project))
        .route("/api/v1/projects/{id}/asks", post(ask_note))
        .route("/api/v1/agents/restart-stale", post(restart_stale))
        .route(
            "/api/v1/projects/{id}/orchestrator",
            post(start_orchestrator),
        )
        .route(
            "/api/v1/projects/{id}/delete",
            get(project_delete_preview).post(delete_project),
        )
        .route(
            "/api/v1/projects/{id}/retire",
            get(project_retire_preview).post(retire_project),
        )
        .route("/api/v1/projects/{id}/setup", get(project_setup))
        .route(
            "/api/v1/projects/{id}",
            get(project_fields).patch(edit_project),
        )
        .route(
            "/api/v1/projects/{id}/grants",
            get(project_grants).post(create_grant),
        )
        .route("/api/v1/grants/{id}", axum::routing::delete(revoke_grant))
        .route(
            "/api/v1/interventions/{id}/label",
            post(relabel_intervention),
        )
        .route("/api/v1/memory", get(memory::view))
        .route("/api/v1/memory/edits", post(memory::edit))
        .route("/api/v1/memory/proposals/decide", post(memory::decide))
        .route(
            "/api/v1/memory/proposals/{id}/repropose",
            post(memory::repropose),
        )
        .route(
            "/api/v1/memory/proposals/{id}/eval",
            get(memory::eval::eval_plan).post(memory::eval::eval_start),
        )
        .route("/api/v1/memory/changes/{proposal}/undo", post(memory::undo))
        .route("/api/v1/retros", get(crate::retros::list_retros))
        .route("/api/v1/retros/{id}/run", post(crate::retros::run_retro))
        .route("/api/v1/memory/revisions/{id}", get(memory::revision))
        .route("/api/v1/recap", get(recap))
        .route("/api/v1/recap/seen", post(mark_recap_seen))
        .route("/api/v1/projects/{id}/replies", get(project_replies))
        .route("/api/v1/replies/{id}/read", post(mark_reply_read))
        .route("/api/v1/signals", get(signals))
        .route("/api/v1/events", get(events))
        .route("/api/v1/session", get(session))
        .route(LOGIN_PATH, post(login_code))
        .route("/api/v1/logout", post(logout))
        .route("/login/{token}", get(login))
        .fallback(app_shell)
        .layer(middleware::from_fn(bind_args))
        .layer(middleware::from_fn_with_state(state.clone(), owner_gate))
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
    peer_query(state, view, path, || {
        let cached = views::peer_board(view, None, Filter::All)
            .rows
            .into_iter()
            .find(|row| names(row, card));
        cached.and_then(|row| serde_json::to_value(row).ok())
    })
    .await
}

/// A `query` to a peer, answered as its own API answers. With no answer it
/// is 503 `peer_unreachable`, with what `cached` finds in `current`.
async fn peer_query<T: for<'de> Deserialize<'de>>(
    state: &AppState,
    view: &PeerView,
    path: String,
    cached: impl FnOnce() -> Option<Value>,
) -> Result<T, ApiError> {
    let Some((status, body)) = state.0.peers.query(&view.host_id, path).await else {
        return Err(ApiError::peer_unreachable(&view.label, cached()));
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

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ProjectsQuery {
    /// A host id or label.
    pub host: Option<String>,
    /// `true` lists this host's retired projects instead.
    pub retired: Option<bool>,
}

#[utoipa::path(get, path = "/api/v1/projects", tag = "reads", params(ProjectsQuery),
    responses((status = 200, body = ProjectsResponse)))]
async fn projects(
    State(state): State<AppState>,
    Query(query): Query<ProjectsQuery>,
) -> ApiResult<ProjectsResponse> {
    if query.retired == Some(true) {
        // Peers cache only live projects, so the archive is this host's.
        if query.host.is_some() {
            return Err(ApiError::usage("retired=true lists this host only"));
        }
        return read(&state, |store| {
            views::retired_projects(store, Timestamp::now())
        })
        .await
        .map(Json);
    }
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
    let home = state.0.home.clone();
    let (seq, merge_app) = tokio::task::spawn_blocking(move || {
        let seq = db
            .exists()
            .then(|| Store::open_current(&db).and_then(|store| store.last_seq()));
        (seq, merge_app_health(&home))
    })
    .await
    .unwrap_or_else(|error| {
        let detail = format!("the merge App key was not read: {error}");
        let merge_app = MergeAppHealth {
            status: "warn".into(),
            app_id: None,
            detail,
        };
        (None, merge_app)
    });
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
        merge_app,
    })
}

/// How long Run now waits for the poll it asked for.
const POLL_NOW_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// The checks `tiller doctor` runs, run now on this host or, with
/// `?host=<label>`, on a paired peer (`protocol.md` 7.3). A peer runs its own
/// checks and sends them as a `query` answer; the board only reads them.
#[utoipa::path(get, path = "/api/v1/doctor", tag = "reads", params(HostQuery),
    responses((status = 200, body = Doctor), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody, description = "`unavailable`, or `peer_unreachable` for a peer")))]
async fn doctor(
    State(state): State<AppState>,
    Query(query): Query<HostQuery>,
) -> ApiResult<Doctor> {
    if let Some(host) = query.host {
        let peers = state.0.peers.federation();
        if let Some(view) = peers.peer(&host) {
            return peer_query(&state, view, "doctor".to_owned(), || None)
                .await
                .map(Json);
        }
        // Any other label must name this host: a herdr machine has no
        // daemon to check.
        read(&state, move |store| {
            let this = store.this_host()?;
            if this.id == host || this.label == host {
                Ok(())
            } else {
                Err(Error::NotFound {
                    entity: "host",
                    id: host,
                    candidates: Vec::new(),
                })
            }
        })
        .await?;
    }
    let Some(services) = state.0.orchestrators.clone() else {
        return Err(Error::Unavailable("this daemon cannot run its checks".into()).into());
    };
    let checked_at = Timestamp::now();
    let report = tokio::task::spawn_blocking(move || services.doctor())
        .await
        .map_err(|error| Error::Internal(error.to_string()))?;
    let Some(report) = report else {
        return Err(Error::Unavailable("this daemon cannot run its checks".into()).into());
    };
    Ok(Json(Doctor {
        checked_at: checked_at.to_string(),
        ok: report.ok,
        checks: report.checks,
        merge_gates: report.merge_gates,
    }))
}

/// Run now: ends one poller's wait so it polls at once, then answers with
/// its health after that poll, or as it stands when the poll takes longer
/// than [`POLL_NOW_WAIT`]. A wake writes nothing, so the key is not
/// replayed: a retry is natural.
#[utoipa::path(post, path = "/api/v1/pollers/{source}/run", tag = "mutations", request_body = RunPoller,
    params(("source" = String, Path, description = "A poller's source, as `/api/v1/health` names it")),
    responses((status = 200, body = MutationResponse, description = "`data` is the poller's `PollerHealth`"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn run_poller(
    State(state): State<AppState>,
    Path(source): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: RunPoller = body(&bytes)?;
    key(&request.idempotency_key)?;
    not_forwarded(&state, request.host.as_deref(), "a poller run").await?;
    writable(&state)?;
    let asked = Timestamp::now();
    if !state.0.pollers.wake(&source) {
        return Err(no_poller(source).into());
    }
    let deadline = tokio::time::Instant::now() + POLL_NOW_WAIT;
    let polled = |health: &tiller_herdr::poller::Health| {
        health
            .last_attempt_at
            .is_some_and(|at| at.as_millis() >= asked.as_millis())
    };
    let health = loop {
        let Some(health) = state.0.pollers.health_of(&source) else {
            return Err(no_poller(source).into());
        };
        if polled(&health) || tokio::time::Instant::now() >= deadline {
            break health;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let finished = polled(&health);
    let health = views::poller_health(vec![health]).into_iter().next();
    Ok(Json(
        Done {
            data: serde_json::to_value(health).map_err(Error::from)?,
            events: Vec::new(),
            replayed: false,
            outbox_id: None,
            approval_id: None,
            warning: (!finished)
                .then(|| format!("{source} is still polling; its line updates when the poll ends")),
        }
        .response(),
    ))
}

fn no_poller(source: String) -> Error {
    Error::NotFound {
        entity: "poller",
        id: source,
        candidates: Vec::new(),
    }
}

/// The merge App key's state, read as `tiller doctor` reads it.
fn merge_app_health(home: &std::path::Path) -> MergeAppHealth {
    let dir = home.join(APP_DIR);
    match AppKey::load(&dir) {
        Ok(Some(key)) => MergeAppHealth {
            status: "ok".into(),
            app_id: Some(key.id()),
            detail: format!("App {} with its key in {}", key.id(), dir.display()),
        },
        Ok(None) => MergeAppHealth {
            status: "missing".into(),
            app_id: None,
            detail: format!(
                "Create the merge App and put its {APP_ID_FILE} and {KEY_FILE} in {} (docs/plan/merge-app-setup.md). Until then every project runs as detect.",
                dir.display()
            ),
        },
        Err(error) => MergeAppHealth {
            status: if matches!(error, KeyError::Exposed { .. }) {
                "fail"
            } else {
                "warn"
            }
            .into(),
            app_id: None,
            detail: format!(
                "the merge App key is not used, so every project runs as detect: {error}"
            ),
        },
    }
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
        (status = 429, body = ErrorBody, description = "`too_many_streams`: this session already holds 64 open streams"),
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
    let cookie = watched_cookie(&state, &headers);
    let session = cookie.clone().unwrap_or_else(|| "loopback".to_owned());
    let Some(slot) = state.0.streams.open(&session) else {
        return ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_streams",
            format!(
                "this session already has {} open change streams; close a board tab and try again",
                sse::STREAMS_PER_SESSION
            ),
        )
        .into_response();
    };
    // At an allowed host the stream ends when its browser signs out, so a
    // revoked cookie stops seeing changes at once. The stream's task holds
    // its slot, so the slot is given back when the stream ends.
    let ended = {
        let state = state.clone();
        async move {
            let _slot = slot;
            until_signed_out(state, cookie).await;
        }
    };
    sse::stream(
        state.db(),
        after,
        state.0.newest.clone(),
        state.0.stop.clone(),
        peers.clone(),
        move || local_health(&pollers, &peers),
        ended,
    )
    .into_response()
}

/// How often an open stream at an allowed host re-checks its cookie, so one
/// that runs out (30 days) ends with no sign-out to wake it.
const RECHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// At an allowed host, the owner cookie a long-lived stream should end with;
/// `None` on loopback, where reads need none. The owner gate already let
/// it through, and [`until_signed_out`] checks it again off the runtime.
fn watched_cookie(state: &AppState, headers: &HeaderMap) -> Option<String> {
    at_allowed_host(state, headers)
        .then(|| sent_cookie(headers))
        .flatten()
}

/// Resolves once `cookie` is no longer an owner cookie: checked on each
/// sign-out and every [`RECHECK`]; never without a cookie to watch.
async fn until_signed_out(state: AppState, cookie: Option<String>) {
    let Some(cookie) = cookie else {
        return std::future::pending().await;
    };
    // Subscribed before the first check, so a sign-out between the owner
    // gate and here is seen by that check or wakes `changed`.
    let mut sign_outs = state.0.sessions.sign_outs();
    let mut recheck = tokio::time::interval(RECHECK);
    recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let home = state.0.home.clone();
        let asked = cookie.clone();
        let owner =
            tokio::task::spawn_blocking(move || auth::is_owner(&home, &asked, Timestamp::now()))
                .await
                .unwrap_or(false);
        if !owner {
            return;
        }
        tokio::select! {
            changed = sign_outs.changed() => {
                if changed.is_err() {
                    return std::future::pending().await;
                }
            }
            _ = recheck.tick() => {}
        }
    }
}

/// The daemon's stop, or the end of this browser's sign-in at an allowed
/// host: what ends a terminal mirror, through its usual stop path.
fn stop_or_signed_out(state: &AppState, headers: &HeaderMap) -> watch::Receiver<bool> {
    let mut stop = state.0.stop.clone();
    let ended = until_signed_out(state.clone(), watched_cookie(state, headers));
    let (tx, rx) = watch::channel(*stop.borrow());
    tokio::spawn(async move {
        tokio::select! {
            () = async {
                // A daemon with nothing to stop it never stops.
                if stop.wait_for(|stopping| *stopping).await.is_err() {
                    std::future::pending::<()>().await;
                }
            } => {}
            () = ended => {}
            // The mirror is over: nothing left to stop.
            () = tx.closed() => return,
        }
        tx.send_replace(true);
    });
    rx
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
    AddDep(String),
    RemoveDep(String),
    Answer(AnswerBody),
    Decide(DecideBody, bool),
    AskNote(AskNote),
    StartOrchestrator(bool),
    RestartStale(Option<Vec<String>>),
}

impl Mutation {
    /// Reads `bytes` as `op`'s body: 400 when it does not parse, and the
    /// handler's own field checks.
    pub(crate) fn parse(op: Op, target: Option<String>, bytes: &[u8]) -> Result<Self, ApiError> {
        let (idempotency_key, host, parsed) = match op {
            Op::CreateCard => {
                let request: CreateCard = body(bytes)?;
                let kind: CardKind = request.kind.parse()?;
                limits::card_text(Some(&request.title), request.body.as_deref())?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::CreateCard(request, kind),
                )
            }
            Op::EditCard => {
                let request: EditCard = body(bytes)?;
                limits::card_text(request.title.as_deref(), request.body.as_deref())?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::EditCard(request),
                )
            }
            Op::Comment => {
                let request: Comment = body(bytes)?;
                within("a comment", &request.text, COMMENT_MAX_CHARS)?;
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
                let outcome: CardOutcome = request
                    .outcome
                    .as_deref()
                    .filter(|outcome| !outcome.trim().is_empty())
                    .ok_or_else(|| ApiError::usage("outcome is required"))?
                    .parse()?;
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
            Op::AddDep | Op::RemoveDep => {
                let request: DepBody = body(bytes)?;
                let parsed = if op == Op::AddDep {
                    Parsed::AddDep(request.depends_on)
                } else {
                    Parsed::RemoveDep(request.depends_on)
                };
                (request.idempotency_key, request.host, parsed)
            }
            Op::Answer => {
                let request: AnswerBody = body(bytes)?;
                within("an answer", &request.answer, ANSWER_MAX_CHARS)?;
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
            Op::AskNote => {
                let request: AskNote = body(bytes)?;
                limits::ask(&request.text)?;
                (
                    request.idempotency_key.clone(),
                    request.host.clone(),
                    Parsed::AskNote(request),
                )
            }
            Op::StartOrchestrator => {
                let request: StartOrchestrator = body(bytes)?;
                (
                    request.idempotency_key,
                    request.host,
                    Parsed::StartOrchestrator(request.restart),
                )
            }
            Op::RestartStale => {
                let request: RestartStale = body(bytes)?;
                (
                    request.idempotency_key,
                    request.host,
                    Parsed::RestartStale(request.sessions),
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
                        category: request.category,
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
                // A completed card's evidence is the reason the owner gave.
                let evidence = (outcome == CardOutcome::Completed)
                    .then_some(request.reason.as_deref())
                    .flatten();
                Done::of(store.card_close(ctx, id, outcome, request.reason.as_deref(), evidence)?)
            }
            Parsed::Cancel(reason) => Done::of(store.card_cancel(ctx, id, &reason)?),
            Parsed::Reopen(reason) => Done::of(store.card_reopen(ctx, id, &reason)?),
            Parsed::AddDep(on) => Done::of(store.dep_add(ctx, id, &on)?),
            Parsed::RemoveDep(on) => Done::of(store.dep_release(ctx, id, &on)?),
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
                    warning: None,
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
            Parsed::AskNote(request) => {
                // The same core call as `tiller ask note`, from the board's ask box.
                let applied = store.ask_record(
                    ctx,
                    Some(key),
                    NewAsk {
                        project_id: id.to_owned(),
                        session_id: None,
                        source: AskSource::Board,
                        text: request.text.trim().to_owned(),
                    },
                )?;
                let outbox_id = applied.value.message.as_ref().map(|m| m.id.clone());
                let mut done = Done::of(applied)?;
                done.outbox_id = outbox_id;
                Ok(done)
            }
            // Needs the daemon's starter, so it never reaches the store alone.
            Parsed::StartOrchestrator(_) | Parsed::RestartStale(_) => Err(Error::Internal(
                "a start or restart runs through its starter".into(),
            )),
        }
    }

    /// Applies on `store` at once, as `peer serve` does for a verified
    /// frame: the response, or the refusal with what the board needs to
    /// recover from it. `starter` serves an orchestrator start.
    pub(crate) fn apply_here(
        self,
        store: &mut Store,
        ctx: &Ctx,
        starter: Option<&dyn OrchestratorStart>,
    ) -> Result<MutationResponse, ApiError> {
        let (op, target) = (self.op, self.target.clone());
        if let Parsed::StartOrchestrator(restart) = self.parsed {
            let project = target.as_deref().unwrap_or_default();
            let applied = start_here(starter, store, ctx, project, restart, |_| {})?;
            return Ok(Done::of(applied)?.response());
        }
        if let Parsed::RestartStale(sessions) = &self.parsed {
            let starter = starter
                .ok_or_else(|| Error::Unavailable("this daemon cannot restart sessions".into()))?;
            let restarted = crate::stale::restart_stale(store, starter, sessions.as_deref())?;
            return Ok(restart_done(&restarted)?.response());
        }
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

/// The same start as `tiller orchestrator start`, as the board: a live
/// orchestrator is returned (`replayed`), never a second one, unless the
/// owner asked for a `restart`, and a retry of a start that landed replays
/// it by its key. `suspect` sees a store error, so the daemon can check the
/// database for damage.
fn start_here(
    starter: Option<&dyn OrchestratorStart>,
    store: &mut Store,
    ctx: &Ctx,
    project: &str,
    restart: bool,
    suspect: impl FnOnce(&Error),
) -> Result<Applied<Orchestrator>, ApiError> {
    let Some(starter) = starter else {
        return Err(Error::Unavailable("this host cannot start orchestrators".into()).into());
    };
    starter
        .start(store, ctx, project, restart)
        .map_err(|error| start_refusal(error, suspect))
}

/// A refused start or agent control as the board gets it, with the
/// command's own exit code as the status. A store failure is handed to
/// `suspect`, for the damage check.
pub(crate) fn start_refusal(error: StartError, suspect: impl FnOnce(&Error)) -> ApiError {
    match error {
        StartError::State { error, notes } => {
            suspect(&error);
            let mut api = ApiError::from(*error);
            if let Some(notes) = notes {
                api.body.error.message = format!("{}; {notes}", api.body.error.message);
            }
            api
        }
        StartError::Refused {
            exit,
            code,
            message,
            hint,
            current,
        } => {
            let mut api = ApiError::new(status_of_exit(exit), code, message);
            api.body.error.hint = hint;
            api.body.error.current = current;
            api
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
    guard(state, headers).await?;
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

impl Addressed {
    fn is_peer(&self) -> bool {
        !matches!(self, Self::Here)
    }
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

/// What a pane link answered with.
enum Opened {
    Stream(String, Box<PaneStream>),
    Answer(Value),
}

/// Opens a pane link when `host` is a paired peer (`protocol.md` 10.7);
/// `None` when the pane is here. A peer whose link is not live is 503 at
/// once. The link is read only: nothing here types into the pane.
async fn peer_pane(
    state: &AppState,
    host: Option<&str>,
    session: &str,
    kind: PaneKind,
    lines: Option<u16>,
) -> Result<Option<Opened>, ApiError> {
    let view = match addressed(state, host).await? {
        Addressed::Here => return Ok(None),
        Addressed::Peer(view) => view,
        Addressed::Unlinked(label) => return Err(ApiError::unreachable(&label)),
    };
    let label = view.label.as_str();
    if view.link != LinkState::Live {
        return Err(ApiError::unreachable(label));
    }
    match state
        .0
        .peers
        .open_pane(&view.host_id, session, kind, lines)
        .await
    {
        Paned::Unreachable => Err(ApiError::unreachable(label)),
        Paned::Answered(200, body) => Ok(Some(Opened::Answer(body))),
        Paned::Answered(status, body) => Err(ApiError::from((status, body)).host(label)),
        Paned::Stream(stream) => Ok(Some(Opened::Stream(view.label.clone(), stream))),
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
    guard(&state, &headers).await?;
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
              (status = 502, body = ErrorBody, description = "`peer_refused`"),
              (status = 503, body = ErrorBody, description = "`peer_unreachable`: nothing was sent or queued")))]
async fn ask_note(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::AskNote, Some(id), &bytes).await
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/orchestrator", tag = "mutations", request_body = StartOrchestrator,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody), (status = 422, body = ErrorBody), (status = 503, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`")))]
async fn start_orchestrator(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let mutation = Mutation::parse(Op::StartOrchestrator, Some(id.clone()), &bytes)?;
    if let Some(view) = peer_for(&state, mutation.host()).await? {
        return forward(&state, &view, mutation, &bytes).await;
    }
    writable(&state)?;
    let Some(starter) = state.0.orchestrators.clone() else {
        return Err(Error::Unavailable("this daemon cannot start orchestrators".into()).into());
    };
    let key = mutation.key().to_owned();
    let restart = matches!(mutation.parsed, Parsed::StartOrchestrator(true));
    let db = state.db();
    let damage = state.0.damage.clone();
    let args = request_args();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let mut store = Store::open_current(&db).map_err(ApiError::from)?;
        start_here(
            Some(starter.as_ref()),
            &mut store,
            &board_ctx(&key, args.as_ref()),
            &id,
            restart,
            |error| damage.suspect(&db, error),
        )
    })
    .await;
    let applied = match joined {
        Ok(result) => result?,
        Err(error) => return Err(Error::Internal(error.to_string()).into()),
    };
    Ok(Json(Done::of(applied)?.response()))
}

#[utoipa::path(post, path = "/api/v1/agents/restart-stale", tag = "mutations", request_body = RestartStale,
    responses((status = 200, body = MutationResponse, description = "`data` is a `RestartStaleResponse`"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 503, body = ErrorBody)))]
async fn restart_stale(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let mutation = Mutation::parse(Op::RestartStale, None, &bytes)?;
    if let Some(view) = peer_for(&state, mutation.host()).await? {
        return forward(&state, &view, mutation, &bytes).await;
    }
    let Parsed::RestartStale(sessions) = mutation.parsed else {
        return Err(Error::Internal("a restart parses as a restart".into()).into());
    };
    writable(&state)?;
    let Some(starter) = state.0.orchestrators.clone() else {
        return Err(Error::Unavailable("this daemon cannot restart sessions".into()).into());
    };
    let db = state.db();
    let damage = state.0.damage.clone();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let done = Store::open_current(&db).and_then(|mut store| {
            crate::stale::restart_stale(&mut store, starter.as_ref(), sessions.as_deref())
        });
        if let Err(error) = &done {
            damage.suspect(&db, error);
        }
        Ok(done?)
    })
    .await;
    let restarted = match joined {
        Ok(result) => result?,
        Err(error) => return Err(Error::Internal(error.to_string()).into()),
    };
    Ok(Json(restart_done(&restarted)?.response()))
}

/// What a restart pass answers with: the outcome of each session, and a
/// warning when some did not restart.
fn restart_done(restarted: &RestartStaleResponse) -> Result<Done, Error> {
    let failed = restarted
        .sessions
        .iter()
        .filter(|row| row.outcome == "failed")
        .count();
    Ok(Done {
        data: serde_json::to_value(restarted)?,
        events: Vec::new(),
        replayed: false,
        outbox_id: None,
        approval_id: None,
        warning: (failed > 0).then(|| format!("{failed} session(s) did not restart")),
    })
}

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
            action: unblock.action.map(delete_action),
        }),
        agents: preview.agents.into_iter().map(project_agent).collect(),
        unlanded: preview
            .unlanded
            .into_iter()
            .map(|work| ProjectUnlanded {
                path: work.path,
                branch: work.branch,
                card: work.card,
                unlanded_commits: work.unlanded_commits,
                dirty: work.dirty,
            })
            .collect(),
        open_prs: preview
            .open_prs
            .into_iter()
            .map(|pr| ProjectOpenPr {
                card: pr.card,
                number: pr.number,
                url: pr.url,
            })
            .collect(),
    }))
}

fn delete_action(action: tiller_core::state::deletion::UnblockAction) -> DeleteAction {
    use tiller_core::state::deletion::UnblockActionId;
    DeleteAction {
        id: match action.id {
            UnblockActionId::AgentStop => DeleteActionId::AgentStop,
            UnblockActionId::CardCancel => DeleteActionId::CardCancel,
        },
        target: action.target,
    }
}

fn project_agent(agent: tiller_core::state::deletion::LiveAgent) -> ProjectAgent {
    let tiller_core::state::deletion::LiveAgent {
        role,
        session,
        card,
        run,
        local,
    } = agent;
    ProjectAgent {
        role,
        session,
        card,
        run,
        local,
    }
}

/// A project delete or retire's agent stops, judged as the store's errors:
/// a stop that did not happen is a refusal naming the agent.
fn stopped_or(project: &str, error: tiller_agents::spawn::SpawnError) -> Error {
    use tiller_agents::spawn::SpawnError;
    match error {
        SpawnError::State(error) => error,
        other => Error::Refused {
            entity: "project",
            id: project.to_owned(),
            fact: other.to_string(),
        },
    }
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/delete", tag = "mutations", request_body = DeleteProject,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: an agent that did not stop, an executing approval, the fleet project, or another host's project or agent"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: DeleteProject = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a project delete").await?;
    if request.confirm.trim() != id {
        return Err(ApiError::usage(format!("type {id} to delete the project")));
    }
    let herdr = state.0.herdr.clone();
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let waits = tiller_agents::control::Waits::default();
            let deleted = tiller_agents::control::delete_project(store, ctx, &herdr, &waits, &id)
                .map_err(|error| stopped_or(&id, error))?;
            Done::of(deleted)
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/projects", tag = "mutations", request_body = AddProject,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: the id is taken by a different project; `unsuitable_checkout`: the Local checkout folder does not suit"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded; `clone_failed`: the clone into the Local checkout folder failed")))]
async fn add_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AddProject = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a project add").await?;
    let delivery: Delivery = request.delivery.parse()?;
    let key = key(&request.idempotency_key)?;
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| request.id.clone(), str::to_owned);
    let new = tiller_core::state::projects::NewProject {
        id: request.id,
        name,
        repo: request.repo,
        remote: None,
        default_branch: None,
        delivery,
        checks: None,
        capacity: request.capacity,
        deploy_workflow: None,
        deploy_environment: None,
        deploy_timeout_minutes: None,
    };
    let Some(checkout) = request.checkout else {
        return add_numbered(&state, key, new, None).await;
    };
    // The default folder is named after the id, so it is checked first.
    ids::validate_project_id(&new.id)?;
    writable(&state)?;
    // Its own task: a browser that goes away mid-clone does not leave a
    // clone with no project, or a project with no checkout.
    detached(add_with_checkout(state, key, new, checkout)).await
}

/// Add project with a Local checkout: the folder checked, the clone it
/// asks for run, then `project.add` and `checkouts.<id>`. A clone is taken
/// back when the add is refused.
async fn add_with_checkout(
    state: AppState,
    key: String,
    new: tiller_core::state::projects::NewProject,
    checkout: NewCheckout,
) -> ApiResult<MutationResponse> {
    let home = state.0.home.clone();
    let plan = {
        let (id, repo) = (new.id.clone(), new.repo.clone());
        read(&state, move |store| {
            Ok(checkout_plan(store, &home, &id, repo.as_deref(), &checkout))
        })
        .await??
    };
    let undo = {
        let plan = plan.clone();
        tokio::task::spawn_blocking(move || {
            tiller_agents::checkout::clone(&plan, tiller_agents::checkout::CLONE_LIMIT)
        })
        .await
        .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
        .map_err(checkout_refused)?
    };
    let done = add_numbered(&state, key, new, Some(plan.path().to_path_buf())).await;
    if done.is_err()
        && let Some(undo) = undo
    {
        // Taking back a whole clone is disk work: off the runtime, and done
        // before the refusal is answered.
        let _ = tokio::task::spawn_blocking(move || undo.run()).await;
    }
    done
}

/// What a Local checkout folder would do for project `id`, refused as the
/// error the board shows. Reads `config.toml` for `spawn.checkouts_root`.
fn checkout_plan(
    store: &Store,
    home: &std::path::Path,
    id: &str,
    repo: Option<&str>,
    checkout: &NewCheckout,
) -> Result<tiller_agents::checkout::Plan, ApiError> {
    let config = config_toml::load(&home.join("config.toml")).map_err(|error| {
        ApiError::from(Error::Unavailable(format!(
            "{error}; fix it before setting a checkout"
        )))
    })?;
    let root = checkouts_root(&config);
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let plan = tiller_agents::checkout::plan(&tiller_agents::checkout::Request {
        project: id,
        repo,
        path: &checkout.path,
        clone: checkout.clone,
        root: root.as_deref(),
        home: user_home.as_deref(),
    })
    .map_err(checkout_refused)?;
    // A clone for an id that is taken would be taken back at once.
    if matches!(plan, tiller_agents::checkout::Plan::Clone { .. }) {
        match store.project(id) {
            Ok(_) => {
                return Err(Error::Refused {
                    entity: "project",
                    id: id.to_owned(),
                    fact: "project id is taken".into(),
                }
                .into());
            }
            Err(Error::NotFound { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(plan)
}

/// `spawn.checkouts_root` from a parsed `config.toml`.
fn checkouts_root(config: &toml::Table) -> Option<PathBuf> {
    config_toml::get(config, "spawn.checkouts_root")
        .and_then(toml::Value::as_str)
        .map(PathBuf::from)
}

/// A 422 `unsuitable_checkout`, or a 502 `clone_failed`.
fn checkout_refused(error: tiller_agents::checkout::CheckoutError) -> ApiError {
    use tiller_agents::checkout::CheckoutError;
    match error {
        CheckoutError::Unsuitable(why) => {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "unsuitable_checkout", why)
        }
        CheckoutError::Clone(why) => ApiError::new(StatusCode::BAD_GATEWAY, "clone_failed", why)
            .hint("git clone must work in a terminal on this host with no password or question"),
    }
}

/// `project.add`, then `checkouts.<id>` when a checkout is given. The key
/// is written after the add, so a refused add leaves `config.toml` as it
/// was; a key that cannot be written is a warning on the added project.
async fn add_numbered(
    state: &AppState,
    key: String,
    new: tiller_core::state::projects::NewProject,
    checkout: Option<PathBuf>,
) -> ApiResult<MutationResponse> {
    let home = state.0.home.clone();
    let (new, numbering, note) = {
        let (home, checkout) = (home.clone(), checkout.clone());
        read(state, move |store| {
            // A config.toml that does not parse only hides the checkout: the
            // remote is still looked at.
            let checkout = checkout.or_else(|| {
                checkout_of(&home, &new.id)
                    .ok()
                    .flatten()
                    .map(PathBuf::from)
            });
            let (numbering, note) =
                tiller_agents::numbering::numbering(store, &new, checkout.as_deref())?;
            Ok((new, numbering, note))
        })
        .await?
    };
    mutate(state, key, move |store, ctx, _| {
        let id = new.id.clone();
        let mut done = Done::of(store.project_add_numbered(ctx, new, numbering)?)?;
        let mut notes: Vec<String> = note.into_iter().collect();
        if let Some(path) = checkout {
            let path = path.to_string_lossy().into_owned();
            let config_key = format!("checkouts.{id}");
            match write_checkout(&home, &config_key, &path) {
                Ok(false) => {}
                Ok(true) => match store.config_changed(
                    &ctx.unkeyed(),
                    &config_key,
                    json!(path),
                ) {
                    Ok(applied) => done.events.extend(applied.events),
                    Err(error) => {
                        tracing::warn!(%error, "recording the project's checkout failed");
                        notes.push(format!(
                            "{config_key} is in config.toml, but its config.changed event was not recorded: {error}"
                        ));
                    }
                },
                Err(error) => notes.push(format!(
                    "the project is added, but {config_key} could not be written ({error}); run tiller config set {config_key} {path}"
                )),
            }
        }
        Ok(done.with_warning((!notes.is_empty()).then(|| notes.join("; "))))
    })
    .await
}

/// Sets `key` in `<home>/config.toml` to `path`; `false` when it already was.
fn write_checkout(
    home: &std::path::Path,
    key: &str,
    path: &str,
) -> Result<bool, tiller_core::config_file::ConfigError> {
    write_config(home, key, toml::Value::String(path.to_owned()))
}

/// Sets `key` in `<home>/config.toml` to `value`; `false` when it already
/// was.
fn write_config(
    home: &std::path::Path,
    key: &str,
    value: toml::Value,
) -> Result<bool, tiller_core::config_file::ConfigError> {
    let file = home.join("config.toml");
    let mut config = config_toml::load(&file)?;
    if config_toml::get(&config, key) == Some(&value) {
        return Ok(false);
    }
    config_toml::set(&mut config, key, value)?;
    config_toml::store(&file, &config)?;
    Ok(true)
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct CheckoutQuery {
    /// The project id; an empty Local checkout folder is named after it.
    /// Absent is the same as empty: the board drops empty parameters.
    #[serde(default)]
    pub project: String,
    /// The GitHub repo, `owner/name`.
    pub repo: Option<String>,
    /// The folder as typed; absent or empty is `<spawn.checkouts_root>/<id>`.
    pub path: Option<String>,
    /// `true` checks the folder as one to clone into.
    pub clone: Option<bool>,
}

#[utoipa::path(get, path = "/api/v1/projects/checkout", tag = "reads", params(CheckoutQuery),
    responses((status = 200, body = CheckoutCheck), (status = 503, body = ErrorBody)))]
async fn project_checkout(
    State(state): State<AppState>,
    Query(query): Query<CheckoutQuery>,
) -> ApiResult<CheckoutCheck> {
    let home = state.0.home.clone();
    read(&state, move |store| {
        let checkout = NewCheckout {
            path: query.path.unwrap_or_default(),
            clone: query.clone.unwrap_or(false),
        };
        let repo = query
            .repo
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        let id = query.project.trim();
        if checkout.path.trim().is_empty() && ids::validate_project_id(id).is_err() {
            return Ok(Ok(CheckoutCheck {
                path: None,
                plan: None,
                remote: None,
                problem: Some("fill in the id first: the folder is named after it".into()),
            }));
        }
        Ok(match checkout_plan(store, &home, id, repo, &checkout) {
            Ok(plan) => {
                let path = Some(plan.path().to_string_lossy().into_owned());
                Ok(match plan {
                    tiller_agents::checkout::Plan::Use { .. } => CheckoutCheck {
                        path,
                        plan: Some("use".into()),
                        remote: None,
                        problem: None,
                    },
                    tiller_agents::checkout::Plan::Clone { remote, .. } => CheckoutCheck {
                        path,
                        plan: Some("clone".into()),
                        remote: Some(remote),
                        problem: None,
                    },
                })
            }
            Err(error) if error.status == StatusCode::SERVICE_UNAVAILABLE => Err(error),
            Err(error) => Ok(CheckoutCheck {
                path: None,
                plan: None,
                remote: None,
                problem: Some(error.body.error.message.clone()),
            }),
        })
    })
    .await?
    .map(Json)
}

/// What Add a host reads before it reaches the new host.
struct PairingStart {
    replay: bool,
    this: Host,
    host_id: Option<String>,
    paired: Option<Host>,
    /// Every paired host: `(label, peer_host_id)`.
    peers: Vec<(String, Option<String>)>,
}

#[utoipa::path(post, path = "/api/v1/peers", tag = "mutations", request_body = AddPeer,
    responses((status = 200, body = MutationResponse, description = "The paired host row, with `ssh`, `key_id` and the key's `fingerprint`; never the key"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`label_taken`: this host's own label, or one already paired; `already_paired`: the alias or the host it reaches is paired under another label"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: the new host refused the pairing (`reason`), or a `host` was sent"),
              (status = 503, body = ErrorBody, description = "`peer_unreachable`: ssh did not reach the new host, or it gave no answer in 30 s")))]
async fn add_peer(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AddPeer = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a pairing").await?;
    let key = key(&request.idempotency_key)?;
    let scope: PeerScope = request.scope.parse()?;
    check_label(&request.label)?;
    check_alias(&request.ssh)?;
    // Refused before the new host accepts a key this host could not record.
    writable(&state)?;
    // Its own task: a browser that goes away mid-ssh does not cut a pairing
    // off between the new host's accept and this host's record of it.
    detached(pair_host(state, key, request.label, request.ssh, scope)).await
}

/// Add a host's steps 2 to 4 (`protocol.md` 10.1), one pairing at a time.
async fn pair_host(
    state: AppState,
    key: String,
    label: String,
    alias: String,
    scope: PeerScope,
) -> ApiResult<MutationResponse> {
    let _alone = pair::PAIRING.lock().await;
    let home = state.0.home.clone();
    let start = {
        let (label, key) = (label.clone(), key.clone());
        read(&state, move |store| {
            Ok(PairingStart {
                replay: store.key_recorded(&Ctx::new(Actor::Board).with_key(key))?,
                this: store.this_host()?,
                host_id: meta(store.conn(), "host_id")?,
                paired: store
                    .host_labelled(&label)?
                    .filter(|host| host.peer_scope.is_some()),
                peers: store
                    .hosts()?
                    .into_iter()
                    .filter(|host| host.peer_scope.is_some())
                    .map(|host| (host.label, host.peer_host_id))
                    .collect(),
            })
        })
        .await?
    };
    // `config.toml` is read now: one that does not parse would fail the
    // pairing only after the new host accepted.
    let config = config_toml::load(&home.join("config.toml"))
        .map_err(|error| Error::Unavailable(format!("{error}; fix it before adding a host")))?;
    if start.replay {
        // The stored response answers before `peer.pair` reads its input,
        // with the alias and key the first request recorded.
        let Some(peer_host_id) = start.paired.and_then(|host| host.peer_host_id) else {
            return Err(Error::Usage(format!(
                "the pairing this idempotency key made is gone: {label} is no longer paired; add the host again"
            ))
            .into());
        };
        let alias = config_toml::get(&config, &format!("peers.{label}.ssh"))
            .and_then(toml::Value::as_str)
            .map_or(alias, str::to_owned);
        return mutate(&state, key, move |store, ctx, _| {
            let paired = store.peer_pair(
                ctx,
                NewPairing {
                    label: label.clone(),
                    herdr_machine: None,
                    scope,
                    peer_host_id,
                },
            )?;
            let key = pairing::key_path(&home, &label)
                .and_then(|path| pairing::read_key_file(&path))
                .ok()
                .map(|key| (key.key_id, fingerprint(&key.public_key)));
            Ok(paired_done(&alias, paired, key, None))
        })
        .await;
    }
    if start.this.label == label {
        return Err(label_taken(format!(
            "{label} is this host's own label; pick another"
        )));
    }
    if start.paired.is_some() {
        return Err(label_taken(format!(
            "{label} is already paired; pick another label, or Unpair it first in its Settings > Hosts card (or run tiller peer remove {label})"
        )));
    }
    // A host paired again under a new label would take the new key in
    // place of the key its first label dials with.
    if let Some(other) = pair::labels_for(&config, &alias)
        .into_iter()
        .find(|other| start.peers.iter().any(|(label, _)| label == other))
    {
        return Err(already_paired(format!(
            "{alias} is already paired here as {other}"
        )));
    }
    // This host's label is a word on the new host's command line.
    check_label(&start.this.label)?;
    let host_id = start.host_id.ok_or_else(|| {
        ApiError::from(Error::Unavailable(format!(
            "state.db has no host id; {}",
            tiller_core::RUN_INIT
        )))
    })?;

    // Step 2: the key, under a pending name until the new host accepts it.
    let signing = pairing::generate_key()?;
    let public_key = pairing::public_key_text(&signing);
    let path = pairing::key_path(&home, &label)?;
    // Its own name: `tiller peer add` for the same label pends `.<label>.key.pending`.
    let pending = path.with_file_name(format!(".{label}.board.key.pending"));
    let file = KeyFile::new(&label, scope, &signing);
    drop(signing);
    pairing::write_key_file(&pending, &file)?;
    let drop_pending = |error: ApiError| {
        let _ = pairing::remove_key_file(&pending);
        error
    };

    // Step 3: the new host's unattended accept, over ssh.
    let accept = Accept {
        alias: &alias,
        from: &start.this.label,
        host_id: &host_id,
        public_key: &public_key,
        scope,
    };
    let answer = pair::run(&state.0.ssh, &accept, pair::ACCEPT_LIMIT)
        .await
        .map_err(|no| drop_pending(not_reached(&label, &alias, &no)))?;
    let reply = pair::accepted_reply(&label, scope, &answer)
        .map_err(|not| drop_pending(not_accepted(&label, &alias, not)))?;
    if let Some((other, _)) = start
        .peers
        .iter()
        .find(|(_, id)| id.as_deref() == Some(reply.host_id.as_str()))
    {
        return Err(drop_pending(already_paired(format!(
            "{alias} is {other}, already paired here; it now holds this pairing's key in place of {other}'s, so run tiller peer add {other} in a terminal to restore that link"
        ))));
    }

    // Step 4: the key file, the alias, then `peer.pair` and the alias's
    // `config.set`. A failure deletes the key file: Add a host again
    // replaces the key the new host holds.
    let file = file.accepted(&reply.host_id, &reply.key_id);
    let key_id = reply.key_id.clone();
    let printed = fingerprint(&public_key);
    pairing::write_key_file(&path, &file).map_err(|error| drop_pending(error.into()))?;
    let _ = pairing::remove_key_file(&pending);
    let done = {
        let (home, label, alias) = (home.clone(), label.clone(), alias.clone());
        mutate(&state, key, move |store, ctx, _| {
            // The alias first: a `peers.<label>.ssh` with no pairing is
            // harmless, a pairing with no alias dials the label.
            let changed = pair::write_alias(&home, &label, &alias)
                .map_err(|error| Error::Internal(error.to_string()))?;
            let paired = store.peer_pair(
                ctx,
                NewPairing {
                    label: label.clone(),
                    herdr_machine: None,
                    scope,
                    peer_host_id: reply.host_id.clone(),
                },
            )?;
            // The key went with `peer.pair`; this event is its own write.
            let recorded = changed.then(|| {
                store.config_changed(&ctx.unkeyed(), &format!("peers.{label}.ssh"), json!(alias))
            });
            Ok(paired_done(
                &alias,
                paired,
                Some((Some(key_id), printed)),
                recorded,
            ))
        })
        .await
    };
    if done.is_err() {
        let _ = pairing::remove_key_file(&path);
    }
    done
}

/// A 422 `label_taken`: Add a host never re-pairs a label.
fn label_taken(message: String) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "label_taken", message)
}

/// A 422 `already_paired`: the alias, or the host it reaches, is paired
/// under another label.
fn already_paired(message: String) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "already_paired", message)
}

/// The accept gave no answer: a 503 `peer_unreachable` when ssh did not get
/// through or ran out of time, a 502 `peer_refused` when the new host's
/// shell has no `tiller`.
fn not_reached(label: &str, alias: &str, no: &NoAnswer) -> ApiError {
    let why = &no.why;
    let error = match no.cause {
        Cause::Unreached => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "peer_unreachable",
            format!("{alias} is not reachable through ssh: {why}"),
        )
        .hint(&format!(
            "run ssh {alias} once in a terminal on this host: it must connect with no password or question"
        )),
        Cause::TimedOut => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "peer_unreachable",
            format!("{alias} gave no answer through ssh: {why}"),
        )
        .hint("it may have accepted the key; adding the host again replaces that key"),
        Cause::NoTiller => ApiError::peer_refused(
            "no_tiller",
            format!("{alias} has no tiller command for ssh to run: {why}"),
        )
        .hint(&format!(
            "install Tiller there, so ssh {alias} tiller --version answers, then add the host again"
        )),
    };
    error.host(label)
}

/// A 502 `peer_refused`: the new host answered, and not with an accept.
fn not_accepted(label: &str, alias: &str, not: NotAccepted) -> ApiError {
    let (reason, message) = match not {
        NotAccepted::Answered { code, message } if code == "refused" => {
            if message.contains("federation.accept") {
                (
                    "accept_off",
                    format!("{alias} refuses pairing: its config has federation.accept = false"),
                )
            } else {
                ("refused", format!("{alias} refused the pairing: {message}"))
            }
        }
        NotAccepted::Answered { code, message } if code == "forbidden" => (
            "forbidden",
            format!("{alias} refused the pairing: {message}"),
        ),
        // An older Tiller has no `--unattended`.
        NotAccepted::Answered { code, message }
            if code == "usage" && message.contains("--unattended") =>
        {
            (
                "version",
                format!(
                    "{alias} runs a Tiller that cannot pair from the board; update it there, or run tiller peer add in a terminal"
                ),
            )
        }
        NotAccepted::Answered { message, .. } => (
            "failed",
            format!("{alias} could not accept the pairing: {message}"),
        ),
        NotAccepted::Malformed(message) => ("bad_answer", message),
    };
    ApiError::peer_refused(reason, message).host(label)
}

/// The paired host row with its alias and key, never the key's secret half.
/// `key` is the key's id and fingerprint.
fn paired_done(
    alias: &str,
    paired: Applied<Host>,
    key: Option<(Option<String>, String)>,
    recorded: Option<tiller_core::Result<Applied<i64>>>,
) -> Done {
    let (key_id, printed) = key.unzip();
    let mut data = serde_json::to_value(&paired.value).unwrap_or(Value::Null);
    data["ssh"] = json!(alias);
    data["key_id"] = json!(key_id.flatten());
    data["fingerprint"] = json!(printed);
    let mut events = paired.events;
    let warning = match recorded {
        Some(Ok(applied)) => {
            events.extend(applied.events);
            None
        }
        Some(Err(error)) => {
            tracing::warn!(%error, "recording the pairing's ssh alias failed");
            Some(format!(
                "the ssh alias is in config.toml, but its config.changed event was not recorded: {error}"
            ))
        }
        None => None,
    };
    Done {
        data,
        events,
        replayed: paired.replayed,
        outbox_id: None,
        approval_id: None,
        warning,
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/retire", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = ProjectRetirePreview), (status = 404, body = ErrorBody)))]
async fn project_retire_preview(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ProjectRetirePreview> {
    let preview = read(&state, move |store| store.project_retire_preview(&id)).await?;
    Ok(Json(ProjectRetirePreview {
        project: preview.project,
        refused: preview.refused,
        agents: preview.agents.into_iter().map(project_agent).collect(),
    }))
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/setup", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = ProjectSetup), (status = 404, body = ErrorBody)))]
async fn project_setup(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ProjectSetup> {
    let home = state.0.home.clone();
    read(&state, move |store| {
        let project = store.project(&id)?;
        let changed = store.merge_gate_changed_at(&project.id)?;
        let (checkout, config_error) = match checkout_of(&home, &project.id) {
            Ok(checkout) => (checkout, None),
            Err(error) => (None, Some(error)),
        };
        Ok(ProjectSetup {
            merge_gate: project.merge_gate.to_string(),
            merge_gate_changed_at: changed.map(|at| at.to_string()),
            project: project.id,
            repo: project.repo,
            checkout,
            config_error,
        })
    })
    .await
    .map(Json)
}

#[utoipa::path(get, path = "/api/v1/projects/{id}", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = ProjectFields), (status = 404, body = ErrorBody)))]
async fn project_fields(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ProjectFields> {
    read(&state, move |store| {
        let project = store.project(&id)?;
        Ok(ProjectFields {
            id: project.id,
            name: project.name,
            repo: project.repo,
            remote: project.remote,
            default_branch: project.default_branch,
            delivery: project.delivery.to_string(),
            checks: project.checks,
            capacity: project.capacity,
            deploy_workflow: project.deploy_workflow,
            deploy_environment: project.deploy_environment,
            deploy_timeout_minutes: project.deploy_timeout_minutes,
            rev: project.rev,
        })
    })
    .await
    .map(Json)
}

#[utoipa::path(patch, path = "/api/v1/projects/{id}", tag = "mutations", request_body = EditProject,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "`conflict`: `rev` is stale; `current` holds the new `rev` and the stored value of the first field the edit changes"),
              (status = 422, body = ErrorBody, description = "`refused`: the project is retired"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn edit_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: EditProject = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a project edit").await?;
    let delivery = request
        .delivery
        .as_deref()
        .map(str::parse::<Delivery>)
        .transpose()?;
    // An empty text clears an optional field, as `tiller project edit`'s
    // empty flag does.
    let clearable = |text: Option<String>| text.map(|text| Some(text).filter(|t| !t.is_empty()));
    let edit = tiller_core::state::projects::ProjectEdit {
        name: request.name,
        repo: clearable(request.repo),
        remote: clearable(request.remote),
        default_branch: request.default_branch,
        delivery,
        checks: request.checks,
        capacity: request.capacity,
        deploy_workflow: clearable(request.deploy_workflow),
        deploy_environment: clearable(request.deploy_environment),
        deploy_timeout_minutes: request.deploy_timeout_minutes,
    };
    let rev = request.rev;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.project_edit(ctx, &id, rev, edit)?),
    )
    .await
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/grants", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = ProjectGrants), (status = 404, body = ErrorBody)))]
async fn project_grants(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<ProjectGrants> {
    read(&state, move |store| {
        let grants = store
            .grants(&id)?
            .into_iter()
            .map(|row| GrantRow {
                short: ids::short(&row.grant.id).to_owned(),
                id: row.grant.id,
                project_id: row.grant.project_id,
                kind: row.grant.kind.to_string(),
                conditions: row.grant.conditions,
                granted_at: row.grant.granted_at.to_string(),
                granted_by: row.grant.granted_by,
                revoked_at: row.grant.revoked_at.map(|at| at.to_string()),
                revoke_reason: row.grant.revoke_reason,
                uses: row.uses,
            })
            .collect();
        Ok(ProjectGrants {
            project: id,
            grants,
        })
    })
    .await
    .map(Json)
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/grants", tag = "mutations", request_body = CreateGrant,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the new grant"),
              (status = 400, body = ErrorBody, description = "`usage`: the kind, the conditions or `expires` is not one a grant takes"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn create_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: CreateGrant = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a grant").await?;
    let kind = request.kind.parse::<GrantKind>()?;
    // The checks `tiller grant create` makes before the store's own.
    let mut conditions = request.conditions;
    if !conditions.is_object() {
        return Err(Error::Usage("conditions is a JSON object".into()).into());
    }
    if let Some(expires) = &request.expires {
        conditions["expires_at"] = json!(tiller_core::time::lisbon::parse_when(expires)?);
    }
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            Done::of(store.grant_create(
                ctx,
                NewGrant {
                    project_id: id,
                    kind,
                    conditions,
                },
            )?)
        },
    )
    .await
}

#[utoipa::path(delete, path = "/api/v1/grants/{id}", tag = "mutations", request_body = RevokeGrant,
    params(("id" = String, Path, description = "A grant's ULID or its short form")),
    responses((status = 200, body = MutationResponse, description = "`data` is the revoked grant; revoking a revoked grant returns it"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn revoke_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: RevokeGrant = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a grant revoke").await?;
    let reason = request
        .reason
        .unwrap_or_else(|| "revoked by the owner".into());
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.grant_revoke(ctx, &id, &reason)?),
    )
    .await
}

#[utoipa::path(get, path = "/api/v1/start-settings", tag = "reads",
    responses((status = 200, body = StartSettings), (status = 503, body = ErrorBody)))]
async fn start_settings(State(state): State<AppState>) -> ApiResult<StartSettings> {
    let Some(starter) = state.0.orchestrators.clone() else {
        return Err(Error::Unavailable("this daemon cannot start orchestrators".into()).into());
    };
    match tokio::task::spawn_blocking(move || starter.settings()).await {
        Ok(settings) => Ok(Json(settings)),
        Err(error) => Err(Error::Internal(error.to_string()).into()),
    }
}

#[utoipa::path(get, path = "/api/v1/host-config", tag = "reads",
    responses((status = 200, body = HostConfig), (status = 503, body = ErrorBody)))]
async fn host_config_read(State(state): State<AppState>) -> ApiResult<HostConfig> {
    let home = state.0.home.clone();
    let db = state.db();
    let read = tokio::task::spawn_blocking(move || {
        let boundary = Store::open_current(&db)
            .ok()
            .and_then(|store| meta(store.conn(), "boundary").ok().flatten());
        host_config_of(&home, boundary)
    })
    .await;
    match read {
        Ok(config) => Ok(Json(config)),
        Err(error) => Err(Error::Internal(error.to_string()).into()),
    }
}

/// The host keys of `<home>/config.toml`. A file that does not parse reads as
/// the defaults and says why.
fn host_config_of(home: &std::path::Path, boundary: Option<String>) -> HostConfig {
    let (table, config_error) = match config_toml::load(&home.join("config.toml")) {
        Ok(table) => (table, None),
        Err(error) => (toml::Table::new(), Some(error.to_string())),
    };
    let text = |key: &str| {
        config_toml::get(&table, key)
            .and_then(toml::Value::as_str)
            .map(str::to_owned)
    };
    let (learning, learning_error) =
        match tiller_core::state::learning::Learning::from_table(&table) {
            Ok(learning) => (learning, None),
            Err(error) => (Default::default(), Some(error.to_string())),
        };
    HostConfig {
        herdr_terminal: text(host_config::HERDR_TERMINAL),
        stop_hook_mode: text(host_config::STOP_HOOK_MODE)
            .filter(|mode| host_config::STOP_HOOK_MODES.contains(&mode.as_str()))
            .unwrap_or_else(|| "observe".to_owned()),
        accept_peers: crate::peer::federation_accept(home),
        memory_remote: text(host_config::MEMORY_REMOTE).filter(|remote| !remote.is_empty()),
        boundary,
        learning: LearningConfig {
            sample_every: i64::try_from(learning.sample_every).unwrap_or(i64::MAX),
            window_cards: learning.window_cards,
            curator_model: learning.curator_model,
            curator_timeout_minutes: i64::try_from(learning.curator_timeout_minutes)
                .unwrap_or(i64::MAX),
            evals_dir: learning.evals_dir,
        },
        config_error: config_error.or(learning_error),
    }
}

#[utoipa::path(post, path = "/api/v1/config", tag = "mutations", request_body = SetConfig,
    responses((status = 200, body = MutationResponse, description = "`data` is the key and the value; `warning` says what else the change leaves wrong"),
              (status = 400, body = ErrorBody, description = "`usage`: not a setting the board sets, or a value a start would refuse"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody)))]
async fn set_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: SetConfig = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a config change").await?;
    let Some(starter) = state.0.orchestrators.clone() else {
        return Err(Error::Unavailable("this daemon cannot check start settings".into()).into());
    };
    let home = state.0.home.clone();
    let SetConfig {
        key: setting,
        value,
        ..
    } = request.clone();
    if setting == host_config::STOP_HOOK_MODE && value == "block-once" {
        block_once_allowed(&state).await?;
    }
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            if !tiller_agents::spawn::settings::settable(&setting) {
                return Err(Error::Usage(format!(
                    "{setting} is not a setting the board can change"
                )));
            }
            // A start setting is a path judged by the rules of a start. Any
            // other settable key is judged by its own rule, into the TOML its
            // reader takes.
            let (typed, warnings) = if host_config::is_key(&setting) {
                let boundary = meta(store.conn(), "boundary")?;
                let typed = host_config::check(
                    &setting,
                    &value,
                    host_config::Host {
                        boundary: boundary.as_deref(),
                    },
                )
                .map_err(Error::Usage)?;
                (typed, Vec::new())
            } else {
                if let Some(project) = setting.strip_prefix("checkouts.") {
                    store.project(project)?;
                }
                let warnings = starter
                    .check_setting(&setting, &value)
                    .map_err(Error::Usage)?;
                (toml::Value::String(value.clone()), warnings)
            };
            let recorded = serde_json::to_value(&typed)?;
            let changed = write_config(&home, &setting, typed)
                .map_err(|error| Error::Unavailable(error.to_string()))?;
            // As `tiller config set`: a key already at that value records
            // nothing.
            let applied = if changed {
                store.config_changed(ctx, &setting, recorded.clone())?
            } else {
                Applied {
                    value: 0,
                    events: Vec::new(),
                    replayed: true,
                }
            };
            let mut done = Done::of(Applied {
                value: json!({"key": setting, "value": recorded, "changed": changed}),
                events: applied.events,
                replayed: applied.replayed,
            })?;
            done.warning = (!warnings.is_empty()).then(|| warnings.join("; "));
            Ok(done)
        },
    )
    .await
}

/// The switching rule for `stop_hook_mode = "block-once"` (`protocol.md`
/// 2.5), judged as `tiller config set` judges it. A refusal is a 422 with
/// the numbers behind it in `current`, and the window they cover.
async fn block_once_allowed(state: &AppState) -> Result<(), ApiError> {
    let now = jiff::Timestamp::now();
    let stats = read(state, move |store| {
        stop_hook::intake_stats_at(store, stop_hook::STOP_HOOK_DAYS, now)
    })
    .await?;
    let Some(refusal) = stats.refusal() else {
        return Ok(());
    };
    let mut current = serde_json::to_value(&stats).unwrap_or(Value::Null);
    current["since"] = json!(
        Timestamp::from_jiff(
            now.checked_add(jiff::SignedDuration::from_hours(-24 * stats.days))
                .unwrap_or(now)
        )
        .to_string()
    );
    current["until"] = json!(Timestamp::from_jiff(now).to_string());
    let mut error = ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "refused", refusal);
    error.body.error.current = Some(current);
    Err(error)
}

/// `checkouts.<project>` from `<home>/config.toml`, as `tiller config get`
/// reads it; a missing file or key is `None`.
fn checkout_of(home: &std::path::Path, project: &str) -> Result<Option<String>, String> {
    let table = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => text
            .parse::<toml::Table>()
            .map_err(|error| format!("config.toml does not parse: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read config.toml: {error}")),
    };
    Ok(table
        .get("checkouts")
        .and_then(|checkouts| checkouts.get(project))
        .and_then(toml::Value::as_str)
        .map(str::to_owned))
}

#[utoipa::path(get, path = "/api/v1/recap", tag = "reads",
    responses((status = 200, body = RecapResponse), (status = 503, body = ErrorBody)))]
async fn recap(State(state): State<AppState>) -> ApiResult<RecapResponse> {
    read(&state, |store| {
        let recap = tiller_core::state::recap::recap(store.conn(), Timestamp::now())?;
        Ok(recap_response(&recap))
    })
    .await
    .map(Json)
}

fn recap_response(recap: &tiller_core::state::recap::Recap) -> RecapResponse {
    RecapResponse {
        seen_at: recap.seen_at.map(|at| at.to_string()),
        since: recap.since.to_string(),
        landed: recap.tally.landed,
        decisions: recap.tally.decisions,
        failed_runs: recap.tally.failed_runs,
        suggestions: recap.tally.suggestions,
    }
}

#[utoipa::path(post, path = "/api/v1/recap/seen", tag = "mutations", request_body = MarkRecapSeen,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn mark_recap_seen(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: MarkRecapSeen = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "marking a recap seen").await?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let marked = store.recap_mark_seen(ctx)?;
            Ok(Done {
                data: serde_json::to_value(recap_response(&marked.value))?,
                events: marked.events,
                replayed: marked.replayed,
                outbox_id: None,
                approval_id: None,
                warning: None,
            })
        },
    )
    .await
}

fn reply_item(view: tiller_core::state::replies::ReplyView) -> ReplyItem {
    let relation = view.card_relation.map(|relation| relation.to_string());
    ReplyItem {
        id: view.reply.id,
        project: view.reply.project_id,
        to: view.to.as_str().to_owned(),
        ask_id: view.reply.ask_id,
        message_id: view.reply.message_id,
        question: view.question,
        asked_at: view.asked_at.to_string(),
        body: view.reply.body,
        replied_at: view.reply.replied_at.to_string(),
        read_at: view.reply.read_at.map(|at| at.to_string()),
        rev: view.reply.rev,
        card: view.card.map(|card| ReplyCard {
            id: card.id,
            display_id: card.display_id,
            title: card.title,
            relation,
        }),
    }
}

#[utoipa::path(get, path = "/api/v1/projects/{id}/replies", tag = "reads",
    params(("id" = String, Path)),
    responses((status = 200, body = RepliesResponse), (status = 404, body = ErrorBody),
              (status = 503, body = ErrorBody)))]
async fn project_replies(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<RepliesResponse> {
    read(&state, move |store| {
        let project = store.project(&id)?;
        let replies = tiller_core::state::replies::unread(store.conn(), &project.id)?;
        Ok(RepliesResponse {
            replies: replies.into_iter().map(reply_item).collect(),
        })
    })
    .await
    .map(Json)
}

#[utoipa::path(post, path = "/api/v1/replies/{id}/read", tag = "mutations", request_body = MarkReplyRead,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn mark_reply_read(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: MarkReplyRead = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "marking a reply read").await?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let read = store.reply_read(ctx, &id)?;
            Ok(Done {
                data: serde_json::to_value(reply_item(read.value))?,
                events: read.events,
                replayed: read.replayed,
                outbox_id: None,
                approval_id: None,
                warning: None,
            })
        },
    )
    .await
}

#[utoipa::path(get, path = "/api/v1/host", tag = "reads",
    responses((status = 200, body = HostFields), (status = 503, body = ErrorBody)))]
async fn host_fields(State(state): State<AppState>) -> ApiResult<HostFields> {
    let home = state.0.home.clone();
    read(&state, move |store| {
        let host = store.local_host()?;
        Ok(HostFields {
            lanes_busy: store
                .capacity()?
                .into_iter()
                .find(|lanes| lanes.host_id == host.id)
                .map_or(0, |lanes| lanes.reserved),
            boundary: tiller_core::state::projects::meta(store.conn(), "boundary")?,
            boundary_recorded_by: store.boundary_recorded()?.map(str::to_owned),
            ram_free_mb: readings::ram_free_mb(),
            disk_free_mb: readings::disk_free_mb(&home),
            id: host.id,
            label: host.label,
            lane_capacity: host.lane_capacity,
            ram_floor_mb: host.ram_floor_mb,
            disk_floor_mb: host.disk_floor_mb,
            rev: host.rev,
        })
    })
    .await
    .map(Json)
}

#[utoipa::path(patch, path = "/api/v1/host", tag = "mutations", request_body = EditHost,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "`conflict`: `rev` is stale; `current` holds the new `rev` and the stored value of the first field the edit changes"),
              (status = 422, body = ErrorBody, description = "`refused`: the label is taken or a peer is paired, or memory, a backup or an export records the boundary"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn edit_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: EditHost = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a host edit").await?;
    let boundary = request
        .boundary
        .as_deref()
        .map(str::parse::<Boundary>)
        .transpose()?;
    let edit = tiller_core::state::projects::HostEdit {
        label: request.label,
        lane_capacity: request.lane_capacity,
        ram_floor_mb: request.ram_floor_mb,
        disk_floor_mb: request.disk_floor_mb,
        bug_project_id: None,
        boundary,
    };
    let rev = request.rev;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let host = store.local_host()?;
            Done::of(store.host_edit(ctx, &host.id, rev, edit)?)
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/bug-project", tag = "mutations", request_body = PickBugProject,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 409, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: fleet, a retired project, or another host's project"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn pick_bug_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PickBugProject = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a bug project pick").await?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let host = store.local_host()?;
            let edit = tiller_core::state::projects::HostEdit {
                bug_project_id: Some(request.project),
                ..Default::default()
            };
            Done::of(store.host_edit(ctx, &host.id, request.rev, edit)?)
        },
    )
    .await
}

#[utoipa::path(post, path = "/api/v1/projects/{id}/retire", tag = "mutations", request_body = RetireProject,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: an open card, the fleet project, or an agent that did not stop"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn retire_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: RetireProject = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a project retire").await?;
    let herdr = state.0.herdr.clone();
    let stop_agents = request.stop_agents;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let waits = tiller_agents::control::Waits::default();
            let retired = tiller_agents::control::retire_project(
                store,
                ctx,
                &herdr,
                &waits,
                &id,
                stop_agents,
            )
            .map_err(|error| stopped_or(&id, error))?;
            Done::of(retired)
        },
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
    guard(&state, &headers).await?;
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

#[utoipa::path(post, path = "/api/v1/cards/{id}/deps", tag = "mutations", request_body = DepBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the dependency"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "a loop, a card waiting on itself, or a closed card"),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn add_dep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::AddDep, Some(id), &bytes).await
}

#[utoipa::path(delete, path = "/api/v1/cards/{id}/deps", tag = "mutations", request_body = DepBody,
    params(("id" = String, Path)),
    responses((status = 200, body = MutationResponse,
               description = "`data` is the removed dependency, or null when none was left to remove"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody), (status = 503, body = ErrorBody), (status = 504, body = ErrorBody)))]
async fn remove_dep(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    submit(&state, &headers, Op::RemoveDep, Some(id), &bytes).await
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
    guard(&state, &headers).await?;
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

/// An away scope from the board's body, as `tiller away start` builds it.
fn away_scope(request: &AwayScopeBody) -> tiller_core::Result<Value> {
    let until = tiller_core::time::lisbon::parse_when(&request.until)?;
    let mut scope = json!({"until_at": until, "projects": request.projects});
    if let Some(orders) = &request.orders {
        scope["orders"] = json!(orders);
    }
    let mut spend = serde_json::Map::new();
    if let Some(cap) = request.max_new_runs {
        spend.insert("max_new_runs".into(), json!(cap));
    }
    if let Some(floor) = request.quota_floor_percent {
        spend.insert("quota_floor_percent".into(), json!(floor));
    }
    if !spend.is_empty() {
        scope["spend"] = Value::Object(spend);
    }
    if let Some(merge) = &request.merge {
        scope["merge"] = merge.clone();
    }
    Ok(scope)
}

#[utoipa::path(post, path = "/api/v1/away", tag = "mutations", request_body = AwayScopeBody,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 409, body = ErrorBody, description = "away mode is already on")))]
async fn away_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AwayScopeBody = body(&bytes)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let scope = away_scope(&request)?;
            Done::of(store.away_start(ctx, scope)?)
        },
    )
    .await
}

#[utoipa::path(put, path = "/api/v1/away", tag = "mutations", request_body = AwayScopeBody,
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody)))]
async fn away_edit(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AwayScopeBody = body(&bytes)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let scope = away_scope(&request)?;
            Done::of(store.away_edit(ctx, scope)?)
        },
    )
    .await
}

#[utoipa::path(delete, path = "/api/v1/away", tag = "mutations", request_body = AwayEndBody,
    responses((status = 200, body = MutationResponse, description = "`data.away` is the ended contract, `null` when away mode was off; `data.brief` is its return brief"),
              (status = 403, body = ErrorBody)))]
async fn away_end(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AwayEndBody = body(&bytes)?;
    let reason = request
        .reason
        .clone()
        .unwrap_or_else(|| "ended by the owner".into());
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let applied = store.away_end(ctx, &reason)?;
            let brief = match &applied.value {
                Some(away) => tiller_agents::handoff::ReturnBrief::load(
                    store.conn(),
                    Some(&away.id),
                    Timestamp::now(),
                )?,
                None => None,
            };
            Ok(Done {
                data: json!({"away": applied.value, "brief": brief}),
                events: applied.events,
                replayed: applied.replayed,
                outbox_id: None,
                approval_id: None,
                warning: None,
            })
        },
    )
    .await
}

#[utoipa::path(get, path = "/api/v1/away/brief", tag = "reads",
    responses((status = 200, body = AwayBriefResponse)))]
async fn away_brief(State(state): State<AppState>) -> ApiResult<AwayBriefResponse> {
    read(&state, |store| {
        Ok(AwayBriefResponse {
            brief: tiller_agents::handoff::ReturnBrief::load(store.conn(), None, Timestamp::now())?,
        })
    })
    .await
    .map(Json)
}

/// An agent's session id, its pane, and the saved machine it runs on, if
/// any.
async fn pane_of(
    state: &AppState,
    session: String,
) -> Result<(String, String, Option<String>), ApiError> {
    read(state, move |store| pane_target(store, &session)).await
}

/// [`pane_of`] on an open store, for `peer pane` too.
pub(crate) fn pane_target(
    store: &Store,
    session: &str,
) -> tiller_core::Result<(String, String, Option<String>)> {
    let id = resolve_id(store.conn(), "sessions", "session", session)?;
    let session: Session = get_row(store.conn(), "session", &id)?;
    let (pane, machine) = tiller_core::state::runs::focus_target(store.conn(), &session)?;
    Ok((session.id, pane, machine))
}

/// An agent's pane on this machine, for the terminal mirror; an agent on a
/// saved machine is refused, since herdr's observe stream stays local.
async fn local_pane(state: &AppState, session: String) -> Result<(String, String), ApiError> {
    let (session_id, pane, machine) = pane_of(state, session).await?;
    if let Some(machine) = machine {
        return Err(saved_machine(&machine));
    }
    Ok((session_id, pane))
}

/// The refusal for an agent on a saved machine, on the stream and the
/// history of this host's own panes and of a peer's alike.
pub(crate) fn saved_machine(machine: &str) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "refused",
        format!("the agent runs on the saved machine {machine}; the terminal mirror shows panes on this machine only"),
    )
    .hint("open it in herdr instead")
}

/// herdr's answer to a pane call, as the board API's error.
pub(crate) fn pane_error(pane: &str, error: &HerdrError) -> ApiError {
    match error {
        HerdrError::Api { code, .. } if code == "pane_not_found" => pane_gone(pane),
        HerdrError::NotFound { .. } => pane_gone(pane),
        HerdrError::Api { code, message, .. } if code == "invalid_key" => {
            ApiError::usage(format!("herdr refused a key: {message}"))
        }
        error => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            error.to_string(),
        ),
    }
}

pub(crate) fn pane_gone(pane: &str) -> ApiError {
    let mut error = ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("herdr has no pane {pane}; the agent's pane closed"),
    );
    error.body.error.entity = Some("pane".to_owned());
    error.body.error.id = Some(pane.to_owned());
    error
}

/// The terminal mirror: the agent's live pane as NDJSON (`protocol.md`
/// 7.4). A POST, so it passes every mutation check, owner cookie included,
/// though it writes nothing.
#[utoipa::path(post, path = "/api/v1/agents/{session}/terminal", tag = "mutations", request_body = TerminalOpen,
    params(("session" = String, Path)),
    responses((status = 200, content_type = "application/x-ndjson", body = TerminalLine,
               description = "One `TerminalLine` per line: `size`, then `frame`s, until `closed` or `unavailable` or the end of the body; `unfit` when herdr would not resize the pane for the Fit"),
              (status = 400, body = ErrorBody, description = "a `fit` outside 20 to 500 cols by 5 to 200 rows"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody, description = "no such agent, or its pane closed"),
              (status = 422, body = ErrorBody, description = "`refused`: the agent runs on a saved machine"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable, or too many terminal views are open")))]
async fn terminal(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    guard(&state, &headers).await?;
    let request: TerminalOpen = body(&bytes)?;
    let fit = request
        .fit
        .map(|fit| mirror::check_fit(fit.cols, fit.rows))
        .transpose()
        .map_err(ApiError::usage)?;
    let viewer = || {
        state.0.mirrors.viewer().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                format!("{MAX_VIEWERS} terminal views are open; close one first"),
            )
        })
    };
    let for_peer = addressed(&state, request.host.as_deref()).await?.is_peer();
    if for_peer && fit.is_some() {
        return Err(ApiError::peer_refused(
            "not_allowlisted",
            "the terminal Fit (a peer pane is watch only)",
        ));
    }
    let held = if for_peer { Some(viewer()?) } else { None };
    if let Some(opened) = peer_pane(
        &state,
        request.host.as_deref(),
        &session,
        PaneKind::Stream,
        None,
    )
    .await?
    {
        let Opened::Stream(label, stream) = opened else {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "internal",
                "the peer answered a mirror request without a stream",
            ));
        };
        let Some(held) = held else {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "internal",
                "a peer's pane was opened without a view",
            ));
        };
        let relayed = stream.relay(label, stop_or_signed_out(&state, &headers), held);
        return Ok((
            [
                (header::CONTENT_TYPE, "application/x-ndjson"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            relayed,
        )
            .into_response());
    }
    let (_, pane) = local_pane(&state, session).await?;
    let viewer = viewer()?;
    let herdr = state.0.herdr.clone();
    let asked = pane.clone();
    let terminal = tokio::task::spawn_blocking(move || herdr.terminal(&asked))
        .await
        .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?;
    let terminal = match terminal {
        Ok(Some(terminal)) => terminal,
        Ok(None) => return Err(pane_gone(&pane)),
        Err(error) => return Err(pane_error(&pane, &error)),
    };
    let body = mirror::stream(
        state.0.herdr.clone(),
        pane,
        terminal,
        fit,
        state.0.mirrors.fits(),
        stop_or_signed_out(&state, &headers),
        viewer,
    );
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response())
}

/// Keys or a paste for the agent's pane, through herdr (`protocol.md` 7.4).
#[utoipa::path(post, path = "/api/v1/agents/{session}/terminal/input", tag = "mutations", request_body = TerminalInput,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse), (status = 400, body = ErrorBody),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: the agent runs on a saved machine"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable, or input came faster than 20 a second; nothing was sent")))]
async fn terminal_input(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: TerminalInput = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "terminal input (a peer pane is watch only)",
    )
    .await?;
    let input = match (request.keys, request.text) {
        (Some(keys), None) => mirror::check_keys(&keys).map(|()| Ok(keys)),
        (None, Some(text)) => mirror::check_text(&text).map(|()| Err(text)),
        _ => Err("send exactly one of keys and text".to_owned()),
    }
    .map_err(ApiError::usage)?;
    let (session_id, pane) = local_pane(&state, session).await?;
    spend_input(&state)?;
    let herdr = state.0.herdr.clone();
    let target = pane.clone();
    let sent = tokio::task::spawn_blocking(move || match input {
        Ok(keys) => {
            let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
            herdr
                .send_keys(&target, &keys)
                .map(|()| json!({"keys": keys.len()}))
        }
        Err(text) => herdr
            .send_text(&target, &text)
            .map(|()| json!({"text": text.len()})),
    })
    .await
    .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
    .map_err(|error| pane_error(&pane, &error))?;
    Ok(Json(MutationResponse {
        ok: true,
        replayed: false,
        events: Vec::new(),
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: None,
            approval_id: None,
        },
        data: json!({"session_id": session_id, "sent": sent}),
        host: None,
        warning: None,
    }))
}

/// One spend of the mirror's input rate, which keys, pastes and prompt
/// answers share.
fn spend_input(state: &AppState) -> Result<(), ApiError> {
    if state.0.mirrors.spend() {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "unavailable",
        "input came faster than 20 a second; nothing was sent",
    )
    .hint("send it again in a moment"))
}

/// What an agent waits on: a permission prompt, with the tool its hook
/// named, or an answer; None when the agent is not waiting. A permission
/// wait no observation named a tool for is the empty name, which no
/// dialog's title starts with.
async fn waiting_on(
    state: &AppState,
    session_id: String,
) -> Result<Option<prompt::Wait>, ApiError> {
    read(state, move |store| {
        let session: Session = get_row(store.conn(), "session", &session_id)?;
        if session.ended_at.is_some() {
            return Ok(None);
        }
        Ok(match session.activity {
            Activity::WaitingPermission => Some(prompt::Wait::Permission(
                views::prompt_tool(store, &session.id)?.unwrap_or_default(),
            )),
            Activity::WaitingInput => Some(prompt::Wait::Input),
            _ => None,
        })
    })
    .await
}

/// The pane's visible screen, read once, and the dialog on it that `wait`
/// can be waiting on.
async fn read_dialog(
    state: &AppState,
    pane: &str,
    wait: prompt::Wait,
) -> Result<Option<prompt::Dialog>, ApiError> {
    let herdr = state.0.herdr.clone();
    let target = pane.to_owned();
    let screen = tokio::task::spawn_blocking(move || herdr.read_visible(&target))
        .await
        .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
        .map_err(|error| pane_error(pane, &error))?;
    Ok(prompt::read(&screen, &wait))
}

/// The dialog on a waiting agent's screen, for the board's answer buttons
/// (`protocol.md` 7.4): a permission dialog, or a cursor menu of a known
/// layout. The screen is read only while the agent's session says it waits.
/// A POST with every mutation check, as the mirror is, though it writes
/// nothing.
#[utoipa::path(post, path = "/api/v1/agents/{session}/prompt", tag = "mutations", request_body = PromptAsk,
    params(("session" = String, Path)),
    responses((status = 200, body = PromptRead),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody, description = "no such agent, or its pane closed"),
              (status = 422, body = ErrorBody, description = "`refused`: the agent runs on a saved machine"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable, or reads and input came faster than 20 a second")))]
async fn prompt_read(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<PromptRead> {
    guard(&state, &headers).await?;
    let request: PromptAsk = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a prompt read (a peer pane is watch only)",
    )
    .await?;
    let (session_id, pane) = local_pane(&state, session).await?;
    let (prompt, reason) = match waiting_on(&state, session_id.clone()).await? {
        None => (None, Some(NoPrompt::NotWaiting)),
        Some(wait) => {
            spend_input(&state)?;
            match read_dialog(&state, &pane, wait).await? {
                Some(dialog) => (Some(dialog.prompt), None),
                None => (None, Some(NoPrompt::NoDialog)),
            }
        }
    };
    Ok(Json(PromptRead {
        session_id,
        read_at: Timestamp::now().to_string(),
        prompt,
        reason,
    }))
}

/// One option of the dialog on a waiting agent's screen (`protocol.md`
/// 7.4): a permission dialog's digit alone, or a cursor menu's arrow keys
/// to the option's row, then Enter. The screen is read again first, and
/// nothing is typed unless its dialog is the one `screen` names.
#[utoipa::path(post, path = "/api/v1/agents/{session}/prompt/answer", tag = "mutations", request_body = PromptAnswer,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 400, body = ErrorBody, description = "an option the dialog does not offer"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody, description = "no such agent, or its pane closed"),
              (status = 409, body = ErrorBody, description = "`screen_changed`: the agent no longer waits, or the dialog on its screen is not the one `screen` names; nothing was sent"),
              (status = 422, body = ErrorBody, description = "`refused`: the agent runs on a saved machine"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable, or input came faster than 20 a second; nothing was sent")))]
async fn prompt_answer(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PromptAnswer = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a prompt answer (a peer pane is watch only)",
    )
    .await?;
    let (session_id, pane) = local_pane(&state, session).await?;
    spend_input(&state)?;
    let changed = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "screen_changed",
            "the prompt changed after the buttons were drawn; nothing was sent",
        )
        .hint("read the prompt again")
    };
    let wait = waiting_on(&state, session_id.clone())
        .await?
        .ok_or_else(changed)?;
    let dialog = read_dialog(&state, &pane, wait)
        .await?
        .filter(|dialog| dialog.prompt.screen == request.screen)
        .ok_or_else(changed)?;
    let option = request.option;
    let keys = dialog
        .keys(option)
        .ok_or_else(|| ApiError::usage(format!("the prompt has no option {option}")))?;
    let herdr = state.0.herdr.clone();
    let target = pane.clone();
    tokio::task::spawn_blocking(move || {
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        herdr.send_keys(&target, &keys)
    })
    .await
    .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
    .map_err(|error| pane_error(&pane, &error))?;
    Ok(Json(MutationResponse {
        ok: true,
        replayed: false,
        events: Vec::new(),
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: None,
            approval_id: None,
        },
        data: json!({"session_id": session_id, "sent": {"option": option}}),
        host: None,
        warning: None,
    }))
}

/// A new board size for the Fit an open stream holds on the agent's pane,
/// sent to herdr's control session in place (`protocol.md` 7.4).
#[utoipa::path(post, path = "/api/v1/agents/{session}/terminal/fit", tag = "mutations", request_body = TerminalRefit,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse),
              (status = 400, body = ErrorBody, description = "a size outside 20 to 500 cols by 5 to 200 rows"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: no open stream holds a Fit on the pane, or the agent runs on a saved machine"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn terminal_fit(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: TerminalRefit = body(&bytes)?;
    let (cols, rows) = mirror::check_fit(request.cols, request.rows).map_err(ApiError::usage)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "the terminal Fit (a peer pane is watch only)",
    )
    .await?;
    let (session_id, pane) = local_pane(&state, session).await?;
    if !state.0.mirrors.fits().refit(&pane, (cols, rows)) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "refused",
            format!("no open terminal stream holds a Fit on {pane}"),
        )
        .hint("open the terminal stream with a fit"));
    }
    Ok(Json(MutationResponse {
        ok: true,
        replayed: false,
        events: Vec::new(),
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: None,
            approval_id: None,
        },
        data: json!({"session_id": session_id, "fit": {"cols": cols, "rows": rows}}),
        host: None,
        warning: None,
    }))
}

/// The pane's scrollback, for the board to read back without herdr
/// (`protocol.md` 7.4). A POST, as the mirror is: it passes every mutation
/// check, owner cookie included, though it writes nothing.
#[utoipa::path(post, path = "/api/v1/agents/{session}/terminal/history", tag = "mutations", request_body = TerminalHistoryAsk,
    params(("session" = String, Path)),
    responses((status = 200, body = TerminalHistory), (status = 400, body = ErrorBody, description = "`lines` outside 1 to 1000"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody, description = "no such agent, or its pane closed"),
              (status = 422, body = ErrorBody, description = "`refused`: the agent runs on a saved machine"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable")))]
async fn terminal_history(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<TerminalHistory> {
    guard(&state, &headers).await?;
    let request: TerminalHistoryAsk = body(&bytes)?;
    let lines = request.lines.unwrap_or(mirror::MAX_HISTORY);
    if !(1..=mirror::MAX_HISTORY).contains(&lines) {
        return Err(ApiError::usage(format!(
            "lines is 1 to {}",
            mirror::MAX_HISTORY
        )));
    }
    if let Some(opened) = peer_pane(
        &state,
        request.host.as_deref(),
        &session,
        PaneKind::History,
        Some(lines),
    )
    .await?
    {
        let Opened::Answer(history) = opened else {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "internal",
                "the peer answered a history request with a stream",
            ));
        };
        let history: TerminalHistory = serde_json::from_value(history).map_err(|error| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "internal",
                format!("the peer answered a malformed history: {error}"),
            )
        })?;
        return Ok(Json(history));
    }
    let (session_id, pane) = local_pane(&state, session).await?;
    let herdr = state.0.herdr.clone();
    let target = pane.clone();
    let text = tokio::task::spawn_blocking(move || herdr.read_recent(&target, lines))
        .await
        .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
        .map_err(|error| pane_error(&pane, &error))?;
    Ok(Json(TerminalHistory { session_id, text }))
}

/// Tail reads in flight at once: each makes two herdr calls, to a saved
/// machine for a remote agent, so a busy board never stacks them up.
const MAX_TAIL_READS: usize = 4;
static TAIL_READS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(MAX_TAIL_READS);

/// The pane's visible screen and its size, read once, for an agent on a
/// saved machine: herdr's observe stream stays on the machine the pane runs
/// on, but its read goes there with `--machine` (`protocol.md` 7.4). A POST
/// with every mutation check, as the mirror is, though it writes nothing.
#[utoipa::path(post, path = "/api/v1/agents/{session}/terminal/tail", tag = "mutations", request_body = TerminalTailAsk,
    params(("session" = String, Path)),
    responses((status = 200, body = TerminalTail),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody, description = "no such agent, or its pane closed"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr, or the saved machine, is unreachable, or 4 tail reads are in flight")))]
async fn terminal_tail(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<TerminalTail> {
    guard(&state, &headers).await?;
    let request: TerminalTailAsk = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "the terminal tail (a peer pane is watch only)",
    )
    .await?;
    let (session_id, pane, machine) = pane_of(&state, session).await?;
    let read = TAIL_READS.try_acquire().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            format!("{MAX_TAIL_READS} tail reads are in flight; the next read tries again"),
        )
    })?;
    let herdr = match machine {
        Some(machine) => state.0.herdr.clone().on_machine(machine),
        None => state.0.herdr.clone(),
    };
    let target = pane.clone();
    // The read holds its place until herdr answers, even when the board
    // has gone.
    let tail = tokio::task::spawn_blocking(move || {
        let _read = read;
        let Some(size) = herdr.terminal_size(&target)? else {
            return Ok(None);
        };
        herdr.read_visible(&target).map(|text| Some((size, text)))
    })
    .await
    .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
    .map_err(|error| pane_error(&pane, &error))?;
    let Some(((cols, rows), text)) = tail else {
        return Err(pane_gone(&pane));
    };
    Ok(Json(TerminalTail {
        session_id,
        cols,
        rows,
        text,
    }))
}

/// One line for an agent, typed into its pane when the pane is idle: the
/// outbox path of `tiller send --kind steer` (`protocol.md` 3.3, 7.4).
#[utoipa::path(post, path = "/api/v1/agents/{session}/steer", tag = "mutations", request_body = SteerBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "The queued outbox message; `receipt.outbox_id` names it"),
              (status = 400, body = ErrorBody, description = "an empty line, a line break, or more than 20,000 characters"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: the session has ended"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn steer_agent(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: SteerBody = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a steer (a peer pane is watch only)",
    )
    .await?;
    let text = request.text;
    if text.trim().is_empty() {
        return Err(ApiError::usage("the message is empty"));
    }
    if text.contains(['\n', '\r']) {
        return Err(ApiError::usage(
            "a steer is one line; send each line on its own",
        ));
    }
    within("the message", &text, STEER_MAX_CHARS)?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, key| {
            let id = resolve_id(store.conn(), "sessions", "session", &session)?;
            let applied = store.outbox_send(ctx, Some(key), &id, OutboxKind::Steer, text)?;
            let outbox_id = Some(applied.value.id.clone());
            let mut done = Done::of(applied)?;
            done.outbox_id = outbox_id;
            Ok(done)
        },
    )
    .await
}

/// Escape to a working agent's pane, and whether its turn ended within
/// 10 s: `tiller agent interrupt` on the session's run (`protocol.md` 6.4,
/// 7.4). The request waits for the proof.
#[utoipa::path(post, path = "/api/v1/agents/{session}/interrupt", tag = "mutations", request_body = InterruptBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data.cancel` is `confirmed` or `unconfirmed`, with `data.evidence`"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: the agent is not working, has no run, pane or agent, or this key already sent its Escape; nothing was sent"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable")))]
async fn interrupt_agent(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: InterruptBody = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "an interrupt (a peer pane is watch only)",
    )
    .await?;
    // herdr's pane calls here reach this machine's panes only, and a pane
    // id on a saved machine may name another agent's pane here.
    if let (_, _, Some(machine)) = pane_of(&state, session.clone()).await? {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "refused",
            format!("the agent runs on the saved machine {machine}; an interrupt reaches panes on this machine only, so nothing was sent"),
        )
        .hint("open it in herdr instead"));
    }
    let herdr = state.0.herdr.clone();
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let id = resolve_id(store.conn(), "sessions", "session", &session)?;
            let session: Session = get_row(store.conn(), "session", &id)?;
            let run_id = session.run_id.ok_or_else(|| Error::Refused {
                entity: "session",
                id: id.clone(),
                fact: "it has no run to interrupt; send it a message instead".to_owned(),
            })?;
            let run: Run = get_row(store.conn(), "run", &run_id)?;
            let waits = tiller_agents::control::Waits::default();
            let done = tiller_agents::control::interrupt(store, ctx, &herdr, &waits, &run)
                .map_err(|error| interrupt_refused(&run.id, error))?;
            Done::of(done)
        },
    )
    .await
}

/// Runs one of the binary's agent controls as the board, off the async
/// runtime: the command's own path, through [`OrchestratorStart`].
async fn control(
    state: &AppState,
    key: String,
    action: impl FnOnce(&dyn OrchestratorStart, &mut Store, &Ctx) -> Result<Controlled, StartError>
    + Send
    + 'static,
) -> ApiResult<MutationResponse> {
    writable(state)?;
    let Some(starter) = state.0.orchestrators.clone() else {
        return Err(Error::Unavailable("this daemon cannot control agents".into()).into());
    };
    let db = state.db();
    let damage = state.0.damage.clone();
    let args = request_args();
    let joined = tokio::task::spawn_blocking(move || {
        if !db.exists() {
            return Err(uninitialized());
        }
        let mut store = Store::open_current(&db).map_err(ApiError::from)?;
        let ctx = board_ctx(&key, args.as_ref());
        let controlled = action(starter.as_ref(), &mut store, &ctx)
            .map_err(|error| start_refusal(error, |error| damage.suspect(&db, error)))?;
        let done = Done::of(controlled.applied).map_err(ApiError::from)?;
        Ok(done.with_warning(controlled.warning))
    })
    .await;
    let done = match joined {
        Ok(result) => result?,
        Err(error) => return Err(Error::Internal(error.to_string()).into()),
    };
    Ok(Json(done.response()))
}

/// The session `session` names, and its run, which an orchestrator lacks.
fn session_run(store: &Store, session: &str, what: &str) -> tiller_core::Result<(Session, Run)> {
    let id = resolve_id(store.conn(), "sessions", "session", session)?;
    let session: Session = get_row(store.conn(), "session", &id)?;
    let Some(run_id) = &session.run_id else {
        return Err(Error::Refused {
            entity: "session",
            id,
            fact: format!("it is an orchestrator, which has no run to {what}"),
        });
    };
    let run: Run = get_row(store.conn(), "run", run_id)?;
    Ok((session, run))
}

/// The proven stop of `tiller agent stop` (`protocol.md` 6.4, 7.4): a
/// worker whose run has ended, or an orchestrator. The request waits for
/// the proof.
#[utoipa::path(post, path = "/api/v1/agents/{session}/stop", tag = "mutations", request_body = AgentControlBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the stop: `session`, `stop_proof`, `cleaned`, `closed_pane`, `warning`"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: the run is active, the pane holds a draft or a dialog, or the agent runs on another host or a saved machine; nothing was sent"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable")))]
async fn stop_agent(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AgentControlBody = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a stop (a peer's agent is stopped on its own host)",
    )
    .await?;
    control(
        &state,
        key(&request.idempotency_key)?,
        move |starter, store, ctx| {
            let id = resolve_id(store.conn(), "sessions", "session", &session)?;
            let session: Session = get_row(store.conn(), "session", &id)?;
            starter.stop_agent(store, ctx, &session)
        },
    )
    .await
}

/// A dated pause on the session's run, which silences its alarms until
/// then: `tiller run pause` (`protocol.md` 6.4, 7.4).
#[utoipa::path(post, path = "/api/v1/agents/{session}/pause", tag = "mutations", request_body = PauseBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the run"),
              (status = 400, body = ErrorBody, description = "a date that does not parse, or an empty or too long reason"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: an orchestrator, an ended run, or a date not in the future"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn pause_agent(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PauseBody = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a pause (a peer's run is paused on its own host)",
    )
    .await?;
    let until: Timestamp = request.until.parse()?;
    if request.reason.trim().is_empty() {
        return Err(ApiError::usage("say why the run waits"));
    }
    within("the reason", &request.reason, COMMENT_MAX_CHARS)?;
    let reason = request.reason;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let (_, run) = session_run(store, &session, "pause")?;
            Done::of(store.run_pause(ctx, &run.id, until, Some(&reason))?)
        },
    )
    .await
}

/// Clears the dated pause on the session's run, as a manual resume does:
/// the board's Resume on a paused agent (`protocol.md` 7.4).
#[utoipa::path(post, path = "/api/v1/agents/{session}/unpause", tag = "mutations", request_body = AgentControlBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the run"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: an orchestrator or an ended run"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
async fn unpause_agent(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AgentControlBody = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a resume (a peer's run is resumed on its own host)",
    )
    .await?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| {
            let (_, run) = session_run(store, &session, "resume")?;
            Done::of(store.run_unpause(ctx, &run.id)?)
        },
    )
    .await
}

/// `claude --resume` in the run's worktree for a session whose agent is
/// gone: `tiller agent resume` (`protocol.md` 6.4, 7.4).
#[utoipa::path(post, path = "/api/v1/agents/{session}/resume", tag = "mutations", request_body = AgentControlBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the resume: `run`, `session`, `pane_id`, `left_alone`, `started`, `evidence`"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: an orchestrator, an ended run, no Claude session to resume, or another host"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable")))]
async fn resume_agent(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: AgentControlBody = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a resume (a peer's agent is resumed on its own host)",
    )
    .await?;
    control(
        &state,
        key(&request.idempotency_key)?,
        move |starter, store, ctx| {
            let (_, run) = session_run(store, &session, "resume")?;
            starter.resume_run(store, ctx, &run)
        },
    )
    .await
}

/// A new session on the run, in the same worktree, after a proven stop of
/// the live one: `tiller agent relaunch` (`protocol.md` 6.4, 7.4).
#[utoipa::path(post, path = "/api/v1/agents/{session}/relaunch", tag = "mutations", request_body = RelaunchBody,
    params(("session" = String, Path)),
    responses((status = 200, body = MutationResponse, description = "`data` is the relaunch: `run`, `session`, `ended_session`, `stop_proof`, `pane_id`, `started`, `evidence`"),
              (status = 400, body = ErrorBody, description = "an empty or too long note"),
              (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 422, body = ErrorBody, description = "`refused`: an orchestrator, an ended run, a worktree on another branch, a brief that is missing, or a stop that could not be proven; nothing was changed"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "herdr is unreachable")))]
async fn relaunch_agent(
    State(state): State<AppState>,
    Path(session): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: RelaunchBody = body(&bytes)?;
    not_forwarded(
        &state,
        request.host.as_deref(),
        "a relaunch (a peer's agent is relaunched on its own host)",
    )
    .await?;
    if request.note.trim().is_empty() {
        return Err(ApiError::usage(
            "write the note the new session starts from",
        ));
    }
    within("the note", &request.note, COMMENT_MAX_CHARS)?;
    let relaunch = BoardRelaunch {
        note: request.note,
        new_attempt: request.new_attempt,
    };
    control(
        &state,
        key(&request.idempotency_key)?,
        move |starter, store, ctx| {
            let (_, run) = session_run(store, &session, "relaunch")?;
            starter.relaunch_run(store, ctx, &run, relaunch)
        },
    )
    .await
}

/// An interrupt's failure as the store's errors: herdr out of reach is 503,
/// and any other precondition a refusal naming the run.
fn interrupt_refused(run: &str, error: tiller_agents::spawn::SpawnError) -> Error {
    use tiller_agents::spawn::SpawnError;
    match error {
        SpawnError::State(error) => error,
        SpawnError::Herdr(error) => Error::Unavailable(format!("herdr: {error}; nothing was sent")),
        other => Error::Refused {
            entity: "run",
            id: run.to_owned(),
            fact: other.to_string(),
        },
    }
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
    guard(&state, &headers).await?;
    let request: FocusBody = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a focus").await?;
    let (session_id, pane, machine) = pane_of(&state, session).await?;
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
        warning: None,
    }))
}

// -- login and the app shell --------------------------------------------------------

/// Where the signed-out screen sends a code.
const LOGIN_PATH: &str = "/api/v1/login";

/// Whether this browser holds a live owner cookie, for the board's Sign out.
#[utoipa::path(get, path = "/api/v1/session", tag = "session",
    responses((status = 200, body = SessionView), (status = 403, body = ErrorBody, description = "`signed_out`: at an --allow-host name, without the owner cookie")))]
async fn session(State(state): State<AppState>, headers: HeaderMap) -> Json<SessionView> {
    let owner = owner_cookie(&state, &headers).await.is_some();
    Json(SessionView { owner })
}

/// Signs in with the 8-digit code `tiller board --login` prints, as the
/// link does: sets the owner cookie. It passes the mutation checks but the
/// cookie. The limits count per client (`auth::Client`): a client's wrong
/// code counts against every pending code, each of which burns for that
/// client on its third, and each client has only so many tries a minute;
/// a pending code burns for everyone only at `auth::CODE_MISS_CEILING`.
/// The code is never logged.
#[utoipa::path(post, path = "/api/v1/login", tag = "session", request_body = CodeLoginBody,
    responses((status = 200, body = SessionView, description = "signed in: the response sets the owner cookie"),
              (status = 400, body = ErrorBody, description = "not 8 digits"),
              (status = 403, body = ErrorBody, description = "`wrong_code`: wrong, used, expired or burned for this client; `code_burned`: too many wrong codes, so the pending code no longer works for this client; or a failed origin check"),
              (status = 429, body = ErrorBody, description = "`too_many_tries`: this client, or its IPv6 network, had its tries for this minute; nothing was checked")))]
async fn login_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    same_origin(&state, &headers)?;
    // Parsed here, not with `body`: a serde error would quote the code.
    let code = serde_json::from_slice::<CodeLoginBody>(&bytes)
        .ok()
        .map(|body| body.code)
        .filter(|code| code.len() == 8 && code.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| ApiError::usage("a sign-in code is 8 digits"))?;
    let client = code_client(&headers);
    let shared = state.clone();
    let redeemed = tokio::task::spawn_blocking(move || {
        let state = &shared.0;
        let (now, at) = (Timestamp::now(), std::time::Instant::now());
        state
            .sessions
            .redeem_code(&state.home, client, &code, now, at)
    })
    .await
    .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
    .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?;
    let hint = "run tiller board --login again for a new code";
    match redeemed {
        auth::CodeLogin::Owner(cookie) => {
            let secure = at_allowed_host(&state, &headers);
            Ok((
                [(header::SET_COOKIE, auth::set_cookie(&cookie, secure))],
                Json(SessionView { owner: true }),
            )
                .into_response())
        }
        auth::CodeLogin::Wrong => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wrong_code",
            "that code is wrong or has expired",
        )
        .hint(hint)),
        auth::CodeLogin::Burned => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "code_burned",
            "too many wrong codes: the current code no longer works",
        )
        .hint(hint)),
        auth::CodeLogin::TooManyTries => Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_tries",
            "too many codes were tried from this address or its network in the last minute; wait a minute and try again",
        )),
    }
}

/// Who sent a sign-in code, for its limits. The daemon listens on loopback
/// only, so a browser elsewhere reaches it through the `--allow-host`
/// proxy, which names the browser's address as the last `X-Forwarded-For`
/// entry; entries before it are the browser's own word, read past even when
/// they are not text. With no header, the try is loopback's, a client of
/// its own; with a last entry that is no address, it is
/// [`auth::Client::UNKNOWN`]'s.
///
/// Only the limits read this, never access. Behind a proxy that does not
/// set the header, a guesser can name any address: that buys it more tries,
/// never past [`auth::CODE_MISS_CEILING`] for every client together, and
/// lets it spend the tries of an address it names.
fn code_client(headers: &HeaderMap) -> auth::Client {
    let Some(value) = headers.get_all("x-forwarded-for").iter().next_back() else {
        return auth::Client::new(std::net::Ipv4Addr::LOCALHOST.into());
    };
    let last = value.as_bytes().rsplit(|byte| *byte == b',').next();
    std::str::from_utf8(last.unwrap_or_default())
        .ok()
        .map(str::trim)
        .and_then(|entry| {
            entry.parse::<std::net::IpAddr>().ok().or_else(|| {
                entry
                    .parse::<std::net::SocketAddr>()
                    .ok()
                    .map(|addr| addr.ip())
            })
        })
        .map_or(auth::Client::UNKNOWN, auth::Client::new)
}

/// Signs this browser out: its owner cookie stops working here and now, on
/// every host name, and the browser is told to drop it. Other browsers stay
/// signed in.
#[utoipa::path(post, path = "/api/v1/logout", tag = "session",
    responses((status = 200, body = SessionView, description = "signed out: the response clears the owner cookie"),
              (status = 403, body = ErrorBody)))]
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    same_origin(&state, &headers)?;
    let cookie = owner_cookie(&state, &headers).await.ok_or_else(not_owner)?;
    let home = state.0.home.clone();
    tokio::task::spawn_blocking(move || auth::sign_out(&home, &cookie, Timestamp::now()))
        .await
        .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?
        .map_err(|error| ApiError::from(Error::Internal(error.to_string())))?;
    state.0.sessions.signed_out();
    let secure = at_allowed_host(&state, &headers);
    Ok((
        [(header::SET_COOKIE, auth::clear_cookie(secure))],
        Json(SessionView { owner: false }),
    )
        .into_response())
}

/// The one-time login link: sets the owner cookie and goes to the board.
/// At an allowed host the cookie is `Secure`: that host is reached over https
/// through a proxy (its mutations come only from its `https://` origin), so
/// the daemon need not trust a forwarded header to know it.
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    let secure = at_allowed_host(&state, &headers);
    let home = state.0.home.clone();
    let redeemed =
        tokio::task::spawn_blocking(move || auth::redeem_login(&home, &token, Timestamp::now()))
            .await;
    match redeemed {
        Ok(Ok(Some(cookie))) => (
            [(header::SET_COOKIE, auth::set_cookie(&cookie, secure))],
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
    // Signed out at an allowed host: the shell alone, which asks for the
    // snapshot, is refused, and shows the signed-out screen.
    if request.extensions().get::<SignedOut>().is_some() {
        return (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            index,
        )
            .into_response();
    }
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
    info(title = "Tiller board API", version = "1", description = "The daemon's board API (docs/spec/protocol.md section 7). Every path answers only on 127.0.0.1, localhost or an --allow-host name; at an --allow-host name every path but the signed-out screen and signing in needs the owner cookie; mutations need the owner cookie and the board's own origin."),
    paths(
        snapshot, board, card, timeline, attention, agents, run, machines, quota, health, doctor, run_poller,
        projects, contributions, signals, events, create_card, edit_card, comment, hold, release_hold, add_dep, remove_dep, close,
        cancel, reopen, answer, withdraw_attention, decide, focus, terminal, terminal_input, terminal_fit, terminal_history, terminal_tail, prompt_read, prompt_answer, steer_agent, interrupt_agent, stop_agent, pause_agent, unpause_agent, resume_agent, relaunch_agent, stage_project, ask_note, start_orchestrator, restart_stale, add_project, project_checkout, add_peer, project_delete_preview, delete_project, project_retire_preview, retire_project, project_setup, project_fields, edit_project, start_settings, host_config_read, set_config,
        project_grants, create_grant, revoke_grant,
        away_start, away_edit, away_end, away_brief, host_fields, edit_host, crate::backups::list, crate::backups::take, pick_bug_project, recap, mark_recap_seen, project_replies, mark_reply_read, relabel_intervention, memory::view, memory::revision,
        memory::edit, memory::decide, memory::undo, memory::repropose, memory::eval::eval_plan,
        memory::eval::eval_start, crate::retros::list_retros, crate::retros::run_retro,
        crate::pairings::list, crate::pairings::unpair, crate::pairings::revoke, session, login_code, logout
    ),
    components(schemas(ChangeEvent, ResetEvent, PeerEvent, LinkEvent, PeerResetEvent, TerminalLine, ErrorBody, ErrorDetail)),
    tags(
        (name = "reads", description = "Reads: any request that passes the Host check on loopback; at an --allow-host name, only with the owner cookie"),
        (name = "mutations", description = "Mutations: owner cookie, JSON body, same origin"),
        (name = "stream", description = "The one SSE stream"),
        (name = "session", description = "Signing in with a code, signing out, and whether this browser is signed in"),
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
    use tiller_herdr::testing::{Fleet, at};
    use tower::ServiceExt;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/dist"]
    pub(crate) struct Fixture;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/empty"]
    #[allow_missing = true]
    struct Missing;

    const PORT: u16 = 7421;
    pub(crate) const HOST: &str = "127.0.0.1:7421";

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
        app_ssh(
            home,
            assets,
            damage,
            peers,
            herdr,
            terminal,
            vec!["ssh".into()],
        )
    }

    /// The board API whose Add a host runs `ssh`.
    fn app_ssh(
        home: &std::path::Path,
        assets: Assets,
        damage: Damage,
        peers: Arc<Peers>,
        herdr: Cli,
        terminal: Option<Arc<dyn Terminal>>,
        ssh: Vec<OsString>,
    ) -> Router {
        app_evals(
            home,
            assets,
            damage,
            peers,
            herdr,
            terminal,
            ssh,
            memory::eval::Evals::default(),
        )
    }

    /// The board API with `evals` as its holdout evals, so a test can stop
    /// them the way the daemon does when it stops.
    pub(crate) fn app_with_evals(home: &std::path::Path, evals: memory::eval::Evals) -> Router {
        app_evals(
            home,
            Assets::of::<Fixture>(),
            Damage::default(),
            Peers::idle(),
            Cli::local(),
            None,
            vec!["ssh".into()],
            evals,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn app_evals(
        home: &std::path::Path,
        assets: Assets,
        damage: Damage,
        peers: Arc<Peers>,
        herdr: Cli,
        terminal: Option<Arc<dyn Terminal>>,
        ssh: Vec<OsString>,
        evals: memory::eval::Evals,
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
            orchestrators: None,
            mirrors: Mirrors::new(home),
            sessions: auth::Sessions::default(),
            streams: sse::Streams::default(),
            ssh,
            evals,
            backups: crate::backups::BackupJobs::default(),
            retros: crate::retros::Retros::default(),
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

    /// A board key replays only for the request it was first used with:
    /// the same method, path and body (in any key order) replay, another
    /// body under that key is refused and writes nothing.
    #[tokio::test]
    async fn a_board_key_replays_only_its_own_request() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let cookie = owner_cookie(home);
        let headers = [("cookie", cookie.as_str())];
        let first = send(&app, create(&headers)).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        let made = cards(&fleet);

        // A true retry, its fields in another order, replays.
        let mut retry = mutation(
            "POST",
            "/api/v1/cards",
            json!({"title": "Undo strokes", "kind": "ship", "project": "alpha", "idempotency_key": "k-1"}),
            &headers,
        );
        *retry.body_mut() = Body::from(
            r#"{ "title": "Undo strokes", "kind": "ship", "project": "alpha", "idempotency_key": "k-1" }"#,
        );
        let again = send(&app, retry).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        let body: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");

        // The same key with another title is refused.
        let other = send(
            &app,
            mutation(
                "POST",
                "/api/v1/cards",
                json!({"idempotency_key": "k-1", "project": "alpha", "kind": "ship", "title": "Redo strokes"}),
                &headers,
            ),
        )
        .await;
        assert_eq!(
            other.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            other.body
        );
        assert!(other.body.contains("other arguments"), "{}", other.body);
        assert_eq!(cards(&fleet), made);

        // A transition's `rev` is a precondition, not an argument: a retry
        // that read the newer rev its own write made still replays.
        let made_body: Value = serde_json::from_str(&first.body).unwrap();
        let card = &made_body["data"]["card"];
        let hold = format!("/api/v1/cards/{}/hold", card["id"].as_str().unwrap());
        let rev = card["rev"].as_i64().unwrap();
        let held = |rev: i64| {
            mutation(
                "POST",
                &hold,
                json!({"idempotency_key": "k-hold", "until": "2031-01-01T00:00:00Z",
                       "reason": "waiting on the vendor", "rev": rev}),
                &headers,
            )
        };
        let once = send(&app, held(rev)).await;
        assert_eq!(once.status, StatusCode::OK, "{}", once.body);
        let twice = send(&app, held(rev + 1)).await;
        assert_eq!(twice.status, StatusCode::OK, "{}", twice.body);
        let body: Value = serde_json::from_str(&twice.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");
    }

    /// A card's title and body, a comment and an answer each have a length
    /// cap: one character past it is a clear 400 that writes nothing, and
    /// text at the cap is taken.
    #[tokio::test]
    async fn long_text_is_refused_at_its_cap() {
        use tiller_core::limits::{ANSWER_MAX_CHARS, BODY_MAX_CHARS, TITLE_MAX_CHARS};

        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let cookie = owner_cookie(home);
        let headers = [("cookie", cookie.as_str())];
        let long = |n: usize| "é".repeat(n);
        let before = cards(&fleet);
        let card = |key: &str, title: String, body: Option<String>| {
            mutation(
                "POST",
                "/api/v1/cards",
                json!({"idempotency_key": key, "project": "alpha", "kind": "ship",
                       "title": title, "body": body}),
                &headers,
            )
        };
        let refused = [
            (
                card("k-t", long(TITLE_MAX_CHARS + 1), None),
                "title is at most 1000 characters",
            ),
            (
                card("k-b", "Undo".into(), Some(long(BODY_MAX_CHARS + 1))),
                "body is at most 100000 characters",
            ),
            (
                mutation(
                    "PATCH",
                    "/api/v1/cards/01M73NREJWV5CC5Q0WKCG3DV54",
                    json!({"idempotency_key": "k-e", "rev": 1, "title": long(TITLE_MAX_CHARS + 1)}),
                    &headers,
                ),
                "title is at most 1000 characters",
            ),
            (
                mutation(
                    "POST",
                    "/api/v1/cards/01M73NREJWV5CC5Q0WKCG3DV54/comment",
                    json!({"idempotency_key": "k-c", "text": long(COMMENT_MAX_CHARS + 1)}),
                    &headers,
                ),
                "a comment is at most 20000 characters",
            ),
            (
                mutation(
                    "POST",
                    "/api/v1/attention/01M73NREJWV5CC5Q0WKCG3DV55/answer",
                    json!({"idempotency_key": "k-a", "answer": long(ANSWER_MAX_CHARS + 1)}),
                    &headers,
                ),
                "an answer is at most 20000 characters",
            ),
        ];
        for (request, words) in refused {
            let answer = send(&app, request).await;
            assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
            assert!(answer.body.contains(words), "{}", answer.body);
        }
        assert_eq!(cards(&fleet), before);

        let taken = send(
            &app,
            card("k-ok", long(TITLE_MAX_CHARS), Some(long(BODY_MAX_CHARS))),
        )
        .await;
        assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
        assert_eq!(cards(&fleet), before + 1);
    }

    /// An empty body on an edit clears the body, as `tiller card edit --body
    /// ""` does: the store decides, so the two paths cannot differ.
    #[tokio::test]
    async fn an_empty_body_on_an_edit_clears_it() {
        let mut fleet = Fleet::new();
        let card = fleet
            .store
            .card_create(
                &Ctx::new(Actor::Owner),
                None,
                tiller_core::state::cards::NewCard {
                    project_id: "alpha".into(),
                    kind: tiller_core::domain::CardKind::Ship,
                    title: "Clear me".into(),
                    body: Some("Old notes".into()),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
            .value;
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let edited = send(
            &app,
            mutation(
                "PATCH",
                &format!("/api/v1/cards/{}", card.id),
                json!({"idempotency_key": "k-clear", "rev": card.rev, "body": ""}),
                &headers,
            ),
        )
        .await;
        assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
        let stored: Option<String> = fleet
            .store
            .conn()
            .query_row("SELECT body FROM cards WHERE id = ?1", [&card.id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored, None);
    }

    /// The owner check reads `board-owner` on the blocking pool: a slow
    /// read (here a FIFO no one writes yet) holds up that one request, never
    /// the runtime under every other.
    #[cfg(unix)]
    #[test]
    fn the_owner_check_never_blocks_the_runtime() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let fleet = Fleet::new();
        let home = fleet.dir.path().to_owned();
        let owner = home.join("board-owner");
        let _ = std::fs::remove_file(&owner);
        let made = std::process::Command::new("mkfifo")
            .arg(&owner)
            .status()
            .unwrap();
        assert!(made.success());
        // Opens and closes the FIFO's write end until the test is done, so
        // the reader sees an empty file and moves on.
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let (done, owner) = (done.clone(), owner.clone());
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(1));
                while !done.load(Ordering::SeqCst) {
                    let opened = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&owner);
                    drop(opened);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            })
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (waited, status) = runtime.block_on(async {
            let app = app(&home, Assets::of::<Fixture>());
            let request = Request::get("/api/v1/session")
                .header(header::HOST, "board.example.com")
                .header(header::COOKIE, "tiller_owner=not-a-live-cookie")
                .body(Body::empty())
                .unwrap();
            let pending = tokio::spawn(async move { send(&app, request).await.status });
            let started = std::time::Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let waited = started.elapsed();
            (waited, pending.await.unwrap())
        });
        done.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            waited < std::time::Duration::from_millis(500),
            "a 50 ms sleep took {waited:?} while the owner file was read"
        );
    }

    /// The owner cookie, minted and redeemed as the login link does.
    pub(crate) fn owner_cookie(home: &std::path::Path) -> String {
        let token = auth::mint_login(home, Timestamp::now()).unwrap().token;
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

    /// An untagged ask's row says where it came from and shows its first
    /// line, cut on a character boundary. The peer link carries the source
    /// but never the owner's words.
    #[tokio::test]
    async fn an_asked_row_carries_its_source_and_first_line() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let long = format!("{}\nthe rest", "é".repeat(250));
        for (key, text) in [
            ("a-1", "\n  Fade the card on exit  \nsecond line"),
            ("a-2", &long),
        ] {
            let sent = send(&app, ask_request(key, text, &owner)).await;
            assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        }

        let board = get(&app, "/api/v1/board?filter=all").await;
        assert_eq!(board.status, StatusCode::OK, "{}", board.body);
        let board: Value = serde_json::from_str(&board.body).unwrap();
        let asked = board["asked"].as_array().unwrap();
        assert_eq!(asked.len(), 2, "{board}");
        assert_eq!(asked[0]["source"], "board");
        assert_eq!(asked[0]["summary"], "Fade the card on exit");
        let cut = asked[1]["summary"].as_str().unwrap();
        assert_eq!(cut.chars().count(), 200);
        assert_eq!(cut, format!("{}…", "é".repeat(199)));

        // The boot snapshot carries the same fields as the GET.
        let snapshot = get(&app, "/api/v1/snapshot").await;
        let snapshot: Value = serde_json::from_str(&snapshot.body).unwrap();
        let booted = &snapshot["queries"]["/api/v1/board?filter=all"]["asked"];
        for (index, row) in asked.iter().enumerate() {
            assert_eq!(booted[index]["source"], row["source"], "{snapshot}");
            assert_eq!(booted[index]["summary"], row["summary"], "{snapshot}");
        }

        let peer = crate::peer::serve::own_snapshot(&fleet.open(), Timestamp::now()).unwrap();
        let carried = &peer.queries.board.asked;
        assert_eq!(carried.len(), 2);
        assert!(
            carried.iter().all(|row| row.summary.is_none()),
            "{carried:?}"
        );
        assert_eq!(carried[0].source.as_deref(), Some("board"));
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
        // No key file: the board's This host box says every project detects.
        assert_eq!(health["merge_app"]["status"], "missing");
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
    async fn an_allowed_host_takes_writes_only_from_its_https_origin() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        for host in ["board.example.com", "Board.Example.com"] {
            for path in ["/", "/api/v1/board", "/assets/app-abc123.js"] {
                let request = Request::get(path)
                    .header(header::HOST, host)
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap();
                let answer = send(&app, request).await;
                assert_eq!(answer.status, StatusCode::OK, "{host} {path}");
            }
        }
        // The allow-list names exact hosts: no suffix or other port matches,
        // not even for the login link.
        for (host, path) in [
            ("evil.board.example.com", "/"),
            ("board.example.com:7421", "/"),
            ("board.example.com.evil.example", "/login/abc"),
            ("", "/"),
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

        let at_host = |origin: &'static str| [("host", "board.example.com"), ("origin", origin)];
        // Anything but the board's own https origin is refused, cookie or not.
        let before = cards(&fleet);
        let refusals: [(&str, Vec<(&str, &str)>); 8] = [
            ("plain http", at_host("http://board.example.com").to_vec()),
            ("another site", at_host("https://evil.example.com").to_vec()),
            (
                "a longer name",
                at_host("https://board.example.com.evil.example").to_vec(),
            ),
            (
                "another port",
                at_host("https://board.example.com:8443").to_vec(),
            ),
            (
                "a null origin",
                vec![("host", "board.example.com"), ("origin", "null")],
            ),
            (
                "a cross-site fetch",
                vec![
                    ("host", "board.example.com"),
                    ("origin", "https://board.example.com"),
                    ("sec-fetch-site", "same-site"),
                ],
            ),
            // Loopback keeps its http origin.
            (
                "https on loopback",
                vec![("origin", "https://127.0.0.1:7421")],
            ),
            ("a forged cookie", vec![("cookie", "tiller_owner=forged")]),
        ];
        for (case, headers) in &refusals {
            let mut headers = headers.clone();
            if !headers.iter().any(|(name, _)| *name == "cookie") {
                headers.push(("cookie", &cookie));
            }
            let refused = send(&app, create(&headers)).await;
            assert_eq!(
                refused.status,
                StatusCode::FORBIDDEN,
                "{case}: {}",
                refused.body
            );
        }
        let unsigned = send(&app, create(&at_host("https://board.example.com"))).await;
        assert_eq!(unsigned.status, StatusCode::FORBIDDEN, "{}", unsigned.body);
        assert!(
            unsigned.body.contains("tiller board --login"),
            "{}",
            unsigned.body
        );
        // An unknown host never reaches the guard.
        let mut stranger = at_host("https://evil.example.com").to_vec();
        stranger[0].1 = "evil.example.com";
        stranger.push(("cookie", &cookie));
        let misdirected = send(&app, create(&stranger)).await;
        assert_eq!(misdirected.status, StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(cards(&fleet), before);

        // The owner's own https origin writes, with the same cookie check.
        let mut owner = at_host("https://board.example.com").to_vec();
        owner.push(("cookie", &cookie));
        let created = send(&app, create(&owner)).await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        assert_eq!(cards(&fleet), before + 1);
    }

    /// `GET /login/<token>` at `host`.
    async fn redeem(app: &Router, host: &str, token: &str) -> Answer {
        let request = Request::get(format!("/login/{token}"))
            .header(header::HOST, host)
            .body(Body::empty())
            .unwrap();
        send(app, request).await
    }

    #[tokio::test]
    async fn the_login_link_redeems_once_on_loopback_or_an_allowed_host() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let cookie_of = |answer: &Answer| {
            answer.headers[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .to_owned()
        };

        // At an allowed host, reached over https, the cookie is Secure.
        let token = auth::mint_login(home, Timestamp::now()).unwrap().token;
        let signed = redeem(&app, "board.example.com", &token).await;
        assert_eq!(signed.status, StatusCode::SEE_OTHER, "{}", signed.body);
        assert_eq!(signed.headers[header::LOCATION], "/");
        let set = cookie_of(&signed);
        assert!(set.ends_with("; Secure"), "{set}");
        let value = auth::cookie_value(set.split(';').next().unwrap()).unwrap();
        assert!(auth::is_owner(home, value, Timestamp::now()));
        // Once: not again there, nor on loopback.
        for host in ["board.example.com", HOST] {
            let again = redeem(&app, host, &token).await;
            assert_eq!(again.status, StatusCode::FORBIDDEN, "{host}");
            assert!(again.headers.get(header::SET_COOKIE).is_none(), "{host}");
        }

        // On loopback, plain http, so not Secure.
        let token = auth::mint_login(home, Timestamp::now()).unwrap().token;
        let signed = redeem(&app, HOST, &token).await;
        assert_eq!(signed.status, StatusCode::SEE_OTHER, "{}", signed.body);
        assert!(!cookie_of(&signed).contains("Secure"));

        // A link past its minute is refused everywhere.
        let stale = auth::mint_login(home, Timestamp::now().plus_minutes(-auth::LOGIN_MINUTES))
            .unwrap()
            .token;
        for host in ["board.example.com", HOST] {
            let late = redeem(&app, host, &stale).await;
            assert_eq!(late.status, StatusCode::FORBIDDEN, "{host}");
            assert!(late.headers.get(header::SET_COOKIE).is_none(), "{host}");
        }
    }

    /// GET `path` at `host`, with `cookie` when there is one.
    async fn get_at(app: &Router, host: &str, path: &str, cookie: Option<&str>) -> Answer {
        let mut request = Request::get(path).header(header::HOST, host);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        send(app, request.body(Body::empty()).unwrap()).await
    }

    fn error_code(answer: &Answer) -> String {
        let body: Value = serde_json::from_str(&answer.body).unwrap_or_default();
        body["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    #[tokio::test]
    async fn signed_out_at_an_allowed_host_gets_only_the_sign_in_screen() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let host = "board.example.com";
        for signed_out in [None, Some("tiller_owner=forged")] {
            // The shell, without the snapshot, and the files it loads.
            for path in ["/", "/cards/c-1?tab=runs", "/settings"] {
                let page = get_at(&app, host, path, signed_out).await;
                assert_eq!(page.status, StatusCode::OK, "{path}");
                assert!(!page.body.contains("tiller-boot"), "{path}");
            }
            let file = get_at(&app, host, "/assets/app-abc123.js", signed_out).await;
            assert_eq!(file.status, StatusCode::OK);
            // Every read, the event stream and the terminal included.
            for path in [
                "/api/v1/snapshot",
                "/api/v1/board",
                "/api/v1/cards/c-1",
                "/api/v1/agents",
                "/api/v1/health",
                "/api/v1/session",
                "/api/v1/events",
                "/api/v1/agents/s-1/terminal/stream",
                "/api/v1/nowhere",
            ] {
                let read = get_at(&app, host, path, signed_out).await;
                assert_eq!(read.status, StatusCode::FORBIDDEN, "{path}");
                assert_eq!(error_code(&read), "signed_out", "{path}");
                assert!(read.body.contains("tiller board --login"), "{path}");
            }
            let before = cards(&fleet);
            let mut headers = vec![("host", host), ("origin", "https://board.example.com")];
            headers.extend(signed_out.map(|cookie| ("cookie", cookie)));
            let write = send(&app, create(&headers)).await;
            assert_eq!(error_code(&write), "signed_out");
            assert_eq!(cards(&fleet), before);
        }
        // The owner cookie opens it all.
        let page = get_at(&app, host, "/", Some(&cookie)).await;
        assert!(page.body.contains("tiller-boot"));
        for path in ["/api/v1/board", "/api/v1/session"] {
            let read = get_at(&app, host, path, Some(&cookie)).await;
            assert_eq!(read.status, StatusCode::OK, "{path}");
        }
        // Loopback is as it was: reads and the snapshot need no cookie.
        let page = get_at(&app, HOST, "/", None).await;
        assert!(page.body.contains("tiller-boot"));
        let read = get_at(&app, HOST, "/api/v1/board", None).await;
        assert_eq!(read.status, StatusCode::OK);
        let session = get_at(&app, HOST, "/api/v1/session", None).await;
        assert_eq!(session.body, r#"{"owner":false}"#);
        let session = get_at(&app, HOST, "/api/v1/session", Some(&cookie)).await;
        assert_eq!(session.body, r#"{"owner":true}"#);
    }

    /// `POST /api/v1/login` as the board's signed-out screen sends it, at
    /// `host`'s own origin.
    async fn enter(app: &Router, host: &str, code: &str) -> Answer {
        let origin = if host == HOST {
            format!("http://{host}")
        } else {
            format!("https://{host}")
        };
        let headers = [("host", host), ("origin", origin.as_str())];
        send(
            app,
            mutation("POST", "/api/v1/login", json!({ "code": code }), &headers),
        )
        .await
    }

    /// [`enter`] at `host` through a proxy that names the browser's address
    /// `from` in `X-Forwarded-For`.
    async fn enter_from(app: &Router, host: &str, from: &str, code: &str) -> Answer {
        let origin = format!("https://{host}");
        let headers = [
            ("host", host),
            ("origin", origin.as_str()),
            ("x-forwarded-for", from),
        ];
        send(
            app,
            mutation("POST", "/api/v1/login", json!({ "code": code }), &headers),
        )
        .await
    }

    /// The board is public, so anyone can guess: a stranger's wrong codes,
    /// as many as it sends, never keep the owner's fresh code from signing in.
    #[tokio::test]
    async fn a_stranger_flooding_wrong_codes_never_blocks_a_fresh_code() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let host = "board.example.com";
        let (stranger, owner) = ("198.51.100.7", "192.0.2.10");

        // While the owner's code is pending, until the stranger is turned
        // away: the pending code still signs the owner in, and so does one
        // minted after.
        let pending = auth::mint_login(home, Timestamp::now()).unwrap();
        for _ in 0..auth::CODE_TRIES_PER_MINUTE {
            let miss = enter_from(&app, host, stranger, &wrong(&pending)).await;
            assert_eq!(miss.status, StatusCode::FORBIDDEN, "{}", miss.body);
        }
        let limited = enter_from(&app, host, stranger, &wrong(&pending)).await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error_code(&limited), "too_many_tries");
        let signed = enter_from(&app, host, owner, &pending.code).await;
        assert_eq!(signed.status, StatusCode::OK, "{}", signed.body);
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        let signed = enter_from(&app, host, owner, &login.code).await;
        assert_eq!(signed.status, StatusCode::OK, "{}", signed.body);

        // While the owner's code is pending: a guesser burns it only for
        // itself, even with the right code after.
        let guesser = "198.51.100.8";
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        for _ in 1..auth::CODE_MISSES {
            let miss = enter_from(&app, host, guesser, &wrong(&login)).await;
            assert_eq!(error_code(&miss), "wrong_code");
        }
        let burned = enter_from(&app, host, guesser, &wrong(&login)).await;
        assert_eq!(error_code(&burned), "code_burned");
        let right = enter_from(&app, host, guesser, &login.code).await;
        assert_eq!(error_code(&right), "wrong_code");
        assert!(right.headers.get(header::SET_COOKIE).is_none());
        let signed = enter_from(&app, host, owner, &login.code).await;
        assert_eq!(signed.status, StatusCode::OK, "{}", signed.body);
    }

    /// A code that is not `login`'s.
    fn wrong(login: &auth::Login) -> String {
        let code: u32 = login.code.parse().unwrap();
        format!("{:08}", (code + 1) % 100_000_000)
    }

    #[tokio::test]
    async fn the_code_signs_in_once_like_its_link() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let host = "board.example.com";

        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        let signed = enter(&app, host, &login.code).await;
        assert_eq!(signed.status, StatusCode::OK, "{}", signed.body);
        let set = signed.headers[header::SET_COOKIE].to_str().unwrap();
        assert!(set.ends_with("; Secure"), "{set}");
        let cookie = set.split(';').next().unwrap().to_owned();
        let board = get_at(&app, host, "/api/v1/board", Some(&cookie)).await;
        assert_eq!(board.status, StatusCode::OK);
        // Once: the code is spent, and so is its link.
        let again = enter(&app, host, &login.code).await;
        assert_eq!(error_code(&again), "wrong_code");
        assert!(again.headers.get(header::SET_COOKIE).is_none());
        let link = redeem(&app, host, &login.token).await;
        assert_eq!(link.status, StatusCode::FORBIDDEN);

        // On loopback too, where the cookie is not Secure.
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        let signed = enter(&app, HOST, &login.code).await;
        assert_eq!(signed.status, StatusCode::OK, "{}", signed.body);
        let set = signed.headers[header::SET_COOKIE].to_str().unwrap();
        assert!(!set.contains("Secure"), "{set}");

        // A code past its minute is refused.
        let stale =
            auth::mint_login(home, Timestamp::now().plus_minutes(-auth::LOGIN_MINUTES)).unwrap();
        let late = enter(&app, host, &stale.code).await;
        assert_eq!(error_code(&late), "wrong_code");
        assert!(late.body.contains("tiller board --login"));

        // Only from the board's own origin, and that spends no code.
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        let headers = [("host", host), ("origin", "https://evil.example.com")];
        let body = json!({ "code": login.code });
        let foreign = send(&app, mutation("POST", "/api/v1/login", body, &headers)).await;
        assert_eq!(foreign.status, StatusCode::FORBIDDEN);
        assert!(foreign.headers.get(header::SET_COOKIE).is_none());
        let signed = enter(&app, host, &login.code).await;
        assert_eq!(signed.status, StatusCode::OK, "{}", signed.body);

        // Not 8 digits: refused without echoing what was sent.
        for code in ["1234567", "123456789", "1234567x", "１２３４５６７８"] {
            let refused = enter(&app, host, code).await;
            assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{code}");
            assert!(!refused.body.contains(code), "{}", refused.body);
        }
        let headers = [("host", host), ("origin", "https://board.example.com")];
        let body = json!({ "code": "87654321", "extra": "87654321" });
        let refused = send(&app, mutation("POST", "/api/v1/login", body, &headers)).await;
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        assert!(!refused.body.contains("87654321"), "{}", refused.body);
    }

    #[tokio::test]
    async fn wrong_codes_burn_the_code_but_not_its_link() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let host = "board.example.com";
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        for _ in 1..auth::CODE_MISSES {
            let miss = enter(&app, host, &wrong(&login)).await;
            assert_eq!(error_code(&miss), "wrong_code");
        }
        let burned = enter(&app, host, &wrong(&login)).await;
        assert_eq!(burned.status, StatusCode::FORBIDDEN);
        assert_eq!(error_code(&burned), "code_burned");
        assert!(burned.body.contains("tiller board --login"));
        let right = enter(&app, host, &login.code).await;
        assert_eq!(error_code(&right), "wrong_code");
        assert!(right.headers.get(header::SET_COOKIE).is_none());
        // The link is the owner's own terminal, not a guess: it still works.
        let link = redeem(&app, host, &login.token).await;
        assert_eq!(link.status, StatusCode::SEE_OTHER);
    }

    #[tokio::test]
    async fn a_client_takes_only_so_many_codes_a_minute() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let host = "board.example.com";
        let guesser = "198.51.100.7";
        // Each try against its own code, so none burns: only the limit binds.
        for _ in 0..auth::CODE_TRIES_PER_MINUTE {
            let login = auth::mint_login(home, Timestamp::now()).unwrap();
            let miss = enter_from(&app, host, guesser, &wrong(&login)).await;
            assert_eq!(miss.status, StatusCode::FORBIDDEN, "{}", miss.body);
            auth::redeem_login(home, &login.token, Timestamp::now()).unwrap();
        }
        // Past it even the right code is turned away unchecked, and kept.
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        let limited = enter_from(&app, host, guesser, &login.code).await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error_code(&limited), "too_many_tries");
        assert!(limited.headers.get(header::SET_COOKIE).is_none());
        // The address is the proxy's last entry: what the guesser put
        // before it changes nothing.
        let forged = "192.0.2.10, 198.51.100.7";
        let limited = enter_from(&app, host, forged, &login.code).await;
        assert_eq!(error_code(&limited), "too_many_tries");
        // Another address, or loopback with no proxy, is a client of its own.
        let kept = enter_from(&app, host, "[2001:db8::7]:443", &login.code).await;
        assert_eq!(kept.status, StatusCode::OK, "{}", kept.body);
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        let local = enter(&app, HOST, &login.code).await;
        assert_eq!(local.status, StatusCode::OK, "{}", local.body);
    }

    #[test]
    fn the_code_client_is_the_proxys_last_forwarded_address() {
        let client = |lines: &[&[u8]]| {
            let mut headers = HeaderMap::new();
            for line in lines {
                let value = header::HeaderValue::from_bytes(line).unwrap();
                headers.append("x-forwarded-for", value);
            }
            code_client(&headers)
        };
        let address = |text: &str| auth::Client::new(text.parse().unwrap());
        assert_eq!(client(&[]), address("127.0.0.1"));
        assert_eq!(
            client(&[b"192.0.2.1, 198.51.100.7"]),
            address("198.51.100.7")
        );
        // Of several header lines, the proxy's is the last.
        let lines: &[&[u8]] = &[b"192.0.2.1", b"198.51.100.7"];
        assert_eq!(client(lines), address("198.51.100.7"));
        // What the browser sent before it, even bytes that are not text,
        // never hides the proxy's entry.
        let garbled: &[&[u8]] = &[b"\xff\xfe, 198.51.100.7"];
        assert_eq!(client(garbled), address("198.51.100.7"));
        assert_eq!(client(&[b"[2001:db8::7]:443"]), address("2001:db8::7"));
        // A last entry that is no address is no one's, loopback least.
        for unread in [&b"proxy"[..], b"198.51.100.7, \xff", b""] {
            assert_eq!(client(&[unread]), auth::Client::UNKNOWN);
        }
    }

    #[test]
    fn the_code_is_never_logged() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let login = auth::mint_login(home, Timestamp::now()).unwrap();
        let wrong = wrong(&login);
        let logs = logged(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                for code in [wrong.as_str(), "1234567x", login.code.as_str()] {
                    enter(&app, "board.example.com", code).await;
                }
            });
        });
        assert!(!logs.contains(&login.code), "{logs}");
        assert!(!logs.contains(&wrong), "{logs}");
    }

    /// What `f` logs at any level, as plain text.
    fn logged(f: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap()
    }

    /// `POST /api/v1/logout` at `host`'s own origin with `cookie`.
    async fn sign_out(app: &Router, host: &str, cookie: &str) -> Answer {
        let origin = if host == HOST {
            format!("http://{host}")
        } else {
            format!("https://{host}")
        };
        let headers = [
            ("host", host),
            ("origin", origin.as_str()),
            ("cookie", cookie),
        ];
        send(app, mutation("POST", "/api/v1/logout", json!({}), &headers)).await
    }

    #[tokio::test]
    async fn signing_out_ends_this_cookie_and_clears_it() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let host = "board.example.com";
        let mine = owner_cookie(home);
        let other = owner_cookie(home);

        // Only from the board's own origin, and only with a live cookie.
        let headers = [
            ("host", host),
            ("origin", "https://evil.example.com"),
            ("cookie", mine.as_str()),
        ];
        let foreign = send(
            &app,
            mutation("POST", "/api/v1/logout", json!({}), &headers),
        )
        .await;
        assert_eq!(foreign.status, StatusCode::FORBIDDEN);
        let forged = sign_out(&app, HOST, "tiller_owner=forged").await;
        assert_eq!(forged.status, StatusCode::FORBIDDEN);
        assert!(forged.headers.get(header::SET_COOKIE).is_none());
        let board = get_at(&app, host, "/api/v1/board", Some(&mine)).await;
        assert_eq!(board.status, StatusCode::OK);

        let out = sign_out(&app, host, &mine).await;
        assert_eq!(out.status, StatusCode::OK, "{}", out.body);
        assert_eq!(out.body, r#"{"owner":false}"#);
        assert_eq!(
            out.headers[header::SET_COOKIE],
            auth::clear_cookie(true).as_str()
        );
        // That cookie is dead everywhere; the other browser stays in.
        let board = get_at(&app, host, "/api/v1/board", Some(&mine)).await;
        assert_eq!(error_code(&board), "signed_out");
        let page = get_at(&app, host, "/", Some(&mine)).await;
        assert!(!page.body.contains("tiller-boot"));
        let write = send(&app, create(&[("cookie", &mine)])).await;
        assert_eq!(write.status, StatusCode::FORBIDDEN);
        let board = get_at(&app, host, "/api/v1/board", Some(&other)).await;
        assert_eq!(board.status, StatusCode::OK);

        // On loopback the cleared cookie is not Secure.
        let out = sign_out(&app, HOST, &other).await;
        assert_eq!(out.status, StatusCode::OK, "{}", out.body);
        assert_eq!(
            out.headers[header::SET_COOKIE],
            auth::clear_cookie(false).as_str()
        );
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

    /// The signals read names when the store's first event happened, so the
    /// Health page can tell a new install from a quiet fortnight: null on an
    /// empty store, then the first event's time, which later events keep.
    #[tokio::test]
    async fn signals_carry_the_time_of_the_first_event() {
        let fleet = Fleet::new();
        let store = fleet.open();
        store.conn().execute_batch("DELETE FROM events").unwrap();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let first_event_at = |answer: Answer| {
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
            let body: Value = serde_json::from_str(&answer.body).unwrap();
            body["first_event_at"].clone()
        };

        assert_eq!(
            first_event_at(get(&app, "/api/v1/signals").await),
            Value::Null
        );

        let headers = [("cookie", cookie.as_str())];
        let asked = send(&app, ask_request("k-1", "first", &headers)).await;
        assert_eq!(asked.status, StatusCode::OK, "{}", asked.body);
        let first: String = store
            .conn()
            .query_row("SELECT at FROM events ORDER BY seq LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            first_event_at(get(&app, "/api/v1/signals").await),
            json!(first)
        );

        let again = send(&app, ask_request("k-2", "second", &headers)).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        assert_eq!(
            first_event_at(get(&app, "/api/v1/signals").await),
            json!(first)
        );
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
        // A review names the head it covered (B11); a row from before
        // migration 16 has none.
        store
            .conn()
            .execute_batch(&format!(
                "INSERT INTO runs (id, card_id, role, attempt, model, effort, host_id, supervisor_session_id, started_at)
                 VALUES ('r-1', 'c-1', 'reviewer', 1, 'm', 'high', '{}', '{}', '2026-11-01T12:30:00Z');
                 INSERT INTO reviews (id, run_id, card_id, reviewer, coverage, verdict, recorded_at, head_sha)
                 VALUES ('v-1', 'r-1', 'c-1', 'code-review', 'diff', 'fail', '2026-11-01T12:40:00Z', NULL),
                        ('v-2', 'r-1', 'c-1', 'code-review', 'diff', 'pass', '2026-11-01T12:50:00Z',
                         '7c2d0e1f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d');",
                fleet.host, fleet.orchestrator_session
            ))
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
        assert_eq!(card["reviews"][0]["head_sha"], Value::Null, "{card}");
        assert_eq!(
            card["reviews"][1]["head_sha"], "7c2d0e1f9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d",
            "{card}"
        );
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

    /// The project detail's setup read: the stored merge gate, and the
    /// checkout `config.toml` names, read fresh on every request; a
    /// `config.toml` that does not parse says so instead of looking unset.
    #[tokio::test]
    async fn project_setup_reads_the_gate_and_the_checkout() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let setup = |body: &str| serde_json::from_str::<Value>(body).unwrap();

        let unset = get(&app, "/api/v1/projects/alpha/setup").await;
        assert_eq!(unset.status, StatusCode::OK, "{}", unset.body);
        let body = setup(&unset.body);
        assert_eq!(body["merge_gate"], "detect");
        assert_eq!(body["checkout"], Value::Null);
        assert_eq!(body["config_error"], Value::Null);

        let config = fleet.dir.path().join("config.toml");
        std::fs::write(
            &config,
            "[checkouts]\nalpha = \"/home/user/checkouts/alpha\"\n",
        )
        .unwrap();
        let body = setup(&get(&app, "/api/v1/projects/alpha/setup").await.body);
        assert_eq!(body["checkout"], "/home/user/checkouts/alpha");

        std::fs::write(&config, "[checkouts\n").unwrap();
        let body = setup(&get(&app, "/api/v1/projects/alpha/setup").await.body);
        assert_eq!(body["checkout"], Value::Null);
        assert!(
            body["config_error"]
                .as_str()
                .unwrap()
                .contains("does not parse"),
            "{body}"
        );

        let missing = get(&app, "/api/v1/projects/nope/setup").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);
    }

    /// Edit a project: the board reads the fields `tiller project edit`
    /// sets, then sends the changed ones against the `rev` it read. The
    /// edit is owner-only, never forwarded, validated as the CLI's is, and
    /// an empty text clears an optional field.
    #[tokio::test]
    async fn board_project_edit_changes_the_fields_project_edit_sets() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let cookie = owner_cookie(home);
        let edit = |key: &str, fields: Value, cookie: &str| {
            let mut body = json!({"idempotency_key": key});
            if let (Some(body), Some(fields)) = (body.as_object_mut(), fields.as_object()) {
                body.extend(fields.clone());
            }
            let headers: Vec<(&str, &str)> = if cookie.is_empty() {
                vec![]
            } else {
                vec![("cookie", cookie)]
            };
            mutation("PATCH", "/api/v1/projects/alpha", body, &headers)
        };
        let fields = |app: Router| async move {
            let answer = get(&app, "/api/v1/projects/alpha").await;
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
            serde_json::from_str::<Value>(&answer.body).unwrap()
        };

        let before = fields(app.clone()).await;
        assert_eq!(before["id"], "alpha");
        assert_eq!(before["repo"], "example/alpha");
        assert_eq!(before["default_branch"], "main");
        assert_eq!(before["delivery"], "direct-pr");
        assert_eq!(before["capacity"], 4);
        assert_eq!(before["checks"], json!([]));
        assert_eq!(before["deploy_timeout_minutes"], 60);
        let rev = before["rev"].as_i64().unwrap();
        let missing = get(&app, "/api/v1/projects/nope").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);

        let anonymous = send(
            &app,
            edit("k-edit-1", json!({"rev": rev, "capacity": 2}), ""),
        )
        .await;
        assert_eq!(
            anonymous.status,
            StatusCode::FORBIDDEN,
            "{}",
            anonymous.body
        );
        let peer = send(
            &app,
            edit(
                "k-edit-2",
                json!({"rev": rev, "capacity": 2, "host": "laptop"}),
                &cookie,
            ),
        )
        .await;
        assert_ne!(peer.status, StatusCode::OK, "{}", peer.body);
        for (key, bad) in [
            ("k-edit-3", json!({"delivery": "yolo"})),
            ("k-edit-4", json!({"capacity": 0})),
            ("k-edit-5", json!({"checks": {"full": "cargo test"}})),
            ("k-edit-6", json!({"name": " "})),
            ("k-edit-7", json!({"default_branch": ""})),
            ("k-edit-8", json!({"deploy_timeout_minutes": 0})),
        ] {
            let mut fields = bad.clone();
            fields["rev"] = json!(rev);
            let refused = send(&app, edit(key, fields, &cookie)).await;
            assert_eq!(
                refused.status,
                StatusCode::BAD_REQUEST,
                "{bad}: {}",
                refused.body
            );
        }
        assert_eq!(
            fields(app.clone()).await["rev"],
            rev,
            "a refused edit wrote"
        );

        let changed = json!({"rev": rev, "name": "Alpha", "remote": "https://example.com/alpha.git",
            "default_branch": "trunk", "delivery": "direct-push", "capacity": 2,
            "checks": {"full": ["cargo test"], "no_logic": []},
            "deploy_workflow": "deploy.yml", "deploy_environment": "production",
            "deploy_timeout_minutes": 20});
        let applied = send(&app, edit("k-edit-9", changed.clone(), &cookie)).await;
        assert_eq!(applied.status, StatusCode::OK, "{}", applied.body);
        let after = fields(app.clone()).await;
        for name in [
            "name",
            "remote",
            "default_branch",
            "delivery",
            "capacity",
            "checks",
            "deploy_workflow",
            "deploy_environment",
            "deploy_timeout_minutes",
        ] {
            assert_eq!(after[name], changed[name], "{name}: {after}");
        }
        assert_eq!(after["repo"], "example/alpha", "an unsent field moved");
        assert_eq!(after["rev"], rev + 1);
        let replay = send(&app, edit("k-edit-9", changed, &cookie)).await;
        let body: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");

        // The rev the board read is stale now: a conflict that names the
        // stored value of the field it tried to change, and when the other
        // edit happened.
        let listed = get(&app, "/api/v1/projects").await;
        let listed: Value = serde_json::from_str(&listed.body).unwrap();
        let edited_at = listed["projects"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == "alpha"))
            .map(|row| row["edited_at"].clone())
            .unwrap();
        assert!(edited_at.is_string(), "{listed}");
        let stale = send(
            &app,
            edit("k-edit-10", json!({"rev": rev, "capacity": 3}), &cookie),
        )
        .await;
        assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
        let body: Value = serde_json::from_str(&stale.body).unwrap();
        assert_eq!(
            body["error"]["current"],
            json!({"rev": rev + 1, "capacity": 2, "edited_at": edited_at})
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("; it changed ")),
            "{body}"
        );

        let cleared = send(
            &app,
            edit(
                "k-edit-11",
                json!({"rev": rev + 1, "remote": "", "deploy_workflow": "",
                    "deploy_environment": ""}),
                &cookie,
            ),
        )
        .await;
        assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
        let after = fields(app.clone()).await;
        assert_eq!(
            (
                &after["remote"],
                &after["deploy_workflow"],
                &after["deploy_environment"]
            ),
            (&Value::Null, &Value::Null, &Value::Null),
            "{after}"
        );
        let events: i64 = fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'project.edited' AND entity_id = 'alpha'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 2);
    }

    /// Grants per project: the board reads every grant with its use count,
    /// creates one and revokes one through the store paths `tiller grant`
    /// uses. Both writes are owner only and refuse what the CLI refuses.
    #[tokio::test]
    async fn board_grants_create_list_and_revoke_as_tiller_grant_does() {
        use tiller_core::domain::{ApprovalAction, ApprovalKind, ApprovalState};
        use tiller_core::state::approvals::{NewApproval, NewPr};
        let mut fleet = Fleet::new();
        let home = fleet.dir.path().to_owned();
        let app = app(&home, Assets::of::<Fixture>());
        let cookie = owner_cookie(&home);
        let write = |method: &str, path: &str, body: Value, cookie: &str| {
            let headers: Vec<(&str, &str)> = if cookie.is_empty() {
                vec![]
            } else {
                vec![("cookie", cookie)]
            };
            mutation(method, path, body, &headers)
        };
        let grants = |app: Router| async move {
            let answer = get(&app, "/api/v1/projects/alpha/grants").await;
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
            serde_json::from_str::<Value>(&answer.body).unwrap()["grants"].clone()
        };
        let squash =
            json!({"v": 1, "actions": ["squash"], "base_ref": "main", "checks": "success"});

        assert_eq!(grants(app.clone()).await, json!([]));
        let missing = get(&app, "/api/v1/projects/nope/grants").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);

        // Refused before anything is written.
        let anonymous = send(
            &app,
            write(
                "POST",
                "/api/v1/projects/alpha/grants",
                json!({"idempotency_key": "k-g-1", "kind": "merge", "conditions": squash}),
                "",
            ),
        )
        .await;
        assert_eq!(
            anonymous.status,
            StatusCode::FORBIDDEN,
            "{}",
            anonymous.body
        );
        for (key, bad) in [
            ("k-g-2", json!({"kind": "yolo", "conditions": squash})),
            ("k-g-3", json!({"kind": "merge", "conditions": ["squash"]})),
            (
                "k-g-4",
                json!({"kind": "merge", "conditions": {"actions": ["squash"]}}),
            ),
            (
                "k-g-5",
                json!({"kind": "merge", "conditions": {"v": 2, "actions": ["squash"]}}),
            ),
            (
                "k-g-6",
                json!({"kind": "merge", "conditions": {"v": 1, "actions": []}}),
            ),
            (
                "k-g-7",
                json!({"kind": "merge", "conditions": {"v": 1, "actions": ["push"]}}),
            ),
            (
                "k-g-8",
                json!({"kind": "land_direct", "conditions": {"v": 1, "actions": ["squash"]}}),
            ),
            (
                "k-g-9",
                json!({"kind": "merge", "conditions": squash, "expires": "someday"}),
            ),
            (
                "k-g-10",
                json!({"kind": "merge", "conditions": squash, "name": "extra"}),
            ),
        ] {
            let mut body = bad.clone();
            body["idempotency_key"] = json!(key);
            let refused = send(
                &app,
                write("POST", "/api/v1/projects/alpha/grants", body, &cookie),
            )
            .await;
            assert_eq!(
                refused.status,
                StatusCode::BAD_REQUEST,
                "{bad}: {}",
                refused.body
            );
        }
        // The full v1 shape is checked in core, as the CLI checks it, and the
        // board shows the message as it comes.
        let typo = send(
            &app,
            write(
                "POST",
                "/api/v1/projects/alpha/grants",
                json!({"idempotency_key": "k-g-10b", "kind": "merge",
                    "conditions": {"v": 1, "actions": ["squash"], "card_kinds": ["bug"]}}),
                &cookie,
            ),
        )
        .await;
        assert_eq!(typo.status, StatusCode::BAD_REQUEST, "{}", typo.body);
        let typo: Value = serde_json::from_str(&typo.body).unwrap();
        assert_eq!(typo["error"]["code"], "usage", "{typo}");
        assert!(
            typo["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("grant conditions: card_kinds takes \"ship\""),
            "{typo}"
        );
        let unknown = send(
            &app,
            write(
                "POST",
                "/api/v1/projects/nope/grants",
                json!({"idempotency_key": "k-g-11", "kind": "merge", "conditions": squash}),
                &cookie,
            ),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
        assert_eq!(grants(app.clone()).await, json!([]), "a refusal wrote");

        let create = json!({"idempotency_key": "k-g-12", "kind": "merge", "conditions": squash,
            "expires": "2099-02-01T18:00"});
        let created = send(
            &app,
            write(
                "POST",
                "/api/v1/projects/alpha/grants",
                create.clone(),
                &cookie,
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        let created: Value = serde_json::from_str(&created.body).unwrap();
        let id = created["data"]["id"].as_str().unwrap().to_owned();
        assert_eq!(created["data"]["granted_by"], "board", "{created}");
        assert_eq!(
            created["data"]["conditions"]["expires_at"], "2099-02-01T18:00:00.000Z",
            "{created}"
        );
        let replay = send(
            &app,
            write("POST", "/api/v1/projects/alpha/grants", create, &cookie),
        )
        .await;
        let replay: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(replay["replayed"], true, "{replay}");

        // An approval the grant accepted counts as one use.
        let owner = Ctx::new(Actor::Owner);
        let daemon = Ctx::new(Actor::Daemon);
        let card = fleet
            .store
            .card_create(
                &owner,
                None,
                tiller_core::state::cards::NewCard {
                    project_id: "alpha".into(),
                    kind: CardKind::Ship,
                    title: "granted".into(),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
            .value;
        let pr = fleet
            .store
            .pr_link(
                &owner,
                NewPr {
                    card: card.id.clone(),
                    run_id: None,
                    number: 4,
                    url: "https://example.com/example/alpha/pull/4".into(),
                    head_sha: Some("1111222233334444".into()),
                    base_ref: Some("main".into()),
                },
            )
            .unwrap()
            .value;
        let accepted = fleet
            .store
            .approval_request(
                &daemon,
                NewApproval {
                    kind: ApprovalKind::Merge,
                    card: card.id.clone(),
                    run_id: None,
                    pr_id: Some(pr.id),
                    branch: None,
                    head_sha: "1111222233334444".into(),
                    action: ApprovalAction::Squash,
                    memory_paths: None,
                    grant_id: Some(id.clone()),
                },
            )
            .unwrap()
            .value;
        assert_eq!(accepted.state, ApprovalState::Accepted);
        let listed = grants(app.clone()).await;
        assert_eq!(listed[0]["id"], id.as_str(), "{listed}");
        assert_eq!(listed[0]["short"], ids::short(&id), "{listed}");
        assert_eq!(listed[0]["kind"], "merge");
        assert_eq!(listed[0]["uses"], 1, "{listed}");
        assert_eq!(listed[0]["revoked_at"], Value::Null);

        // The revoke: owner only, a reason when one is sent, a known grant.
        let path = format!("/api/v1/grants/{}", ids::short(&id));
        let anonymous = send(
            &app,
            write("DELETE", &path, json!({"idempotency_key": "k-g-13"}), ""),
        )
        .await;
        assert_eq!(
            anonymous.status,
            StatusCode::FORBIDDEN,
            "{}",
            anonymous.body
        );
        let blank = send(
            &app,
            write(
                "DELETE",
                &path,
                json!({"idempotency_key": "k-g-14", "reason": " "}),
                &cookie,
            ),
        )
        .await;
        assert_eq!(blank.status, StatusCode::BAD_REQUEST, "{}", blank.body);
        let unknown = send(
            &app,
            write(
                "DELETE",
                "/api/v1/grants/zzzzzzz",
                json!({"idempotency_key": "k-g-15"}),
                &cookie,
            ),
        )
        .await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
        assert_eq!(grants(app.clone()).await[0]["revoked_at"], Value::Null);

        let revoked = send(
            &app,
            write(
                "DELETE",
                &path,
                json!({"idempotency_key": "k-g-16"}),
                &cookie,
            ),
        )
        .await;
        assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
        let listed = grants(app.clone()).await;
        assert_ne!(listed[0]["revoked_at"], Value::Null, "{listed}");
        assert_eq!(listed[0]["revoke_reason"], "revoked by the owner");
        let approval: Approval = get_row(fleet.store.conn(), "approval", &accepted.id).unwrap();
        assert_eq!(
            approval.state,
            ApprovalState::Stale,
            "the revoke left it accepted"
        );
        // Revoking a revoked grant returns it and writes nothing new.
        let again = send(
            &app,
            write(
                "DELETE",
                &path,
                json!({"idempotency_key": "k-g-17"}),
                &cookie,
            ),
        )
        .await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        let revokes: i64 = fleet
            .store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'grant.revoked'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revokes, 1);
    }

    /// The board's project add: owner-only, never forwarded, replayed by
    /// its key, idempotent for the same values and refused for a taken id.
    #[tokio::test]
    async fn board_project_add_registers_a_project() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let cookie = owner_cookie(home);
        let add = |key: &str, fields: Value, cookie: &str| {
            let mut body = json!({"idempotency_key": key, "id": "gamma", "name": "Gamma",
                "repo": "example/gamma", "delivery": "direct-pr", "capacity": 2});
            if let (Some(body), Some(fields)) = (body.as_object_mut(), fields.as_object()) {
                body.extend(fields.clone());
            }
            let headers: Vec<(&str, &str)> = if cookie.is_empty() {
                vec![]
            } else {
                vec![("cookie", cookie)]
            };
            mutation("POST", "/api/v1/projects", body, &headers)
        };

        let anonymous = send(&app, add("k-add-1", json!({}), "")).await;
        assert_eq!(
            anonymous.status,
            StatusCode::FORBIDDEN,
            "{}",
            anonymous.body
        );
        let peer = send(&app, add("k-add-2", json!({"host": "laptop"}), &cookie)).await;
        assert_ne!(peer.status, StatusCode::OK, "{}", peer.body);
        let bad = send(&app, add("k-add-3", json!({"delivery": "yolo"}), &cookie)).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);

        let added = send(&app, add("k-add-4", json!({}), &cookie)).await;
        assert_eq!(added.status, StatusCode::OK, "{}", added.body);
        let body: Value = serde_json::from_str(&added.body).unwrap();
        assert_eq!(body["data"]["id"], "gamma", "{body}");
        assert_eq!(body["data"]["repo"], "example/gamma", "{body}");
        assert_eq!(body["data"]["delivery"], "direct-pr", "{body}");
        assert_eq!(body["data"]["capacity"], 2, "{body}");
        let replay = send(&app, add("k-add-4", json!({}), &cookie)).await;
        let body: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");
        let again = send(&app, add("k-add-5", json!({}), &cookie)).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        let taken = send(&app, add("k-add-6", json!({"name": "Other"}), &cookie)).await;
        assert_eq!(
            taken.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            taken.body
        );
        assert!(taken.body.contains("project id is taken"), "{}", taken.body);
        let projects = get(&app, "/api/v1/projects").await;
        assert!(projects.body.contains("\"gamma\""), "{}", projects.body);
    }

    /// A git checkout at `path` whose origin is `example/<repo>` on GitHub.
    fn git_checkout(path: &std::path::Path, repo: &str) {
        std::fs::create_dir_all(path).unwrap();
        for args in [
            vec!["init", "--quiet", "-b", "main"],
            vec![
                "remote",
                "add",
                "origin",
                &format!("https://github.com/example/{repo}.git"),
            ],
        ] {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
        }
    }

    /// Add project's Local checkout: an existing checkout of the repo is
    /// set as `checkouts.<id>` with a `config.changed` event, the name
    /// defaults to the id, and a folder that does not suit is refused with
    /// why, before anything is added or written.
    #[tokio::test]
    async fn board_project_add_sets_the_local_checkout() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let cookie = owner_cookie(home);
        let code = tempfile::tempdir().unwrap();
        let root = code.path().join("code");
        std::fs::create_dir_all(&root).unwrap();
        let config = home.join("config.toml");
        std::fs::write(
            &config,
            format!("[spawn]\ncheckouts_root = \"{}\"\n", root.display()),
        )
        .unwrap();
        let add = |key: &str, id: &str, checkout: Value| {
            mutation(
                "POST",
                "/api/v1/projects",
                json!({"idempotency_key": key, "id": id, "repo": format!("example/{id}"),
                    "delivery": "direct-pr", "checkout": checkout}),
                &[("cookie", cookie.as_str())],
            )
        };
        let refusal = |answer: &Answer| -> (StatusCode, String, String) {
            let body: Value = serde_json::from_str(&answer.body).unwrap();
            (
                answer.status,
                body["error"]["code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                body["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        };

        // Not a checkout, a checkout of another repo, outside the root and
        // a non-empty folder to clone into: each refused, nothing added.
        let plain = root.join("plain");
        std::fs::create_dir_all(plain.join("src")).unwrap();
        let other = root.join("other");
        git_checkout(&other, "other");
        for (key, checkout, why) in [
            (
                "k-co-1",
                json!({"path": plain.display().to_string(), "clone": false}),
                format!("{} is not a git checkout", plain.display()),
            ),
            (
                "k-co-2",
                json!({"path": other.display().to_string(), "clone": false}),
                format!(
                    "{} is a checkout of example/other, not example/gamma",
                    other.display()
                ),
            ),
            (
                "k-co-3",
                json!({"path": "/srv/gamma", "clone": true}),
                format!(
                    "/srv/gamma is not under spawn.checkouts_root ({}), where the orchestrator looks for checkouts",
                    root.display()
                ),
            ),
            (
                "k-co-4",
                json!({"path": plain.display().to_string(), "clone": true}),
                format!(
                    "{} is not empty; give a new or empty folder to clone into",
                    plain.display()
                ),
            ),
        ] {
            let answer = send(&app, add(key, "gamma", checkout)).await;
            assert_eq!(
                refusal(&answer),
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "unsuitable_checkout".to_owned(),
                    why
                ),
                "{key}"
            );
        }
        let projects = get(&app, "/api/v1/projects").await;
        assert!(!projects.body.contains("\"gamma\""), "{}", projects.body);

        // An empty path is `<root>/<id>`: there, a checkout of the repo.
        let gamma = root.join("gamma");
        git_checkout(&gamma, "gamma");
        let added = send(&app, add("k-co-5", "gamma", json!({"path": ""}))).await;
        assert_eq!(added.status, StatusCode::OK, "{}", added.body);
        let body: Value = serde_json::from_str(&added.body).unwrap();
        assert_eq!(body["data"]["name"], "gamma", "{body}");
        assert_eq!(body["events"].as_array().map(Vec::len), Some(2), "{body}");
        let table = config_toml::load(&config).unwrap();
        assert_eq!(
            config_toml::get(&table, "checkouts.gamma").and_then(toml::Value::as_str),
            Some(gamma.to_str().unwrap())
        );
        let replay = send(&app, add("k-co-5", "gamma", json!({"path": ""}))).await;
        let body: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");

        // A clone for an id that is taken is refused before it runs.
        let taken = send(
            &app,
            add(
                "k-co-6",
                "gamma",
                json!({"path": root.join("new").display().to_string(), "clone": true}),
            ),
        )
        .await;
        assert_eq!(
            refusal(&taken),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                "refused".to_owned(),
                "project gamma: project id is taken".to_owned()
            )
        );
        assert!(!root.join("new").exists());
    }

    /// The Local checkout check Add project asks as the folder is typed.
    #[tokio::test]
    async fn the_checkout_check_says_what_add_project_would_do() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let code = tempfile::tempdir().unwrap();
        let root = code.path().join("code");
        std::fs::write(
            home.join("config.toml"),
            format!("[spawn]\ncheckouts_root = \"{}\"\n", root.display()),
        )
        .unwrap();
        let check = |query: &str| {
            let app = app.clone();
            let path = format!("/api/v1/projects/checkout?{query}");
            async move {
                let answer = get(&app, &path).await;
                assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
                serde_json::from_str::<Value>(&answer.body).unwrap()
            }
        };

        let fresh = check("project=delta&repo=example/delta&clone=true").await;
        assert_eq!(
            fresh,
            json!({"path": root.join("delta").display().to_string(), "plan": "clone",
                "remote": "https://github.com/example/delta.git", "problem": null})
        );
        let delta = root.join("delta");
        git_checkout(&delta, "delta");
        let there = check("project=delta&repo=example/delta").await;
        assert_eq!(there["plan"], "use", "{there}");
        let cloned = check("project=delta&repo=example/delta&clone=true").await;
        assert_eq!(cloned["plan"], "use", "{cloned}");

        let missing = check("project=delta&repo=example/delta&path=%2Fsrv%2Fdelta").await;
        assert_eq!(missing["plan"], Value::Null, "{missing}");
        assert!(
            missing["problem"]
                .as_str()
                .unwrap()
                .contains("is not under spawn.checkouts_root"),
            "{missing}"
        );
        let no_id = check("project=&repo=example/delta").await;
        assert_eq!(
            no_id["problem"], "fill in the id first: the folder is named after it",
            "{no_id}"
        );
        // The board sends no `project` at all while the id is empty.
        let absent = check("repo=example/delta").await;
        assert_eq!(absent, no_id, "{absent}");
        // An id that is taken has no folder to clone into.
        let alpha = check("project=alpha&repo=example/alpha&clone=true").await;
        assert_eq!(
            alpha["problem"], "project alpha: project id is taken",
            "{alpha}"
        );
    }

    /// A fake `ssh` for Add a host: it records its arguments and what the
    /// peers directory held while it ran, then runs `body`.
    fn fake_ssh(dir: &std::path::Path, home: &std::path::Path, body: &str) -> Vec<OsString> {
        let script = dir.join("ssh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{args}'\nls -a '{peers}' > '{during}' 2>/dev/null\n{body}\n",
                args = dir.join("args").display(),
                peers = home.join("peers").display(),
                during = dir.join("during").display(),
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        vec![script.into_os_string()]
    }

    /// The peer's accept of `scope`, as `tiller peer accept --json` prints it.
    fn accepts(scope: &str) -> String {
        format!(
            "echo '{}'",
            json!({"ok": true, "data": {
                "host_id": "01JC00000000000000000PEER1", "label": "host-b",
                "key_id": "01JC00000000000000000KEY01", "scope": scope,
                "schema_version": 1, "peer_protocol": "tiller-peer/1", "tiller_version": "0.1.0",
            }})
        )
    }

    fn pairing_app(home: &std::path::Path, ssh: Vec<OsString>) -> Router {
        app_ssh(
            home,
            Assets::of::<Fixture>(),
            Damage::default(),
            Peers::idle(),
            Cli::local(),
            None,
            ssh,
        )
    }

    /// `POST /api/v1/peers` as Settings sends it.
    fn pair_request(key: &str, fields: Value, cookie: &str) -> Request<Body> {
        let mut body = json!({"idempotency_key": key, "label": "host-b",
            "ssh": "b.example.com", "scope": "act"});
        if let (Some(body), Some(fields)) = (body.as_object_mut(), fields.as_object()) {
            body.extend(fields.clone());
        }
        let headers: Vec<(&str, &str)> = if cookie.is_empty() {
            vec![]
        } else {
            vec![("cookie", cookie)]
        };
        mutation("POST", "/api/v1/peers", body, &headers)
    }

    pub(crate) fn error_of(answer: &Answer) -> Value {
        let body: Value = serde_json::from_str(&answer.body).unwrap();
        body["error"].clone()
    }

    #[tokio::test]
    async fn pairing_needs_the_owner_cookie() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let scratch = tempfile::tempdir().unwrap();
        let app = pairing_app(home, fake_ssh(scratch.path(), home, &accepts("act")));

        let anonymous = send(&app, pair_request("k-pair-1", json!({}), "")).await;
        assert_eq!(
            anonymous.status,
            StatusCode::FORBIDDEN,
            "{}",
            anonymous.body
        );
        // Refused before ssh: nothing ran and no key was made.
        assert!(!scratch.path().join("args").exists());
        assert!(!home.join("peers").exists());
        let no_origin = mutation(
            "POST",
            "/api/v1/peers",
            json!({"idempotency_key": "k-pair-2", "label": "host-b", "ssh": "b", "scope": "act"}),
            &[
                ("cookie", &owner_cookie(home)),
                ("origin", "https://evil.example.com"),
            ],
        );
        let foreign = send(&app, no_origin).await;
        assert_eq!(foreign.status, StatusCode::FORBIDDEN, "{}", foreign.body);
        assert!(!scratch.path().join("args").exists());
    }

    #[tokio::test]
    async fn pairing_is_never_forwarded() {
        let mut fleet = Fleet::new();
        fleet
            .store
            .peer_pair(
                &Ctx::new(Actor::Owner),
                NewPairing {
                    label: "laptop".into(),
                    herdr_machine: None,
                    scope: PeerScope::Act,
                    peer_host_id: "01JC00000000000000000PEER2".into(),
                },
            )
            .unwrap();
        let home = fleet.dir.path();
        let scratch = tempfile::tempdir().unwrap();
        let app = pairing_app(home, fake_ssh(scratch.path(), home, &accepts("act")));
        let cookie = owner_cookie(home);

        let forwarded = send(
            &app,
            pair_request("k-pair-1", json!({"host": "laptop"}), &cookie),
        )
        .await;
        assert_eq!(
            forwarded.status,
            StatusCode::BAD_GATEWAY,
            "{}",
            forwarded.body
        );
        assert_eq!(
            error_of(&forwarded)["reason"],
            "not_allowlisted",
            "{}",
            forwarded.body
        );
        assert!(!scratch.path().join("args").exists());
    }

    #[tokio::test]
    async fn pairing_refuses_a_taken_label() {
        let mut fleet = Fleet::new();
        fleet
            .store
            .peer_pair(
                &Ctx::new(Actor::Owner),
                NewPairing {
                    label: "host-b".into(),
                    herdr_machine: None,
                    scope: PeerScope::Read,
                    peer_host_id: "01JC00000000000000000PEER1".into(),
                },
            )
            .unwrap();
        let home = fleet.dir.path();
        let scratch = tempfile::tempdir().unwrap();
        let app = pairing_app(home, fake_ssh(scratch.path(), home, &accepts("act")));
        let cookie = owner_cookie(home);

        let paired = send(&app, pair_request("k-pair-1", json!({}), &cookie)).await;
        assert_eq!(
            paired.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            paired.body
        );
        assert_eq!(error_of(&paired)["code"], "label_taken", "{}", paired.body);
        assert_eq!(
            error_of(&paired)["message"],
            "host-b is already paired; pick another label, or Unpair it first in its Settings > Hosts card (or run tiller peer remove host-b)",
            "{}",
            paired.body
        );
        let own = send(
            &app,
            pair_request("k-pair-2", json!({"label": "test-host"}), &cookie),
        )
        .await;
        assert_eq!(own.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", own.body);
        assert!(own.body.contains("this host's own label"), "{}", own.body);
        for (key, fields) in [
            ("k-pair-3", json!({"label": "Host B"})),
            ("k-pair-4", json!({"ssh": "-oProxyCommand=sh"})),
            ("k-pair-5", json!({"ssh": "b.example.com;id"})),
            ("k-pair-6", json!({"scope": "admin"})),
        ] {
            let bad = send(&app, pair_request(key, fields.clone(), &cookie)).await;
            assert_eq!(
                bad.status,
                StatusCode::BAD_REQUEST,
                "{fields}: {}",
                bad.body
            );
        }
        assert!(!scratch.path().join("args").exists());
    }

    #[tokio::test]
    async fn pairing_maps_peer_refusal_and_unreachable_ssh() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let cookie = owner_cookie(home);
        let cases = [
            (
                "echo 'ssh: connect to host b.example.com port 22: Connection refused' >&2; exit 255",
                StatusCode::SERVICE_UNAVAILABLE,
                "peer_unreachable",
                None,
                "Connection refused (ssh exit 255)",
            ),
            (
                "echo 'Host key verification failed.' >&2; exit 255",
                StatusCode::SERVICE_UNAVAILABLE,
                "peer_unreachable",
                None,
                "Host key verification failed.",
            ),
            (
                r#"echo '{"ok":false,"error":{"code":"refused","message":"federation.accept is false here"}}'; exit 5"#,
                StatusCode::BAD_GATEWAY,
                "peer_refused",
                Some("accept_off"),
                "federation.accept = false",
            ),
            (
                r#"echo '{"ok":false,"error":{"code":"forbidden","message":"runs only under sshd"}}'; exit 6"#,
                StatusCode::BAD_GATEWAY,
                "peer_refused",
                Some("forbidden"),
                "runs only under sshd",
            ),
            (
                r#"echo '{"ok":false,"error":{"code":"usage","message":"unexpected argument --unattended"}}'; exit 2"#,
                StatusCode::BAD_GATEWAY,
                "peer_refused",
                Some("version"),
                "update it there",
            ),
            (
                r#"echo '{"ok":false,"error":{"code":"usage","message":"host-a is not a label"}}'; exit 2"#,
                StatusCode::BAD_GATEWAY,
                "peer_refused",
                Some("failed"),
                "host-a is not a label",
            ),
            (
                "echo 'bash: line 1: tiller: command not found' >&2; exit 127",
                StatusCode::BAD_GATEWAY,
                "peer_refused",
                Some("no_tiller"),
                "has no tiller command",
            ),
            (
                &accepts("read"),
                StatusCode::BAD_GATEWAY,
                "peer_refused",
                Some("bad_answer"),
                "accepted scope",
            ),
        ];
        for (n, (body, status, code, reason, words)) in cases.into_iter().enumerate() {
            let scratch = tempfile::tempdir().unwrap();
            let app = pairing_app(home, fake_ssh(scratch.path(), home, body));
            let key = format!("k-pair-{n}");
            let answer = send(&app, pair_request(&key, json!({}), &cookie)).await;
            assert_eq!(answer.status, status, "{body}: {}", answer.body);
            let error = error_of(&answer);
            assert_eq!(error["code"], code, "{}", answer.body);
            if let Some(reason) = reason {
                assert_eq!(error["reason"], reason, "{}", answer.body);
            }
            assert!(answer.body.contains(words), "{words}: {}", answer.body);
            assert_eq!(error["host"], "host-b", "{}", answer.body);
        }
        let hosts = get(&app(home, Assets::of::<Fixture>()), "/api/v1/machines").await;
        assert!(!hosts.body.contains("host-b"), "{}", hosts.body);
    }

    #[tokio::test]
    async fn pairing_writes_the_key_file_0600_and_never_returns_it() {
        use std::os::unix::fs::PermissionsExt;
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let scratch = tempfile::tempdir().unwrap();
        let app = pairing_app(home, fake_ssh(scratch.path(), home, &accepts("act")));
        let cookie = owner_cookie(home);

        let paired = send(&app, pair_request("k-pair-1", json!({}), &cookie)).await;
        assert_eq!(paired.status, StatusCode::OK, "{}", paired.body);
        let path = home.join("peers").join("host-b.key");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let secret = raw["secret_key"].as_str().unwrap();
        assert!(!paired.body.contains(secret), "{}", paired.body);
        let file = pairing::read_key_file(&path).unwrap();
        assert_eq!(
            file.peer_host_id.as_deref(),
            Some("01JC00000000000000000PEER1")
        );
        assert_eq!(file.key_id.as_deref(), Some("01JC00000000000000000KEY01"));

        let body: Value = serde_json::from_str(&paired.body).unwrap();
        assert_eq!(body["data"]["label"], "host-b", "{body}");
        assert_eq!(body["data"]["ssh"], "b.example.com", "{body}");
        assert_eq!(
            body["data"]["key_id"], "01JC00000000000000000KEY01",
            "{body}"
        );
        assert_eq!(
            body["data"]["fingerprint"],
            json!(fingerprint(&file.public_key)),
            "{body}"
        );
        // The new host got this host's label and the public half, unattended.
        let args = std::fs::read_to_string(scratch.path().join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        let after = |flag: &str| args[args.iter().position(|a| *a == flag).unwrap() + 1];
        assert_eq!(after("--"), "b.example.com");
        assert_eq!(after("--from"), "test-host");
        assert_eq!(after("--pubkey"), file.public_key);
        assert_eq!(after("--scope"), "act");
        assert!(args.contains(&"--unattended") && args.contains(&"BatchMode=yes"));
        assert!(!args.iter().any(|a| a.contains(secret)));
        // While ssh ran, the key waited under its pending name.
        let during = std::fs::read_to_string(scratch.path().join("during")).unwrap();
        assert!(
            during.lines().any(|f| f == ".host-b.board.key.pending"),
            "{during}"
        );
        assert!(
            !home
                .join("peers")
                .join(".host-b.board.key.pending")
                .exists()
        );

        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("b.example.com"), "{config}");
        let hosts = get(&app, "/api/v1/machines").await;
        let hosts: Value = serde_json::from_str(&hosts.body).unwrap();
        let row = hosts["machines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["label"] == "host-b")
            .unwrap();
        // Listed at once as a peer whose link has not opened, before the
        // link supervisor reads the new pairing.
        assert_eq!(row["link"], "never", "{row}");
        assert_eq!(row["peer_scope"], "act", "{row}");
        let events: Vec<String> = fleet
            .open()
            .conn()
            .prepare("SELECT txn || ' ' || kind || ' ' || actor FROM events ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            events.iter().any(|e| e == "peer.pair host.added board"),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|e| e == "config.set config.changed board"),
            "{events:?}"
        );

        // The same key replays the stored answer and runs no ssh again.
        std::fs::remove_file(scratch.path().join("args")).unwrap();
        let replay = send(&app, pair_request("k-pair-1", json!({}), &cookie)).await;
        let body: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");
        assert!(!scratch.path().join("args").exists());
        let again = send(&app, pair_request("k-pair-2", json!({}), &cookie)).await;
        assert_eq!(
            again.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            again.body
        );

        // Once removed, a replay of the key pairs nothing.
        fleet
            .open()
            .peer_remove(&Ctx::new(Actor::Owner), "host-b")
            .unwrap();
        let gone = send(&app, pair_request("k-pair-1", json!({}), &cookie)).await;
        assert_eq!(gone.status, StatusCode::BAD_REQUEST, "{}", gone.body);
        assert!(gone.body.contains("no longer paired"), "{}", gone.body);
        assert!(!scratch.path().join("args").exists());
    }

    #[tokio::test]
    async fn pairing_refuses_a_host_paired_under_another_label() {
        let mut fleet = Fleet::new();
        fleet
            .store
            .peer_pair(
                &Ctx::new(Actor::Owner),
                NewPairing {
                    label: "laptop".into(),
                    herdr_machine: None,
                    scope: PeerScope::Act,
                    peer_host_id: "01JC00000000000000000PEER1".into(),
                },
            )
            .unwrap();
        let home = fleet.dir.path();
        let cookie = owner_cookie(home);
        let scratch = tempfile::tempdir().unwrap();
        let app = pairing_app(home, fake_ssh(scratch.path(), home, &accepts("act")));

        // The same alias: refused before ssh.
        std::fs::write(
            home.join("config.toml"),
            "[peers.laptop]\nssh = \"b.example.com\"\n",
        )
        .unwrap();
        let alias = send(&app, pair_request("k-pair-1", json!({}), &cookie)).await;
        assert_eq!(
            alias.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            alias.body
        );
        assert_eq!(error_of(&alias)["code"], "already_paired", "{}", alias.body);
        assert!(
            alias.body.contains("paired here as laptop"),
            "{}",
            alias.body
        );
        assert!(!scratch.path().join("args").exists());

        // Another alias to the same host: its answer names it.
        std::fs::write(
            home.join("config.toml"),
            "[peers.laptop]\nssh = \"laptop.example.com\"\n",
        )
        .unwrap();
        let host = send(&app, pair_request("k-pair-2", json!({}), &cookie)).await;
        assert_eq!(
            host.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            host.body
        );
        assert_eq!(error_of(&host)["code"], "already_paired", "{}", host.body);
        assert!(
            host.body.contains("tiller peer add laptop"),
            "{}",
            host.body
        );
        assert!(scratch.path().join("args").exists());
        let left: Vec<_> = std::fs::read_dir(home.join("peers"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        let hosts = get(&app, "/api/v1/machines").await;
        assert!(!hosts.body.contains("host-b"), "{}", hosts.body);
    }

    #[tokio::test]
    async fn pairing_refuses_a_config_that_does_not_parse() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let scratch = tempfile::tempdir().unwrap();
        let app = pairing_app(home, fake_ssh(scratch.path(), home, &accepts("act")));
        std::fs::write(home.join("config.toml"), "[peers\n").unwrap();

        let answer = send(
            &app,
            pair_request("k-pair-1", json!({}), &owner_cookie(home)),
        )
        .await;
        assert_eq!(
            answer.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            answer.body
        );
        assert!(
            answer.body.contains("config.toml does not parse"),
            "{}",
            answer.body
        );
        assert!(!scratch.path().join("args").exists());
        assert!(!home.join("peers").exists());
    }

    #[tokio::test]
    async fn a_failed_pairing_deletes_the_pending_key() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let scratch = tempfile::tempdir().unwrap();
        let refuse = r#"echo '{"ok":false,"error":{"code":"refused","message":"federation.accept is false here"}}'; exit 5"#;
        let app = pairing_app(home, fake_ssh(scratch.path(), home, refuse));
        let cookie = owner_cookie(home);

        let refused = send(&app, pair_request("k-pair-1", json!({}), &cookie)).await;
        assert_eq!(refused.status, StatusCode::BAD_GATEWAY, "{}", refused.body);
        let during = std::fs::read_to_string(scratch.path().join("during")).unwrap();
        assert!(
            during.lines().any(|f| f == ".host-b.board.key.pending"),
            "{during}"
        );
        let left: Vec<_> = std::fs::read_dir(home.join("peers"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "{left:?}");
        assert!(
            !home.join("config.toml").exists() || {
                let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
                !config.contains("b.example.com")
            }
        );
    }

    /// A retired project leaves the board with its closed cards, which the
    /// board would otherwise keep showing for three days. A later change on
    /// one, such as a PR reconcile, carries no row for the board to put
    /// back.
    #[tokio::test]
    async fn a_retired_projects_closed_cards_leave_the_board() {
        let mut fleet = Fleet::new();
        let owner = Ctx::new(Actor::Owner);
        fleet
            .store
            .project_add(
                &owner,
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
        let card = fleet
            .store
            .card_create(
                &owner,
                None,
                tiller_core::state::cards::NewCard {
                    project_id: "gamma".into(),
                    kind: tiller_core::domain::CardKind::Ship,
                    title: "dropped".into(),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
            .value;
        fleet
            .store
            .card_cancel(&owner, &card.id, "not needed")
            .unwrap();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let on_board = |body: &str| {
            let body: Value = serde_json::from_str(body).unwrap();
            body["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["project_id"] == "gamma")
        };

        let health = views::LocalHealth {
            failing_pollers: Vec::new(),
            peers: crate::peer::Federation::default(),
        };
        let change = |store: &Store| {
            let event: tiller_core::domain::Event = tiller_core::state::find(
                store.conn(),
                "card_id = ?1 ORDER BY seq DESC",
                &[&card.id],
            )
            .unwrap()
            .unwrap();
            views::change(store, &health, &event, Timestamp::now())
        };

        let before = get(&app, "/api/v1/board?filter=all").await;
        assert!(on_board(&before.body), "{}", before.body);
        assert!(change(&fleet.store).card.is_some());
        fleet.store.project_retire(&owner, "gamma").unwrap();
        let after = get(&app, "/api/v1/board?filter=all").await;
        assert!(!on_board(&after.body), "{}", after.body);
        let late = change(&fleet.store);
        assert_eq!(late.card_id.as_deref(), Some(card.id.as_str()));
        assert!(late.card.is_none(), "{:?}", late.card);
    }

    /// The board's Archived list: a retired project leaves the projects
    /// read and shows on `?retired=true`, which lists this host only, and
    /// its delete still works from there.
    #[tokio::test]
    async fn retired_projects_list_apart_and_still_delete() {
        let mut fleet = Fleet::new();
        let owner = Ctx::new(Actor::Owner);
        fleet
            .store
            .project_add(
                &owner,
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
        fleet.store.project_retire(&owner, "gamma").unwrap();
        let home = fleet.dir.path();
        let app = app(home, Assets::of::<Fixture>());
        let ids = |body: &str| {
            let body: Value = serde_json::from_str(body).unwrap();
            body["projects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|project| project["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };

        let live = get(&app, "/api/v1/projects").await;
        assert!(
            !ids(&live.body).contains(&"gamma".to_owned()),
            "{}",
            live.body
        );
        assert!(!live.body.contains("retired_at"), "{}", live.body);
        let archived = get(&app, "/api/v1/projects?retired=true").await;
        assert_eq!(archived.status, StatusCode::OK, "{}", archived.body);
        assert_eq!(ids(&archived.body), ["gamma"], "{}", archived.body);
        let body: Value = serde_json::from_str(&archived.body).unwrap();
        assert!(body["projects"][0]["retired_at"].is_string(), "{body}");
        let peer = get(&app, "/api/v1/projects?retired=true&host=laptop").await;
        assert_eq!(peer.status, StatusCode::BAD_REQUEST, "{}", peer.body);

        let cookie = owner_cookie(home);
        let deleted = send(
            &app,
            mutation(
                "POST",
                "/api/v1/projects/gamma/delete",
                json!({"idempotency_key": "k-arch-1", "confirm": "gamma"}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
        let after = get(&app, "/api/v1/projects?retired=true").await;
        assert!(ids(&after.body).is_empty(), "{}", after.body);
    }

    /// The board's project delete: the preview counts what goes and names
    /// the agents it stops, the delete needs the id typed again, an agent
    /// that does not stop refuses it by name, and only the owner may send it.
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
        // A herdr that fails every call: no stop can be proven.
        let herdr = Cli::local().program(vec!["sh".into(), "-c".into(), "exit 1".into()]);
        let app = app_full(
            home,
            Assets::of::<Fixture>(),
            Damage::default(),
            Peers::idle(),
            herdr,
            None,
        );
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

        // alpha's orchestrator is live: the preview lists it to stop, and
        // the delete, whose stop fails, refuses naming it and removes nothing.
        let live = get(&app, "/api/v1/projects/alpha/delete").await;
        let body: Value = serde_json::from_str(&live.body).unwrap();
        assert_eq!(body["refused"], Value::Null, "{body}");
        assert_eq!(body["agents"][0]["role"], "orchestrator", "{body}");
        assert_eq!(body["agents"][0]["local"], true, "{body}");
        let refused = send(&app, delete("alpha", "k-del-1", "alpha", &cookie)).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );
        assert!(
            refused.body.contains("could not stop the orchestrator"),
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

    /// A herdr for the terminal mirror: it logs each call, gives the pane
    /// the size in `{dir}/size` (`cols rows`), and observes by recording
    /// its pid, writing one frame and sleeping as that same process. The
    /// frame is full unless `{dir}/full` says `false`. Its control logs what
    /// it reads and answers a resize with a full frame, and a read gives two
    /// rows of scrollback.
    pub(crate) fn mirror_herdr(dir: &std::path::Path) -> Cli {
        let script = format!(
            r#"echo "$*" >> '{dir}/calls'
case "$*" in
*"pane read"*)
  if [ -e '{dir}/screen' ]; then cat '{dir}/screen'; else printf 'one\n\033[34mtwo\033[0m\n'; fi;;
*"pane layout"*)
  read cols rows < '{dir}/size'
  for pane; do :; done
  [ -e '{dir}/gone' ] && pane=gone
  printf '{{"result":{{"layout":{{"panes":[{{"pane_id":"%s","rect":{{"width":%s,"height":%s}}}}]}}}}}}\n' "$pane" "$cols" "$rows";;
*"pane process-info"*)
  [ -e '{dir}/shell' ] || exit 1
  printf '{{"result":{{"process_info":{{"shell_pid":%s}}}}}}\n' "$(cat '{dir}/shell')";;
*"terminal session observe"*)
  echo $$ >> '{dir}/observers'
  full=$(cat '{dir}/full' 2>/dev/null || echo true)
  printf '{{"type":"terminal.frame","seq":1,"full":%s,"width":80,"height":24,"bytes":"aGk="}}\n' "$full"
  exec sleep 30;;
*"terminal session control"*)
  echo $$ >> '{dir}/controllers'
  if [ -e '{dir}/refuse' ]; then
    echo 'herdr: terminal attach failed: terminal w2:p1 already has an attached client; retry with --takeover' >&2
    exit 1
  fi
  printf '{{"type":"terminal.frame","seq":1,"full":true,"width":100,"height":30,"bytes":"aGk="}}\n'
  while read -r line; do
    printf '%s\n' "$line" >> '{dir}/control-in'
    case "$line" in *terminal.resize*)
      printf '{{"type":"terminal.frame","seq":1,"full":true,"width":0,"height":0,"bytes":"aGk="}}\n';;
    esac
  done;;
esac"#,
            dir = dir.display()
        );
        std::fs::write(dir.join("size"), "80 24\n").unwrap();
        Cli::local().program(vec![
            "sh".into(),
            "-c".into(),
            script.into(),
            "herdr".into(),
        ])
    }

    fn mirror_app(fleet: &Fleet, dir: &std::path::Path) -> Router {
        app_full(
            fleet.dir.path(),
            Assets::of::<Fixture>(),
            Damage::default(),
            Peers::idle(),
            mirror_herdr(dir),
            None,
        )
    }

    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// Waits up to 5 seconds for `pid` to exit.
    async fn gone(pid: &str) -> bool {
        for _ in 0..100 {
            if !alive(pid) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        false
    }

    /// The next NDJSON line of a streaming body.
    pub(crate) async fn next_line(
        body: &mut (impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
        pending: &mut String,
    ) -> Value {
        use futures_util::StreamExt;
        loop {
            if let Some(end) = pending.find('\n') {
                let line: String = pending.drain(..=end).collect();
                return serde_json::from_str(&line).unwrap();
            }
            let chunk = tokio::time::timeout(std::time::Duration::from_secs(10), body.next())
                .await
                .expect("the stream stalled")
                .expect("the stream ended")
                .unwrap();
            pending.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    #[tokio::test]
    async fn the_terminal_mirror_refuses_all_but_the_owner() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = mirror_app(&fleet, dir);
        let cookie = owner_cookie(fleet.dir.path());
        let stream = format!("/api/v1/agents/{session}/terminal");
        let input = format!("/api/v1/agents/{session}/terminal/input");
        let refit = format!("/api/v1/agents/{session}/terminal/fit");
        let history = format!("/api/v1/agents/{session}/terminal/history");
        let tail = format!("/api/v1/agents/{session}/terminal/tail");
        let prompt = format!("/api/v1/agents/{session}/prompt");
        let answer = format!("/api/v1/agents/{session}/prompt/answer");
        let paths = [
            (stream.as_str(), json!({})),
            (input.as_str(), json!({"keys": ["ctrl+c"]})),
            (refit.as_str(), json!({"cols": 100, "rows": 30})),
            (history.as_str(), json!({})),
            (tail.as_str(), json!({})),
            (prompt.as_str(), json!({})),
            (answer.as_str(), json!({"option": 1, "screen": "x"})),
        ];
        let cookie_header = ("cookie", cookie.as_str());
        let refusals: [(&str, Vec<(&str, &str)>); 4] = [
            ("no owner cookie", vec![]),
            ("a forged cookie", vec![("cookie", "tiller_owner=forged")]),
            // An --allow-host name takes the owner only from its https origin.
            (
                "an allow-host name over http",
                vec![
                    cookie_header,
                    ("host", "board.example.com"),
                    ("origin", "http://board.example.com"),
                ],
            ),
            (
                "another site",
                vec![cookie_header, ("sec-fetch-site", "cross-site")],
            ),
        ];
        for (path, body) in &paths {
            for (case, headers) in &refusals {
                let answer = send(&app, mutation("POST", path, body.clone(), headers)).await;
                assert_eq!(
                    answer.status,
                    StatusCode::FORBIDDEN,
                    "{case} {path}: {}",
                    answer.body
                );
            }
        }
        // Nothing reached herdr.
        assert!(!dir.join("calls").exists());

        // The owner's keys and pastes reach the pane, in order and verbatim.
        let sent = send(
            &app,
            mutation(
                "POST",
                &input,
                json!({"keys": ["-", "ctrl+c", "enter"]}),
                &[cookie_header],
            ),
        )
        .await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        let pasted = send(
            &app,
            mutation("POST", &input, json!({"text": "-rf x"}), &[cookie_header]),
        )
        .await;
        assert_eq!(pasted.status, StatusCode::OK, "{}", pasted.body);
        let both = send(
            &app,
            mutation(
                "POST",
                &input,
                json!({"keys": ["a"], "text": "b"}),
                &[cookie_header],
            ),
        )
        .await;
        assert_eq!(both.status, StatusCode::BAD_REQUEST, "{}", both.body);
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            [
                "pane send-keys w2:p1 - ctrl+c enter",
                // A paste that starts with a dash presses it, so herdr
                // never reads the text as a flag.
                "pane send-keys w2:p1 -",
                "pane send-text w2:p1 rf x"
            ]
        );

        // The owner reads the pane's scrollback, colors kept; a size herdr
        // would not read is refused before it is asked.
        let read = send(
            &app,
            mutation("POST", &history, json!({}), &[cookie_header]),
        )
        .await;
        assert_eq!(read.status, StatusCode::OK, "{}", read.body);
        let read: Value = serde_json::from_str(&read.body).unwrap();
        assert_eq!(read["session_id"], session);
        assert_eq!(read["text"], "one\n\u{1b}[34mtwo\u{1b}[0m\n");
        for lines in [0, 1001] {
            let answer = send(
                &app,
                mutation("POST", &history, json!({"lines": lines}), &[cookie_header]),
            )
            .await;
            assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
        }
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert_eq!(
            calls.lines().last(),
            Some("pane read w2:p1 --source recent --lines 1000 --format ansi")
        );

        // At an allowed host, the owner's keys come from its https origin.
        let remote = send(
            &app,
            mutation(
                "POST",
                &input,
                json!({"keys": ["enter"]}),
                &[
                    cookie_header,
                    ("host", "board.example.com"),
                    ("origin", "https://board.example.com"),
                ],
            ),
        )
        .await;
        assert_eq!(remote.status, StatusCode::OK, "{}", remote.body);
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert_eq!(calls.lines().last(), Some("pane send-keys w2:p1 enter"));
    }

    /// Puts `session` on a permission prompt for `tool`, as its hook does.
    fn waiting(fleet: &Fleet, session: &str, tool: &str) {
        let store = fleet.open();
        store
            .conn()
            .execute(
                "INSERT INTO observations (id, session_id, source, event, observed_at, recorded_at, payload)
                 VALUES ('wait', ?1, 'hook', 'PermissionRequest', ?2, ?2, ?3)",
                rusqlite::params![session, at(0), json!({"tool_name": tool}).to_string()],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE sessions SET activity = 'waiting_permission', activity_observed_at = ?2
                 WHERE id = ?1",
                rusqlite::params![session, at(0)],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn a_prompt_answer_presses_its_digit_only_on_the_dialog_it_was_drawn_from() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = mirror_app(&fleet, dir);
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let read = format!("/api/v1/agents/{session}/prompt");
        let answer = format!("/api/v1/agents/{session}/prompt/answer");
        let press = |screen: &str, option: u8| json!({"option": option, "screen": screen});
        let calls = || std::fs::read_to_string(dir.join("calls")).unwrap_or_default();

        // Both pass every mirror guard.
        for (path, body) in [(&read, json!({})), (&answer, press("x", 1))] {
            for headers in [vec![], vec![("cookie", "tiller_owner=forged")]] {
                let refused = send(&app, mutation("POST", path, body.clone(), &headers)).await;
                assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
            }
        }

        // An agent that is not waiting has its screen left unread, and an
        // answer to it is refused.
        let idle = send(&app, mutation("POST", &read, json!({}), &owner)).await;
        assert_eq!(idle.status, StatusCode::OK, "{}", idle.body);
        let idle: Value = serde_json::from_str(&idle.body).unwrap();
        assert_eq!(idle["reason"], "not_waiting");
        assert_eq!(idle["prompt"], Value::Null);
        let late = send(&app, mutation("POST", &answer, press("x", 1), &owner)).await;
        assert_eq!(late.status, StatusCode::CONFLICT, "{}", late.body);
        assert_eq!(calls(), "");

        waiting(&fleet, &session, "Bash");
        let bash = include_str!("../../herdr/tests/fixtures/claude/permission-dialog.ansi");
        std::fs::write(dir.join("screen"), bash).unwrap();
        let shown = send(&app, mutation("POST", &read, json!({}), &owner)).await;
        assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
        let shown: Value = serde_json::from_str(&shown.body).unwrap();
        assert_eq!(shown["reason"], Value::Null);
        assert_eq!(
            shown["prompt"]["command"],
            "rm -f nothing-here.txt && touch made.txt"
        );
        assert_eq!(shown["prompt"]["options"][3]["text"], "No");
        let screen = shown["prompt"]["screen"].as_str().unwrap();

        // A dialog that changed after the buttons were drawn, or an option
        // it does not offer, types nothing.
        let stale = send(&app, mutation("POST", &answer, press("other", 1), &owner)).await;
        assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
        assert!(stale.body.contains("screen_changed"), "{}", stale.body);
        let unoffered = send(&app, mutation("POST", &answer, press(screen, 5), &owner)).await;
        assert_eq!(
            unoffered.status,
            StatusCode::BAD_REQUEST,
            "{}",
            unoffered.body
        );
        assert!(!calls().contains("send-keys"), "{}", calls());

        // The matching dialog gets the digit alone, with no Enter.
        let sent = send(&app, mutation("POST", &answer, press(screen, 4), &owner)).await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        let sent: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(sent["data"]["sent"]["option"], 4);
        assert_eq!(calls().lines().last(), Some("pane send-keys w2:p1 4"));

        // A parallel tool call observed after the dialog opened leaves the
        // tool it asks about alone.
        fleet
            .open()
            .conn()
            .execute(
                "INSERT INTO observations (id, session_id, source, event, observed_at, recorded_at, payload)
                 VALUES ('later', ?1, 'hook', 'PostToolUse', ?2, ?2, ?3)",
                rusqlite::params![session, at(5), json!({"tool_name": "Read"}).to_string()],
            )
            .unwrap();
        let still = send(&app, mutation("POST", &read, json!({}), &owner)).await;
        let still: Value = serde_json::from_str(&still.body).unwrap();
        assert_eq!(still["prompt"]["tool"], "Bash", "{still}");

        // A dialog for another tool than the hook named is none.
        let edit = include_str!("../tests/fixtures/prompt/edit.txt");
        std::fs::write(dir.join("screen"), edit).unwrap();
        let other = send(&app, mutation("POST", &read, json!({}), &owner)).await;
        let other: Value = serde_json::from_str(&other.body).unwrap();
        assert_eq!(other["reason"], "no_dialog");
    }

    #[tokio::test]
    async fn a_trust_dialog_answer_presses_the_arrow_keys_to_its_row_then_enter() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = mirror_app(&fleet, dir);
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let read = format!("/api/v1/agents/{session}/prompt");
        let answer = format!("/api/v1/agents/{session}/prompt/answer");
        let press = |screen: &str, option: u8| json!({"option": option, "screen": screen});
        let calls = || std::fs::read_to_string(dir.join("calls")).unwrap_or_default();
        let trust = include_str!("../../herdr/tests/fixtures/claude/trust-dialog.ansi");
        std::fs::write(dir.join("screen"), trust).unwrap();

        // No hook asks for the dialog: herdr sees the agent blocked, and the
        // session waits for an answer.
        fleet
            .open()
            .conn()
            .execute(
                "UPDATE sessions SET activity = 'waiting_input', activity_observed_at = ?2
                 WHERE id = ?1",
                rusqlite::params![session, at(0)],
            )
            .unwrap();
        let shown = send(&app, mutation("POST", &read, json!({}), &owner)).await;
        assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
        let shown: Value = serde_json::from_str(&shown.body).unwrap();
        assert_eq!(shown["prompt"]["layout"], "trust", "{shown}");
        assert_eq!(shown["prompt"]["folder"], "/home/user/project");
        assert_eq!(
            shown["prompt"]["options"][1]["text"],
            "Yes, I trust this folder"
        );
        let screen = shown["prompt"]["screen"].as_str().unwrap();

        // A changed dialog, or an option it does not offer, types nothing.
        let stale = send(&app, mutation("POST", &answer, press("other", 2), &owner)).await;
        assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
        let unoffered = send(&app, mutation("POST", &answer, press(screen, 3), &owner)).await;
        assert_eq!(
            unoffered.status,
            StatusCode::BAD_REQUEST,
            "{}",
            unoffered.body
        );
        assert!(!calls().contains("send-keys"), "{}", calls());

        // The cursor is on No, exit: Yes is one row down, then Enter, in one call.
        let sent = send(&app, mutation("POST", &answer, press(screen, 2), &owner)).await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        assert_eq!(
            calls().lines().last(),
            Some("pane send-keys w2:p1 down enter")
        );
        let sent = send(&app, mutation("POST", &answer, press(screen, 1), &owner)).await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        assert_eq!(calls().lines().last(), Some("pane send-keys w2:p1 enter"));

        // A permission wait never reads the trust dialog.
        waiting(&fleet, &session, "Bash");
        let other = send(&app, mutation("POST", &read, json!({}), &owner)).await;
        let other: Value = serde_json::from_str(&other.body).unwrap();
        assert_eq!(other["reason"], "no_dialog", "{other}");
        let late = send(&app, mutation("POST", &answer, press(screen, 2), &owner)).await;
        assert_eq!(late.status, StatusCode::CONFLICT, "{}", late.body);
    }

    #[tokio::test]
    async fn an_agents_last_output_is_its_own_newest_turn_event() {
        let mut fleet = Fleet::new();
        let quiet = fleet.worker("w3:p1");
        let orchestrator = fleet.orchestrator_session.clone();
        let store = fleet.open();
        // A tool call and a turn end are the agent's output; the owner's
        // prompt, a notification, herdr's poll and the transcript after them
        // are not.
        for (n, (source, event)) in [
            ("hook", "PreToolUse"),
            ("hook", "Stop"),
            ("hook", "UserPromptSubmit"),
            ("hook", "Notification:idle_prompt"),
            ("herdr", "herdr:working"),
            ("transcript", "transcript:turn_end"),
        ]
        .into_iter()
        .enumerate()
        {
            let observed_at = at(10 * i64::try_from(n).unwrap());
            store
                .conn()
                .execute(
                    "INSERT INTO observations (id, session_id, source, event, observed_at, recorded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    rusqlite::params![format!("o{n}"), orchestrator, source, event, observed_at],
                )
                .unwrap();
        }
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let read = get(&app, "/api/v1/agents").await;
        assert_eq!(read.status, StatusCode::OK, "{}", read.body);
        let read: Value = serde_json::from_str(&read.body).unwrap();
        let row = &read["orchestrators"][0];
        assert_eq!(row["session_id"], orchestrator);
        assert_eq!(row["last_output_at"], at(10).to_string());
        let worker = row["workers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|worker| worker["session_id"] == quiet.as_str())
            .unwrap();
        assert_eq!(worker["last_output_at"], Value::Null);
    }

    /// A starter that restarts a session by stamping it as launched now, and
    /// remembers which it restarted and as whom.
    #[derive(Default)]
    struct FakeRestarts(Mutex<Vec<String>>, Mutex<Vec<String>>);

    impl OrchestratorStart for FakeRestarts {
        fn start(
            &self,
            _store: &mut Store,
            _ctx: &Ctx,
            _project: &str,
            _restart: bool,
        ) -> Result<Applied<Orchestrator>, StartError> {
            unreachable!("the stale restart does not start an orchestrator")
        }

        fn restart(&self, store: &mut Store, ctx: &Ctx, session: &Session) -> Result<(), String> {
            let role = tiller_agents::roles::Role::parse(&session.role.to_string()).unwrap();
            tiller_agents::stale::record(store, &Ctx::new(Actor::Daemon), &session.id, role)
                .map_err(|error| error.to_string())?;
            self.0.lock().unwrap().push(session.id.clone());
            self.1.lock().unwrap().push(ctx.actor.to_string());
            Ok(())
        }
    }

    pub(crate) fn app_restarting(
        home: &std::path::Path,
        starter: Arc<dyn OrchestratorStart>,
    ) -> Router {
        app_retros(home, starter, crate::retros::Retros::default())
    }

    /// The board API with this starter, running retros through `retros`.
    pub(crate) fn app_retros(
        home: &std::path::Path,
        starter: Arc<dyn OrchestratorStart>,
        retros: crate::retros::Retros,
    ) -> Router {
        app_running(home, Some(starter), Registry::new(), retros)
    }

    fn app_polling(
        home: &std::path::Path,
        starter: Option<Arc<dyn OrchestratorStart>>,
        pollers: Registry,
    ) -> Router {
        app_running(home, starter, pollers, crate::retros::Retros::default())
    }

    fn app_running(
        home: &std::path::Path,
        starter: Option<Arc<dyn OrchestratorStart>>,
        pollers: Registry,
        retros: crate::retros::Retros,
    ) -> Router {
        let (_newest_tx, newest) = watch::channel(0);
        let (_stop_tx, stop) = watch::channel(false);
        router(AppState::new(Inner {
            home: home.to_owned(),
            port: PORT,
            allowed_hosts: vec!["board.example.com".into()],
            assets: Assets::of::<Fixture>(),
            newest,
            stop,
            pollers,
            peers: Peers::idle(),
            herdr: Cli::local(),
            terminal: None,
            started_at: Timestamp::now(),
            tools: Arc::new(Mutex::new(Value::Null)),
            damage: Damage::default(),
            orchestrators: starter,
            mirrors: Mirrors::new(home),
            sessions: auth::Sessions::default(),
            streams: sse::Streams::default(),
            ssh: vec!["ssh".into()],
            evals: memory::eval::Evals::default(),
            backups: crate::backups::BackupJobs::default(),
            retros,
        }))
    }

    /// A starter that accepts a path only when it is absolute, and reports
    /// two missing settings.
    struct FakeSettings;

    impl OrchestratorStart for FakeSettings {
        fn start(
            &self,
            _store: &mut Store,
            _ctx: &Ctx,
            _project: &str,
            _restart: bool,
        ) -> Result<Applied<Orchestrator>, StartError> {
            Err(StartError::Refused {
                exit: 2,
                code: "config",
                message: "spawn.pool_root is not set".into(),
                hint: None,
                current: Some(json!({"settings": [{"key": "spawn.pool_root"}]})),
            })
        }

        fn settings(&self) -> StartSettings {
            StartSettings {
                pool_root: None,
                checkouts_root: None,
                orchestrator_home: None,
                scratch_root: None,
                problems: vec![SettingProblem {
                    key: "spawn.pool_root".into(),
                    kind: "missing".into(),
                    problem: "spawn.pool_root is not set".into(),
                }],
                config_error: None,
            }
        }

        fn check_setting(&self, _key: &str, value: &str) -> Result<Vec<String>, String> {
            if value.starts_with('/') {
                Ok(vec!["a warning".into()])
            } else {
                Err(format!("{value} is not an absolute path"))
            }
        }
    }

    #[tokio::test]
    async fn the_board_reads_start_settings_and_sets_one_as_the_owner() {
        let fleet = Fleet::new();
        let app = app_restarting(fleet.dir.path(), Arc::new(FakeSettings));

        let read: Value =
            serde_json::from_str(&get(&app, "/api/v1/start-settings").await.body).unwrap();
        assert_eq!(read["problems"][0]["key"], "spawn.pool_root", "{read}");

        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let set = |key: &str, setting: &str, value: &str| {
            mutation(
                "POST",
                "/api/v1/config",
                json!({"idempotency_key": key, "key": setting, "value": value}),
                &owner,
            )
        };

        // Not the owner: refused, nothing written.
        let anon = mutation(
            "POST",
            "/api/v1/config",
            json!({"idempotency_key": "c-0", "key": "spawn.pool_root", "value": "/pool"}),
            &[],
        );
        let refused = send(&app, anon).await;
        assert!(
            refused.status == StatusCode::UNAUTHORIZED || refused.status == StatusCode::FORBIDDEN,
            "{} {}",
            refused.status,
            refused.body
        );
        assert!(!fleet.dir.path().join("config.toml").exists());

        // A key outside the settable set, an unknown project and a refused
        // value change nothing.
        for (key, setting, value, status) in [
            ("c-1", "owner.guard", "/x", StatusCode::BAD_REQUEST),
            ("c-2", "checkouts.nope", "/x", StatusCode::NOT_FOUND),
            (
                "c-3",
                "spawn.pool_root",
                "relative",
                StatusCode::BAD_REQUEST,
            ),
        ] {
            let answer = send(&app, set(key, setting, value)).await;
            assert_eq!(answer.status, status, "{setting}: {}", answer.body);
        }
        assert!(!fleet.dir.path().join("config.toml").exists());

        // A good value is written and recorded, with the warning handed on.
        let sent = send(&app, set("c-4", "spawn.pool_root", "/pool")).await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        let sent: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(sent["data"]["changed"], true, "{sent}");
        assert_eq!(sent["warning"], "a warning", "{sent}");
        let text = std::fs::read_to_string(fleet.dir.path().join("config.toml")).unwrap();
        assert!(text.contains("/pool"), "{text}");
        let events: i64 = fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'config.changed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events, 1);

        // The same value again records nothing.
        let again = send(&app, set("c-5", "spawn.pool_root", "/pool")).await;
        let again: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(again["data"]["changed"], false, "{again}");

        // A project's checkout is set where the project is edited.
        let checkout = send(&app, set("c-6", "checkouts.alpha", "/pool/alpha")).await;
        assert_eq!(checkout.status, StatusCode::OK, "{}", checkout.body);

        // A peer's settings are written on that peer, not forwarded.
        let remote = mutation(
            "POST",
            "/api/v1/config",
            json!({"idempotency_key": "c-7", "host": "elsewhere", "key": "spawn.pool_root", "value": "/pool"}),
            &owner,
        );
        let remote = send(&app, remote).await;
        assert_ne!(remote.status, StatusCode::OK, "{}", remote.body);
    }

    #[tokio::test]
    async fn the_board_sets_the_other_host_keys_one_at_a_time() {
        let fleet = Fleet::new();
        let app = app_restarting(fleet.dir.path(), Arc::new(FakeSettings));
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let config = fleet.dir.path().join("config.toml");
        let mut n = 0;
        let mut set = |setting: &str, value: &str| {
            n += 1;
            mutation(
                "POST",
                "/api/v1/config",
                json!({"idempotency_key": format!("h-{n}"), "key": setting, "value": value}),
                &owner,
            )
        };

        // The secret and every other key stay out, and nothing is written.
        for setting in [
            "owner_guard.decider_key",
            "owner_guard.endpoint",
            "intake_source",
            "learning",
        ] {
            let answer = send(&app, set(setting, "x")).await;
            assert_eq!(
                answer.status,
                StatusCode::BAD_REQUEST,
                "{setting}: {}",
                answer.body
            );
            assert!(answer.body.contains("not a setting"), "{}", answer.body);
        }
        assert!(!config.exists());

        // A value its reader would refuse changes nothing.
        for (setting, value) in [
            ("herdr.terminal", ""),
            ("stop_hook_mode", "block once"),
            ("federation.accept", "yes"),
            ("memory.remote", "http://example.com/memory.git"),
            ("learning.sample_every", "0"),
            ("learning.window_cards", "many"),
            ("learning.curator_model", "--help"),
            ("learning.curator_timeout_minutes", "0"),
            ("learning.evals_dir", "/nonexistent/evals"),
        ] {
            let answer = send(&app, set(setting, value)).await;
            assert_eq!(
                answer.status,
                StatusCode::BAD_REQUEST,
                "{setting}: {}",
                answer.body
            );
        }
        assert!(!config.exists());

        // block-once is refused by the switching rule, with the numbers.
        let refused = send(&app, set("stop_hook_mode", "block-once")).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );
        let refused: Value = serde_json::from_str(&refused.body).unwrap();
        assert_eq!(refused["error"]["code"], "refused", "{refused}");
        let current = &refused["error"]["current"];
        assert_eq!(current["stops"], 0, "{refused}");
        assert_eq!(current["min_stops"], 200, "{refused}");
        assert_eq!(current["max_false_block_rate"], 0.02, "{refused}");
        assert_eq!(current["rule_met"], false, "{refused}");
        assert!(
            current["since"].is_string() && current["until"].is_string(),
            "{refused}"
        );
        assert!(!config.exists());

        // Off and observe always save; a typed value lands as the TOML the
        // reader takes, and each change is one config event.
        for (setting, value) in [
            ("stop_hook_mode", "off"),
            ("stop_hook_mode", "observe"),
            ("herdr.terminal", "Ghostty"),
            ("federation.accept", "false"),
            ("memory.remote", "git@example.com:team/memory.git"),
            ("learning.sample_every", "3"),
            ("learning.window_cards", "12"),
            ("learning.curator_model", "opus"),
            ("learning.curator_timeout_minutes", "30"),
        ] {
            let sent = send(&app, set(setting, value)).await;
            assert_eq!(sent.status, StatusCode::OK, "{setting}: {}", sent.body);
        }
        let evals = tempfile::tempdir().unwrap();
        std::fs::create_dir(evals.path().join("evals")).unwrap();
        std::fs::write(evals.path().join("evals/run.sh"), "#!/bin/sh\n").unwrap();
        let dir = evals.path().to_str().unwrap();
        let sent = send(&app, set("learning.evals_dir", dir)).await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        let table: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
        assert_eq!(table["stop_hook_mode"].as_str(), Some("observe"));
        assert_eq!(table["herdr"]["terminal"].as_str(), Some("Ghostty"));
        assert_eq!(table["federation"]["accept"].as_bool(), Some(false));
        assert_eq!(table["learning"]["sample_every"].as_integer(), Some(3));
        assert_eq!(table["learning"]["window_cards"].as_integer(), Some(12));
        assert_eq!(table["learning"]["curator_model"].as_str(), Some("opus"));
        assert_eq!(
            table["learning"]["curator_timeout_minutes"].as_integer(),
            Some(30)
        );
        let events: Vec<(String, String)> = {
            let store = fleet.open();
            let mut stmt = store
                .conn()
                .prepare("SELECT entity_id, payload FROM events WHERE kind = 'config.changed' ORDER BY seq")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(events.len(), 10, "{events:?}");
        assert_eq!(events[0].0, "stop_hook_mode");
        assert!(events[0].1.contains("\"off\""), "{}", events[0].1);
        assert_eq!(events[3].0, "federation.accept");
        assert!(events[3].1.contains("false"), "{}", events[3].1);
        assert!(!events.iter().any(|(_, payload)| payload.contains("<set>")));

        // The same value again records nothing.
        let again = send(&app, set("learning.sample_every", "3")).await;
        let again: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(again["data"]["changed"], false, "{again}");

        // The read shows what was saved.
        let read: Value =
            serde_json::from_str(&get(&app, "/api/v1/host-config").await.body).unwrap();
        assert_eq!(read["stop_hook_mode"], "observe", "{read}");
        assert_eq!(read["herdr_terminal"], "Ghostty", "{read}");
        assert_eq!(read["accept_peers"], false, "{read}");
        assert_eq!(
            read["memory_remote"], "git@example.com:team/memory.git",
            "{read}"
        );
        assert_eq!(read["learning"]["sample_every"], 3, "{read}");
        assert_eq!(read["learning"]["curator_model"], "opus", "{read}");
        assert_eq!(read["learning"]["evals_dir"], dir, "{read}");
        assert_eq!(read["boundary"], "personal", "{read}");
        assert!(read["config_error"].is_null(), "{read}");
        assert!(!read.to_string().contains("decider"), "{read}");

        // A work host never pushes its memory repo, but may clear the remote.
        fleet
            .open()
            .conn()
            .execute("UPDATE meta SET value = 'work' WHERE key = 'boundary'", [])
            .unwrap();
        let pushed = send(&app, set("memory.remote", "git@example.com:team/other.git")).await;
        assert_eq!(pushed.status, StatusCode::BAD_REQUEST, "{}", pushed.body);
        assert!(pushed.body.contains("work host"), "{}", pushed.body);
        let cleared = send(&app, set("memory.remote", "")).await;
        assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
        let read: Value =
            serde_json::from_str(&get(&app, "/api/v1/host-config").await.body).unwrap();
        assert!(read["memory_remote"].is_null(), "{read}");
    }

    /// Services whose checks report one failed required check and the owner
    /// marker's case.
    struct FakeDoctor;

    impl OrchestratorStart for FakeDoctor {
        fn start(
            &self,
            _store: &mut Store,
            _ctx: &Ctx,
            _project: &str,
            _restart: bool,
        ) -> Result<Applied<Orchestrator>, StartError> {
            Err(StartError::Refused {
                exit: 5,
                code: "refused",
                message: "not here".into(),
                hint: None,
                current: None,
            })
        }

        fn doctor(&self) -> Option<DoctorReport> {
            Some(DoctorReport {
                ok: false,
                checks: vec![
                    DoctorCheck::missing("gh", true, "gh is not on PATH"),
                    DoctorCheck::new(
                        "owner marker",
                        true,
                        CheckStatus::Ok,
                        "tiller owns this host",
                    )
                    .code("tiller"),
                ],
                merge_gates: Vec::new(),
            })
        }
    }

    #[tokio::test]
    async fn the_board_reads_the_doctor_checks_of_its_own_host() {
        let fleet = Fleet::new();
        let app = app_restarting(fleet.dir.path(), Arc::new(FakeDoctor));
        let read = get(&app, "/api/v1/doctor").await;
        assert_eq!(read.status, StatusCode::OK, "{}", read.body);
        let read: Value = serde_json::from_str(&read.body).unwrap();
        assert_eq!(read["ok"], false, "{read}");
        assert_eq!(read["checks"][0]["name"], "gh", "{read}");
        assert_eq!(read["checks"][0]["status"], "fail", "{read}");
        assert_eq!(read["checks"][1]["code"], "tiller", "{read}");
        assert!(read["checks"][0].get("code").is_none(), "{read}");
        assert!(
            read["checked_at"]
                .as_str()
                .is_some_and(|at| at.ends_with('Z')),
            "{read}"
        );

        // A binary with no checks says so instead of an empty report.
        let bare = app_polling(fleet.dir.path(), None, Registry::new());
        let none = get(&bare, "/api/v1/doctor").await;
        assert_eq!(
            none.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{}",
            none.body
        );
    }

    /// A source that always fails, so it waits 5 s after its first poll.
    struct Failing;

    impl tiller_herdr::poller::Source for Failing {
        fn name(&self) -> String {
            "ci".to_owned()
        }
        fn interval(&self) -> std::time::Duration {
            std::time::Duration::from_secs(60)
        }
        fn poll(&mut self, _now: Timestamp) -> Result<(), tiller_herdr::poller::PollError> {
            Err(tiller_herdr::poller::PollError::Herdr(
                HerdrError::Unreachable("checks are not read".into()),
            ))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_now_polls_a_waiting_poller_at_once_for_the_owner_only() {
        let fleet = Fleet::new();
        let pollers = Registry::new();
        let poller = pollers.register(Failing).unwrap();
        let (stop_tx, stop) = watch::channel(false);
        let task = tokio::spawn(poller.run(stop));
        while pollers.health_of("ci").unwrap().consecutive_failures < 1 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let app = app_polling(
            fleet.dir.path(),
            Some(Arc::new(FakeDoctor)),
            pollers.clone(),
        );
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let run = |path: &str, key: &str, headers: &[(&str, &str)]| {
            mutation("POST", path, json!({"idempotency_key": key}), headers)
        };

        let anon = send(&app, run("/api/v1/pollers/ci/run", "p-0", &[])).await;
        assert!(
            anon.status == StatusCode::UNAUTHORIZED || anon.status == StatusCode::FORBIDDEN,
            "{} {}",
            anon.status,
            anon.body
        );
        assert_eq!(pollers.health_of("ci").unwrap().consecutive_failures, 1);

        let unknown = send(&app, run("/api/v1/pollers/nope/run", "p-1", &owner)).await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);

        // The first failure waits 5 s; Run now answers after the next poll.
        let started = std::time::Instant::now();
        let ran = send(&app, run("/api/v1/pollers/ci/run", "p-2", &owner)).await;
        assert_eq!(ran.status, StatusCode::OK, "{}", ran.body);
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
        let ran: Value = serde_json::from_str(&ran.body).unwrap();
        assert_eq!(ran["data"]["source"], "ci", "{ran}");
        assert_eq!(ran["data"]["consecutive_failures"], 2, "{ran}");
        assert!(
            ran["data"]["last_error"]
                .as_str()
                .unwrap()
                .contains("checks are not read")
        );
        assert!(ran.get("warning").is_none(), "{ran}");

        // Each host runs its own pollers.
        let remote = mutation(
            "POST",
            "/api/v1/pollers/ci/run",
            json!({"idempotency_key": "p-3", "host": "elsewhere"}),
            &owner,
        );
        assert_ne!(send(&app, remote).await.status, StatusCode::OK);

        stop_tx.send(true).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn the_board_starts_edits_and_ends_away_mode_as_the_owner() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let owner = [("cookie", cookie.as_str())];
        let until = "2099-11-12T18:00";
        let scope = |key: &str, extra: Value| {
            let mut body = json!({"idempotency_key": key, "until": until, "projects": ["alpha"]});
            for (name, value) in extra.as_object().unwrap() {
                body[name] = value.clone();
            }
            body
        };

        // Never away: the brief is null.
        let brief: Value =
            serde_json::from_str(&get(&app, "/api/v1/away/brief").await.body).unwrap();
        assert_eq!(brief["brief"], Value::Null, "{brief}");

        // Not the owner: refused, nothing written.
        let anon = mutation("POST", "/api/v1/away", scope("a-0", json!({})), &[]);
        let refused = send(&app, anon).await;
        assert!(
            refused.status == StatusCode::UNAUTHORIZED || refused.status == StatusCode::FORBIDDEN,
            "{} {}",
            refused.status,
            refused.body
        );

        // A scope the CLI refuses is refused here too: a past time, no
        // projects, a floor above 100, an unknown merge mode.
        for (key, extra) in [
            ("a-1", json!({"until": "2020-01-01T09:00"})),
            ("a-2", json!({"projects": []})),
            ("a-3", json!({"quota_floor_percent": 120.0})),
            ("a-4", json!({"merge": "always"})),
            ("a-5", json!({"until": "next tuesday"})),
        ] {
            let answer = send(
                &app,
                mutation("POST", "/api/v1/away", scope(key, extra), &owner),
            )
            .await;
            assert_eq!(
                answer.status,
                StatusCode::BAD_REQUEST,
                "{key}: {}",
                answer.body
            );
        }

        // Starting writes the contract, with the CLI's scope shape.
        let started = send(
            &app,
            mutation(
                "POST",
                "/api/v1/away",
                scope(
                    "a-6",
                    json!({"orders": "keep going", "max_new_runs": 6, "quota_floor_percent": 20.0, "merge": "none"}),
                ),
                &owner,
            ),
        )
        .await;
        assert_eq!(started.status, StatusCode::OK, "{}", started.body);
        let started: Value = serde_json::from_str(&started.body).unwrap();
        assert_eq!(started["data"]["kind"], "away", "{started}");
        assert_eq!(
            started["data"]["scope"]["spend"]["max_new_runs"], 6,
            "{started}"
        );
        assert_eq!(started["data"]["scope"]["merge"], "none", "{started}");
        assert_eq!(
            started["data"]["scope"]["until_at"], "2099-11-12T18:00:00.000Z",
            "{started}"
        );

        // The same key replays; a second start is refused.
        let replay = send(&app, mutation("POST", "/api/v1/away", scope("a-6", json!({"orders": "keep going", "max_new_runs": 6, "quota_floor_percent": 20.0, "merge": "none"})), &owner)).await;
        let replay: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(replay["replayed"], true, "{replay}");
        let twice = send(
            &app,
            mutation("POST", "/api/v1/away", scope("a-7", json!({})), &owner),
        )
        .await;
        assert_eq!(
            twice.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            twice.body
        );

        // The brief reads the running contract.
        let brief: Value =
            serde_json::from_str(&get(&app, "/api/v1/away/brief").await.body).unwrap();
        assert_eq!(brief["brief"]["away"], true, "{brief}");
        assert_eq!(brief["brief"]["orders"], "keep going", "{brief}");
        assert_eq!(brief["brief"]["merge"], "none", "{brief}");
        assert_eq!(brief["brief"]["spend"]["max_new_runs"], 6, "{brief}");

        // Editing changes the scope in place: the same contract, new limits.
        let edited = send(
            &app,
            mutation(
                "PUT",
                "/api/v1/away",
                scope(
                    "a-8",
                    json!({"orders": "slow down", "max_new_runs": 2, "merge": "grants"}),
                ),
                &owner,
            ),
        )
        .await;
        assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);
        let edited: Value = serde_json::from_str(&edited.body).unwrap();
        assert_eq!(edited["data"]["id"], started["data"]["id"], "{edited}");
        let brief: Value =
            serde_json::from_str(&get(&app, "/api/v1/away/brief").await.body).unwrap();
        assert_eq!(brief["brief"]["orders"], "slow down", "{brief}");
        assert_eq!(brief["brief"]["merge"], "grants", "{brief}");
        assert_eq!(
            brief["brief"]["approval_id"], started["data"]["id"],
            "{brief}"
        );

        // Ending turns it off and hands back the brief.
        let ended = send(
            &app,
            mutation(
                "DELETE",
                "/api/v1/away",
                json!({"idempotency_key": "a-9"}),
                &owner,
            ),
        )
        .await;
        assert_eq!(ended.status, StatusCode::OK, "{}", ended.body);
        let ended: Value = serde_json::from_str(&ended.body).unwrap();
        assert_eq!(ended["data"]["away"]["state"], "succeeded", "{ended}");
        assert_eq!(ended["data"]["brief"]["away"], false, "{ended}");
        assert_eq!(
            ended["data"]["brief"]["ended_reason"], "ended by the owner",
            "{ended}"
        );

        // Ending again is a no-op, and editing a contract that is over is refused.
        let again = send(
            &app,
            mutation(
                "DELETE",
                "/api/v1/away",
                json!({"idempotency_key": "a-10"}),
                &owner,
            ),
        )
        .await;
        let again: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(again["data"]["away"], Value::Null, "{again}");
        let late = send(
            &app,
            mutation("PUT", "/api/v1/away", scope("a-11", json!({})), &owner),
        )
        .await;
        assert_eq!(late.status, StatusCode::BAD_REQUEST, "{}", late.body);

        // The ended contract's brief stays readable.
        let brief: Value =
            serde_json::from_str(&get(&app, "/api/v1/away/brief").await.body).unwrap();
        assert_eq!(brief["brief"]["away"], false, "{brief}");
    }

    #[tokio::test]
    async fn a_refused_start_hands_on_the_settings_it_lists() {
        let fleet = Fleet::new();
        let app = app_restarting(fleet.dir.path(), Arc::new(FakeSettings));
        let cookie = owner_cookie(fleet.dir.path());
        let started = send(
            &app,
            mutation(
                "POST",
                "/api/v1/projects/alpha/orchestrator",
                json!({"idempotency_key": "s-1"}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        assert_eq!(started.status, StatusCode::BAD_REQUEST, "{}", started.body);
        let body: Value = serde_json::from_str(&started.body).unwrap();
        assert_eq!(
            body["error"]["current"]["settings"][0]["key"], "spawn.pool_root",
            "{body}"
        );
    }

    fn stale_of(read: &Value, session: &str) -> Value {
        let row = &read["orchestrators"][0];
        if row["session_id"] == session {
            return row["stale"].clone();
        }
        row["workers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|worker| worker["session_id"] == session)
            .unwrap()["stale"]
            .clone()
    }

    #[tokio::test]
    async fn stale_sessions_are_listed_restarted_when_idle_and_queued_when_working() {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w3:p1");
        let orchestrator = fleet.orchestrator_session.clone();
        let store = fleet.open();
        // The fleet's sessions were launched before launches were recorded.
        // The worker is idle and the orchestrator at work.
        for (id, activity) in [(&worker, "idle"), (&orchestrator, "working")] {
            store
                .conn()
                .execute(
                    "UPDATE sessions SET activity = ?2,
                            activity_observed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE id = ?1",
                    rusqlite::params![id, activity],
                )
                .unwrap();
        }
        let starter = Arc::new(FakeRestarts::default());
        let app = app_restarting(fleet.dir.path(), starter.clone());

        let read: Value = serde_json::from_str(&get(&app, "/api/v1/agents").await.body).unwrap();
        assert_eq!(read["tiller_version"], env!("CARGO_PKG_VERSION"));
        for id in [&worker, &orchestrator] {
            let stale = stale_of(&read, id);
            assert_eq!(stale["since"], "unknown", "{read}");
            assert_eq!(stale["restart_queued"], false);
        }

        // Restart: the idle worker restarts now, the working orchestrator is
        // queued.
        let cookie = owner_cookie(fleet.dir.path());
        let restart = send(
            &app,
            mutation(
                "POST",
                "/api/v1/agents/restart-stale",
                json!({"idempotency_key": "k-1", "sessions": [&worker, &orchestrator]}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        assert_eq!(restart.status, StatusCode::OK, "{}", restart.body);
        let restart: Value = serde_json::from_str(&restart.body).unwrap();
        let outcome = |id: &str| {
            restart["data"]["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["session_id"] == id)
                .unwrap()["outcome"]
                .clone()
        };
        assert_eq!(outcome(&worker), "restarted");
        assert_eq!(outcome(&orchestrator), "queued");
        assert_eq!(*starter.0.lock().unwrap(), vec![worker.clone()]);

        let read: Value = serde_json::from_str(&get(&app, "/api/v1/agents").await.body).unwrap();
        assert_eq!(stale_of(&read, &worker), Value::Null);
        assert_eq!(stale_of(&read, &orchestrator)["restart_queued"], true);

        // The orchestrator goes idle: the daemon's pass restarts it.
        let mut store = fleet.open();
        store
            .conn()
            .execute(
                "UPDATE sessions SET activity = 'idle' WHERE id = ?1",
                [&orchestrator],
            )
            .unwrap();
        assert_eq!(
            crate::stale::drain(&mut store, starter.as_ref()).unwrap(),
            1
        );
        assert_eq!(starter.0.lock().unwrap().len(), 2);
        let read: Value = serde_json::from_str(&get(&app, "/api/v1/agents").await.body).unwrap();
        assert_eq!(stale_of(&read, &orchestrator), Value::Null);

        // T6: the click and the drain are one board action. The queue and
        // both restarts name the board, never the owner as well.
        assert_eq!(*starter.1.lock().unwrap(), ["board", "board"]);
        let queued: String = fleet
            .open()
            .conn()
            .query_row(
                "SELECT actor FROM events WHERE kind = 'session.restart_queued'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(queued, "board");
    }

    /// A herdr whose one Claude pane, `w2:p1`, shows the agent status in
    /// `{dir}/status`; it logs each call.
    fn agent_herdr(dir: &std::path::Path) -> Cli {
        let script = format!(
            r#"echo "$*" >> '{dir}/calls'
case "$*" in
*"pane get"*)
  printf '{{"result":{{"pane":{{"pane_id":"w2:p1","workspace_id":"w2","agent":"claude","agent_status":"%s"}}}}}}\n' "$(cat '{dir}/status')";;
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

    fn agent_app(fleet: &Fleet, dir: &std::path::Path) -> Router {
        app_full(
            fleet.dir.path(),
            Assets::of::<Fixture>(),
            Damage::default(),
            Peers::idle(),
            agent_herdr(dir),
            None,
        )
    }

    fn herdr_calls(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The message bar's steer is `tiller send --kind steer`: one queued
    /// outbox line for the session, on its run's card, keyed by the board.
    #[tokio::test]
    async fn a_steer_queues_one_line_for_the_agent() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let app = agent_app(&fleet, scratch.path());
        let cookie = owner_cookie(fleet.dir.path());
        let path = format!("/api/v1/agents/{session}/steer");
        let body = json!({"idempotency_key": "k-steer", "text": "Stop after the lint fix"});

        let refused = send(&app, mutation("POST", &path, body.clone(), &[])).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
        for (text, why) in [
            ("", "is empty"),
            ("   ", "is empty"),
            ("one\ntwo", "one line"),
            (&"a".repeat(COMMENT_MAX_CHARS + 1), "at most 20000"),
        ] {
            let bad = json!({"idempotency_key": "k-bad", "text": text});
            let answer = send(&app, mutation("POST", &path, bad, &[("cookie", &cookie)])).await;
            assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
            assert!(answer.body.contains(why), "{}", answer.body);
        }

        let sent = send(
            &app,
            mutation("POST", &path, body.clone(), &[("cookie", &cookie)]),
        )
        .await;
        assert_eq!(sent.status, StatusCode::OK, "{}", sent.body);
        let sent: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(sent["data"]["kind"], "steer");
        assert_eq!(sent["data"]["state"], "pending");
        assert_eq!(sent["data"]["body"], "Stop after the lint fix");
        assert_eq!(sent["receipt"]["outbox_id"], sent["data"]["id"]);
        let (count, by, card): (i64, String, Option<String>) = fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*), max(created_by), max(card_id) FROM outbox WHERE target_session_id = ?1",
                [&session],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((count, by.as_str()), (1, "board"));
        let run_card: String = fleet
            .open()
            .conn()
            .query_row(
                "SELECT card_id FROM runs WHERE id = (SELECT run_id FROM sessions WHERE id = ?1)",
                [&session],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(card, Some(run_card));
        // Nothing is typed now: the daemon's delivery loop types it when
        // the pane is idle.
        assert!(herdr_calls(scratch.path()).is_empty());

        // A retry replays; a second line is a second message.
        let again = send(&app, mutation("POST", &path, body, &[("cookie", &cookie)])).await;
        let again: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(again["replayed"], true);
        assert_eq!(again["data"]["id"], sent["data"]["id"]);
        let next = json!({"idempotency_key": "k-steer-2", "text": "Then open the PR"});
        let next = send(&app, mutation("POST", &path, next, &[("cookie", &cookie)])).await;
        assert_eq!(next.status, StatusCode::OK, "{}", next.body);
        let count: i64 = fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*) FROM outbox WHERE target_session_id = ?1",
                [&session],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    /// The message bar's Interrupt is `tiller agent interrupt` on the
    /// session's run: refused while the agent is not working, else one
    /// Escape and the checked result. Its key never sends a second one.
    #[tokio::test]
    async fn an_interrupt_sends_one_escape_to_a_working_agent() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().to_owned();
        let app = agent_app(&fleet, &dir);
        let cookie = owner_cookie(fleet.dir.path());
        let path = format!("/api/v1/agents/{session}/interrupt");
        let body = json!({"idempotency_key": "k-int"});

        let refused = send(&app, mutation("POST", &path, body.clone(), &[])).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);

        std::fs::write(dir.join("status"), "idle").unwrap();
        let idle = send(
            &app,
            mutation("POST", &path, body.clone(), &[("cookie", &cookie)]),
        )
        .await;
        assert_eq!(
            idle.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            idle.body
        );
        assert!(idle.body.contains("is not working"), "{}", idle.body);
        assert!(!herdr_calls(&dir).iter().any(|c| c.contains("send-keys")));

        // The turn ends after the Escape, as Claude's Stop hook reports.
        std::fs::write(dir.join("status"), "working").unwrap();
        let mut hook_store = fleet.open();
        let (watch, hooked) = (dir.clone(), session.clone());
        let claude = std::thread::spawn(move || {
            for _ in 0..500 {
                if herdr_calls(&watch)
                    .iter()
                    .any(|c| c == "pane send-keys w2:p1 esc")
                {
                    hook_store
                        .observation_record(
                            &Ctx::new(Actor::Hook),
                            tiller_core::state::runs::NewObservation {
                                session_id: Some(hooked),
                                run_id: None,
                                source: tiller_core::domain::ObservationSource::Hook,
                                event: "Stop".into(),
                                verdict: None,
                                observed_at: Timestamp::now(),
                                payload: json!({}),
                            },
                        )
                        .unwrap();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let done = send(
            &app,
            mutation("POST", &path, body.clone(), &[("cookie", &cookie)]),
        )
        .await;
        claude.join().unwrap();
        assert_eq!(done.status, StatusCode::OK, "{}", done.body);
        let done: Value = serde_json::from_str(&done.body).unwrap();
        assert_eq!(done["data"]["cancel"], "confirmed", "{done}");
        assert_eq!(done["data"]["session"], session);
        assert!(done["data"]["evidence"].as_str().unwrap().contains("Stop"));

        // A retry of the same press finds the agent still working, and
        // sends nothing.
        let retry = send(&app, mutation("POST", &path, body, &[("cookie", &cookie)])).await;
        assert_eq!(
            retry.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            retry.body
        );
        assert!(retry.body.contains("sent already"), "{}", retry.body);
        let escapes = herdr_calls(&dir)
            .iter()
            .filter(|c| c.contains("send-keys"))
            .count();
        assert_eq!(escapes, 1);
    }

    #[tokio::test]
    async fn an_interrupt_needs_a_run_on_this_machine() {
        let mut fleet = Fleet::new();
        let remote = fleet.worker("w3:p1");
        fleet
            .open()
            .conn()
            .execute_batch(&format!(
                "INSERT INTO hosts (id, label, kind, herdr_machine) VALUES ('h-r', 'remote-a', 'remote', 'm-1');
                 UPDATE sessions SET host_id = 'h-r' WHERE id = '{remote}'"
            ))
            .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        std::fs::write(scratch.path().join("status"), "working").unwrap();
        let app = agent_app(&fleet, scratch.path());
        let cookie = owner_cookie(fleet.dir.path());
        let cases = [
            (remote.clone(), "saved machine m-1"),
            (fleet.orchestrator_session.clone(), "no run to interrupt"),
        ];
        for (session, why) in cases {
            let path = format!("/api/v1/agents/{session}/interrupt");
            let body = json!({"idempotency_key": format!("k-{session}")});
            let answer = send(&app, mutation("POST", &path, body, &[("cookie", &cookie)])).await;
            assert_eq!(
                answer.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{}",
                answer.body
            );
            assert!(answer.body.contains(why), "{}", answer.body);
        }
        assert!(
            !herdr_calls(scratch.path())
                .iter()
                .any(|c| c.contains("send-keys"))
        );
    }

    #[tokio::test]
    async fn restarting_stale_sessions_needs_the_owner() {
        let fleet = Fleet::new();
        let app = app_restarting(fleet.dir.path(), Arc::new(FakeRestarts::default()));
        let refused = send(
            &app,
            mutation(
                "POST",
                "/api/v1/agents/restart-stale",
                json!({"idempotency_key": "k-1"}),
                &[],
            ),
        )
        .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    }

    #[tokio::test]
    async fn a_remote_agent_gets_a_tail_and_no_terminal_mirror() {
        let mut fleet = Fleet::new();
        let remote = fleet.worker("w3:p1");
        fleet
            .open()
            .conn()
            .execute_batch(&format!(
                "INSERT INTO hosts (id, label, kind, herdr_machine) VALUES ('h-r', 'remote-a', 'remote', 'm-1');
                 UPDATE sessions SET host_id = 'h-r' WHERE id = '{remote}'"
            ))
            .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let app = mirror_app(&fleet, scratch.path());
        let cookie = owner_cookie(fleet.dir.path());
        for (path, body) in [
            (format!("/api/v1/agents/{remote}/terminal"), json!({})),
            (
                format!("/api/v1/agents/{remote}/terminal/input"),
                json!({"keys": ["a"]}),
            ),
            (
                format!("/api/v1/agents/{remote}/terminal/fit"),
                json!({"cols": 100, "rows": 30}),
            ),
            (
                format!("/api/v1/agents/{remote}/terminal/history"),
                json!({}),
            ),
            (format!("/api/v1/agents/{remote}/prompt"), json!({})),
            (
                format!("/api/v1/agents/{remote}/prompt/answer"),
                json!({"option": 1, "screen": "x"}),
            ),
        ] {
            let answer = send(&app, mutation("POST", &path, body, &[("cookie", &cookie)])).await;
            assert_eq!(
                answer.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{}",
                answer.body
            );
            assert!(answer.body.contains("\"refused\""), "{}", answer.body);
        }
        assert!(!scratch.path().join("calls").exists());

        // Its tail is read through the saved machine instead: the pane's
        // size, then its visible screen, colors kept.
        let tail = format!("/api/v1/agents/{remote}/terminal/tail");
        let read = send(
            &app,
            mutation("POST", &tail, json!({}), &[("cookie", &cookie)]),
        )
        .await;
        assert_eq!(read.status, StatusCode::OK, "{}", read.body);
        let read: Value = serde_json::from_str(&read.body).unwrap();
        assert_eq!(read["session_id"], remote);
        assert_eq!(
            (read["cols"].clone(), read["rows"].clone()),
            (json!(80), json!(24))
        );
        assert_eq!(read["text"], "one\n\u{1b}[34mtwo\u{1b}[0m\n");
        let calls = std::fs::read_to_string(scratch.path().join("calls")).unwrap();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            [
                "--machine m-1 pane layout --pane w3:p1",
                "--machine m-1 pane read w3:p1 --source visible --format ansi"
            ]
        );
        // A closed pane is a 404.
        std::fs::write(scratch.path().join("gone"), "").unwrap();
        let closed = send(
            &app,
            mutation("POST", &tail, json!({}), &[("cookie", &cookie)]),
        )
        .await;
        assert_eq!(closed.status, StatusCode::NOT_FOUND, "{}", closed.body);
    }

    #[tokio::test]
    async fn the_observe_process_ends_with_its_viewer_and_restarts_on_a_resize() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = mirror_app(&fleet, dir);
        let cookie = owner_cookie(fleet.dir.path());
        let observers = || {
            std::fs::read_to_string(dir.join("observers"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let response = app
            .clone()
            .oneshot(mutation(
                "POST",
                &format!("/api/v1/agents/{session}/terminal"),
                json!({}),
                &[("cookie", &cookie)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/x-ndjson"
        );
        let mut body = response.into_body().into_data_stream();
        let mut pending = String::new();
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 80, "rows": 24})
        );
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "frame", "seq": 1, "full": true, "bytes": "aGk="})
        );
        let first = observers();
        assert_eq!(first.len(), 1);
        assert!(alive(&first[0]));
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(
            calls.contains("terminal session observe w2:p1 --cols 80 --rows 24"),
            "{calls}"
        );

        // The pane is resized: the old observe is killed, a new one runs at
        // the new size and starts with a full frame.
        std::fs::write(dir.join("size"), "120 36\n").unwrap();
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 120, "rows": 36})
        );
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        assert!(gone(&first[0]).await, "the first observe outlived its size");
        let second = observers();
        assert_eq!(second.len(), 2);
        assert!(alive(&second[1]));

        // The viewer leaves: its observe is killed.
        drop(body);
        assert!(gone(&second[1]).await, "the observe outlived its viewer");
    }

    /// At an allowed host, signing out ends an open mirror of the pane, not
    /// only the next one.
    #[tokio::test]
    async fn signing_out_ends_an_open_terminal_mirror() {
        use futures_util::StreamExt;
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = mirror_app(&fleet, dir);
        let host = "board.example.com";
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [
            ("host", host),
            ("origin", "https://board.example.com"),
            ("cookie", cookie.as_str()),
        ];
        let open = mutation(
            "POST",
            &format!("/api/v1/agents/{session}/terminal"),
            json!({}),
            &headers,
        );
        let response = app.clone().oneshot(open).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut pending = String::new();
        assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        let observer = std::fs::read_to_string(dir.join("observers")).unwrap();

        let out = sign_out(&app, host, &cookie).await;
        assert_eq!(out.status, StatusCode::OK, "{}", out.body);
        let ended = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(chunk) = body.next().await {
                chunk.unwrap();
            }
        })
        .await;
        assert!(ended.is_ok(), "the mirror outlived the sign-out");
        assert!(
            gone(observer.trim()).await,
            "the observe outlived the sign-out"
        );
    }

    /// herdr lays its tabs out at 120 by 40 with no client attached, while
    /// each pane's PTY keeps the size the last client gave it. The mirror
    /// draws at the PTY's size, so no row of the agent's screen is cut off,
    /// and a Fit gives the pane back at it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_mirror_draws_at_the_panes_pty_size_not_herdrs_headless_layout() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = mirror_app(&fleet, dir);
        std::fs::write(dir.join("size"), "120 40\n").unwrap();
        let pty = tiller_herdr::testing::Pty::new(120, 43);
        std::fs::write(dir.join("shell"), pty.shell_pid().to_string()).unwrap();
        let cookie = owner_cookie(fleet.dir.path());
        let fits = fleet.dir.path().join("mirror-fits.json");
        let held = || std::fs::read_to_string(&fits).ok();
        let open = |request: Value| {
            mutation(
                "POST",
                &format!("/api/v1/agents/{session}/terminal"),
                request,
                &[("cookie", &cookie)],
            )
        };

        let response = app.clone().oneshot(open(json!({}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut pending = String::new();
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 120, "rows": 43})
        );
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(
            calls.contains("terminal session observe w2:p1 --cols 120 --rows 43"),
            "{calls}"
        );
        // A herdr client resizes the PTY and the layout stays: the mirror
        // follows the PTY.
        pty.resize(150, 50);
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 150, "rows": 50})
        );
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        drop(body);

        let response = app
            .clone()
            .oneshot(open(json!({"fit": {"cols": 100, "rows": 30}})))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut pending = String::new();
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 100, "rows": 30})
        );
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        assert_eq!(held().as_deref(), Some(r#"{"w2:p1":[150,50]}"#));
        // A size read passes with the layout where it was: the size to give
        // back is still the PTY's.
        let reads = || {
            std::fs::read_to_string(dir.join("calls"))
                .unwrap()
                .matches("pane layout --pane w2:p1")
                .count()
        };
        let (before, deadline) = (
            reads(),
            std::time::Instant::now() + std::time::Duration::from_secs(10),
        );
        while reads() == before {
            assert!(std::time::Instant::now() < deadline, "no size read came");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(held().as_deref(), Some(r#"{"w2:p1":[150,50]}"#));
        let control = std::fs::read_to_string(dir.join("controllers")).unwrap();
        drop(body);
        assert!(
            gone(control.trim()).await,
            "the control outlived its viewer"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("control-in")).unwrap(),
            "{\"type\":\"terminal.resize\",\"cols\":150,\"rows\":50}\n{\"type\":\"terminal.release\"}\n"
        );
    }

    #[tokio::test]
    async fn an_observe_that_never_sends_a_full_frame_gives_up() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        std::fs::write(dir.join("full"), "false\n").unwrap();
        let app = mirror_app(&fleet, dir);
        let cookie = owner_cookie(fleet.dir.path());
        let response = app
            .oneshot(mutation(
                "POST",
                &format!("/api/v1/agents/{session}/terminal"),
                json!({}),
                &[("cookie", &cookie)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut pending = String::new();
        // Each try says the size, and none sends a frame on.
        for _ in 0..3 {
            assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");
        }
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "unavailable", "message": "herdr observe never sent a full frame"})
        );
        let observers = std::fs::read_to_string(dir.join("observers")).unwrap();
        let observers: Vec<_> = observers.lines().collect();
        assert_eq!(observers.len(), 3);
        for pid in observers {
            assert!(gone(pid).await, "an observe outlived its try");
        }
    }

    /// Waits up to 6 seconds for `ready` to hold.
    async fn until(mut ready: impl FnMut() -> bool) -> bool {
        for _ in 0..120 {
            if ready() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_fit_resizes_the_pane_and_gives_it_back_when_the_viewer_leaves() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let app = mirror_app(&fleet, dir);
        let cookie = owner_cookie(fleet.dir.path());
        let fits = fleet.dir.path().join("mirror-fits.json");
        let held = || std::fs::read_to_string(&fits).ok();
        let response = app
            .clone()
            .oneshot(mutation(
                "POST",
                &format!("/api/v1/agents/{session}/terminal"),
                json!({"fit": {"cols": 100, "rows": 30}}),
                &[("cookie", &cookie)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut pending = String::new();
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 100, "rows": 30})
        );
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(
            calls.contains("terminal session control w2:p1 --cols 100 --rows 30"),
            "{calls}"
        );
        assert!(!calls.contains("--takeover"), "{calls}");
        // The size to give back is on disk while herdr holds the Fit.
        assert_eq!(held().as_deref(), Some(r#"{"w2:p1":[80,24]}"#));

        // The board changes size: the same control resizes the pane in
        // place, and the stream says the new size before its full frame.
        let refit = format!("/api/v1/agents/{session}/terminal/fit");
        let refitted = send(
            &app,
            mutation(
                "POST",
                &refit,
                json!({"cols": 110, "rows": 32}),
                &[("cookie", &cookie)],
            ),
        )
        .await;
        assert_eq!(refitted.status, StatusCode::OK, "{}", refitted.body);
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 110, "rows": 32})
        );
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        let control = std::fs::read_to_string(dir.join("controllers")).unwrap();
        assert_eq!(control.lines().count(), 1, "the refit ran a second control");

        // The owner changes herdr's layout meanwhile: the pane goes back to
        // the new size, and the Fit holds on.
        std::fs::write(dir.join("size"), "90 28\n").unwrap();
        assert!(until(|| held().as_deref() == Some(r#"{"w2:p1":[90,28]}"#)).await);

        drop(body);
        assert!(
            gone(control.trim()).await,
            "the control outlived its viewer"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("control-in")).unwrap(),
            "{\"type\":\"terminal.resize\",\"cols\":110,\"rows\":32}\n\
             {\"type\":\"terminal.resize\",\"cols\":90,\"rows\":28}\n{\"type\":\"terminal.release\"}\n"
        );
        assert!(until(|| held().is_none()).await, "the Fit was not let go");

        // With the stream gone no Fit holds the pane, so a refit is refused.
        let late = send(
            &app,
            mutation(
                "POST",
                &refit,
                json!({"cols": 110, "rows": 32}),
                &[("cookie", &cookie)],
            ),
        )
        .await;
        assert_eq!(
            late.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            late.body
        );
    }

    #[tokio::test]
    async fn a_fit_herdr_refuses_goes_on_at_the_panes_own_size() {
        let mut fleet = Fleet::new();
        let session = fleet.worker("w2:p1");
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        std::fs::write(dir.join("refuse"), "").unwrap();
        let app = mirror_app(&fleet, dir);
        let cookie = owner_cookie(fleet.dir.path());
        let path = format!("/api/v1/agents/{session}/terminal");
        let tiny = send(
            &app,
            mutation(
                "POST",
                &path,
                json!({"fit": {"cols": 10, "rows": 30}}),
                &[("cookie", &cookie)],
            ),
        )
        .await;
        assert_eq!(tiny.status, StatusCode::BAD_REQUEST, "{}", tiny.body);
        let response = app
            .oneshot(mutation(
                "POST",
                &path,
                json!({"fit": {"cols": 100, "rows": 30}}),
                &[("cookie", &cookie)],
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut pending = String::new();
        assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");
        let unfit = next_line(&mut body, &mut pending).await;
        assert_eq!(unfit["type"], "unfit");
        assert!(
            unfit["message"]
                .as_str()
                .unwrap()
                .contains("already has an attached client"),
            "{unfit}"
        );
        assert_eq!(
            next_line(&mut body, &mut pending).await,
            json!({"type": "size", "cols": 80, "rows": 24})
        );
        assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
        assert!(!fleet.dir.path().join("mirror-fits.json").exists());
    }

    #[tokio::test]
    async fn a_daemon_that_stopped_mid_fit_gives_the_pane_back_when_it_starts() {
        let home = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let fits = home.path().join("mirror-fits.json");
        std::fs::write(&fits, r#"{"w2:p1":[80,24]}"#).unwrap();
        let mirrors = Mirrors::new(home.path());
        let herdr = mirror_herdr(dir);
        // herdr's layout is not the pane's size (it lays tabs out at 120 by
        // 40 with no client attached): the pane goes back to the size it had
        // before the Fit.
        std::fs::write(dir.join("size"), "120 40\n").unwrap();
        mirrors.fits().restore_left(&herdr).await;
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(
            calls.contains("terminal session control w2:p1 --cols 80 --rows 24"),
            "{calls}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("control-in")).unwrap(),
            "{\"type\":\"terminal.resize\",\"cols\":80,\"rows\":24}\n{\"type\":\"terminal.release\"}\n"
        );
        assert!(!fits.exists());
        assert!(mirrors.fits().held().is_empty());

        // A pane herdr no longer has is dropped with nothing to give back.
        std::fs::write(&fits, r#"{"w2:p1":[80,24]}"#).unwrap();
        std::fs::write(dir.join("gone"), "").unwrap();
        let mirrors = Mirrors::new(home.path());
        mirrors.fits().restore_left(&herdr).await;
        let after = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert_eq!(
            after.matches("terminal session control").count(),
            1,
            "{after}"
        );
        assert!(!fits.exists());
    }

    #[test]
    fn pane_tail_drops_escapes_and_trailing_blank_lines() {
        let screen = "\u{1b}[1mbold\u{1b}[0m line\n\u{1b}]0;title\u{7}second\n\n   \n";
        assert_eq!(pane_tail(screen, PANE_TAIL), vec!["bold line", "second"]);
        assert_eq!(pane_tail(screen, 1), vec!["second"]);
    }

    /// The owner picks this host's bug project on the board; a Tiller bug
    /// then routes there, and its row and timeline say where it came from.
    #[tokio::test]
    async fn a_bug_project_pick_routes_tiller_bugs_and_the_board_shows_it() {
        let mut fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let machine = |app: Router| async move {
            let answer = get(&app, "/api/v1/machines").await;
            let body: Value = serde_json::from_str(&answer.body).unwrap();
            body["machines"][0].clone()
        };
        let pick = |key: &str, project: Value, rev: &Value| {
            mutation(
                "POST",
                "/api/v1/bug-project",
                json!({"idempotency_key": key, "project": project, "rev": rev}),
                &headers,
            )
        };

        let before = machine(app.clone()).await;
        assert!(before.get("bug_project").is_none(), "{before}");
        let refused = send(&app, pick("k-fleet", json!("fleet"), &before["rev"])).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );
        let picked = send(&app, pick("k-alpha", json!("alpha"), &before["rev"])).await;
        assert_eq!(picked.status, StatusCode::OK, "{}", picked.body);
        let after = machine(app.clone()).await;
        assert_eq!(after["bug_project"], "alpha");

        let filed = fleet
            .store
            .bug_file(
                &Ctx::new(Actor::Session(fleet.orchestrator_session.clone())),
                None,
                tiller_core::state::cards::NewBug {
                    title: "list exits 3".into(),
                    body: "seen".into(),
                    evidence: None,
                },
            )
            .unwrap()
            .value
            .card;
        let board = get(&app, "/api/v1/board?filter=all").await;
        let body: Value = serde_json::from_str(&board.body).unwrap();
        let row = body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == filed.id.as_str())
            .unwrap()
            .clone();
        assert_eq!(row["project_id"], "alpha");
        assert_eq!(row["category"], "tiller-bug");
        assert_eq!(row["filed_from"], "alpha");
        let timeline = get(&app, &format!("/api/v1/cards/{}/timeline", filed.id)).await;
        assert!(
            timeline
                .body
                .contains("filed as a Tiller bug from alpha into alpha"),
            "{}",
            timeline.body
        );

        // A board create carries its category, as Move sends it.
        let created = send(
            &app,
            mutation(
                "POST",
                "/api/v1/cards",
                json!({"idempotency_key": "k-move", "project": "alpha", "kind": "ship",
                       "title": "moved", "category": "tiller-bug"}),
                &headers,
            ),
        )
        .await;
        assert_eq!(created.status, StatusCode::OK, "{}", created.body);
        let category: Option<String> = fleet
            .open()
            .conn()
            .query_row(
                "SELECT category FROM cards WHERE title = 'moved'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(category.as_deref(), Some("tiller-bug"));

        // Clearing it sends bugs back to fleet.
        let cleared = send(&app, pick("k-clear", Value::Null, &after["rev"])).await;
        assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
        assert!(machine(app.clone()).await.get("bug_project").is_none());
    }

    /// Settings > Backups lists this host's backups for the owner alone, and
    /// Back up now takes a verified copy as `tiller backup` does, lists it,
    /// and refuses a peer label.
    #[tokio::test]
    async fn the_board_lists_and_takes_backups_for_the_owner_only() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let list = |app: Router, cookie: Option<String>| async move {
            let mut request = Request::get("/api/v1/backups")
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap();
            if let Some(cookie) = cookie {
                request
                    .headers_mut()
                    .insert(header::COOKIE, cookie.parse().unwrap());
            }
            send(&app, request).await
        };

        // The list is the owner's, and a stranger's click is refused too.
        assert_eq!(list(app.clone(), None).await.status, StatusCode::FORBIDDEN);
        let refused = send(
            &app,
            mutation(
                "POST",
                "/api/v1/backups",
                json!({"idempotency_key": "k-no"}),
                &[],
            ),
        )
        .await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);

        let empty = list(app.clone(), Some(cookie.clone())).await;
        assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
        let empty: Value = serde_json::from_str(&empty.body).unwrap();
        assert_eq!(empty["running"], false);
        assert_eq!(empty["backups"], json!([]));
        assert!(empty["folder"].as_str().unwrap().ends_with("backups"));

        let peer = send(
            &app,
            mutation(
                "POST",
                "/api/v1/backups",
                json!({"idempotency_key": "k-peer", "host": "elsewhere"}),
                &headers,
            ),
        )
        .await;
        assert_ne!(peer.status, StatusCode::OK, "{}", peer.body);

        let taken = send(
            &app,
            mutation(
                "POST",
                "/api/v1/backups",
                json!({"idempotency_key": "k-now"}),
                &headers,
            ),
        )
        .await;
        assert_eq!(taken.status, StatusCode::OK, "{}", taken.body);
        let taken: Value = serde_json::from_str(&taken.body).unwrap();
        assert_eq!(taken["data"]["kind"], "manual");
        assert_eq!(taken["data"]["status"], "verified");

        let after = list(app.clone(), Some(cookie.clone())).await;
        let after: Value = serde_json::from_str(&after.body).unwrap();
        let rows = after["backups"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{after}");
        assert_eq!(rows[0]["id"], taken["data"]["id"]);
        assert_eq!(rows[0]["status"], "verified");
        assert!(rows[0]["size"].as_u64().unwrap() > 0);
        assert!(rows[0]["error"].is_null());
    }

    /// Settings > Hosts edits this host's label, lanes, floors and boundary
    /// against the `rev` it read, as `tiller host edit` does: a null clears a
    /// floor, a stale `rev` is a conflict, and the boundary stays once a
    /// backup records it.
    #[tokio::test]
    async fn the_board_edits_this_hosts_fields_against_its_rev() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let fields = |app: Router| async move {
            let answer = get(&app, "/api/v1/host").await;
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
            serde_json::from_str::<Value>(&answer.body).unwrap()
        };
        let edit = |body: Value| mutation("PATCH", "/api/v1/host", body, &headers);

        let before = fields(app.clone()).await;
        assert_eq!(before["label"], "test-host");
        assert_eq!(before["boundary"], "personal");
        assert!(before["boundary_recorded_by"].is_null(), "{before}");
        assert_eq!(before["lane_capacity"], 4);
        assert!(before["ram_floor_mb"].is_null(), "{before}");
        // An orchestrator holds no lane.
        assert_eq!(before["lanes_busy"], 0);

        let saved = send(
            &app,
            edit(
                json!({"idempotency_key": "k-cap", "rev": before["rev"], "label": "host-a",
                        "lane_capacity": 2, "ram_floor_mb": 8192, "disk_floor_mb": 20480}),
            ),
        )
        .await;
        assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
        let after = fields(app.clone()).await;
        assert_eq!(after["label"], "host-a");
        assert_eq!(after["lane_capacity"], 2);
        assert_eq!(after["ram_floor_mb"], 8192);
        assert_eq!(after["disk_floor_mb"], 20480);
        assert_eq!(after["rev"], before["rev"].as_i64().unwrap() + 1);

        // The rev read before the save is stale now.
        let stale = send(
            &app,
            edit(json!({"idempotency_key": "k-stale", "rev": before["rev"], "lane_capacity": 3})),
        )
        .await;
        assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);

        // A null clears a floor; the field left out keeps its value.
        let cleared = send(
            &app,
            edit(
                json!({"idempotency_key": "k-clear", "rev": after["rev"], "ram_floor_mb": null,
                        "boundary": "work"}),
            ),
        )
        .await;
        assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
        let now = fields(app.clone()).await;
        assert!(now["ram_floor_mb"].is_null(), "{now}");
        assert_eq!(now["disk_floor_mb"], 20480);
        assert_eq!(now["boundary"], "work");

        // A backup records the boundary: the board reads that, and an edit
        // of it is refused in plain words.
        let paths = tiller_core::backup::Paths::in_home(fleet.dir.path());
        tiller_core::backup::backup(fleet.store.conn(), &paths, Timestamp::now()).unwrap();
        let recorded = fields(app.clone()).await;
        assert_eq!(recorded["boundary_recorded_by"], "a backup");
        let refused = send(
            &app,
            edit(json!({"idempotency_key": "k-back", "rev": recorded["rev"],
                        "boundary": "personal"})),
        )
        .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );
        assert!(
            refused
                .body
                .contains("a backup already records the work boundary"),
            "{}",
            refused.body
        );
    }

    /// The recap counts what the log holds since the owner's mark, and Mark
    /// seen moves the mark to now, so the same events are not counted twice.
    #[tokio::test]
    async fn the_recap_counts_since_the_mark_and_marking_seen_resets_it() {
        let fleet = Fleet::new();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let recap = |app: Router| async move {
            let answer = get(&app, "/api/v1/recap").await;
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
            serde_json::from_str::<Value>(&answer.body).unwrap()
        };

        let first = recap(app.clone()).await;
        assert!(first["seen_at"].is_null(), "{first}");
        assert_eq!(first["landed"], 0);

        let store = fleet.open();
        let note = |kind: &str, payload: Value| {
            store
                .conn()
                .execute(
                    "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, payload)
                     VALUES (?1, ?2, 'test', 'daemon', 'x', 'x', ?3, ?4)",
                    rusqlite::params![
                        tiller_core::ids::new_ulid(),
                        Timestamp::now().to_string(),
                        kind,
                        payload.to_string()
                    ],
                )
                .unwrap();
        };
        note("card.closed", json!({"outcome": "landed"}));
        note("card.closed", json!({"outcome": "landed"}));
        note("card.closed", json!({"outcome": "cancelled"}));
        note("attention.opened", json!({"kind": "decision"}));
        note("attention.opened", json!({"kind": "report"}));
        note("run.ended", json!({"outcome": "failed"}));
        note("run.ended", json!({"outcome": "landed"}));
        note("proposal.created", json!({}));
        note("proposal.created", json!({}));
        note("proposal.created", json!({}));

        let counted = recap(app.clone()).await;
        assert_eq!(counted["landed"], 2, "{counted}");
        assert_eq!(counted["decisions"], 1, "{counted}");
        assert_eq!(counted["failed_runs"], 1, "{counted}");
        assert_eq!(counted["suggestions"], 3, "{counted}");

        let mark = |key: &str| {
            mutation(
                "POST",
                "/api/v1/recap/seen",
                json!({"idempotency_key": key}),
                &headers,
            )
        };
        let marked = send(&app, mark("k-seen")).await;
        assert_eq!(marked.status, StatusCode::OK, "{}", marked.body);
        let body: Value = serde_json::from_str(&marked.body).unwrap();
        assert_eq!(body["data"]["landed"], 0, "{body}");
        assert!(body["data"]["seen_at"].is_string(), "{body}");

        let after = recap(app.clone()).await;
        assert!(after["seen_at"].is_string(), "{after}");
        assert_eq!(after["landed"], 0, "{after}");
        assert_eq!(after["suggestions"], 0, "{after}");

        // The same key replays and writes no second mark.
        let again = send(&app, mark("k-seen")).await;
        let again: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(again["replayed"], true, "{again}");

        // A new event after the mark counts again.
        note("run.ended", json!({"outcome": "failed"}));
        assert_eq!(recap(app.clone()).await["failed_runs"], 1);

        // A peer's label is refused: the mark is this daemon's.
        let refused = send(
            &app,
            mutation(
                "POST",
                "/api/v1/recap/seen",
                json!({"idempotency_key": "k-peer", "host": "elsewhere"}),
                &headers,
            ),
        )
        .await;
        assert_ne!(refused.status, StatusCode::OK, "{}", refused.body);
    }

    /// P5: the orchestrator's reply lands in the project's unread replies,
    /// quoted on its card's timeline, and Mark read takes it out of the list.
    #[tokio::test]
    async fn a_reply_is_listed_unread_until_the_owner_marks_it_read() {
        use tiller_core::domain::{AskSource, CardKind};
        use tiller_core::state::asks::Resolution;
        use tiller_core::state::replies::NewReply;

        let mut fleet = Fleet::new();
        let orchestrator = Ctx::new(Actor::Session(fleet.orchestrator_session.clone()));
        let ask = fleet
            .store
            .ask_record(
                &Ctx::new(Actor::Board),
                Some("k-ask"),
                NewAsk {
                    project_id: "alpha".into(),
                    session_id: None,
                    source: AskSource::Board,
                    text: "Can the export run nightly?".into(),
                },
            )
            .unwrap()
            .value
            .ask;
        fleet
            .store
            .ask_resolve(
                &orchestrator,
                &ask.id,
                Resolution::New(NewCard {
                    kind: CardKind::Ship,
                    title: "Run the export nightly".into(),
                    ..NewCard::default()
                }),
            )
            .unwrap();
        let reply = fleet
            .store
            .reply_send(
                &orchestrator,
                None,
                NewReply {
                    to: ask.id.clone(),
                    text: "Yes. A run at 02:00 covers every merge of the day.".into(),
                    card: None,
                },
            )
            .unwrap()
            .value
            .reply;
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let unread = |app: Router| async move {
            let answer = get(&app, "/api/v1/projects/alpha/replies").await;
            assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
            serde_json::from_str::<Value>(&answer.body).unwrap()["replies"].clone()
        };

        let listed = unread(app.clone()).await;
        assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
        let item = &listed[0];
        assert_eq!(item["id"], reply.id.as_str());
        assert_eq!(item["to"], "ask");
        assert_eq!(item["question"], "Can the export run nightly?");
        assert_eq!(item["card"]["relation"], "created", "{item}");
        let card = item["card"]["display_id"].as_str().unwrap().to_owned();

        let timeline = get(&app, &format!("/api/v1/cards/{card}/timeline")).await;
        let timeline: Value = serde_json::from_str(&timeline.body).unwrap();
        let line = timeline["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["text"] == "replied to your ask")
            .unwrap_or_else(|| panic!("no reply line in {timeline}"));
        assert_eq!(line["actor"], "orchestrator");
        assert_eq!(
            line["quote"],
            "Yes. A run at 02:00 covers every merge of the day."
        );

        let mark = |key: &str, host: Option<&str>| {
            mutation(
                "POST",
                &format!("/api/v1/replies/{}/read", reply.id),
                json!({"idempotency_key": key, "host": host}),
                &headers,
            )
        };
        let refused = send(&app, mark("k-peer", Some("elsewhere"))).await;
        assert_ne!(refused.status, StatusCode::OK, "{}", refused.body);

        let marked = send(&app, mark("k-read", None)).await;
        assert_eq!(marked.status, StatusCode::OK, "{}", marked.body);
        let body: Value = serde_json::from_str(&marked.body).unwrap();
        assert!(body["data"]["read_at"].is_string(), "{body}");
        assert!(unread(app.clone()).await.as_array().unwrap().is_empty());

        // Marking it again writes nothing new.
        let again = send(&app, mark("k-read-2", None)).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        let missing = get(&app, "/api/v1/projects/nope/replies").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);
    }

    /// The card page's Depends on row (P13): `POST` and `DELETE`
    /// `/cards/{id}/deps` run `dep.add` and `dep.release` with their checks,
    /// the card page reads what each dependency is and which cards would
    /// make a loop, and a peer's card is forwarded, not changed here.
    #[tokio::test]
    async fn the_card_page_adds_and_removes_a_dependency() {
        let mut fleet = Fleet::new();
        let owner = Ctx::new(Actor::Owner);
        let mut make = |title: &str| {
            fleet
                .store
                .card_create(
                    &owner,
                    None,
                    tiller_core::state::cards::NewCard {
                        project_id: "alpha".into(),
                        kind: tiller_core::domain::CardKind::Ops,
                        title: title.into(),
                        ..Default::default()
                    },
                    None,
                )
                .unwrap()
                .value
        };
        let export = make("Split the export worker");
        let bucket = make("Export bucket per run");
        let dashboard = make("Export retry dashboard");
        let shipped = make("Retry the export upload");
        fleet
            .store
            .dep_add(&owner, &dashboard.display_id, &export.display_id)
            .unwrap();
        fleet
            .store
            .card_close(
                &owner,
                &shipped.display_id,
                tiller_core::domain::CardOutcome::Cancelled,
                Some("not needed"),
                None,
            )
            .unwrap();
        fleet
            .store
            .peer_pair(
                &owner,
                NewPairing {
                    label: "laptop".into(),
                    herdr_machine: None,
                    scope: PeerScope::Act,
                    peer_host_id: "01JC00000000000000000PEER2".into(),
                },
            )
            .unwrap();
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let deps = format!("/api/v1/cards/{}/deps", export.display_id);
        let call = |method: &str, body: Value| mutation(method, &deps, body, &headers);

        let added = send(
            &app,
            call(
                "POST",
                json!({"idempotency_key": "k-add", "depends_on": bucket.display_id}),
            ),
        )
        .await;
        assert_eq!(added.status, StatusCode::OK, "{}", added.body);
        let body: Value = serde_json::from_str(&added.body).unwrap();
        assert_eq!(body["data"]["depends_on_card_id"], bucket.id);
        assert_eq!(body["replayed"], false);
        // A second add of an open dependency is natural.
        let again = send(
            &app,
            call(
                "POST",
                json!({"idempotency_key": "k-add-2", "depends_on": bucket.display_id}),
            ),
        )
        .await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);

        // The card page reads the dependency's state and the loop.
        let page = format!("/api/v1/cards/{}", export.id);
        let read = get(&app, &page).await;
        assert_eq!(read.status, StatusCode::OK, "{}", read.body);
        let detail: Value = serde_json::from_str(&read.body).unwrap();
        assert_eq!(
            detail["deps"],
            json!([{"display_id": bucket.display_id, "released_at": null,
                    "title": "Export bucket per run", "column": "queued", "badge": "filed"}])
        );
        assert_eq!(detail["waiting_on_this"], json!([dashboard.display_id]));
        assert_eq!(detail["column"]["badge"], "blocked");

        // The checks are core's: a loop, the card itself, a closed card and
        // an unknown card are each refused.
        for (key, on, status, text) in [
            (
                "k-loop",
                dashboard.display_id.as_str(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "already waits on",
            ),
            (
                "k-self",
                export.display_id.as_str(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "cannot wait on itself",
            ),
            (
                "k-closed",
                shipped.display_id.as_str(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "already closed",
            ),
            ("k-nope", "alpha-999", StatusCode::NOT_FOUND, "alpha-999"),
        ] {
            let refused = send(
                &app,
                call("POST", json!({"idempotency_key": key, "depends_on": on})),
            )
            .await;
            assert_eq!(refused.status, status, "{on}: {}", refused.body);
            assert!(refused.body.contains(text), "{on}: {}", refused.body);
        }

        // Remove releases it as `removed`; a second remove finds none open.
        let removed = send(
            &app,
            call(
                "DELETE",
                json!({"idempotency_key": "k-rm", "depends_on": bucket.display_id}),
            ),
        )
        .await;
        assert_eq!(removed.status, StatusCode::OK, "{}", removed.body);
        let body: Value = serde_json::from_str(&removed.body).unwrap();
        assert_eq!(body["data"]["release_reason"], "removed");
        let none = send(
            &app,
            call(
                "DELETE",
                json!({"idempotency_key": "k-rm-2", "depends_on": bucket.display_id}),
            ),
        )
        .await;
        assert_eq!(none.status, StatusCode::OK, "{}", none.body);
        let body: Value = serde_json::from_str(&none.body).unwrap();
        assert_eq!(body["data"], Value::Null);
        let read = get(&app, &page).await;
        let detail: Value = serde_json::from_str(&read.body).unwrap();
        assert_eq!(detail["deps"][0]["release_reason"], "removed");
        assert_ne!(detail["column"]["badge"], "blocked");

        // A peer's card is forwarded: with no link to the peer yet it is
        // unreachable and nothing is sent or written. An unknown field is
        // refused.
        for method in ["POST", "DELETE"] {
            let peer = send(
                &app,
                call(
                    method,
                    json!({"idempotency_key": format!("k-peer-{method}"), "host": "laptop",
                           "depends_on": bucket.display_id}),
                ),
            )
            .await;
            assert_eq!(
                peer.status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{}",
                peer.body
            );
            assert_eq!(error_of(&peer)["code"], "peer_unreachable", "{}", peer.body);
        }
        let odd = send(
            &app,
            call(
                "POST",
                json!({"idempotency_key": "k-odd", "depends_on": bucket.display_id, "why": 1}),
            ),
        )
        .await;
        assert_eq!(odd.status, StatusCode::BAD_REQUEST, "{}", odd.body);
        let anonymous = send(
            &app,
            mutation(
                "POST",
                &deps,
                json!({"idempotency_key": "k-anon", "depends_on": bucket.display_id}),
                &[],
            ),
        )
        .await;
        assert_eq!(
            anonymous.status,
            StatusCode::FORBIDDEN,
            "{}",
            anonymous.body
        );
    }

    /// The card page's actions, one route each: edit, hold and release,
    /// close with its outcome, cancel and reopen.
    #[tokio::test]
    async fn the_card_page_actions_each_reach_their_route() {
        let mut fleet = Fleet::new();
        let owner = Ctx::new(Actor::Owner);
        let card = fleet
            .store
            .card_create(
                &owner,
                None,
                tiller_core::state::cards::NewCard {
                    project_id: "alpha".into(),
                    kind: tiller_core::domain::CardKind::Ops,
                    title: "Rotate the export key".into(),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
            .value;
        let app = app(fleet.dir.path(), Assets::of::<Fixture>());
        let cookie = owner_cookie(fleet.dir.path());
        let headers = [("cookie", cookie.as_str())];
        let path = |tail: &str| format!("/api/v1/cards/{}{tail}", card.id);
        let post = |tail: &str, body: Value| mutation("POST", &path(tail), body, &headers);

        // Edit: the title and priority change against the rev the board read.
        let edited = send(
            &app,
            mutation(
                "PATCH",
                &path(""),
                json!({"idempotency_key": "k-edit", "rev": card.rev, "title": "Rotate the key",
                       "priority": 1}),
                &headers,
            ),
        )
        .await;
        assert_eq!(edited.status, StatusCode::OK, "{}", edited.body);

        // Hold and release.
        let held = send(
            &app,
            post(
                "/hold",
                json!({"idempotency_key": "k-hold", "until": "2031-01-01T00:00:00Z",
                       "reason": "waiting on the vendor"}),
            ),
        )
        .await;
        assert_eq!(held.status, StatusCode::OK, "{}", held.body);
        let released = send(
            &app,
            mutation(
                "DELETE",
                &path("/hold"),
                json!({"idempotency_key": "k-release"}),
                &headers,
            ),
        )
        .await;
        assert_eq!(released.status, StatusCode::OK, "{}", released.body);

        // Close names its outcome. A body without one is a clear 400, not an
        // evidence error from core (B7).
        let missing = send(
            &app,
            post(
                "/close",
                json!({"idempotency_key": "k-none", "reason": "done"}),
            ),
        )
        .await;
        assert_eq!(missing.status, StatusCode::BAD_REQUEST, "{}", missing.body);
        assert!(
            missing.body.contains("outcome is required"),
            "{}",
            missing.body
        );
        let blank = send(
            &app,
            post(
                "/close",
                json!({"idempotency_key": "k-blank", "outcome": " ", "reason": "done"}),
            ),
        )
        .await;
        assert_eq!(blank.status, StatusCode::BAD_REQUEST, "{}", blank.body);
        let unknown = send(
            &app,
            post(
                "/close",
                json!({"idempotency_key": "k-odd", "outcome": "finished", "reason": "done"}),
            ),
        )
        .await;
        assert!(unknown.status.is_client_error(), "{}", unknown.body);

        // Cancel needs a reason; Reopen brings the card back.
        let no_reason = send(&app, post("/cancel", json!({"idempotency_key": "k-nr"}))).await;
        assert_eq!(
            no_reason.status,
            StatusCode::BAD_REQUEST,
            "{}",
            no_reason.body
        );
        let cancelled = send(
            &app,
            post(
                "/cancel",
                json!({"idempotency_key": "k-cancel", "reason": "the vendor dropped it"}),
            ),
        )
        .await;
        assert_eq!(cancelled.status, StatusCode::OK, "{}", cancelled.body);
        let reopened = send(
            &app,
            post(
                "/reopen",
                json!({"idempotency_key": "k-reopen", "reason": "it came back"}),
            ),
        )
        .await;
        assert_eq!(reopened.status, StatusCode::OK, "{}", reopened.body);

        // A completed close takes its evidence from the reason.
        let closed = send(
            &app,
            post(
                "/close",
                json!({"idempotency_key": "k-close", "outcome": "completed",
                       "reason": "key rotated, old one revoked"}),
            ),
        )
        .await;
        assert_eq!(closed.status, StatusCode::OK, "{}", closed.body);
        let outcome: Option<String> = fleet
            .open()
            .conn()
            .query_row(
                "SELECT outcome FROM cards WHERE id = ?1",
                [&card.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outcome.as_deref(), Some("completed"));
    }

    /// A starter that records each lifecycle control it is handed, and as
    /// whom: the stop and the relaunch go through with a warning, a resume
    /// is refused, and a start reports whether it was a restart.
    #[derive(Default)]
    struct FakeControls(Mutex<Vec<String>>);

    impl FakeControls {
        fn calls(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }

        fn record(&self, call: String) {
            self.0.lock().unwrap().push(call);
        }

        fn warned(value: Value) -> Controlled {
            Controlled {
                applied: Applied {
                    value,
                    events: Vec::new(),
                    replayed: false,
                },
                warning: Some("the cleanup refused".into()),
            }
        }
    }

    impl OrchestratorStart for FakeControls {
        fn start(
            &self,
            _store: &mut Store,
            ctx: &Ctx,
            project: &str,
            restart: bool,
        ) -> Result<Applied<Orchestrator>, StartError> {
            self.record(format!("start {} {project} restart={restart}", ctx.actor));
            Err(StartError::Refused {
                exit: 4,
                code: "conflict",
                message: "a live orchestrator".into(),
                hint: None,
                current: None,
            })
        }

        fn stop_agent(
            &self,
            _store: &mut Store,
            ctx: &Ctx,
            session: &Session,
        ) -> Result<Controlled, StartError> {
            self.record(format!("stop {} {}", ctx.actor, session.id));
            Ok(Self::warned(json!({"session": session.id})))
        }

        fn resume_run(
            &self,
            _store: &mut Store,
            ctx: &Ctx,
            run: &Run,
        ) -> Result<Controlled, StartError> {
            self.record(format!("resume {} {}", ctx.actor, run.id));
            Err(StartError::Refused {
                exit: 5,
                code: "refused",
                message: format!("run {} has no Claude session to resume", run.id),
                hint: None,
                current: None,
            })
        }

        fn relaunch_run(
            &self,
            _store: &mut Store,
            ctx: &Ctx,
            run: &Run,
            relaunch: BoardRelaunch,
        ) -> Result<Controlled, StartError> {
            self.record(format!(
                "relaunch {} {} {} new_attempt={}",
                ctx.actor, run.id, relaunch.note, relaunch.new_attempt
            ));
            Ok(Self::warned(json!({"run": run.id})))
        }
    }

    /// P11: the board's Stop, Resume and Relaunch reach the commands' own
    /// path as the board, with the session's run; an orchestrator, which has
    /// no run, is refused before the starter is asked.
    #[tokio::test]
    async fn the_board_stops_resumes_and_relaunches_through_the_command_path() {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let run: String = fleet
            .open()
            .conn()
            .query_row(
                "SELECT run_id FROM sessions WHERE id = ?1",
                [&worker],
                |row| row.get(0),
            )
            .unwrap();
        let orchestrator = fleet.orchestrator_session.clone();
        let starter = Arc::new(FakeControls::default());
        let app = app_restarting(fleet.dir.path(), starter.clone());
        let cookie = owner_cookie(fleet.dir.path());
        let post = |path: String, body: Value| {
            let app = app.clone();
            let cookie = cookie.clone();
            async move {
                send(
                    &app,
                    mutation("POST", &path, body, &[("cookie", cookie.as_str())]),
                )
                .await
            }
        };

        let stopped = post(
            format!("/api/v1/agents/{worker}/stop"),
            json!({"idempotency_key": "k-stop"}),
        )
        .await;
        assert_eq!(stopped.status, StatusCode::OK, "{}", stopped.body);
        let stopped: Value = serde_json::from_str(&stopped.body).unwrap();
        assert_eq!(stopped["data"]["session"], worker.as_str());
        assert_eq!(stopped["warning"], "the cleanup refused", "{stopped}");

        let resumed = post(
            format!("/api/v1/agents/{worker}/resume"),
            json!({"idempotency_key": "k-resume"}),
        )
        .await;
        assert_eq!(
            resumed.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            resumed.body
        );
        assert!(
            resumed.body.contains("no Claude session"),
            "{}",
            resumed.body
        );

        let blank = post(
            format!("/api/v1/agents/{worker}/relaunch"),
            json!({"idempotency_key": "k-blank", "note": "  "}),
        )
        .await;
        assert_eq!(blank.status, StatusCode::BAD_REQUEST, "{}", blank.body);
        let relaunched = post(
            format!("/api/v1/agents/{worker}/relaunch"),
            json!({"idempotency_key": "k-relaunch", "note": "tests pass", "new_attempt": true}),
        )
        .await;
        assert_eq!(relaunched.status, StatusCode::OK, "{}", relaunched.body);

        for verb in ["resume", "relaunch"] {
            let refused = post(
                format!("/api/v1/agents/{orchestrator}/{verb}"),
                if verb == "relaunch" {
                    json!({"idempotency_key": "k-o-relaunch", "note": "n"})
                } else {
                    json!({"idempotency_key": "k-o-resume"})
                },
            )
            .await;
            assert_eq!(
                refused.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{}",
                refused.body
            );
            assert!(
                refused.body.contains(&format!("has no run to {verb}")),
                "{}",
                refused.body
            );
        }

        assert_eq!(
            starter.calls(),
            [
                format!("stop board {worker}"),
                format!("resume board {run}"),
                format!("relaunch board {run} tests pass new_attempt=true"),
            ]
        );
    }

    /// P11: the board's Pause until is `tiller run pause` as the board; the
    /// agents view shows the pause with who took it and why.
    #[tokio::test]
    async fn the_board_pauses_a_run_and_the_agents_view_shows_why() {
        let mut fleet = Fleet::new();
        let worker = fleet.worker("w2:p1");
        let orchestrator = fleet.orchestrator_session.clone();
        let app = app_restarting(fleet.dir.path(), Arc::new(FakeControls::default()));
        let cookie = owner_cookie(fleet.dir.path());
        let path = format!("/api/v1/agents/{worker}/pause");
        let until = Timestamp::now().plus_minutes(48 * 60).to_string();
        let pause = |body: Value, path: String| {
            let app = app.clone();
            let cookie = cookie.clone();
            async move {
                send(
                    &app,
                    mutation("POST", &path, body, &[("cookie", cookie.as_str())]),
                )
                .await
            }
        };

        let blank = pause(
            json!({"idempotency_key": "k-0", "until": until, "reason": " "}),
            path.clone(),
        )
        .await;
        assert_eq!(blank.status, StatusCode::BAD_REQUEST, "{}", blank.body);
        let bad_date = pause(
            json!({"idempotency_key": "k-0", "until": "Thursday", "reason": "r"}),
            path.clone(),
        )
        .await;
        assert_eq!(
            bad_date.status,
            StatusCode::BAD_REQUEST,
            "{}",
            bad_date.body
        );
        let orchestrated = pause(
            json!({"idempotency_key": "k-0", "until": until, "reason": "r"}),
            format!("/api/v1/agents/{orchestrator}/pause"),
        )
        .await;
        assert_eq!(
            orchestrated.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            orchestrated.body
        );

        let body =
            json!({"idempotency_key": "k-1", "until": until, "reason": "waiting on the API key"});
        let paused = pause(body.clone(), path.clone()).await;
        assert_eq!(paused.status, StatusCode::OK, "{}", paused.body);
        let again = pause(body, path.clone()).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);

        let read: Value = serde_json::from_str(&get(&app, "/api/v1/agents").await.body).unwrap();
        let row = read["orchestrators"][0]["workers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["session_id"] == worker.as_str())
            .unwrap()
            .clone();
        assert_eq!(row["pause"]["by"], "board", "{row}");
        assert_eq!(row["pause"]["reason"], "waiting on the API key");
        let shown: Timestamp = row["pause"]["until"].as_str().unwrap().parse().unwrap();
        assert_eq!(shown, until.parse::<Timestamp>().unwrap());
        let paused_events: i64 = fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'run.paused' AND actor = 'board'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(paused_events, 1, "the retry replays");

        // The board's Resume clears the pause, and the row drops it.
        let unpause = format!("/api/v1/agents/{worker}/unpause");
        let orchestrated = pause(
            json!({"idempotency_key": "k-2"}),
            format!("/api/v1/agents/{orchestrator}/unpause"),
        )
        .await;
        assert_eq!(
            orchestrated.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            orchestrated.body
        );
        let cleared = pause(json!({"idempotency_key": "k-3"}), unpause.clone()).await;
        assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.body);
        let cleared: Value = serde_json::from_str(&cleared.body).unwrap();
        assert!(cleared["data"]["pause_until"].is_null(), "{cleared}");
        let natural = pause(json!({"idempotency_key": "k-4"}), unpause).await;
        assert_eq!(natural.status, StatusCode::OK, "{}", natural.body);
        let read: Value = serde_json::from_str(&get(&app, "/api/v1/agents").await.body).unwrap();
        let row = read["orchestrators"][0]["workers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["session_id"] == worker.as_str())
            .unwrap()
            .clone();
        assert!(row["pause"].is_null(), "{row}");
        let unpaused_events: i64 = fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'run.unpaused' AND actor = 'board'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unpaused_events, 1, "a run with no pause writes nothing");
    }

    /// P11: Restart on the board is `tiller orchestrator start --restart`
    /// as the board; a plain Start is never a restart.
    #[tokio::test]
    async fn only_the_boards_explicit_restart_restarts_an_orchestrator() {
        let fleet = Fleet::new();
        let starter = Arc::new(FakeControls::default());
        let app = app_restarting(fleet.dir.path(), starter.clone());
        let cookie = owner_cookie(fleet.dir.path());
        for (key, body) in [
            ("k-start", json!({"idempotency_key": "k-start"})),
            (
                "k-restart",
                json!({"idempotency_key": "k-restart", "restart": true}),
            ),
        ] {
            let answer = send(
                &app,
                mutation(
                    "POST",
                    "/api/v1/projects/alpha/orchestrator",
                    body,
                    &[("cookie", cookie.as_str())],
                ),
            )
            .await;
            assert_eq!(
                answer.status,
                StatusCode::CONFLICT,
                "{key}: {}",
                answer.body
            );
        }
        assert_eq!(
            starter.calls(),
            [
                "start board alpha restart=false",
                "start board alpha restart=true"
            ]
        );
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
