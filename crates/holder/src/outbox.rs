//! A connection's queue of frames to send (`terminal-holder.md` 2.7, 2.8).
//!
//! Everything the holder sends a client goes through its outbox, so the
//! PTY reader never waits for a client: it queues a frame and goes on. The
//! queue keeps one order for all of a connection's frames, and counts their
//! bytes in three parts:
//!
//! - Replies and events, [`OUTBOX_CAP`] together. An event past it cuts the
//!   connection. A reply past it is refused with `busy`, and the `busy` has
//!   [`BUSY_ROOM`] more, so the client always learns of it.
//! - Each subscription's live output, [`SUBSCRIBER_CAP`] of its own. A slow
//!   client's subscriber is held up to it and loses nothing. Past it, the
//!   stream's queued output is dropped and the stream gets `lagged`, and the
//!   connection and its other streams go on.
//! - Repaints, [`REPAINT_CAP`], apart from the rest: a big screen's repaint
//!   alone can pass the other caps, and the client asked for it.
//!
//! A new subscription takes its place in the order with
//! [`Outbox::reserve`], under the pane's lock, and its first frames go
//! there with [`Outbox::fill`] once its repaint is drawn, away from that
//! lock. The writer waits at a place not filled yet, so the stream's live
//! output, queued behind it meanwhile, never comes first.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use tokio::time::Instant;

/// The most reply and event bytes a connection's queue holds.
pub const OUTBOX_CAP: usize = 4 * 1024 * 1024;

/// The room past [`OUTBOX_CAP`] for a `busy` reply and a stream's `lagged`.
pub const BUSY_ROOM: usize = 64 * 1024;

/// The most live output bytes one subscription's queue holds.
pub const SUBSCRIBER_CAP: usize = 4 * 1024 * 1024;

/// The most repaint bytes a connection's queue holds: one repaint of any
/// size, or several up to the frame limit together.
pub const REPAINT_CAP: usize = tiller_panes::frame::MAX_FRAME_LEN as usize;

/// How long a cut connection has to take its last frames.
pub const CUT_GRACE: Duration = Duration::from_secs(1);

/// Why a frame was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The frame would pass its cap.
    Full,
    /// The connection or the stream is closing, was cut or has ended.
    Gone,
}

/// What a queued frame's bytes count toward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Control,
    Stream(u32),
    Repaint,
}

#[derive(Debug)]
enum Entry {
    Frame {
        bytes: Vec<u8>,
        part: Part,
    },
    /// A new subscription's first frames go here.
    Slot(u32),
}

impl Entry {
    fn of_stream(&self, stream: u32) -> bool {
        match self {
            Entry::Frame { part, .. } => *part == Part::Stream(stream),
            Entry::Slot(own) => *own == stream,
        }
    }
}

#[derive(Debug, Default)]
struct Queue {
    entries: VecDeque<Entry>,
    /// Reply and event bytes queued.
    control: usize,
    /// Repaint bytes queued.
    repaint: usize,
    /// Each open stream's live output bytes queued. A stream not here
    /// takes no more.
    streams: HashMap<u32, usize>,
    /// No more frames are taken: the connection ends once these are out.
    closed: bool,
}

impl Queue {
    fn push(&mut self, bytes: Vec<u8>, part: Part) {
        match part {
            Part::Control => self.control += bytes.len(),
            Part::Repaint => self.repaint += bytes.len(),
            Part::Stream(stream) => {
                if let Some(queued) = self.streams.get_mut(&stream) {
                    *queued += bytes.len();
                }
            }
        }
        self.entries.push_back(Entry::Frame { bytes, part });
    }

    fn took(&mut self, len: usize, part: Part) {
        match part {
            Part::Control => self.control -= len,
            Part::Repaint => self.repaint -= len,
            Part::Stream(stream) => {
                if let Some(queued) = self.streams.get_mut(&stream) {
                    *queued -= len;
                }
            }
        }
    }

    /// Drops the stream's queued output, and its slot too when `slot`.
    fn drop_output(&mut self, stream: u32, slot: bool) {
        self.entries.retain(|entry| match entry {
            Entry::Frame { .. } => !entry.of_stream(stream),
            Entry::Slot(_) => !slot || !entry.of_stream(stream),
        });
    }
}

/// One connection's queue.
#[derive(Debug)]
pub struct Outbox {
    queue: Mutex<Queue>,
    wake: Notify,
    /// When the connection was cut, the time its last frames must be out by.
    cut: watch::Sender<Option<Instant>>,
}

impl Default for Outbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Outbox {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(Queue::default()),
            wake: Notify::new(),
            cut: watch::Sender::new(None),
        }
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues a reply or an event.
    pub fn push(&self, frame: Vec<u8>) -> Result<(), Refused> {
        self.push_control(frame, OUTBOX_CAP)
    }

    /// Queues a reply or an event with [`BUSY_ROOM`] past the cap: a `busy`
    /// that says the cap was reached.
    pub fn push_over(&self, frame: Vec<u8>) -> Result<(), Refused> {
        self.push_control(frame, OUTBOX_CAP + BUSY_ROOM)
    }

    fn push_control(&self, frame: Vec<u8>, cap: usize) -> Result<(), Refused> {
        let mut queue = self.queue();
        if queue.closed {
            return Err(Refused::Gone);
        }
        if queue.control + frame.len() > cap {
            return Err(Refused::Full);
        }
        queue.push(frame, Part::Control);
        drop(queue);
        self.wake.notify_one();
        Ok(())
    }

    /// Queues live output for `stream`, against its own
    /// [`SUBSCRIBER_CAP`].
    pub fn push_stream(&self, stream: u32, frame: Vec<u8>) -> Result<(), Refused> {
        let mut queue = self.queue();
        if queue.closed {
            return Err(Refused::Gone);
        }
        let Some(queued) = queue.streams.get(&stream) else {
            return Err(Refused::Gone);
        };
        if queued + frame.len() > SUBSCRIBER_CAP {
            return Err(Refused::Full);
        }
        queue.push(frame, Part::Stream(stream));
        drop(queue);
        self.wake.notify_one();
        Ok(())
    }

    /// Opens `stream` and takes the place of its first frames, which
    /// [`fill`](Self::fill) puts there. Until then the writer sends nothing
    /// that comes after it.
    pub fn reserve(&self, stream: u32) -> Result<(), Refused> {
        let mut queue = self.queue();
        if queue.closed {
            return Err(Refused::Gone);
        }
        queue.streams.insert(stream, 0);
        queue.entries.push_back(Entry::Slot(stream));
        Ok(())
    }

    /// Puts `stream`'s first frames, all or none, in the place
    /// [`reserve`](Self::reserve) took. They are refused only when other
    /// repaints are queued and the lot would pass [`REPAINT_CAP`]; the
    /// place is kept for [`drop_stream`](Self::drop_stream).
    pub fn fill(&self, stream: u32, frames: Vec<Vec<u8>>) -> Result<(), Refused> {
        let mut queue = self.queue();
        let Some(at) = queue
            .entries
            .iter()
            .position(|entry| matches!(entry, Entry::Slot(own) if *own == stream))
        else {
            return Err(Refused::Gone);
        };
        let len: usize = frames.iter().map(Vec::len).sum();
        if queue.repaint > 0 && queue.repaint + len > REPAINT_CAP {
            return Err(Refused::Full);
        }
        queue.repaint += len;
        queue.entries.remove(at);
        for (offset, bytes) in frames.into_iter().enumerate() {
            queue.entries.insert(
                at + offset,
                Entry::Frame {
                    bytes,
                    part: Part::Repaint,
                },
            );
        }
        drop(queue);
        self.wake.notify_one();
        Ok(())
    }

    /// Ends `stream` because it fell behind: its queued live output is
    /// dropped, `last` (its `lagged`) is queued with [`BUSY_ROOM`] past the
    /// cap, and it takes no more. A place not filled yet is kept, so its
    /// reply still comes first. False when the stream had ended already.
    pub fn lag(&self, stream: u32, last: Vec<u8>) -> bool {
        let mut queue = self.queue();
        if queue.closed || queue.streams.remove(&stream).is_none() {
            return false;
        }
        queue.drop_output(stream, false);
        if queue.control + last.len() <= OUTBOX_CAP + BUSY_ROOM {
            queue.push(last, Part::Control);
        }
        drop(queue);
        self.wake.notify_one();
        true
    }

    /// Ends `stream`: it takes no more, and what is queued for it goes out.
    pub fn end_stream(&self, stream: u32) {
        self.queue().streams.remove(&stream);
    }

    /// Ends `stream` and drops all that is queued for it, its place too:
    /// a subscription that failed before its first frames.
    pub fn drop_stream(&self, stream: u32) {
        let mut queue = self.queue();
        queue.streams.remove(&stream);
        queue.drop_output(stream, true);
        drop(queue);
        self.wake.notify_one();
    }

    /// Drops every queued frame, queues `last` instead, and takes no more.
    /// The writer sends `last` within [`CUT_GRACE`] and ends. False when the
    /// outbox was closed already.
    pub fn cut(&self, last: Vec<Vec<u8>>) -> bool {
        let mut queue = self.queue();
        if queue.closed {
            return false;
        }
        *queue = Queue {
            closed: true,
            ..Queue::default()
        };
        for frame in last {
            queue.push(frame, Part::Control);
        }
        drop(queue);
        self.cut.send_replace(Some(Instant::now() + CUT_GRACE));
        self.wake.notify_one();
        true
    }

    /// Takes no more frames. The writer sends what is queued and ends. A
    /// place reserved before still takes its frames.
    pub fn close(&self) {
        self.queue().closed = true;
        self.wake.notify_one();
    }

    /// The next frame to send, or `None` once the outbox is closed and
    /// empty.
    pub async fn next(&self) -> Option<Vec<u8>> {
        loop {
            let wake = self.wake.notified();
            {
                let mut queue = self.queue();
                match queue.entries.front() {
                    Some(Entry::Frame { .. }) => {
                        if let Some(Entry::Frame { bytes, part }) = queue.entries.pop_front() {
                            queue.took(bytes.len(), part);
                            return Some(bytes);
                        }
                    }
                    // Its frames are on their way.
                    Some(Entry::Slot(_)) => {}
                    None if queue.closed => return None,
                    None => {}
                }
            }
            wake.await;
        }
    }

    /// When the cut connection's last frames must be out by. It waits until
    /// a cut, if there is none yet.
    pub async fn cut_deadline(&self) -> Instant {
        let mut cut = self.cut.subscribe();
        match cut.wait_for(Option::is_some).await {
            Ok(deadline) => deadline.unwrap_or_else(Instant::now),
            // The sender lives in `self`, so this cannot end.
            Err(_) => Instant::now(),
        }
    }

    /// Whether the outbox takes no more frames: the connection was cut or
    /// is closing.
    pub fn is_closed(&self) -> bool {
        self.queue().closed
    }

    /// The cut's deadline, if the connection was cut.
    pub fn cut_at(&self) -> Option<Instant> {
        *self.cut.borrow()
    }

    /// Bytes queued now, of every part.
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        let queue = self.queue();
        queue.control + queue.repaint + queue.streams.values().sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_come_out_in_order_and_a_close_ends_after_them() {
        let outbox = Outbox::new();
        outbox.push(b"one".to_vec()).unwrap();
        outbox.push(b"two".to_vec()).unwrap();
        outbox.close();
        assert_eq!(outbox.push(b"late".to_vec()), Err(Refused::Gone));
        assert_eq!(outbox.next().await.unwrap(), b"one");
        assert_eq!(outbox.next().await.unwrap(), b"two");
        assert_eq!(outbox.next().await, None);
    }

    #[tokio::test]
    async fn a_frame_past_the_cap_is_refused_and_a_cut_keeps_only_its_last_frames() {
        let outbox = Outbox::new();
        outbox.push(vec![0; OUTBOX_CAP - 10]).unwrap();
        outbox.push(vec![0; 10]).unwrap();
        assert_eq!(outbox.push(vec![0; 1]), Err(Refused::Full));
        // A `busy` still fits.
        outbox.push_over(vec![0; 100]).unwrap();
        assert_eq!(outbox.push_over(vec![0; BUSY_ROOM]), Err(Refused::Full));
        assert_eq!(outbox.queued(), OUTBOX_CAP + 100);
        assert!(outbox.cut_at().is_none());
        assert!(outbox.cut(vec![b"lagged".to_vec()]));
        assert!(outbox.cut_at().is_some());
        assert_eq!(outbox.push(b"x".to_vec()), Err(Refused::Gone));
        assert_eq!(outbox.next().await.unwrap(), b"lagged");
        assert_eq!(outbox.next().await, None);
        assert!(!outbox.cut(Vec::new()));
        let deadline = outbox.cut_deadline().await;
        assert!(deadline <= Instant::now() + CUT_GRACE);
    }

    #[tokio::test]
    async fn a_slow_stream_is_held_to_its_cap_then_lagged_and_the_rest_go_on() {
        let outbox = Outbox::new();
        outbox.reserve(1).unwrap();
        outbox.fill(1, vec![b"repaint".to_vec()]).unwrap();
        outbox.reserve(2).unwrap();
        outbox.fill(2, vec![b"other".to_vec()]).unwrap();
        // Stream 1 is held to the byte, with no frame lost.
        let chunk = vec![1; 64 * 1024];
        for _ in 0..SUBSCRIBER_CAP / chunk.len() {
            outbox.push_stream(1, chunk.clone()).unwrap();
        }
        // Each stream has a cap of its own, and replies have theirs.
        outbox.push_stream(2, vec![2; 10]).unwrap();
        outbox.push(b"reply".to_vec()).unwrap();
        assert_eq!(outbox.push_stream(1, vec![1; 1]), Err(Refused::Full));
        assert!(outbox.lag(1, b"lagged".to_vec()));
        assert_eq!(outbox.push_stream(1, vec![1; 1]), Err(Refused::Gone));
        assert!(!outbox.lag(1, b"lagged".to_vec()));
        let mut out = Vec::new();
        while outbox.queued() > 0 {
            out.push(outbox.next().await.unwrap());
        }
        assert_eq!(
            out,
            [
                b"repaint".to_vec(),
                b"other".to_vec(),
                vec![2; 10],
                b"reply".to_vec(),
                b"lagged".to_vec()
            ]
        );
    }

    #[tokio::test]
    async fn the_writer_waits_at_a_place_until_it_is_filled() {
        let outbox = std::sync::Arc::new(Outbox::new());
        outbox.push(b"before".to_vec()).unwrap();
        outbox.reserve(7).unwrap();
        outbox.push_stream(7, b"live".to_vec()).unwrap();
        outbox.push(b"after".to_vec()).unwrap();
        assert_eq!(outbox.next().await.unwrap(), b"before");
        let waiter = tokio::spawn({
            let outbox = outbox.clone();
            async move { outbox.next().await }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished());
        outbox
            .fill(7, vec![b"subscribed".to_vec(), b"repaint".to_vec()])
            .unwrap();
        assert_eq!(waiter.await.unwrap().unwrap(), b"subscribed");
        assert_eq!(outbox.next().await.unwrap(), b"repaint");
        assert_eq!(outbox.next().await.unwrap(), b"live");
        assert_eq!(outbox.next().await.unwrap(), b"after");
        // A place that failed is dropped with what came for it.
        outbox.reserve(8).unwrap();
        outbox.push_stream(8, b"lost".to_vec()).unwrap();
        outbox.push(b"kept".to_vec()).unwrap();
        outbox.drop_stream(8);
        assert_eq!(outbox.fill(8, Vec::new()), Err(Refused::Gone));
        assert_eq!(outbox.next().await.unwrap(), b"kept");
        assert_eq!(outbox.queued(), 0);
    }

    #[tokio::test]
    async fn a_lag_before_the_fill_keeps_the_reply_first() {
        let outbox = Outbox::new();
        outbox.reserve(3).unwrap();
        outbox.push_stream(3, vec![3; 10]).unwrap();
        assert!(outbox.lag(3, b"lagged".to_vec()));
        outbox.fill(3, vec![b"subscribed".to_vec()]).unwrap();
        assert_eq!(outbox.next().await.unwrap(), b"subscribed");
        assert_eq!(outbox.next().await.unwrap(), b"lagged");
        assert_eq!(outbox.queued(), 0);
    }

    #[tokio::test]
    async fn an_ended_stream_sends_what_it_queued_and_takes_no_more() {
        let outbox = Outbox::new();
        outbox.reserve(4).unwrap();
        outbox.fill(4, Vec::new()).unwrap();
        outbox.push_stream(4, b"last".to_vec()).unwrap();
        outbox.end_stream(4);
        assert_eq!(outbox.push_stream(4, b"x".to_vec()), Err(Refused::Gone));
        assert_eq!(outbox.next().await.unwrap(), b"last");
        assert_eq!(outbox.queued(), 0);
    }

    #[tokio::test]
    async fn a_repaint_over_the_cap_is_taken_apart_from_it() {
        let outbox = Outbox::new();
        outbox.push(vec![1; 10]).unwrap();
        outbox.reserve(1).unwrap();
        outbox
            .fill(1, vec![vec![2; 5], vec![3; OUTBOX_CAP + 1]])
            .unwrap();
        // Replies still have the whole cap but the bytes before it.
        outbox.push(vec![4; OUTBOX_CAP - 10]).unwrap();
        assert_eq!(outbox.push(vec![5; 1]), Err(Refused::Full));
        // A second repaint fits while the lot stays under its cap.
        outbox.reserve(2).unwrap();
        assert_eq!(
            outbox.fill(2, vec![vec![6; REPAINT_CAP]]),
            Err(Refused::Full)
        );
        outbox.fill(2, vec![vec![7; 100]]).unwrap();
        let mut firsts = Vec::new();
        for _ in 0..5 {
            firsts.push(outbox.next().await.unwrap()[0]);
        }
        assert_eq!(firsts, [1, 2, 3, 4, 7]);
        assert_eq!(outbox.queued(), 0);
        // With none queued, one repaint of any size is taken.
        outbox.reserve(3).unwrap();
        outbox.fill(3, vec![vec![8; REPAINT_CAP + 1]]).unwrap();
    }

    #[tokio::test]
    async fn a_waiting_writer_wakes_for_a_frame() {
        let outbox = std::sync::Arc::new(Outbox::new());
        let waiter = tokio::spawn({
            let outbox = outbox.clone();
            async move { outbox.next().await }
        });
        tokio::task::yield_now().await;
        outbox.push(b"hi".to_vec()).unwrap();
        assert_eq!(waiter.await.unwrap().unwrap(), b"hi");
    }
}
