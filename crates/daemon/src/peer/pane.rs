//! A peer's pane on the board (`protocol.md` 10.7): the board host opens a
//! second SSH session to the peer, `tiller peer pane --from <label>`, sends
//! one signed [`PaneOpen`], and gets back either the pane's NDJSON mirror
//! stream (the same lines `POST .../terminal` streams) or one `result`
//! frame, which is a refusal or the pane's scrollback.
//!
//! While a stream runs, the board host may send the owner's keys
//! ([`PaneInput`]) and the board's new size ([`PaneFit`]) on the same
//! link. Each is signed, carries the open's `id` and a `seq` that only
//! grows, and is checked again here before it reaches the pane: the key,
//! the signature, `ts`, this host, the `act` scope, and that the session
//! is still running on this host. Each gets one `result` line between the
//! mirror's lines. A pane is typed into through this host's own input
//! path, the terminal holder's, and nothing else on the link writes:
//! tail, prompt answers, steer and the other agent verbs have no frame.
//! The owner's cookie is checked on the board host before any of it is
//! dialed.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_core::domain::{PeerScope, Session};
use tiller_core::state::peers::parse_public_key;
use tiller_core::{Store, Timestamp, ids};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use super::nonces;
use super::serve::{Denied, ServeConfig, ServeError, current_key, refused, skew_ms};
use super::{Frame, HelloKey};
use crate::api::{
    ApiError, fit_sent, holder_error, holder_history, input_sent, pane_target, remote_pane,
    terminal_keys, type_into,
};
use crate::dto::{TerminalFit, TerminalHistory};
use crate::holder;
use crate::mirror::{self, Mirrors};

/// How long `peer pane` waits for the opening [`PaneOpen`].
const OPEN_WAIT: Duration = Duration::from_secs(30);

/// How long a stream that was told to stop has to end, so a Fit gives the
/// pane back its size before the process exits: longer than a give-back
/// waits for its turn.
const DRAIN_WAIT: Duration = Duration::from_secs(10);

/// Inputs and Fits the stream's reader can hand on before it waits.
const ASKS: usize = 16;

/// What a [`PaneOpen`] asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    /// The live mirror stream, at the pane's own size.
    Stream,
    /// The scrollback, one read.
    History,
}

impl PaneKind {
    fn as_str(self) -> &'static str {
        match self {
            PaneKind::Stream => "stream",
            PaneKind::History => "history",
        }
    }
}

/// The board host's one request on a pane link. `sig` is Ed25519 over every
/// other field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneOpen {
    pub id: String,
    /// The peer's own session id, as its rows carry it.
    pub session: String,
    pub kind: PaneKind,
    /// Scrollback rows for [`PaneKind::History`], 1 to 1000.
    pub lines: Option<u16>,
    /// The board's size for a [`PaneKind::Stream`]: the pane is fitted to
    /// it while the stream runs, as a lease on the holder pane. Absent, the
    /// pane keeps its size. Asking for one needs the `act` scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fit: Option<TerminalFit>,
    pub ts: String,
    pub viewer_host_id: String,
    pub peer_host_id: String,
    pub key_id: String,
    pub sig: String,
}

fn open_message(frame: &PaneOpen) -> Vec<u8> {
    let mut message = vec![
        json!("tiller-peer/1 pane"),
        json!(frame.id),
        json!(frame.session),
        json!(frame.kind.as_str()),
        json!(frame.lines),
        json!(frame.ts),
        json!(frame.viewer_host_id),
        json!(frame.peer_host_id),
        json!(frame.key_id),
    ];
    // A Fit is signed after the rest, so an open without one signs as it
    // did before the Fit could be asked for.
    if let Some(fit) = frame.fit {
        message.extend([json!(fit.cols), json!(fit.rows)]);
    }
    Value::Array(message).to_string().into_bytes()
}

/// True when `sig` is a valid signature of `message` for `public_key`
/// (base64, as `peer_keys` stores it).
fn signed_by(public_key: &str, message: &[u8], sig: &str) -> bool {
    let Ok(key) = parse_public_key(public_key) else {
        return false;
    };
    let Some(signature) = BASE64
        .decode(sig)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
    else {
        return false;
    };
    key.verify(message, &signature).is_ok()
}

/// What the board host signs a pane link's frames with: its key on the
/// peer, and the two hosts' ids.
pub struct PaneSigner {
    pub key: SigningKey,
    pub key_id: String,
    pub viewer_host_id: String,
    pub peer_host_id: String,
}

/// The owner's keys or paste for the pane an open stream shows, sent on
/// its pane link. `sig` is Ed25519 over every other field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneInput {
    pub id: String,
    /// The [`PaneOpen`] whose stream this types into.
    pub open_id: String,
    /// One more than the link's last input or Fit: the peer applies them
    /// in this order and refuses one that does not grow.
    pub seq: u64,
    pub session: String,
    /// Keys pressed in order, as the board's terminal input names them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
    /// Text pasted at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub ts: String,
    pub viewer_host_id: String,
    pub peer_host_id: String,
    pub key_id: String,
    pub sig: String,
}

fn input_message(frame: &PaneInput) -> Vec<u8> {
    json!([
        "tiller-peer/1 pane_input",
        frame.id,
        frame.open_id,
        frame.seq,
        frame.session,
        frame.keys,
        frame.text,
        frame.ts,
        frame.viewer_host_id,
        frame.peer_host_id,
        frame.key_id,
    ])
    .to_string()
    .into_bytes()
}

impl PaneInput {
    /// Signed keys (`Ok`) or paste (`Err`) for the stream `open_id`.
    pub fn signed(
        signer: &PaneSigner,
        open_id: &str,
        seq: u64,
        session: &str,
        input: Result<Vec<String>, String>,
        now: Timestamp,
    ) -> Self {
        let (keys, text) = match input {
            Ok(keys) => (Some(keys), None),
            Err(text) => (None, Some(text)),
        };
        let mut frame = PaneInput {
            id: ids::new_ulid(),
            open_id: open_id.to_owned(),
            seq,
            session: session.to_owned(),
            keys,
            text,
            ts: now.to_string(),
            viewer_host_id: signer.viewer_host_id.clone(),
            peer_host_id: signer.peer_host_id.clone(),
            key_id: signer.key_id.clone(),
            sig: String::new(),
        };
        frame.sign(&signer.key);
        frame
    }

    pub(super) fn sign(&mut self, key: &SigningKey) {
        self.sig = BASE64.encode(key.sign(&input_message(self)).to_bytes());
    }

    fn header(&self) -> Header<'_> {
        Header {
            id: &self.id,
            open_id: &self.open_id,
            seq: self.seq,
            session: &self.session,
            ts: &self.ts,
            viewer_host_id: &self.viewer_host_id,
            peer_host_id: &self.peer_host_id,
            key_id: &self.key_id,
            message: input_message(self),
            sig: &self.sig,
        }
    }
}

/// The board's new size for the Fit an open stream holds, sent on its
/// pane link. `sig` is Ed25519 over every other field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneFit {
    pub id: String,
    pub open_id: String,
    /// Shares the link's order with [`PaneInput`].
    pub seq: u64,
    pub session: String,
    pub cols: u16,
    pub rows: u16,
    pub ts: String,
    pub viewer_host_id: String,
    pub peer_host_id: String,
    pub key_id: String,
    pub sig: String,
}

fn fit_message(frame: &PaneFit) -> Vec<u8> {
    json!([
        "tiller-peer/1 pane_fit",
        frame.id,
        frame.open_id,
        frame.seq,
        frame.session,
        frame.cols,
        frame.rows,
        frame.ts,
        frame.viewer_host_id,
        frame.peer_host_id,
        frame.key_id,
    ])
    .to_string()
    .into_bytes()
}

impl PaneFit {
    /// A signed new size for the stream `open_id`.
    pub fn signed(
        signer: &PaneSigner,
        open_id: &str,
        seq: u64,
        session: &str,
        (cols, rows): (u16, u16),
        now: Timestamp,
    ) -> Self {
        let mut frame = PaneFit {
            id: ids::new_ulid(),
            open_id: open_id.to_owned(),
            seq,
            session: session.to_owned(),
            cols,
            rows,
            ts: now.to_string(),
            viewer_host_id: signer.viewer_host_id.clone(),
            peer_host_id: signer.peer_host_id.clone(),
            key_id: signer.key_id.clone(),
            sig: String::new(),
        };
        frame.sign(&signer.key);
        frame
    }

    pub(super) fn sign(&mut self, key: &SigningKey) {
        self.sig = BASE64.encode(key.sign(&fit_message(self)).to_bytes());
    }

    fn header(&self) -> Header<'_> {
        Header {
            id: &self.id,
            open_id: &self.open_id,
            seq: self.seq,
            session: &self.session,
            ts: &self.ts,
            viewer_host_id: &self.viewer_host_id,
            peer_host_id: &self.peer_host_id,
            key_id: &self.key_id,
            message: fit_message(self),
            sig: &self.sig,
        }
    }
}

/// The fields every frame on an open stream is checked by.
struct Header<'a> {
    id: &'a str,
    open_id: &'a str,
    seq: u64,
    session: &'a str,
    ts: &'a str,
    viewer_host_id: &'a str,
    peer_host_id: &'a str,
    key_id: &'a str,
    message: Vec<u8>,
    sig: &'a str,
}

impl PaneOpen {
    /// A signed request for the peer whose `meta.host_id` is
    /// `peer_host_id`, at `now`.
    pub fn signed(
        signer: &HelloKey<'_>,
        peer_host_id: &str,
        session: &str,
        kind: PaneKind,
        lines: Option<u16>,
        fit: Option<TerminalFit>,
        now: Timestamp,
    ) -> Self {
        let mut frame = PaneOpen {
            id: ids::new_ulid(),
            session: session.to_owned(),
            kind,
            lines,
            fit,
            ts: now.to_string(),
            viewer_host_id: signer.viewer_host_id.to_owned(),
            peer_host_id: peer_host_id.to_owned(),
            key_id: signer.key_id.to_owned(),
            sig: String::new(),
        };
        frame.sign(signer.key);
        frame
    }

    pub(super) fn sign(&mut self, key: &SigningKey) {
        self.sig = BASE64.encode(key.sign(&open_message(self)).to_bytes());
    }

    /// True when `sig` is valid for `public_key` (base64, as `peer_keys`
    /// stores it).
    pub fn verifies(&self, public_key: &str) -> bool {
        signed_by(public_key, &open_message(self), &self.sig)
    }
}

/// The checks on the peer, in the order `mutate`'s run (`protocol.md`
/// 10.3 step 3) up to the allowlist: `federation.accept`, an unrevoked key,
/// the signature, `ts` within the skew, this host's `host_id`. A `read`
/// pairing may watch, so the scope is checked only for a Fit, which sizes
/// the pane. A pane is no project's record, so there is no owner-marker or
/// project check.
fn verify(
    store: &Store,
    config: &ServeConfig,
    frame: &PaneOpen,
    now: Timestamp,
) -> Result<(), Denied> {
    let key = current_key(store, config, &frame.key_id)?;
    if key.viewer_host_id != frame.viewer_host_id || key.viewer_label != config.from {
        return refused(
            "unknown_key",
            format!("key {} is not this board host's", frame.key_id),
        );
    }
    if !frame.verifies(&key.public_key) {
        return refused("bad_sig", "the pane request signature is not valid".into());
    }
    let skew = skew_ms(&frame.ts, now);
    if u128::from(skew) > config.timing.skew.as_millis() {
        return refused(
            "clock_skew",
            format!("the pane request is {skew} ms from this host's clock"),
        );
    }
    let host = store.this_host()?;
    if frame.peer_host_id != host.id {
        return refused(
            "wrong_host",
            format!(
                "the pane request is for host {}, not {}",
                frame.peer_host_id, host.id
            ),
        );
    }
    if frame.fit.is_some() && key.scope != PeerScope::Act {
        return read_scope(&host.label);
    }
    // Last, so only a request that passed every other check is kept.
    nonces::take(&config.home, &frame.id, &frame.ts, config.timing.skew, now)
}

fn read_scope(label: &str) -> Result<(), Denied> {
    refused(
        "read_scope",
        format!("{label} is paired with a read scope: view only pairing"),
    )
}

/// The checks on an input or a Fit for the open stream `open`: those of
/// the open itself, in the same order but for the nonce, then the `act`
/// scope, the open it names, an order that only grows, and its session.
/// `last` is the link's last `seq`.
fn verify_act(
    store: &Store,
    config: &ServeConfig,
    open: &PaneOpen,
    frame: &Header<'_>,
    last: u64,
    now: Timestamp,
) -> Result<(), Denied> {
    let key = current_key(store, config, frame.key_id)?;
    if frame.key_id != open.key_id
        || key.viewer_host_id != frame.viewer_host_id
        || key.viewer_label != config.from
    {
        return refused(
            "unknown_key",
            format!("key {} is not this board host's", frame.key_id),
        );
    }
    if !signed_by(&key.public_key, &frame.message, frame.sig) {
        return refused("bad_sig", "the pane input signature is not valid".into());
    }
    let skew = skew_ms(frame.ts, now);
    if u128::from(skew) > config.timing.skew.as_millis() {
        return refused(
            "clock_skew",
            format!("the pane input is {skew} ms from this host's clock"),
        );
    }
    let host = store.this_host()?;
    if frame.peer_host_id != host.id {
        return refused(
            "wrong_host",
            format!(
                "the pane input is for host {}, not {}",
                frame.peer_host_id, host.id
            ),
        );
    }
    if key.scope != PeerScope::Act {
        return read_scope(&host.label);
    }
    if frame.open_id != open.id || frame.session != open.session {
        return refused(
            "not_allowlisted",
            format!(
                "pane input {} is for another pane link than {}",
                frame.id, open.id
            ),
        );
    }
    if frame.seq <= last {
        return refused(
            "replayed",
            format!("pane input {} came after {last} on this link", frame.seq),
        );
    }
    Ok(())
}

/// The session the stream shows, read again for each input: it must still
/// run, on this host.
fn still_running(store: &Store, session_id: &str) -> Result<(), ApiError> {
    let session: Session =
        tiller_core::state::get(store.conn(), "session", session_id).map_err(ApiError::from)?;
    if session.host_id != store.this_host().map_err(ApiError::from)?.id {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "refused",
            format!("session {session_id} runs on another host"),
        ));
    }
    if session.ended_at.is_some() {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "refused",
            format!("session {session_id} has ended; nothing reads its input"),
        ));
    }
    Ok(())
}

/// `tiller peer pane --from <label>`, on the peer, for as long as the
/// board host's viewer watches.
pub fn serve_stdio(config: &ServeConfig) -> Result<(), ServeError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let ended = runtime.block_on(serve(
        config,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    ));
    // The stdin read cannot be cancelled: do not wait for it to see EOF.
    runtime.shutdown_background();
    ended
}

pub async fn serve(
    config: &ServeConfig,
    input: impl AsyncBufRead + Send + Unpin + 'static,
    mut output: impl AsyncWrite + Send + Unpin,
) -> Result<(), ServeError> {
    let mut lines = input.lines();
    let opened = tokio::time::timeout(OPEN_WAIT, lines.next_line()).await;
    let Ok(Ok(Some(line))) = opened else {
        // Nothing was asked, so nothing is answered.
        return Ok(());
    };
    let Ok(Frame::PaneOpen(open)) = serde_json::from_str::<Frame>(&line) else {
        return Ok(());
    };
    let store = Store::open_current(config.home.join("state.db"))?;
    if let Err(denied) = verify(&store, config, &open, Timestamp::now()) {
        let (reason, message) = match denied {
            Denied::Refused(reason, message) => (reason, message),
            Denied::Failed(error) => return Err(error.into()),
        };
        write(
            &mut output,
            &Frame::Refused {
                reason: reason.to_owned(),
                message: Some(message.clone()),
            },
        )
        .await?;
        return Err(ServeError::Refused {
            reason: reason.to_owned(),
            message,
        });
    }
    // The session is looked up here and the store closed: the stream below
    // outlives any read transaction.
    let target = pane_target(&store, &open.session);
    drop(store);
    let answered = match target {
        Ok((_, _, Some(label))) => Err(remote_pane(
            &label,
            "the terminal mirror shows panes on this machine only",
        )),
        Ok((session_id, pane, None)) => match open.kind {
            PaneKind::History => history(config, &open, session_id, pane).await,
            PaneKind::Stream => {
                return stream(config, &open, session_id, pane, lines, &mut output).await;
            }
        },
        Err(error) => Err(ApiError::from(error)),
    };
    let (http_status, body) = match answered {
        Ok(body) => (StatusCode::OK.as_u16(), body),
        Err(error) => error.into_result(),
    };
    write(
        &mut output,
        &Frame::Result {
            id: open.id,
            http_status,
            body,
        },
    )
    .await?;
    Ok(())
}

async fn history(
    config: &ServeConfig,
    open: &PaneOpen,
    session_id: String,
    pane: String,
) -> Result<Value, ApiError> {
    let lines = open.lines.unwrap_or(mirror::MAX_HISTORY);
    if !(1..=mirror::MAX_HISTORY).contains(&lines) {
        return Err(ApiError::usage(format!(
            "lines is 1 to {}",
            mirror::MAX_HISTORY
        )));
    }
    let text = holder_history(holder(config)?, &pane, lines).await?;
    serde_json::to_value(TerminalHistory { session_id, text })
        .map_err(|error| ApiError::from(tiller_core::Error::from(error)))
}

fn holder(config: &ServeConfig) -> Result<Arc<dyn crate::holder::Holder>, ApiError> {
    config.holder.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "the terminal holder is unreachable: this peer has no link to it",
        )
    })
}

/// Streams the pane until the board host hangs up, the pane closes or the
/// pane's server stops answering, and types the board host's inputs into
/// it as they come. The mirror's own lines go out as they are, with each
/// input's `result` between them; a failure before the first line goes
/// out as a `result` frame.
async fn stream(
    config: &ServeConfig,
    open: &PaneOpen,
    session_id: String,
    pane: String,
    input: tokio::io::Lines<impl AsyncBufRead + Send + Unpin + 'static>,
    output: &mut (impl AsyncWrite + Send + Unpin),
) -> Result<(), ServeError> {
    let mirrors = Arc::new(Mirrors::new());
    let (stop_tx, stop) = watch::channel(false);
    let stop_tx = Arc::new(stop_tx);
    let body = match view(config, open, &pane, &mirrors, stop.clone()).await {
        Ok(body) => body,
        Err(error) => {
            let (http_status, body) = error.into_result();
            write(
                output,
                &Frame::Result {
                    id: open.id.clone(),
                    http_status,
                    body,
                },
            )
            .await?;
            return Ok(());
        }
    };
    // The board host's inputs, read in order and each answered once it
    // reached the pane. It hangs up by closing its end: the viewer is gone.
    let (answer, mut answers) = mpsc::channel::<Frame>(ASKS);
    let reader = {
        let typist = Typist {
            config: config.clone(),
            open: open.clone(),
            session_id,
            pane,
            mirrors: Arc::clone(&mirrors),
        };
        let hung_up = Arc::clone(&stop_tx);
        tokio::spawn(async move {
            typist.read(input, answer).await;
            hung_up.send_replace(true);
        })
    };
    // Revoking the key ends an open watch at once, as it cuts a live `peer
    // serve` link: the key row is read again whenever another connection
    // committed (`PRAGMA data_version`), on every poll, and no line goes out
    // once the watch is stopped. `federation.accept = false` ends it within a
    // tick.
    let recheck = {
        let mut trust = Trust::open(&config.home, &open.key_id);
        let home = config.home.clone();
        let (poll, tick) = (config.timing.poll, config.timing.tick);
        let stop_tx = Arc::clone(&stop_tx);
        tokio::spawn(async move {
            let mut accept_read = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(poll).await;
                let accept_due = accept_read.elapsed() >= tick;
                if accept_due {
                    accept_read = tokio::time::Instant::now();
                }
                if !trust.still() || (accept_due && !super::federation_accept(&home)) {
                    stop_tx.send_replace(true);
                    return;
                }
            }
        })
    };
    let (stopped, mut stopping) = (stop.clone(), stop);
    let mut lines = body.into_data_stream();
    let mut ended = Ok(());
    // Once stopped, nothing more goes out, but the stream is still read to
    // its end, so a Fit gives the pane back before the process exits.
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let drained = async {
            match deadline {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            chunk = lines.next() => {
                let Some(Ok(chunk)) = chunk else { break };
                if deadline.is_some() || *stopped.borrow() {
                    continue;
                }
                if let Err(error) = put(output, &chunk).await {
                    ended = Err(error.into());
                    stop_tx.send_replace(true);
                }
            }
            Some(frame) = answers.recv(), if deadline.is_none() => {
                if *stopped.borrow() {
                    continue;
                }
                if let Err(error) = write(output, &frame).await {
                    ended = Err(error.into());
                    stop_tx.send_replace(true);
                }
            }
            () = mirror::stopping(&mut stopping), if deadline.is_none() => {
                deadline = Some(tokio::time::Instant::now() + DRAIN_WAIT);
            }
            () = drained => break,
        }
    }
    reader.abort();
    recheck.abort();
    ended
}

async fn put(output: &mut (impl AsyncWrite + Unpin), chunk: &[u8]) -> std::io::Result<()> {
    output.write_all(chunk).await?;
    output.flush().await
}

/// What applies the board host's inputs and Fits to the pane a stream
/// shows.
struct Typist {
    config: ServeConfig,
    open: PaneOpen,
    session_id: String,
    pane: String,
    mirrors: Arc<Mirrors>,
}

impl Typist {
    /// Reads frames until the board host hangs up, applying each input or
    /// Fit in turn and handing on its `result`. Any other frame gets no
    /// answer.
    async fn read(
        self,
        mut input: tokio::io::Lines<impl AsyncBufRead + Send + Unpin>,
        answer: mpsc::Sender<Frame>,
    ) {
        let mut store = Store::open_current(self.config.home.join("state.db")).ok();
        let mut last = 0;
        while let Ok(Some(line)) = input.next_line().await {
            let (id, applied) = match serde_json::from_str::<Frame>(&line) {
                Ok(Frame::PaneInput(frame)) => {
                    let id = frame.id.clone();
                    (id, self.input(&mut store, &mut last, &frame).await)
                }
                Ok(Frame::PaneFit(frame)) => {
                    let id = frame.id.clone();
                    (id, self.fit(&mut store, &mut last, &frame))
                }
                _ => continue,
            };
            let (http_status, body) = match applied {
                Ok(body) => (StatusCode::OK.as_u16(), body),
                Err(error) => error.into_result(),
            };
            let frame = Frame::Result {
                id,
                http_status,
                body,
            };
            if answer.send(frame).await.is_err() {
                return;
            }
        }
    }

    /// Checks a frame on this stream, and that its session still runs.
    fn check(
        &self,
        store: &mut Option<Store>,
        last: &mut u64,
        frame: &Header<'_>,
    ) -> Result<(), ApiError> {
        if store.is_none() {
            *store = Store::open_current(self.config.home.join("state.db")).ok();
        }
        let Some(store) = store.as_ref() else {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "this peer's state does not open",
            ));
        };
        match verify_act(
            store,
            &self.config,
            &self.open,
            frame,
            *last,
            Timestamp::now(),
        ) {
            Ok(()) => {}
            Err(Denied::Refused(reason, message)) => {
                return Err(ApiError::peer_refused(reason, message));
            }
            Err(Denied::Failed(error)) => return Err(ApiError::from(error)),
        }
        *last = frame.seq;
        still_running(store, &self.session_id)
    }

    async fn input(
        &self,
        store: &mut Option<Store>,
        last: &mut u64,
        frame: &PaneInput,
    ) -> Result<Value, ApiError> {
        self.check(store, last, &frame.header())?;
        let input = terminal_keys(frame.keys.clone(), frame.text.clone())?;
        let sent = type_into(&self.mirrors, self.config.holder.clone(), &self.pane, input).await?;
        to_value(input_sent(&self.session_id, sent))
    }

    fn fit(
        &self,
        store: &mut Option<Store>,
        last: &mut u64,
        frame: &PaneFit,
    ) -> Result<Value, ApiError> {
        self.check(store, last, &frame.header())?;
        let fit = mirror::check_fit(frame.cols, frame.rows).map_err(ApiError::usage)?;
        if !self.mirrors.refit(&self.pane, fit) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "refused",
                format!("the stream on {} holds no Fit", self.pane),
            )
            .hint("open the terminal stream with a fit"));
        }
        to_value(fit_sent(&self.session_id, fit))
    }
}

fn to_value(response: impl Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(response).map_err(|error| ApiError::from(tiller_core::Error::from(error)))
}

/// The pane's mirror body: the holder's subscription, fitted to the board
/// when the open asked for it. One `closed` line for a pane whose child has
/// exited.
async fn view(
    config: &ServeConfig,
    open: &PaneOpen,
    pane: &str,
    mirrors: &Mirrors,
    stop: watch::Receiver<bool>,
) -> Result<Body, ApiError> {
    let fit = open
        .fit
        .map(|fit| mirror::check_fit(fit.cols, fit.rows))
        .transpose()
        .map_err(ApiError::usage)?;
    let viewer = mirrors.viewer().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            format!(
                "{} terminal views are open; close one first",
                mirror::MAX_VIEWERS
            ),
        )
    })?;
    let holder = holder(config)?;
    let id = tiller_panes::PaneId::from(pane);
    let lister = Arc::clone(&holder);
    let listing = tokio::task::spawn_blocking(move || lister.list())
        .await
        .map_err(|error| ApiError::from(tiller_core::Error::Internal(error.to_string())))?
        .map_err(|error| holder_error(pane, &error))?;
    match holder::running(&listing, &id) {
        Ok(_) => Ok(crate::holder_mirror::stream(
            holder,
            id,
            fit,
            mirrors.leases(),
            stop,
            viewer,
        )),
        Err(tiller_panes::PanesError::Exited(_)) => {
            let exit = listing.panes.iter().find(|found| found.pane == id);
            Ok(crate::holder_mirror::closed(holder::exit_reason(
                exit.and_then(|found| found.exit.as_ref()),
            )))
        }
        Err(error) => Err(holder_error(pane, &error)),
    }
}

/// Whether a pane link's key is still unrevoked, read again only once
/// another connection has committed since the last look.
struct Trust {
    store: Option<Store>,
    key_id: String,
    data_version: Option<i64>,
}

impl Trust {
    fn open(home: &std::path::Path, key_id: &str) -> Self {
        Self {
            store: Store::open_current(home.join("state.db")).ok(),
            key_id: key_id.to_owned(),
            data_version: None,
        }
    }

    /// A store that does not open or read is no longer trusted.
    fn still(&mut self) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        let Ok(version) = store
            .conn()
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        else {
            return false;
        };
        if self.data_version == Some(version) {
            return true;
        }
        self.data_version = Some(version);
        store.peer_keys().is_ok_and(|keys| {
            keys.iter()
                .any(|key| key.id == self.key_id && key.revoked_at.is_none())
        })
    }
}

async fn write(output: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> std::io::Result<()> {
    let line = frame.line().map_err(std::io::Error::other)?;
    output.write_all(line.as_bytes()).await?;
    output.flush().await
}
