//! A copy of the part of a model that a repaint or a `screen` read draws
//! (`terminal-holder.md` 3.3, 3.4). It is taken under the pane's lock and
//! formatted after it, so a big screen holds up neither the PTY reader
//! nor the pane's other requests while it is formatted.

use alacritty_terminal::grid::{Cursor, Dimensions, Row};
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::CursorStyle;

/// The rows, cursor and modes of one model.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// The scrollback rows taken, oldest first, then the screen's rows.
    rows: Vec<Row<Cell>>,
    history: usize,
    lines: usize,
    cols: usize,
    pub cursor: Cursor<Cell>,
    pub mode: TermMode,
    pub style: CursorStyle,
}

impl Snapshot {
    /// The active screen of `term`, and up to `history` rows of its
    /// scrollback above it.
    pub fn of<T>(term: &Term<T>, history: usize) -> Self {
        let grid = term.grid();
        let history = history.min(grid.history_size());
        let lines = grid.screen_lines();
        let first = -i32::try_from(history).unwrap_or(i32::MAX);
        let last = i32::try_from(lines).unwrap_or(i32::MAX);
        Self {
            rows: (first..last).map(|line| grid[Line(line)].clone()).collect(),
            history,
            lines,
            cols: grid.columns(),
            cursor: grid.cursor.clone(),
            mode: *term.mode(),
            style: term.cursor_style(),
        }
    }

    /// The scrollback rows taken.
    pub fn history(&self) -> usize {
        self.history
    }

    pub fn screen_lines(&self) -> usize {
        self.lines
    }

    pub fn columns(&self) -> usize {
        self.cols
    }

    /// The row at `line`: the screen's from 0 down, the scrollback's from
    /// -1 up. It must be one that was taken.
    pub fn row(&self, line: i32) -> &Row<Cell> {
        let history = i32::try_from(self.history).unwrap_or(i32::MAX);
        &self.rows[usize::try_from(line + history).unwrap_or(0)]
    }
}
