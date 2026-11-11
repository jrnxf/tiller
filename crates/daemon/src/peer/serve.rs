//! `tiller peer serve` (`protocol.md` 10.2): the peer's half of a link, run
//! by sshd for the board host's daemon. It reads `hello`, answers `welcome`
//! or `refused`, then sends a `snapshot` or the replay, every new `events`
//! row as an `event`, a `beat` every 10 s, and a `result` for each `query`
//! and each `mutate` (10.3, applied by [`super::forward`]).
//!
//! It serves this host's own records only. It has no client half: it never
//! opens a link of its own, so a board host that is also some other host's
//! peer never forwards that host's rows on.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::Value;
use tiller_core::domain::{HostKind, PeerKey};
use tiller_core::error::READ_ONLY_MARKER;
use tiller_core::state::peers::PEER_PROTOCOL;
use tiller_core::{Store, Timestamp};
use tiller_herdr::client::Cli;

use super::forward::{self, Mutate};
use super::nonces;
use super::{Frame, Hello, PeerQueries, PeerSnapshot, Timing, Welcome, federation_accept};
use crate::api::{ApiError, pane_tail_of};
use crate::dto::{Doctor, ErrorBody, MachinesResponse};
use crate::sse::{self, Opening};
use crate::views::{self, Filter, LocalHealth};

/// Rows sent per read while replaying or streaming.
const BATCH: i64 = 200;

/// How long `peer serve` waits for the opening `hello`.
const HELLO_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("peer link: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Core(#[from] tiller_core::Error),
    #[error("peer link frame: {0}")]
    Json(#[from] serde_json::Error),
    /// The `hello` was refused, or the pairing changed under a live link.
    #[error("refused the link ({reason}): {message}")]
    Refused { reason: String, message: String },
}

/// What `peer serve` runs with.
pub struct ServeConfig {
    /// This host's `~/.tiller`.
    pub home: PathBuf,
    /// `--from`: the board host's label.
    pub from: String,
    pub timing: Timing,
    /// The local herdr server, for a run's pane tail; none in tests.
    pub herdr: Option<Cli>,
    /// Starts an orchestrator for a forwarded start; none refuses it 503.
    pub orchestrators: Option<std::sync::Arc<dyn crate::api::OrchestratorStart>>,
}

/// Serves one link until the board host hangs up, `input` ends, the
/// pairing changes, or a write fails.
pub fn serve(
    config: &ServeConfig,
    input: impl BufRead + Send + 'static,
    mut output: impl Write,
) -> Result<(), ServeError> {
    let frames = read_frames(input);
    let hello = match frames.recv_timeout(HELLO_WAIT) {
        Ok(Frame::Hello(hello)) => hello,
        // No hello: nothing was asked, so nothing is answered.
        Ok(_) | Err(_) => return Ok(()),
    };
    let store = Store::open_current(config.home.join("state.db"))?;
    let key = match check_hello(&store, config, &hello, Timestamp::now()) {
        Ok(key) => key,
        Err(denied) => return refuse(&mut output, denied),
    };
    let host = store.this_host()?;
    send(
        &mut output,
        &Frame::Welcome(Welcome {
            host_id: host.id,
            label: host.label,
            schema_version: tiller_core::migrate::LATEST,
            tiller_version: env!("CARGO_PKG_VERSION").to_owned(),
            server_time: Timestamp::now().to_string(),
        }),
    )?;
    let mut link = Link {
        store,
        config,
        key,
        cursor: 0,
        data_version: None,
    };
    // A board host with no copy (`last_seq` null) gets a snapshot, as one
    // whose copy is too old or ahead of this database does.
    let opening = match hello.last_seq {
        None => Opening::Reset { newest: 0 },
        Some(seq) => sse::opening(Some(seq), link.store.last_seq()?),
    };
    link.cursor = match opening {
        Opening::Replay(after) => after,
        Opening::Reset { .. } => link.send_snapshot(&mut output)?,
    };
    link.run(&frames, &mut output)
}

/// Why a link is not served, or a `mutate` is refused.
pub(super) enum Denied {
    /// A `refused` reason and a line for the owner.
    Refused(&'static str, String),
    /// This host could not check: no `refused` is sent, the link just ends,
    /// and the board host retries with its backoff.
    Failed(tiller_core::Error),
}

impl From<tiller_core::Error> for Denied {
    fn from(error: tiller_core::Error) -> Self {
        Self::Failed(error)
    }
}

pub(super) fn refused<T>(reason: &'static str, message: String) -> Result<T, Denied> {
    Err(Denied::Refused(reason, message))
}

/// How far a frame's `ts` is from `now`, in ms; `u64::MAX` when it does
/// not parse.
pub(super) fn skew_ms(ts: &str, now: Timestamp) -> u64 {
    ts.parse::<Timestamp>()
        .map(|ts| (now.as_millis() - ts.as_millis()).unsigned_abs())
        .unwrap_or(u64::MAX)
}

/// The hello checks, in `protocol.md` 10.2 order.
fn check_hello(
    store: &Store,
    config: &ServeConfig,
    hello: &Hello,
    now: Timestamp,
) -> Result<PeerKey, Denied> {
    if hello.v != PEER_PROTOCOL {
        return refused(
            "version",
            format!("this host speaks {PEER_PROTOCOL}, not {}", hello.v),
        );
    }
    let key = current_key(store, config, &hello.key_id)?;
    if key.viewer_host_id != hello.viewer_host_id {
        return refused(
            "unknown_key",
            format!("key {} belongs to another board host", key.id),
        );
    }
    let host = store.this_host()?;
    if !hello.verifies(&key.public_key, &host.id) {
        return refused("bad_sig", "the hello signature is not valid".to_owned());
    }
    let skew = skew_ms(&hello.ts, now);
    if u128::from(skew) > config.timing.skew.as_millis() {
        return refused(
            "clock_skew",
            format!("the hello is {skew} ms from this host's clock"),
        );
    }
    if hello.from != key.viewer_label || config.from != key.viewer_label {
        return refused(
            "unknown_key",
            format!("key {} was accepted for {}", key.id, key.viewer_label),
        );
    }
    // Last, so only a hello that passed every other check is kept.
    nonces::take(
        &config.home,
        &hello.nonce,
        &hello.ts,
        config.timing.skew,
        now,
    )?;
    Ok(key)
}

/// `federation.accept`, then the key row: what every `query` and every
/// tick re-reads, so a changed pairing cuts a live link at once.
pub(super) fn current_key(
    store: &Store,
    config: &ServeConfig,
    key_id: &str,
) -> Result<PeerKey, Denied> {
    if !federation_accept(&config.home) {
        return refused("accept_off", "config federation.accept is false".to_owned());
    }
    let keys = store.peer_keys()?;
    let Some(key) = keys.into_iter().find(|key| key.id == key_id) else {
        return refused("unknown_key", format!("no key {key_id}"));
    };
    if key.revoked_at.is_some() {
        return refused("revoked", format!("key {key_id} is revoked"));
    }
    Ok(key)
}

fn refuse(output: &mut impl Write, denied: Denied) -> Result<(), ServeError> {
    let (reason, message) = match denied {
        Denied::Refused(reason, message) => (reason, message),
        Denied::Failed(error) => return Err(error.into()),
    };
    send(
        output,
        &Frame::Refused {
            reason: reason.to_owned(),
            message: Some(message.clone()),
        },
    )?;
    Err(ServeError::Refused {
        reason: reason.to_owned(),
        message,
    })
}

/// Reads frames on a thread of its own, so the link can poll `state.db`
/// between them. A line that does not parse is skipped; the channel closes
/// when `input` ends.
fn read_frames(input: impl BufRead + Send + 'static) -> mpsc::Receiver<Frame> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in input.lines() {
            let Ok(line) = line else { return };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Frame>(&line) {
                Ok(frame) => {
                    if tx.send(frame).is_err() {
                        return;
                    }
                }
                Err(error) => tracing::warn!(%error, "peer frame skipped"),
            }
        }
    });
    rx
}

fn send(output: &mut impl Write, frame: &Frame) -> Result<(), ServeError> {
    output.write_all(frame.line()?.as_bytes())?;
    output.flush()?;
    Ok(())
}

/// A link past its `welcome`.
struct Link<'a> {
    store: Store,
    config: &'a ServeConfig,
    key: PeerKey,
    /// The newest `seq` sent.
    cursor: i64,
    data_version: Option<i64>,
}

impl Link<'_> {
    fn run(
        &mut self,
        frames: &mpsc::Receiver<Frame>,
        output: &mut impl Write,
    ) -> Result<(), ServeError> {
        let timing = self.config.timing;
        let mut beat = Instant::now();
        let mut tick = Instant::now();
        self.send_events(output, true)?;
        loop {
            match frames.recv_timeout(timing.poll) {
                Ok(Frame::Query { id, path }) => {
                    self.recheck(output)?;
                    let (http_status, body) = self.answer(&path);
                    send(
                        output,
                        &Frame::Result {
                            id,
                            http_status,
                            body,
                        },
                    )?;
                }
                Ok(Frame::Mutate(frame)) => {
                    // The checks run before anything is written; a refusal
                    // is a 502 `result` and the link stays up.
                    let (http_status, body) = self.mutate(&frame);
                    // This link's own write does not move `data_version`.
                    self.send_events(output, true)?;
                    send(
                        output,
                        &Frame::Result {
                            id: frame.id,
                            http_status,
                            body,
                        },
                    )?;
                    self.recheck(output)?;
                }
                Ok(Frame::Refresh) => {
                    self.send_events(output, true)?;
                    let seq = self.send_snapshot(output)?;
                    self.cursor = self.cursor.max(seq);
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
            if tick.elapsed() >= timing.tick {
                tick = Instant::now();
                self.recheck(output)?;
            }
            self.send_events(output, false)?;
            if beat.elapsed() >= timing.beat {
                beat = Instant::now();
                send(
                    output,
                    &Frame::Beat {
                        server_time: Timestamp::now().to_string(),
                        last_event_seq: self.cursor,
                    },
                )?;
            }
        }
    }

    /// Verifies a `mutate` (`protocol.md` 10.3 step 3) and applies it.
    fn mutate(&mut self, frame: &Mutate) -> (u16, Value) {
        match forward::verify(&self.store, self.config, &self.key, frame, Timestamp::now()) {
            Ok(op) => match std::fs::read_to_string(self.config.home.join(READ_ONLY_MARKER)) {
                // The daemon found `state.db` damaged (`recovery.md` R10):
                // a forwarded write is refused as a local one is.
                Ok(reason) => crate::api::read_only(reason.trim()).into_result(),
                Err(_) => forward::apply(&mut self.store, self.config, &self.key, frame, op),
            },
            Err(Denied::Refused(reason, message)) => {
                tracing::warn!(reason, %message, "forwarded mutation refused");
                ApiError::peer_refused(reason, message).into_result()
            }
            Err(Denied::Failed(error)) => ApiError::from(error).into_result(),
        }
    }

    /// Re-reads `federation.accept` and the key row; a change sends
    /// `refused` and closes the link.
    fn recheck(&self, output: &mut impl Write) -> Result<(), ServeError> {
        match current_key(&self.store, self.config, &self.key.id) {
            Ok(key) if key.scope == self.key.scope => Ok(()),
            Ok(_) => refuse(
                output,
                Denied::Refused("revoked", "the pairing's scope changed".to_owned()),
            ),
            Err(denied) => refuse(output, denied),
        }
    }

    /// Sends every row after the cursor when another connection committed
    /// since the last look (`PRAGMA data_version`), or always with `force`.
    fn send_events(&mut self, output: &mut impl Write, force: bool) -> Result<(), ServeError> {
        let version: i64 = self
            .store
            .conn()
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(tiller_core::Error::from)?;
        if !force && self.data_version == Some(version) {
            return Ok(());
        }
        self.data_version = Some(version);
        // The commit may be this key's revoke: the key row is read again
        // before any frame goes out, so the link closes at once, not on the
        // next tick.
        self.recheck(output)?;
        let local = LocalHealth::default();
        loop {
            let now = Timestamp::now();
            let rows = self.store.events_since(self.cursor, BATCH)?;
            let full = i64::try_from(rows.len()).unwrap_or(i64::MAX) >= BATCH;
            for event in rows {
                let change = views::change(&self.store, &local, &event, now);
                send(output, &Frame::Event(Box::new(change)))?;
                self.cursor = event.seq;
            }
            if !full {
                return Ok(());
            }
        }
    }

    /// Sends a `snapshot` and returns its `last_event_seq`.
    fn send_snapshot(&self, output: &mut impl Write) -> Result<i64, ServeError> {
        let snapshot = own_snapshot(&self.store, Timestamp::now())?;
        let seq = snapshot.last_event_seq;
        send(output, &Frame::Snapshot(Box::new(snapshot)))?;
        Ok(seq)
    }

    /// One allowlisted single-entity GET, or this host's checks, as the
    /// board API would answer it.
    fn answer(&self, path: &str) -> (u16, Value) {
        match self.read(path) {
            Ok(body) => (StatusCode::OK.as_u16(), body),
            Err(error) => {
                let (status, body) = error.into_parts();
                (status.as_u16(), error_value(&body))
            }
        }
    }

    fn read(&self, path: &str) -> Result<Value, ApiError> {
        let query = parse_query(path)?;
        let store = &self.store;
        let body = match query {
            Query::Card(id) => serde_json::to_value(views::card_detail(store, &id)?),
            Query::Timeline(id, before) => {
                serde_json::to_value(views::timeline(store, &id, before)?)
            }
            Query::Run(id) => {
                let (mut detail, pane) = views::run_detail(store, &id)?;
                if let (Some(herdr), Some(target)) = (&self.config.herdr, pane) {
                    detail.pane_tail = pane_tail_of(herdr.clone(), target);
                }
                serde_json::to_value(detail)
            }
            Query::Doctor => serde_json::to_value(self.doctor()?),
        };
        body.map_err(|error| ApiError::from(tiller_core::Error::from(error)))
    }

    /// The checks `tiller doctor` runs, run on this host now, as its own
    /// board's `GET /api/v1/doctor` answers them. Read only: the board host
    /// shows them and can act on none.
    fn doctor(&self) -> Result<Doctor, ApiError> {
        let checked_at = Timestamp::now();
        let report = self
            .config
            .orchestrators
            .as_ref()
            .and_then(|services| services.doctor())
            .ok_or_else(|| {
                tiller_core::Error::Unavailable("this host cannot run its checks".into())
            })?;
        Ok(Doctor {
            checked_at: checked_at.to_string(),
            ok: report.ok,
            checks: report.checks,
            merge_gates: report.merge_gates,
        })
    }
}

fn error_value(body: &ErrorBody) -> Value {
    serde_json::to_value(body).unwrap_or(Value::Null)
}

/// An allowlisted `query` path (`protocol.md` 10.2).
#[derive(Debug, PartialEq, Eq)]
enum Query {
    Card(String),
    Timeline(String, Option<i64>),
    Run(String),
    Doctor,
}

fn parse_query(path: &str) -> Result<Query, ApiError> {
    let usage = |message: &str| ApiError::from(tiller_core::Error::Usage(message.to_owned()));
    let (route, params) = path.split_once('?').unwrap_or((path, ""));
    let mut before = None;
    for param in params.split('&').filter(|param| !param.is_empty()) {
        match param.split_once('=').unwrap_or((param, "")) {
            ("host", _) => return Err(usage("a peer serves only its own records")),
            ("before", seq) => {
                before = Some(seq.parse().map_err(|_| usage("before is a seq"))?);
            }
            (name, _) => return Err(usage(&format!("unknown query parameter {name:?}"))),
        }
    }
    let segments: Vec<&str> = route.trim_start_matches('/').split('/').collect();
    let query = match segments.as_slice() {
        ["cards", id] if before.is_none() => Query::Card((*id).to_owned()),
        ["cards", id, "timeline"] => Query::Timeline((*id).to_owned(), before),
        ["runs", id] if before.is_none() => Query::Run((*id).to_owned()),
        ["doctor"] if before.is_none() => Query::Doctor,
        _ => {
            return Err(usage(
                "a query is cards/{id}, cards/{id}/timeline, runs/{id} or doctor",
            ));
        }
    };
    match &query {
        Query::Card(id) | Query::Timeline(id, _) | Query::Run(id) if id.is_empty() => {
            Err(usage("the query names no id"))
        }
        _ => Ok(query),
    }
}

/// This host's own rows, built by the same views as the boot snapshot, in
/// one read transaction with `last_event_seq` (`protocol.md` 10.2).
pub fn own_snapshot(store: &Store, now: Timestamp) -> tiller_core::Result<PeerSnapshot> {
    store.conn().execute_batch("BEGIN")?;
    let built = (|| -> tiller_core::Result<PeerSnapshot> {
        let last_event_seq = store.last_seq()?;
        let local = HostKind::Local.to_string();
        let machines = views::machines(store, &LocalHealth::default(), now)?
            .machines
            .into_iter()
            .filter(|machine| machine.kind == local)
            .collect();
        Ok(PeerSnapshot {
            server_time: now.to_string(),
            last_event_seq,
            queries: PeerQueries {
                board: views::board(store, None, Filter::All, None)?,
                attention: views::attention(store)?,
                agents: views::agents(store, None)?,
                machines: MachinesResponse { machines },
                projects: views::projects(store, now)?,
            },
        })
    })();
    let ended = store.conn().execute_batch("COMMIT");
    let snapshot = built?;
    ended?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_are_allowlisted() {
        assert_eq!(
            parse_query("cards/alpha-1").ok(),
            Some(Query::Card("alpha-1".into()))
        );
        assert_eq!(
            parse_query("cards/alpha-1/timeline?before=40").ok(),
            Some(Query::Timeline("alpha-1".into(), Some(40)))
        );
        assert_eq!(
            parse_query("runs/01JC").ok(),
            Some(Query::Run("01JC".into()))
        );
        assert_eq!(parse_query("doctor").ok(), Some(Query::Doctor));
        for refused in [
            "doctor?before=4",
            "doctor/again",
            "cards/alpha-1?host=other",
            "cards",
            "board",
            "cards/alpha-1/comment",
            "runs/01JC?before=4",
            "cards/",
        ] {
            assert!(parse_query(refused).is_err(), "{refused}");
        }
    }
}
