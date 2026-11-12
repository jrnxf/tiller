//! The holder's socket (`terminal-holder.md` 2.2 to 2.4): the run
//! directory, the lock, the listener, the peer check and each connection's
//! handshake and frames.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tiller_panes::frame::{Decoder, Frame, FrameError};
use tiller_panes::handshake::{self, Hello, Refused, SUPPORTED};
use tiller_panes::message::{ErrorCode, HolderMessage, Request};
use tiller_panes::paths::{SocketPathTooLong, check_socket_path};
use tiller_panes::{run_dir, socket_path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};

use crate::holder::{Conn, Holder, Slot, message_frame, ok};
use crate::lock::{Acquired, HolderLock, lock_path};
use crate::outbox::{self, OUTBOX_CAP};
use crate::peer::{self, PeerCred};

/// How long a client has to send `hello`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a closing connection has to take what is queued for it, and
/// a cut one to stop sending.
const CLOSE_FLUSH: Duration = Duration::from_secs(1);

/// The most a socket read takes at once.
const READ_CHUNK: usize = 64 * 1024;

/// How a holder run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It ran and was told to stop.
    Stopped,
    /// Another holder holds this home's lock. Its pid, when it wrote it.
    AlreadyRuns { pid: Option<u32> },
}

#[derive(Debug, thiserror::Error)]
pub enum HolderError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathTooLong),

    #[error("{what} {path}: {source}")]
    Io {
        what: &'static str,
        path: PathBuf,
        source: io::Error,
    },

    /// Something answers on the socket, yet holds no lock: not a holder of
    /// this build, so the holder leaves it alone.
    #[error("something already listens on {0} without the holder lock")]
    Occupied(PathBuf),

    #[error("the holder's runtime: {0}")]
    Runtime(io::Error),
}

fn io_error(what: &'static str, path: &Path) -> impl FnOnce(io::Error) -> HolderError + use<> {
    let path = path.to_owned();
    move |source| HolderError::Io { what, path, source }
}

/// Runs the holder for `home` until `SIGTERM` or `SIGINT`.
pub fn run(home: &Path) -> Result<Outcome, HolderError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("tiller-holder")
        .build()
        .map_err(HolderError::Runtime)?;
    runtime.block_on(serve(home))
}

async fn serve(home: &Path) -> Result<Outcome, HolderError> {
    let socket = socket_path(home);
    check_socket_path(&socket)?;
    let run = run_dir(home);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .map_err(io_error("create", &run))?;
    // The mode above is not applied to a directory that was there already.
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))
        .map_err(io_error("chmod", &run))?;

    let lock_file = lock_path(&run);
    let _lock = match HolderLock::acquire(&lock_file).map_err(io_error("lock", &lock_file))? {
        Acquired::Held(lock) => lock,
        Acquired::Taken { pid } => return Ok(Outcome::AlreadyRuns { pid }),
    };
    let listener = bind(&socket)?;

    let holder = Holder::new();
    // The reaper listens before the first spawn, so no exit is missed.
    let sigchld = signal(SignalKind::child()).map_err(HolderError::Runtime)?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(HolderError::Runtime)?;
    let mut sigint = signal(SignalKind::interrupt()).map_err(HolderError::Runtime)?;
    tokio::spawn(holder.clone().reaper(sigchld));

    let own = peer::own_uid();
    tracing::info!(
        socket = %socket.display(),
        pid = std::process::id(),
        instance = %holder.welcome(handshake::PROTO).instance,
        "the holder is listening"
    );
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => admit(&holder, stream, own),
                Err(err) => {
                    tracing::warn!(%err, "accept failed");
                    // An fd limit: wait instead of spinning.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        }
    }
    tracing::info!("the holder is stopping and hangs up its panes");
    holder.hang_up_all();
    if let Err(err) = std::fs::remove_file(&socket) {
        tracing::warn!(%err, "the socket could not be removed");
    }
    Ok(Outcome::Stopped)
}

/// Binds the socket with mode 0600. A socket file left by a holder that
/// died is removed first. The lock is held, so no other holder uses it.
fn bind(socket: &Path) -> Result<UnixListener, HolderError> {
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => return Err(HolderError::Occupied(socket.to_owned())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) => std::fs::remove_file(socket).map_err(io_error("remove", socket))?,
    }
    // No moment with a wider mode: the socket is made 0600 by the umask.
    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
    let bound = UnixListener::bind(socket);
    rustix::process::umask(old);
    let listener = bound.map_err(io_error("bind", socket))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .map_err(io_error("chmod", socket))?;
    Ok(listener)
}

/// The peer check: a peer of another uid gets no reply, and the socket
/// closes. So does a connection past the holder's limit.
fn admit(holder: &Arc<Holder>, stream: UnixStream, own: u32) {
    let peer = match PeerCred::of(&stream) {
        Ok(peer) => peer,
        Err(err) => {
            tracing::warn!(%err, "a connection's peer cannot be read; it is closed");
            return;
        }
    };
    if let Err(refusal) = peer::admit(peer, own) {
        tracing::warn!(%refusal, "refused");
        return;
    }
    let Some(slot) = holder.take_conn_slot() else {
        tracing::warn!(
            %peer,
            "the holder has {} connections, its limit; one more is closed",
            holder.limits().connections
        );
        return;
    };
    tracing::debug!(%peer, "a connection");
    tokio::spawn(connection(holder.clone(), stream, slot));
}

/// One connection, from `hello` to its end.
async fn connection(holder: Arc<Holder>, stream: UnixStream, _slot: Slot) {
    let (read, write) = stream.into_split();
    let conn = Arc::new(Conn::default());
    let writer = write_loop(conn.clone(), write);
    tokio::pin!(writer);
    let reader = read_loop(&holder, &conn, read);
    tokio::pin!(reader);
    tokio::select! {
        () = &mut writer => {
            // The writer ends first on a cut or a broken socket. The reader
            // runs nothing more, but takes what the client still sends for a
            // short time, so a client that stops sending in time reads the
            // close and not a reset.
            conn.outbox.close();
            let _ = tokio::time::timeout(CLOSE_FLUSH, &mut reader).await;
        }
        () = &mut reader => {
            conn.outbox.close();
            let _ = tokio::time::timeout(CLOSE_FLUSH, &mut writer).await;
        }
    }
    conn.outbox.close();
    holder.drop_conn(&conn);
}

/// Sends the connection's queued frames until it closes or is cut. A cut
/// connection's last frames, and a write in flight at the cut, have until
/// the cut's deadline.
async fn write_loop(conn: Arc<Conn>, mut out: OwnedWriteHalf) {
    while let Some(frame) = conn.outbox.next().await {
        let write = out.write_all(&frame);
        tokio::pin!(write);
        let written = tokio::select! {
            written = &mut write => written.is_ok(),
            deadline = conn.outbox.cut_deadline() => {
                matches!(tokio::time::timeout_at(deadline, &mut write).await, Ok(Ok(())))
            }
        };
        if !written {
            return;
        }
    }
    let _ = out.shutdown().await;
}

/// What a connection's reader does after a frame.
enum Next {
    Go,
    End,
}

/// Reads the connection's frames: `hello` first, then requests and raw
/// input, until the client closes or breaks the protocol.
async fn read_loop(holder: &Arc<Holder>, conn: &Arc<Conn>, mut input: OwnedReadHalf) {
    let mut decoder = Decoder::new();
    let mut buf = vec![0; READ_CHUNK];
    let mut greeted = false;
    let handshake_by = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        loop {
            // A cut connection runs nothing more, so no input comes after a
            // gap.
            if conn.outbox.is_closed() {
                discard(&mut input, &mut buf).await;
                return;
            }
            let frame = match decoder.next_frame() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(err) => {
                    reply_error(conn, None, ErrorCode::BadRequest, err.to_string());
                    return;
                }
            };
            let next = if greeted {
                frame_in(holder, conn, frame)
            } else {
                greeted = true;
                hello(holder, conn, frame)
            };
            if let Next::End = next {
                return;
            }
        }
        let read = if greeted {
            input.read(&mut buf).await
        } else {
            match tokio::time::timeout_at(handshake_by, input.read(&mut buf)).await {
                Ok(read) => read,
                Err(_) => {
                    tracing::debug!("a client sent no hello in time");
                    return;
                }
            }
        };
        match read {
            Ok(0) | Err(_) => return,
            Ok(len) => decoder.push(&buf[..len]),
        }
    }
}

/// Reads and drops what a closing client still sends, until it closes.
async fn discard(input: &mut OwnedReadHalf, buf: &mut [u8]) {
    while let Ok(len) = input.read(buf).await {
        if len == 0 {
            return;
        }
    }
}

/// The first frame: `hello`, answered with `welcome` or `refused`.
fn hello(holder: &Holder, conn: &Conn, frame: Frame) -> Next {
    let Frame::Message(text) = frame else {
        reply_error(
            conn,
            None,
            ErrorCode::BadRequest,
            "the first frame must be hello".into(),
        );
        return Next::End;
    };
    let hello: Hello = match Request::decode(&text) {
        Ok(Request::Hello(hello)) => hello,
        Ok(other) => {
            reply_error(
                conn,
                other.id(),
                ErrorCode::BadRequest,
                "the first message must be hello".into(),
            );
            return Next::End;
        }
        Err(undecodable) => {
            conn.send_message(&undecodable.reply());
            return Next::End;
        }
    };
    match handshake::negotiate(hello.proto, SUPPORTED) {
        Some(proto) => {
            tracing::debug!(client = %hello.client, version = %hello.version, proto, "welcome");
            if hello.client == "attach" {
                conn.mark_attach();
            }
            conn.send_message(&HolderMessage::Welcome(holder.welcome(proto)));
            Next::Go
        }
        None => {
            tracing::info!(client = %hello.client, proto = %hello.proto, "refused: no shared version");
            conn.send_message(&HolderMessage::Refused(Refused {
                proto: SUPPORTED,
                message: format!("this holder speaks {SUPPORTED}, the client {}", hello.proto),
            }));
            Next::End
        }
    }
}

/// A frame after the handshake.
fn frame_in(holder: &Arc<Holder>, conn: &Arc<Conn>, frame: Frame) -> Next {
    match frame {
        Frame::Input { num, bytes } => {
            holder.input(conn, num, bytes);
            Next::Go
        }
        Frame::Output { .. } => {
            reply_error(
                conn,
                None,
                ErrorCode::BadRequest,
                "a client never sends kind 1 frames".into(),
            );
            Next::End
        }
        Frame::Message(text) => match Request::decode(&text) {
            Ok(request) => {
                request_in(holder, conn, request);
                Next::Go
            }
            // A message that is no JSON object breaks the protocol, and ends
            // the connection (section 9.4). One of a type or with fields
            // this build does not know is answered, and the connection goes
            // on.
            Err(undecodable) => {
                conn.send_message(&undecodable.reply());
                if serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
                    .is_err()
                {
                    Next::End
                } else {
                    Next::Go
                }
            }
        },
    }
}

/// One request, answered on its connection. A request that waits, for a
/// spawn, a long screen read or a child's exit, is answered from a task of
/// its own, so it holds up none of the connection's other replies (section
/// 2.4). The rest are answered before the next frame is read, so text typed with `input`,
/// `paste` or `keys` keeps its place among the raw input frames.
fn request_in(holder: &Arc<Holder>, conn: &Arc<Conn>, request: Request) {
    let request_id = request.id();
    let reply = match request {
        Request::Hello(_) => Err((ErrorCode::BadRequest, "hello comes once, first".to_owned())),
        Request::Spawn { id, spawn } => {
            let holder = holder.clone();
            answer_later(holder.limits().in_flight, conn, id, async move {
                let (pane, existing) = holder.spawn(spawn).await?;
                Ok(HolderMessage::Pane { id, pane, existing })
            });
            return;
        }
        Request::List { id } => Ok(HolderMessage::Panes {
            id,
            listing: holder.list(),
        }),
        Request::Get { id, pane } => holder.get(&pane).map(|pane| HolderMessage::Pane {
            id,
            pane,
            existing: false,
        }),
        Request::Screen {
            id,
            pane,
            read,
            format,
        } => {
            let holder = holder.clone();
            answer_later(holder.limits().in_flight, conn, id, async move {
                let screen = holder.screen_read(pane, read, format).await?;
                Ok(HolderMessage::Screen { id, screen })
            });
            return;
        }
        Request::Input { id, pane, text } => holder.input_text(conn, &pane, text).map(|()| ok(id)),
        Request::Paste { id, pane, text } => holder.paste(conn, &pane, text).map(|()| ok(id)),
        Request::Keys { id, pane, keys } => holder.keys(conn, &pane, &keys).map(|()| ok(id)),
        Request::Subscribe { id, pane, sync } => match holder.subscribe(conn, id, &pane, sync) {
            Ok(()) => return,
            Err(failure) => Err(failure),
        },
        Request::Unsubscribe { id, stream } => match holder.unsubscribe(conn, id, stream) {
            Ok(()) => return,
            Err(failure) => Err(failure),
        },
        Request::Resize {
            id,
            pane,
            cols,
            rows,
            lease,
        } => holder
            .resize(conn, &pane, tiller_panes::Size { cols, rows }, &lease)
            .map(|size| ok_size(id, size)),
        Request::Release { id, pane, lease } => holder
            .release(conn, &pane, &lease)
            .map(|size| ok_size(id, size)),
        Request::Meta {
            id,
            pane,
            set,
            remove,
        } => holder
            .meta(&pane, set, &remove)
            .map(|pane| HolderMessage::Pane {
                id,
                pane,
                existing: false,
            }),
        Request::Signal { id, pane, signal } => holder.signal(&pane, signal).map(|()| ok(id)),
        Request::Close { id, pane, grace_ms } => {
            let holder = holder.clone();
            answer_later(holder.limits().in_flight, conn, id, async move {
                let exit = holder.close(&pane, Duration::from_millis(grace_ms)).await?;
                Ok(HolderMessage::Exited {
                    id: Some(id),
                    pane,
                    exit,
                })
            });
            return;
        }
        Request::Forget { id, pane } => holder.forget(&pane).map(|()| ok(id)),
        Request::Watch { id } => {
            holder.watch(conn, id);
            return;
        }
        Request::Switch { id, pane } => holder.switch(&pane).map(|clients| HolderMessage::Ok {
            id,
            cols: None,
            rows: None,
            clients: Some(clients),
        }),
        Request::Ping { id } => Ok(HolderMessage::Pong { id }),
        // The holder's upgrade comes with H05.
        request @ Request::Upgrade { .. } => Err((
            ErrorCode::Unsupported,
            format!("this holder does not serve {} yet", request.kind()),
        )),
    };
    send_reply(conn, request_id, reply);
}

/// Answers request `id` from a task of its own, once `reply` is ready.
/// Past `limit` such requests in flight on the connection, it is `busy`.
fn answer_later(
    limit: usize,
    conn: &Arc<Conn>,
    id: u64,
    reply: impl Future<Output = Result<HolderMessage, (ErrorCode, String)>> + Send + 'static,
) {
    let Some(turn) = Slot::take(&conn.in_flight, limit) else {
        return reply_error(
            conn,
            Some(id),
            ErrorCode::Busy,
            format!("{limit} requests of this connection are in flight, its limit"),
        );
    };
    let conn = conn.clone();
    tokio::spawn(async move {
        let reply = reply.await;
        send_reply(&conn, Some(id), reply);
        // The request counts until its answer is queued.
        drop(turn);
    });
}

/// The bare `ok` with the pane's size after the request.
fn ok_size(id: u64, size: tiller_panes::Size) -> HolderMessage {
    HolderMessage::Ok {
        id,
        cols: Some(size.cols),
        rows: Some(size.rows),
        clients: None,
    }
}

/// Queues a reply. A reply that the connection's queue cannot hold now,
/// such as a long `ansi` screen read, is refused with `busy`, so the client
/// learns of it and the connection is not cut.
fn send_reply(conn: &Conn, id: Option<u64>, reply: Result<HolderMessage, (ErrorCode, String)>) {
    let message = match reply {
        Ok(message) => message,
        Err((code, message)) => return reply_error(conn, id, code, message),
    };
    let text = match serde_json::to_string(&message) {
        Ok(text) => text,
        Err(err) => {
            tracing::error!(%err, "a reply would not encode");
            return reply_error(
                conn,
                id,
                ErrorCode::Internal,
                format!("the reply would not encode: {err}"),
            );
        }
    };
    let frame_len = match Frame::Message(text).to_bytes() {
        Ok(frame) => {
            let len = frame.len() as u64;
            match conn.outbox.push(frame) {
                Ok(()) | Err(outbox::Refused::Gone) => return,
                Err(outbox::Refused::Full) => len,
            }
        }
        Err(FrameError::TooLarge { len }) => len + 4,
        Err(err) => {
            return reply_error(
                conn,
                id,
                ErrorCode::Internal,
                format!("the reply would not encode: {err}"),
            );
        }
    };
    let busy = HolderMessage::Error {
        id,
        code: ErrorCode::Busy,
        message: format!(
            "the reply frame is {frame_len} bytes, more than this connection's {OUTBOX_CAP}-byte queue holds now"
        ),
    };
    // With room past the cap, so the client learns of it. Past that room
    // too, the client reads nothing, and is cut.
    if let Some(frame) = message_frame(&busy)
        && conn.outbox.push_over(frame) == Err(outbox::Refused::Full)
    {
        conn.cut();
    }
}

fn reply_error(conn: &Conn, id: Option<u64>, code: ErrorCode, message: String) {
    conn.send_message(&HolderMessage::Error { id, code, message });
}

#[cfg(test)]
mod tests {
    use jiff::{SignedDuration, Timestamp};
    use tiller_panes::PaneId;
    use tiller_panes::frame::MAX_FRAME_LEN;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::holder::Limits;

    async fn hello_on(stream: &mut UnixStream) -> Vec<u8> {
        let hello = serde_json::to_string(&Request::Hello(Hello::new("daemon"))).unwrap();
        let frame = Frame::Message(hello).to_bytes().unwrap();
        // A refused peer's socket may be shut before this lands.
        let _ = stream.write_all(&frame).await;
        let mut got = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut got)).await;
        assert!(read.is_ok(), "the holder neither answered nor closed");
        got
    }

    // The host has no second uid to connect from, so the holder is told it
    // runs as another uid than the real peer's: the accept path then sees a
    // stranger, as it would see one.
    #[tokio::test]
    async fn a_peer_of_another_uid_gets_no_reply_and_a_closed_socket() {
        let holder = Holder::new();
        let (theirs, mut ours) = UnixStream::pair().unwrap();
        admit(&holder, theirs, peer::own_uid().wrapping_add(1));
        assert!(hello_on(&mut ours).await.is_empty());
    }

    #[tokio::test]
    async fn a_peer_of_the_holders_uid_gets_its_welcome() {
        let holder = Holder::new();
        let (theirs, mut ours) = UnixStream::pair().unwrap();
        admit(&holder, theirs, peer::own_uid());
        let hello = serde_json::to_string(&Request::Hello(Hello::new("daemon"))).unwrap();
        ours.write_all(&Frame::Message(hello).to_bytes().unwrap())
            .await
            .unwrap();
        let mut len = [0; 4];
        ours.read_exact(&mut len).await.unwrap();
        let mut body = vec![0; u32::from_be_bytes(len) as usize];
        ours.read_exact(&mut body).await.unwrap();
        assert_eq!(body[0], 0, "not a kind 0 frame");
        let reply: serde_json::Value = serde_json::from_slice(&body[1..]).unwrap();
        assert_eq!(reply["type"], "welcome", "{reply}");
    }

    #[tokio::test]
    async fn a_reply_no_queue_could_hold_is_refused_and_the_connection_stays() {
        let conn = Conn::default();
        let screen = tiller_panes::message::Screen {
            cols: 80,
            rows: 24,
            cursor: tiller_panes::message::Cursor {
                col: 0,
                row: 0,
                visible: true,
            },
            modes: Vec::new(),
            rows_text: Vec::new(),
        };
        let json_len = |row: usize| {
            let screen = tiller_panes::message::Screen {
                rows_text: vec!["x".repeat(row)],
                ..screen.clone()
            };
            serde_json::to_string(&HolderMessage::Screen { id: 7, screen })
                .unwrap()
                .len()
        };
        // A message just under the queue whose length and kind push its
        // frame past it, one past the queue, and one past the frame limit.
        let overhead = json_len(0);
        let under = OUTBOX_CAP - 2 - overhead;
        assert!(json_len(under) < OUTBOX_CAP && json_len(under) + 5 > OUTBOX_CAP);
        let rows = [under, OUTBOX_CAP, MAX_FRAME_LEN as usize];
        for (id, len) in (7..).zip(rows) {
            let screen = tiller_panes::message::Screen {
                rows_text: vec!["x".repeat(len)],
                ..screen.clone()
            };
            send_reply(&conn, Some(id), Ok(HolderMessage::Screen { id, screen }));
            assert!(conn.outbox.cut_at().is_none(), "the connection was cut");
            let frame = conn.outbox.next().await.unwrap();
            // The length and the kind come before the message.
            let reply: serde_json::Value = serde_json::from_slice(&frame[5..]).unwrap();
            assert_eq!(
                (&reply["type"], &reply["id"], &reply["code"]),
                (&"error".into(), &id.into(), &"busy".into()),
                "{reply}"
            );
        }
    }

    async fn read_message(stream: &mut UnixStream) -> serde_json::Value {
        let mut len = [0; 4];
        stream.read_exact(&mut len).await.unwrap();
        let mut body = vec![0; u32::from_be_bytes(len) as usize];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(body[0], 0, "not a kind 0 frame");
        serde_json::from_slice(&body[1..]).unwrap()
    }

    fn spawn_request(id: u64, key: &str, argv: &[&str]) -> Request {
        Request::Spawn {
            id,
            spawn: tiller_panes::Spawn {
                key: key.to_owned(),
                argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
                cwd: "/".to_owned(),
                env: std::collections::BTreeMap::from([(
                    "PATH".to_owned(),
                    "/usr/bin:/bin".to_owned(),
                )]),
                size: None,
                meta: tiller_panes::Meta::new(),
            },
        }
    }

    /// A client's end of a connection, read frame by frame.
    struct Client {
        stream: UnixStream,
        decoder: Decoder,
    }

    impl Client {
        async fn connect(holder: &Arc<Holder>) -> Client {
            let (theirs, stream) = UnixStream::pair().unwrap();
            admit(holder, theirs, peer::own_uid());
            let mut client = Client {
                stream,
                decoder: Decoder::new(),
            };
            client.send(Request::Hello(Hello::new("daemon"))).await;
            let welcome = client.message(|_| true).await;
            assert_eq!(welcome["type"], "welcome");
            client
        }

        async fn send(&mut self, request: Request) {
            let text = serde_json::to_string(&request).unwrap();
            self.stream
                .write_all(&Frame::Message(text).to_bytes().unwrap())
                .await
                .unwrap();
        }

        /// The frames up to the first `wanted` takes, which it returns.
        async fn until(&mut self, mut wanted: impl FnMut(&Frame) -> bool) -> Frame {
            let read = async {
                loop {
                    while let Some(frame) = self.decoder.next_frame().unwrap() {
                        if wanted(&frame) {
                            return frame;
                        }
                    }
                    let mut buf = [0; 4096];
                    let len = self.stream.read(&mut buf).await.unwrap();
                    assert_ne!(len, 0, "the holder closed the connection");
                    self.decoder.push(&buf[..len]);
                }
            };
            tokio::time::timeout(Duration::from_secs(10), read)
                .await
                .unwrap()
        }

        /// The first message `wanted` takes.
        async fn message(
            &mut self,
            mut wanted: impl FnMut(&serde_json::Value) -> bool,
        ) -> serde_json::Value {
            let frame = self
                .until(|frame| match frame {
                    Frame::Message(text) => wanted(&serde_json::from_str(text).unwrap()),
                    _ => false,
                })
                .await;
            let Frame::Message(text) = frame else {
                unreachable!()
            };
            serde_json::from_str(&text).unwrap()
        }

        async fn reply(&mut self, id: u64) -> serde_json::Value {
            self.message(|message| message["id"] == id).await
        }

        /// Output on `stream` up to the first read that holds `text`, which
        /// it returns.
        async fn output(&mut self, stream: u32, text: &str) -> String {
            let frame = self
                .until(|frame| match frame {
                    Frame::Output { stream: on, bytes } => {
                        *on == stream && String::from_utf8_lossy(bytes).contains(text)
                    }
                    _ => false,
                })
                .await;
            let Frame::Output { bytes, .. } = frame else {
                unreachable!()
            };
            String::from_utf8_lossy(&bytes).into_owned()
        }

        async fn spawn_cat(&mut self, id: u64, key: &str) -> PaneId {
            self.spawn(id, key, &["/bin/cat"]).await
        }

        async fn spawn(&mut self, id: u64, key: &str, argv: &[&str]) -> PaneId {
            self.send(spawn_request(id, key, argv)).await;
            let reply = self.reply(id).await;
            assert_eq!(reply["type"], "pane", "{reply}");
            serde_json::from_value(reply["pane"]["pane"].clone()).unwrap()
        }

        async fn subscribe(&mut self, id: u64, pane: &PaneId) -> u32 {
            self.send(Request::Subscribe {
                id,
                pane: pane.clone(),
                sync: tiller_panes::message::SyncMode::Pass,
            })
            .await;
            let reply = self.reply(id).await;
            assert_eq!(reply["type"], "subscribed", "{reply}");
            u32::try_from(reply["stream"].as_u64().unwrap()).unwrap()
        }

        async fn input(&mut self, id: u64, pane: &PaneId, text: &str) {
            self.send(Request::Input {
                id,
                pane: pane.clone(),
                text: text.to_owned(),
            })
            .await;
            assert_eq!(self.reply(id).await["type"], "ok");
        }
    }

    // Section 3.5: a model that panics resets its own pane. That pane's
    // subscribers are cut and its watchers told, while its child, the
    // other panes and the holder go on.
    #[tokio::test]
    async fn a_panicking_model_resets_its_pane_and_the_holder_and_other_panes_live_on() {
        let holder = Holder::new();
        tokio::spawn(holder.clone().reaper(signal(SignalKind::child()).unwrap()));
        let mut watcher = Client::connect(&holder).await;
        watcher.send(Request::Watch { id: 1 }).await;
        assert_eq!(watcher.reply(1).await["type"], "watching");
        let mut client = Client::connect(&holder).await;
        let a = client.spawn_cat(2, "a").await;
        let b = client.spawn_cat(3, "b").await;
        let on_a = client.subscribe(4, &a).await;
        let on_b = client.subscribe(5, &b).await;

        holder.fail_next(&a);
        // `cat`'s terminal echoes the text, and the echo goes into the model.
        client.input(6, &a, "boom\r").await;
        let lagged = client.message(|message| message["type"] == "lagged").await;
        assert_eq!(lagged, serde_json::json!({"type":"lagged","stream":on_a}));
        let reset = watcher
            .message(|message| message["type"] == "model_reset")
            .await;
        assert_eq!(reset["pane"], a.as_str());

        client.input(7, &b, "still\r").await;
        client.output(on_b, "still").await;
        client.send(Request::Ping { id: 8 }).await;
        assert_eq!(client.reply(8).await["type"], "pong");
        client
            .send(Request::Get {
                id: 9,
                pane: a.clone(),
            })
            .await;
        assert_eq!(client.reply(9).await["pane"]["state"], "running");
        let again = client.subscribe(10, &a).await;
        client.input(11, &a, "again\r").await;
        client.output(again, "again").await;

        for (id, pane) in [(12, &a), (13, &b)] {
            client
                .send(Request::Close {
                    id,
                    pane: pane.clone(),
                    grace_ms: 3_000,
                })
                .await;
            assert_eq!(client.reply(id).await["type"], "exited");
        }
    }

    #[tokio::test]
    async fn a_request_this_holder_does_not_serve_yet_is_unsupported_and_a_ping_is_answered() {
        let holder = Holder::new();
        let (theirs, mut ours) = UnixStream::pair().unwrap();
        admit(&holder, theirs, peer::own_uid());
        for request in [
            Request::Hello(Hello::new("daemon")),
            Request::Upgrade {
                id: 7,
                binary: "/home/user/tiller".into(),
                dry_run: false,
            },
            Request::Ping { id: 8 },
        ] {
            let text = serde_json::to_string(&request).unwrap();
            ours.write_all(&Frame::Message(text).to_bytes().unwrap())
                .await
                .unwrap();
        }
        assert_eq!(read_message(&mut ours).await["type"], "welcome");
        let unsupported = read_message(&mut ours).await;
        assert_eq!(
            unsupported,
            serde_json::json!({"type":"error","id":7,"code":"unsupported",
                "message":"this holder does not serve upgrade yet"})
        );
        assert_eq!(
            read_message(&mut ours).await,
            serde_json::json!({"type":"pong","id":8})
        );
    }

    fn resize(id: u64, pane: &PaneId, cols: u16, rows: u16, lease: &str) -> Request {
        Request::Resize {
            id,
            pane: pane.clone(),
            cols,
            rows,
            lease: lease.to_owned(),
        }
    }

    fn release(id: u64, pane: &PaneId, lease: &str) -> Request {
        Request::Release {
            id,
            pane: pane.clone(),
            lease: lease.to_owned(),
        }
    }

    fn sized(id: u64, cols: u16, rows: u16) -> serde_json::Value {
        serde_json::json!({"type":"ok","id":id,"cols":cols,"rows":rows})
    }

    /// The pane's size, as `get` reports it.
    async fn size_of(client: &mut Client, id: u64, pane: &PaneId) -> (u64, u64) {
        client
            .send(Request::Get {
                id,
                pane: pane.clone(),
            })
            .await;
        let reply = client.reply(id).await;
        let pane = &reply["pane"];
        (
            pane["cols"].as_u64().unwrap(),
            pane["rows"].as_u64().unwrap(),
        )
    }

    // Section 2.6: the top lease sets the size, a release gives it to the
    // lease under it, and a connection's end drops its leases. With none
    // left, the size stays.
    #[tokio::test]
    async fn the_top_lease_sets_the_size_and_a_release_gives_it_back() {
        let holder = Holder::new();
        tokio::spawn(holder.clone().reaper(signal(SignalKind::child()).unwrap()));
        let mut watcher = Client::connect(&holder).await;
        watcher.send(Request::Watch { id: 1 }).await;
        assert_eq!(watcher.reply(1).await["type"], "watching");
        let mut a = Client::connect(&holder).await;
        let mut b = Client::connect(&holder).await;
        let pane = a.spawn_cat(2, "a").await;
        let resized = |cols: u16, rows: u16| serde_json::json!({"type":"resized","pane":pane.as_str(),"cols":cols,"rows":rows});

        a.send(resize(3, &pane, 100, 30, "fit")).await;
        assert_eq!(a.reply(3).await, sized(3, 100, 30));
        assert_eq!(
            watcher.message(|m| m["type"] == "resized").await,
            resized(100, 30)
        );
        // The same lease id on another connection is another lease.
        b.send(resize(4, &pane, 90, 20, "fit")).await;
        assert_eq!(b.reply(4).await, sized(4, 90, 20));
        assert_eq!(
            watcher.message(|m| m["type"] == "resized").await,
            resized(90, 20)
        );
        a.send(resize(5, &pane, 110, 35, "attach")).await;
        assert_eq!(a.reply(5).await, sized(5, 110, 35));
        assert_eq!(size_of(&mut a, 6, &pane).await, (110, 35));

        // The release gives the size back to the lease under it.
        a.send(release(7, &pane, "attach")).await;
        assert_eq!(a.reply(7).await, sized(7, 90, 20));
        assert_eq!(
            watcher.message(|m| m["type"] == "resized").await,
            resized(110, 35)
        );
        assert_eq!(
            watcher.message(|m| m["type"] == "resized").await,
            resized(90, 20)
        );
        // A lease the connection does not hold changes nothing.
        a.send(release(8, &pane, "attach")).await;
        assert_eq!(a.reply(8).await, sized(8, 90, 20));

        // The end of b's connection drops its lease, and a's is on top.
        drop(b);
        assert_eq!(
            watcher.message(|m| m["type"] == "resized").await,
            resized(100, 30)
        );
        assert_eq!(size_of(&mut a, 9, &pane).await, (100, 30));
        // With no lease left, the size stays.
        a.send(release(10, &pane, "fit")).await;
        assert_eq!(a.reply(10).await, sized(10, 100, 30));
        assert_eq!(size_of(&mut a, 11, &pane).await, (100, 30));

        a.send(Request::Close {
            id: 12,
            pane: pane.clone(),
            grace_ms: 3_000,
        })
        .await;
        assert_eq!(a.reply(12).await["type"], "exited");
        // An exited pane takes no lease.
        a.send(resize(13, &pane, 80, 24, "fit")).await;
        assert_eq!(a.reply(13).await["code"], "exited");
    }

    #[tokio::test]
    async fn a_pane_holds_at_most_its_limit_of_leases() {
        let holder = Holder::new();
        tokio::spawn(holder.clone().reaper(signal(SignalKind::child()).unwrap()));
        let mut client = Client::connect(&holder).await;
        let pane = client.spawn_cat(1, "a").await;
        for n in 0..crate::lease::LEASES_PER_PANE as u64 {
            client
                .send(resize(n + 2, &pane, 80, 24, &n.to_string()))
                .await;
            assert_eq!(client.reply(n + 2).await["type"], "ok");
        }
        client.send(resize(100, &pane, 80, 24, "one more")).await;
        assert_eq!(client.reply(100).await["code"], "busy");
        client
            .send(Request::Close {
                id: 101,
                pane: pane.clone(),
                grace_ms: 3_000,
            })
            .await;
        assert_eq!(client.reply(101).await["type"], "exited");
    }

    // Section 2.8, with small limits: a pane or a subscriber past its limit
    // is `busy`, and a connection past it is closed.
    #[tokio::test]
    async fn a_pane_a_subscriber_and_a_connection_past_their_limits_are_refused() {
        let holder = Holder::with_limits(Limits {
            panes: 1,
            subscribers: 1,
            connections: 2,
            ..Limits::default()
        });
        tokio::spawn(holder.clone().reaper(signal(SignalKind::child()).unwrap()));
        let mut client = Client::connect(&holder).await;
        let pane = client.spawn_cat(1, "a").await;
        client.send(spawn_request(2, "b", &["/bin/cat"])).await;
        assert_eq!(client.reply(2).await["code"], "busy");
        // The same key is the same pane, not one more.
        assert_eq!(client.spawn_cat(3, "a").await, pane);

        client.subscribe(4, &pane).await;
        client
            .send(Request::Subscribe {
                id: 5,
                pane: pane.clone(),
                sync: tiller_panes::message::SyncMode::Pass,
            })
            .await;
        assert_eq!(client.reply(5).await["code"], "busy");

        let second = Client::connect(&holder).await;
        let (theirs, mut ours) = UnixStream::pair().unwrap();
        admit(&holder, theirs, peer::own_uid());
        assert!(
            hello_on(&mut ours).await.is_empty(),
            "a third connection was served"
        );
        // A connection's end gives its place back.
        drop(second);
        let mut third = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (theirs, mut ours) = UnixStream::pair().unwrap();
                admit(&holder, theirs, peer::own_uid());
                let hello = serde_json::to_string(&Request::Hello(Hello::new("daemon"))).unwrap();
                let mut len = [0; 4];
                if ours
                    .write_all(&Frame::Message(hello).to_bytes().unwrap())
                    .await
                    .is_ok()
                    && ours.read_exact(&mut len).await.is_ok()
                {
                    return ours;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        third.shutdown().await.unwrap();

        client
            .send(Request::Close {
                id: 6,
                pane: pane.clone(),
                grace_ms: 3_000,
            })
            .await;
        assert_eq!(client.reply(6).await["type"], "exited");
    }

    // A child that leaves its group behind as it exits: the exit is
    // reported, and what is left of the group, which outlived `hup` and
    // holds the PTY, is killed at the end of the grace. Past the
    // connection's limit of requests in flight, one more is `busy`.
    #[tokio::test]
    async fn a_close_kills_the_group_its_child_left_and_a_request_past_the_flight_limit_is_busy() {
        let holder = Holder::with_limits(Limits {
            in_flight: 1,
            ..Limits::default()
        });
        tokio::spawn(holder.clone().reaper(signal(SignalKind::child()).unwrap()));
        let mut client = Client::connect(&holder).await;
        // The sleep ignores `hup`; the shell does not, and exits on it.
        let pane = client
            .spawn(
                1,
                "a",
                &[
                    "/bin/sh",
                    "-c",
                    "trap '' HUP; sleep 1000 & trap - HUP; echo \"left $!.\"; wait",
                ],
            )
            .await;
        let on = client.subscribe(2, &pane).await;
        let said = client.output(on, ".").await;
        let left = said
            .split_once("left ")
            .and_then(|(_, rest)| rest.split_once('.'))
            .and_then(|(pid, _)| pid.parse().ok())
            .and_then(rustix::process::Pid::from_raw)
            .unwrap_or_else(|| panic!("no pid in {said:?}"));
        let lives = || rustix::process::test_kill_process(left).is_ok();
        assert!(lives());
        let started = std::time::Instant::now();
        client
            .send(Request::Close {
                id: 3,
                pane: pane.clone(),
                grace_ms: 1_000,
            })
            .await;
        // The close is in flight until the shell's exit is in.
        client.send(spawn_request(4, "b", &["/bin/cat"])).await;
        let busy = client.reply(4).await;
        assert_eq!(busy["code"], "busy", "{busy}");
        let exited = client.reply(3).await;
        assert_eq!(exited["exit"]["signal"], 1, "{exited}");
        assert!(lives(), "the group was killed before its grace ended");
        let dead = tokio::time::timeout(Duration::from_secs(5), async {
            while lives() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(dead.is_ok(), "what the shell left lives on");
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "the group was killed before its grace ended: {:?}",
            started.elapsed()
        );
        // Its place is free again.
        client.spawn_cat(5, "b").await;
    }

    // Section 2.8: past the limit of exited panes the oldest is forgotten,
    // and so is one past its age.
    #[tokio::test]
    async fn exited_panes_past_their_limit_or_their_age_are_forgotten() {
        let holder = Holder::with_limits(Limits {
            exited: 1,
            ..Limits::default()
        });
        tokio::spawn(holder.clone().reaper(signal(SignalKind::child()).unwrap()));
        let mut watcher = Client::connect(&holder).await;
        watcher.send(Request::Watch { id: 1 }).await;
        assert_eq!(watcher.reply(1).await["type"], "watching");
        let mut client = Client::connect(&holder).await;
        let first = client.spawn(2, "first", &["/bin/sh", "-c", "exit 3"]).await;
        watcher
            .message(|m| m["type"] == "exited" && m["pane"] == first.as_str())
            .await;
        let second = client
            .spawn(3, "second", &["/bin/sh", "-c", "exit 4"])
            .await;
        let forgotten = watcher.message(|m| m["type"] == "forgotten").await;
        assert_eq!(
            forgotten,
            serde_json::json!({"type":"forgotten","pane":first.as_str(),"reason":"limit"})
        );
        client
            .send(Request::Get {
                id: 4,
                pane: first.clone(),
            })
            .await;
        assert_eq!(client.reply(4).await["code"], "not_found");

        // A day later, the second is past its age.
        holder.prune(Timestamp::now());
        client
            .send(Request::Get {
                id: 5,
                pane: second.clone(),
            })
            .await;
        assert_eq!(client.reply(5).await["pane"]["state"], "exited");
        holder.prune(Timestamp::now() + SignedDuration::from_hours(25));
        let forgotten = watcher.message(|m| m["type"] == "forgotten").await;
        assert_eq!(
            forgotten,
            serde_json::json!({"type":"forgotten","pane":second.as_str(),"reason":"limit"})
        );
    }
}
