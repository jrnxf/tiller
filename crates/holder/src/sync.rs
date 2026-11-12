//! Sync buffering for a subscriber that asks `sync: "buffer"`
//! (`terminal-holder.md` 2.7).
//!
//! An app wraps a frame in mode 2026's start and end markers so a terminal
//! draws it at once. The board's xterm.js may not honor them, so for such
//! a subscriber the holder holds the bytes from the start marker to the end
//! marker and sends them as one frame: at the end marker, after
//! [`HOLD_FOR`], or at [`HOLD_MAX`] bytes, whichever comes first. The
//! markers stay in the stream.

use tokio::time::{Duration, Instant};

/// The longest a subscriber's frame is held.
pub const HOLD_FOR: Duration = Duration::from_millis(150);

/// The most bytes a subscriber's frame holds. Past it, what is held goes
/// out, and the rest of the frame passes as it comes.
pub const HOLD_MAX: usize = 1024 * 1024;

/// Both markers start with this; `h` ends the start one, `l` the end one.
const MARK: &[u8] = b"\x1b[?2026";

/// The length of a whole marker.
const MARK_LEN: usize = MARK.len() + 1;

/// A marker found in a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// A start marker begins at this offset, or before the read when it
    /// came cut.
    Start(usize),
    /// An end marker ends just before this offset.
    End(usize),
}

/// Finds the markers in a pane's output, with a marker cut between reads.
#[derive(Debug, Default)]
pub struct Marks {
    /// How much of [`MARK`] the last bytes matched.
    matched: usize,
}

impl Marks {
    /// The markers in `bytes`, in order.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<Mark> {
        let mut marks = Vec::new();
        if self.matched == 0 && !bytes.contains(&0x1b) {
            return marks;
        }
        for (at, &byte) in bytes.iter().enumerate() {
            self.matched = if self.matched == MARK.len() {
                match byte {
                    b'h' => marks.push(Mark::Start((at + 1).saturating_sub(MARK_LEN))),
                    b'l' => marks.push(Mark::End(at + 1)),
                    _ => {}
                }
                usize::from(byte == 0x1b)
            } else if byte == MARK[self.matched] {
                self.matched + 1
            } else {
                usize::from(byte == 0x1b)
            };
        }
        marks
    }

    /// Forgets a cut marker: no subscriber buffers any more.
    pub fn reset(&mut self) {
        self.matched = 0;
    }
}

/// One subscriber's held frame.
#[derive(Debug, Default)]
pub struct Hold {
    bytes: Vec<u8>,
    /// When it must go out, while one is held.
    until: Option<Instant>,
}

impl Hold {
    /// Takes one read, with its `marks`, and gives what goes out now.
    pub fn take(&mut self, bytes: &[u8], marks: &[Mark], now: Instant) -> Vec<u8> {
        let mut out = Vec::new();
        let mut from = 0;
        for mark in marks {
            let to = match *mark {
                Mark::Start(at) | Mark::End(at) => at,
            };
            self.add(&bytes[from..to], &mut out);
            from = to;
            match mark {
                Mark::Start(_) if self.until.is_none() => self.until = Some(now + HOLD_FOR),
                Mark::Start(_) => {}
                Mark::End(_) => self.release(&mut out),
            }
        }
        self.add(&bytes[from..], &mut out);
        out
    }

    fn add(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        if self.until.is_none() {
            out.extend_from_slice(bytes);
            return;
        }
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() >= HOLD_MAX {
            self.release(out);
        }
    }

    fn release(&mut self, out: &mut Vec<u8>) {
        out.append(&mut self.bytes);
        self.until = None;
    }

    /// When the held frame must go out, if one is held.
    pub fn until(&self) -> Option<Instant> {
        self.until
    }

    /// What is held, at its deadline or before an `exited`.
    pub fn flush(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.release(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `reads` through one subscriber's hold, and gives what went out
    /// at each read.
    fn run(reads: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut marks = Marks::default();
        let mut hold = Hold::default();
        let now = Instant::now();
        reads
            .iter()
            .map(|read| hold.take(read, &marks.scan(read), now))
            .collect()
    }

    #[test]
    fn a_frame_goes_out_whole_at_its_end_marker_with_its_markers() {
        let out = run(&[b"a\x1b[?2026hb", b"c", b"d\x1b[?2026le"]);
        assert_eq!(
            out,
            [
                b"a".to_vec(),
                Vec::new(),
                b"\x1b[?2026hbcd\x1b[?2026le".to_vec()
            ]
        );
    }

    #[test]
    fn a_marker_cut_between_reads_is_found() {
        let out = run(&[b"a\x1b[?20", b"26hb", b"\x1b[?", b"2026l"]);
        // The start's first part went out before it was known.
        assert_eq!(
            out,
            [
                b"a\x1b[?20".to_vec(),
                Vec::new(),
                Vec::new(),
                b"26hb\x1b[?2026l".to_vec()
            ]
        );
    }

    #[test]
    fn other_modes_and_text_pass_as_they_come() {
        let out = run(&[b"\x1b[?2027h\x1b[?2026x\x1b\x1b[?2004h plain"]);
        assert_eq!(
            out,
            [b"\x1b[?2027h\x1b[?2026x\x1b\x1b[?2004h plain".to_vec()]
        );
    }

    #[test]
    fn a_frame_past_the_byte_limit_goes_out_and_the_rest_passes() {
        let mut marks = Marks::default();
        let mut hold = Hold::default();
        let now = Instant::now();
        let start = b"\x1b[?2026h";
        assert!(hold.take(start, &marks.scan(start), now).is_empty());
        let big = vec![b'x'; HOLD_MAX];
        let out = hold.take(&big, &marks.scan(&big), now);
        assert_eq!(out.len(), start.len() + HOLD_MAX);
        assert_eq!(hold.until(), None);
        assert_eq!(hold.take(b"more", &marks.scan(b"more"), now), b"more");
    }

    #[test]
    fn a_held_frame_has_a_deadline_and_flushes() {
        let mut marks = Marks::default();
        let mut hold = Hold::default();
        let now = Instant::now();
        let read = b"\x1b[?2026hpart";
        assert!(hold.take(read, &marks.scan(read), now).is_empty());
        assert_eq!(hold.until(), Some(now + HOLD_FOR));
        assert_eq!(hold.flush(), read);
        assert_eq!(hold.until(), None);
        assert!(hold.flush().is_empty());
    }
}
