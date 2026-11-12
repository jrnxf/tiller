//! Raw input on its way to a pane's PTY, and the caps on it.
//!
//! A kind 2 frame has no reply, and the holder never waits for a pane's app
//! to read, as that would hold up every other request on the connection.
//! So input waits in a queue per pane, with two caps on it. Each client's
//! own unwritten bytes for the pane have [`INPUT_CAP`], so a client is not
//! cut for what other clients sent. All the pane's unwritten bytes have
//! [`PANE_INPUT_CAP`], so clients that come back again and again cannot
//! grow it. A frame past either cap cuts the client that sent it, so a gap
//! in its input is never silent. H04 puts back-pressure in place of this.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The most raw input bytes of one client that may wait for a pane's app
/// to read them. A frame is taken whole when none of that client's input to
/// the pane waits.
pub const INPUT_CAP: usize = 1024 * 1024;

/// The most raw input bytes of all clients that may wait for a pane's app
/// to read them. A frame is taken whole when no input to the pane waits.
pub const PANE_INPUT_CAP: usize = 4 * INPUT_CAP;

/// A pane's unwritten input, in all and for each client.
#[derive(Debug, Default)]
pub struct InputQueue {
    counts: Mutex<Counts>,
}

#[derive(Debug, Default)]
struct Counts {
    total: usize,
    /// Only clients with input waiting: a count that falls to 0 goes.
    by_conn: HashMap<u64, usize>,
}

/// Raw input that counts toward its pane's queue until it drops: once it is
/// written, or when the PTY closes first.
#[derive(Debug)]
pub struct Chunk {
    pub bytes: Vec<u8>,
    queue: Arc<InputQueue>,
    conn: u64,
}

impl Drop for Chunk {
    fn drop(&mut self) {
        self.queue.give_back(self.conn, self.bytes.len());
    }
}

impl InputQueue {
    fn counts(&self) -> MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts `bytes` from the client `conn`, or `None` when they would pass
    /// a cap. The caller cuts the client then.
    pub fn take(self: &Arc<Self>, conn: u64, bytes: Vec<u8>) -> Option<Chunk> {
        let len = bytes.len();
        let mut counts = self.counts();
        let mine = counts.by_conn.get(&conn).copied().unwrap_or(0);
        let fits = |queued: usize, cap: usize| queued == 0 || queued + len <= cap;
        if !(fits(mine, INPUT_CAP) && fits(counts.total, PANE_INPUT_CAP)) {
            return None;
        }
        counts.total += len;
        counts.by_conn.insert(conn, mine + len);
        drop(counts);
        Some(Chunk {
            bytes,
            queue: self.clone(),
            conn,
        })
    }

    fn give_back(&self, conn: u64, len: usize) {
        let mut counts = self.counts();
        counts.total = counts.total.saturating_sub(len);
        if let Some(mine) = counts.by_conn.get_mut(&conn) {
            *mine = mine.saturating_sub(len);
            if *mine == 0 {
                counts.by_conn.remove(&conn);
            }
        }
    }

    /// Bytes waiting in all.
    #[cfg(test)]
    pub fn queued(&self) -> usize {
        self.counts().total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_past_its_cap_is_refused_and_another_is_not() {
        let queue = Arc::new(InputQueue::default());
        let first = queue.take(1, vec![0; INPUT_CAP]).unwrap();
        assert!(queue.take(1, vec![0; 1]).is_none());
        let other = queue.take(2, vec![0; 1]).unwrap();
        assert_eq!(queue.queued(), INPUT_CAP + 1);
        drop(first);
        // Written input makes room again.
        let again = queue.take(1, vec![0; 10]).unwrap();
        drop((other, again));
        assert_eq!(queue.queued(), 0);
        assert!(queue.counts().by_conn.is_empty());
    }

    #[test]
    fn a_frame_is_taken_whole_when_none_waits_and_the_pane_has_a_cap_of_its_own() {
        let queue = Arc::new(InputQueue::default());
        let big = queue.take(1, vec![0; INPUT_CAP * 2]).unwrap();
        let chunks: Vec<Chunk> = (2..=3)
            .map(|conn| queue.take(conn, vec![0; INPUT_CAP]).unwrap())
            .collect();
        assert_eq!(queue.queued(), PANE_INPUT_CAP);
        // A client with nothing waiting is still refused at the pane's cap.
        assert!(queue.take(4, vec![0; 1]).is_none());
        drop(big);
        assert!(queue.take(4, vec![0; 1]).is_some());
        drop(chunks);
    }

    #[test]
    fn chunks_dropped_unwritten_give_their_bytes_back() {
        let queue = Arc::new(InputQueue::default());
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..4 {
            tx.send(queue.take(1, vec![0; 1000]).unwrap()).unwrap();
        }
        assert_eq!(queue.queued(), 4000);
        // A closed PTY drops what is still queued for it.
        drop((tx, rx));
        assert_eq!(queue.queued(), 0);
        assert!(queue.counts().by_conn.is_empty());
    }
}
