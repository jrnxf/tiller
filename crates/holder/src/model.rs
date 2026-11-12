//! A pane's screen model (`terminal-holder.md` 3.1, 3.4.1, 3.5).
//!
//! The model is an `alacritty_terminal` [`Term`] fed by a `vte`
//! [`Processor`]. The [`Scanner`] in front of it holds back a read's cut
//! sequence, so the parser is in its ground state between reads. Every
//! update runs inside `catch_unwind`: a panic resets this model only, and
//! the caller learns of it through [`ModelPanic`] to tell the child and the
//! pane's subscribers.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use tiller_panes::Size;
use tiller_panes::message::{Format, Read, Screen as ScreenRead};

use crate::scanner::Scanner;
use crate::{repaint, screen};

/// How much of the latest input a panic report quotes.
pub const RECENT_INPUT: usize = 4 * 1024;

/// The fewest columns the model takes (`alacritty_terminal`'s minimum).
pub const MIN_COLS: u16 = 2;
/// The fewest rows the model takes.
pub const MIN_ROWS: u16 = 1;
/// The most columns the model takes. The grid is allocated whole, so a
/// size near `u16::MAX` would abort the holder on allocation, which no
/// `catch_unwind` stops. This is wider than any real screen.
pub const MAX_COLS: u16 = 1024;
/// The most rows the model takes.
pub const MAX_ROWS: u16 = 512;

/// What the app told its terminal, other than how to draw: the pane's
/// events (`terminal-holder.md` 2.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Told {
    /// The app set its title (OSC 0 or 2), or reset it to empty.
    Title(String),
    /// The app rang the bell.
    Bell,
    /// The app sent a notification (OSC 9 or OSC 777).
    Notify { title: String, body: String },
}

/// The model's event listener: it keeps titles and bells for the pane to
/// take after each update. Query answers (`PtyWrite`) come with H04.
#[derive(Clone, Debug, Default)]
pub struct Listener {
    told: Arc<Mutex<Vec<Told>>>,
}

impl Listener {
    fn push(&self, told: Told) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(told);
    }

    fn take(&self) -> Vec<Told> {
        std::mem::take(&mut *self.told.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => self.push(Told::Title(title)),
            Event::ResetTitle => self.push(Told::Title(String::new())),
            Event::Bell => self.push(Told::Bell),
            _ => {}
        }
    }
}

/// The notifications in `bytes`, which hold whole sequences only: OSC 9
/// (`ESC ] 9 ; body`) and OSC 777 (`ESC ] 777 ; notify ; title ; body`),
/// each ended by BEL or ST. ConEmu's OSC 9 commands (`9;1` to `9;12`, such
/// as the progress bar `9;4`) are not notifications.
fn notifications(bytes: &[u8]) -> Vec<Told> {
    let mut found = Vec::new();
    let mut rest = bytes;
    while let Some(start) = rest.windows(2).position(|pair| pair == b"\x1b]") {
        rest = &rest[start + 2..];
        let end = rest
            .iter()
            .position(|&b| b == 0x07 || b == 0x1b)
            .unwrap_or(rest.len());
        let body = String::from_utf8_lossy(&rest[..end]);
        rest = &rest[end..];
        if let Some(text) = body.strip_prefix("9;") {
            let command = text.split(';').next().unwrap_or_default();
            let conemu = command
                .parse::<u8>()
                .is_ok_and(|n| (1..=12).contains(&n) && !command.starts_with('0'));
            if !conemu {
                found.push(Told::Notify {
                    title: String::new(),
                    body: text.to_owned(),
                });
            }
        } else if let Some(text) = body.strip_prefix("777;notify;") {
            let (title, body) = text.split_once(';').unwrap_or((text, ""));
            found.push(Told::Notify {
                title: title.to_owned(),
                body: body.to_owned(),
            });
        }
    }
    found
}

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
    term: Term<Listener>,
    parser: Processor,
}

impl Screen {
    fn new(size: Size, listener: Listener) -> Self {
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &GridSize(size), listener),
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
    /// What the app told the terminal since the last [`Model::take_told`].
    listener: Listener,
    /// Makes the next update panic, for the tests of section 3.5.
    #[cfg(test)]
    fault: bool,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Model {
    /// A blank screen of `size`, which must be between [`MIN_COLS`] by
    /// [`MIN_ROWS`] and [`MAX_COLS`] by [`MAX_ROWS`].
    pub fn new(size: Size) -> Self {
        let listener = Listener::default();
        Self {
            screen: Screen::new(size, listener.clone()),
            scanner: Scanner::new(),
            size,
            recent: Vec::new(),
            listener,
            #[cfg(test)]
            fault: false,
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
        for told in notifications(&whole) {
            self.listener.push(told);
        }
        self.guarded(|screen| screen.parser.advance(&mut screen.term, &whole))
    }

    /// What the app told the terminal since the last call: titles, bells
    /// and notifications, in order.
    pub fn take_told(&self) -> Vec<Told> {
        self.listener.take()
    }

    /// The terminal modes the app has on.
    pub fn mode(&self) -> TermMode {
        *self.screen.term.mode()
    }

    /// A `screen` read (section 3.3). `read` must be in range. A panic while
    /// reading resets the model, as an update does.
    pub fn screen(&mut self, read: Read, format: Format) -> Result<ScreenRead, ModelPanic> {
        self.guarded(|screen| screen::read(&screen.term, read, format))
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
        self.guarded(|screen| repaint::render(&screen.term))
    }

    /// Runs `update` on the screen. A panic in it leaves a fresh screen at
    /// the same size, and says what panicked.
    fn guarded<R>(&mut self, update: impl FnOnce(&mut Screen) -> R) -> Result<R, ModelPanic> {
        let screen = &mut self.screen;
        #[cfg(test)]
        let fault = std::mem::take(&mut self.fault);
        match catch_unwind(AssertUnwindSafe(|| {
            #[cfg(test)]
            if fault {
                panic!("a fault the test asked for");
            }
            update(screen)
        })) {
            Ok(out) => Ok(out),
            Err(payload) => {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "a panic with no message".to_owned());
                self.screen = Screen::new(self.size, self.listener.clone());
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
    pub(crate) fn term(&self) -> &Term<Listener> {
        &self.screen.term
    }

    /// Makes the next update panic inside its `catch_unwind`.
    #[cfg(test)]
    pub(crate) fn fail_next(&mut self) {
        self.fault = true;
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
        let err = model
            .guarded::<()>(|_| panic!("the model broke"))
            .unwrap_err();
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
        let err = model.guarded::<()>(|_| panic!("x")).unwrap_err();
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

    #[test]
    fn titles_bells_and_notifications_are_told_in_order() {
        let mut model = Model::new(size());
        model
            .feed(b"\x1b]2;build\x07a\x07\x1b]9;done\x1b\\\x1b]777;notify;Claude;needs you\x07")
            .unwrap();
        model.feed(b"\x1b]0;\x07").unwrap();
        assert_eq!(
            model.take_told(),
            [
                // The notifications are found before the parser runs.
                Told::Notify {
                    title: String::new(),
                    body: "done".into()
                },
                Told::Notify {
                    title: "Claude".into(),
                    body: "needs you".into()
                },
                Told::Title("build".into()),
                Told::Bell,
                Told::Title(String::new()),
            ]
        );
        assert!(model.take_told().is_empty());
    }

    #[test]
    fn conemu_osc_9_commands_are_not_notifications() {
        let mut model = Model::new(size());
        model
            .feed(b"\x1b]9;4;1;50\x07\x1b]9;12\x07\x1b]9;13;x\x07\x1b]9;04\x07")
            .unwrap();
        assert_eq!(
            model.take_told(),
            [
                Told::Notify {
                    title: String::new(),
                    body: "13;x".into()
                },
                Told::Notify {
                    title: String::new(),
                    body: "04".into()
                },
            ]
        );
    }

    #[test]
    fn a_screen_read_that_panics_resets_the_model() {
        let mut model = Model::new(size());
        model.feed(b"before").unwrap();
        model.fail_next();
        let err = model.screen(Read::Visible, Format::Text).unwrap_err();
        assert_eq!(err.message, "a fault the test asked for");
        let screen = model.screen(Read::Visible, Format::Text).unwrap();
        assert_eq!(screen.rows_text, ["", "", "", ""]);
    }
}
