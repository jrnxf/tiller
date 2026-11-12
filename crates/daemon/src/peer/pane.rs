//! Watching a peer's pane (`protocol.md` 10.7): the board host opens a
//! second SSH session to the peer, `tiller peer pane --from <label>`, sends
//! one signed [`PaneOpen`], and gets back either the pane's NDJSON mirror
//! stream (the same lines `POST .../terminal` streams) or one `result`
//! frame, which is a refusal or the pane's scrollback.
//!
//! The peer's half is read only by construction. It runs `herdr terminal
//! session observe` and `herdr pane read` for a pane on this machine and
//! nothing else: there is no frame that carries a key, a paste, a Fit or a
//! prompt answer, and this module never calls a herdr write. The signature
//! covers every field, and the owner's cookie is checked on the board host
//! before any of it is dialed.

use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_core::state::peers::parse_public_key;
use tiller_core::{Store, Timestamp, ids};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::watch;

use super::nonces;
use super::serve::{Denied, ServeConfig, ServeError, current_key, refused, skew_ms};
use super::{Frame, HelloKey};
use crate::api::{ApiError, herdr_only, pane_error, pane_gone, pane_target, saved_machine};
use crate::dto::TerminalHistory;
use crate::mirror::{self, Mirrors};

/// How long `peer pane` waits for the opening [`PaneOpen`].
const OPEN_WAIT: Duration = Duration::from_secs(30);

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
    pub ts: String,
    pub viewer_host_id: String,
    pub peer_host_id: String,
    pub key_id: String,
    pub sig: String,
}

fn open_message(frame: &PaneOpen) -> Vec<u8> {
    json!([
        "tiller-peer/1 pane",
        frame.id,
        frame.session,
        frame.kind.as_str(),
        frame.lines,
        frame.ts,
        frame.viewer_host_id,
        frame.peer_host_id,
        frame.key_id,
    ])
    .to_string()
    .into_bytes()
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
        now: Timestamp,
    ) -> Self {
        let mut frame = PaneOpen {
            id: ids::new_ulid(),
            session: session.to_owned(),
            kind,
            lines,
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
        let Ok(key) = parse_public_key(public_key) else {
            return false;
        };
        let Some(signature) = BASE64
            .decode(&self.sig)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
        else {
            return false;
        };
        key.verify(&open_message(self), &signature).is_ok()
    }
}

/// The checks on the peer, in the order `mutate`'s run (`protocol.md`
/// 10.3 step 3) up to the allowlist: `federation.accept`, an unrevoked key,
/// the signature, `ts` within the skew, this host's `host_id`. A `read`
/// pairing may watch, so there is no scope check, and a pane is no
/// project's record, so there is no owner-marker or project check.
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
    // Last, so only a request that passed every other check is kept.
    nonces::take(&config.home, &frame.id, &frame.ts, config.timing.skew, now)
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
        Ok((_, _, Some(machine))) => Err(saved_machine(&machine)),
        Ok((session_id, pane, None)) => match open.kind {
            PaneKind::History => history(config, &open, session_id, pane).await,
            PaneKind::Stream => {
                return stream(config, &open, pane, lines, &mut output).await;
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
    herdr_only(&pane, "A peer's history")?;
    let lines = open.lines.unwrap_or(mirror::MAX_HISTORY);
    if !(1..=mirror::MAX_HISTORY).contains(&lines) {
        return Err(ApiError::usage(format!(
            "lines is 1 to {}",
            mirror::MAX_HISTORY
        )));
    }
    let herdr = herdr(config)?;
    let target = pane.clone();
    let text = tokio::task::spawn_blocking(move || herdr.read_recent(&target, lines))
        .await
        .map_err(|error| ApiError::from(tiller_core::Error::Internal(error.to_string())))?
        .map_err(|error| pane_error(&pane, &error))?;
    serde_json::to_value(TerminalHistory { session_id, text })
        .map_err(|error| ApiError::from(tiller_core::Error::from(error)))
}

fn herdr(config: &ServeConfig) -> Result<tiller_herdr::client::Cli, ApiError> {
    config.herdr.clone().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "herdr is unreachable: this peer has no herdr configured",
        )
    })
}

/// Streams the pane until the board host hangs up, the pane closes or herdr
/// stops answering. The mirror's own lines go out as they are; a failure
/// before the first one goes out as a `result` frame.
async fn stream(
    config: &ServeConfig,
    open: &PaneOpen,
    pane: String,
    mut input: tokio::io::Lines<impl AsyncBufRead + Send + Unpin + 'static>,
    output: &mut (impl AsyncWrite + Send + Unpin),
) -> Result<(), ServeError> {
    let begun = begin(config, &pane).await;
    let (terminal, viewer, mirrors) = match begun {
        Ok(begun) => begun,
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
    let cli = match herdr(config) {
        Ok(cli) => cli,
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
    let (stop_tx, stop) = watch::channel(false);
    // The board host hangs up by closing its end: the viewer is gone.
    let stop_tx = std::sync::Arc::new(stop_tx);
    let hung_up = stop_tx.clone();
    let hangup = tokio::spawn(async move {
        while let Ok(Some(_)) = input.next_line().await {}
        hung_up.send_replace(true);
    });
    // Revoking the key ends an open watch at once, as it cuts a live `peer
    // serve` link: the key row is read again whenever another connection
    // committed (`PRAGMA data_version`), on every poll, and no line goes out
    // once the watch is stopped. `federation.accept = false` ends it within a
    // tick.
    let recheck = {
        let mut trust = Trust::open(&config.home, &open.key_id);
        let home = config.home.clone();
        let (poll, tick) = (config.timing.poll, config.timing.tick);
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
    let stopped = stop.clone();
    let body = mirror::stream(cli, pane, terminal, None, mirrors.fits(), stop, viewer);
    let mut lines = body.into_data_stream();
    let mut ended = Ok(());
    while let Some(chunk) = lines.next().await {
        let Ok(chunk) = chunk else { break };
        if *stopped.borrow() {
            break;
        }
        if let Err(error) = output.write_all(&chunk).await {
            ended = Err(error.into());
            break;
        }
        if let Err(error) = output.flush().await {
            ended = Err(error.into());
            break;
        }
    }
    hangup.abort();
    recheck.abort();
    ended
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

type Begun = (
    (
        tiller_herdr::client::Cells,
        Option<tiller_herdr::client::Tty>,
    ),
    mirror::Viewer,
    Mirrors,
);

async fn begin(config: &ServeConfig, pane: &str) -> Result<Begun, ApiError> {
    // A peer is not served the holder's panes yet: the view says so, never
    // that the pane is closed.
    herdr_only(pane, "A peer's live view")?;
    let mirrors = Mirrors::new(&config.home);
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
    let herdr = herdr(config)?;
    let asked = pane.to_owned();
    let terminal = tokio::task::spawn_blocking(move || herdr.terminal(&asked))
        .await
        .map_err(|error| ApiError::from(tiller_core::Error::Internal(error.to_string())))?;
    match terminal {
        Ok(Some(terminal)) => Ok((terminal, viewer, mirrors)),
        Ok(None) => Err(pane_gone(pane)),
        Err(error) => Err(pane_error(pane, &error)),
    }
}

async fn write(output: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> std::io::Result<()> {
    let line = frame.line().map_err(std::io::Error::other)?;
    output.write_all(line.as_bytes()).await?;
    output.flush().await
}
