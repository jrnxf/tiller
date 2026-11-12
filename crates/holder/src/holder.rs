//! The holder's state: its panes and what each request does to them
//! (`terminal-holder.md` 2.4 to 2.7).
//!
//! Each pane has three tasks: one reads its PTY into the model and out to
//! its subscribers, one writes raw input to it, and one delivers its exit.
//! One reaper serves every pane. A pane's lock covers its record, model and
//! subscribers, so a new subscriber's repaint and the live bytes after it
//! are queued in order, and nothing waits for a client under it.
//!
//! Locks are taken in one order: a pane's, then the table's, then the
//! watchers', then a connection's list of streams. A pane's events go out
//! under its lock, so they reach every watcher in the order they happened.

use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use alacritty_terminal::term::TermMode;
use jiff::Timestamp;
use rustix::process::{Pid, Signal, WaitOptions};
use tiller_panes::frame::Frame;
use tiller_panes::handshake::Welcome;
use tiller_panes::keys::Key;
use tiller_panes::message::{ErrorCode, Format, HolderMessage, MAX_RECENT, Read, Screen};
use tiller_panes::{Exit, Listing, Meta, Pane, PaneId, PaneState, Size, Spawn};
use tokio::io::unix::AsyncFd;
use tokio::sync::{Notify, watch};

use crate::input::{self, INPUT_CAP, InputQueue};
use crate::model::{MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS, Model, ModelPanic, Told};
use crate::outbox::{Outbox, Refused};
use crate::{keys, pty};

/// How long an exit waits for the child's last output before `exited` is
/// sent, when something else still holds the PTY open.
pub const EXIT_DRAIN: Duration = Duration::from_millis(250);

/// The most a PTY read takes at once.
const READ_CHUNK: usize = 64 * 1024;

/// How long a pane is silent after output before its `quiet` event.
pub const QUIET_AFTER: Duration = Duration::from_millis(500);

/// How long `close` waits for the reaper after `kill`. A killed child ends
/// at once unless the kernel holds it, so a longer wait is an error.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// The start and end of a bracketed paste (mode 2004).
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// An error reply's code and message.
pub(crate) type Failure = (ErrorCode, String);

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

/// The connections that asked for events (`watch`).
#[derive(Debug, Default)]
struct Watchers(Mutex<Vec<Arc<Conn>>>);

impl Watchers {
    /// Sends `event` to every watcher, and drops each whose connection is
    /// cut or gone.
    fn emit(&self, event: &HolderMessage) {
        let Some(frame) = message_frame(event) else {
            return;
        };
        lock(&self.0).retain(|conn| conn.send(frame.clone()));
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
    /// The child's exit, once it is in the record.
    exit: watch::Sender<Option<Exit>>,
    /// True once the pane is forgotten: its PTY read stops.
    forgotten: watch::Sender<bool>,
    watchers: Arc<Watchers>,
}

impl PaneCell {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// An event about this pane, to every watcher.
    fn emit(&self, event: &HolderMessage) {
        self.watchers.emit(event);
    }

    /// Sends the events for what the app told the model: titles, bells and
    /// notifications. A title that did not change is no event.
    fn told(&self, inner: &mut Inner) {
        for told in inner.model.take_told() {
            let pane = self.id.clone();
            match told {
                Told::Title(title) if title == inner.record.title => {}
                Told::Title(title) => {
                    inner.record.title.clone_from(&title);
                    self.emit(&HolderMessage::Title { pane, title });
                }
                Told::Bell => self.emit(&HolderMessage::Bell { pane }),
                Told::Notify { title, body } => {
                    self.emit(&HolderMessage::Notify { pane, title, body });
                }
            }
        }
    }

    /// A model panic: the model is fresh already. The app in front redraws
    /// on `SIGWINCH`, every subscriber of the pane is cut, and watchers get
    /// `model_reset` (section 3.5).
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
        self.emit(&HolderMessage::ModelReset {
            pane: self.id.clone(),
        });
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
        let fed = inner.model.feed(bytes);
        self.told(&mut inner);
        if let Err(panic) = fed {
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
        let ended = inner.model.end_sync();
        self.told(&mut inner);
        if let Err(panic) = ended {
            self.model_reset(&mut inner, &panic);
        }
    }

    /// No output for [`QUIET_AFTER`] after output.
    fn quiet(&self) {
        self.emit(&HolderMessage::Quiet {
            pane: self.id.clone(),
            at: Timestamp::now(),
        });
    }

    /// Sends `signal` to the child's group while the child runs. Under the
    /// pane's lock, the reaper cannot free the pid in between.
    fn signal_running(&self, signal: Signal) -> bool {
        let inner = self.inner();
        let running = inner.running();
        if running {
            self.signal(signal);
        }
        running
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
    watchers: Arc<Watchers>,
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
            watchers: Arc::default(),
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

    fn cell(&self, pane: &PaneId) -> Result<Arc<PaneCell>, Failure> {
        self.find(pane).ok_or_else(|| not_found(pane))
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
            exit: watch::Sender::new(None),
            forgotten: watch::Sender::new(false),
            watchers: self.watchers.clone(),
        });
        table.panes.push(cell.clone());
        drop(table);
        tracing::info!(pane = %record.pane, num, pid = record.pid, argv = ?record.argv, "spawned");
        self.watchers.emit(&HolderMessage::Spawned {
            pane: record.pane.clone(),
        });
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

    /// `get`: one pane.
    pub fn get(&self, pane: &PaneId) -> Result<Pane, Failure> {
        Ok(self.cell(pane)?.inner().record())
    }

    /// `screen`: a read of the pane's model, running or exited. A model
    /// that panics while it is read is reset, and the read fails.
    pub fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen, Failure> {
        if !read.in_range() {
            return Err(bad_request(format!(
                "recent takes 1 to {MAX_RECENT} rows, not {read:?}"
            )));
        }
        let cell = self.cell(pane)?;
        let mut inner = cell.inner();
        inner.model.screen(read, format).map_err(|panic| {
            cell.model_reset(&mut inner, &panic);
            (
                ErrorCode::Internal,
                format!("the screen read failed: {}", panic.message),
            )
        })
    }

    /// `input`: text typed as it is.
    pub fn input_text(&self, conn: &Conn, pane: &PaneId, text: String) -> Result<(), Failure> {
        self.type_in(conn, pane, |_| text.into_bytes())
    }

    /// `paste`: text inside bracketed paste when the app turned mode 2004
    /// on, else as `input`. An end marker inside the text is dropped, so
    /// the text cannot end the paste early.
    pub fn paste(&self, conn: &Conn, pane: &PaneId, text: String) -> Result<(), Failure> {
        self.type_in(conn, pane, |mode| {
            if !mode.contains(TermMode::BRACKETED_PASTE) {
                return text.into_bytes();
            }
            let mut inside = text;
            while inside.contains(PASTE_END) {
                inside = inside.replace(PASTE_END, "");
            }
            format!("{PASTE_START}{inside}{PASTE_END}").into_bytes()
        })
    }

    /// `keys`: named keys, encoded for the app's keyboard mode.
    pub fn keys(&self, conn: &Conn, pane: &PaneId, keys: &[Key]) -> Result<(), Failure> {
        self.type_in(conn, pane, |mode| keys::encode(keys, mode))
    }

    /// Queues bytes made for the pane's modes as `conn`'s input, behind
    /// its raw input, and answers once they are queued. Past the input cap
    /// the request is refused with `busy` and nothing is queued: the client
    /// learns of it, so it is not cut as a raw sender is.
    fn type_in(
        &self,
        conn: &Conn,
        pane: &PaneId,
        bytes: impl FnOnce(TermMode) -> Vec<u8>,
    ) -> Result<(), Failure> {
        let cell = self.cell(pane)?;
        let bytes = {
            let inner = cell.inner();
            if !inner.running() {
                return Err(exited(pane));
            }
            bytes(inner.model.mode())
        };
        match cell.input.push(&conn.sender, bytes) {
            Ok(()) => Ok(()),
            Err(input::Refused::Full(unread)) => Err((
                ErrorCode::Busy,
                format!("the pane's app left {unread} bytes of this client's input unread"),
            )),
            Err(input::Refused::Closed) => Err(exited(pane)),
            Err(input::Refused::Broken) => Err((
                ErrorCode::Internal,
                format!("pane {pane}'s PTY refused input, and takes no more"),
            )),
        }
    }

    /// `subscribe`: the reply, the repaint and its frame, then live output,
    /// all queued under the pane's lock so no byte comes between them.
    pub fn subscribe(&self, conn: &Arc<Conn>, id: u64, pane: &PaneId) -> Result<(), Failure> {
        let cell = self.cell(pane)?;
        let mut inner = cell.inner();
        // The model holds an open synchronized update apart from its screen,
        // and the new subscriber never got its start: the update ends here,
        // so the repaint shows it and the bytes after it pass as they are.
        if inner.model.sync_deadline().is_some() {
            let ended = inner.model.end_sync();
            cell.told(&mut inner);
            if let Err(panic) = ended {
                cell.model_reset(&mut inner, &panic);
            }
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

    /// `unsubscribe`: ends one of this connection's streams. The reply is
    /// queued under the pane's lock, so no output for the stream follows it.
    pub fn unsubscribe(&self, conn: &Conn, id: u64, stream: u32) -> Result<(), Failure> {
        let cell = lock(&conn.streams)
            .iter()
            .find(|(own, _)| *own == stream)
            .map(|(_, cell)| cell.clone())
            .ok_or_else(|| {
                (
                    ErrorCode::NotFound,
                    format!("no stream {stream} on this connection"),
                )
            })?;
        let mut inner = cell.inner();
        inner.subscribers.retain(|sub| sub.stream != stream);
        conn.forget(stream);
        conn.send_message(&HolderMessage::Ok {
            id,
            cols: None,
            rows: None,
            clients: None,
        });
        Ok(())
    }

    /// `resize`: sets the PTY's size and the model's. Leases come with H04;
    /// until then every resize is taken as it comes.
    pub fn resize(&self, pane: &PaneId, size: Size) -> Result<Size, Failure> {
        check_size(size)?;
        let cell = self.cell(pane)?;
        let mut inner = cell.inner();
        if inner.record.state == PaneState::Exited {
            return Err(exited(pane));
        }
        pty::set_size(cell.master.get_ref(), size).map_err(|err| internal(&err))?;
        let resized = inner.model.resize(size);
        cell.told(&mut inner);
        if let Err(panic) = resized {
            cell.model_reset(&mut inner, &panic);
        }
        let changed = (inner.record.cols, inner.record.rows) != (size.cols, size.rows);
        inner.record.cols = size.cols;
        inner.record.rows = size.rows;
        if changed {
            cell.emit(&HolderMessage::Resized {
                pane: pane.clone(),
                cols: size.cols,
                rows: size.rows,
            });
        }
        Ok(size)
    }

    /// `release`: drops a lease and answers with the size after it. Leases
    /// come with H04, so no connection holds one: the size stays as it is,
    /// as it does when no lease is left (section 2.6).
    pub fn release(&self, pane: &PaneId) -> Result<Size, Failure> {
        let inner = self.cell(pane)?;
        let inner = inner.inner();
        Ok(Size {
            cols: inner.record.cols,
            rows: inner.record.rows,
        })
    }

    /// `meta`: merges `set` into the pane's metadata, then drops `remove`.
    /// An exited pane keeps its metadata, so it takes changes too.
    pub fn meta(&self, pane: &PaneId, set: Meta, remove: &[String]) -> Result<Pane, Failure> {
        let cell = self.cell(pane)?;
        let mut inner = cell.inner();
        inner.record.meta.extend(set);
        for key in remove {
            inner.record.meta.remove(key);
        }
        cell.emit(&HolderMessage::Meta { pane: pane.clone() });
        Ok(inner.record())
    }

    /// `signal`: a signal to the child's process group, while it runs.
    pub fn signal(&self, pane: &PaneId, signal: tiller_panes::Signal) -> Result<(), Failure> {
        let signal = match signal {
            tiller_panes::Signal::Int => Signal::INT,
            tiller_panes::Signal::Term => Signal::TERM,
            tiller_panes::Signal::Hup => Signal::HUP,
            tiller_panes::Signal::Kill => Signal::KILL,
        };
        if self.cell(pane)?.signal_running(signal) {
            Ok(())
        } else {
            Err(exited(pane))
        }
    }

    /// `close`: `hup` to the child's group, then `kill` after `grace`, and
    /// the exit once the child is reaped and its last output is in. An
    /// exited pane answers with its exit at once. The caller runs it as a
    /// task of its own, so the connection's other requests go on.
    pub async fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit, Failure> {
        let cell = self.cell(pane)?;
        let mut exit = cell.exit.subscribe();
        cell.signal_running(Signal::HUP);
        let has_exit = |exit: &Option<Exit>| exit.is_some();
        if let Ok(Ok(exit)) = tokio::time::timeout(grace, exit.wait_for(has_exit)).await
            && let Some(exit) = *exit
        {
            return Ok(exit);
        }
        if cell.signal_running(Signal::KILL) {
            tracing::info!(pane = %pane, grace_ms = grace.as_millis(), "the child outlived its close grace and was killed");
        }
        match tokio::time::timeout(KILL_WAIT, exit.wait_for(has_exit)).await {
            Ok(Ok(exit)) => (*exit).ok_or_else(|| close_failure(pane)),
            _ => Err(close_failure(pane)),
        }
    }

    /// `forget`: drops an exited pane, its screen and its subscriptions. A
    /// pane whose exit is not in yet is refused.
    pub fn forget(&self, pane: &PaneId) -> Result<(), Failure> {
        let cell = self.cell(pane)?;
        let mut inner = cell.inner();
        if inner.record.state != PaneState::Exited {
            return Err(bad_request(format!("{pane} is running")));
        }
        {
            let mut table = lock(&self.table);
            let before = table.panes.len();
            table.panes.retain(|other| !Arc::ptr_eq(other, &cell));
            if table.panes.len() == before {
                return Err(not_found(pane));
            }
        }
        // Each stream got its `exited` already, and ends here.
        for sub in inner.subscribers.drain(..) {
            sub.conn.forget(sub.stream);
        }
        cell.forgotten.send_replace(true);
        tracing::info!(pane = %pane, "forgotten");
        cell.emit(&HolderMessage::Forgotten {
            pane: pane.clone(),
            reason: "forget".to_owned(),
        });
        Ok(())
    }

    /// `watch`: `watching`, then every event, on `conn`. The reply is
    /// queued under the watchers' lock, so no event comes before it.
    pub fn watch(&self, conn: &Arc<Conn>, id: u64) {
        let mut watchers = lock(&self.watchers.0);
        if !conn.send_message(&HolderMessage::Watching { id }) {
            return;
        }
        if !watchers.iter().any(|other| Arc::ptr_eq(other, conn)) {
            watchers.push(conn.clone());
        }
    }

    /// `switch`: tells every attach client to show a running pane, and
    /// answers with how many were told. No attach client exists before
    /// `tiller attach` (H07), so none is.
    pub fn switch(&self, pane: &PaneId) -> Result<usize, Failure> {
        if self.cell(pane)?.inner().running() {
            Ok(0)
        } else {
            Err(exited(pane))
        }
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
        lock(&self.watchers.0).retain(|other| !Arc::ptr_eq(other, conn));
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
    /// Each wait holds the pane's lock, so a signal sent under it never
    /// reaches a pid that was freed and used again.
    fn reap_once(&self) {
        for cell in self.panes() {
            let mut inner = cell.inner();
            if inner.reaped {
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
            inner.reaped = true;
            drop(inner);
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

    /// Makes the next update of `pane`'s model panic.
    #[cfg(test)]
    pub(crate) fn fail_next(&self, pane: &PaneId) {
        if let Some(cell) = self.find(pane) {
            cell.inner().model.fail_next();
        }
    }

    /// Hangs up every running child's group, as the holder stops.
    pub fn hang_up_all(&self) {
        for cell in self.panes() {
            cell.signal_running(Signal::HUP);
        }
    }
}

/// Reads a pane's PTY until every holder of the slave has closed it. The
/// holder always reads, whether or not anyone watches, so a child never
/// blocks on a full PTY.
async fn read_pty(cell: Arc<PaneCell>) {
    let mut buf = vec![0; READ_CHUNK];
    let mut forgotten = cell.forgotten.subscribe();
    // When the pane's `quiet` is due, once it had output.
    let mut quiet_at = None;
    loop {
        let sync_at = cell.sync_deadline();
        let ready = tokio::select! {
            ready = cell.master.readable() => ready,
            () = until(sync_at) => {
                cell.end_sync();
                continue;
            }
            () = until(quiet_at) => {
                quiet_at = None;
                cell.quiet();
                continue;
            }
            _ = forgotten.wait_for(|forgotten| *forgotten) => break,
        };
        let mut guard = match ready {
            Ok(guard) => guard,
            Err(err) => {
                tracing::warn!(pane = %cell.id, %err, "the PTY cannot be polled");
                break;
            }
        };
        match guard.try_io(|fd| rustix::io::read(fd.get_ref(), &mut buf).map_err(io::Error::from)) {
            Ok(Ok(0)) => break,
            Ok(Ok(len)) => {
                cell.output(&buf[..len]);
                quiet_at = Some(tokio::time::Instant::now() + QUIET_AFTER);
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

/// Sleeps until `deadline`, or for ever when there is none.
async fn until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
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
    let exited = HolderMessage::Exited {
        id: None,
        pane: cell.id.clone(),
        exit,
    };
    inner.fan_out(|_| message_frame(&exited));
    cell.emit(&exited);
    cell.exit.send_replace(Some(exit));
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

fn exited(pane: &PaneId) -> Failure {
    (ErrorCode::Exited, format!("pane {pane} has exited"))
}

fn close_failure(pane: &PaneId) -> Failure {
    (
        ErrorCode::Internal,
        format!("pane {pane}'s child was killed and not reaped within {KILL_WAIT:?}"),
    )
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
