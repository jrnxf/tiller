//! The holder socket client (`terminal-holder.md` 2.4, 4.1).
//!
//! [`HolderClient`] holds one connection for requests. It sends one request
//! at a time and waits for its reply, so a reply never has to be matched
//! against another caller's. Each [`Subscription`] has a connection of its
//! own: its kind 1 frames never wait behind a request, and a size it sets
//! through its lease ends when it is dropped, because its connection ends.
//!
//! A connection that fails (no answer in time, a closed socket, a bad frame)
//! is not used again: every later call on that client is `Unreachable`, and
//! the caller connects again. Nothing here retries, so a holder restart is
//! never hidden behind a quiet reconnect.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::error::{PanesError, Result};
use crate::frame::{Frame, read_frame, write_frame};
use crate::handshake::{Hello, ProtoRange, SUPPORTED, Welcome};
use crate::message::{Exit, HolderMessage, Listing, Pane, PaneId, Request, Spawn, SyncMode};
use crate::panes::Panes;

/// How long the handshake may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a request may wait for its reply.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One handshaken connection.
struct Conn {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Conn {
    /// Connects to `path` and does the handshake as `client`.
    fn open(path: &Path, client: &str) -> Result<(Self, Welcome)> {
        let stream = UnixStream::connect(path)
            .map_err(|err| PanesError::Unreachable(format!("{}: {err}", path.display())))?;
        let timeout = Some(HANDSHAKE_TIMEOUT);
        stream
            .set_read_timeout(timeout)
            .and_then(|()| stream.set_write_timeout(timeout))?;
        let writer = stream.try_clone()?;
        let mut conn = Conn {
            reader: BufReader::new(stream),
            writer,
        };
        conn.send(&Request::Hello(Hello::new(client)))?;
        let welcome = match conn.read_message()? {
            HolderMessage::Welcome(welcome) => welcome,
            HolderMessage::Refused(refused) => {
                return Err(PanesError::Version {
                    client: SUPPORTED,
                    holder: refused.proto,
                });
            }
            HolderMessage::Error { code, message, .. } => {
                return Err(PanesError::from_reply("hello", code, message));
            }
            other => {
                return Err(PanesError::Protocol(format!(
                    "the holder answered hello with {other:?}"
                )));
            }
        };
        if !SUPPORTED.contains(welcome.proto) {
            return Err(PanesError::Version {
                client: SUPPORTED,
                holder: ProtoRange {
                    min: welcome.proto,
                    max: welcome.proto,
                },
            });
        }
        Ok((conn, welcome))
    }

    /// Sets how long a read or a write may block; `None` for no limit.
    fn set_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.writer.set_read_timeout(timeout)?;
        self.writer.set_write_timeout(timeout)?;
        Ok(())
    }

    fn send(&mut self, request: &Request) -> Result<()> {
        let text = serde_json::to_string(request)
            .map_err(|err| PanesError::Protocol(format!("a request did not encode: {err}")))?;
        self.write(&Frame::Message(text))
    }

    fn write(&mut self, frame: &Frame) -> Result<()> {
        write_frame(&self.writer, frame).map_err(PanesError::from_frame)
    }

    /// The next frame. The end of the stream is `None`.
    fn read(&mut self) -> Result<Option<Frame>> {
        read_frame(&mut self.reader).map_err(PanesError::from_frame)
    }

    /// The next frame, which must be a JSON message.
    fn read_message(&mut self) -> Result<HolderMessage> {
        match self.read()? {
            Some(Frame::Message(text)) => parse(&text),
            Some(frame) => Err(PanesError::Protocol(format!(
                "a kind {} frame where a message was due",
                frame.kind()
            ))),
            None => Err(PanesError::Unreachable(
                "the holder closed the connection".into(),
            )),
        }
    }

    /// Sends `request` and reads until its reply. Messages for nobody (a
    /// type from a newer holder) are skipped; an `error` reply is the error.
    fn call(&mut self, name: &'static str, request: &Request) -> Result<HolderMessage> {
        let id = request.id();
        self.send(request)?;
        loop {
            let message = self.read_message()?;
            if message == HolderMessage::Unknown {
                continue;
            }
            if message.reply_to() != id {
                return Err(PanesError::Protocol(format!(
                    "{name} (id {id:?}) got a message for another request: {message:?}"
                )));
            }
            return match message {
                HolderMessage::Error { code, message, .. } => {
                    Err(PanesError::from_reply(name, code, message))
                }
                reply => Ok(reply),
            };
        }
    }
}

fn parse(text: &str) -> Result<HolderMessage> {
    serde_json::from_str(text)
        .map_err(|err| PanesError::Protocol(format!("an unreadable message ({err}): {text}")))
}

fn unexpected(request: &str, reply: &HolderMessage) -> PanesError {
    PanesError::Protocol(format!("the holder answered {request} with {reply:?}"))
}

/// Whether a failed call leaves the connection fit for the next one. An
/// `error` reply does; a broken transport or frame does not.
fn keeps_connection(err: &PanesError) -> bool {
    matches!(
        err,
        PanesError::NotFound(_) | PanesError::Exited(_) | PanesError::Refused { .. }
    )
}

/// A client of the terminal holder.
pub struct HolderClient {
    path: PathBuf,
    client: String,
    welcome: Welcome,
    /// The request connection, or why it is no longer used.
    conn: Mutex<std::result::Result<Conn, String>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for HolderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderClient")
            .field("path", &self.path)
            .field("client", &self.client)
            .field("welcome", &self.welcome)
            .finish_non_exhaustive()
    }
}

impl HolderClient {
    /// Connects to the holder at `path` (see [`socket_path`](crate::socket_path))
    /// and does the handshake as `client` (`daemon`, `attach` or `peer`).
    /// A holder with no protocol version in common is
    /// [`PanesError::Version`].
    pub fn connect(path: impl Into<PathBuf>, client: impl Into<String>) -> Result<Self> {
        let path = path.into();
        let client = client.into();
        let (conn, welcome) = Conn::open(&path, &client)?;
        conn.set_timeout(Some(REQUEST_TIMEOUT))?;
        Ok(Self {
            path,
            client,
            welcome,
            conn: Mutex::new(Ok(conn)),
            next_id: AtomicU64::new(1),
        })
    }

    /// The holder's handshake answer: its version, `instance` and features.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn conn(&self) -> MutexGuard<'_, std::result::Result<Conn, String>> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `work` on the request connection, and retires the connection
    /// when the call broke it.
    fn with_conn<T>(&self, work: impl FnOnce(&mut Conn) -> Result<T>) -> Result<T> {
        let mut guard = self.conn();
        let conn = match guard.as_mut() {
            Ok(conn) => conn,
            Err(why) => {
                return Err(PanesError::Unreachable(format!(
                    "the connection to the holder failed earlier ({why}); connect again"
                )));
            }
        };
        let result = work(conn);
        if let Err(err) = &result
            && !keeps_connection(err)
        {
            *guard = Err(err.to_string());
        }
        result
    }

    fn call(&self, name: &'static str, request: Request) -> Result<HolderMessage> {
        self.with_conn(|conn| conn.call(name, &request))
    }

    /// Writes raw bytes to the pane with this `num` (a kind 2 frame), as
    /// they are. The holder sends no reply.
    pub fn raw_input(&self, num: u32, bytes: &[u8]) -> Result<()> {
        let frame = Frame::Input {
            num,
            bytes: bytes.to_vec(),
        };
        // A frame over the limit is the caller's mistake, found before any
        // byte is written, so the connection stays.
        let wire = frame.to_bytes().map_err(|err| PanesError::Refused {
            request: "raw input",
            code: crate::message::ErrorCode::BadRequest,
            message: err.to_string(),
        })?;
        self.with_conn(|conn| {
            use std::io::Write;
            (&conn.writer)
                .write_all(&wire)
                .map_err(|err| PanesError::from_frame(err.into()))
        })
    }

    /// Subscribes to a pane's output on a connection of its own. The
    /// [`Subscription`] yields one repaint, then live output.
    pub fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> Result<Subscription> {
        let (mut conn, _) = Conn::open(&self.path, &self.client)?;
        conn.set_timeout(Some(REQUEST_TIMEOUT))?;
        let request = Request::Subscribe {
            id: self.id(),
            pane: pane.clone(),
            sync: opts.sync,
        };
        let (stream, cols, rows) = match conn.call("subscribe", &request)? {
            HolderMessage::Subscribed {
                stream, cols, rows, ..
            } => (stream, cols, rows),
            reply => return Err(unexpected("subscribe", &reply)),
        };
        // Output may be quiet for hours; only writes keep a limit.
        conn.writer.set_read_timeout(None)?;
        Ok(Subscription {
            pane: pane.clone(),
            stream,
            cols,
            rows,
            lease: format!("stream-{stream}"),
            conn,
            next_id: AtomicU64::new(request.id().map_or(1, |id| id + 1)),
            state: StreamState::AwaitingRepaint,
        })
    }
}

impl Panes for HolderClient {
    fn spawn(&self, req: &Spawn) -> Result<Pane> {
        let request = Request::Spawn {
            id: self.id(),
            spawn: req.clone(),
        };
        match self.call("spawn", request)? {
            HolderMessage::Pane { pane, .. } => Ok(pane),
            reply => Err(unexpected("spawn", &reply)),
        }
    }

    fn list(&self) -> Result<Listing> {
        match self.call("list", Request::List { id: self.id() })? {
            HolderMessage::Panes { listing, .. } => Ok(listing),
            reply => Err(unexpected("list", &reply)),
        }
    }
}

/// How to subscribe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Subscribe {
    pub sync: SyncMode,
}

/// What a subscription yields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    /// The first frame: bytes that draw the whole screen on an empty
    /// terminal of this size.
    Repaint {
        cols: u16,
        rows: u16,
        bytes: Vec<u8>,
    },
    /// The PTY's bytes, as the child wrote them.
    Output(Vec<u8>),
    /// The holder took a [`Subscription::resize`]: the PTY's size now.
    Resized { cols: u16, rows: u16 },
    /// The pane's child was reaped.
    Exited(Exit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamState {
    /// Waiting for the `repaint` message.
    AwaitingRepaint,
    /// The `repaint` message came; its kind 1 frame is next.
    Repainting { cols: u16, rows: u16 },
    /// Live output.
    Live,
    /// The stream ended or failed; nothing more comes.
    Ended,
}

/// A pane's output on a connection of its own. Dropping it ends the
/// connection, and with it any size it set.
pub struct Subscription {
    pane: PaneId,
    stream: u32,
    cols: u16,
    rows: u16,
    /// The lease id this subscription's `resize` names.
    lease: String,
    conn: Conn,
    next_id: AtomicU64,
    state: StreamState,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("pane", &self.pane)
            .field("stream", &self.stream)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Subscription {
    pub fn pane(&self) -> &PaneId {
        &self.pane
    }

    /// The `stream` number on this subscription's kind 1 frames.
    pub fn stream(&self) -> u32 {
        self.stream
    }

    /// The PTY's size when the subscription started.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Asks the holder to set the PTY to this size, under this
    /// subscription's lease (the board's Fit). The holder's answer comes
    /// through [`Subscription::recv`] as [`Update::Resized`], or as an error.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let request = Request::Resize {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            pane: self.pane.clone(),
            cols,
            rows,
            lease: self.lease.clone(),
        };
        let sent = self.conn.send(&request);
        if sent.is_err() {
            self.state = StreamState::Ended;
        }
        sent
    }

    /// The next update. `None` once the stream has ended: the holder closed
    /// it (a pane forgotten, a client too slow) or an earlier call failed.
    /// An `error` reply to a `resize` is an error that leaves the stream
    /// open; any other error ends it.
    pub fn recv(&mut self) -> Result<Option<Update>> {
        if self.state == StreamState::Ended {
            return Ok(None);
        }
        let result = self.read_update();
        match &result {
            Ok(None) => self.state = StreamState::Ended,
            Err(err) if !keeps_connection(err) => self.state = StreamState::Ended,
            _ => {}
        }
        result
    }

    fn read_update(&mut self) -> Result<Option<Update>> {
        loop {
            let Some(frame) = self.conn.read()? else {
                return Ok(None);
            };
            match frame {
                Frame::Output { stream, bytes } => {
                    if stream != self.stream {
                        return Err(PanesError::Protocol(format!(
                            "output for stream {stream} on the connection of stream {}",
                            self.stream
                        )));
                    }
                    return match self.state {
                        StreamState::Repainting { cols, rows } => {
                            self.state = StreamState::Live;
                            Ok(Some(Update::Repaint { cols, rows, bytes }))
                        }
                        StreamState::Live => Ok(Some(Update::Output(bytes))),
                        StreamState::AwaitingRepaint | StreamState::Ended => Err(
                            PanesError::Protocol("output before the repaint message".into()),
                        ),
                    };
                }
                Frame::Input { .. } => {
                    return Err(PanesError::Protocol(
                        "the holder sent a raw input frame".into(),
                    ));
                }
                Frame::Message(text) => match parse(&text)? {
                    HolderMessage::Repaint { stream, cols, rows } if stream == self.stream => {
                        if self.state != StreamState::AwaitingRepaint {
                            return Err(PanesError::Protocol(
                                "a second repaint message on one stream".into(),
                            ));
                        }
                        self.state = StreamState::Repainting { cols, rows };
                    }
                    HolderMessage::Ok {
                        cols: Some(cols),
                        rows: Some(rows),
                        ..
                    } => return Ok(Some(Update::Resized { cols, rows })),
                    HolderMessage::Exited { pane, exit } if pane == self.pane => {
                        return Ok(Some(Update::Exited(exit)));
                    }
                    HolderMessage::Error { code, message, .. } => {
                        return Err(PanesError::from_reply("resize", code, message));
                    }
                    HolderMessage::Unknown => {}
                    other => {
                        return Err(PanesError::Protocol(format!(
                            "an unexpected message on stream {}: {other:?}",
                            self.stream
                        )));
                    }
                },
            }
        }
    }
}
