//! The `screen` read (`terminal-holder.md` 3.3): what `herdr pane read` gave
//! the parsers, read from the model.
//!
//! A `text` row is the row's characters with trailing spaces trimmed. An
//! `ansi` row is in the shape herdr wrote, which `composer::classify` and
//! `prompt.rs` read: at each pen change `SGR 0` and then one escape per
//! attribute and color, in the semicolon form, and `SGR 0` at the row's end
//! when the pen is not the default.

use std::fmt::Write as _;

use alacritty_terminal::grid::Row;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use tiller_panes::message::{Cursor, Format, Read, Screen};

use crate::snapshot::Snapshot;

/// The modes a `screen` reply names, in this order.
const MODES: &[(TermMode, &str)] = &[
    (TermMode::APP_CURSOR, "app_cursor"),
    (TermMode::APP_KEYPAD, "app_keypad"),
    (TermMode::BRACKETED_PASTE, "bracketed_paste"),
    (TermMode::ALT_SCREEN, "alt_screen"),
    (TermMode::MOUSE_REPORT_CLICK, "mouse_click"),
    (TermMode::MOUSE_DRAG, "mouse_drag"),
    (TermMode::MOUSE_MOTION, "mouse_motion"),
    (TermMode::UTF8_MOUSE, "mouse_utf8"),
    (TermMode::SGR_MOUSE, "mouse_sgr"),
    (TermMode::FOCUS_IN_OUT, "focus"),
    (TermMode::ALTERNATE_SCROLL, "alternate_scroll"),
    (TermMode::INSERT, "insert"),
    (TermMode::ORIGIN, "origin"),
    (TermMode::LINE_FEED_NEW_LINE, "newline"),
    (TermMode::DISAMBIGUATE_ESC_CODES, "kitty_disambiguate"),
    (TermMode::REPORT_EVENT_TYPES, "kitty_event_types"),
    (TermMode::REPORT_ALTERNATE_KEYS, "kitty_alternate_keys"),
    (TermMode::REPORT_ALL_KEYS_AS_ESC, "kitty_all_keys"),
    (TermMode::REPORT_ASSOCIATED_TEXT, "kitty_associated_text"),
];

/// The attributes an `ansi` row writes, each as its own escape.
const ATTRIBUTES: &[(Flags, &str)] = &[
    (Flags::BOLD, "1"),
    (Flags::DIM, "2"),
    (Flags::ITALIC, "3"),
    (Flags::ALL_UNDERLINES, "4"),
    (Flags::INVERSE, "7"),
    (Flags::HIDDEN, "8"),
    (Flags::STRIKEOUT, "9"),
];

/// What an `ansi` row writes for a cell: its colors and attributes. The
/// underline color is left out, as herdr left it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    flags: Flags,
}

impl Pen {
    const DEFAULT: Pen = Pen {
        fg: Color::Named(NamedColor::Foreground),
        bg: Color::Named(NamedColor::Background),
        flags: Flags::empty(),
    };

    /// The flags a pen keeps: those in [`ATTRIBUTES`].
    const FLAGS: Flags = {
        let mut flags = Flags::empty();
        let mut index = 0;
        while index < ATTRIBUTES.len() {
            flags = flags.union(ATTRIBUTES[index].0);
            index += 1;
        }
        flags
    };

    fn of(cell: &Cell) -> Pen {
        Pen {
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags.intersection(Pen::FLAGS),
        }
    }

    /// `SGR 0`, then one escape for each part of this pen.
    fn write(&self, out: &mut String) {
        out.push_str("\x1b[0m");
        for (flag, code) in ATTRIBUTES {
            if self.flags.intersects(*flag) {
                _ = write!(out, "\x1b[{code}m");
            }
        }
        color(out, self.fg, 30, 90, 38);
        color(out, self.bg, 40, 100, 48);
    }
}

/// One color's escape: `base` and `bright` for the 16 named colors,
/// `extended` with `;5;n` or `;2;r;g;b` for the rest. A default color
/// writes nothing, since `SGR 0` came first.
fn color(out: &mut String, color: Color, base: usize, bright: usize, extended: u8) {
    match color {
        Color::Named(named) => {
            let index = named as usize;
            let code = match named {
                NamedColor::DimBlack
                | NamedColor::DimRed
                | NamedColor::DimGreen
                | NamedColor::DimYellow
                | NamedColor::DimBlue
                | NamedColor::DimMagenta
                | NamedColor::DimCyan
                | NamedColor::DimWhite => base + index - NamedColor::DimBlack as usize,
                _ if index < 8 => base + index,
                _ if index < 16 => bright + index - 8,
                // The default colors, and the colors only a renderer uses.
                _ => return,
            };
            _ = write!(out, "\x1b[{code}m");
        }
        Color::Indexed(index) => _ = write!(out, "\x1b[{extended};5;{index}m"),
        Color::Spec(rgb) => _ = write!(out, "\x1b[{extended};2;{};{};{}m", rgb.r, rgb.g, rgb.b),
    }
}

/// Whether a cell reads as nothing at a row's end.
fn blank(cell: &Cell, format: Format) -> bool {
    let empty = (cell.c == ' ' || cell.c == '\t') && cell.zerowidth().is_none();
    match format {
        Format::Text => empty,
        Format::Ansi => empty && Pen::of(cell) == Pen::DEFAULT,
    }
}

/// One row, in `format`.
fn row(row: &Row<Cell>, cols: usize, format: Format) -> String {
    let end = (0..cols)
        .rev()
        .find(|&col| !blank(&row[Column(col)], format))
        .map_or(0, |col| col + 1);
    let mut out = String::new();
    let mut pen = Pen::DEFAULT;
    for col in 0..end {
        let cell = &row[Column(col)];
        // A wide character's spacer is part of the character.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        if format == Format::Ansi {
            let next = Pen::of(cell);
            if next != pen {
                next.write(&mut out);
                pen = next;
            }
        }
        out.push(if cell.c == '\t' { ' ' } else { cell.c });
        if let Some(zerowidth) = cell.zerowidth() {
            out.extend(zerowidth);
        }
    }
    if pen != Pen::DEFAULT {
        out.push_str("\x1b[0m");
    }
    if format == Format::Text {
        out.truncate(out.trim_end_matches(' ').len());
    }
    out
}

/// The modes in `mode` that a reply names, by name.
pub fn modes(mode: TermMode) -> Vec<String> {
    MODES
        .iter()
        .filter(|(flag, _)| mode.contains(*flag))
        .map(|(_, name)| (*name).to_owned())
        .collect()
}

/// The scrollback rows a `read` takes, at most, on a screen of
/// `screen_lines`.
pub fn history_for(read: Read, screen_lines: usize) -> usize {
    match read {
        Read::Visible => 0,
        Read::Recent(n) => usize::from(n).saturating_sub(screen_lines),
    }
}

/// A `screen` read of `snapshot`, which holds the scrollback rows
/// [`history_for`] names, or all there are. `read` must be in range
/// ([`Read::in_range`]). A `recent` read is the last rows up to the
/// screen's bottom, scrollback first, as many as there are.
pub fn read(snapshot: &Snapshot, read: Read, format: Format) -> Screen {
    let (screen_lines, cols) = (snapshot.screen_lines(), snapshot.columns());
    let first = match read {
        Read::Visible => 0,
        Read::Recent(n) => {
            let above = history_for(read, screen_lines);
            let above = above.min(snapshot.history());
            let below = usize::from(n).min(screen_lines);
            // The first row read: `above` rows into the scrollback, or
            // `below` rows up from the screen's bottom when the read is
            // shorter than the screen.
            if above > 0 {
                -(above as i32)
            } else {
                (screen_lines - below) as i32
            }
        }
    };
    let rows_text = (first..screen_lines as i32)
        .map(|line| row(snapshot.row(line), cols, format))
        .collect();
    let cursor = &snapshot.cursor.point;
    Screen {
        cols: u16::try_from(cols).unwrap_or(u16::MAX),
        rows: u16::try_from(screen_lines).unwrap_or(u16::MAX),
        cursor: Cursor {
            col: u16::try_from(cursor.column.0).unwrap_or(u16::MAX),
            row: u16::try_from(cursor.line.0.max(0)).unwrap_or(u16::MAX),
            visible: snapshot.mode.contains(TermMode::SHOW_CURSOR),
        },
        modes: modes(snapshot.mode),
        rows_text,
    }
}

#[cfg(test)]
mod tests {
    use tiller_panes::Size;

    use crate::model::Model;

    use super::*;

    /// A read of `model` with all its scrollback at hand.
    fn read(model: &Model, read: Read, format: Format) -> Screen {
        super::read(&Snapshot::of(model.term(), usize::MAX), read, format)
    }

    fn model(cols: u16, rows: u16, bytes: &[u8]) -> Model {
        let mut model = Model::new(Size { cols, rows });
        model.feed(bytes).unwrap();
        model
    }

    #[test]
    fn a_text_read_trims_each_row_and_skips_wide_spacers() {
        let model = model(12, 3, "a  b   \r\n日本 x\t\r\ne\u{301}".as_bytes());
        let screen = read(&model, Read::Visible, Format::Text);
        assert_eq!(screen.rows_text, ["a  b", "日本 x", "e\u{301}"]);
        assert_eq!((screen.cols, screen.rows), (12, 3));
        assert_eq!(
            screen.cursor,
            Cursor {
                col: 1,
                row: 2,
                visible: true
            }
        );
    }

    #[test]
    fn an_ansi_read_writes_each_part_of_the_pen_on_its_own_in_semicolon_form() {
        let model = model(
            40,
            2,
            b"\x1b[1;38;5;0;48;5;3mhost\x1b[0m \x1b[2;38;2;1;2;3mdim\x1b[0m\r\n\x1b[31;104mred\x1b[4:3;58;5;9mx",
        );
        let screen = read(&model, Read::Visible, Format::Ansi);
        assert_eq!(
            screen.rows_text,
            [
                "\x1b[0m\x1b[1m\x1b[38;5;0m\x1b[48;5;3mhost\x1b[0m \x1b[0m\x1b[2m\x1b[38;2;1;2;3mdim\x1b[0m",
                "\x1b[0m\x1b[31m\x1b[104mred\x1b[0m\x1b[4m\x1b[31m\x1b[104mx\x1b[0m",
            ]
        );
    }

    #[test]
    fn an_ansi_row_keeps_trailing_cells_that_have_a_color() {
        let model = model(10, 1, b"a\x1b[44m  \x1b[0m   ");
        let screen = read(&model, Read::Visible, Format::Ansi);
        assert_eq!(screen.rows_text, ["a\x1b[0m\x1b[44m  \x1b[0m"]);
        let screen = read(&model, Read::Visible, Format::Text);
        assert_eq!(screen.rows_text, ["a"]);
    }

    #[test]
    fn a_recent_read_takes_scrollback_and_the_screen_up_to_its_bottom() {
        let model = model(10, 2, b"one\r\ntwo\r\nthree\r\nfour");
        let text = |n| read(&model, Read::Recent(n), Format::Text).rows_text;
        assert_eq!(text(1), ["four"]);
        assert_eq!(text(2), ["three", "four"]);
        assert_eq!(text(3), ["two", "three", "four"]);
        // No more than there is.
        assert_eq!(text(100), ["one", "two", "three", "four"]);
        let visible = read(&model, Read::Visible, Format::Text);
        assert_eq!(visible.rows_text, ["three", "four"]);
    }

    #[test]
    fn modes_are_named() {
        let model = model(10, 2, b"\x1b[?2004h\x1b[?1h\x1b[>1u\x1b[?25l");
        let screen = read(&model, Read::Visible, Format::Text);
        assert_eq!(
            screen.modes,
            // Alternate scroll is on from the start, as in xterm.
            [
                "app_cursor",
                "bracketed_paste",
                "alternate_scroll",
                "kitty_disambiguate"
            ]
        );
        assert!(!screen.cursor.visible);
    }
}
