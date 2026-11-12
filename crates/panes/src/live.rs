//! What [`Live`](crate::Live) hands out: a pane's output ([`Subscription`],
//! [`StreamHandle`]) and the holder's events ([`Events`]).
//!
//! These types do not know where their data comes from. The socket client
//! backs them with a connection each, and [`FakePanes`](crate::FakePanes)
//! with channels. Another backend implements [`Feed`], [`Control`] and
//! [`EventFeed`] and builds them with `from_parts`.

use std::sync::Arc;

use crate::error::Result;
use crate::message::{Event, Exit, PaneId, SyncMode};

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
    /// The PTY's size now: the holder took a [`Subscription::resize`] or a
    /// [`Subscription::release`], or another lease set it.
    Resized { cols: u16, rows: u16 },
    /// The pane's child was reaped. It comes after the repaint even when
    /// the pane had exited before the subscription started.
    Exited(Exit),
    /// The holder cut this subscription because it fell behind its queue.
    /// Nothing more comes; subscribe again for a fresh repaint.
    Lagged,
}

/// Where a subscription's updates come from.
pub trait Feed: Send {
    /// The next update, as [`Subscription::recv`] says.
    fn recv(&mut self) -> Result<Option<Update>>;
}

/// What a [`StreamHandle`] asks of a subscription's backend. The answers
/// come through the [`Feed`].
pub trait Control: Send + Sync {
    fn resize(&self, cols: u16, rows: u16) -> Result<()>;
    fn release(&self) -> Result<()>;
    /// Ends the stream: a waiting [`Feed::recv`] returns `None` now.
    fn end(&self);
}

/// A pane's output, one repaint and then live bytes. Dropping it ends the
/// stream, and with it any size its lease set, even while a
/// [`StreamHandle`] lives.
pub struct Subscription {
    stream: u32,
    cols: u16,
    rows: u16,
    feed: Box<dyn Feed>,
    handle: StreamHandle,
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("pane", &self.handle.pane)
            .field("stream", &self.stream)
            .field("cols", &self.cols)
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.handle.end();
    }
}

impl Subscription {
    /// A subscription to `pane` on stream number `stream`, which started at
    /// this size. Resizes name the lease `lease`.
    pub fn from_parts(
        pane: PaneId,
        stream: u32,
        (cols, rows): (u16, u16),
        lease: String,
        feed: Box<dyn Feed>,
        control: Arc<dyn Control>,
    ) -> Self {
        Self {
            stream,
            cols,
            rows,
            feed,
            handle: StreamHandle {
                pane,
                lease,
                control,
            },
        }
    }

    pub fn pane(&self) -> &PaneId {
        &self.handle.pane
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

    /// [`StreamHandle::release`] on this subscription.
    pub fn release(&self) -> Result<()> {
        self.handle.release()
    }

    /// The next update. `None` once the stream has ended: after
    /// [`Update::Lagged`], after a close that follows [`Update::Exited`],
    /// after [`StreamHandle::end`], or after an earlier error ended it. A
    /// close at any other time is `Unreachable`: the holder may have died,
    /// so the pane's state is unknown, not gone. An `error` reply to a
    /// resize or a release is an error that leaves the stream open; any
    /// other error ends it.
    pub fn recv(&mut self) -> Result<Option<Update>> {
        self.feed.recv()
    }
}

/// Resizes, releases or ends a [`Subscription`]. It is `Send` and `Clone`,
/// so one thread can use it while another waits in [`Subscription::recv`].
/// The holder's answers come through `recv`.
#[derive(Clone)]
pub struct StreamHandle {
    pane: PaneId,
    lease: String,
    control: Arc<dyn Control>,
}

impl std::fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamHandle")
            .field("pane", &self.pane)
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}

impl StreamHandle {
    pub fn pane(&self) -> &PaneId {
        &self.pane
    }

    /// The lease id this subscription's resizes name.
    pub fn lease(&self) -> &str {
        &self.lease
    }

    /// Asks the holder to set the PTY to this size, under the subscription's
    /// lease (the board's Fit). The answer comes through
    /// [`Subscription::recv`] as [`Update::Resized`], or as an error.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.control.resize(cols, rows)
    }

    /// Drops the subscription's lease, so the size goes back to the next
    /// lease, and keeps the stream. The answer comes as [`resize`]'s does.
    ///
    /// [`resize`]: StreamHandle::resize
    pub fn release(&self) -> Result<()> {
        self.control.release()
    }

    /// Ends the subscription from any thread: the stream closes, the
    /// holder drops its lease, and a [`Subscription::recv`] waiting now
    /// returns `None`. Later resizes fail.
    pub fn end(&self) {
        self.control.end();
    }
}

/// Where a watch's events come from.
pub trait EventFeed: Send {
    /// The next event, as [`Events::recv`] says.
    fn recv(&mut self) -> Result<Option<Event>>;
}

/// The holder's events, from `watch`. Dropping it ends the watch.
pub struct Events {
    feed: Box<dyn EventFeed>,
    end: EventsEnd,
}

impl std::fmt::Debug for Events {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Events").finish_non_exhaustive()
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        self.end.end();
    }
}

impl Events {
    /// A watch that reads `feed`, and that `end` stops from any thread.
    pub fn from_parts(feed: Box<dyn EventFeed>, end: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            feed,
            end: EventsEnd(end),
        }
    }

    /// The next event. `None` once the watch has ended, by
    /// [`EventsEnd::end`] or after an earlier error. A close by the holder
    /// is `Unreachable`: what happened since is unknown, so the caller runs
    /// `list` again after it reconnects.
    pub fn recv(&mut self) -> Result<Option<Event>> {
        self.feed.recv()
    }

    /// A handle that ends this watch from another thread.
    pub fn ender(&self) -> EventsEnd {
        self.end.clone()
    }
}

/// Ends an [`Events`] from any thread: a [`Events::recv`] waiting now
/// returns `None`.
#[derive(Clone)]
pub struct EventsEnd(Arc<dyn Fn() + Send + Sync>);

impl std::fmt::Debug for EventsEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventsEnd").finish_non_exhaustive()
    }
}

impl EventsEnd {
    pub fn end(&self) {
        (self.0)();
    }
}
