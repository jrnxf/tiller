//! The holder's state: its panes and what each request does to them
//! (`terminal-holder.md` 2.4 to 2.7).
//!
//! Each pane has three tasks: one reads its PTY into the model and out to
//! its subscribers, one writes raw input to it, and one delivers its exit.
//! One reaper serves every pane. A pane's lock covers its record, model and
//! subscribers, so a new subscriber's repaint and the live bytes after it
//! are queued in order, and nothing waits for a client under it.

use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use rustix::process::{Pid, Signal, WaitOptions};
use tiller_panes::frame::Frame;
use tiller_panes::handshake::Welcome;
use tiller_panes::message::{ErrorCode, HolderMessage};
use tiller_panes::{Exit, Listing, Pane, PaneId, PaneState, Size, Spawn};
use tokio::io::unix::AsyncFd;
use tokio::sync::{Notify, watch};

use crate::input::{self, INPUT_CAP, InputQueue};
use crate::model::{MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS, Model, ModelPanic};
use crate::outbox::{Outbox, Refused};
use crate::pty;

/// How long an exit waits for the child's last output before `exited` is
/// sent, when something else still holds the PTY open.
pub const EXIT_DRAIN: Duration = Duration::from_millis(250);

/// The most a PTY read takes at once.
const READ_CHUNK: usize = 64 * 1024;

/// An error reply's code and message.
type Failure = (ErrorCode, String);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A kind 0 frame, encoded.
pub fn message_frame(message: &HolderMessage) -> Option<Vec<u8>> {
    let text = match serde_json::to_string(message) {
        Ok(text) => text,
        Err(err) => {
            tracing::error!(%err, "a holder message would not encode");
            return None;
        }
    };
    frame_bytes(&Frame::Message(text))
}

fn frame_bytes(frame: &Frame) -> Option<Vec<u8>> {
    match frame.to_bytes() {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            tracing::error!(%err, "a frame would not encode");
            None
        }
    }
}

/// One client connection, as the panes see it.
#[derive(Debug, Default)]
pub struct Conn {
    pub outbox: Outbox,
    /// This connection, as the panes' input queues see it.
    sender: Arc<input::Sender>,
    /// This connection's subscriptions: each stream and its pane.
    streams: Mutex<Vec<(u32, Arc<PaneCell>)>>,
}

impl Conn {
    /// Queues a frame. A full queue cuts the connection. False when the
    /// frame was not queued.
    pub fn send(&self, frame: Vec<u8>) -> bool {
        match self.outbox.push(frame) {
            Ok(()) => true,
            Err(Refused::Full) => {
                if self.cut() {
                    tracing::warn!(
                        "a client fell behind its {}-byte queue and was cut",
                        crate::outbox::OUTBOX_CAP
                    );
                }
                false
            }
            Err(Refused::Gone) => false,
        }
    }

    pub fn send_message(&self, message: &HolderMessage) -> bool {
        message_frame(message).is_some_and(|frame| self.send(frame))
    }

    /// Queues a subscription's first frames, all or none, apart from the
    /// cap on live output. False when they were not queued.
    fn send_repaint(&self, frames: Vec<Vec<u8>>) -> bool {
        match self.outbox.push_repaint(frames) {
            Ok(()) => true,
            Err(Refused::Full) => {
                if self.cut() {
                    tracing::warn!("a client let its repaints pile up and was cut");
                }
                false
            }
            Err(Refused::Gone) => false,
        }
    }

    /// Cuts the connection: its queue is dropped, each of its streams gets
    /// `lagged`, and it ends.
    /// False when it was cut or closed already.
    pub fn cut(&self) -> bool {
        let last = lock(&self.streams)
            .iter()
            .filter_map(|(stream, _)| message_frame(&HolderMessage::Lagged { stream: *stream }))
            .collect();
        let cut = self.outbox.cut(last);
        if cut {
            self.sender.cut();
        }
        cut
    }

    /// Drops one subscription from this connection's list.
    fn forget(&self, stream: u32) {
        lock(&self.streams).retain(|(own, _)| *own != stream);
    }

    /// Takes this connection's subscriptions, at its end.
    fn take_streams(&self) -> Vec<(u32, Arc<PaneCell>)> {
        std::mem::take(&mut *lock(&self.streams))
    }
}

/// A subscriber of one pane.
#[derive(Debug)]
struct Subscriber {
    stream: u32,
    conn: Arc<Conn>,
}

/// What a pane's lock covers.
#[derive(Debug)]
struct Inner {
    record: Pane,
    model: Model,
    subscribers: Vec<Subscriber>,
    /// The reaper has collected the child. Its exit goes into the record
    /// once its last output is in.
    reaped: bool,
}

impl Inner {
    /// The pane's record as `list` shows it.
    fn record(&self) -> Pane {
        Pane {
            subscribers: u32::try_from(self.subscribers.len()).unwrap_or(u32::MAX),
            ..self.record.clone()
        }
    }

    /// Whether the child still runs: not yet reaped. A reaped child's exit
    /// goes into the record a little later, once its last output is in.
    fn running(&self) -> bool {
        self.record.state == PaneState::Running && !self.reaped
    }

    /// Sends `frame_for(stream)` to each subscriber, and drops each one
    /// whose connection is cut or gone.
    fn fan_out(&mut self, frame_for: impl Fn(u32) -> Option<Vec<u8>>) {
        self.subscribers
            .retain(|sub| frame_for(sub.stream).is_some_and(|frame| sub.conn.send(frame)));
    }

    /// Ends every subscription of this pane with `lagged`: the model was
    /// reset, so what they drew from it is stale. Each may subscribe again
    /// for a repaint. The connections stay, with any other pane's streams.
    fn lag_all(&mut self) {
        for sub in self.subscribers.drain(..) {
            // Sent while the stream is still the connection's, so a cut on a
            // full queue sends its `lagged` too.
            sub.conn
                .send_message(&HolderMessage::Lagged { stream: sub.stream });
            sub.conn.forget(sub.stream);
        }
    }
}

/// One pane: its PTY, its child and its screen.
#[derive(Debug)]
pub struct PaneCell {
    id: PaneId,
    num: u32,
    /// The child's pid, which is also its process group's id.
    pid: Pid,
    master: AsyncFd<OwnedFd>,
    /// The raw input not yet written.
    input: InputQueue,
    inner: Mutex<Inner>,
    /// True once the PTY read ends: every holder of the slave closed it.
    output_ended: watch::Sender<bool>,
}

impl PaneCell {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// A model panic: the model is fresh already. The app in front redraws
    /// on `SIGWINCH`, and every subscriber of the pane is cut (section 3.5).
    fn model_reset(&self, inner: &mut Inner, panic: &ModelPanic) {
        tracing::error!(
            pane = %self.id,
            panic = %panic.message,
            recent = %String::from_utf8_lossy(&panic.recent).escape_debug(),
            "the pane's screen model panicked and was reset"
        );
        if inner.running() {
            self.redraw();
        }
        inner.lag_all();
    }

    /// Asks the terminal's foreground process group to redraw. That is the
    /// app that draws the screen: a shell's job runs in a group of its own.
    fn redraw(&self) {
        let group = match rustix::termios::tcgetpgrp(self.master.get_ref()) {
            Ok(group) => group,
            Err(err) => {
                tracing::debug!(pane = %self.id, %err, "no foreground group; the child's is used");
                self.pid
            }
        };
        if let Err(err) = rustix::process::kill_process_group(group, Signal::WINCH) {
            tracing::debug!(pane = %self.id, %err, "the redraw signal failed");
        }
    }

    /// Sends `signal` to the child's process group.
    fn signal(&self, signal: Signal) {
        if let Err(err) = rustix::process::kill_process_group(self.pid, signal) {
            tracing::debug!(pane = %self.id, %err, "a signal to the pane's group failed");
        }
    }

    /// One PTY read: into the model, and out to every subscriber.
    fn output(&self, bytes: &[u8]) {
        let mut inner = self.inner();
        inner.record.last_output_at = Some(Timestamp::now());
        if let Err(panic) = inner.model.feed(bytes) {
            self.model_reset(&mut inner, &panic);
            return;
        }
        inner.fan_out(|stream| {
            frame_bytes(&Frame::Output {
                stream,
                bytes: bytes.to_vec(),
            })
        });
    }

    /// The open synchronized update's deadline passed.
    fn end_sync(&self) {
        let mut inner = self.inner();
        if let Err(panic) = inner.model.end_sync() {
            self.model_reset(&mut inner, &panic);
        }
    }

    fn sync_deadline(&self) -> Option<tokio::time::Instant> {
        self.inner()
            .model
            .sync_deadline()
            .map(tokio::time::Instant::from_std)
    }
}

/// The holder's panes and counters.
#[derive(Debug, Default)]
struct Table {
    /// In the order they were spawned.
    panes: Vec<Arc<PaneCell>>,
    next_num: u32,
}

/// The holder's whole state.
#[derive(Debug)]
pub struct Holder {
    instance: String,
    boot_id: String,
    started_at: Timestamp,
    table: Mutex<Table>,
    /// Held for a whole `spawn`, so two spawns with one key start one
    /// child, while the table's lock is free for everything else during
    /// the fork and exec.
    spawning: Mutex<()>,
    next_stream: AtomicU32,
    /// Wakes the reaper after a spawn, so an exit that came before the pane
    /// was in the table is not missed.
    reap: Notify,
}

impl Holder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            instance: ulid::Ulid::generate().to_string(),
            boot_id: boot_id(),
            started_at: Timestamp::now(),
            table: Mutex::new(Table::default()),
            spawning: Mutex::new(()),
            next_stream: AtomicU32::new(1),
            reap: Notify::new(),
        })
    }

    /// The handshake's answer, for a version both sides speak.
    pub fn welcome(&self, proto: u32) -> Welcome {
        Welcome {
            proto,
            holder: env!("CARGO_PKG_VERSION").to_owned(),
            instance: self.instance.clone(),
            boot_id: self.boot_id.clone(),
            started_at: self.started_at,
            // Kitty keys and sync buffering come with H04.
            features: Vec::new(),
        }
    }

    fn panes(&self) -> Vec<Arc<PaneCell>> {
        lock(&self.table).panes.clone()
    }

    fn find(&self, pane: &PaneId) -> Option<Arc<PaneCell>> {
        lock(&self.table)
            .panes
            .iter()
            .find(|cell| cell.id == *pane)
            .cloned()
    }

    fn by_num(&self, num: u32) -> Option<Arc<PaneCell>> {
        lock(&self.table)
            .panes
            .iter()
            .find(|cell| cell.num == num)
            .cloned()
    }

    /// `spawn`: a new pane, or the running pane that holds the key. It
    /// forks and waits for the exec, so it runs off the runtime's workers.
    pub fn spawn(self: &Arc<Self>, req: Spawn) -> Result<(Pane, bool), Failure> {
        let size = req.size.unwrap_or(tiller_panes::message::DEFAULT_SIZE);
        check_size(size)?;
        if !Path::new(&req.cwd).is_absolute() {
            return Err(bad_request(format!(
                "cwd {:?} is not an absolute path",
                req.cwd
            )));
        }
        let _spawning = lock(&self.spawning);
        let running = self.panes().iter().find_map(|cell| {
            let inner = cell.inner();
            (inner.record.key == req.key && inner.running()).then(|| inner.record())
        });
        if let Some(record) = running {
            return Ok((record, true));
        }
        let id = PaneId::generate();
        let started = pty::start(pty::Launch {
            argv: &req.argv,
            cwd: Path::new(&req.cwd),
            env: &req.env,
            size,
            pane: id.as_str(),
        })
        .map_err(|err| spawn_failure(&req, &err))?;
        let master = match register(started.master) {
            Ok(master) => master,
            Err(err) => {
                // Not in the table, so the reaper would never collect it.
                kill_and_reap(started.pid);
                return Err(internal(&err));
            }
        };
        let mut table = lock(&self.table);
        table.next_num += 1;
        let num = table.next_num;
        let record = Pane {
            pane: id.clone(),
            num,
            key: req.key,
            pid: started.pid.as_raw_nonzero().get().unsigned_abs(),
            argv: req.argv,
            cwd: req.cwd,
            started_at: Timestamp::now(),
            state: PaneState::Running,
            exit: None,
            cols: size.cols,
            rows: size.rows,
            meta: req.meta,
            title: String::new(),
            last_output_at: None,
            attach_clients: 0,
            subscribers: 0,
        };
        let cell = Arc::new(PaneCell {
            id,
            num,
            pid: started.pid,
            master,
            input: InputQueue::default(),
            inner: Mutex::new(Inner {
                record: record.clone(),
                model: Model::new(size),
                subscribers: Vec::new(),
                reaped: false,
            }),
            output_ended: watch::Sender::new(false),
        });
        table.panes.push(cell.clone());
        drop(table);
        tracing::info!(pane = %record.pane, num, pid = record.pid, argv = ?record.argv, "spawned");
        tokio::spawn(read_pty(cell.clone()));
        tokio::spawn(write_input(cell));
        self.reap.notify_one();
        Ok((record, false))
    }

    /// `list`: every pane, running or exited.
    pub fn list(&self) -> Listing {
        Listing {
            instance: self.instance.clone(),
            panes: self
                .panes()
                .iter()
                .map(|cell| cell.inner().record())
                .collect(),
        }
    }

    /// `subscribe`: the reply, the repaint and its frame, then live output,
    /// all queued under the pane's lock so no byte comes between them.
    pub fn subscribe(&self, conn: &Arc<Conn>, id: u64, pane: &PaneId) -> Result<(), Failure> {
        let cell = self.find(pane).ok_or_else(|| not_found(pane))?;
        let mut inner = cell.inner();
        // The model holds an open synchronized update apart from its screen,
        // and the new subscriber never got its start: the update ends here,
        // so the repaint shows it and the bytes after it pass as they are.
        if inner.model.sync_deadline().is_some()
            && let Err(panic) = inner.model.end_sync()
        {
            cell.model_reset(&mut inner, &panic);
        }
        let repaint = match inner.model.repaint() {
            Ok(repaint) => repaint,
            Err(panic) => {
                cell.model_reset(&mut inner, &panic);
                inner.model.repaint().map_err(|panic| {
                    (
                        ErrorCode::Internal,
                        format!("the repaint failed: {}", panic.message),
                    )
                })?
            }
        };
        let stream = self.next_stream.fetch_add(1, Ordering::Relaxed);
        let Size { cols, rows } = inner.model.size();
        let mut frames = vec![
            message_frame(&HolderMessage::Subscribed {
                id,
                stream,
                cols,
                rows,
            }),
            message_frame(&HolderMessage::Repaint { stream, cols, rows }),
            frame_bytes(&Frame::Output {
                stream,
                bytes: repaint,
            }),
        ];
        // A read's cut sequence that the model holds back went out to the
        // older subscribers: this one gets it too, so the rest of it makes
        // sense when it comes.
        let held = inner.model.held().to_vec();
        if !held.is_empty() {
            frames.push(frame_bytes(&Frame::Output {
                stream,
                bytes: held,
            }));
        }
        if let Some(exit) = inner.record.exit {
            frames.push(message_frame(&HolderMessage::Exited {
                id: None,
                pane: cell.id.clone(),
                exit,
            }));
        }
        // Every frame encodes before the first goes out, so a failure is the
        // only reply.
        let Some(frames) = frames.into_iter().collect::<Option<Vec<_>>>() else {
            return Err((ErrorCode::Internal, "the repaint would not encode".into()));
        };
        if !conn.send_repaint(frames) {
            return Ok(());
        }
        lock(&conn.streams).push((stream, cell.clone()));
        inner.subscribers.push(Subscriber {
            stream,
            conn: conn.clone(),
        });
        Ok(())
    }

    /// `resize`: sets the PTY's size and the model's. Leases come with H04;
    /// the slice takes every resize as it comes.
    pub fn resize(&self, pane: &PaneId, size: Size) -> Result<Size, Failure> {
        check_size(size)?;
        let cell = self.find(pane).ok_or_else(|| not_found(pane))?;
        let mut inner = cell.inner();
        if inner.record.state == PaneState::Exited {
            return Err((ErrorCode::Exited, format!("pane {pane} has exited")));
        }
        pty::set_size(cell.master.get_ref(), size).map_err(|err| internal(&err))?;
        if let Err(panic) = inner.model.resize(size) {
            cell.model_reset(&mut inner, &panic);
        }
        inner.record.cols = size.cols;
        inner.record.rows = size.rows;
        Ok(size)
    }

    /// A kind 2 frame from `conn`: raw bytes for the pane `num`, with no
    /// reply. Input for a pane that is gone or has exited is dropped. Input
    /// is never waited for, as that would hold up every other request on
    /// the connection: past [`INPUT_CAP`] (see `crate::input`), `conn` is
    /// cut.
    pub fn input(&self, conn: &Conn, num: u32, bytes: Vec<u8>) {
        let Some(cell) = self.by_num(num) else {
            tracing::debug!(num, "raw input for no pane");
            return;
        };
        if !cell.inner().running() {
            tracing::debug!(pane = %cell.id, "raw input for an exited pane");
            return;
        }
        match cell.input.push(&conn.sender, bytes) {
            Ok(()) => {}
            Err(input::Refused::Full(unread)) => {
                if conn.cut() {
                    tracing::warn!(
                        pane = %cell.id,
                        unread,
                        "the pane's app left {INPUT_CAP} bytes of a client's raw input unread, so the client was cut when it sent more"
                    );
                }
                cell.input.drop_cut();
            }
            Err(input::Refused::Closed) => {
                tracing::debug!(pane = %cell.id, "raw input for a pane whose PTY is closed");
            }
            Err(input::Refused::Broken) => {
                if conn.cut() {
                    tracing::warn!(
                        pane = %cell.id,
                        "raw input for a pane whose PTY refused input, so the client was cut"
                    );
                }
            }
        }
    }

    /// A connection ended: its subscriptions end with it.
    /// A cut connection's unwritten input goes too, as the client knows
    /// it has a gap. A connection that closed or broke off keeps its input.
    pub fn drop_conn(&self, conn: &Arc<Conn>) {
        for (stream, cell) in conn.take_streams() {
            let mut inner = cell.inner();
            inner.subscribers.retain(|sub| sub.stream != stream);
        }
        if conn.sender.is_cut() {
            for cell in self.panes() {
                cell.input.drop_cut();
            }
        }
    }

    /// Collects every child that has ended, without waiting. Only the
    /// holder's own panes are waited for, one pid at a time.
    fn reap_once(&self) {
        for cell in self.panes() {
            if cell.inner().reaped {
                continue;
            }
            let exit = match rustix::process::waitpid(Some(cell.pid), WaitOptions::NOHANG) {
                Ok(None) => continue,
                Ok(Some((_, status))) => Exit {
                    code: status.exit_status(),
                    signal: status.terminating_signal(),
                    at: Timestamp::now(),
                },
                Err(rustix::io::Errno::INTR) => continue,
                Err(err) => {
                    // Not this holder's child any more: nothing will report
                    // how it ended.
                    tracing::warn!(pane = %cell.id, %err, "waitpid failed; the exit is unknown");
                    Exit {
                        code: None,
                        signal: None,
                        at: Timestamp::now(),
                    }
                }
            };
            cell.inner().reaped = true;
            tracing::info!(pane = %cell.id, code = ?exit.code, signal = ?exit.signal, "the child exited");
            tokio::spawn(deliver_exit(cell, exit));
        }
    }

    /// Reaps on every `SIGCHLD` and after every spawn, until the holder
    /// stops.
    pub async fn reaper(self: Arc<Self>, mut sigchld: tokio::signal::unix::Signal) {
        loop {
            tokio::select! {
                _ = sigchld.recv() => {}
                () = self.reap.notified() => {}
            }
            self.reap_once();
        }
    }

    /// Hangs up every running child's group, as the holder stops.
    pub fn hang_up_all(&self) {
        for cell in self.panes() {
            if !cell.inner().reaped {
                cell.signal(Signal::HUP);
            }
        }
    }
}

/// Reads a pane's PTY until every holder of the slave has closed it. The
/// holder always reads, whether or not anyone watches, so a child never
/// blocks on a full PTY.
async fn read_pty(cell: Arc<PaneCell>) {
    let mut buf = vec![0; READ_CHUNK];
    loop {
        let ready = match cell.sync_deadline() {
            Some(deadline) => tokio::select! {
                ready = cell.master.readable() => Some(ready),
                () = tokio::time::sleep_until(deadline) => None,
            },
            None => Some(cell.master.readable().await),
        };
        let mut guard = match ready {
            None => {
                cell.end_sync();
                continue;
            }
            Some(Ok(guard)) => guard,
            Some(Err(err)) => {
                tracing::warn!(pane = %cell.id, %err, "the PTY cannot be polled");
                break;
            }
        };
        match guard.try_io(|fd| rustix::io::read(fd.get_ref(), &mut buf).map_err(io::Error::from)) {
            Ok(Ok(0)) => break,
            Ok(Ok(len)) => {
                cell.output(&buf[..len]);
                // A flooding app keeps the PTY readable, so this loop never
                // waits. Yield on budget, or the clients' writer tasks woken
                // on this worker never run, and their queues fill.
                tokio::task::consume_budget().await;
            }
            Ok(Err(err)) if err.kind() == io::ErrorKind::Interrupted => {}
            // EIO: the child and everything else that had the slave open
            // closed it.
            Ok(Err(_)) => break,
            Err(_would_block) => {}
        }
    }
    cell.output_ended.send_replace(true);
}

/// Writes raw input to a pane's PTY, in order, as fast as it takes it.
/// It ends once the PTY's output ends: with the slave closed, the master
/// takes a few KiB and then is never writable again.
/// When it ends, the input still queued is dropped.
async fn write_input(cell: Arc<PaneCell>) {
    write_all_input(&cell).await;
    cell.input.close();
}

async fn write_all_input(cell: &PaneCell) {
    let mut ended = cell.output_ended.subscribe();
    loop {
        let mut chunk = tokio::select! {
            chunk = cell.input.next() => chunk,
            _ = ended.wait_for(|ended| *ended) => return,
        };
        while !chunk.rest().is_empty() {
            let writable = tokio::select! {
                writable = cell.master.writable() => writable,
                _ = ended.wait_for(|ended| *ended) => return,
            };
            let Ok(mut guard) = writable.inspect_err(|err| {
                tracing::warn!(pane = %cell.id, %err, "the PTY cannot be polled for raw input; the pane takes no more");
                cell.input.break_off();
            }) else {
                return;
            };
            let rest = chunk.rest();
            match guard.try_io(|fd| rustix::io::write(fd.get_ref(), rest).map_err(io::Error::from))
            {
                Ok(Ok(len)) => chunk.wrote(len),
                Ok(Err(err)) if err.kind() == io::ErrorKind::Interrupted => {}
                Ok(Err(err)) => {
                    // None may be written after a gap: every client that
                    // sends more is cut.
                    tracing::warn!(pane = %cell.id, %err, "the PTY refused raw input; the pane takes no more");
                    cell.input.break_off();
                    return;
                }
                Err(_would_block) => {}
            }
        }
    }
}

/// Records a reaped child's exit and tells its subscribers, once its last
/// output is in: when the PTY read ends, or after [`EXIT_DRAIN`] when
/// something else still holds the PTY open. The PTY is still read after.
async fn deliver_exit(cell: Arc<PaneCell>, exit: Exit) {
    let mut ended = cell.output_ended.subscribe();
    let _ = tokio::time::timeout(EXIT_DRAIN, ended.wait_for(|ended| *ended)).await;
    let mut inner = cell.inner();
    inner.record.state = PaneState::Exited;
    inner.record.exit = Some(exit);
    let id = cell.id.clone();
    inner.fan_out(|_| {
        message_frame(&HolderMessage::Exited {
            id: None,
            pane: id.clone(),
            exit,
        })
    });
}

/// Puts a PTY master in the runtime's poller.
#[allow(unsafe_code)]
fn register(master: OwnedFd) -> io::Result<AsyncFd<OwnedFd>> {
    // SAFETY: `AsyncFd` needs the fd to stay open and unchanged while it is
    // registered. It owns the `OwnedFd`, and nothing else closes or
    // replaces that fd: the pane only reads and writes through it.
    unsafe { AsyncFd::register(master) }.map_err(io::Error::from)
}

/// Kills a child the holder cannot keep, and collects it. Killed, it ends
/// at once, so the wait is short.
fn kill_and_reap(pid: Pid) {
    let _ = rustix::process::kill_process_group(pid, Signal::KILL);
    let _ = rustix::process::waitpid(Some(pid), WaitOptions::empty());
}

fn check_size(size: Size) -> Result<(), Failure> {
    if size.cols < MIN_COLS || size.rows < MIN_ROWS {
        return Err(bad_request(format!(
            "a size of {} by {} is under the {MIN_COLS} by {MIN_ROWS} minimum",
            size.cols, size.rows
        )));
    }
    if size.cols > MAX_COLS || size.rows > MAX_ROWS {
        return Err(bad_request(format!(
            "a size of {} by {} is over the {MAX_COLS} by {MAX_ROWS} maximum",
            size.cols, size.rows
        )));
    }
    Ok(())
}

fn bad_request(message: String) -> Failure {
    (ErrorCode::BadRequest, message)
}

fn not_found(pane: &PaneId) -> Failure {
    (ErrorCode::NotFound, format!("no pane {pane}"))
}

fn internal(err: &io::Error) -> Failure {
    (ErrorCode::Internal, err.to_string())
}

/// A failed start: the request's mistake (a bad argv or cwd) or the
/// holder's.
fn spawn_failure(req: &Spawn, err: &io::Error) -> Failure {
    let message = format!("spawn {:?} in {:?}: {err}", req.argv, req.cwd);
    match err.kind() {
        io::ErrorKind::InvalidInput | io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => {
            (ErrorCode::BadRequest, message)
        }
        _ => (ErrorCode::Internal, message),
    }
}

/// This boot's id, so a client can tell a reboot from a holder restart:
/// Linux's `boot_id`, or macOS's boot session id. Empty when the system
/// reports neither. The daemon reads the same two, the same way
/// (`workers::boot_id`); the two move to one place once the daemon runs on
/// the holder.
fn boot_id() -> String {
    if let Ok(id) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        return id.trim().to_owned();
    }
    // By its full path: a `sysctl` earlier on PATH may not know the name.
    std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.bootsessionuuid"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|id| id.trim().to_owned())
        .unwrap_or_default()
}
