//! The holder's socket (`terminal-holder.md` 2.2 to 2.4): the run
//! directory, the lock, the listener, the peer check and each connection's
//! handshake and frames.

use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tiller_panes::frame::{Decoder, Frame};
use tiller_panes::handshake::{self, Hello, Refused, SUPPORTED};
use tiller_panes::message::{ErrorCode, HolderMessage, Request};
use tiller_panes::paths::{SocketPathTooLong, check_socket_path};
use tiller_panes::{run_dir, socket_path};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};

use crate::holder::{Conn, Holder, ok};
use crate::lock::{Acquired, HolderLock, lock_path};
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
/// closes.
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
    tracing::debug!(%peer, "a connection");
    tokio::spawn(connection(holder.clone(), stream));
}

/// One connection, from `hello` to its end.
async fn connection(holder: Arc<Holder>, stream: UnixStream) {
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
                frame_in(holder, conn, frame).await
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
async fn frame_in(holder: &Arc<Holder>, conn: &Arc<Conn>, frame: Frame) -> Next {
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
            Err(undecodable) => {
                conn.send_message(&undecodable.reply());
                Next::Go
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
            answer_later(conn, id, async move {
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
            answer_later(conn, id, async move {
                let screen = holder.screen_read(pane, read, format).await?;
                Ok(HolderMessage::Screen { id, screen })
            });
            return;
        }
        Request::Input { id, pane, text } => holder.input_text(conn, &pane, text).map(|()| ok(id)),
        Request::Paste { id, pane, text } => holder.paste(conn, &pane, text).map(|()| ok(id)),
        Request::Keys { id, pane, keys } => holder.keys(conn, &pane, &keys).map(|()| ok(id)),
        // `sync` is `pass` for every subscriber: this holder does not offer
        // `sync_buffer` (H04), so a client does not ask for it.
        Request::Subscribe { id, pane, sync: _ } => match holder.subscribe(conn, id, &pane) {
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
            lease: _,
        } => holder
            .resize(&pane, tiller_panes::Size { cols, rows })
            .map(|size| ok_size(id, size)),
        Request::Release { id, pane, lease: _ } => {
            holder.release(&pane).map(|size| ok_size(id, size))
        }
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
            answer_later(conn, id, async move {
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
fn answer_later(
    conn: &Arc<Conn>,
    id: u64,
    reply: impl Future<Output = Result<HolderMessage, (ErrorCode, String)>> + Send + 'static,
) {
    let conn = conn.clone();
    tokio::spawn(async move {
        let reply = reply.await;
        send_reply(&conn, Some(id), reply);
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

fn send_reply(conn: &Conn, id: Option<u64>, reply: Result<HolderMessage, (ErrorCode, String)>) {
    match reply {
        Ok(message) => {
            conn.send_message(&message);
        }
        Err((code, message)) => reply_error(conn, id, code, message),
    }
}

fn reply_error(conn: &Conn, id: Option<u64>, code: ErrorCode, message: String) {
    conn.send_message(&HolderMessage::Error { id, code, message });
}

#[cfg(test)]
mod tests {
    use tiller_panes::PaneId;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

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

    async fn read_message(stream: &mut UnixStream) -> serde_json::Value {
        let mut len = [0; 4];
        stream.read_exact(&mut len).await.unwrap();
        let mut body = vec![0; u32::from_be_bytes(len) as usize];
        stream.read_exact(&mut body).await.unwrap();
        assert_eq!(body[0], 0, "not a kind 0 frame");
        serde_json::from_slice(&body[1..]).unwrap()
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

        /// Output on `stream` up to the first read that holds `text`.
        async fn output(&mut self, stream: u32, text: &str) {
            self.until(|frame| match frame {
                Frame::Output { stream: on, bytes } => {
                    *on == stream && String::from_utf8_lossy(bytes).contains(text)
                }
                _ => false,
            })
            .await;
        }

        async fn spawn_cat(&mut self, id: u64, key: &str) -> PaneId {
            self.send(Request::Spawn {
                id,
                spawn: tiller_panes::Spawn {
                    key: key.to_owned(),
                    argv: vec!["/bin/cat".to_owned()],
                    cwd: "/".to_owned(),
                    env: std::collections::BTreeMap::new(),
                    size: None,
                    meta: tiller_panes::Meta::new(),
                },
            })
            .await;
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
}
