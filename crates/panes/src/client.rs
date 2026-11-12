//! The holder socket client (`terminal-holder.md` 2.4, 4.1).
//!
//! [`HolderClient`] holds one connection for requests. Requests on it may
//! overlap: each caller registers its request id, writes its frame, and
//! waits for its own reply, which one reader thread hands over by id. Raw
//! input takes only the write side, so a keystroke never waits here for
//! another call's reply. (How soon the holder serves it is the holder's
//! part: spec 2.4 has it serve a connection's requests concurrently.) Each [`Subscription`] and each [`Events`] has a connection
//! of its own: its frames never wait behind a request, and a size a
//! subscription sets through its lease ends when it is dropped, because its
//! connection ends. A [`StreamHandle`] resizes or ends a subscription from
//! another thread while one thread waits in [`Subscription::recv`].
//!
//! A connection that fails (a timeout, a closed socket, a bad frame, a
//! reply for no request in flight) is not used again: every later call on
//! that client is `Unreachable`, and the caller connects again. Nothing here
//! retries, so a holder restart is never hidden behind a quiet reconnect.

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufReader, Write};
use std::net::Shutdown;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, FdFlags, fcntl_setfd, ioctl_fionbio};
use rustix::net::{AddressFamily, SocketAddrUnix, SocketType, connect, socket, sockopt};

use crate::error::{PanesError, Result};
use crate::frame::{Frame, FrameError, encode_input, read_frame};
use crate::handshake::{Hello, ProtoRange, SUPPORTED, Welcome};
use crate::keys::Key;
use crate::live::{Control, EventFeed, Events, Feed, Subscribe, Subscription, Update};
use crate::message::{
    ErrorCode, Event, Exit, Format, HolderMessage, Listing, Meta, Pane, PaneId, Read, Request,
    Screen, Signal, Spawn,
};
use crate::panes::{Live, Panes};
use crate::paths::check_socket_path;

/// How long the connect and the handshake may take in all.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a request may take in all, from its first byte out to its
/// reply's last byte in, and how long one frame's write may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a connect waits before it tries again, when the holder's
/// listen queue is full.
const CONNECT_RETRY: Duration = Duration::from_millis(10);

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

    /// Reads with no limit from here on, as a stream's and the reply
    /// reader's do.
    fn unlimit_reads(&mut self) -> io::Result<()> {
        self.deadline = None;
        self.stream.set_read_timeout(None)
    }
}

impl io::Read for Timed {
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

/// Reads until the reply to request `id`, before `deadline`, on a
/// connection with one request in flight. Messages for nobody (a type from
/// a newer holder) are skipped; an `error` reply is the error. An `error`
/// with no `id` answers this request, the only one in flight.
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

/// Connects to the unix socket at `path`, or fails at `deadline`. A plain
/// connect has no time limit, and a holder with a full listen queue would
/// hold it up for as long as it stays full.
fn connect_by(path: &Path, deadline: Instant) -> Result<UnixStream> {
    let unreachable = |err: Errno| {
        PanesError::Unreachable(format!("{}: {}", path.display(), io::Error::from(err)))
    };
    let addr = SocketAddrUnix::new(path).map_err(unreachable)?;
    let fd = socket(AddressFamily::UNIX, SocketType::STREAM, None).map_err(io::Error::from)?;
    fcntl_setfd(&fd, FdFlags::CLOEXEC).map_err(io::Error::from)?;
    ioctl_fionbio(&fd, true).map_err(io::Error::from)?;
    loop {
        match connect(&fd, &addr) {
            // A connect that already went through, after a signal cut the
            // call short and it ran again.
            Ok(()) | Err(Errno::ISCONN) => break,
            Err(Errno::INTR) => {}
            // Linux: the listen queue is full. Try again until the deadline.
            Err(Errno::AGAIN) => {
                let left = time_left(path, deadline)?;
                std::thread::sleep(CONNECT_RETRY.min(left));
            }
            // `ALREADY`: the connect a signal cut short is still going.
            Err(Errno::INPROGRESS | Errno::ALREADY) => {
                wait_writable(path, &fd, deadline)?;
                match sockopt::socket_error(&fd).map_err(io::Error::from)? {
                    Ok(()) => break,
                    Err(err) => return Err(unreachable(err)),
                }
            }
            Err(err) => return Err(unreachable(err)),
        }
    }
    ioctl_fionbio(&fd, false).map_err(io::Error::from)?;
    Ok(UnixStream::from(fd))
}

fn time_left(path: &Path, deadline: Instant) -> Result<Duration> {
    match deadline.saturating_duration_since(Instant::now()) {
        left if left.is_zero() => Err(PanesError::Unreachable(format!(
            "{}: the holder did not take the connection in time",
            path.display()
        ))),
        left => Ok(left),
    }
}

fn wait_writable(path: &Path, fd: &OwnedFd, deadline: Instant) -> Result<()> {
    loop {
        let left = time_left(path, deadline)?;
        let timeout = Timespec::try_from(left).map_err(io::Error::other)?;
        let mut fds = [PollFd::new(fd, PollFlags::OUT)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(Errno::INTR) => {}
            Ok(_) => return Ok(()),
            Err(err) => return Err(io::Error::from(err).into()),
        }
    }
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
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let stream = connect_by(path, deadline)?;
        let mut conn = Conn {
            writer: Mutex::new(Timed::new(stream.try_clone()?)),
            reader: BufReader::new(Timed::new(stream)),
        };
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

    /// Sends `request` and reads until its reply, for at most
    /// [`REQUEST_TIMEOUT`].
    fn call(&mut self, name: &'static str, request: &Request) -> Result<HolderMessage> {
        let wire = encode_request(name, request)?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        send(&mut lock(&self.writer), &wire, deadline)?;
        await_reply(&mut self.reader, name, request.id(), deadline)
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

/// The same error for another caller. Only the kinds a broken connection
/// has are kept: a holder that broke the protocol broke it for every call
/// in flight. Anything else is `Unreachable` with its text.
fn copy(err: &PanesError) -> PanesError {
    match err {
        PanesError::Protocol(why) => PanesError::Protocol(why.clone()),
        PanesError::Unreachable(why) => PanesError::Unreachable(why.clone()),
        err => PanesError::Unreachable(err.to_string()),
    }
}

/// The instant `wait` from now. A wait too long to add, such as a huge
/// close grace, waits a year, which no call outlives.
fn after(wait: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(wait)
        .or_else(|| now.checked_add(Duration::from_secs(365 * 86_400)))
        .unwrap_or(now)
}

/// A caller waiting for its reply.
struct Waiter {
    name: &'static str,
    reply: SyncSender<Result<HolderMessage>>,
}

/// What the callers and the reply reader share.
struct Shared {
    /// Why the request connection is no longer used, once it is not.
    retired: OnceLock<String>,
    /// The request connection's socket, to shut it when it is retired.
    socket: UnixStream,
    /// The requests in flight, by id.
    waiters: Mutex<HashMap<u64, Waiter>>,
}

impl Shared {
    fn waiters(&self) -> MutexGuard<'_, HashMap<u64, Waiter>> {
        lock(&self.waiters)
    }

    /// Stops using the request connection, shuts its socket, and ends every
    /// wait: each caller still waiting gets `err`.
    ///
    /// The reason is set before the waiters are taken, and a caller checks
    /// it after it adds its waiter, so no caller is left waiting.
    fn retire(&self, err: &PanesError) {
        let _ = self.retired.set(brief(&err.to_string()));
        let _ = self.socket.shutdown(Shutdown::Both);
        let waiters: Vec<Waiter> = self.waiters().drain().map(|(_, waiter)| waiter).collect();
        for waiter in waiters {
            let _ = waiter.reply.send(Err(copy(err)));
        }
    }

    /// The id of the one request in flight, when there is exactly one.
    fn only(&self) -> Option<u64> {
        let waiters = self.waiters();
        match waiters.len() {
            1 => waiters.keys().next().copied(),
            _ => None,
        }
    }
}

/// Hands each reply on the request connection to its caller, until the
/// connection fails or is retired. Messages for nobody (a type from a newer
/// holder) are skipped. An `error` with no `id` goes to the request in
/// flight when there is just one. Any other message that answers no
/// request in flight breaks the protocol.
fn read_replies(shared: &Shared, mut reader: BufReader<Timed>) {
    loop {
        let message = match read_message(&mut reader) {
            Ok(HolderMessage::Unknown) => continue,
            Ok(message) => message,
            Err(err) => return shared.retire(&err),
        };
        let id = match &message {
            HolderMessage::Error { id: None, .. } => shared.only(),
            message => message.reply_to(),
        };
        let Some(waiter) = id.and_then(|id| shared.waiters().remove(&id)) else {
            return shared.retire(&PanesError::Protocol(format!(
                "a message that answers no request in flight: {}",
                brief_debug(&message)
            )));
        };
        let reply = match message {
            HolderMessage::Error { code, message, .. } => {
                Err(PanesError::from_reply(waiter.name, code, message))
            }
            reply => Ok(reply),
        };
        let _ = waiter.reply.send(reply);
    }
}

/// How the holder answered `upgrade`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Upgrade {
    /// The holder re-executes now; this connection ends.
    Upgrading,
    /// The holder will not re-execute, and keeps running as it is.
    Refused(String),
}

/// A client of the terminal holder.
pub struct HolderClient {
    path: PathBuf,
    client: String,
    welcome: Welcome,
    shared: Arc<Shared>,
    /// Held only while one frame is written.
    writer: Mutex<Timed>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for HolderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HolderClient")
            .field("path", &self.path)
            .field("client", &self.client)
            .field("welcome", &self.welcome)
            .field("retired", &self.shared.retired.get())
            .finish_non_exhaustive()
    }
}

impl Drop for HolderClient {
    fn drop(&mut self) {
        // Ends the reply reader's wait, and with it the thread.
        let _ = self.shared.socket.shutdown(Shutdown::Both);
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
        let Conn { mut reader, writer } = conn;
        // A reply may take as long as its request allows, and the reader
        // waits between requests too: each caller keeps its own deadline.
        reader.get_mut().unlimit_reads()?;
        let shared = Arc::new(Shared {
            retired: OnceLock::new(),
            socket: reader.get_ref().stream.try_clone()?,
            waiters: Mutex::new(HashMap::new()),
        });
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("tiller-panes-replies".into())
            .spawn(move || read_replies(&theirs, reader))?;
        Ok(Self {
            path,
            client,
            welcome,
            shared,
            writer,
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
        match self.shared.retired.get() {
            None => Ok(()),
            Some(why) => Err(PanesError::Unreachable(format!(
                "the connection to the holder failed earlier ({why}); connect again"
            ))),
        }
    }

    /// Retires the request connection when `result` broke it.
    fn settle<T>(&self, result: Result<T>) -> Result<T> {
        if let Err(err) = &result
            && !keeps_connection(err)
        {
            self.shared.retire(err);
        }
        result
    }

    /// Sends `request`, waits up to `wait` for its reply, and picks the
    /// value out of it. A reply of a type this request does not take
    /// ([`unexpected`]) breaks the protocol, so `pick`'s error retires the
    /// connection as any broken reply does.
    fn call<T>(
        &self,
        name: &'static str,
        request: Request,
        wait: Duration,
        pick: impl FnOnce(HolderMessage) -> Result<T>,
    ) -> Result<T> {
        self.check()?;
        let wire = encode_request(name, &request)?;
        let id = request.id().ok_or_else(|| refused(name, "it has no id"))?;
        let result = self.exchange(name, id, &wire, wait).and_then(pick);
        self.settle(result)
    }

    /// Sends one request's `wire` bytes and waits for its reply, for at most
    /// `wait` in all. The clock starts once the write side is taken, so a
    /// wait behind another writer does not count against it.
    fn exchange(
        &self,
        name: &'static str,
        id: u64,
        wire: &[u8],
        wait: Duration,
    ) -> Result<HolderMessage> {
        let (reply, answer) = mpsc::sync_channel(1);
        let mut writer = lock(&self.writer);
        // The waiter is in place before the bytes leave, so a fast reply
        // finds it, and only once this request holds the write side, so the
        // waiters are the requests sent or being sent: an error with no id
        // never goes to a request still waiting to be written.
        if self
            .shared
            .waiters()
            .insert(id, Waiter { name, reply })
            .is_some()
        {
            return Err(PanesError::Protocol(format!("request id {id} used twice")));
        }
        let forget = || {
            self.shared.waiters().remove(&id);
        };
        // After the insert: a retire that drained the waiters before it
        // shows here.
        if let Err(err) = self.check() {
            forget();
            return Err(err);
        }
        let deadline = after(wait);
        let sent = send(&mut writer, wire, deadline);
        drop(writer);
        if let Err(err) = sent {
            forget();
            return Err(err);
        }
        match answer.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => {
                forget();
                Err(PanesError::Unreachable(format!(
                    "the holder did not answer {name} in time"
                )))
            }
            Err(RecvTimeoutError::Disconnected) => Err(PanesError::Unreachable(format!(
                "the reply reader stopped during {name}"
            ))),
        }
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

    /// Asks the holder to re-execute from `binary` (`terminal-holder.md`
    /// 6.2). With `dry_run`, it checks and answers, and does not.
    pub fn upgrade(&self, binary: &str, dry_run: bool) -> Result<Upgrade> {
        let request = Request::Upgrade {
            id: self.id(),
            binary: binary.to_owned(),
            dry_run,
        };
        self.call("upgrade", request, REQUEST_TIMEOUT, |reply| match reply {
            HolderMessage::Upgrading { .. } => Ok(Upgrade::Upgrading),
            HolderMessage::UpgradeRefused { reason, .. } => Ok(Upgrade::Refused(reason)),
            reply => Err(unexpected("upgrade", &reply)),
        })
    }

    /// A health check: the holder answers.
    pub fn ping(&self) -> Result<()> {
        let request = Request::Ping { id: self.id() };
        self.call("ping", request, REQUEST_TIMEOUT, |reply| match reply {
            HolderMessage::Pong { .. } => Ok(()),
            reply => Err(unexpected("ping", &reply)),
        })
    }

    /// A new connection for a stream, from the same holder instance.
    ///
    /// A holder with another `instance` than [`HolderClient::welcome`]'s
    /// started after this client connected, and none of the panes this
    /// client knows are in it (R21). That is `Unreachable`, and the client
    /// is retired, so the caller connects again and sees the new instance.
    fn open_stream(&self) -> Result<Conn> {
        self.check()?;
        let (conn, welcome) = Conn::open(&self.path, &self.client)?;
        if welcome.instance != self.welcome.instance {
            let why = format!(
                "the terminal holder restarted: instance {} is now {}",
                self.welcome.instance, welcome.instance
            );
            self.shared.retire(&PanesError::Unreachable(why.clone()));
            return Err(PanesError::Unreachable(format!("{why}; connect again")));
        }
        Ok(conn)
    }

    /// A request that the holder answers with a bare `ok`.
    fn ok(&self, name: &'static str, request: Request) -> Result<()> {
        self.call(name, request, REQUEST_TIMEOUT, |reply| match reply {
            HolderMessage::Ok { .. } => Ok(()),
            reply => Err(unexpected(name, &reply)),
        })
    }

    /// A request that the holder answers with a `pane`.
    fn pane(&self, name: &'static str, request: Request) -> Result<Pane> {
        self.call(name, request, REQUEST_TIMEOUT, |reply| match reply {
            HolderMessage::Pane { pane, .. } => Ok(pane),
            reply => Err(unexpected(name, &reply)),
        })
    }
}

impl Panes for HolderClient {
    fn spawn(&self, req: &Spawn) -> Result<Pane> {
        let request = Request::Spawn {
            id: self.id(),
            spawn: req.clone(),
        };
        self.pane("spawn", request)
    }

    fn list(&self) -> Result<Listing> {
        let request = Request::List { id: self.id() };
        self.call("list", request, REQUEST_TIMEOUT, |reply| match reply {
            HolderMessage::Panes { listing, .. } => Ok(listing),
            reply => Err(unexpected("list", &reply)),
        })
    }

    fn get(&self, pane: &PaneId) -> Result<Option<Pane>> {
        let request = Request::Get {
            id: self.id(),
            pane: pane.clone(),
        };
        match self.pane("get", request) {
            Ok(pane) => Ok(Some(pane)),
            Err(PanesError::NotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen> {
        let request = Request::Screen {
            id: self.id(),
            pane: pane.clone(),
            read,
            format,
        };
        self.call("screen", request, REQUEST_TIMEOUT, |reply| match reply {
            HolderMessage::Screen { screen, .. } => Ok(screen),
            reply => Err(unexpected("screen", &reply)),
        })
    }

    fn input(&self, pane: &PaneId, text: &str) -> Result<()> {
        let request = Request::Input {
            id: self.id(),
            pane: pane.clone(),
            text: text.to_owned(),
        };
        self.ok("input", request)
    }

    fn paste(&self, pane: &PaneId, text: &str) -> Result<()> {
        let request = Request::Paste {
            id: self.id(),
            pane: pane.clone(),
            text: text.to_owned(),
        };
        self.ok("paste", request)
    }

    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()> {
        let request = Request::Keys {
            id: self.id(),
            pane: pane.clone(),
            keys: keys.to_vec(),
        };
        self.ok("keys", request)
    }

    fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> Result<Pane> {
        let request = Request::Meta {
            id: self.id(),
            pane: pane.clone(),
            set: set.clone(),
            remove: remove.iter().map(|key| (*key).to_owned()).collect(),
        };
        self.pane("meta", request)
    }

    fn signal(&self, pane: &PaneId, signal: Signal) -> Result<()> {
        let request = Request::Signal {
            id: self.id(),
            pane: pane.clone(),
            signal,
        };
        self.ok("signal", request)
    }

    /// The wait for the reply is [`REQUEST_TIMEOUT`] past `grace`, since the
    /// holder answers once the child is reaped.
    fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit> {
        let grace_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX);
        let request = Request::Close {
            id: self.id(),
            pane: pane.clone(),
            grace_ms,
        };
        let wait = REQUEST_TIMEOUT.saturating_add(grace);
        let ours = pane.clone();
        self.call("close", request, wait, move |reply| match reply {
            HolderMessage::Exited { pane, exit, .. } if pane == ours => Ok(exit),
            reply => Err(unexpected("close", &reply)),
        })
    }

    fn forget(&self, pane: &PaneId) -> Result<()> {
        let request = Request::Forget {
            id: self.id(),
            pane: pane.clone(),
        };
        self.ok("forget", request)
    }

    fn switch(&self, pane: &PaneId) -> Result<usize> {
        let request = Request::Switch {
            id: self.id(),
            pane: pane.clone(),
        };
        self.call("switch", request, REQUEST_TIMEOUT, |reply| match reply {
            HolderMessage::Ok {
                clients: Some(clients),
                ..
            } => Ok(clients),
            reply => Err(unexpected("switch", &reply)),
        })
    }
}

impl Live for HolderClient {
    /// Subscribes on a connection of its own. A retired client subscribes
    /// to nothing, as it does nothing else.
    fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> Result<Subscription> {
        let mut conn = self.open_stream()?;
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
        let side = Arc::new(StreamSide {
            pane: pane.clone(),
            lease: format!("stream-{stream}"),
            socket: reader.get_ref().stream.try_clone()?,
            closed: Mutex::new(None),
            writer,
            next_id: AtomicU64::new(id + 1),
            pending: Mutex::new(VecDeque::new()),
        });
        let feed = SocketFeed {
            stream,
            reader,
            side: Arc::clone(&side),
            state: StreamState::AwaitingRepaint,
            exited: false,
        };
        Ok(Subscription::from_parts(
            pane.clone(),
            stream,
            (cols, rows),
            side.lease.clone(),
            Box::new(feed),
            side,
        ))
    }

    /// Watches on a connection of its own.
    fn watch(&self) -> Result<Events> {
        let mut conn = self.open_stream()?;
        let request = Request::Watch { id: self.id() };
        match conn.call("watch", &request)? {
            HolderMessage::Watching { .. } => {}
            reply => return Err(unexpected("watch", &reply)),
        }
        // Events may be quiet for hours: reads have no limit from here.
        let Conn { mut reader, .. } = conn;
        reader.get_mut().unlimit_reads()?;
        let socket = reader.get_ref().stream.try_clone()?;
        let ended = Arc::new(AtomicBool::new(false));
        let feed = SocketEvents {
            reader,
            ended: Arc::clone(&ended),
            done: false,
        };
        let end = move || {
            ended.store(true, Ordering::SeqCst);
            let _ = socket.shutdown(Shutdown::Both);
        };
        Ok(Events::from_parts(Box::new(feed), Arc::new(end)))
    }
}

/// A watch connection's events.
struct SocketEvents {
    reader: BufReader<Timed>,
    /// Whether this side ended the watch, so the close that follows is not
    /// read as the holder's.
    ended: Arc<AtomicBool>,
    /// Whether the watch is over, by an end or an error.
    done: bool,
}

impl EventFeed for SocketEvents {
    fn recv(&mut self) -> Result<Option<Event>> {
        if self.done || self.ended.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let result = self.read_event();
        if self.ended.load(Ordering::SeqCst) {
            self.done = true;
            return Ok(None);
        }
        if result.is_err() {
            self.done = true;
        }
        result.map(Some)
    }
}

impl SocketEvents {
    fn read_event(&mut self) -> Result<Event> {
        loop {
            let message = match read_message(&mut self.reader)?.into_event() {
                Ok(event) => return Ok(event),
                Err(message) => *message,
            };
            match message {
                HolderMessage::Unknown => {}
                HolderMessage::Error {
                    id: None,
                    code,
                    message,
                } => return Err(PanesError::from_reply("watch", code, message)),
                other => {
                    return Err(PanesError::Protocol(format!(
                        "an unexpected message on the watch: {}",
                        brief_debug(&other)
                    )));
                }
            }
        }
    }
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

/// What a subscription asked on its connection and has no answer to yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Resize,
    Release,
}

impl Asked {
    fn name(self) -> &'static str {
        match self {
            Asked::Resize => "resize",
            Asked::Release => "release",
        }
    }
}

/// A subscription connection's write side, shared by its handles.
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
    /// The requests sent and not yet answered.
    pending: Mutex<VecDeque<(u64, Asked)>>,
}

/// Why this side closed a subscription's connection.
#[derive(Debug, Clone)]
enum Closed {
    /// [`StreamHandle::end`], a drop, or a stream that ended on its own.
    Ended,
    /// A write failed partway, so the connection is no longer usable.
    Broken(String),
}

impl Control for StreamSide {
    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.ask(Asked::Resize, |id| Request::Resize {
            id,
            pane: self.pane.clone(),
            cols,
            rows,
            lease: self.lease.clone(),
        })
    }

    fn release(&self) -> Result<()> {
        self.ask(Asked::Release, |id| Request::Release {
            id,
            pane: self.pane.clone(),
            lease: self.lease.clone(),
        })
    }

    fn end(&self) {
        self.close(Closed::Ended);
    }
}

impl StreamSide {
    fn pending(&self) -> MutexGuard<'_, VecDeque<(u64, Asked)>> {
        lock(&self.pending)
    }

    /// Sends the request `request` builds, with a new id, and records it as
    /// pending.
    fn ask(&self, asked: Asked, request: impl FnOnce(u64) -> Request) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let wire = encode_request(asked.name(), &request(id))?;
        let mut writer = lock(&self.writer);
        if let Some(closed) = self.closed() {
            return Err(self.closed_error(closed));
        }
        // The id is pending before its bytes leave, so a fast answer finds it.
        self.pending().push_back((id, asked));
        let sent = send(&mut writer, &wire, Instant::now() + REQUEST_TIMEOUT);
        if let Err(err) = &sent {
            self.pending().retain(|(pending, _)| *pending != id);
            // Part of the frame may be out, so nothing more can follow it on
            // this socket. Shut it: the stream ends, and later requests fail
            // at once.
            self.close(Closed::Broken(format!("a {} failed: {err}", asked.name())));
            // An end on another thread may be what cut the write.
            if let Some(closed @ Closed::Ended) = self.closed() {
                return Err(self.closed_error(closed));
            }
        }
        sent
    }

    /// Takes the pending request with this `id`, if one is pending.
    fn take_pending(&self, id: u64) -> Option<Asked> {
        let mut pending = self.pending();
        let at = pending.iter().position(|(pending, _)| *pending == id)?;
        pending.remove(at).map(|(_, asked)| asked)
    }

    /// Why this side closed the stream, if it did.
    fn closed(&self) -> Option<Closed> {
        lock(&self.closed).clone()
    }

    /// The error for a request on a stream this side closed.
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

/// A subscription connection's read side.
struct SocketFeed {
    stream: u32,
    reader: BufReader<Timed>,
    side: Arc<StreamSide>,
    state: StreamState,
    /// Whether the pane's `exited` came, after which a close is the end.
    exited: bool,
}

impl Feed for SocketFeed {
    fn recv(&mut self) -> Result<Option<Update>> {
        if self.state == StreamState::Ended {
            return Ok(None);
        }
        // Once this side ended the stream, what is still buffered, and the
        // read error or close the shutdown causes, are not news. After a
        // broken resize, what is buffered still comes (an exit, a lag), and
        // the read that fails then says why.
        let result = match self.side.closed() {
            Some(Closed::Ended) => Ok(None),
            _ => self.read_update(),
        };
        let result = match (result, self.side.closed()) {
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
}

impl SocketFeed {
    /// Ends the stream, and its connection, so the holder drops the
    /// subscriber and its lease now, not when the subscription is dropped.
    fn end(&mut self) {
        self.state = StreamState::Ended;
        self.side.close(Closed::Ended);
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
                    HolderMessage::Ok { id, cols, rows, .. } => {
                        let Some(asked) = self.side.take_pending(id) else {
                            return Err(PanesError::Protocol(format!(
                                "an ok (id {id}) that answers no request of stream {}",
                                self.stream
                            )));
                        };
                        let (Some(cols), Some(rows)) = (cols, rows) else {
                            return Err(PanesError::Protocol(format!(
                                "the ok to {} {id} has no size",
                                asked.name()
                            )));
                        };
                        return Ok(Some(Update::Resized { cols, rows }));
                    }
                    // Another lease set the size (`terminal-holder.md` 2.6).
                    HolderMessage::Resized { pane, cols, rows } if pane == self.side.pane => {
                        return Ok(Some(Update::Resized { cols, rows }));
                    }
                    HolderMessage::Exited {
                        id: None,
                        pane,
                        exit,
                    } if pane == self.side.pane => {
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
                        let Some(asked) = self.side.take_pending(id) else {
                            return Err(PanesError::Protocol(format!(
                                "an error (id {id}) that answers no request of stream {}: {code}: {}",
                                self.stream,
                                brief(&message)
                            )));
                        };
                        return Err(PanesError::from_reply(asked.name(), code, message));
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

    /// A live subscription to stream 1 on `ours`, and its write side.
    fn live(ours: UnixStream) -> (Subscription, Arc<StreamSide>) {
        live_after(ours, false)
    }

    /// As [`live`], after the pane's `exited` came when `exited`.
    fn live_after(ours: UnixStream, exited: bool) -> (Subscription, Arc<StreamSide>) {
        let side = Arc::new(StreamSide {
            pane: "p1".into(),
            lease: "stream-1".into(),
            socket: ours.try_clone().unwrap(),
            closed: Mutex::new(None),
            writer: Mutex::new(Timed::new(ours.try_clone().unwrap())),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(VecDeque::new()),
        });
        let feed = SocketFeed {
            stream: 1,
            reader: BufReader::new(Timed::new(ours)),
            side: Arc::clone(&side),
            state: StreamState::Live,
            exited,
        };
        let subscription = Subscription::from_parts(
            "p1".into(),
            1,
            (80, 24),
            "stream-1".into(),
            Box::new(feed),
            side.clone(),
        );
        (subscription, side)
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
        let (mut subscription, _side) = live_after(ours, true);
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
        let (mut subscription, side) = live(ours);
        assert!(matches!(subscription.recv(), Ok(Some(Update::Output(_)))));
        let handle = subscription.handle();
        side.close(Closed::Broken("a resize failed: test".into()));
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
        let (mut subscription, side) = live(ours);
        let (done, finished) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _ = done.send(subscription.recv());
        });
        // Let the read start waiting, then break the stream.
        std::thread::sleep(Duration::from_millis(100));
        side.close(Closed::Broken("a resize failed: test".into()));
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
        let (mut subscription, side) = live(ours);
        assert!(matches!(subscription.recv(), Ok(Some(Update::Output(_)))));
        let started = Instant::now();
        side.close(Closed::Broken("a resize failed: test".into()));
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

    /// A listener whose queue is full on Linux: backlog 0 holds one
    /// connection that nobody accepts, and the next connect would wait.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_connect_to_a_full_listen_queue_gives_up_at_its_deadline() {
        use rustix::net::{bind, listen};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.sock");
        let fd = socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
        bind(&fd, &SocketAddrUnix::new(&path).unwrap()).unwrap();
        listen(&fd, 0).unwrap();
        // Fill the queue: these connect and are never accepted.
        let mut held = Vec::new();
        let full = loop {
            match connect_by(&path, Instant::now() + Duration::from_millis(200)) {
                Ok(stream) => held.push(stream),
                Err(err) => break err,
            }
            assert!(held.len() < 16, "the queue never filled");
        };
        let PanesError::Unreachable(why) = &full else {
            panic!("expected Unreachable, got {full:?}");
        };
        assert!(why.contains("in time"), "{why}");
        let started = Instant::now();
        let err = connect_by(&path, Instant::now() + Duration::from_millis(100)).unwrap_err();
        assert!(matches!(err, PanesError::Unreachable(_)), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        drop(fd);
    }

    #[test]
    fn a_connect_to_no_socket_is_unreachable_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let err = connect_by(
            &dir.path().join("none.sock"),
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(err, PanesError::Unreachable(_)), "{err:?}");
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
