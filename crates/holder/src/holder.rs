//! The holder's state: its panes and what each request does to them
//! (`terminal-holder.md` 2.4 to 2.8).
//!
//! Each pane has three tasks: one reads its PTY into the model and out to
//! its subscribers, one writes raw input to it, and one delivers its exit.
//! One reaper serves every pane. A pane's lock covers its record, model,
//! leases and subscribers, so a new subscriber's place in its connection's
//! queue and the live bytes after it are taken in order. Nothing waits for
//! a client under it, and the long work, a repaint or a `screen` read's
//! text, is formatted from a copy away from it.
//!
//! Locks are taken in one order: a pane's, then the table's, then the
//! watchers', then a connection's list of streams. A pane's events go out
//! under its lock, so they reach every watcher in the order they happened.

use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use alacritty_terminal::term::TermMode;
use jiff::{SignedDuration, Timestamp};
use rustix::process::{Pid, Signal, WaitOptions};
use tiller_panes::frame::Frame;
use tiller_panes::handshake::Welcome;
use tiller_panes::keys::Key;
use tiller_panes::message::{ErrorCode, Format, HolderMessage, MAX_RECENT, Read, Screen, SyncMode};
use tiller_panes::{Exit, Listing, Meta, Pane, PaneId, PaneState, Size, Spawn};
use tokio::io::unix::AsyncFd;
use tokio::sync::{Notify, watch};
use tokio::time::{Instant, Sleep};

use crate::input::{self, INPUT_CAP, InputQueue};
use crate::lease::{self, LEASES_PER_PANE, Leases};
use crate::model::{MAX_COLS, MAX_ROWS, MIN_COLS, MIN_ROWS, Model, ModelPanic, Told};
use crate::outbox::{Outbox, Refused, SUBSCRIBER_CAP};
use crate::query::{self, QueryFilter};
use crate::snapshot::Snapshot;
use crate::sync::{Hold, Marks};
use crate::{keys, pty, repaint, screen};

/// How long an exit waits for the child's last output before `exited` is
/// sent, when something else still holds the PTY open.
pub const EXIT_DRAIN: Duration = Duration::from_millis(250);

/// The most a PTY read takes at once.
const READ_CHUNK: usize = 64 * 1024;

/// How long a pane is silent after output before its `quiet` event.
pub const QUIET_AFTER: Duration = Duration::from_millis(500);

/// The `screen` reads and repaints that are formatted at once. The rest
/// wait their turn.
const SCREEN_READS: usize = 2;

/// How long `close` waits for the reaper after `kill`. A killed child ends
/// at once unless the kernel holds it, so a longer wait is an error.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How often the reaper drops exited panes past their age (section 2.8).
const PRUNE_EVERY: Duration = Duration::from_secs(60);

/// A pane's steady rate of `title`, `bell` and `notify` events: one per
/// [`EVENT_INTERVAL`], after a burst of [`EVENT_BURST`] (section 2.8).
/// Past it, a title waits for the pane's `quiet`, and a bell or a
/// notification is dropped.
pub const EVENT_INTERVAL: Duration = Duration::from_millis(50);
pub const EVENT_BURST: u32 = 20;

/// The start and end of a bracketed paste (mode 2004).
const PASTE_START: &str = "\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// The holder's limits (section 2.8). Tests hold smaller ones.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Panes, running and exited. One more `spawn` is `busy`.
    pub panes: usize,
    /// Exited panes kept. Past it the oldest is dropped.
    pub exited: usize,
    /// How long an exited pane is kept.
    pub exited_for: Duration,
    /// Subscribers of one pane. One more `subscribe` is `busy`.
    pub subscribers: usize,
    /// Connections at once. One more is closed.
    pub connections: usize,
    /// Requests of one connection answered later and not answered yet.
    /// One more is `busy`.
    pub in_flight: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            panes: 128,
            exited: 64,
            exited_for: Duration::from_secs(24 * 60 * 60),
            subscribers: 16,
            connections: 64,
            in_flight: 64,
        }
    }
}

/// An error reply's code and message.
pub(crate) type Failure = (ErrorCode, String);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The `ok` reply that carries nothing but its request's id.
pub(crate) fn ok(id: u64) -> HolderMessage {
    HolderMessage::Ok {
        id,
        cols: None,
        rows: None,
        clients: None,
    }
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

/// The number of the next connection, for its leases.
static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

/// One client connection, as the panes see it.
#[derive(Debug)]
pub struct Conn {
    /// This connection's own number: its leases are held under it.
    id: u64,
    pub outbox: Outbox,
    /// This connection, as the panes' input queues see it.
    sender: Arc<input::Sender>,
    /// This connection's subscriptions: each stream and its pane.
    streams: Mutex<Vec<(u32, Arc<PaneCell>)>>,
    /// Its requests answered from a task of their own, not answered yet.
    pub(crate) in_flight: AtomicUsize,
}

impl Default for Conn {
    fn default() -> Self {
        Self {
            id: NEXT_CONN.fetch_add(1, Ordering::Relaxed),
            outbox: Outbox::default(),
            sender: Arc::default(),
            streams: Mutex::default(),
            in_flight: AtomicUsize::new(0),
        }
    }
}

impl Conn {
    /// Queues a reply or an event. A full queue cuts the connection. False
    /// when the frame was not queued.
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

    /// Queues one of `stream`'s frames against the stream's own queue. A
    /// full one ends the stream with `lagged`, and the connection and its
    /// other streams go on. False when the frame was not queued: the
    /// stream is over.
    fn send_stream(&self, stream: u32, frame: Vec<u8>) -> bool {
        match self.outbox.push_stream(stream, frame) {
            Ok(()) => true,
            Err(Refused::Full) => {
                tracing::warn!(
                    stream,
                    "a subscriber fell behind its {SUBSCRIBER_CAP}-byte queue and was lagged"
                );
                self.lag(stream);
                false
            }
            Err(Refused::Gone) => false,
        }
    }

    /// Ends `stream` with `lagged`, in place of the output it has queued:
    /// it fell behind, or what it drew is stale. It may subscribe again for
    /// a repaint.
    fn lag(&self, stream: u32) {
        match message_frame(&HolderMessage::Lagged { stream }) {
            Some(lagged) => {
                self.outbox.lag(stream, lagged);
            }
            None => self.outbox.drop_stream(stream),
        }
        self.forget(stream);
    }

    /// Ends `stream`: it takes no more, and what it has queued goes out.
    fn end_stream(&self, stream: u32) {
        self.outbox.end_stream(stream);
        self.forget(stream);
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
        let mut watchers = lock(&self.0);
        if watchers.is_empty() {
            return;
        }
        let Some(frame) = message_frame(event) else {
            return;
        };
        watchers.retain(|conn| conn.send(frame.clone()));
    }
}

/// A subscriber of one pane.
#[derive(Debug)]
struct Subscriber {
    stream: u32,
    conn: Arc<Conn>,
    sync: SyncMode,
    /// Its held synchronized frame, when it asked `sync: "buffer"`.
    hold: Hold,
}

impl Subscriber {
    /// Queues output for this subscriber. False when its stream is over.
    fn send(&self, bytes: Vec<u8>) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let stream = self.stream;
        frame_bytes(&Frame::Output { stream, bytes })
            .is_none_or(|frame| self.conn.send_stream(stream, frame))
    }
}

/// A pane's budget of events, as the generic cell rate algorithm keeps it:
/// each event moves `due` on by [`EVENT_INTERVAL`], and one is allowed
/// while `due` is less than a burst ahead.
#[derive(Debug, Default)]
struct Rate {
    due: Option<Instant>,
}

impl Rate {
    fn allow(&mut self, now: Instant) -> bool {
        let due = self.due.map_or(now, |due| due.max(now));
        if due > now + EVENT_INTERVAL * (EVENT_BURST - 1) {
            return false;
        }
        self.due = Some(due + EVENT_INTERVAL);
        true
    }
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
    /// Takes the queries the model answers out of the subscribers' bytes.
    filter: QueryFilter,
    /// The synchronized frames' markers in those bytes, while a subscriber
    /// asks `sync: "buffer"`.
    marks: Marks,
    leases: Leases,
    rate: Rate,
    /// The title changed past the event budget: its event is owed.
    title_owed: bool,
}

impl Inner {
    /// The pane's record as `list` shows it.
    fn record(&self) -> Pane {
        Pane {
            subscribers: u32::try_from(self.subscribers.len()).unwrap_or(u32::MAX),
            ..self.record.clone()
        }
    }

    fn size(&self) -> Size {
        Size {
            cols: self.record.cols,
            rows: self.record.rows,
        }
    }

    /// Whether the child still runs: not yet reaped. A reaped child's exit
    /// goes into the record a little later, once its last output is in.
    fn running(&self) -> bool {
        self.record.state == PaneState::Running && !self.reaped
    }

    /// Sends one read's bytes, less its queries, to every subscriber, each
    /// as it asked for synchronized frames, and drops each one whose stream
    /// is over.
    fn fan_out(&mut self, pass: &[u8], now: Instant) {
        let buffering = self
            .subscribers
            .iter()
            .any(|sub| sub.sync == SyncMode::Buffer);
        let marks = if buffering {
            self.marks.scan(pass)
        } else {
            self.marks.reset();
            Vec::new()
        };
        self.subscribers.retain_mut(|sub| {
            let out = match sub.sync {
                SyncMode::Buffer => sub.hold.take(pass, &marks, now),
                SyncMode::Pass => pass.to_vec(),
            };
            sub.send(out)
        });
    }

    /// Sends each subscriber's held frame, at the pane's end.
    fn flush_holds(&mut self) {
        self.subscribers.retain_mut(|sub| {
            let held = sub.hold.flush();
            sub.send(held)
        });
    }

    /// When the pane's next timer is due: the model's open synchronized
    /// update, or a subscriber's held frame.
    fn next_timer(&self) -> Option<Instant> {
        let sync = self.model.sync_deadline().map(Instant::from_std);
        self.subscribers
            .iter()
            .filter_map(|sub| sub.hold.until())
            .chain(sync)
            .min()
    }

    /// Ends every subscription of this pane with `lagged`: the model was
    /// reset, so what they drew from it is stale. Each may subscribe again
    /// for a repaint. The connections stay, with any other pane's streams.
    fn lag_all(&mut self) {
        for sub in self.subscribers.drain(..) {
            sub.conn.lag(sub.stream);
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
    /// The model's answers to the app's queries, as the input queue sees
    /// them.
    answerer: Arc<input::Sender>,
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

    /// The pane's lock, for a request: a pane forgotten since the request
    /// found it is not found. `forget` marks the pane under this lock.
    fn live(&self) -> Result<MutexGuard<'_, Inner>, Failure> {
        let inner = self.inner();
        if *self.forgotten.borrow() {
            return Err(not_found(&self.id));
        }
        Ok(inner)
    }

    /// An event about this pane, to every watcher. Called under the pane's
    /// lock, so no event follows its `forgotten`.
    fn emit(&self, event: &HolderMessage) {
        if !*self.forgotten.borrow() {
            self.watchers.emit(event);
        }
    }

    /// Sends the events for what the app told the model: titles, bells and
    /// notifications, within the pane's event budget. A title that did not
    /// change is no event, and one past the budget is owed.
    fn told(&self, inner: &mut Inner) {
        let now = Instant::now();
        for told in inner.model.take_told() {
            let pane = self.id.clone();
            match told {
                Told::Title(title) if title == inner.record.title => {}
                Told::Title(title) => {
                    inner.record.title.clone_from(&title);
                    inner.title_owed = !inner.rate.allow(now);
                    if !inner.title_owed {
                        self.emit(&HolderMessage::Title { pane, title });
                    }
                }
                Told::Bell if inner.rate.allow(now) => self.emit(&HolderMessage::Bell { pane }),
                Told::Notify { title, body } if inner.rate.allow(now) => {
                    self.emit(&HolderMessage::Notify { pane, title, body });
                }
                Told::Bell | Told::Notify { .. } => {
                    tracing::debug!(pane = %self.id, "an event past the pane's budget was dropped");
                }
            }
        }
    }

    /// Sends the title event owed, if one is.
    fn pay_title(&self, inner: &mut Inner) {
        if std::mem::take(&mut inner.title_owed) {
            self.emit(&HolderMessage::Title {
                pane: self.id.clone(),
                title: inner.record.title.clone(),
            });
        }
    }

    /// Runs `change` on the pane's model, sends what the app told it,
    /// answers the queries it read, and resets the pane when the model
    /// panicked.
    fn update<T>(
        &self,
        inner: &mut Inner,
        change: impl FnOnce(&mut Model) -> Result<T, ModelPanic>,
    ) -> Result<T, ModelPanic> {
        let changed = change(&mut inner.model);
        self.told(inner);
        self.answer(inner.model.take_answers());
        if let Err(panic) = &changed {
            self.model_reset(inner, panic);
        }
        changed
    }

    /// Writes answers to the app's queries on its PTY, behind the input
    /// queued before them. An app that leaves its input unread gets none.
    fn answer(&self, answers: Vec<u8>) {
        if answers.is_empty() {
            return;
        }
        if let Err(refused) = self.input.push(&self.answerer, answers) {
            tracing::debug!(pane = %self.id, ?refused, "the answers to the app's queries were dropped");
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

    /// One PTY read: into the model, with the queries in it answered, and
    /// out to every subscriber without them. Gives the pane's next timer.
    fn output(&self, bytes: &[u8]) -> Option<Instant> {
        let mut inner = self.inner();
        inner.record.last_output_at = Some(Timestamp::now());
        let filtered = inner.filter.filter(bytes);
        // The model answers each query as it reads it, and the holder
        // answers XTVERSION: its answer goes after the ones before it.
        let mut from = 0;
        for end in filtered.xtversions {
            if self
                .update(&mut inner, |model| model.feed(&bytes[from..end]))
                .is_err()
            {
                return inner.next_timer();
            }
            self.answer(query::xtversion());
            from = end;
        }
        if self
            .update(&mut inner, |model| model.feed(&bytes[from..]))
            .is_ok()
        {
            inner.fan_out(&filtered.pass, Instant::now());
        }
        inner.next_timer()
    }

    /// A timer of the pane's came due: the model's open synchronized update
    /// ends, and each subscriber's held frame whose time is up goes out.
    /// Gives the next timer.
    fn tick(&self) -> Option<Instant> {
        let mut inner = self.inner();
        let now = Instant::now();
        if inner
            .model
            .sync_deadline()
            .is_some_and(|at| Instant::from_std(at) <= now)
        {
            _ = self.update(&mut inner, Model::end_sync);
        }
        inner.subscribers.retain_mut(|sub| {
            if sub.hold.until().is_some_and(|at| at <= now) {
                let held = sub.hold.flush();
                sub.send(held)
            } else {
                true
            }
        });
        inner.next_timer()
    }

    /// Ends the synchronized update the model holds open, if there is one.
    fn end_open_sync(&self, inner: &mut Inner) {
        if inner.model.sync_deadline().is_some() {
            _ = self.update(inner, Model::end_sync);
        }
    }

    /// No output for [`QUIET_AFTER`] after output. An owed title goes
    /// first.
    fn quiet(&self) {
        let mut inner = self.inner();
        self.pay_title(&mut inner);
        self.emit(&HolderMessage::Quiet {
            pane: self.id.clone(),
            at: Timestamp::now(),
        });
    }

    /// Sends `signal` to the child's group while the child runs, and says
    /// whether it did. Under the pane's lock, the reaper cannot free the pid
    /// in between. A forgotten pane is not found.
    fn signal_running(&self, signal: Signal) -> Result<bool, Failure> {
        let inner = self.live()?;
        let running = inner.running();
        if running {
            self.signal(signal);
        }
        Ok(running)
    }

    /// Kills the child's group while the child runs, or while something
    /// still holds the PTY after the child was reaped: the rest of its
    /// group, which outlived it. Says whether it did. The group's id stays
    /// taken while any process of the group lives, so it is not another's.
    fn kill_what_is_left(&self) -> Result<bool, Failure> {
        let inner = self.live()?;
        let left = inner.running() || (inner.reaped && !*self.output_ended.borrow());
        if left {
            self.signal(Signal::KILL);
        }
        Ok(left)
    }

    /// Sets the PTY and the model to the top lease's size, and tells the
    /// watchers. With no lease left, or once the child has exited, the size
    /// stays.
    fn apply_top(&self, inner: &mut Inner) -> Result<(), Failure> {
        let Some(size) = inner.leases.top().map(|lease| lease.size) else {
            return Ok(());
        };
        if size == inner.size() || !inner.running() {
            return Ok(());
        }
        pty::set_size(self.master.get_ref(), size).map_err(|err| internal(&err))?;
        _ = self.update(inner, |model| model.resize(size));
        inner.record.cols = size.cols;
        inner.record.rows = size.rows;
        self.emit(&HolderMessage::Resized {
            pane: self.id.clone(),
            cols: size.cols,
            rows: size.rows,
        });
        Ok(())
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
    limits: Limits,
    table: Mutex<Table>,
    /// Held for a whole `spawn`, so two spawns with one key start one
    /// child, while the table's lock is free for everything else during
    /// the fork and exec. A spawn waits its turn here before it takes a
    /// blocking thread, so a burst of spawns holds one thread, not one
    /// each.
    spawning: Arc<tokio::sync::Mutex<()>>,
    next_stream: AtomicU32,
    /// Wakes the reaper after a spawn, so an exit that came before the pane
    /// was in the table is not missed.
    reap: Notify,
    watchers: Arc<Watchers>,
    /// The `screen` reads and repaints that may be formatted at once, each
    /// on a blocking thread.
    screen_reads: Arc<tokio::sync::Semaphore>,
    /// The connections open now.
    connections: AtomicUsize,
}

/// A connection's place under [`Limits::connections`], given back when it
/// drops.
#[derive(Debug)]
pub struct ConnSlot(Arc<Holder>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A subscription's first frames, to draw away from the pane's lock.
struct FirstFrames {
    id: u64,
    stream: u32,
    size: Size,
    snapshot: Snapshot,
    /// The bytes the model holds back, which the older subscribers got.
    tail: Vec<u8>,
    exit: Option<Exit>,
}

impl Holder {
    pub fn new() -> Arc<Self> {
        Self::with_limits(Limits::default())
    }

    pub fn with_limits(limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            instance: ulid::Ulid::generate().to_string(),
            boot_id: boot_id(),
            started_at: Timestamp::now(),
            limits,
            table: Mutex::new(Table::default()),
            spawning: Arc::default(),
            next_stream: AtomicU32::new(1),
            reap: Notify::new(),
            watchers: Arc::default(),
            screen_reads: Arc::new(tokio::sync::Semaphore::new(SCREEN_READS)),
            connections: AtomicUsize::new(0),
        })
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The handshake's answer, for a version both sides speak.
    pub fn welcome(&self, proto: u32) -> Welcome {
        Welcome {
            proto,
            holder: env!("CARGO_PKG_VERSION").to_owned(),
            instance: self.instance.clone(),
            boot_id: self.boot_id.clone(),
            started_at: self.started_at,
            features: vec!["kitty_keys".to_owned(), "sync_buffer".to_owned()],
        }
    }

    /// A place for one more connection, or `None` past
    /// [`Limits::connections`].
    pub fn take_conn_slot(self: &Arc<Self>) -> Option<ConnSlot> {
        self.connections
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |open| {
                (open < self.limits.connections).then_some(open + 1)
            })
            .ok()
            .map(|_| ConnSlot(self.clone()))
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

    /// `spawn`: a new pane, or the running pane that holds the key. One
    /// spawn runs at a time, and it forks and waits for the exec on a
    /// blocking thread, off the runtime's workers.
    pub async fn spawn(self: &Arc<Self>, req: Spawn) -> Result<(Pane, bool), Failure> {
        let turn = self.spawning.clone().lock_owned().await;
        let holder = self.clone();
        tokio::task::spawn_blocking(move || {
            let _turn = turn;
            holder.spawn_in_turn(req)
        })
        .await
        .map_err(|err| (ErrorCode::Internal, format!("the spawn failed: {err}")))?
    }

    fn spawn_in_turn(self: &Arc<Self>, req: Spawn) -> Result<(Pane, bool), Failure> {
        // A spawn for a key that runs gets that pane, as the request that
        // started it got it, whatever else the request asks for.
        let running = self.panes().iter().find_map(|cell| {
            let inner = cell.inner();
            (inner.record.key == req.key && inner.running()).then(|| inner.record())
        });
        if let Some(record) = running {
            return Ok((record, true));
        }
        let panes = lock(&self.table).panes.len();
        if panes >= self.limits.panes {
            return Err((
                ErrorCode::Busy,
                format!("the holder holds {panes} panes, its limit; forget an exited one"),
            ));
        }
        let size = req.size.unwrap_or(tiller_panes::message::DEFAULT_SIZE);
        check_size(size)?;
        if !Path::new(&req.cwd).is_absolute() {
            return Err(bad_request(format!(
                "cwd {:?} is not an absolute path",
                req.cwd
            )));
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
        let num = {
            let mut table = lock(&self.table);
            table.next_num += 1;
            table.next_num
        };
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
            answerer: Arc::default(),
            inner: Mutex::new(Inner {
                record: record.clone(),
                model: Model::new(size),
                subscribers: Vec::new(),
                reaped: false,
                filter: QueryFilter::new(),
                marks: Marks::default(),
                leases: Leases::default(),
                rate: Rate::default(),
                title_owed: false,
            }),
            output_ended: watch::Sender::new(false),
            exit: watch::Sender::new(None),
            forgotten: watch::Sender::new(false),
            watchers: self.watchers.clone(),
        });
        {
            // `spawned` goes out under the pane's lock, as its other events
            // do, so none of them comes before it.
            let _inner = cell.inner();
            lock(&self.table).panes.push(cell.clone());
            tracing::info!(pane = %record.pane, num, pid = record.pid, argv = ?record.argv, "spawned");
            cell.emit(&HolderMessage::Spawned {
                pane: record.pane.clone(),
            });
        }
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
        Ok(self.cell(pane)?.live()?.record())
    }

    /// `screen`, as a request runs it: a long read formats many rows, so
    /// it runs on a blocking thread, and at most [`SCREEN_READS`] run at
    /// once.
    pub async fn screen_read(
        self: &Arc<Self>,
        pane: PaneId,
        read: Read,
        format: Format,
    ) -> Result<Screen, Failure> {
        // A read out of range waits for no other read.
        check_read(read)?;
        let permit = self
            .screen_reads
            .clone()
            .acquire_owned()
            .await
            .map_err(|err| {
                (
                    ErrorCode::Internal,
                    format!("the screen read failed: {err}"),
                )
            })?;
        let holder = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            holder.screen(&pane, read, format)
        })
        .await
        .map_err(|err| {
            (
                ErrorCode::Internal,
                format!("the screen read failed: {err}"),
            )
        })?
    }

    /// `screen`: a read of the pane's model, running or exited. The rows
    /// it needs are copied under the pane's lock and formatted after it. A
    /// model that panics while it is copied is reset, and the read fails. A
    /// read inside an open synchronized update shows the last whole frame,
    /// as a terminal shows it, and leaves the update open.
    pub fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen, Failure> {
        check_read(read)?;
        let cell = self.cell(pane)?;
        let snapshot = {
            let mut inner = cell.live()?;
            let history = screen::history_for(read, usize::from(inner.record.rows));
            cell.update(&mut inner, |model| model.snapshot(history))
                .map_err(|panic| {
                    (
                        ErrorCode::Internal,
                        format!("the screen read failed: {}", panic.message),
                    )
                })?
        };
        Ok(screen::read(&snapshot, read, format))
    }

    /// `input`: text typed as it is.
    pub fn input_text(&self, conn: &Conn, pane: &PaneId, text: String) -> Result<(), Failure> {
        self.type_in(conn, pane, |_| text.into_bytes())
    }

    /// `paste`: text inside bracketed paste when the app turned mode 2004
    /// on. An end marker inside the text is dropped, so the text cannot end
    /// the paste early. Without mode 2004 each line ends with a carriage
    /// return, as xterm and Alacritty paste it: a line feed alone moves the
    /// cursor down in a raw mode app, and does not end the line.
    pub fn paste(&self, conn: &Conn, pane: &PaneId, text: String) -> Result<(), Failure> {
        self.type_in(conn, pane, |mode| {
            if !mode.contains(TermMode::BRACKETED_PASTE) {
                return paste_lines(&text).into_bytes();
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
            let mut inner = cell.live()?;
            if !inner.running() {
                return Err(exited(pane));
            }
            // A mode the app set inside an open synchronized update is set
            // for the keys and the paste it waits for.
            cell.end_open_sync(&mut inner);
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

    /// `subscribe`: the reply, the repaint and its frame, then live output.
    /// Under the pane's lock the screen is copied and the stream takes its
    /// place in the connection's queue, so no byte comes between the two.
    /// The repaint is drawn from the copy after the lock, and its frames
    /// go to that place. Live output queues behind it meanwhile.
    pub fn subscribe(
        self: &Arc<Self>,
        conn: &Arc<Conn>,
        id: u64,
        pane: &PaneId,
        sync: SyncMode,
    ) -> Result<(), Failure> {
        let cell = self.cell(pane)?;
        let mut inner = cell.live()?;
        if inner.subscribers.len() >= self.limits.subscribers {
            return Err((
                ErrorCode::Busy,
                format!(
                    "pane {pane} has {} subscribers, its limit",
                    inner.subscribers.len()
                ),
            ));
        }
        // The model holds an open synchronized update apart from its screen,
        // and the new subscriber never got its start: the update ends here,
        // so the repaint shows it and the bytes after it pass as they are.
        cell.end_open_sync(&mut inner);
        let snapshot = match cell.update(&mut inner, |model| model.snapshot(0)) {
            Ok(snapshot) => snapshot,
            // The fresh model is copied again, and a second panic is reset
            // and told as the first was.
            Err(_) => cell
                .update(&mut inner, |model| model.snapshot(0))
                .map_err(|panic| {
                    (
                        ErrorCode::Internal,
                        format!("the repaint failed: {}", panic.message),
                    )
                })?,
        };
        let stream = self.next_stream.fetch_add(1, Ordering::Relaxed);
        if conn.outbox.reserve(stream).is_err() {
            // The connection is closing or was cut: it gets nothing more.
            return Ok(());
        }
        lock(&conn.streams).push((stream, cell.clone()));
        inner.subscribers.push(Subscriber {
            stream,
            conn: conn.clone(),
            sync,
            hold: Hold::default(),
        });
        // A read's cut sequence that the model holds back went out to the
        // older subscribers, less what the query filter still holds: this
        // one gets it too, so the rest of it makes sense when it comes.
        let held = inner.model.held();
        let sent = held.len().saturating_sub(inner.filter.pending().len());
        let first = FirstFrames {
            id,
            stream,
            size: inner.model.size(),
            snapshot,
            tail: held[..sent].to_vec(),
            exit: inner.record.exit,
        };
        drop(inner);
        let holder = self.clone();
        let conn = conn.clone();
        tokio::spawn(async move {
            let frames = holder.draw(&cell.id, first).await;
            let filled = frames.and_then(|frames| {
                let len: usize = frames.iter().map(Vec::len).sum();
                conn.outbox.fill(stream, frames).map_err(|_| {
                    (
                        ErrorCode::Busy,
                        format!("the repaint is {len} bytes, more than this connection's queue holds with the repaints before it"),
                    )
                })
            });
            if let Err((code, message)) = filled {
                cell.inner().subscribers.retain(|sub| sub.stream != stream);
                conn.forget(stream);
                conn.outbox.drop_stream(stream);
                conn.send_message(&HolderMessage::Error {
                    id: Some(id),
                    code,
                    message,
                });
            }
        });
        Ok(())
    }

    /// A subscription's first frames: its reply, the repaint and its frame,
    /// what the model holds back, and the exit when the pane has exited.
    async fn draw(&self, pane: &PaneId, first: FirstFrames) -> Result<Vec<Vec<u8>>, Failure> {
        let failed = |err: &dyn std::fmt::Display| {
            (ErrorCode::Internal, format!("the repaint failed: {err}"))
        };
        let permit = self
            .screen_reads
            .clone()
            .acquire_owned()
            .await
            .map_err(|err| failed(&err))?;
        let snapshot = first.snapshot;
        let repaint = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            repaint::render(&snapshot)
        })
        .await
        .map_err(|err| failed(&err))?;
        let FirstFrames {
            id, stream, size, ..
        } = first;
        let Size { cols, rows } = size;
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
        if !first.tail.is_empty() {
            frames.push(frame_bytes(&Frame::Output {
                stream,
                bytes: first.tail,
            }));
        }
        if let Some(exit) = first.exit {
            frames.push(message_frame(&HolderMessage::Exited {
                id: None,
                pane: pane.clone(),
                exit,
            }));
        }
        frames
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| (ErrorCode::Internal, "the repaint would not encode".into()))
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
        conn.end_stream(stream);
        conn.send_message(&ok(id));
        Ok(())
    }

    /// `resize`: puts `conn`'s lease `lease` on top of the pane's leases
    /// with `size`, a new lease or a held one moved up and changed. The top
    /// lease sets the PTY's size and the model's (section 2.6). Answers
    /// with the size after it.
    pub fn resize(
        &self,
        conn: &Conn,
        pane: &PaneId,
        size: Size,
        lease: &str,
    ) -> Result<Size, Failure> {
        check_size(size)?;
        let cell = self.cell(pane)?;
        let mut inner = cell.live()?;
        if !inner.running() {
            return Err(exited(pane));
        }
        inner
            .leases
            .take(conn.id, lease, size)
            .map_err(|lease::Full| {
                (
                    ErrorCode::Busy,
                    format!("pane {pane} holds {LEASES_PER_PANE} leases, its limit"),
                )
            })?;
        cell.apply_top(&mut inner)?;
        Ok(inner.size())
    }

    /// `release`: drops `conn`'s lease `lease`, and the next lease sets the
    /// size. With no lease left the size stays as it is (section 2.6).
    /// Answers with the size after it. A lease the connection does not hold
    /// changes nothing.
    pub fn release(&self, conn: &Conn, pane: &PaneId, lease: &str) -> Result<Size, Failure> {
        let cell = self.cell(pane)?;
        let mut inner = cell.live()?;
        if inner.leases.release(conn.id, lease) {
            cell.apply_top(&mut inner)?;
        }
        Ok(inner.size())
    }

    /// `meta`: merges `set` into the pane's metadata, then drops `remove`.
    /// An exited pane keeps its metadata, so it takes changes too. Only a
    /// change is an event.
    pub fn meta(&self, pane: &PaneId, set: Meta, remove: &[String]) -> Result<Pane, Failure> {
        let cell = self.cell(pane)?;
        let mut inner = cell.live()?;
        let before = inner.record.meta.clone();
        inner.record.meta.extend(set);
        for key in remove {
            inner.record.meta.remove(key);
        }
        if inner.record.meta != before {
            cell.emit(&HolderMessage::Meta { pane: pane.clone() });
        }
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
        if self.cell(pane)?.signal_running(signal)? {
            Ok(())
        } else {
            Err(exited(pane))
        }
    }

    /// `close`: `hup` to the child's group, then `kill` after `grace`, and
    /// the exit once the child is reaped and its last output is in. A child
    /// that exits in time may leave the rest of its group holding the PTY:
    /// what is left of it at the end of `grace` is killed too. An exited
    /// pane answers with its exit at once. The caller runs it as a task of
    /// its own, so the connection's other requests go on.
    pub async fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit, Failure> {
        let cell = self.cell(pane)?;
        let mut exit = cell.exit.subscribe();
        // Only an exited pane can be forgotten, so a pane forgotten since
        // its exit came in answers with that exit.
        let known = |failure: Failure, exit: &watch::Receiver<Option<Exit>>| {
            (*exit.borrow()).ok_or(failure)
        };
        if let Err(failure) = cell.signal_running(Signal::HUP) {
            return known(failure, &exit);
        }
        let deadline = Instant::now() + grace;
        let has_exit = |exit: &Option<Exit>| exit.is_some();
        if let Ok(Ok(got)) = tokio::time::timeout_at(deadline, exit.wait_for(has_exit)).await
            && let Some(got) = *got
        {
            if !*cell.output_ended.borrow() {
                tokio::spawn(kill_at(cell.clone(), deadline));
            }
            return Ok(got);
        }
        match cell.kill_what_is_left() {
            Ok(true) => {
                tracing::info!(pane = %pane, grace_ms = grace.as_millis(), "the child outlived its close grace and was killed")
            }
            Ok(false) => {}
            Err(failure) => return known(failure, &exit),
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
        let mut inner = cell.live()?;
        if inner.record.state != PaneState::Exited {
            return Err(bad_request(format!("{pane} is running")));
        }
        if self.drop_pane(&cell, &mut inner, "forget") {
            Ok(())
        } else {
            Err(not_found(pane))
        }
    }

    /// Drops an exited pane from the table, under its lock: its streams
    /// end, and `forgotten` with `reason` is its last event. False when it
    /// was dropped already.
    fn drop_pane(&self, cell: &Arc<PaneCell>, inner: &mut Inner, reason: &str) -> bool {
        {
            let mut table = lock(&self.table);
            let before = table.panes.len();
            table.panes.retain(|other| !Arc::ptr_eq(other, cell));
            if table.panes.len() == before {
                return false;
            }
        }
        // Each stream got its `exited` already, and ends here.
        for sub in inner.subscribers.drain(..) {
            sub.conn.end_stream(sub.stream);
        }
        cell.forgotten.send_replace(true);
        tracing::info!(pane = %cell.id, reason, "forgotten");
        // The pane's last event: [`PaneCell::emit`] sends no more.
        self.watchers.emit(&HolderMessage::Forgotten {
            pane: cell.id.clone(),
            reason: reason.to_owned(),
        });
        true
    }

    /// Drops the exited panes past the limits (section 2.8): each that
    /// exited more than [`Limits::exited_for`] before `now`, then the
    /// oldest past [`Limits::exited`].
    pub fn prune(&self, now: Timestamp) {
        let mut exited: Vec<_> = self
            .panes()
            .into_iter()
            .filter_map(|cell| {
                let exit = cell.inner().record.exit;
                exit.map(|exit| (exit.at, cell))
            })
            .collect();
        exited.sort_by_key(|(at, _)| *at);
        let over = exited.len().saturating_sub(self.limits.exited);
        let kept_for =
            SignedDuration::try_from(self.limits.exited_for).unwrap_or(SignedDuration::MAX);
        for (n, (at, cell)) in exited.into_iter().enumerate() {
            if n < over || now.duration_since(at) > kept_for {
                let mut inner = cell.inner();
                self.drop_pane(&cell, &mut inner, "limit");
            }
        }
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
        if self.cell(pane)?.live()?.running() {
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

    /// A connection ended: its subscriptions and its leases end with it,
    /// and each pane it held the top lease of takes the next one's size.
    /// A cut connection's unwritten input goes too, as the client knows
    /// it has a gap. A connection that closed or broke off keeps its input.
    pub fn drop_conn(&self, conn: &Arc<Conn>) {
        lock(&self.watchers.0).retain(|other| !Arc::ptr_eq(other, conn));
        for (stream, cell) in conn.take_streams() {
            let mut inner = cell.inner();
            inner.subscribers.retain(|sub| sub.stream != stream);
        }
        for cell in self.panes() {
            let mut inner = cell.inner();
            if inner.leases.drop_conn(conn.id)
                && let Err((_, err)) = cell.apply_top(&mut inner)
            {
                tracing::warn!(pane = %cell.id, %err, "the next lease's size could not be set");
            }
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
    fn reap_once(self: &Arc<Self>) {
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
            tokio::spawn(deliver_exit(self.clone(), cell, exit));
        }
    }

    /// Reaps on every `SIGCHLD` and after every spawn, and drops the
    /// exited panes past their age every [`PRUNE_EVERY`], until the holder
    /// stops.
    pub async fn reaper(self: Arc<Self>, mut sigchld: tokio::signal::unix::Signal) {
        let mut prune = tokio::time::interval(PRUNE_EVERY);
        prune.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = sigchld.recv() => {}
                () = self.reap.notified() => {}
                _ = prune.tick() => {
                    self.prune(Timestamp::now());
                    continue;
                }
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
            // A pane forgotten in between has no child to hang up.
            _ = cell.signal_running(Signal::HUP);
        }
    }
}

/// Reads a pane's PTY until every holder of the slave has closed it. The
/// holder always reads, whether or not anyone watches, so a child never
/// blocks on a full PTY.
async fn read_pty(cell: Arc<PaneCell>) {
    let mut buf = vec![0; READ_CHUNK];
    let mut forgotten = cell.forgotten.subscribe();
    // The pane's next timer, from its last read or tick.
    let timer = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(timer);
    let mut timer_at = None;
    // When the pane's `quiet` is due once it had output, and its last read.
    let quiet = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(quiet);
    let mut quiet_at = None;
    let mut last_output = Instant::now();
    loop {
        // In this order: a forgotten pane reads no more, and a deadline that
        // passed is kept even while a flooding app keeps the PTY readable.
        let ready = tokio::select! {
            biased;
            _ = forgotten.wait_for(|forgotten| *forgotten) => break,
            () = &mut timer, if timer_at.is_some() => {
                timer_at = None;
                arm(timer.as_mut(), &mut timer_at, cell.tick());
                continue;
            }
            () = &mut quiet, if quiet_at.is_some() => {
                // Output since the timer was set moves it on.
                let due = last_output + QUIET_AFTER;
                if due > Instant::now() {
                    quiet_at = None;
                    arm(quiet.as_mut(), &mut quiet_at, Some(due));
                } else {
                    quiet_at = None;
                    cell.quiet();
                }
                continue;
            }
            ready = cell.master.readable() => ready,
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
                arm(timer.as_mut(), &mut timer_at, cell.output(&buf[..len]));
                last_output = Instant::now();
                if quiet_at.is_none() {
                    arm(
                        quiet.as_mut(),
                        &mut quiet_at,
                        Some(last_output + QUIET_AFTER),
                    );
                }
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

/// Sets `timer` to `next`, when that is not when it is set for already.
fn arm(timer: Pin<&mut Sleep>, at: &mut Option<Instant>, next: Option<Instant>) {
    if *at == next {
        return;
    }
    if let Some(next) = next {
        timer.reset(next);
    }
    *at = next;
}

/// Kills what is left of a closed pane's group at `deadline`, if it still
/// holds the PTY then.
async fn kill_at(cell: Arc<PaneCell>, deadline: Instant) {
    let mut ended = cell.output_ended.subscribe();
    if tokio::time::timeout_at(deadline, ended.wait_for(|ended| *ended))
        .await
        .is_err()
        && let Ok(true) = cell.kill_what_is_left()
    {
        tracing::info!(pane = %cell.id, "what the child left of its group outlived its close grace and was killed");
    }
}

/// Pasted text with each line ended by a carriage return, as a terminal
/// pastes it without bracketed paste.
fn paste_lines(text: &str) -> String {
    text.replace("\r\n", "\r").replace('\n', "\r")
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
/// Then the exited panes past the limits are dropped.
async fn deliver_exit(holder: Arc<Holder>, cell: Arc<PaneCell>, exit: Exit) {
    let mut ended = cell.output_ended.subscribe();
    let _ = tokio::time::timeout(EXIT_DRAIN, ended.wait_for(|ended| *ended)).await;
    {
        let mut inner = cell.inner();
        // What is held goes out before the exit: the start of a sequence
        // the query filter held, and each subscriber's held frame.
        let rest = inner.filter.flush();
        inner.fan_out(&rest, Instant::now());
        inner.flush_holds();
        cell.pay_title(&mut inner);
        inner.record.state = PaneState::Exited;
        inner.record.exit = Some(exit);
        let exited = HolderMessage::Exited {
            id: None,
            pane: cell.id.clone(),
            exit,
        };
        if let Some(frame) = message_frame(&exited) {
            inner
                .subscribers
                .retain(|sub| sub.conn.send_stream(sub.stream, frame.clone()));
        }
        cell.emit(&exited);
        cell.exit.send_replace(Some(exit));
    }
    holder.prune(Timestamp::now());
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

fn check_read(read: Read) -> Result<(), Failure> {
    if read.in_range() {
        Ok(())
    } else {
        Err(bad_request(format!(
            "recent takes 1 to {MAX_RECENT} rows, not {read:?}"
        )))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_without_bracketed_paste_ends_each_line_with_a_carriage_return() {
        assert_eq!(
            paste_lines("one\r\ntwo\nthree\rfour"),
            "one\rtwo\rthree\rfour"
        );
        assert_eq!(paste_lines("\n\n"), "\r\r");
        assert_eq!(paste_lines("plain"), "plain");
    }

    #[test]
    fn a_pane_has_a_burst_of_events_then_one_per_interval() {
        let mut rate = Rate::default();
        let start = Instant::now();
        for n in 0..EVENT_BURST {
            assert!(rate.allow(start), "event {n} of the burst");
        }
        assert!(!rate.allow(start));
        // Each interval that passes gives back one event, and no more.
        let later = start + EVENT_INTERVAL;
        assert!(rate.allow(later));
        assert!(!rate.allow(later));
        // A quiet spell gives back the whole burst, and no more.
        let quiet = start + EVENT_INTERVAL * (EVENT_BURST * 4);
        for n in 0..EVENT_BURST {
            assert!(rate.allow(quiet), "event {n} after the quiet");
        }
        assert!(!rate.allow(quiet));
    }
}
