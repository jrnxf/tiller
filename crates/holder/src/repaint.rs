//! The repaint renderer (`terminal-holder.md` 3.4).
//!
//! `alacritty_terminal` has no ANSI output, so this writes its own: bytes
//! that draw the model's active screen and modes on an empty terminal of the
//! same size, and the scrollback rows asked for above it. The repaint is for
//! a viewer. It does not carry the primary screen under an alternate one,
//! the saved cursor, the scroll region, tab stops or the charsets: the
//! app's next bytes go to the viewer's own state.
//!
//! Each pen change writes the shorter of two forms: the attributes and
//! colors that change, or `SGR 0` and the whole new pen.

use std::fmt::Write as _;

use alacritty_terminal::grid::Row;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor};

use crate::snapshot::Snapshot;

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

/// Each underline style and its `SGR` code. Each one clears the others.
const UNDERLINES: [(Flags, &str); 5] = [
    (Flags::UNDERLINE, "4"),
    (Flags::DOUBLE_UNDERLINE, "21"),
    (Flags::UNDERCURL, "4:3"),
    (Flags::DOTTED_UNDERLINE, "4:4"),
    (Flags::DASHED_UNDERLINE, "4:5"),
];

/// The attributes other than bold, dim and underline: each code that sets
/// it, and the one that clears it alone.
const OTHERS: [(Flags, &str, &str); 4] = [
    (Flags::ITALIC, "3", "23"),
    (Flags::INVERSE, "7", "27"),
    (Flags::HIDDEN, "8", "28"),
    (Flags::STRIKEOUT, "9", "29"),
];

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

    fn underline_style(&self) -> Flags {
        self.flags & Flags::ALL_UNDERLINES
    }

    /// The `SGR` parameters that take a terminal from pen `from` to this
    /// one, each with a `;` before it.
    fn change(&self, from: &Pen, out: &mut String) {
        let gone = from.flags - self.flags;
        let mut bold_dim = self.flags & (Flags::BOLD | Flags::DIM);
        // `SGR 22` clears bold and dim both, so the one kept is set again.
        if gone.intersects(Flags::BOLD | Flags::DIM) {
            out.push_str(";22");
        } else {
            bold_dim -= from.flags;
        }
        if bold_dim.contains(Flags::BOLD) {
            out.push_str(";1");
        }
        if bold_dim.contains(Flags::DIM) {
            out.push_str(";2");
        }
        if self.underline_style() != from.underline_style() {
            match UNDERLINES
                .iter()
                .find(|(flag, _)| self.flags.contains(*flag))
            {
                Some((_, code)) => {
                    out.push(';');
                    out.push_str(code);
                }
                None => out.push_str(";24"),
            }
        }
        for (flag, set, clear) in OTHERS {
            match (from.flags.contains(flag), self.flags.contains(flag)) {
                (false, true) => {
                    out.push(';');
                    out.push_str(set);
                }
                (true, false) => {
                    out.push(';');
                    out.push_str(clear);
                }
                _ => {}
            }
        }
        if self.fg != from.fg {
            color(out, self.fg, Layer::Fg);
        }
        if self.bg != from.bg {
            color(out, self.bg, Layer::Bg);
        }
        if self.underline != from.underline {
            match self.underline {
                Some(underline) => color(out, underline, Layer::Underline),
                None => out.push_str(";59"),
            }
        }
    }

    /// The shorter `SGR` that takes a terminal from pen `from` to this one:
    /// the changes alone, or `SGR 0` and the whole pen.
    fn write(&self, from: &Pen, out: &mut String) {
        let mut changes = String::new();
        self.change(from, &mut changes);
        let mut whole = String::from(";0");
        self.change(&Pen::DEFAULT, &mut whole);
        let params = if changes.len() < whole.len() {
            changes
        } else {
            whole
        };
        out.push_str("\x1b[");
        out.push_str(&params[1..]);
        out.push('m');
    }
}

#[derive(Clone, Copy)]
enum Layer {
    Fg,
    Bg,
    Underline,
}

/// The `SGR` parameters for one color, after a `;`. A default color, and
/// a color only a renderer uses, is the layer's default.
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
                _ => None,
            };
            match (layer, base) {
                (Layer::Fg, None) => out.push_str(";39"),
                (Layer::Bg, None) => out.push_str(";49"),
                (Layer::Underline, None) => out.push_str(";59"),
                (Layer::Fg, Some(base)) if base < 8 => _ = write!(out, ";{}", 30 + base),
                (Layer::Fg, Some(base)) => _ = write!(out, ";{}", 90 + base - 8),
                (Layer::Bg, Some(base)) if base < 8 => _ = write!(out, ";{}", 40 + base),
                (Layer::Bg, Some(base)) => _ = write!(out, ";{}", 100 + base - 8),
                (Layer::Underline, Some(base)) => _ = write!(out, ";58:5:{base}"),
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
            pen.write(&self.pen, &mut self.out);
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

    /// The cells of one row up to `end`, from where the cursor is, and
    /// whether the drawing wraps into the next row.
    fn row(&mut self, row: &Row<Cell>, end: usize, wraps: bool) {
        for col in 0..end {
            let cell = &row[Column(col)];
            // A wide character's spacer is drawn by the character, and the
            // spacer at the end of a row before a wrapped wide character by
            // the wrap itself. With no wrap drawn, that spacer is a blank.
            let spacer = if wraps {
                Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER
            } else {
                Flags::WIDE_CHAR_SPACER
            };
            if cell.flags.intersects(spacer) {
                continue;
            }
            self.cell(cell);
        }
    }

    /// After a row that wraps, the spacer at its end that a wide character
    /// leaves when it does not fit, if the next row's wide character does
    /// not draw it: there is none (a line was put in between), or its pen
    /// is not the spacer's. A wide character drawn there with the spacer's
    /// pen makes the spacer and the wrap, and it is erased again on the
    /// next row, where the cursor is left.
    fn lone_spacer(&mut self, row: &Row<Cell>, next: &Row<Cell>, cols: usize) {
        let (spacer, first) = (&row[Column(cols - 1)], &next[Column(0)]);
        if !spacer.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            || (first.flags.contains(Flags::WIDE_CHAR)
                && Pen::of(first) == Pen::of(spacer)
                && first.hyperlink() == spacer.hyperlink())
        {
            return;
        }
        self.set_pen(Pen::of(spacer));
        self.set_link(spacer.hyperlink());
        self.out.push('\u{3000}');
        self.set_pen(Pen::DEFAULT);
        self.out.push_str("\r\x1b[2X");
    }

    /// A line feed that scrolls a blank row in. The pen is the default
    /// first, as a terminal fills the new row with the pen's background.
    fn new_line(&mut self, feed: &str) {
        self.set_pen(Pen::DEFAULT);
        self.out.push_str(feed);
    }
}

/// Whether a cell draws as nothing on a cleared screen.
fn blank(cell: &Cell) -> bool {
    (cell.c == ' ' || cell.c == '\t')
        && Pen::of(cell) == Pen::DEFAULT
        && cell.zerowidth().is_none_or(<[char]>::is_empty)
        && cell.hyperlink().is_none()
}

/// Where a row's drawing ends: its last cell when it wraps into the next,
/// else after its last cell that is not blank.
fn row_end(row: &Row<Cell>, cols: usize, wraps: bool) -> usize {
    if wraps {
        cols
    } else {
        (0..cols)
            .rev()
            .find(|&col| !blank(&row[Column(col)]))
            .map_or(0, |col| col + 1)
    }
}

/// The repaint of a snapshot: its scrollback rows, which scroll off the
/// top of the viewer's screen into its scrollback, then its screen.
pub fn render(snapshot: &Snapshot) -> Vec<u8> {
    let mode = snapshot.mode;
    let (rows, cols) = (snapshot.screen_lines(), snapshot.columns());
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

    let history = snapshot.history();
    if history > 0 {
        // The viewer's own scrollback goes, so the rows sent are not shown
        // twice.
        w.out.push_str("\x1b[3J");
        // Each scrollback row in turn from the top, joined by a wrap or a
        // line feed. Then enough line feeds that the last of them is above
        // the screen, which is blank again for the screen's rows.
        let first = -i32::try_from(history).unwrap_or(i32::MAX);
        let mut continues = false;
        for line in first..0 {
            let row = snapshot.row(line);
            let wraps = line + 1 < 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            let mut end = row_end(row, cols, wraps);
            if continues {
                end = end.max(1);
            }
            w.row(row, end, wraps);
            // The wrap into this row scrolled a row in with the pen of its
            // first cell: the rest of it is cleared with the default.
            if continues && end < cols {
                w.set_pen(Pen::DEFAULT);
                w.out.push_str("\x1b[K");
            }
            if wraps {
                w.lone_spacer(row, snapshot.row(line + 1), cols);
            } else {
                w.new_line("\r\n");
            }
            continues = wraps;
        }
        w.set_link(None);
        w.new_line(&"\n".repeat(rows - 1));
        w.out.push_str("\x1b[H");
    }

    let mut continues = false;
    for line in 0..rows {
        let row = snapshot.row(i32::try_from(line).unwrap_or(i32::MAX));
        // The last row's flag points below the screen, so it is not kept.
        let wraps = line + 1 < rows && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        let mut end = row_end(row, cols, wraps);
        // A row that the one above wraps into is reached by the wrap, not a
        // cursor move, and draws at least one cell so the wrap happens.
        if continues {
            end = end.max(1);
        } else if end > 0 {
            w.cup(line, 0);
        }
        w.row(row, end, wraps);
        if wraps {
            w.lone_spacer(
                row,
                snapshot.row(i32::try_from(line + 1).unwrap_or(i32::MAX)),
                cols,
            );
        }
        continues = wraps;
    }
    w.set_link(None);

    modes(&mut w.out, mode, snapshot.style);

    // The cursor last. After a write in the last column the cursor waits to
    // wrap: drawing that cell again puts it in the same state, before
    // insert mode is back on.
    let cursor = &snapshot.cursor;
    let (line, col) = (
        usize::try_from(cursor.point.line.0).unwrap_or(0),
        cursor.point.column.0,
    );
    // With autowrap off the wait does nothing to the next character, so
    // the cell is not drawn again.
    if cursor.input_needs_wrap && mode.contains(TermMode::LINE_WRAP) {
        let row = snapshot.row(i32::try_from(line).unwrap_or(0));
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
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Line, Point};
    use alacritty_terminal::term::Term;
    use proptest::prelude::*;
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
            // A cell can keep an empty list of them, which draws as none.
            cell.zerowidth()
                .filter(|chars| !chars.is_empty())
                .map(<[char]>::to_vec),
            cell.hyperlink().map(|link| link.uri().to_owned()),
            cell.flags & layout,
        )
    }

    /// Whether every wide character from `first` down has its spacer, every
    /// spacer its wide character, and every wrap is in the last column. An
    /// insert or delete in a row (`ICH`, `DCH`) can break that in
    /// `alacritty_terminal`, and no bytes draw such a state on a fresh
    /// terminal, so a repaint of it is not compared.
    fn drawable(term: &Term<crate::query::Listener>, first: i32) -> bool {
        let grid = term.grid();
        let cols = grid.columns();
        (first..i32::try_from(grid.screen_lines()).unwrap()).all(|line| {
            let row = &grid[Line(line)];
            (0..cols).all(|col| {
                let flags = row[Column(col)].flags;
                let wide = !flags.contains(Flags::WIDE_CHAR)
                    || (col + 1 < cols
                        && row[Column(col + 1)].flags.contains(Flags::WIDE_CHAR_SPACER));
                let spacer = !flags.contains(Flags::WIDE_CHAR_SPACER)
                    || (col > 0 && row[Column(col - 1)].flags.contains(Flags::WIDE_CHAR));
                // A wrap, and the spacer before one, are in the last column.
                let wrap = col + 1 == cols
                    || !flags.intersects(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER);
                wide && spacer && wrap
            })
        })
    }

    /// Feeds `input` to a model, repaints it with up to `history` rows of
    /// its scrollback into a fresh model of the same size, and says how the
    /// two differ, if they do.
    fn differs(input: &[u8], history: usize) -> Option<String> {
        differs_at(size(), input, history)
    }

    /// [`differs`] on a model of `size`.
    fn differs_at(size: Size, input: &[u8], history: usize) -> Option<String> {
        let mut source = Model::new(size);
        source.feed(input).unwrap();
        let snapshot = source.snapshot(history).unwrap();
        let taken = snapshot.history();
        let repaint = render(&snapshot);
        let mut copy = Model::new(size);
        copy.feed(&repaint).unwrap();
        let (a, b) = (source.term(), copy.term());
        let first = -i32::try_from(taken).unwrap();
        if !drawable(a, first) {
            return None;
        }
        // A cursor waiting to wrap is put back by drawing its cell again.
        // When the cursor or a line moved with the wait kept (`RI`, `IL`,
        // `DL`) and that cell is where a row wraps, the drawing ends that
        // row's wrap: a terminal has no other way to make the cursor wait.
        let cursor = &a.grid().cursor;
        let wrap_on = a.mode().contains(TermMode::LINE_WRAP);
        if cursor.input_needs_wrap
            && wrap_on
            && a.grid()[cursor.point]
                .flags
                .intersects(Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            return None;
        }
        let shown = || String::from_utf8_lossy(&repaint).escape_debug().to_string();
        // A clear with no scrollback rows sent may push blank rows into
        // the copy's scrollback, as `ED 2` does.
        if taken > 0 && b.grid().history_size() != taken {
            return Some(format!(
                "{} scrollback rows, not {taken}: repaint {}",
                b.grid().history_size(),
                shown()
            ));
        }
        for line in first..i32::from(size.rows) {
            for col in 0..usize::from(size.cols) {
                let point = Point::new(Line(line), Column(col));
                let (mut want, mut got) = (visible(&a.grid()[point]), visible(&b.grid()[point]));
                // The last scrollback row's wrap into the screen is not
                // carried, nor the last screen row's wrap below it, nor the
                // spacer a wide character leaves before such a wrap.
                if line == -1 || line == i32::from(size.rows) - 1 {
                    let wrap = Flags::WRAPLINE | Flags::LEADING_WIDE_CHAR_SPACER;
                    want.4.remove(wrap);
                    got.4.remove(wrap);
                }
                if want != got {
                    return Some(format!(
                        "at {point:?}: {want:?} != {got:?}, repaint {}",
                        shown()
                    ));
                }
            }
        }
        let (ca, cb) = (&a.grid().cursor, &b.grid().cursor);
        let checks = [
            (ca.point == cb.point, "cursor"),
            // With autowrap off a wait does nothing, and is not carried.
            (
                !wrap_on || ca.input_needs_wrap == cb.input_needs_wrap,
                "pending wrap",
            ),
            (Pen::of(&ca.template) == Pen::of(&cb.template), "pen"),
            (a.mode() == b.mode(), "modes"),
            (a.cursor_style() == b.cursor_style(), "cursor style"),
        ];
        checks
            .iter()
            .find(|(same, _)| !same)
            .map(|(_, what)| format!("{what} differs, repaint {}", shown()))
    }

    fn round_trip(input: &[u8]) {
        if let Some(diff) = differs(input, 0) {
            panic!("{}: {diff}", String::from_utf8_lossy(input).escape_debug());
        }
    }

    // Section 9.1: each Claude Code fixture, at the 119 columns and the
    // rows herdr read it at, repaints to the same screen.
    #[test]
    fn every_claude_code_fixture_round_trips() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../panes/tests/fixtures/claude"
        );
        let mut fixtures: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "ansi"))
            .collect();
        fixtures.sort();
        assert_eq!(fixtures.len(), 9, "{fixtures:?}");
        for path in fixtures {
            let fixture = std::fs::read(&path).unwrap();
            let rows = fixture.iter().filter(|&&byte| byte == b'\n').count() + 1;
            let size = Size {
                cols: 119,
                rows: u16::try_from(rows).unwrap(),
            };
            assert_eq!(differs_at(size, &fixture, 0), None, "{}", path.display());
        }
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

    #[test]
    fn a_pen_change_writes_only_what_changes_when_that_is_shorter() {
        let pen = |flags: Flags, fg: Color| Pen {
            fg,
            flags,
            ..Pen::DEFAULT
        };
        let red = Color::Named(NamedColor::Red);
        let sgr = |from: Pen, to: Pen| {
            let mut out = String::new();
            to.write(&from, &mut out);
            out
        };
        assert_eq!(sgr(Pen::DEFAULT, pen(Flags::BOLD, red)), "\x1b[1;31m");
        assert_eq!(
            sgr(
                pen(Flags::BOLD | Flags::ITALIC, red),
                pen(Flags::ITALIC, red)
            ),
            "\x1b[22m"
        );
        assert_eq!(
            sgr(pen(Flags::BOLD | Flags::DIM, red), pen(Flags::DIM, red)),
            "\x1b[22;2m"
        );
        assert_eq!(
            sgr(pen(Flags::UNDERCURL, red), pen(Flags::UNDERLINE, red)),
            "\x1b[4m"
        );
        assert_eq!(
            sgr(pen(Flags::ITALIC, red), pen(Flags::ITALIC, Pen::DEFAULT.fg)),
            "\x1b[39m"
        );
        // Back to the default, `SGR 0` is the shorter.
        assert_eq!(
            sgr(
                pen(Flags::BOLD | Flags::ITALIC | Flags::INVERSE, red),
                Pen::DEFAULT
            ),
            "\x1b[0m"
        );
    }

    #[test]
    fn scrollback_rows_go_above_the_screen() {
        let mut input = Vec::new();
        for n in 0..30 {
            input.extend_from_slice(format!("\x1b[3{}mline {n}\r\n", n % 8).as_bytes());
        }
        input.extend_from_slice(b"0123456789ABCDEFGHIJKLMNOP\r\nend");
        for history in [0, 1, 4, 12, 40] {
            assert_eq!(differs(&input, history), None, "{history} rows");
        }
        // The alternate screen has no scrollback.
        input.extend_from_slice(b"\x1b[?1049halt");
        assert_eq!(differs(&input, 10), None);
    }

    /// Bytes an app writes, one piece at a time.
    fn piece() -> impl Strategy<Value = Vec<u8>> {
        let text = prop_oneof![
            "[ -~]{1,8}",
            Just("日本".to_owned()),
            Just("e\u{301}".to_owned()),
            Just("🦀".to_owned()),
            Just("\t".to_owned()),
        ]
        .prop_map(String::into_bytes);
        let control = prop::sample::select(vec![
            &b"\r\n"[..],
            b"\r",
            b"\n",
            b"\x08",
            b"\x1b[K",
            b"\x1b[1K",
            b"\x1b[J",
            b"\x1b[2J",
            b"\x1b[L",
            b"\x1b[M",
            b"\x1b[2@",
            b"\x1b[P",
            b"\x1b[X",
            b"\x1bM",
            b"\x1b7",
            b"\x1b8",
            b"\x1b[?1049h",
            b"\x1b[?1049l",
            b"\x1b[?7l",
            b"\x1b[?7h",
            b"\x1b[4h",
            b"\x1b[4l",
            b"\x1b[?25l",
            b"\x1b[?2004h",
            b"\x1b[?1h",
            b"\x1b=",
            b"\x1b[?1002h",
            b"\x1b[?1006h",
            b"\x1b[>1u",
            b"\x1b[>3u",
            b"\x1b[<u",
            b"\x1b[5 q",
            b"\x1b]8;;https://example.com\x1b\\",
            b"\x1b]8;;\x1b\\",
        ])
        .prop_map(<[u8]>::to_vec);
        let sgr = prop::collection::vec(
            prop_oneof![
                prop::sample::select(vec![
                    "0", "1", "2", "3", "4", "4:3", "4:4", "4:5", "21", "7", "8", "9", "22", "23",
                    "24", "27", "28", "29", "39", "49", "59",
                ])
                .prop_map(str::to_owned),
                (30u8..38).prop_map(|n| n.to_string()),
                (40u8..48).prop_map(|n| n.to_string()),
                (90u8..98).prop_map(|n| n.to_string()),
                (100u8..108).prop_map(|n| n.to_string()),
                any::<u8>().prop_map(|n| format!("38;5;{n}")),
                any::<u8>().prop_map(|n| format!("48;5;{n}")),
                any::<u8>().prop_map(|n| format!("58;5;{n}")),
                any::<[u8; 3]>().prop_map(|[r, g, b]| format!("38;2;{r};{g};{b}")),
                any::<[u8; 3]>().prop_map(|[r, g, b]| format!("48;2;{r};{g};{b}")),
            ],
            1..4,
        )
        .prop_map(|params| format!("\x1b[{}m", params.join(";")).into_bytes());
        let cup =
            (1u8..7, 1u8..14).prop_map(|(line, col)| format!("\x1b[{line};{col}H").into_bytes());
        prop_oneof![4 => text, 2 => control, 2 => sgr, 1 => cup]
    }

    proptest! {
        /// A repaint fed to a fresh model gives the same active screen, the
        /// same scrollback rows asked for, cursor, pen and modes (section
        /// 3.4).
        #[test]
        fn a_repaint_fed_to_a_fresh_model_gives_the_same_screen(
            pieces in prop::collection::vec(piece(), 0..80),
            history in 0usize..20,
        ) {
            let input = pieces.concat();
            let diff = differs(&input, history);
            prop_assert!(
                diff.is_none(),
                "{}: {}",
                String::from_utf8_lossy(&input).escape_debug(),
                diff.unwrap_or_default()
            );
        }
    }
}
