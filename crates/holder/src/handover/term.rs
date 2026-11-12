//! A `Term`'s whole state, read out and written back (`terminal-holder.md`
//! 6.2).
//!
//! Every part is read through a public getter, some of them from the
//! patched crate (`vendor/README.md`), and written back with the term's
//! own `Handler` methods or a setter. What the record leaves out has no
//! effect the holder can see: the title (the holder's listener drops it,
//! and the model's teller keeps its own), damage, the selection, vi mode
//! and the display offset, which the holder never moves.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Cursor, Dimensions, Grid};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::term::color::COUNT;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{
    CharsetIndex, Color, CursorShape, CursorStyle, Handler, KeyboardModes,
    KeyboardModesApplyBehavior, NamedColor, Rgb, StandardCharset,
};

use super::RecordError;
use super::codec::{Reader, Writer};
use crate::model::{MAX_COLS, MAX_ROWS, SCROLLBACK};

/// The most kitty keyboard modes a stack in the record holds. The term's
/// own bound (4096) does not hold (its push removes from the wrong stack),
/// so the record allows more.
const KEYBOARD_MAX: usize = 1 << 16;
/// The most characters a cell stacks on its first.
const ZEROWIDTH_MAX: usize = 1 << 12;
/// The most bytes of a hyperlink's id or URI.
const LINK_MAX: usize = 1 << 20;

/// One grid: its rows and its two cursors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridState {
    /// The scrollback, oldest first, then the screen, top first. Each row
    /// is cut after its last cell that is not `Cell::default()`.
    pub rows: Vec<Vec<Cell>>,
    pub cursor: Cursor<Cell>,
    pub saved: Cursor<Cell>,
}

/// Everything a `Term` keeps that the holder can see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermState {
    pub mode: TermMode,
    pub style: CursorStyle,
    pub primary: GridState,
    pub alternate: GridState,
    /// The active screen's kitty keyboard mode stack, bottom first.
    pub keyboard: Vec<KeyboardModes>,
    /// The other screen's stack.
    pub inactive_keyboard: Vec<KeyboardModes>,
    pub tabs: Vec<bool>,
    pub scroll_region: (i32, i32),
    pub active_charset: CharsetIndex,
    /// The colors the app set, by index.
    pub colors: Vec<(usize, Rgb)>,
}

fn grid_state(grid: &Grid<Cell>) -> GridState {
    let history = i32::try_from(grid.history_size()).unwrap_or(i32::MAX);
    let screen = i32::try_from(grid.screen_lines()).unwrap_or(i32::MAX);
    let default = Cell::default();
    let rows = (-history..screen)
        .map(|line| {
            let row = &grid[Line(line)];
            let cells = (0..grid.columns())
                .map(|col| &row[Column(col)])
                .collect::<Vec<_>>();
            let len = cells
                .iter()
                .rposition(|cell| **cell != default)
                .map_or(0, |last| last + 1);
            cells[..len].iter().map(|cell| (*cell).clone()).collect()
        })
        .collect();
    GridState {
        rows,
        cursor: grid.cursor.clone(),
        saved: grid.saved_cursor.clone(),
    }
}

/// Reads `term` out whole.
pub fn capture<T>(term: &Term<T>) -> TermState {
    let alt = term.mode().contains(TermMode::ALT_SCREEN);
    let (active, inactive) = (grid_state(term.grid()), grid_state(term.inactive_grid()));
    let (primary, alternate) = if alt {
        (inactive, active)
    } else {
        (active, inactive)
    };
    let region = term.scroll_region();
    let colors = term.colors();
    TermState {
        mode: *term.mode(),
        style: term.cursor_style(),
        primary,
        alternate,
        keyboard: term.keyboard_mode_stack().to_vec(),
        inactive_keyboard: term.inactive_keyboard_mode_stack().to_vec(),
        tabs: term.tab_stops().to_vec(),
        scroll_region: (region.start.0, region.end.0),
        active_charset: term.active_charset(),
        colors: (0..COUNT)
            .filter_map(|index| colors[index].map(|rgb| (index, rgb)))
            .collect(),
    }
}

/// Writes `rows` into `grid`, the scrollback through the grid's own
/// scrolling, so the history is what the grid would have kept.
fn write_grid(grid: &mut Grid<Cell>, state: &GridState) {
    let lines = grid.screen_lines();
    let cols = grid.columns();
    let write = |grid: &mut Grid<Cell>, line: usize, cells: &[Cell]| {
        let row = &mut grid[Line(i32::try_from(line).unwrap_or(0))];
        for col in 0..cols {
            row[Column(col)] = cells.get(col).cloned().unwrap_or_default();
        }
    };
    // New rows come in blank.
    grid.cursor = Cursor::default();
    let (first, rest) = state.rows.split_at(lines.min(state.rows.len()));
    for (line, cells) in first.iter().enumerate() {
        write(grid, line, cells);
    }
    let all = Line(i32::try_from(lines).unwrap_or(0));
    for chunk in rest.chunks(lines.max(1)) {
        grid.scroll_up(&(Line(0)..all), chunk.len());
        for (at, cells) in chunk.iter().enumerate() {
            write(grid, lines - chunk.len() + at, cells);
        }
    }
    grid.cursor = state.cursor.clone();
    grid.saved_cursor = state.saved.clone();
}

/// Writes `state` into `term`, a fresh one of the same size, after its
/// modes were set by bytes (`repaint::mode_bytes`). The kitty keyboard
/// stacks and mode, the cursor style, the tab stops, the scroll region,
/// the active charset, the colors and both grids are set here.
pub fn restore<T: EventListener>(term: &mut Term<T>, state: &TermState) {
    if state.style != CursorStyle::default() {
        term.set_cursor_style(Some(state.style));
    }
    term.set_keyboard_mode_stacks(state.keyboard.clone(), state.inactive_keyboard.clone());
    let kitty = (state.mode & TermMode::KITTY_KEYBOARD_PROTOCOL).bits() >> 18;
    Handler::set_keyboard_mode(
        term,
        KeyboardModes::from_bits_truncate(u8::try_from(kitty).unwrap_or(0)),
        KeyboardModesApplyBehavior::Replace,
    );
    term.set_tab_stops(state.tabs.clone());
    term.set_scroll_region(Line(state.scroll_region.0)..Line(state.scroll_region.1));
    term.set_active_charset(state.active_charset);
    for &(index, rgb) in &state.colors {
        term.set_color(index, rgb);
    }
    let alt = state.mode.contains(TermMode::ALT_SCREEN);
    let (active, inactive) = if alt {
        (&state.alternate, &state.primary)
    } else {
        (&state.primary, &state.alternate)
    };
    write_grid(term.grid_mut(), active);
    write_grid(term.inactive_grid_mut(), inactive);
}

const NAMED: [NamedColor; 29] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
    NamedColor::Foreground,
    NamedColor::Background,
    NamedColor::Cursor,
    NamedColor::DimBlack,
    NamedColor::DimRed,
    NamedColor::DimGreen,
    NamedColor::DimYellow,
    NamedColor::DimBlue,
    NamedColor::DimMagenta,
    NamedColor::DimCyan,
    NamedColor::DimWhite,
    NamedColor::BrightForeground,
    NamedColor::DimForeground,
];

fn put_color(w: &mut Writer, color: Color) {
    match color {
        Color::Named(named) => {
            w.u8(0);
            w.usize(named as usize);
        }
        Color::Spec(rgb) => {
            w.u8(1);
            w.raw(&[rgb.r, rgb.g, rgb.b]);
        }
        Color::Indexed(index) => {
            w.u8(2);
            w.u8(index);
        }
    }
}

fn get_rgb(r: &mut Reader<'_>) -> Result<Rgb, RecordError> {
    let raw = r.raw(3)?;
    Ok(Rgb {
        r: raw[0],
        g: raw[1],
        b: raw[2],
    })
}

fn get_color(r: &mut Reader<'_>) -> Result<Color, RecordError> {
    Ok(match r.u8()? {
        0 => {
            let index = r.uvar()?;
            Color::Named(
                NAMED
                    .into_iter()
                    .find(|&named| named as u64 == index)
                    .ok_or(RecordError::Bad("a color"))?,
            )
        }
        1 => Color::Spec(get_rgb(r)?),
        2 => Color::Indexed(r.u8()?),
        _ => return Err(RecordError::Bad("a color")),
    })
}

/// A cell, with its colors and flags left out when they are those of the
/// cell before it and neither has extras: most cells are then one or two
/// bytes.
fn put_cell(w: &mut Writer, cell: &Cell, before: &Cell) {
    let same = cell.extra.is_none()
        && before.extra.is_none()
        && cell.fg == before.fg
        && cell.bg == before.bg
        && cell.flags == before.flags;
    w.uvar((u64::from(u32::from(cell.c)) << 1) | u64::from(same));
    if same {
        return;
    }
    put_color(w, cell.fg);
    put_color(w, cell.bg);
    w.uvar(u64::from(cell.flags.bits()));
    w.option(cell.extra.as_deref(), |w, _| {
        let zerowidth = cell.zerowidth().unwrap_or_default();
        w.usize(zerowidth.len());
        for &c in zerowidth {
            w.char(c);
        }
        w.option(cell.underline_color(), put_color);
        w.option(cell.hyperlink(), |w, link| {
            w.str(link.id());
            w.str(link.uri());
        });
    });
}

fn get_cell(r: &mut Reader<'_>, before: &Cell) -> Result<Cell, RecordError> {
    let head = r.uvar()?;
    let c = u32::try_from(head >> 1)
        .ok()
        .and_then(char::from_u32)
        .ok_or(RecordError::Bad("a character"))?;
    if head & 1 == 1 {
        return Ok(Cell {
            c,
            fg: before.fg,
            bg: before.bg,
            flags: before.flags,
            extra: None,
        });
    }
    let fg = get_color(r)?;
    let bg = get_color(r)?;
    let flags = u16::try_from(r.uvar()?)
        .ok()
        .and_then(Flags::from_bits)
        .ok_or(RecordError::Bad("cell flags"))?;
    let mut cell = Cell::default();
    let extra = r.option(|r| {
        let count = r.len(ZEROWIDTH_MAX, "zero-width characters")?;
        let zerowidth = (0..count)
            .map(|_| r.char())
            .collect::<Result<Vec<_>, _>>()?;
        let underline = r.option(get_color)?;
        let link = r.option(|r| {
            let id = r.str(LINK_MAX, "a hyperlink id")?;
            let uri = r.str(LINK_MAX, "a hyperlink")?;
            Ok(Hyperlink::new(Some(id), uri))
        })?;
        Ok((zerowidth, underline, link))
    })?;
    if let Some((zerowidth, underline, link)) = extra {
        // An extra that is there but empty, as a cleared wide character
        // leaves one.
        cell.push_zerowidth(' ');
        cell.clear_wide();
        if underline.is_some() {
            cell.set_underline_color(underline);
        }
        if link.is_some() {
            cell.set_hyperlink(link);
        }
        for c in zerowidth {
            cell.push_zerowidth(c);
        }
    }
    cell.c = c;
    cell.fg = fg;
    cell.bg = bg;
    cell.flags = flags;
    Ok(cell)
}

fn put_cursor(w: &mut Writer, cursor: &Cursor<Cell>) {
    w.ivar(i64::from(cursor.point.line.0));
    w.usize(cursor.point.column.0);
    put_cell(w, &cursor.template, &Cell::default());
    for index in CHARSET_INDEXES {
        w.u8(match cursor.charsets[index] {
            StandardCharset::Ascii => 0,
            StandardCharset::SpecialCharacterAndLineDrawing => 1,
        });
    }
    w.bool(cursor.input_needs_wrap);
}

const CHARSET_INDEXES: [CharsetIndex; 4] = [
    CharsetIndex::G0,
    CharsetIndex::G1,
    CharsetIndex::G2,
    CharsetIndex::G3,
];

fn get_cursor(r: &mut Reader<'_>, lines: usize, cols: usize) -> Result<Cursor<Cell>, RecordError> {
    let line = i32::try_from(r.ivar()?)
        .ok()
        .filter(|&line| usize::try_from(line).is_ok_and(|line| line < lines))
        .ok_or(RecordError::Bad("a cursor line"))?;
    let column = r.len(cols.saturating_sub(1), "a cursor column")?;
    let mut cursor = Cursor {
        point: Point::new(Line(line), Column(column)),
        template: get_cell(r, &Cell::default())?,
        ..Cursor::default()
    };
    for index in CHARSET_INDEXES {
        cursor.charsets[index] = match r.u8()? {
            0 => StandardCharset::Ascii,
            1 => StandardCharset::SpecialCharacterAndLineDrawing,
            _ => return Err(RecordError::Bad("a charset")),
        };
    }
    cursor.input_needs_wrap = r.bool()?;
    Ok(cursor)
}

fn put_grid(w: &mut Writer, grid: &GridState) {
    w.usize(grid.rows.len());
    for row in &grid.rows {
        w.usize(row.len());
        let mut before = Cell::default();
        for cell in row {
            put_cell(w, cell, &before);
            before = cell.clone();
        }
    }
    put_cursor(w, &grid.cursor);
    put_cursor(w, &grid.saved);
}

fn get_grid(
    r: &mut Reader<'_>,
    lines: usize,
    cols: usize,
    history: usize,
) -> Result<GridState, RecordError> {
    let count = r.len(lines + history, "rows")?;
    if count < lines {
        return Err(RecordError::Bad("a grid with too few rows"));
    }
    let mut rows = Vec::with_capacity(count);
    for _ in 0..count {
        let len = r.len(cols, "a row")?;
        let mut row = Vec::with_capacity(len);
        let mut before = Cell::default();
        for _ in 0..len {
            let cell = get_cell(r, &before)?;
            before = cell.clone();
            row.push(cell);
        }
        rows.push(row);
    }
    Ok(GridState {
        rows,
        cursor: get_cursor(r, lines, cols)?,
        saved: get_cursor(r, lines, cols)?,
    })
}

fn put_keyboard(w: &mut Writer, stack: &[KeyboardModes]) {
    w.usize(stack.len());
    for modes in stack {
        w.u8(modes.bits());
    }
}

fn get_keyboard(r: &mut Reader<'_>) -> Result<Vec<KeyboardModes>, RecordError> {
    let len = r.len(KEYBOARD_MAX, "a keyboard mode stack")?;
    (0..len)
        .map(|_| KeyboardModes::from_bits(r.u8()?).ok_or(RecordError::Bad("keyboard modes")))
        .collect()
}

pub fn put(w: &mut Writer, term: &TermState) {
    w.uvar(u64::from(term.mode.bits()));
    w.u8(match term.style.shape {
        CursorShape::Block => 0,
        CursorShape::Underline => 1,
        CursorShape::Beam => 2,
        CursorShape::HollowBlock => 3,
        CursorShape::Hidden => 4,
    });
    w.bool(term.style.blinking);
    put_grid(w, &term.primary);
    put_grid(w, &term.alternate);
    put_keyboard(w, &term.keyboard);
    put_keyboard(w, &term.inactive_keyboard);
    w.usize(term.tabs.len());
    for &tab in &term.tabs {
        w.bool(tab);
    }
    w.ivar(i64::from(term.scroll_region.0));
    w.ivar(i64::from(term.scroll_region.1));
    w.u8(term.active_charset as u8);
    w.usize(term.colors.len());
    for &(index, rgb) in &term.colors {
        w.usize(index);
        w.raw(&[rgb.r, rgb.g, rgb.b]);
    }
}

/// A term's state for a screen of `lines` by `cols`.
pub fn get(r: &mut Reader<'_>, lines: usize, cols: usize) -> Result<TermState, RecordError> {
    let mode = u32::try_from(r.uvar()?)
        .ok()
        .and_then(TermMode::from_bits)
        .filter(|mode| !mode.contains(TermMode::VI))
        .ok_or(RecordError::Bad("terminal modes"))?;
    let shape = match r.u8()? {
        0 => CursorShape::Block,
        1 => CursorShape::Underline,
        2 => CursorShape::Beam,
        3 => CursorShape::HollowBlock,
        4 => CursorShape::Hidden,
        _ => return Err(RecordError::Bad("a cursor shape")),
    };
    let style = CursorStyle {
        shape,
        blinking: r.bool()?,
    };
    let primary = get_grid(r, lines, cols, SCROLLBACK)?;
    let alternate = get_grid(r, lines, cols, 0)?;
    let keyboard = get_keyboard(r)?;
    let inactive_keyboard = get_keyboard(r)?;
    let tab_count = r.len(usize::from(MAX_COLS), "tab stops")?;
    let tabs = (0..tab_count).map(|_| r.bool()).collect::<Result<_, _>>()?;
    let top = r.ivar()?;
    let bottom = r.ivar()?;
    let scroll_region = match (i32::try_from(top), i32::try_from(bottom)) {
        (Ok(top), Ok(bottom))
            if 0 <= top && top < bottom && usize::try_from(bottom).is_ok_and(|b| b <= lines) =>
        {
            (top, bottom)
        }
        _ => return Err(RecordError::Bad("a scroll region")),
    };
    let active_charset = match r.u8()? {
        0 => CharsetIndex::G0,
        1 => CharsetIndex::G1,
        2 => CharsetIndex::G2,
        3 => CharsetIndex::G3,
        _ => return Err(RecordError::Bad("a charset")),
    };
    let color_count = r.len(COUNT, "colors")?;
    let mut colors = Vec::with_capacity(color_count);
    for _ in 0..color_count {
        let index = r.len(COUNT - 1, "a color index")?;
        colors.push((index, get_rgb(r)?));
    }
    Ok(TermState {
        mode,
        style,
        primary,
        alternate,
        keyboard,
        inactive_keyboard,
        tabs,
        scroll_region,
        active_charset,
        colors,
    })
}

/// Whether a screen of `lines` by `cols` is one the model takes.
pub fn fits(lines: usize, cols: usize) -> bool {
    (1..=usize::from(MAX_ROWS)).contains(&lines) && (2..=usize::from(MAX_COLS)).contains(&cols)
}
