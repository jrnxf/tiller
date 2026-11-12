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
//! - A cut client's frames that wait are dropped, so a client cut again
//!   and again does not grow the queue. A frame being written is written
//!   to its end, so no other client's input follows half a sequence. A
//!   client that closes, or breaks off, keeps the input it sent, which is
//!   written as the app reads: the holder had all of it.
//!
//! This is the cap of `terminal-holder.md` 2.8 on input the app left
//! unread. Output has its own queues ([`crate::outbox`]).

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
    /// Set once the client is cut: its waiting input is dropped.
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
    /// The client was cut. Each queue drops its waiting input as it comes
    /// to it, or at [`InputQueue::drop_cut`].
    pub fn cut(&self) {
        self.cut.store(true, Ordering::Release);
    }

    pub fn is_cut(&self) -> bool {
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
    /// A write to the PTY failed while it was open: input to the pane has
    /// a gap, and none is taken after it.
    Broken,
}

/// A pane's unwritten input.
#[derive(Debug, Default)]
pub struct InputQueue {
    state: Mutex<State>,
    wake: Notify,
}

#[derive(Debug, Default)]
struct State {
    /// The rest of a frame the writer wrote part of and put back: it goes
    /// first, and to its end, even when its client is cut since.
    partial: Option<(Arc<Sender>, Vec<u8>)>,
    /// Each frame not yet taken by the writer, and its client.
    chunks: VecDeque<(Arc<Sender>, Vec<u8>)>,
    /// Each client's unwritten bytes, in `chunks` and in the writer's
    /// hands. Only clients with input waiting: a count that falls to 0
    /// goes.
    by_sender: HashMap<u64, usize>,
    /// The PTY closed or broke: no more is taken.
    closed: Option<Refused>,
}

fn give_back(by_sender: &mut HashMap<u64, usize>, sender: u64, len: usize) {
    if let Some(mine) = by_sender.get_mut(&sender) {
        *mine = mine.saturating_sub(len);
        if *mine == 0 {
            by_sender.remove(&sender);
        }
    }
}

impl State {
    fn give_back(&mut self, sender: u64, len: usize) {
        give_back(&mut self.by_sender, sender, len);
    }

    /// Drops the waiting input of every cut client.
    fn drop_cut(&mut self) {
        let Self {
            chunks, by_sender, ..
        } = self;
        chunks.retain(|(sender, bytes)| {
            if !sender.is_cut() {
                return true;
            }
            give_back(by_sender, sender.id, bytes.len());
            false
        });
    }
}

/// A frame the writer took. Its unwritten part counts toward its client's
/// cap until it is written or dropped.
#[derive(Debug)]
pub struct InFlight<'a> {
    bytes: Vec<u8>,
    written: usize,
    /// Part of the frame was written before it was taken this time.
    started: bool,
    queue: &'a InputQueue,
    sender: Arc<Sender>,
}

impl InFlight<'_> {
    /// The bytes still to write.
    pub fn rest(&self) -> &[u8] {
        self.bytes.get(self.written..).unwrap_or_default()
    }

    /// `len` more bytes were written.
    pub fn wrote(&mut self, len: usize) {
        let len = len.min(self.bytes.len() - self.written);
        self.written += len;
        self.queue.state().give_back(self.sender.id, len);
    }
}

impl InFlight<'_> {
    /// Puts the unwritten part back at the front of the queue, still its
    /// client's, for the writer's next turn: the writer holds no frame
    /// while it waits, so the live upgrade's handover (section 6.2) takes
    /// the queue whole. A frame written in part is written to its end.
    pub fn put_back(mut self) {
        let started = self.started || self.written > 0;
        let rest = self.bytes.split_off(self.written);
        if rest.is_empty() {
            return;
        }
        let mut state = self.queue.state();
        if state.closed.is_some() {
            state.give_back(self.sender.id, rest.len());
        } else if started {
            state.partial = Some((self.sender.clone(), rest));
        } else {
            state.chunks.push_front((self.sender.clone(), rest));
        }
        drop(state);
        // Counted in the queue now, not in the writer's hands.
        self.written = self.bytes.len();
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let unwritten = self.bytes.len() - self.written;
        self.queue.state().give_back(self.sender.id, unwritten);
    }
}

/// A queue held still: nothing is pushed or taken while it lives.
#[derive(Debug)]
pub struct Held<'a>(MutexGuard<'a, State>);

impl Held<'_> {
    /// What waits, for the handover, in the order it is written. A cut
    /// client's input is left out: it would be dropped, not written. A
    /// frame written in part is kept, as it is written to its end.
    pub fn queued(&self) -> Queued {
        let state = &self.0;
        Queued {
            chunks: state
                .partial
                .iter()
                .map(|(_, bytes)| bytes.clone())
                .chain(
                    state
                        .chunks
                        .iter()
                        .filter(|(sender, _)| !sender.is_cut())
                        .map(|(_, bytes)| bytes.clone()),
                )
                .collect(),
            closed: state.closed,
        }
    }
}

/// A queue's state for the live upgrade's handover (section 6.2): what
/// waits, in order, with the clients it came from left out, as every
/// connection ends at the upgrade.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Queued {
    /// The unwritten frames, in the order they are written.
    pub chunks: Vec<Vec<u8>>,
    /// The queue takes no more input.
    pub closed: Option<Refused>,
}

impl InputQueue {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `bytes` from `sender`. A frame is taken whole while the
    /// sender's unwritten input is under [`INPUT_CAP`].
    pub fn push(&self, sender: &Arc<Sender>, bytes: Vec<u8>) -> Result<(), Refused> {
        let mut state = self.state();
        if let Some(refused) = state.closed {
            return Err(refused);
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
        self.close_as(Refused::Closed);
    }

    /// Takes no more input and drops what waits: a write to the PTY failed.
    pub fn break_off(&self) {
        self.close_as(Refused::Broken);
    }

    fn close_as(&self, refused: Refused) {
        let mut state = self.state();
        state.closed.get_or_insert(refused);
        let chunks = std::mem::take(&mut state.chunks);
        for (sender, bytes) in state.partial.take().into_iter().chain(chunks) {
            state.give_back(sender.id, bytes.len());
        }
    }

    /// The next frame to write, in the order frames came, or `None` when
    /// none waits. A cut client's frames are dropped, not written.
    pub fn try_next(&self) -> Option<InFlight<'_>> {
        let mut state = self.state();
        let started = state.partial.is_some();
        let (sender, bytes) = match state.partial.take() {
            Some(partial) => partial,
            None => loop {
                match state.chunks.pop_front()? {
                    (sender, bytes) if sender.is_cut() => state.give_back(sender.id, bytes.len()),
                    next => break next,
                }
            },
        };
        Some(InFlight {
            bytes,
            written: 0,
            started,
            queue: self,
            sender,
        })
    }

    /// Waits until a frame waits. It may be a cut client's, which
    /// [`InputQueue::try_next`] drops.
    pub async fn ready(&self) {
        loop {
            let wake = self.wake.notified();
            {
                let state = self.state();
                if state.partial.is_some() || !state.chunks.is_empty() {
                    return;
                }
            }
            wake.await;
        }
    }

    /// The next frame to write, once one waits.
    #[cfg(test)]
    async fn next(&self) -> InFlight<'_> {
        loop {
            self.ready().await;
            if let Some(next) = self.try_next() {
                return next;
            }
        }
    }

    /// Holds the queue still, for the handover: a push waits until it is
    /// let go. Taken under the pane's lock, as a push under it is.
    pub fn hold(&self) -> Held<'_> {
        Held(self.state())
    }

    /// What waits, for the handover.
    #[cfg(test)]
    fn queued(&self) -> Queued {
        self.hold().queued()
    }

    /// A queue that holds `queued`, as the new image takes it over. It all
    /// counts as one client's, the handover's, which sends no more.
    pub fn resumed(queued: Queued) -> Self {
        let sender = Arc::new(Sender::default());
        let mut state = State {
            closed: queued.closed,
            ..State::default()
        };
        for bytes in queued.chunks.into_iter().filter(|bytes| !bytes.is_empty()) {
            *state.by_sender.entry(sender.id).or_default() += bytes.len();
            state.chunks.push_back((sender.clone(), bytes));
        }
        Self {
            state: Mutex::new(state),
            wake: Notify::new(),
        }
    }

    /// Bytes waiting in all, in the queue and in the writer's hands.
    #[cfg(test)]
    fn unwritten(&self) -> usize {
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
        assert_eq!(queue.unwritten(), 10 + 1 + 1);
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
        assert_eq!(queue.unwritten(), 100 + 3);
        queue.push(&one, b"key".to_vec()).unwrap();
        assert_eq!(first.rest(), [1; 100]);
        first.wrote(100);
        assert!(first.rest().is_empty());
        drop(first);
        assert_eq!(queue.next().await.rest(), b"two");
        assert_eq!(queue.next().await.rest(), b"key");
        assert_eq!(queue.unwritten(), 0);
        assert!(queue.state().by_sender.is_empty());
        // An empty frame is nothing to write.
        queue.push(&one, Vec::new()).unwrap();
        assert!(queue.state().chunks.is_empty());
    }

    #[tokio::test]
    async fn a_cut_client_s_waiting_input_goes_and_the_rest_stays() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, b"a1".to_vec()).unwrap();
        queue.push(&two, b"b1".to_vec()).unwrap();
        queue.push(&one, b"a2".to_vec()).unwrap();
        queue.push(&one, b"a3".to_vec()).unwrap();
        let in_flight = queue.next().await;
        one.cut();
        // The frame being written is written to its end.
        assert_eq!(in_flight.rest(), b"a1");
        queue.drop_cut();
        // The frame in flight counts until the writer lets it go.
        assert_eq!(queue.unwritten(), 4);
        drop(in_flight);
        assert_eq!(queue.next().await.rest(), b"b1");
        assert_eq!(queue.unwritten(), 0);
        assert!(queue.state().by_sender.is_empty());
        // Frames queued after the sweep are dropped as the writer comes
        // to them.
        let three = Arc::new(Sender::default());
        queue.push(&three, b"c1".to_vec()).unwrap();
        queue.push(&two, b"b2".to_vec()).unwrap();
        three.cut();
        assert_eq!(queue.next().await.rest(), b"b2");
        assert_eq!(queue.unwritten(), 0);
    }

    #[tokio::test]
    async fn a_closed_queue_drops_what_waits_and_takes_no_more() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, vec![0; 1000]).unwrap();
        queue.push(&two, vec![0; 1000]).unwrap();
        queue.close();
        assert_eq!(queue.unwritten(), 0);
        assert_eq!(queue.push(&one, vec![0; 1]), Err(Refused::Closed));
        let broken = InputQueue::default();
        broken.break_off();
        broken.close();
        assert_eq!(broken.push(&one, vec![0; 1]), Err(Refused::Broken));
    }

    #[tokio::test]
    async fn a_paused_writers_frame_goes_back_first_and_the_handover_keeps_the_order() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, b"half".to_vec()).unwrap();
        queue.push(&two, b"next".to_vec()).unwrap();
        let cut = Arc::new(Sender::default());
        queue.push(&cut, b"gone".to_vec()).unwrap();
        cut.cut();
        let mut in_flight = queue.next().await;
        in_flight.wrote(2);
        in_flight.put_back();
        // The rest still counts as the client's.
        assert_eq!(queue.state().by_sender.get(&one.id), Some(&2));
        let queued = queue.queued();
        assert_eq!(
            queued,
            Queued {
                chunks: vec![b"lf".to_vec(), b"next".to_vec()],
                closed: None,
            }
        );
        let resumed = InputQueue::resumed(queued);
        assert_eq!(resumed.unwritten(), 6);
        assert_eq!(resumed.next().await.rest(), b"lf");
        assert_eq!(resumed.next().await.rest(), b"next");
        assert_eq!(resumed.unwritten(), 0);

        queue.close();
        let closed = InputQueue::resumed(queue.queued());
        assert_eq!(closed.push(&one, vec![0; 1]), Err(Refused::Closed));
    }

    #[tokio::test]
    async fn a_frame_written_in_part_goes_first_and_to_its_end_after_its_client_is_cut() {
        let queue = InputQueue::default();
        let (one, two) = senders();
        queue.push(&one, b"\x1b[A".to_vec()).unwrap();
        queue.push(&one, b"more".to_vec()).unwrap();
        queue.push(&two, b"two".to_vec()).unwrap();
        let mut first = queue.try_next().unwrap();
        first.wrote(1);
        first.put_back();
        // Taken again and put back with nothing more written, it is still
        // the rest of a frame.
        let again = queue.try_next().unwrap();
        assert_eq!(again.rest(), b"[A");
        again.put_back();
        queue.push(&two, b"three".to_vec()).unwrap();
        one.cut();
        queue.drop_cut();
        assert_eq!(
            queue.queued().chunks,
            [b"[A".to_vec(), b"two".to_vec(), b"three".to_vec()],
            "the cut client's rest of a sequence is kept, its next frame is not"
        );
        assert_eq!(queue.try_next().unwrap().rest(), b"[A");
        // A frame not started goes back as it was.
        let two_frame = queue.try_next().unwrap();
        assert_eq!(two_frame.rest(), b"two");
        two_frame.put_back();
        assert_eq!(queue.try_next().unwrap().rest(), b"two");
        assert_eq!(queue.try_next().unwrap().rest(), b"three");
        assert!(queue.try_next().is_none());
        assert_eq!(queue.unwritten(), 0);
    }
}
