//! Whole sequences only (`terminal-holder.md` 3.4.1).
//!
//! The model is fed only whole escape sequences and whole UTF-8 characters.
//! The scanner follows the escape grammar over each read and finds the last
//! point where it is back in the ground state. The bytes after that point
//! are held back and fed with the next read, so the model's parser is in its
//! ground state between reads, and a live upgrade can hand the held tail
//! over as plain bytes.

/// A held tail over this size is fed as it is.
pub const HELD_MAX: usize = 64 * 1024;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// Where the scanner is in the escape grammar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// Inside a UTF-8 character, with this many continuation bytes to come.
    Utf8(u8),
    /// After `ESC`.
    Esc,
    /// After `ESC` and one or more intermediate bytes.
    EscIntermediate,
    /// Inside `CSI`, before its final byte.
    Csi,
    /// Inside an OSC string, which `BEL` or `ESC \` ends.
    Osc,
    /// Inside a DCS, APC, PM or SOS string, which `ESC \` ends.
    Str,
    /// After an `ESC` inside a string: `\` ends the string, and anything
    /// else ends it and starts a new escape.
    StrEsc,
}

/// Splits a pane's output into what the model may take now and the tail it
/// holds back.
#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
    held: Vec<u8>,
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the next read. Returns the bytes to feed the model now: the
    /// tail held from before, and `input` up to its last ground point. The
    /// rest is held for the next call. A held tail over [`HELD_MAX`] is
    /// given back whole.
    pub fn split(&mut self, input: &[u8]) -> Vec<u8> {
        let mut ground_end = None;
        for (at, &byte) in input.iter().enumerate() {
            self.state = step(self.state, byte);
            if self.state == State::Ground {
                ground_end = Some(at + 1);
            }
        }
        let mut feed = std::mem::take(&mut self.held);
        match ground_end {
            Some(end) => {
                feed.extend_from_slice(&input[..end]);
                self.held.extend_from_slice(&input[end..]);
            }
            None => feed.extend_from_slice(input),
        }
        if ground_end.is_none() {
            if feed.len() > HELD_MAX {
                return feed;
            }
            self.held = feed;
            return Vec::new();
        }
        if self.held.len() > HELD_MAX {
            feed.append(&mut self.held);
        }
        feed
    }

    /// The bytes held back now: a cut sequence the next read ends.
    pub fn held(&self) -> &[u8] {
        &self.held
    }
}

/// The state after `byte`.
fn step(state: State, byte: u8) -> State {
    match state {
        State::Ground => ground(byte),
        State::Utf8(left) => match byte {
            0x80..=0xbf if left > 1 => State::Utf8(left - 1),
            0x80..=0xbf => State::Ground,
            // A character cut short: the parser drops it and reads this byte
            // afresh.
            _ => ground(byte),
        },
        State::Esc => match byte {
            b'[' => State::Csi,
            b']' => State::Osc,
            b'P' | b'X' | b'^' | b'_' => State::Str,
            0x20..=0x2f => State::EscIntermediate,
            ESC => State::Esc,
            CAN | SUB => State::Ground,
            // C0 controls run inside an escape and leave it open; DEL is
            // ignored.
            0x00..=0x1f | 0x7f => State::Esc,
            _ => State::Ground,
        },
        State::EscIntermediate => match byte {
            0x20..=0x2f | 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => State::EscIntermediate,
            ESC => State::Esc,
            _ => State::Ground,
        },
        State::Csi => match byte {
            0x40..=0x7e | CAN | SUB => State::Ground,
            ESC => State::Esc,
            _ => State::Csi,
        },
        State::Osc => match byte {
            BEL | CAN | SUB => State::Ground,
            ESC => State::StrEsc,
            _ => State::Osc,
        },
        State::Str => match byte {
            CAN | SUB => State::Ground,
            ESC => State::StrEsc,
            _ => State::Str,
        },
        State::StrEsc => match byte {
            b'\\' => State::Ground,
            other => step(State::Esc, other),
        },
    }
}

fn ground(byte: u8) -> State {
    match byte {
        ESC => State::Esc,
        0xc2..=0xdf => State::Utf8(1),
        0xe0..=0xef => State::Utf8(2),
        0xf0..=0xf4 => State::Utf8(3),
        _ => State::Ground,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `input` in two reads, cut at every point, and checks that each
    /// feed ends on a whole sequence and that nothing is lost.
    fn every_cut(input: &[u8]) {
        for cut in 0..=input.len() {
            let mut scanner = Scanner::new();
            let mut fed = scanner.split(&input[..cut]);
            let first = fed.len();
            assert!(first <= cut, "cut {cut}: fed {first}");
            fed.extend(scanner.split(&input[cut..]));
            assert_eq!(fed, input, "cut {cut}");
            assert!(scanner.held().is_empty(), "cut {cut}");
        }
    }

    #[test]
    fn whole_input_passes_through_at_every_cut() {
        every_cut(b"plain \x1b[1;31mred\x1b[0m \x1b]0;title\x07 \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\");
        every_cut("wide 日本 and é and 🦀\x1b[?2004h\x1bPq#0\x1b\\\x1b_apc\x1b\\done".as_bytes());
        every_cut(b"\x1b(B\x1b#8\x1b7\x1b8\x1b[>4;1m\x1b[ q");
    }

    #[test]
    fn a_read_that_ends_inside_a_sequence_holds_its_tail() {
        let cases: &[(&[u8], &[u8])] = &[
            (b"ab\x1b", b"\x1b"),
            (b"ab\x1b[", b"\x1b["),
            (b"ab\x1b[1;3", b"\x1b[1;3"),
            (b"ab\x1b]0;tit", b"\x1b]0;tit"),
            (b"ab\x1b]0;title\x1b", b"\x1b]0;title\x1b"),
            (b"ab\x1bPq#0", b"\x1bPq#0"),
            (b"ab\x1b(", b"\x1b("),
            (b"ab\xe6\x97", b"\xe6\x97"),
            (b"ab\xf0\x9f\xa6", b"\xf0\x9f\xa6"),
        ];
        for (input, tail) in cases {
            let mut scanner = Scanner::new();
            assert_eq!(scanner.split(input), b"ab", "{input:?}");
            assert_eq!(scanner.held(), *tail, "{input:?}");
        }
    }

    #[test]
    fn can_and_sub_end_a_sequence() {
        let mut scanner = Scanner::new();
        assert_eq!(scanner.split(b"\x1b[12\x18x"), b"\x1b[12\x18x");
        assert_eq!(scanner.split(b"\x1b]0;t\x1ay"), b"\x1b]0;t\x1ay");
        assert!(scanner.held().is_empty());
    }

    #[test]
    fn a_broken_utf8_character_does_not_hold_what_follows() {
        let mut scanner = Scanner::new();
        assert_eq!(scanner.split(b"\xe6x"), b"\xe6x");
        assert!(scanner.held().is_empty());
    }

    #[test]
    fn a_held_tail_over_the_limit_is_fed_as_it_is() {
        let mut scanner = Scanner::new();
        assert!(scanner.split(b"\x1b]0;").is_empty());
        let long = vec![b'a'; HELD_MAX];
        let fed = scanner.split(&long);
        assert_eq!(fed.len(), HELD_MAX + 4);
        assert!(scanner.held().is_empty());
        // The string is still open, and its end comes through whole.
        assert_eq!(scanner.split(b"\x07z"), b"\x07z");
    }
}
