//! The repaint renderer (`terminal-holder.md` 3.4).
//!
//! `alacritty_terminal` has no ANSI output, so this writes its own: bytes
//! that draw the model's active screen and modes on an empty terminal of the
//! same size. The repaint is for a viewer. It does not carry the primary
//! screen under an alternate one, the saved cursor, the scroll region, tab
//! stops or the charsets: the app's next bytes go to the viewer's own state.
//!
//! The slice emits the whole pen (`SGR 0` and every attribute) at each pen
//! change. The near-minimal diff, the scrollback rows and the round-trip
//! proptest come with H04.

use std::fmt::Write as _;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor};

/// The start of every repaint. `ESC c` is not used, because it would clear
/// the outer terminal's own state: a soft reset (`CSI ! p`) and an explicit
/// list of the modes an app may have left on.
const RESET: &str = concat!(
    "\x1b[!p",
    // Mouse modes, focus events, alternate scroll and bracketed paste off.
    "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l",
    "\x1b[?1004l\x1b[?1007l\x1b[?2004l",
    // Normal cursor keys and keypad, and no kitty keyboard flags.
    "\x1b[?1l\x1b>\x1b[=0;1u",
    // The whole screen scrolls, origin at its corner, replace mode, no
    // newline mode, wrap on, cursor hidden while drawing, the default
    // cursor shape and the default pen.
    "\x1b[r\x1b[?6l\x1b[4l\x1b[20l\x1b[?7h\x1b[?25l\x1b[0 q\x1b[0m",
);

/// The attributes that `SGR` sets, as cell flags.
const PEN_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// What `SGR` sets: colors and attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    underline: Option<Color>,
    flags: Flags,
}

impl Pen {
    const DEFAULT: Pen = Pen {
        fg: Color::Named(NamedColor::Foreground),
        bg: Color::Named(NamedColor::Background),
        underline: None,
        flags: Flags::empty(),
    };

    fn of(cell: &Cell) -> Pen {
        Pen {
            fg: cell.fg,
            bg: cell.bg,
            underline: cell.underline_color(),
            flags: cell.flags & PEN_FLAGS,
        }
    }

    /// `SGR 0`, then every part of this pen.
    fn write(&self, out: &mut String) {
        out.push_str("\x1b[0");
        let attributes = [
            (Flags::BOLD, "1"),
            (Flags::DIM, "2"),
            (Flags::ITALIC, "3"),
            (Flags::UNDERLINE, "4"),
            (Flags::DOUBLE_UNDERLINE, "21"),
            (Flags::UNDERCURL, "4:3"),
            (Flags::DOTTED_UNDERLINE, "4:4"),
            (Flags::DASHED_UNDERLINE, "4:5"),
            (Flags::INVERSE, "7"),
            (Flags::HIDDEN, "8"),
            (Flags::STRIKEOUT, "9"),
        ];
        for (flag, code) in attributes {
            if self.flags.contains(flag) {
                out.push(';');
                out.push_str(code);
            }
        }
        color(out, self.fg, Layer::Fg);
        color(out, self.bg, Layer::Bg);
        if let Some(underline) = self.underline {
            color(out, underline, Layer::Underline);
        }
        out.push('m');
    }
}

#[derive(Clone, Copy)]
enum Layer {
    Fg,
    Bg,
    Underline,
}

/// The `SGR` parameters for one color, after a `;`. A default color writes
/// nothing, since `SGR 0` came first.
fn color(out: &mut String, color: Color, layer: Layer) {
    let extended = match layer {
        Layer::Fg => 38,
        Layer::Bg => 48,
        Layer::Underline => 58,
    };
    match color {
        Color::Named(named) => {
            let index = named as usize;
            let base = match named {
                NamedColor::DimBlack
                | NamedColor::DimRed
                | NamedColor::DimGreen
                | NamedColor::DimYellow
                | NamedColor::DimBlue
                | NamedColor::DimMagenta
                | NamedColor::DimCyan
                | NamedColor::DimWhite => Some(index - NamedColor::DimBlack as usize),
                _ if index < 16 => Some(index),
                // The default colors, and the colors only a renderer uses.
                _ => None,
            };
            let Some(base) = base else { return };
            match layer {
                Layer::Fg if base < 8 => _ = write!(out, ";{}", 30 + base),
                Layer::Fg => _ = write!(out, ";{}", 90 + base - 8),
                Layer::Bg if base < 8 => _ = write!(out, ";{}", 40 + base),
                Layer::Bg => _ = write!(out, ";{}", 100 + base - 8),
                Layer::Underline => _ = write!(out, ";58:5:{base}"),
            }
        }
        Color::Indexed(index) => _ = write!(out, ";{extended}:5:{index}"),
        Color::Spec(rgb) => _ = write!(out, ";{extended}:2::{}:{}:{}", rgb.r, rgb.g, rgb.b),
    }
}

/// The repaint writer's state: the pen and the hyperlink in force.
struct Writer {
    out: String,
    pen: Pen,
    link: Option<Hyperlink>,
}

impl Writer {
    fn cup(&mut self, line: usize, col: usize) {
        _ = write!(self.out, "\x1b[{};{}H", line + 1, col + 1);
    }

    fn set_pen(&mut self, pen: Pen) {
        if pen != self.pen {
            pen.write(&mut self.out);
            self.pen = pen;
        }
    }

    fn set_link(&mut self, link: Option<Hyperlink>) {
        if link == self.link {
            return;
        }
        match &link {
            Some(link) => _ = write!(self.out, "\x1b]8;id={};{}\x1b\\", link.id(), link.uri()),
            None => self.out.push_str("\x1b]8;;\x1b\\"),
        }
        self.link = link;
    }

    /// One cell, with its pen and hyperlink, and its zero-width characters
    /// after it.
    fn cell(&mut self, cell: &Cell) {
        self.set_pen(Pen::of(cell));
        self.set_link(cell.hyperlink());
        // A tab's cell holds `\t`, which would move the cursor here: it
        // draws as the blank it is.
        self.out.push(if cell.c == '\t' { ' ' } else { cell.c });
        if let Some(zerowidth) = cell.zerowidth() {
            self.out.extend(zerowidth);
        }
    }
}

/// Whether a cell draws as nothing on a cleared screen.
fn blank(cell: &Cell) -> bool {
    (cell.c == ' ' || cell.c == '\t')
        && Pen::of(cell) == Pen::DEFAULT
        && cell.zerowidth().is_none()
        && cell.hyperlink().is_none()
}

/// The repaint of `term`'s active screen.
pub fn render<T: EventListener>(term: &Term<T>) -> Vec<u8> {
    let grid = term.grid();
    let mode = *term.mode();
    let (rows, cols) = (grid.screen_lines(), grid.columns());
    let mut w = Writer {
        out: String::from(RESET),
        pen: Pen::DEFAULT,
        link: None,
    };
    w.out.push_str(if mode.contains(TermMode::ALT_SCREEN) {
        "\x1b[?1049h"
    } else {
        "\x1b[?1049l"
    });
    w.out.push_str("\x1b[H\x1b[2J");

    let mut continues = false;
    for line in 0..rows {
        let row = &grid[Line(line as i32)];
        // The last row's flag points below the screen, so it is not kept.
        let wraps = line + 1 < rows && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        let mut end = if wraps {
            cols
        } else {
            (0..cols)
                .rev()
                .find(|&col| !blank(&row[Column(col)]))
                .map_or(0, |col| col + 1)
        };
        // A row that the one above wraps into is reached by the wrap, not a
        // cursor move, and draws at least one cell so the wrap happens.
        if continues {
            end = end.max(1);
        } else if end > 0 {
            w.cup(line, 0);
        }
        for col in 0..end {
            let cell = &row[Column(col)];
            // A wide character's spacer is drawn by the character, and the
            // spacer at the end of a row before a wrapped wide character by
            // the wrap itself.
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            w.cell(cell);
        }
        continues = wraps;
    }
    w.set_link(None);

    modes(&mut w.out, mode, term.cursor_style());

    // The cursor last. After a write in the last column the cursor waits to
    // wrap: drawing that cell again puts it in the same state, before
    // insert mode is back on.
    let cursor = &grid.cursor;
    let (line, col) = (cursor.point.line.0.max(0) as usize, cursor.point.column.0);
    if cursor.input_needs_wrap && mode.contains(TermMode::LINE_WRAP) {
        let row = &grid[Line(line as i32)];
        let col = if col > 0 && row[Column(col)].flags.contains(Flags::WIDE_CHAR_SPACER) {
            col - 1
        } else {
            col
        };
        w.cup(line, col);
        w.cell(&row[Column(col)]);
        w.set_link(None);
    } else {
        w.cup(line, col);
    }
    w.set_pen(Pen::of(&cursor.template));
    if mode.contains(TermMode::INSERT) {
        w.out.push_str("\x1b[4h");
    }
    if mode.contains(TermMode::SHOW_CURSOR) {
        w.out.push_str("\x1b[?25h");
    }
    w.out.into_bytes()
}

/// The bytes that set `mode` and `style` on a fresh terminal, with no
/// screen content: the alternate screen first, then every mode a repaint
/// sets, and the modes on from the start that the app turned off.
pub(crate) fn mode_bytes(mode: TermMode, style: CursorStyle) -> Vec<u8> {
    let mut out = String::new();
    if mode.contains(TermMode::ALT_SCREEN) {
        out.push_str("\x1b[?1049h");
    }
    modes(&mut out, mode, style);
    // On in a fresh terminal, so it is set only when the app turned it off.
    if !mode.contains(TermMode::ALTERNATE_SCROLL) {
        out.push_str("\x1b[?1007l");
    }
    if mode.contains(TermMode::INSERT) {
        out.push_str("\x1b[4h");
    }
    if !mode.contains(TermMode::SHOW_CURSOR) {
        out.push_str("\x1b[?25l");
    }
    out.into_bytes()
}

/// The modes the app turned on, other than insert mode and the cursor's
/// visibility, which come after the cursor.
fn modes(out: &mut String, mode: TermMode, style: CursorStyle) {
    let private = [
        (TermMode::BRACKETED_PASTE, "\x1b[?2004h"),
        (TermMode::APP_CURSOR, "\x1b[?1h"),
        (TermMode::APP_KEYPAD, "\x1b="),
        (TermMode::MOUSE_REPORT_CLICK, "\x1b[?1000h"),
        (TermMode::MOUSE_DRAG, "\x1b[?1002h"),
        (TermMode::MOUSE_MOTION, "\x1b[?1003h"),
        (TermMode::UTF8_MOUSE, "\x1b[?1005h"),
        (TermMode::SGR_MOUSE, "\x1b[?1006h"),
        (TermMode::FOCUS_IN_OUT, "\x1b[?1004h"),
        (TermMode::ALTERNATE_SCROLL, "\x1b[?1007h"),
        (TermMode::LINE_FEED_NEW_LINE, "\x1b[20h"),
        (TermMode::ORIGIN, "\x1b[?6h"),
    ];
    for (flag, set) in private {
        if mode.contains(flag) {
            out.push_str(set);
        }
    }
    if !mode.contains(TermMode::LINE_WRAP) {
        out.push_str("\x1b[?7l");
    }
    let kitty = [
        (TermMode::DISAMBIGUATE_ESC_CODES, 1),
        (TermMode::REPORT_EVENT_TYPES, 2),
        (TermMode::REPORT_ALTERNATE_KEYS, 4),
        (TermMode::REPORT_ALL_KEYS_AS_ESC, 8),
        (TermMode::REPORT_ASSOCIATED_TEXT, 16),
    ]
    .into_iter()
    .filter(|(flag, _)| mode.contains(*flag))
    .fold(0, |flags, (_, bit)| flags | bit);
    if kitty != 0 {
        _ = write!(out, "\x1b[>{kitty}u");
    }
    if style != CursorStyle::default() {
        let shape = match style.shape {
            CursorShape::Block => Some(1),
            CursorShape::Underline => Some(3),
            CursorShape::Beam => Some(5),
            CursorShape::HollowBlock | CursorShape::Hidden => None,
        };
        if let Some(shape) = shape {
            let steady = u8::from(!style.blinking);
            _ = write!(out, "\x1b[{} q", shape + steady);
        }
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::Point;
    use tiller_panes::Size;

    use super::*;
    use crate::model::Model;

    fn size() -> Size {
        Size { cols: 12, rows: 5 }
    }

    /// The parts of a cell a viewer sees.
    fn visible(cell: &Cell) -> (char, Pen, Option<Vec<char>>, Option<String>, Flags) {
        let layout = Flags::WRAPLINE
            | Flags::WIDE_CHAR
            | Flags::WIDE_CHAR_SPACER
            | Flags::LEADING_WIDE_CHAR_SPACER;
        (
            if cell.c == '\t' { ' ' } else { cell.c },
            Pen::of(cell),
            cell.zerowidth().map(<[char]>::to_vec),
            cell.hyperlink().map(|link| link.uri().to_owned()),
            cell.flags & layout,
        )
    }

    /// Feeds `input` to a model, repaints it into a fresh model of the same
    /// size, and checks that the two show the same thing.
    fn round_trip(input: &[u8]) {
        let mut source = Model::new(size());
        source.feed(input).unwrap();
        let repaint = source.repaint().unwrap();
        let mut copy = Model::new(size());
        copy.feed(&repaint).unwrap();
        let (a, b) = (source.term(), copy.term());
        for line in 0..i32::from(size().rows) {
            for col in 0..usize::from(size().cols) {
                let point = Point::new(Line(line), Column(col));
                assert_eq!(
                    visible(&a.grid()[point]),
                    visible(&b.grid()[point]),
                    "{input:?} at {point:?}, repaint {:?}",
                    String::from_utf8_lossy(&repaint)
                );
            }
        }
        let (ca, cb) = (&a.grid().cursor, &b.grid().cursor);
        assert_eq!(ca.point, cb.point, "{input:?}: cursor");
        assert_eq!(
            ca.input_needs_wrap, cb.input_needs_wrap,
            "{input:?}: pending wrap"
        );
        assert_eq!(
            Pen::of(&ca.template),
            Pen::of(&cb.template),
            "{input:?}: pen"
        );
        assert_eq!(a.mode(), b.mode(), "{input:?}: modes");
        assert_eq!(
            a.cursor_style(),
            b.cursor_style(),
            "{input:?}: cursor style"
        );
    }

    #[test]
    fn plain_text_and_the_cursor_round_trip() {
        round_trip(b"");
        round_trip(b"hello\r\nworld");
        round_trip(b"a\r\n\r\n   b   \x1b[2;3H");
        round_trip(b"tab\there\r\n");
    }

    #[test]
    fn colors_and_attributes_round_trip() {
        round_trip(b"\x1b[1;31mred\x1b[0m \x1b[42;3mgreen\x1b[0m");
        round_trip(b"\x1b[38;5;200mix\x1b[48;2;1;2;3mrgb\x1b[0m");
        round_trip(b"\x1b[4:3;58;5;9mcurl\x1b[21mdbl\x1b[24;7;9;2;8mz\x1b[0m");
        round_trip(b"\x1b[95;104mbright\x1b[44m   \x1b[0m\r\n");
        // The pen in force at the end is the cursor's.
        round_trip(b"x\x1b[1;33;44m");
    }

    #[test]
    fn wide_and_combining_characters_round_trip() {
        round_trip("日本語 e\u{301} 🦀!".as_bytes());
        // A wide character that does not fit wraps, and leaves a spacer.
        round_trip("0123456789a日b".as_bytes());
    }

    #[test]
    fn wrapped_rows_and_a_pending_wrap_round_trip() {
        round_trip(b"0123456789ABCDEFGHIJ");
        round_trip(b"0123456789AB");
        round_trip(b"0123456789AB\r\n");
        round_trip(b"0123456789AB            x");
        round_trip(b"\x1b[5;1H0123456789AB");
    }

    #[test]
    fn modes_and_the_alternate_screen_round_trip() {
        round_trip(b"main\x1b[?1049halt\x1b[?2004h\x1b[?1h\x1b=\x1b[?1002h\x1b[?1006h");
        round_trip(b"\x1b[?1004h\x1b[?25l\x1b[5 q\x1b[>5u\x1b[?7l\x1b[4h");
        round_trip(b"\x1b[?1049h\x1b[>1u\x1b[?1003h\x1b[20h\x1b[?6h\x1b[3;4H");
        round_trip(b"\x1b[?1007l\x1b[2 q");
    }

    #[test]
    fn hyperlinks_round_trip() {
        round_trip(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ text");
        round_trip(b"\x1b]8;id=a;https://example.com/a\x07ab\x1b]8;;\x07");
    }

    #[test]
    fn a_repaint_starts_with_the_reset_and_ends_with_the_cursor() {
        let mut model = Model::new(size());
        model.feed(b"\x1b[?2004hhi").unwrap();
        let repaint = String::from_utf8(model.repaint().unwrap()).unwrap();
        assert!(repaint.starts_with("\x1b[!p"), "{repaint:?}");
        assert!(!repaint.contains("\x1bc"), "{repaint:?}");
        assert!(repaint.ends_with("\x1b[1;3H\x1b[?25h"), "{repaint:?}");
        assert!(repaint.contains("\x1b[?2004h"), "{repaint:?}");
    }
}
