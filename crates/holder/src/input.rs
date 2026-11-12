//! Raw input on its way to a pane's PTY, and the cap on it.
//!
//! A kind 2 frame has no reply, and the holder never waits for a pane's app
//! to read, as that would hold up every other request on the connection.
//! So input waits in a queue per pane, in the order it came. Each client's
//! unwritten input to a pane counts apart, so a client is never cut for
//! what other clients sent:
//!
//! - A frame is taken while the client's unwritten input to the pane is
//!   under [`INPUT_CAP`], whatever its size, so a big paste right after a
//!   key is not cut because the key is still in flight.
//! - Once the pane's app has left [`INPUT_CAP`] of the client's input
//!   unread, its next frame to the pane cuts the client, so a gap in its
//!   input is never silent.
//! - A cut client's unwritten input is dropped at once, so a client cut
//!   again and again does not grow the queue. A client that closes keeps
//!   its input, which is written as the app reads.
//!
//! H04 puts back-pressure in place of this.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::Notify;

/// How much of one client's input to a pane may be left unread before its
/// next frame to the pane cuts it. The same as a client's output queue.
pub const INPUT_CAP: usize = crate::outbox::OUTBOX_CAP;

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
    chunks: VecDeque<(u64, Vec<u8>)>,
    /// Each client's bytes in `chunks` and in the writer's hands. Only
    /// clients with input waiting: a count that falls to 0 goes.
    by_conn: HashMap<u64, usize>,
    /// The PTY closed: no more is taken.
    closed: bool,
}

impl State {
    fn give_back(&mut self, conn: u64, len: usize) {
        if let Some(mine) = self.by_conn.get_mut(&conn) {
            *mine = mine.saturating_sub(len);
            if *mine == 0 {
                self.by_conn.remove(&conn);
            }
        }
    }
}

/// A frame the writer took: it counts toward its client's cap until it is
/// written or dropped.
#[derive(Debug)]
pub struct InFlight<'a> {
    pub bytes: Vec<u8>,
    queue: &'a InputQueue,
    conn: u64,
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.queue.state().give_back(self.conn, self.bytes.len());
    }
}

impl InputQueue {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `bytes` from the client `conn`.
    pub fn push(&self, conn: u64, bytes: Vec<u8>) -> Result<(), Refused> {
        let mut state = self.state();
        if state.closed {
            return Err(Refused::Closed);
        }
        let mine = state.by_conn.get(&conn).copied().unwrap_or(0);
        if mine >= INPUT_CAP {
            return Err(Refused::Full(mine));
        }
        state.by_conn.insert(conn, mine + bytes.len());
        state.chunks.push_back((conn, bytes));
        drop(state);
        self.wake.notify_one();
        Ok(())
    }

    /// Drops the client's input that the writer has not taken: the client
    /// was cut.
    pub fn drop_conn(&self, conn: u64) {
        let mut state = self.state();
        let mut dropped = 0;
        state.chunks.retain(|(own, bytes)| {
            let keep = *own != conn;
            if !keep {
                dropped += bytes.len();
            }
            keep
        });
        state.give_back(conn, dropped);
    }

    /// Takes no more input and drops what waits: the PTY closed.
    pub fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        let chunks = std::mem::take(&mut state.chunks);
        for (conn, bytes) in chunks {
            state.give_back(conn, bytes.len());
        }
    }

    /// The next frame to write, in the order frames came.
    pub async fn next(&self) -> InFlight<'_> {
        loop {
            let wake = self.wake.notified();
            let next = self.state().chunks.pop_front();
            if let Some((conn, bytes)) = next {
                return InFlight {
                    bytes,
                    queue: self,
                    conn,
                };
            }
            wake.await;
        }
    }

    /// Bytes waiting in all, in the queue and in the writer's hands.
    #[cfg(test)]
    fn queued(&self) -> usize {
        self.state().by_conn.values().sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_client_at_its_cap_is_refused_and_another_is_not() {
        let queue = InputQueue::default();
        queue.push(1, vec![0; INPUT_CAP - 1]).unwrap();
        // Under the cap, a frame of any size is taken.
        queue.push(1, vec![0; 10]).unwrap();
        assert_eq!(queue.push(1, vec![0; 1]), Err(Refused::Full(INPUT_CAP + 9)));
        queue.push(2, vec![0; 1]).unwrap();
        // Written input makes room again.
        drop(queue.next().await);
        queue.push(1, vec![0; 1]).unwrap();
        assert_eq!(queue.queued(), 10 + 1 + 1);
    }

    #[tokio::test]
    async fn frames_come_out_in_order_and_count_until_written() {
        let queue = InputQueue::default();
        queue.push(1, b"one".to_vec()).unwrap();
        queue.push(2, b"two".to_vec()).unwrap();
        let first = queue.next().await;
        assert_eq!(first.bytes, b"one");
        assert_eq!(queue.queued(), 6);
        drop(first);
        assert_eq!(queue.next().await.bytes, b"two");
        assert_eq!(queue.queued(), 0);
        assert!(queue.state().by_conn.is_empty());
    }

    #[tokio::test]
    async fn a_cut_client_s_waiting_input_goes_and_the_rest_stays() {
        let queue = InputQueue::default();
        queue.push(1, b"a1".to_vec()).unwrap();
        queue.push(2, b"b1".to_vec()).unwrap();
        queue.push(1, b"a2".to_vec()).unwrap();
        let in_flight = queue.next().await;
        queue.drop_conn(1);
        // The frame in flight still counts until it is written.
        assert_eq!(queue.queued(), 4);
        drop(in_flight);
        assert_eq!(queue.next().await.bytes, b"b1");
        assert_eq!(queue.queued(), 0);
        assert!(queue.state().by_conn.is_empty());
    }

    #[tokio::test]
    async fn a_closed_queue_drops_what_waits_and_takes_no_more() {
        let queue = InputQueue::default();
        queue.push(1, vec![0; 1000]).unwrap();
        queue.push(2, vec![0; 1000]).unwrap();
        queue.close();
        assert_eq!(queue.queued(), 0);
        assert_eq!(queue.push(1, vec![0; 1]), Err(Refused::Closed));
    }
}
