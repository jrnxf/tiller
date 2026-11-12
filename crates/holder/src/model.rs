//! A pane's screen model (`terminal-holder.md` 3.1, 3.4.1, 3.5).
//!
//! The model is an `alacritty_terminal` [`Term`] fed by a `vte`
//! [`Processor`]. The [`Scanner`] in front of it holds back a read's cut
//! sequence, so the parser is in its ground state between reads. Every
//! update runs inside `catch_unwind`: a panic resets this model only, and
//! the caller learns of it through [`ModelPanic`] to tell the child and the
//! pane's subscribers.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use tiller_panes::Size;

use crate::repaint;
use crate::scanner::Scanner;

/// How much of the latest input a panic report quotes.
pub const RECENT_INPUT: usize = 4 * 1024;

/// The fewest columns the model takes (`alacritty_terminal`'s minimum).
pub const MIN_COLS: u16 = 2;
/// The fewest rows the model takes.
pub const MIN_ROWS: u16 = 1;

/// The model's event listener. The slice drops every event: query answers
/// (`PtyWrite`), titles and bells come with H03 and H04.
#[derive(Clone, Copy, Debug, Default)]
pub struct Quiet;

impl EventListener for Quiet {}

/// A size in the form `alacritty_terminal` takes.
#[derive(Clone, Copy, Debug)]
struct GridSize(Size);

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        usize::from(self.0.rows)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.0.cols)
    }
}

/// A model update panicked. The model is fresh again, at the same size.
#[derive(Debug)]
pub struct ModelPanic {
    /// The panic's message.
    pub message: String,
    /// The last [`RECENT_INPUT`] bytes fed before it.
    pub recent: Vec<u8>,
}

/// The terminal state and its parser.
struct Screen {
    term: Term<Quiet>,
    parser: Processor,
}

impl Screen {
    fn new(size: Size) -> Self {
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &GridSize(size), Quiet),
            parser: Processor::new(),
        }
    }
}

/// One pane's screen.
pub struct Model {
    screen: Screen,
    scanner: Scanner,
    size: Size,
    /// The latest bytes fed, for a panic report.
    recent: Vec<u8>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Model {
    /// A blank screen of `size`, which must be at least
    /// [`MIN_COLS`] by [`MIN_ROWS`].
    pub fn new(size: Size) -> Self {
        Self {
            screen: Screen::new(size),
            scanner: Scanner::new(),
            size,
            recent: Vec::new(),
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Feeds the PTY's next read: its whole sequences now, its cut tail with
    /// the next read.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ModelPanic> {
        let whole = self.scanner.split(bytes);
        if whole.is_empty() {
            return Ok(());
        }
        self.remember(&whole);
        self.guarded(|screen| screen.parser.advance(&mut screen.term, &whole))
    }

    /// When a synchronized update (mode 2026) that the parser holds must
    /// end, if one is open.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.screen.parser.sync_timeout().sync_timeout()
    }

    /// The cut sequence at the end of the last read, which the model holds
    /// until the next read ends it.
    pub fn held(&self) -> &[u8] {
        self.scanner.held()
    }

    /// Ends the open synchronized update: its deadline passed.
    pub fn end_sync(&mut self) -> Result<(), ModelPanic> {
        self.guarded(|screen| screen.parser.stop_sync(&mut screen.term))
    }

    /// Sets the screen's size.
    pub fn resize(&mut self, size: Size) -> Result<(), ModelPanic> {
        self.size = size;
        self.guarded(|screen| screen.term.resize(GridSize(size)))
    }

    /// The bytes that draw this screen on an empty terminal of its size
    /// (section 3.4). A panic while rendering resets the model, as an update
    /// does.
    pub fn repaint(&mut self) -> Result<Vec<u8>, ModelPanic> {
        let mut out = Vec::new();
        self.guarded(|screen| out = repaint::render(&screen.term))?;
        Ok(out)
    }

    /// Runs `update` on the screen. A panic in it leaves a fresh screen at
    /// the same size, and says what panicked.
    fn guarded(&mut self, update: impl FnOnce(&mut Screen)) -> Result<(), ModelPanic> {
        let screen = &mut self.screen;
        match catch_unwind(AssertUnwindSafe(|| update(screen))) {
            Ok(()) => Ok(()),
            Err(payload) => {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a panic with no message".to_owned());
                self.screen = Screen::new(self.size);
                self.scanner = Scanner::new();
                Err(ModelPanic {
                    message,
                    recent: std::mem::take(&mut self.recent),
                })
            }
        }
    }

    fn remember(&mut self, bytes: &[u8]) {
        if bytes.len() >= RECENT_INPUT {
            self.recent.clear();
            self.recent
                .extend_from_slice(&bytes[bytes.len() - RECENT_INPUT..]);
            return;
        }
        let keep = RECENT_INPUT - bytes.len();
        if self.recent.len() > keep {
            self.recent.drain(..self.recent.len() - keep);
        }
        self.recent.extend_from_slice(bytes);
    }

    #[cfg(test)]
    pub(crate) fn term(&self) -> &Term<Quiet> {
        &self.screen.term
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::{Column, Line, Point};

    use super::*;

    fn size() -> Size {
        Size { cols: 20, rows: 4 }
    }

    fn row(model: &Model, line: i32) -> String {
        let grid = model.term().grid();
        (0..grid.columns())
            .map(|col| grid[Point::new(Line(line), Column(col))].c)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn a_sequence_cut_between_reads_is_fed_whole() {
        let mut model = Model::new(size());
        model.feed(b"ab\x1b[3").unwrap();
        assert_eq!(row(&model, 0), "ab");
        model.feed(b"1mcd\xe6\x97").unwrap();
        model.feed(b"\xa5").unwrap();
        assert_eq!(row(&model, 0), "abcd日");
        let grid = model.term().grid();
        assert_eq!(
            grid[Point::new(Line(0), Column(2))].fg,
            alacritty_terminal::vte::ansi::Color::Named(
                alacritty_terminal::vte::ansi::NamedColor::Red
            )
        );
    }

    #[test]
    fn a_panic_resets_the_model_at_its_size_and_quotes_the_input() {
        let mut model = Model::new(size());
        model.feed(b"before").unwrap();
        let err = model.guarded(|_| panic!("the model broke")).unwrap_err();
        assert_eq!(err.message, "the model broke");
        assert_eq!(err.recent, b"before");
        assert_eq!(row(&model, 0), "");
        assert_eq!(model.size(), size());
        model.feed(b"after").unwrap();
        assert_eq!(row(&model, 0), "after");
    }

    #[test]
    fn the_quoted_input_keeps_only_the_latest_bytes() {
        let mut model = Model::new(size());
        model.feed(&vec![b'a'; RECENT_INPUT]).unwrap();
        model.feed(b"\r\nzz").unwrap();
        let err = model.guarded(|_| panic!("x")).unwrap_err();
        assert_eq!(err.recent.len(), RECENT_INPUT);
        assert!(err.recent.ends_with(b"\r\nzz"));
    }

    #[test]
    fn a_synchronized_update_waits_for_its_end_or_its_deadline() {
        let mut model = Model::new(size());
        model.feed(b"\x1b[?2026hheld").unwrap();
        assert_eq!(row(&model, 0), "");
        assert!(model.sync_deadline().is_some());
        model.end_sync().unwrap();
        assert_eq!(row(&model, 0), "held");
        assert!(model.sync_deadline().is_none());
        model.feed(b"\x1b[?2026h more\x1b[?2026l").unwrap();
        assert_eq!(row(&model, 0), "held more");
    }
}
