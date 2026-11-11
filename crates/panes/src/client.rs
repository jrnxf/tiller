//! The holder socket client (`terminal-holder.md` 2.4, 4.1).
//!
//! [`HolderClient`] holds one connection for requests. It sends one request
//! at a time and waits for its reply, so a reply never has to be matched
//! against another caller's. Raw input takes only the write side, so a
//! keystroke never waits for another call's reply. Each [`Subscription`]
//! has a connection of its own: its kind 1 frames never wait behind a
//! request, and a size it sets through its lease ends when it is dropped,
//! because its connection ends. A [`StreamHandle`] resizes or ends a
//! subscription from another thread while one thread waits in
//! [`Subscription::recv`].
//!
//! A connection that fails (a timeout, a closed socket, a bad frame)
//! is not used again: every later call on that client is `Unreachable`, and
//! the caller connects again. Nothing here retries, so a holder restart is
//! never hidden behind a quiet reconnect.

use std::collections::VecDeque;
use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::error::{PanesError, Result};
use crate::frame::{Frame, FrameError, encode_input, read_frame};
use crate::handshake::{Hello, ProtoRange, SUPPORTED, Welcome};
use crate::message::{
    ErrorCode, Exit, HolderMessage, Listing, Pane, PaneId, Request, Spawn, SyncMode,
};
use crate::panes::Panes;
use crate::paths::check_socket_path;

/// How long the handshake may take in all.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a request may take in all, from its first byte out to its
/// reply's last byte in, and how long one frame's write may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a subscription's socket stays open for reads after a broken
/// resize, so what was buffered still comes. Then it shuts, so a holder that
/// keeps the connection open cannot keep the broken stream alive.
const BROKEN_GRACE: Duration = Duration::from_secs(2);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One end of a socket whose reads and writes stop at a deadline. Each
/// read or write sets the socket's timeout to the time left, so a peer that
/// trickles bytes cannot stretch a call past it. With no deadline, the
/// socket's own timeout stands.
struct Timed {
    stream: UnixStream,
    deadline: Option<Instant>,
}

impl Timed {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            deadline: None,
        }
    }

    fn left(&self) -> io::Result<Option<Duration>> {
        let Some(deadline) = self.deadline else {
            return Ok(None);
        };
        match deadline.saturating_duration_since(Instant::now()) {
            left if left.is_zero() => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the holder did not keep up in time",
            )),
            left => Ok(Some(left)),
        }
    }

    /// Reads with no limit from here on, as a subscription's do.
    fn unlimit_reads(&mut self) -> io::Result<()> {
        self.deadline = None;
        self.stream.set_read_timeout(None)
    }
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(left) = self.left()? {
            self.stream.set_read_timeout(Some(left))?;
        }
        self.stream.read(buf)
    }
}

impl Write for Timed {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(left) = self.left()? {
            self.stream.set_write_timeout(Some(left))?;
        }
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// A frame's wire bytes. A frame too large to send is the caller's mistake,
/// found before any byte is written, so it is a refusal that keeps the
/// connection.
fn encode(name: &'static str, frame: &Frame) -> Result<Vec<u8>> {
    frame.to_bytes().map_err(|err| refused(name, &err))
}

fn refused(name: &'static str, err: impl std::fmt::Display) -> PanesError {
    PanesError::Refused {
        request: name,
        code: ErrorCode::BadRequest,
        message: format!("not sent: {err}"),
    }
}

/// A request's wire bytes, as [`encode`].
fn encode_request(name: &'static str, request: &Request) -> Result<Vec<u8>> {
    let text = serde_json::to_string(request)
        .map_err(|err| refused(name, format_args!("the request did not encode: {err}")))?;
    encode(name, &Frame::Message(text))
}

/// Writes `wire` before `deadline`.
fn send(writer: &mut Timed, wire: &[u8], deadline: Instant) -> Result<()> {
    writer.deadline = Some(deadline);
    writer
        .write_all(wire)
        .map_err(|err| PanesError::from_frame(err.into()))
}

/// The next frame, which must be a JSON message.
fn read_message(reader: &mut BufReader<Timed>) -> Result<HolderMessage> {
    match read_frame(reader).map_err(PanesError::from_frame)? {
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

/// Reads until the reply to request `id`, before `deadline`. Messages for
/// nobody (a type from a newer holder) are skipped; an `error` reply is the
/// error. An `error` with no `id` answers this request, the only one in
/// flight.
fn await_reply(
    reader: &mut BufReader<Timed>,
    name: &'static str,
    id: Option<u64>,
    deadline: Instant,
) -> Result<HolderMessage> {
    reader.get_mut().deadline = Some(deadline);
    loop {
        let message = read_message(reader)?;
        if message == HolderMessage::Unknown {
            continue;
        }
        let unaddressed_error = matches!(message, HolderMessage::Error { id: None, .. });
        if message.reply_to() != id && !unaddressed_error {
            return Err(PanesError::Protocol(format!(
                "{name} (id {id:?}) got a message for another request: {}",
                brief_debug(&message)
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

/// Sends one request's `wire` bytes and reads until its reply, for at most
/// [`REQUEST_TIMEOUT`] in all. The clock starts once the write side is
/// taken, so a wait behind another writer does not count against it.
fn exchange(
    reader: &mut BufReader<Timed>,
    writer: &Mutex<Timed>,
    name: &'static str,
    id: Option<u64>,
    wire: &[u8],
) -> Result<HolderMessage> {
    let mut writer = lock(writer);
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let sent = send(&mut writer, wire, deadline);
    drop(writer);
    sent?;
    await_reply(reader, name, id, deadline)
}

/// One handshaken connection.
struct Conn {
    reader: BufReader<Timed>,
    writer: Mutex<Timed>,
}

impl Conn {
    /// Connects to `path` and does the handshake as `client`.
    fn open(path: &Path, client: &str) -> Result<(Self, Welcome)> {
        // A path too long for the socket address is a setup mistake, not a
        // holder that is down: say so, and do not leave the caller retrying.
        check_socket_path(path).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        let stream = UnixStream::connect(path)
            .map_err(|err| PanesError::Unreachable(format!("{}: {err}", path.display())))?;
        let mut conn = Conn {
            writer: Mutex::new(Timed::new(stream.try_clone()?)),
            reader: BufReader::new(Timed::new(stream)),
        };
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let hello = encode_request("hello", &Request::Hello(Hello::new(client)))?;
        send(&mut lock(&conn.writer), &hello, deadline)?;
        let welcome = match await_reply(&mut conn.reader, "hello", None, deadline)? {
            HolderMessage::Welcome(welcome) => welcome,
            HolderMessage::Refused(refused) => {
                return Err(PanesError::Version {
                    client: SUPPORTED,
                    holder: refused.proto,
                    message: brief(&refused.message),
                });
            }
            other => {
                return Err(PanesError::Protocol(format!(
                    "the holder answered hello with {}",
                    brief_debug(&other)
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
                message: String::new(),
            });
        }
        Ok((conn, welcome))
    }

    /// Sends `request` and reads until its reply, as [`exchange`].
    fn call(&mut self, name: &'static str, request: &Request) -> Result<HolderMessage> {
        let wire = encode_request(name, request)?;
        exchange(&mut self.reader, &self.writer, name, request.id(), &wire)
    }
}

/// How much of a bad message an error quotes.
const QUOTE_BYTES: usize = 512;

/// `text`, cut to [`QUOTE_BYTES`] at a char boundary, so an error stays
/// small when a frame of 16 MiB is wrong.
fn brief(text: &str) -> String {
    if text.len() <= QUOTE_BYTES {
        return text.to_owned();
    }
    let end = text.floor_char_boundary(QUOTE_BYTES);
    format!("{}... ({} bytes)", &text[..end], text.len())
}

/// A value's `Debug` form, cut as [`brief`]. It stops formatting at the
/// cut, so a large listing is never formatted whole.
fn brief_debug(value: &impl std::fmt::Debug) -> String {
    struct Capped(String);
    impl std::fmt::Write for Capped {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let room = QUOTE_BYTES - self.0.len();
            if text.len() <= room {
                self.0.push_str(text);
                return Ok(());
            }
            let end = text.floor_char_boundary(room);
            self.0.push_str(&text[..end]);
            Err(std::fmt::Error)
        }
    }
    let mut capped = Capped(String::new());
    match std::fmt::write(&mut capped, format_args!("{value:?}")) {
        Ok(()) => capped.0,
        Err(_) => capped.0 + "...",
    }
}

/// A message, which must be a JSON object: serde would also take an array,
/// with its first element as the type.
fn parse(text: &str) -> Result<HolderMessage> {
    if !text
        .trim_start_matches([' ', '\t', '\n', '\r'])
        .starts_with('{')
    {
        return Err(PanesError::Protocol(format!(
            "a message that is not a JSON object: {}",
            brief(text)
        )));
    }
    serde_json::from_str(text).map_err(|err| {
        PanesError::Protocol(format!("an unreadable message ({err}): {}", brief(text)))
    })
}

fn unexpected(request: &str, reply: &HolderMessage) -> PanesError {
    PanesError::Protocol(format!(
        "the holder answered {request} with {}",
        brief_debug(reply)
    ))
}

/// Whether a failed call leaves the connection fit for the next one. An
/// `error` reply or a request refused before it was sent does; a broken
/// transport or frame does not.
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
    /// Why the request connection is no longer used, once it is not.
    retired: OnceLock<String>,
    /// Held for a whole request and its reply, so one call is in flight.
    reader: Mutex<BufReader<Timed>>,
    /// Held only while one frame is written.
    writer: Mutex<Timed>,
    /// The request connection's socket, to shut it when it is retired.
    socket: UnixStream,
    next_id: AtomicU64,
}

impl std::fmt::Debug for HolderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderClient")
            .field("path", &self.path)
            .field("client", &self.client)
            .field("welcome", &self.welcome)
            .field("retired", &self.retired.get())
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
        let socket = lock(&conn.writer).stream.try_clone()?;
        Ok(Self {
            path,
            client,
            welcome,
            retired: OnceLock::new(),
            socket,
            reader: Mutex::new(conn.reader),
            writer: conn.writer,
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

    /// `Unreachable` once the request connection is retired.
    fn check(&self) -> Result<()> {
        match self.retired.get() {
            None => Ok(()),
            Some(why) => Err(PanesError::Unreachable(format!(
                "the connection to the holder failed earlier ({why}); connect again"
            ))),
        }
    }

    /// Stops using the request connection, and shuts its socket so a call
    /// waiting on it ends now.
    fn retire(&self, why: String) {
        let _ = self.retired.set(brief(&why));
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    /// Retires the request connection when `result` broke it. When another
    /// call retired it first, and so cut this one, the error says why.
    fn settle<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(err) = &result
            && !keeps_connection(err)
        {
            if let Some(why) = self.retired.get() {
                return Err(PanesError::Unreachable(format!(
                    "the connection to the holder failed during this call ({why}); connect again"
                )));
            }
            self.retire(err.to_string());
        }
        result
    }

    /// Sends `request` and picks the value out of its reply. A reply of a
    /// type this request does not take ([`unexpected`]) breaks the protocol,
    /// so `pick`'s error retires the connection as any broken reply does.
    fn call<T>(
        &self,
        name: &'static str,
        request: Request,
        pick: impl FnOnce(HolderMessage) -> Result<T>,
    ) -> Result<T> {
        self.check()?;
        let wire = encode_request(name, &request)?;
        let mut reader = lock(&self.reader);
        self.check()?;
        let result = exchange(&mut reader, &self.writer, name, request.id(), &wire).and_then(pick);
        self.settle(result)
    }

    /// Writes raw bytes to the pane with this `num` (a kind 2 frame), as
    /// they are. The holder sends no reply, and a call in flight does not
    /// hold it up.
    pub fn raw_input(&self, num: u32, bytes: &[u8]) -> Result<()> {
        self.check()?;
        let mut wire = Vec::new();
        encode_input(num, bytes, &mut wire).map_err(|err| refused("raw input", &err))?;
        let mut writer = lock(&self.writer);
        self.check()?;
        let sent = send(&mut writer, &wire, Instant::now() + REQUEST_TIMEOUT);
        drop(writer);
        self.settle(sent)
    }

    /// Subscribes to a pane's output on a connection of its own. The
    /// [`Subscription`] yields one repaint, then live output. A retired
    /// client subscribes to nothing, as it does nothing else.
    ///
    /// A holder with another `instance` than [`HolderClient::welcome`]'s
    /// started after this client connected, and none of the panes this
    /// client knows are in it (R21). That is `Unreachable`, and the client
    /// is retired, so the caller connects again and sees the new instance.
    pub fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> Result<Subscription> {
        self.check()?;
        let (mut conn, welcome) = Conn::open(&self.path, &self.client)?;
        if welcome.instance != self.welcome.instance {
            let why = format!(
                "the terminal holder restarted: instance {} is now {}",
                self.welcome.instance, welcome.instance
            );
            self.retire(why.clone());
            return Err(PanesError::Unreachable(format!("{why}; connect again")));
        }
        let id = self.id();
        let request = Request::Subscribe {
            id,
            pane: pane.clone(),
            sync: opts.sync,
        };
        let (stream, cols, rows) = match conn.call("subscribe", &request)? {
            HolderMessage::Subscribed {
                stream, cols, rows, ..
            } => (stream, cols, rows),
            reply => return Err(unexpected("subscribe", &reply)),
        };
        // Output may be quiet for hours: reads have no limit from here.
        let Conn { mut reader, writer } = conn;
        reader.get_mut().unlimit_reads()?;
        let socket = reader.get_ref().stream.try_clone()?;
        Ok(Subscription {
            stream,
            cols,
            rows,
            reader,
            handle: StreamHandle(Arc::new(StreamSide {
                pane: pane.clone(),
                lease: format!("stream-{stream}"),
                socket,
                closed: Mutex::new(None),
                writer,
                next_id: AtomicU64::new(id + 1),
                pending: Mutex::new(VecDeque::new()),
            })),
            state: StreamState::AwaitingRepaint,
            exited: false,
        })
    }
}

impl Panes for HolderClient {
    fn spawn(&self, req: &Spawn) -> Result<Pane> {
        let request = Request::Spawn {
            id: self.id(),
            spawn: req.clone(),
        };
        self.call("spawn", request, |reply| match reply {
            HolderMessage::Pane { pane, .. } => Ok(pane),
            reply => Err(unexpected("spawn", &reply)),
        })
    }

    fn list(&self) -> Result<Listing> {
        self.call(
            "list",
            Request::List { id: self.id() },
            |reply| match reply {
                HolderMessage::Panes { listing, .. } => Ok(listing),
                reply => Err(unexpected("list", &reply)),
            },
        )
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
    /// The holder cut this subscription because it fell behind its queue.
    /// Nothing more comes; subscribe again for a fresh repaint.
    Lagged,
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
/// connection, and with it any size it set, even while a [`StreamHandle`] lives.
pub struct Subscription {
    stream: u32,
    cols: u16,
    rows: u16,
    reader: BufReader<Timed>,
    handle: StreamHandle,
    state: StreamState,
    /// Whether the pane's `exited` came, after which a close is the end.
    exited: bool,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("pane", &self.handle.0.pane)
            .field("stream", &self.stream)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // A StreamHandle holds the same socket; shut it so the holder sees the
        // subscription end now, and a later resize fails.
        self.handle.end();
    }
}

/// Resizes or ends a [`Subscription`]. It is `Send` and `Clone`, so one
/// thread can use it while another waits in [`Subscription::recv`]. The
/// holder's answers come through `recv`.
#[derive(Clone)]
pub struct StreamHandle(Arc<StreamSide>);

struct StreamSide {
    pane: PaneId,
    /// The lease id this subscription's resizes name.
    lease: String,
    /// The subscription's socket, to shut it from any thread.
    socket: UnixStream,
    /// Why this side closed the stream, once it did, so the close that
    /// follows is not read as the holder's.
    closed: Mutex<Option<Closed>>,
    writer: Mutex<Timed>,
    next_id: AtomicU64,
    /// The ids of resizes sent and not yet answered.
    pending: Mutex<VecDeque<u64>>,
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("pane", &self.0.pane)
            .field("lease", &self.0.lease)
            .finish_non_exhaustive()
    }
}

impl StreamHandle {
    /// Asks the holder to set the PTY to this size, under the subscription's
    /// lease (the board's Fit). The answer comes through
    /// [`Subscription::recv`] as [`Update::Resized`], or as an error.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let side = &self.0;
        let id = side.next_id.fetch_add(1, Ordering::Relaxed);
        let request = Request::Resize {
            id,
            pane: side.pane.clone(),
            cols,
            rows,
            lease: side.lease.clone(),
        };
        let wire = encode_request("resize", &request)?;
        let mut writer = lock(&side.writer);
        if let Some(closed) = side.closed() {
            return Err(side.closed_error(closed));
        }
        // The id is pending before its bytes leave, so a fast answer finds it.
        side.pending().push_back(id);
        let sent = send(&mut writer, &wire, Instant::now() + REQUEST_TIMEOUT);
        if let Err(err) = &sent {
            side.pending().retain(|pending| *pending != id);
            // Part of the frame may be out, so nothing more can follow it on
            // this socket. Shut it: the stream ends, and later resizes fail
            // at once.
            side.close(Closed::Broken(format!("a resize failed: {err}")));
            // An end on another thread may be what cut the write.
            if let Some(closed @ Closed::Ended) = side.closed() {
                return Err(side.closed_error(closed));
            }
        }
        sent
    }

    /// Ends the subscription from any thread: the connection closes, the
    /// holder drops its lease, and a [`Subscription::recv`] waiting now
    /// returns `None`. Later resizes fail.
    pub fn end(&self) {
        self.0.close(Closed::Ended);
    }
}

/// Why this side closed a subscription's connection.
#[derive(Debug, Clone)]
enum Closed {
    /// [`StreamHandle::end`], a drop, or a stream that ended on its own.
    Ended,
    /// A write failed partway, so the connection is no longer usable.
    Broken(String),
}

impl StreamSide {
    fn pending(&self) -> MutexGuard<'_, VecDeque<u64>> {
        lock(&self.pending)
    }

    /// Takes the pending resize with this `id`. False when none is pending.
    fn take_pending(&self, id: u64) -> bool {
        let mut pending = self.pending();
        match pending.iter().position(|pending| *pending == id) {
            Some(at) => {
                pending.remove(at);
                true
            }
            None => false,
        }
    }

    /// Why this side closed the stream, if it did.
    fn closed(&self) -> Option<Closed> {
        lock(&self.closed).clone()
    }

    /// The error for a resize on a stream this side closed.
    fn closed_error(&self, closed: Closed) -> PanesError {
        PanesError::Unreachable(match closed {
            Closed::Ended => format!("the subscription {} has ended", self.lease),
            Closed::Broken(why) => why,
        })
    }

    /// Records why the stream closed, and shuts the socket. An end wins
    /// over a break, and shuts both ways at once, so a waiting read ends
    /// now. A break shuts the write side now and the read side after
    /// [`BROKEN_GRACE`]: on some systems a read shutdown drops what is
    /// buffered, and an exit or a lag buffered there must still come.
    fn close(&self, why: Closed) {
        let mut closed = lock(&self.closed);
        match why {
            Closed::Ended => {
                *closed = Some(Closed::Ended);
                let _ = self.socket.shutdown(Shutdown::Both);
            }
            Closed::Broken(_) if closed.is_some() => {}
            Closed::Broken(_) => {
                *closed = Some(why);
                let _ = self.socket.shutdown(Shutdown::Write);
                if self.shut_reads_later().is_err() {
                    let _ = self.socket.shutdown(Shutdown::Both);
                }
            }
        }
    }

    /// Shuts the socket's read side after [`BROKEN_GRACE`], on a thread of
    /// its own, so a read that waits now ends too.
    fn shut_reads_later(&self) -> io::Result<()> {
        let socket = self.socket.try_clone()?;
        std::thread::Builder::new()
            .name("tiller-panes-shut".into())
            .spawn(move || {
                std::thread::sleep(BROKEN_GRACE);
                let _ = socket.shutdown(Shutdown::Both);
            })?;
        Ok(())
    }
}

impl Subscription {
    pub fn pane(&self) -> &PaneId {
        &self.handle.0.pane
    }

    /// The `stream` number on this subscription's kind 1 frames.
    pub fn stream(&self) -> u32 {
        self.stream
    }

    /// The PTY's size when the subscription started.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// A handle that resizes or ends this subscription from another thread.
    pub fn handle(&self) -> StreamHandle {
        self.handle.clone()
    }

    /// [`StreamHandle::resize`] on this subscription.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.handle.resize(cols, rows)
    }

    /// The next update. `None` once the stream has ended: after
    /// [`Update::Lagged`], after a close that follows [`Update::Exited`],
    /// after [`StreamHandle::end`], or after an earlier error ended it. A
    /// close at any other time is `Unreachable`: the holder may have died,
    /// so the pane's state is unknown, not gone. An `error` reply to a
    /// `resize` is an error that leaves the stream open; any other error
    /// ends it.
    pub fn recv(&mut self) -> Result<Option<Update>> {
        if self.state == StreamState::Ended {
            return Ok(None);
        }
        // Once this side ended the stream, what is still buffered, and the
        // read error or close the shutdown causes, are not news. After a
        // broken resize, what is buffered still comes (an exit, a lag), and
        // the read that fails then says why.
        let result = match self.handle.0.closed() {
            Some(Closed::Ended) => Ok(None),
            _ => self.read_update(),
        };
        let result = match (result, self.handle.0.closed()) {
            (_, Some(Closed::Ended)) => Ok(None),
            (Err(PanesError::Unreachable(_) | PanesError::Io(_)), Some(Closed::Broken(why))) => {
                Err(PanesError::Unreachable(why))
            }
            (result, _) => result,
        };
        let ends = match &result {
            Ok(None | Some(Update::Lagged)) => true,
            Ok(Some(_)) => false,
            Err(err) => !keeps_connection(err),
        };
        // read_update ends the stream itself on an error that is about it.
        if ends || self.state == StreamState::Ended {
            self.end();
        }
        result
    }

    /// Ends the stream, and its connection, so the holder drops the
    /// subscriber and its lease now, not when this value is dropped.
    fn end(&mut self) {
        self.state = StreamState::Ended;
        self.handle.0.close(Closed::Ended);
    }

    fn read_update(&mut self) -> Result<Option<Update>> {
        loop {
            // After the exit, a holder that closes with a resize it never
            // read resets the connection: that is the end too. A frame cut
            // short is still an error.
            let read = match read_frame(&mut self.reader) {
                Err(FrameError::Io(err))
                    if self.exited && err.kind() == io::ErrorKind::ConnectionReset =>
                {
                    Ok(None)
                }
                read => read.map_err(PanesError::from_frame),
            };
            let Some(frame) = read? else {
                if self.exited {
                    return Ok(None);
                }
                return Err(PanesError::Unreachable(format!(
                    "the holder closed stream {} before the pane exited",
                    self.stream
                )));
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
                    HolderMessage::Ok { id, cols, rows } => {
                        if !self.handle.0.take_pending(id) {
                            return Err(PanesError::Protocol(format!(
                                "an ok (id {id}) that answers no resize of stream {}",
                                self.stream
                            )));
                        }
                        let (Some(cols), Some(rows)) = (cols, rows) else {
                            return Err(PanesError::Protocol(format!(
                                "the ok to resize {id} has no size"
                            )));
                        };
                        return Ok(Some(Update::Resized { cols, rows }));
                    }
                    HolderMessage::Exited { pane, exit } if pane == *self.pane() => {
                        self.exited = true;
                        return Ok(Some(Update::Exited(exit)));
                    }
                    HolderMessage::Lagged { stream } if stream == self.stream => {
                        return Ok(Some(Update::Lagged));
                    }
                    HolderMessage::Error {
                        id: None,
                        code,
                        message,
                    } => {
                        // An error that names no request is about the
                        // subscription itself, not a resize: it ends, and
                        // recv closes the connection.
                        self.state = StreamState::Ended;
                        return Err(PanesError::from_reply("subscription", code, message));
                    }
                    HolderMessage::Error {
                        id: Some(id),
                        code,
                        message,
                    } => {
                        if !self.handle.0.take_pending(id) {
                            return Err(PanesError::Protocol(format!(
                                "an error (id {id}) that answers no resize of stream {}: {code}: {}",
                                self.stream,
                                brief(&message)
                            )));
                        }
                        return Err(PanesError::from_reply("resize", code, message));
                    }
                    HolderMessage::Unknown => {}
                    other => {
                        return Err(PanesError::Protocol(format!(
                            "an unexpected message on stream {}: {}",
                            self.stream,
                            brief_debug(&other)
                        )));
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live subscription to stream 1 on `ours`.
    fn live(ours: UnixStream) -> Subscription {
        let socket = ours.try_clone().unwrap();
        let writer = Mutex::new(Timed::new(ours.try_clone().unwrap()));
        Subscription {
            stream: 1,
            cols: 80,
            rows: 24,
            reader: BufReader::new(Timed::new(ours)),
            handle: StreamHandle(Arc::new(StreamSide {
                pane: "p1".into(),
                lease: "stream-1".into(),
                socket,
                closed: Mutex::new(None),
                writer,
                next_id: AtomicU64::new(1),
                pending: Mutex::new(VecDeque::new()),
            })),
            state: StreamState::Live,
            exited: false,
        }
    }

    /// A holder on `theirs` that sends output to stream 1 until it cannot.
    fn chatty(mut theirs: UnixStream) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut wire = Vec::new();
            Frame::Output {
                stream: 1,
                bytes: b"tick".to_vec(),
            }
            .encode(&mut wire)
            .unwrap();
            while theirs.write_all(&wire).is_ok() {
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    }

    #[test]
    fn a_frame_cut_short_after_the_exit_is_still_an_error() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut subscription = live(ours);
        subscription.exited = true;
        let mut wire = Vec::new();
        Frame::Output {
            stream: 1,
            bytes: b"the last bytes".to_vec(),
        }
        .encode(&mut wire)
        .unwrap();
        theirs.write_all(&wire[..wire.len() - 3]).unwrap();
        drop(theirs);
        let got = subscription.recv();
        assert!(matches!(got, Err(PanesError::Unreachable(_))), "{got:?}");
    }

    #[test]
    fn an_end_after_a_break_is_still_the_end() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let holder = chatty(theirs);
        let mut subscription = live(ours);
        assert!(matches!(subscription.recv(), Ok(Some(Update::Output(_)))));
        let handle = subscription.handle();
        handle
            .0
            .close(Closed::Broken("a resize failed: test".into()));
        handle.end();
        assert_eq!(subscription.recv().unwrap(), None);
        let err = handle.resize(100, 30).unwrap_err();
        assert!(err.to_string().contains("has ended"), "{err}");
        drop(subscription);
        holder.join().unwrap();
    }

    #[test]
    fn a_read_that_waits_on_a_silent_holder_ends_soon_after_a_break() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut subscription = live(ours);
        let handle = subscription.handle();
        let (done, finished) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = done.send(subscription.recv());
        });
        // Let the read start waiting, then break the stream.
        std::thread::sleep(Duration::from_millis(100));
        handle
            .0
            .close(Closed::Broken("a resize failed: test".into()));
        let got = finished.recv_timeout(BROKEN_GRACE * 3).unwrap();
        let Err(PanesError::Unreachable(why)) = got else {
            panic!("expected the break's reason, got {got:?}");
        };
        assert_eq!(why, "a resize failed: test");
        reader.join().unwrap();
        drop(theirs);
    }

    #[test]
    fn a_broken_stream_ends_soon_while_the_holder_keeps_sending() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let holder = chatty(theirs);
        let mut subscription = live(ours);
        assert!(matches!(subscription.recv(), Ok(Some(Update::Output(_)))));
        let started = Instant::now();
        subscription
            .handle
            .0
            .close(Closed::Broken("a resize failed: test".into()));
        let end = loop {
            match subscription.recv() {
                Ok(Some(Update::Output(_))) => {
                    assert!(started.elapsed() < BROKEN_GRACE * 3, "the stream lived on");
                }
                other => break other,
            }
        };
        let Err(PanesError::Unreachable(why)) = end else {
            panic!("expected the break's reason, got {end:?}");
        };
        assert_eq!(why, "a resize failed: test");
        assert_eq!(subscription.recv().unwrap(), None);
        drop(subscription);
        holder.join().unwrap();
    }

    #[test]
    fn a_long_quote_is_cut_at_a_char_boundary() {
        assert_eq!(brief("short"), "short");
        let long = format!("{}é{}", "x".repeat(QUOTE_BYTES - 1), "y".repeat(100));
        let cut = brief(&long);
        assert!(cut.starts_with(&"x".repeat(QUOTE_BYTES - 1)), "{cut}");
        assert!(
            cut.ends_with(&format!("... ({} bytes)", long.len())),
            "{cut}"
        );
        assert!(!cut.contains('é'), "{cut}");
    }

    #[test]
    fn a_long_debug_form_is_cut_while_it_is_formatted() {
        assert_eq!(brief_debug(&"short"), r#""short""#);
        let cut = brief_debug(&vec!["é".repeat(400); 1000]);
        assert!(cut.len() <= QUOTE_BYTES + 3, "{} bytes", cut.len());
        assert!(cut.ends_with("..."), "{cut}");
    }

    #[test]
    fn a_message_must_be_an_object() {
        assert!(matches!(
            parse(r#"["lagged",3]"#),
            Err(PanesError::Protocol(_))
        ));
        assert_eq!(
            parse(r#"{"type":"lagged","stream":3}"#).unwrap(),
            HolderMessage::Lagged { stream: 3 }
        );
    }
}
