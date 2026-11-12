//! Whole sequences only (`terminal-holder.md` 3.4.1).
//!
//! The model is fed only whole escape sequences and whole UTF-8 characters.
//! The scanner follows the escape grammar over each read and finds the last
//! point where it is back in the ground state. The bytes after that point
//! are held back and fed with the next read, so the model's parser is in its
//! ground state between reads, and a live upgrade can hand the held tail
//! over as plain bytes.
//!
//! A sequence longer than [`HELD_MAX`] is the one exception: it is fed as it
//! comes, and the scanner keeps only its introducer (`ESC ]` and so on).
//! [`Scanner::held`] then gives that introducer, so a new reader of the
//! stream takes the rest of the sequence as a sequence and not as text.

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
    /// The tail since the last ground point. Once the open sequence passed
    /// [`HELD_MAX`] and was fed, only its introducer.
    held: Vec<u8>,
    /// The open sequence's first bytes: `ESC ]`, `ESC [`, `ESC P` and so on.
    intro: Vec<u8>,
    /// The open sequence passed [`HELD_MAX`] and was fed. The rest of it is
    /// fed as it comes, up to its end.
    overflowed: bool,
}

/// How many of a sequence's first bytes make its introducer.
const INTRO_LEN: usize = 2;

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
            let was = self.state;
            self.state = step(was, byte);
            self.track_intro(was, byte);
            if self.state == State::Ground {
                ground_end = Some(at + 1);
            }
        }
        // After an overflow, `held` is the introducer the model has had.
        let mut feed = if self.overflowed {
            self.held.clear();
            Vec::new()
        } else {
            std::mem::take(&mut self.held)
        };
        let Some(end) = ground_end else {
            feed.extend_from_slice(input);
            if self.overflowed || feed.len() > HELD_MAX {
                self.overflow();
                return feed;
            }
            self.held = feed;
            return Vec::new();
        };
        self.overflowed = false;
        feed.extend_from_slice(&input[..end]);
        if input.len() - end > HELD_MAX {
            feed.extend_from_slice(&input[end..]);
            self.overflow();
        } else {
            self.held.extend_from_slice(&input[end..]);
        }
        feed
    }

    /// The open sequence was fed whole: only its introducer is held, for a
    /// reader that joins now.
    fn overflow(&mut self) {
        self.overflowed = true;
        self.held.clone_from(&self.intro);
    }

    /// Keeps the open sequence's introducer up to date after `byte`.
    fn track_intro(&mut self, was: State, byte: u8) {
        if self.state == State::Ground {
            self.intro.clear();
        } else if was == State::Ground || (byte == ESC && self.state == State::Esc) {
            // A new sequence starts with this byte.
            self.intro.clear();
            self.intro.push(byte);
        } else if was == State::StrEsc && self.state != State::Esc {
            // A string ended with `ESC`, and that `ESC` started this one.
            self.intro.clear();
            self.intro.extend_from_slice(&[ESC, byte]);
        } else if self.intro.len() < INTRO_LEN && self.intro.first() == Some(&ESC) {
            self.intro.push(byte);
        }
    }

    /// The bytes a new reader of the stream needs before the next read: a
    /// cut sequence the next read ends, or the introducer of a long one.
    pub fn held(&self) -> &[u8] {
        &self.held
    }

    /// Whether the open sequence passed [`HELD_MAX`]: the next read goes on
    /// with it, and does not start in the ground state.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Everything the scanner knows, for a live upgrade.
    pub fn export(&self) -> ScannerState {
        ScannerState {
            state: self.state.code(),
            held: self.held.clone(),
            intro: self.intro.clone(),
            overflowed: self.overflowed,
        }
    }

    /// A scanner from [`Scanner::export`]. `None` when the state is not one
    /// this scanner can be in.
    pub fn import(state: ScannerState) -> Option<Self> {
        let scanner = Self {
            state: State::from_code(state.state)?,
            held: state.held,
            intro: state.intro,
            overflowed: state.overflowed,
        };
        let held_max = if scanner.overflowed {
            INTRO_LEN
        } else {
            HELD_MAX
        };
        (scanner.held.len() <= held_max && scanner.intro.len() <= INTRO_LEN).then_some(scanner)
    }
}

/// A scanner's state, as a live upgrade carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScannerState {
    pub state: u8,
    pub held: Vec<u8>,
    pub intro: Vec<u8>,
    pub overflowed: bool,
}

impl State {
    fn code(self) -> u8 {
        match self {
            Self::Ground => 0,
            Self::Utf8(left) => left,
            Self::Esc => 4,
            Self::EscIntermediate => 5,
            Self::Csi => 6,
            Self::Osc => 7,
            Self::Str => 8,
            Self::StrEsc => 9,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Ground,
            1..=3 => Self::Utf8(code),
            4 => Self::Esc,
            5 => Self::EscIntermediate,
            6 => Self::Csi,
            7 => Self::Osc,
            8 => Self::Str,
            9 => Self::StrEsc,
            _ => return None,
        })
    }
}

/// Whether `bytes`, read from the ground state, end in it.
pub fn ends_in_ground(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .fold(State::Ground, |state, &byte| step(state, byte))
        == State::Ground
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
    fn a_held_tail_over_the_limit_is_fed_as_it_is_and_keeps_its_introducer() {
        let mut scanner = Scanner::new();
        assert_eq!(scanner.split(b"ab\x1b]52;c;"), b"ab");
        let long = vec![b'a'; HELD_MAX];
        let fed = scanner.split(&long);
        assert_eq!(fed.len(), HELD_MAX + 7);
        assert!(fed.starts_with(b"\x1b]52;c;"));
        // A reader that joins now takes the rest as part of an OSC.
        assert_eq!(scanner.held(), b"\x1b]");
        assert_eq!(scanner.split(b"aa"), b"aa");
        assert_eq!(scanner.held(), b"\x1b]");
        // The string's end comes through whole, and nothing is held after.
        assert_eq!(scanner.split(b"\x07z"), b"\x07z");
        assert!(scanner.held().is_empty());
    }

    #[test]
    fn a_long_tail_after_a_ground_point_keeps_its_introducer() {
        let mut scanner = Scanner::new();
        let mut input = b"x\x1bPq".to_vec();
        input.extend(vec![b'#'; HELD_MAX + 1]);
        assert_eq!(scanner.split(&input), input);
        assert_eq!(scanner.held(), b"\x1bP");
        assert_eq!(scanner.split(b"##\x1b"), b"##\x1b");
        assert_eq!(scanner.split(b"\\\x1b[1"), b"\\");
        assert_eq!(scanner.held(), b"\x1b[1");
        assert_eq!(scanner.split(b"m"), b"\x1b[1m");
        assert!(scanner.held().is_empty());
    }

    #[test]
    fn an_introducer_names_the_sequence_that_is_open() {
        let mut scanner = Scanner::new();
        // A CSI that an OSC's end started: the CSI's introducer is kept.
        scanner.split(b"\x1b]0;t\x1b[");
        assert_eq!(scanner.intro, b"\x1b[");
        scanner.split(b"m\x1b\x1b]");
        assert_eq!(scanner.intro, b"\x1b]");
        scanner.split(b"\x07\xe6");
        assert_eq!(scanner.intro, b"\xe6");
    }

    #[test]
    fn an_exported_scanner_goes_on_where_it_stopped() {
        let cuts: [&[u8]; 4] = [
            b"ab\x1b]0;ti",
            "\u{2500}".as_bytes().split_at(1).0,
            b"\x1b[3",
            b"\x1bP",
        ];
        for cut in cuts {
            let mut whole = Scanner::new();
            let mut fed = whole.split(cut);
            let mut moved = Scanner::import(whole.export()).unwrap();
            assert_eq!(moved.export(), whole.export());
            fed.extend(moved.split(b"tle\x1b\\z"));
            assert_eq!(fed, [cut, b"tle\x1b\\z"].concat());
        }
        let mut long = Scanner::new();
        long.split(b"\x1b]0;");
        long.split(&vec![b'x'; HELD_MAX + 1]);
        let moved = Scanner::import(long.export()).unwrap();
        assert_eq!(moved.held(), b"\x1b]");
        assert!(
            Scanner::import(ScannerState {
                state: 10,
                ..ScannerState::default()
            })
            .is_none()
        );
        let too_long = ScannerState {
            held: vec![0; HELD_MAX + 1],
            ..ScannerState::default()
        };
        assert!(Scanner::import(too_long).is_none());
    }
}
