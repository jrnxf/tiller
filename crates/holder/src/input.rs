//! Raw input on its way to a pane's PTY, and the cap on it.
//!
//! A kind 2 frame has no reply, and the holder never waits for a pane's app
//! to read, as that would hold up every other request on the connection.
//! So input waits in a queue per pane, in the order it came. Each client's
//! unwritten input to a pane counts apart, so a client is never cut for
//! what other clients sent:
//!
//! - A frame is taken while the client's unwritten input to the pane is
//!   under [`INPUT_CAP`], whatever its size. Bytes count only until they
//!   are written, so a key while the app reads a big paste is not cut.
//! - Once the pane's app has left [`INPUT_CAP`] of the client's input
//!   unread, its next frame to the pane cuts the client, so a gap in its
//!   input is never silent.
//! - A cut client's input is written no further, from the next write on,
//!   and what waits is dropped, so a client cut again and again does not
//!   grow the queue. A client that closes, or breaks off, keeps the input
//!   it sent, which is written as the app reads: the holder had all of it.
//!
//! H04 puts back-pressure in place of this.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;

/// How much of one client's input to a pane may be left unread before its
/// next frame to the pane cuts it. The same as a client's output queue.
pub const INPUT_CAP: usize = crate::outbox::OUTBOX_CAP;

/// The client a frame came from, as the input queues see it.
#[derive(Debug)]
pub struct Sender {
    /// Unique within the holder's run, never reused.
    id: u64,
    /// Set once the client is cut: its input is written no further.
    cut: AtomicBool,
}

impl Default for Sender {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            cut: AtomicBool::new(false),
        }
    }
}

impl Sender {
    /// The client was cut. Each queue drops its input as it comes to it,
    /// and stops one being written at its next write.
    pub fn cut(&self) {
        self.cut.store(true, Ordering::Release);
    }

    fn is_cut(&self) -> bool {
        self.cut.load(Ordering::Acquire)
    }
}

/// Why input was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The client is at [`INPUT_CAP`] for this pane: the caller cuts it.
    /// It holds the client's unwritten bytes.
    Full(usize),
    /// The PTY closed.
    Closed,
}

/// A pane's unwritten input.
#[derive(Debug, Default)]
pub struct InputQueue {
    state: Mutex<State>,
    wake: Notify,
}

#[derive(Debug, Default)]
struct State {
    /// Each frame not yet taken by the writer, and its client.
    chunks: VecDeque<(Arc<Sender>, Vec<u8>)>,
    /// Each client's unwritten bytes, in `chunks` and in the writer's
    /// hands. Only clients with input waiting: a count that falls to 0
    /// goes.
    by_sender: HashMap<u64, usize>,
    /// The PTY closed: no more is taken.
    closed: bool,
}

impl State {
    fn give_back(&mut self, sender: u64, len: usize) {
        if let Some(mine) = self.by_sender.get_mut(&sender) {
            *mine = mine.saturating_sub(len);
            if *mine == 0 {
                self.by_sender.remove(&sender);
            }
        }
    }

    /// Drops the waiting input of every cut client.
    fn drop_cut(&mut self) {
        let mut dropped = Vec::new();
        self.chunks.retain(|(sender, bytes)| {
            let cut = sender.is_cut();
            if cut {
                dropped.push((sender.id, bytes.len()));
            }
            !cut
        });
        for (sender, len) in dropped {
            self.give_back(sender, len);
        }
    }
}

/// A frame the writer took. Its unwritten part counts toward its client's
/// cap until it is written or dropped.
#[derive(Debug)]
pub struct InFlight<'a> {
    bytes: Vec<u8>,
    written: usize,
    queue: &'a InputQueue,
    sender: Arc<Sender>,
}

impl InFlight<'_> {
    /// The bytes still to write, or none once the client was cut.
    pub fn rest(&self) -> &[u8] {
        if self.sender.is_cut() {
            return &[];
        }
        self.bytes.get(self.written..).unwrap_or_default()
    }

    /// `len` more bytes were written.
    pub fn wrote(&mut self, len: usize) {
        let len = len.min(self.bytes.len() - self.written);
        self.written += len;
        self.queue.state().give_back(self.sender.id, len);
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let unwritten = self.bytes.len() - self.written;
        self.queue.state().give_back(self.sender.id, unwritten);
    }
}

impl InputQueue {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `bytes` from `sender`. A frame is taken whole while the
    /// sender's unwritten input is under [`INPUT_CAP`].
    pub fn push(&self, sender: &Arc<Sender>, bytes: Vec<u8>) -> Result<(), Refused> {
        let mut state = self.state();
        if state.closed {
            return Err(Refused::Closed);
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let mine = state.by_sender.get(&sender.id).copied().unwrap_or(0);
        if mine >= INPUT_CAP {
            return Err(Refused::Full(mine));
        }
        state.by_sender.insert(sender.id, mine + bytes.len());
        state.chunks.push_back((sender.clone(), bytes));
        drop(state);
        self.wake.notify_one();
        Ok(())
    }

    /// Drops what cut clients left waiting, so the memory goes now and not
    /// only once the app reads.
    pub fn drop_cut(&self) {
        self.state().drop_cut();
    }

    /// Takes no more input and drops what waits: the PTY closed.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        let chunks = std::mem::take(&mut state.chunks);
        for (sender, bytes) in chunks {
            state.give_back(sender.id, bytes.len());
        }
    }

    /// The next frame to write, in the order frames came. A cut client's
    /// frames are dropped, not written.
    pub async fn next(&self) -> InFlight<'_> {
        loop {
            let wake = self.wake.notified();
            let next = {
                let mut state = self.state();
                state.drop_cut();
                state.chunks.pop_front()
            };
            if let Some((sender, bytes)) = next {
                return InFlight {
                    bytes,
                    written: 0,
                    queue: self,
                    sender,
                };
            }
            wake.await;
        }
    }

    /// Bytes waiting in all, in the queue and in the writer's hands.
    #[cfg(test)]
    fn queued(&self) -> usize {
        self.state().by_sender.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn senders() -> (Arc<Sender>, Arc<Sender>) {
        (Arc::default(), Arc::default())
    }

    #[tokio::test]
    async fn a_client_at_its_cap_is_refused_and_another_is_not() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, vec![0; INPUT_CAP - 1]).unwrap();
        // Under the cap, a frame of any size is taken.
        queue.push(&one, vec![0; 10]).unwrap();
        assert_eq!(
            queue.push(&one, vec![0; 1]),
            Err(Refused::Full(INPUT_CAP + 9))
        );
        queue.push(&two, vec![0; 1]).unwrap();
        // Written input makes room again.
        drop(queue.next().await);
        queue.push(&one, vec![0; 1]).unwrap();
        assert_eq!(queue.queued(), 10 + 1 + 1);
    }

    #[tokio::test]
    async fn bytes_count_until_they_are_written_and_frames_keep_their_order() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, vec![1; INPUT_CAP + 100]).unwrap();
        queue.push(&two, b"two".to_vec()).unwrap();
        let mut first = queue.next().await;
        first.wrote(INPUT_CAP);
        // Most of the paste is written, so a key from its client is taken.
        assert_eq!(queue.queued(), 100 + 3);
        queue.push(&one, b"key".to_vec()).unwrap();
        assert_eq!(first.rest(), [1; 100]);
        first.wrote(100);
        assert!(first.rest().is_empty());
        drop(first);
        assert_eq!(queue.next().await.rest(), b"two");
        assert_eq!(queue.next().await.rest(), b"key");
        assert_eq!(queue.queued(), 0);
        assert!(queue.state().by_sender.is_empty());
        // An empty frame is nothing to write.
        queue.push(&one, Vec::new()).unwrap();
        assert!(queue.state().chunks.is_empty());
    }

    #[tokio::test]
    async fn a_cut_client_s_input_is_written_no_further_and_the_rest_stays() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, b"a1".to_vec()).unwrap();
        queue.push(&two, b"b1".to_vec()).unwrap();
        queue.push(&one, b"a2".to_vec()).unwrap();
        let in_flight = queue.next().await;
        one.cut();
        assert!(in_flight.rest().is_empty());
        queue.drop_cut();
        // The frame in flight counts until the writer lets it go.
        assert_eq!(queue.queued(), 4);
        drop(in_flight);
        assert_eq!(queue.next().await.rest(), b"b1");
        assert_eq!(queue.queued(), 0);
        assert!(queue.state().by_sender.is_empty());
    }

    #[tokio::test]
    async fn a_closed_queue_drops_what_waits_and_takes_no_more() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, vec![0; 1000]).unwrap();
        queue.push(&two, vec![0; 1000]).unwrap();
        queue.close();
        assert_eq!(queue.queued(), 0);
        assert_eq!(queue.push(&one, vec![0; 1]), Err(Refused::Closed));
    }
}
