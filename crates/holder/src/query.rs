//! The terminal queries the holder answers (`terminal-holder.md` 2.7).
//!
//! An app asks its terminal things: what it is (DA1, DA2, XTVERSION), where
//! the cursor is (CPR), whether a mode is set (DECRQM), which kitty keyboard
//! flags are on (`CSI ? u`), and its colors (OSC 4, 10, 11 and 12). The
//! holder is that terminal, so it answers each once, on the PTY, and no
//! subscriber sees the query: a subscriber's own terminal would answer it a
//! second time.
//!
//! The model answers what `alacritty_terminal` answers, through
//! [`Listener`]. The holder answers XTVERSION itself, as `alacritty_terminal`
//! does not. [`QueryFilter`] takes every query the model answers out of the
//! bytes that go to subscribers.

use std::sync::{Arc, Mutex, PoisonError};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use alacritty_terminal::vte::{Params, Parser, Perform};

/// The answer to XTVERSION (`CSI > q`).
pub fn xtversion() -> Vec<u8> {
    format!("\x1bP>|tiller {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes()
}

/// The size of one cell in pixels, for `CSI 14 t`. The holder draws no
/// pixels, so it answers with a common cell size.
const CELL_WIDTH: u16 = 8;
const CELL_HEIGHT: u16 = 16;

/// The colors a query is answered with when the app set none: the board's
/// theme for the 16 named colors, the foreground, the background and the
/// cursor, and xterm's for the rest of the 256.
const THEME: [u32; 16] = [
    0x1d1f23, 0xef7c7c, 0x5fcf86, 0xf0bf00, 0x7fb6f5, 0xb4a0ff, 0x6fd1d8, 0xc9ced6, 0x6f7680,
    0xf2a0a0, 0x8be0a8, 0xf2a46b, 0xa5cbf8, 0xcdbfff, 0x9fe2e7, 0xe6e9ee,
];
const FOREGROUND: u32 = 0xc9ced6;
const BACKGROUND: u32 = 0x0d0e10;
const CURSOR: u32 = 0xe6e9ee;

fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

/// The color at `index` when the app set none: the 256 indexed colors,
/// then the foreground, the background and the cursor.
pub fn default_color(index: usize) -> Rgb {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..16 => rgb(THEME[index]),
        16..232 => {
            let cube = index - 16;
            Rgb {
                r: STEPS[cube / 36],
                g: STEPS[cube / 6 % 6],
                b: STEPS[cube % 6],
            }
        }
        232..256 => {
            let gray = 8 + 10 * (index - 232) as u8;
            Rgb {
                r: gray,
                g: gray,
                b: gray,
            }
        }
        _ if index == NamedColor::Background as usize => rgb(BACKGROUND),
        _ if index == NamedColor::Cursor as usize => rgb(CURSOR),
        _ => rgb(FOREGROUND),
    }
}

/// Takes the model's events. Only the ones that answer a query are kept;
/// the model tells titles and bells through its own parser.
#[derive(Clone, Debug, Default)]
pub struct Listener(Arc<Mutex<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if matches!(
            event,
            Event::PtyWrite(_) | Event::ColorRequest(..) | Event::TextAreaSizeRequest(_)
        ) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
        }
    }
}

impl Listener {
    /// The answers to the queries the model read since the last call, in
    /// order, as bytes for the PTY. A clipboard read is never answered.
    pub fn answers(&self, colors: &Colors, size: (u16, u16)) -> Vec<u8> {
        let events = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        let mut out = Vec::new();
        for event in events {
            let answer = match event {
                Event::PtyWrite(text) => text,
                Event::ColorRequest(index, format) => {
                    let color = colors[index].unwrap_or_else(|| default_color(index));
                    format(color)
                }
                Event::TextAreaSizeRequest(format) => format(WindowSize {
                    num_lines: size.1,
                    num_cols: size.0,
                    cell_width: CELL_WIDTH,
                    cell_height: CELL_HEIGHT,
                }),
                _ => continue,
            };
            out.extend_from_slice(answer.as_bytes());
        }
        out
    }
}

/// The most bytes of one sequence the filter holds back while it may still
/// be a query. Every query is shorter: a longer sequence passes.
const PENDING_MAX: usize = 64;

/// What the filter's parser saw at the last byte.
#[derive(Debug, Default)]
struct Seen {
    /// A query the holder answers ended.
    query: bool,
    /// That query was XTVERSION.
    xtversion: bool,
}

impl Seen {
    fn query(&mut self) {
        self.query = true;
    }
}

impl Perform for Seen {
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // As `alacritty_terminal` drops them: answered by no one.
        if ignore || intermediates.len() > 2 {
            return;
        }
        let first = params.iter().next().map_or(0, |param| param[0]);
        match (action, intermediates) {
            ('c', [] | [b'>']) if first == 0 => self.query(),
            ('n', []) if first == 5 || first == 6 => self.query(),
            ('p', [b'$'] | [b'?', b'$']) => self.query(),
            ('t', []) if first == 14 || first == 18 => self.query(),
            ('u', [b'?']) => self.query(),
            ('q', [b'>']) if first == 0 => {
                self.query();
                self.xtversion = true;
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if byte == b'Z' && intermediates.is_empty() && !ignore {
            self.query();
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some((&kind, rest)) = params.split_first() else {
            return;
        };
        let asks = match kind {
            b"4" => rest.chunks(2).any(|pair| pair.get(1) == Some(&&b"?"[..])),
            b"10" | b"11" | b"12" => rest.contains(&&b"?"[..]),
            _ => false,
        };
        if asks {
            self.query();
        }
    }
}

/// Whether `pending`, which starts with ESC, may still grow into a query:
/// a CSI with no final byte yet, an OSC of digits, `;` and `?` with no end
/// yet, or the ESC alone.
fn may_be_query(pending: &[u8]) -> bool {
    match pending {
        [] | [0x1b] => true,
        [0x1b, b'[', rest @ ..] => rest.iter().all(|byte| (0x20..0x40).contains(byte)),
        [0x1b, b']', rest @ ..] => rest
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b';' || *byte == b'?'),
        _ => false,
    }
}

/// Takes the queries out of a pane's output for its subscribers.
///
/// It reads the bytes as the model's parser does, and holds back a
/// sequence from its ESC while it may still be a query, up to
/// [`PENDING_MAX`] bytes. A query is dropped whole, with the ST that ends
/// an OSC. Anything else is passed as it came, in order: the filter only
/// takes bytes out. A query the filter did not hold from its ESC passes:
/// one with a control byte inside it, or an OSC that sets colors as well
/// as asking, which the subscriber needs for its colors.
pub struct QueryFilter {
    parser: Parser,
    seen: Seen,
    pending: Vec<u8>,
    /// The ESC in `pending` came right after an OSC query it ended: a `\`
    /// after it is that query's ST.
    after_osc: bool,
}

impl std::fmt::Debug for QueryFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryFilter")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

impl Default for QueryFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// What one read filtered to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Filtered {
    /// The bytes for subscribers.
    pub pass: Vec<u8>,
    /// Where in the read each XTVERSION ended, for its answer to go after
    /// the answers to what came before it.
    pub xtversions: Vec<usize>,
}

impl QueryFilter {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            seen: Seen::default(),
            pending: Vec::new(),
            after_osc: false,
        }
    }

    /// The bytes held back as the start of what may be a query.
    pub fn pending(&self) -> &[u8] {
        &self.pending
    }

    /// The bytes held back, as they are, for the pane's end: no more
    /// output comes to finish them.
    pub fn flush(&mut self) -> Vec<u8> {
        self.parser = Parser::new();
        self.seen = Seen::default();
        self.after_osc = false;
        std::mem::take(&mut self.pending)
    }

    /// Filters one read.
    pub fn filter(&mut self, bytes: &[u8]) -> Filtered {
        let mut out = Filtered {
            pass: Vec::with_capacity(bytes.len()),
            xtversions: Vec::new(),
        };
        let mut at = 0;
        while at < bytes.len() {
            if self.pending.is_empty() {
                // No sequence held: everything up to the next ESC passes.
                let run = bytes[at..]
                    .iter()
                    .position(|byte| *byte == 0x1b)
                    .map_or(bytes.len(), |esc| at + esc);
                if run > at {
                    self.parser.advance(&mut self.seen, &bytes[at..run]);
                    out.pass.extend_from_slice(&bytes[at..run]);
                    self.take_seen(&mut out, run);
                    at = run;
                    continue;
                }
            }
            let byte = bytes[at];
            at += 1;
            self.parser.advance(&mut self.seen, &[byte]);
            let (query, xtversion) = self.take_seen_flags();
            if xtversion {
                out.xtversions.push(at);
            }
            if std::mem::take(&mut self.after_osc) && byte == b'\\' && self.pending == [0x1b] {
                // The ST of a query already dropped.
                self.pending.clear();
                continue;
            }
            if byte == 0x1b {
                // The ESC ends what is held and starts a new sequence.
                if query && !self.pending.is_empty() {
                    self.pending.clear();
                    self.after_osc = true;
                } else {
                    out.pass.append(&mut self.pending);
                }
                self.pending.push(byte);
                continue;
            }
            if query && !self.pending.is_empty() {
                self.pending.clear();
                continue;
            }
            if self.pending.is_empty() {
                out.pass.push(byte);
                continue;
            }
            self.pending.push(byte);
            if !may_be_query(&self.pending) || self.pending.len() > PENDING_MAX {
                out.pass.append(&mut self.pending);
            }
        }
        out
    }

    fn take_seen_flags(&mut self) -> (bool, bool) {
        let seen = std::mem::take(&mut self.seen);
        (seen.query, seen.xtversion)
    }

    /// After a run of bytes that passed: an XTVERSION in it still gets its
    /// answer, though it passes.
    fn take_seen(&mut self, out: &mut Filtered, end: usize) {
        let (_, xtversion) = self.take_seen_flags();
        if xtversion {
            out.xtversions.push(end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(parts: &[&[u8]]) -> Vec<u8> {
        let mut filter = QueryFilter::new();
        let mut out = Vec::new();
        for part in parts {
            out.extend(filter.filter(part).pass);
        }
        out.extend_from_slice(filter.pending());
        out
    }

    #[test]
    fn every_query_is_taken_out_and_the_rest_passes() {
        let queries: &[&[u8]] = &[
            b"\x1b[c",
            b"\x1b[0c",
            b"\x1b[>c",
            b"\x1b[>0c",
            b"\x1b[5n",
            b"\x1b[6n",
            b"\x1b[?2004$p",
            b"\x1b[4$p",
            b"\x1b[?u",
            b"\x1b[>q",
            b"\x1b[>0q",
            b"\x1b[14t",
            b"\x1b[18t",
            b"\x1bZ",
            b"\x1b]10;?\x07",
            b"\x1b]11;?\x1b\\",
            b"\x1b]12;?\x07",
            b"\x1b]4;1;?;2;?\x07",
        ];
        for query in queries {
            let mut input = b"a\x1b[1mb".to_vec();
            input.extend_from_slice(query);
            input.extend_from_slice(b"c\x1b[0m");
            assert_eq!(
                filtered(&[&input]),
                b"a\x1b[1mbc\x1b[0m",
                "{}",
                String::from_utf8_lossy(query).escape_debug()
            );
            // Cut at every byte, it is taken out all the same.
            let parts: Vec<&[u8]> = input.chunks(1).collect();
            assert_eq!(filtered(&parts), b"a\x1b[1mbc\x1b[0m");
        }
    }

    #[test]
    fn what_is_no_query_passes_as_it_came() {
        let others: &[&[u8]] = &[
            b"\x1b[1;31mred\x1b[0m",
            b"\x1b[?1049h\x1b[2J\x1b[H",
            b"\x1b[=c",
            b"\x1b[1c",
            b"\x1b[>1q",
            b"\x1b[?2026h\x1b[?2026l",
            b"\x1b]0;title\x07",
            b"\x1b]11;rgb:00/00/00\x07",
            b"\x1b]52;c;?\x07",
            b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
            b"\x1bP>|other\x1b\\",
            b"\x1b\x1b[c\\",
            "日本 \x1b7\x1b8".as_bytes(),
        ];
        for other in others {
            let want: Vec<u8> = if *other == b"\x1b\x1b[c\\" {
                b"\x1b\\".to_vec()
            } else {
                other.to_vec()
            };
            assert_eq!(
                filtered(&[other]),
                want,
                "{}",
                String::from_utf8_lossy(other).escape_debug()
            );
        }
        // A long sequence passes once it outgrows the hold.
        let long = format!("\x1b[{}m", "1;".repeat(100));
        assert_eq!(filtered(&[long.as_bytes()]), long.as_bytes());
    }

    #[test]
    fn xtversion_ends_are_where_its_answer_goes() {
        let mut filter = QueryFilter::new();
        let read = b"ab\x1b[>q\x1b[c\x1b[>0q";
        let out = filter.filter(read);
        assert_eq!(out.pass, b"ab");
        assert_eq!(out.xtversions, [6, 14]);
        assert!(xtversion().starts_with(b"\x1bP>|tiller "));
    }

    #[test]
    fn a_query_with_a_control_byte_inside_passes() {
        assert_eq!(filtered(&[b"\x1b[\n6n"]), b"\x1b[\n6n");
    }

    #[test]
    fn default_colors_are_the_theme_then_xterms() {
        assert_eq!(default_color(1), rgb(0xef7c7c));
        assert_eq!(default_color(16), rgb(0x000000));
        assert_eq!(default_color(231), rgb(0xffffff));
        assert_eq!(default_color(196), rgb(0xff0000));
        assert_eq!(default_color(232), rgb(0x080808));
        assert_eq!(default_color(255), rgb(0xeeeeee));
        assert_eq!(default_color(256), rgb(FOREGROUND));
        assert_eq!(default_color(257), rgb(BACKGROUND));
        assert_eq!(default_color(258), rgb(CURSOR));
    }
}
