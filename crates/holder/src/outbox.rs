//! A connection's queue of frames to send (`terminal-holder.md` 2.7, 2.8).
//!
//! Everything the holder sends a client goes through its outbox, so the
//! PTY reader never waits for a client: it queues a frame and goes on. The
//! queue has one fixed cap, [`OUTBOX_CAP`]. A client that lets it fill is
//! cut: its queued frames are dropped, each of its subscriptions gets
//! `lagged`, and the connection ends once those are out or after
//! [`CUT_GRACE`]. H04 puts per-subscriber back-pressure in place of this.
//!
//! A repaint does not count toward the cap: a big screen's repaint alone
//! can pass it, and the client asked for it. Repaints have a cap of their
//! own, [`REPAINT_CAP`].

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, watch};
use tokio::time::Instant;

/// The most bytes a connection's queue holds.
pub const OUTBOX_CAP: usize = 4 * 1024 * 1024;

/// The most repaint bytes a connection's queue holds, apart from
/// [`OUTBOX_CAP`]: one repaint of any size, or several up to the frame
/// limit together.
pub const REPAINT_CAP: usize = tiller_panes::frame::MAX_FRAME_LEN as usize;

/// How long a cut connection has to take its last frames.
pub const CUT_GRACE: Duration = Duration::from_secs(1);

/// Why a frame was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The frame would pass the cap. The caller cuts the connection.
    Full,
    /// The connection is closing or was cut.
    Gone,
}

#[derive(Debug, Default)]
struct Queue {
    /// Each frame, and whether it is a repaint's.
    frames: VecDeque<(Vec<u8>, bool)>,
    /// Bytes queued apart from repaints.
    bytes: usize,
    /// Repaint bytes queued.
    repaint_bytes: usize,
    /// No more frames are taken: the connection ends once these are out.
    closed: bool,
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

    /// Queues one encoded frame.
    pub fn push(&self, frame: Vec<u8>) -> Result<(), Refused> {
        let mut queue = self.queue();
        if queue.closed {
            return Err(Refused::Gone);
        }
        if queue.bytes + frame.len() > OUTBOX_CAP {
            return Err(Refused::Full);
        }
        queue.bytes += frame.len();
        queue.frames.push_back((frame, false));
        drop(queue);
        self.wake.notify_one();
        Ok(())
    }

    /// Queues the frames of one repaint, all or none, apart from
    /// [`OUTBOX_CAP`]. They are refused only when other repaints are queued
    /// and the lot would pass [`REPAINT_CAP`].
    pub fn push_repaint(&self, frames: Vec<Vec<u8>>) -> Result<(), Refused> {
        let mut queue = self.queue();
        if queue.closed {
            return Err(Refused::Gone);
        }
        let len: usize = frames.iter().map(Vec::len).sum();
        if queue.repaint_bytes > 0 && queue.repaint_bytes + len > REPAINT_CAP {
            return Err(Refused::Full);
        }
        queue.repaint_bytes += len;
        queue
            .frames
            .extend(frames.into_iter().map(|frame| (frame, true)));
        drop(queue);
        self.wake.notify_one();
        Ok(())
    }

    /// Drops every queued frame, queues `last` instead, and takes no more.
    /// The writer sends `last` within [`CUT_GRACE`] and ends. False when the
    /// outbox was closed already.
    pub fn cut(&self, last: Vec<Vec<u8>>) -> bool {
        let mut queue = self.queue();
        if queue.closed {
            return false;
        }
        queue.closed = true;
        queue.bytes = last.iter().map(Vec::len).sum();
        queue.repaint_bytes = 0;
        queue.frames = last.into_iter().map(|frame| (frame, false)).collect();
        drop(queue);
        self.cut.send_replace(Some(Instant::now() + CUT_GRACE));
        self.wake.notify_one();
        true
    }

    /// Takes no more frames. The writer sends what is queued and ends.
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
                if let Some((frame, repaint)) = queue.frames.pop_front() {
                    if repaint {
                        queue.repaint_bytes -= frame.len();
                    } else {
                        queue.bytes -= frame.len();
                    }
                    return Some(frame);
                }
                if queue.closed {
                    return None;
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

    /// The cut's deadline, if the connection was cut.
    pub fn cut_at(&self) -> Option<Instant> {
        *self.cut.borrow()
    }

    /// Bytes queued now, repaints with the rest.
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        let queue = self.queue();
        queue.bytes + queue.repaint_bytes
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
        assert_eq!(outbox.queued(), OUTBOX_CAP);
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
    async fn a_repaint_over_the_cap_is_taken_apart_from_it() {
        let outbox = Outbox::new();
        outbox.push(vec![1; 10]).unwrap();
        outbox
            .push_repaint(vec![vec![2; 5], vec![3; OUTBOX_CAP + 1]])
            .unwrap();
        // Live output still has the whole cap but the bytes before it.
        outbox.push(vec![4; OUTBOX_CAP - 10]).unwrap();
        assert_eq!(outbox.push(vec![5; 1]), Err(Refused::Full));
        // A second repaint fits while the lot stays under its cap.
        assert_eq!(
            outbox.push_repaint(vec![vec![6; REPAINT_CAP]]),
            Err(Refused::Full)
        );
        outbox.push_repaint(vec![vec![7; 100]]).unwrap();
        let mut firsts = Vec::new();
        for _ in 0..5 {
            firsts.push(outbox.next().await.unwrap()[0]);
        }
        assert_eq!(firsts, [1, 2, 3, 4, 7]);
        assert_eq!(outbox.queued(), 0);
        // With none queued, one repaint of any size is taken.
        outbox.push_repaint(vec![vec![8; REPAINT_CAP + 1]]).unwrap();
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
