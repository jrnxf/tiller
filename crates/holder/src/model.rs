//! A pane's screen model (`terminal-holder.md` 3.1, 3.4.1, 3.5).
//!
//! The model is an `alacritty_terminal` [`Term`] fed by a `vte`
//! [`Processor`]. The [`Scanner`] in front of it holds back a read's cut
//! sequence, so the parser is in its ground state between reads. Every
//! update runs inside `catch_unwind`: a panic resets this model only, and
//! the caller learns of it through [`ModelPanic`] to tell the child and the
//! pane's subscribers.
//!
//! The model answers the app's queries as a terminal does (section 2.7):
//! its [`Listener`] keeps what the term would write back, and
//! [`Model::take_answers`] hands those bytes to the caller for the PTY.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorStyle, Processor};
use alacritty_terminal::vte::{Params, Parser, Perform};
use tiller_panes::Size;
use tiller_panes::message::{Format, Read, Screen as ScreenRead};

use crate::handover::RecordError;
use crate::handover::term::{self, TermState};
use crate::query::Listener;
use crate::scanner::{Scanner, ScannerState};
use crate::snapshot::Snapshot;
use crate::{repaint, screen};

/// What starts a synchronized update (mode 2026).
const BSU: &[u8] = b"\x1b[?2026h";

/// Everything a pane's model keeps, as a live upgrade hands it over
/// (`terminal-holder.md` 6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelState {
    pub size: Size,
    pub term: TermState,
    /// The character `CSI b` repeats.
    pub preceding: Option<char>,
    /// The bytes of the open synchronized update, which the term has not
    /// had yet.
    pub sync: Option<Vec<u8>>,
    pub teller: TellerState,
    pub scanner: ScannerState,
}

/// What the model's [`Teller`] keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TellerState {
    pub title: String,
    /// The title stack, bottom first.
    pub stack: Vec<String>,
    /// What was told and not yet taken.
    pub told: Vec<Told>,
    /// The last character printed, the sync update's bytes included.
    pub last: Option<char>,
}

/// The scrollback rows the model keeps (section 2.8).
pub const SCROLLBACK: usize = 10_000;

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

/// The most titles the title stack keeps, as `alacritty_terminal` keeps.
pub const TITLE_STACK: usize = 4096;

/// The most notifications told between two takes.
pub const NOTIFY_MAX: usize = 8;

/// The most bytes of a title, or of a notification's title or body, that
/// is told. The rest is cut, so each event stays small.
pub const TEXT_MAX: usize = 4 * 1024;

/// `text` cut to [`TEXT_MAX`] bytes, at a character's start.
fn capped(mut text: String) -> String {
    if text.len() > TEXT_MAX {
        let mut end = TEXT_MAX;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// Reads what the app tells its terminal, in the order it tells it, from
/// the bytes the model parses: titles (OSC 0 and 2, the title stack of
/// `CSI 22 t` and `CSI 23 t`, and the reset `ESC c`), the bell, and
/// notifications (OSC 9 and OSC 777). It has a parser of its own, so an
/// OSC cut between reads is read whole, and CAN, SUB or ESC ends an OSC
/// where they end it for the model. It reads a synchronized update
/// (mode 2026) as it comes: the model parses that only when it ends.
///
/// One PTY read can tell a flood (`yes $'\a'` rings a bell every two
/// bytes), and each thing told is an event to every watcher. So between
/// two takes it keeps the latest title only, one bell and the first
/// [`NOTIFY_MAX`] notifications. That bounds one read, not a rate: a
/// flood over many reads tells this much for each read. Its parser splits an OSC at `;` into 16
/// parts at most, as the model's does, so a body with more than 13 `;` in
/// it loses its end.
///
/// It also keeps the last character printed, the one `CSI b` repeats: the
/// model's parser keeps it where nothing can read it, and a live upgrade
/// hands it over.
#[derive(Debug, Default)]
struct Teller {
    title: String,
    stack: VecDeque<String>,
    told: Vec<Told>,
    last: Option<char>,
}

impl Teller {
    fn tell(&mut self, told: Told) {
        match told {
            Told::Title(_) => self.told.retain(|old| !matches!(old, Told::Title(_))),
            Told::Bell if self.told.contains(&Told::Bell) => return,
            Told::Notify { .. }
                if self
                    .told
                    .iter()
                    .filter(|old| matches!(old, Told::Notify { .. }))
                    .count()
                    >= NOTIFY_MAX =>
            {
                return;
            }
            Told::Bell | Told::Notify { .. } => {}
        }
        self.told.push(told);
    }

    fn set_title(&mut self, title: String) {
        let title = capped(title);
        self.title.clone_from(&title);
        self.tell(Told::Title(title));
    }
}

impl Perform for Teller {
    fn print(&mut self, c: char) {
        self.last = Some(c);
    }

    fn execute(&mut self, byte: u8) {
        if byte == 0x07 {
            self.tell(Told::Bell);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let text = |parts: &[&[u8]]| {
            parts
                .iter()
                .map(|part| String::from_utf8_lossy(part))
                .collect::<Vec<_>>()
                .join(";")
        };
        let Some((&kind, rest)) = params.split_first() else {
            return;
        };
        match kind {
            // The title as `alacritty_terminal` reads it: the parts that
            // are UTF-8, trimmed.
            b"0" | b"2" if !rest.is_empty() => {
                let title = rest
                    .iter()
                    .flat_map(|part| std::str::from_utf8(part))
                    .collect::<Vec<_>>()
                    .join(";");
                self.set_title(title.trim().to_owned());
            }
            // ConEmu's OSC 9 commands (`9;1` to `9;12`, such as the
            // progress bar `9;4`) are not notifications.
            b"9" => {
                let Some(first) = rest.first() else {
                    return;
                };
                let conemu = std::str::from_utf8(first).is_ok_and(|command| {
                    !command.starts_with('0')
                        && command.parse::<u8>().is_ok_and(|n| (1..=12).contains(&n))
                });
                if !conemu {
                    self.tell(Told::Notify {
                        title: String::new(),
                        body: capped(text(rest)),
                    });
                }
            }
            b"777" => {
                if let [b"notify", title, body @ ..] = rest {
                    self.tell(Told::Notify {
                        title: capped(String::from_utf8_lossy(title).into_owned()),
                        body: capped(text(body)),
                    });
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if action != 't' || ignore || !intermediates.is_empty() {
            return;
        }
        match params.iter().next().map_or(0, |param| param[0]) {
            22 => {
                if self.stack.len() >= TITLE_STACK {
                    self.stack.pop_front();
                }
                self.stack.push_back(self.title.clone());
            }
            23 => {
                if let Some(title) = self.stack.pop_back() {
                    self.set_title(title);
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if byte == b'c' && !ignore && intermediates.is_empty() {
            self.stack.clear();
            self.set_title(String::new());
        }
    }
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
            scrolling_history: SCROLLBACK,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &GridSize(size), listener),
            parser: Processor::new(),
        }
    }

    /// A blank screen with `mode` and `style` set. Should setting them
    /// panic, the screen is blank with none.
    fn with_modes(size: Size, mode: TermMode, style: CursorStyle, listener: &Listener) -> Self {
        let bytes = repaint::mode_bytes(mode, style);
        let mut screen = Screen::new(size, listener.clone());
        let set = catch_unwind(AssertUnwindSafe(|| {
            screen.parser.advance(&mut screen.term, &bytes);
        }));
        if set.is_err() {
            screen = Screen::new(size, listener.clone());
        }
        screen
    }
}

/// What the model's parser holds of an open synchronized update (mode
/// 2026), which it keeps where nothing can read it: a live upgrade hands
/// this over (`terminal-holder.md` 6.2).
#[derive(Debug)]
struct SyncMirror {
    /// The bytes the parser holds, the latest it was fed.
    kept: Vec<u8>,
    /// The last character the parser printed before them.
    preceding: Option<char>,
    /// Whether the parser was in its ground state where `kept` starts, so
    /// that a new parser fed `kept` after the update's start is in the
    /// same state. Only a sequence the update's start cut short, such as a
    /// UTF-8 character or an OSC, makes it false.
    exact: bool,
}

/// One pane's screen.
pub struct Model {
    screen: Screen,
    teller: Teller,
    teller_parser: Parser,
    /// The open synchronized update, if one is open.
    sync: Option<SyncMirror>,
    /// What the term writes back to the app, shared with `screen.term`.
    listener: Listener,
    scanner: Scanner,
    size: Size,
    /// The latest bytes fed, for a panic report.
    recent: Vec<u8>,
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
            teller: Teller::default(),
            teller_parser: Parser::new(),
            sync: None,
            listener,
            scanner: Scanner::new(),
            size,
            recent: Vec::new(),
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
        let overflowed = self.scanner.overflowed();
        let whole = self.scanner.split(bytes);
        if whole.is_empty() {
            return Ok(());
        }
        self.remember(&whole);
        let before = self.sync.take();
        let fed = self.guarded(|screen| screen.parser.advance(&mut screen.term, &whole));
        if fed.is_err() {
            self.tell(&whole);
            self.teller.last = None;
            return fed;
        }
        self.follow(&whole, before, overflowed);
        Ok(())
    }

    /// The teller reads `bytes`. A panic in it starts it afresh.
    fn tell(&mut self, bytes: &[u8]) {
        let read = catch_unwind(AssertUnwindSafe(|| {
            self.teller_parser.advance(&mut self.teller, bytes);
        }));
        if read.is_err() {
            self.teller = Teller::default();
            self.teller_parser = Parser::new();
        }
    }

    /// The teller reads `whole`, which the model's parser was just fed, and
    /// the sync mirror follows what the parser held of it. `before` is the
    /// mirror before the feed. `overflowed` is whether `whole` went on a
    /// sequence over the scanner's bound, so it did not start in the ground
    /// state.
    fn follow(&mut self, whole: &[u8], before: Option<SyncMirror>, overflowed: bool) {
        if self.sync_deadline().is_none() {
            self.tell(whole);
            return;
        }
        let count = self.screen.parser.sync_bytes_count();
        let mut sync = match before {
            Some(mut sync) if count == sync.kept.len() + whole.len() => {
                // Held whole: the parser read none of it.
                sync.kept.extend_from_slice(whole);
                self.tell(whole);
                self.sync = Some(sync);
                return;
            }
            Some(sync) => sync,
            None => SyncMirror {
                kept: Vec::new(),
                preceding: None,
                exact: !overflowed,
            },
        };
        // The parser read up to the last `count` bytes and holds those: the
        // update's start, or a new start inside it, is there. The buffer is
        // always the end of what the parser was fed.
        let parsed_new = whole.len().saturating_sub(count);
        self.tell(&whole[..parsed_new]);
        sync.preceding = self.teller.last;
        self.tell(&whole[parsed_new..]);
        sync.kept.extend_from_slice(whole);
        let Some(parsed) = sync.kept.len().checked_sub(count) else {
            sync.exact = false;
            self.sync = Some(sync);
            return;
        };
        sync.exact &= crate::scanner::ends_in_ground(&sync.kept[..parsed]);
        sync.kept.drain(..parsed);
        self.sync = Some(sync);
    }

    /// What the app told the terminal since the last call: titles, bells
    /// and notifications, in order, with a flood cut down as [`Teller`]
    /// says. What the update that panicked told is kept.
    pub fn take_told(&mut self) -> Vec<Told> {
        std::mem::take(&mut self.teller.told)
    }

    /// The bytes that answer the queries fed since the last call, in the
    /// order they came, for the app's PTY.
    pub fn take_answers(&mut self) -> Vec<u8> {
        let size = (self.size.cols, self.size.rows);
        self.listener.answers(self.screen.term.colors(), size)
    }

    /// The terminal modes the app has on.
    pub fn mode(&self) -> TermMode {
        *self.screen.term.mode()
    }

    /// A copy of the active screen and up to `history` scrollback rows,
    /// to format away from the pane's lock. A panic while copying resets
    /// the model, as an update does.
    pub fn snapshot(&mut self, history: usize) -> Result<Snapshot, ModelPanic> {
        self.guarded(|screen| Snapshot::of(&screen.term, history))
    }

    /// A `screen` read (section 3.3). `read` must be in range. A panic while
    /// reading resets the model, as an update does.
    pub fn screen(&mut self, read: Read, format: Format) -> Result<ScreenRead, ModelPanic> {
        let history = screen::history_for(read, usize::from(self.size.rows));
        let snapshot = self.snapshot(history)?;
        Ok(screen::read(&snapshot, read, format))
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
        self.sync = None;
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
        let snapshot = self.snapshot(0)?;
        Ok(repaint::render(&snapshot))
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
                // What the teller read stands: only the screen is lost, and
                // with it the character `CSI b` repeats and the open
                // synchronized update. The app set its modes once and does not set
                // them again when it redraws, so the fresh screen keeps them:
                // keys and pastes still reach the app in its form. The
                // modes in force are kept, not the kitty flag stacks, the
                // scroll region or the saved cursor: the term keeps those
                // to itself.
                let mode = *self.screen.term.mode();
                let style = self.screen.term.cursor_style();
                self.screen = Screen::with_modes(self.size, mode, style, &self.listener);
                self.teller.last = None;
                self.sync = None;
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

    /// Everything the model keeps, for a live upgrade. A synchronized
    /// update that a sequence it cut short started (see [`SyncMirror`])
    /// does not fit a record: its parser state is lost.
    pub fn export(&self) -> Result<ModelState, RecordError> {
        let (preceding, sync) = match &self.sync {
            Some(sync) if !sync.exact => {
                return Err(RecordError::Unfit(
                    "a synchronized update starts inside a cut sequence".into(),
                ));
            }
            Some(sync) => (sync.preceding, Some(sync.kept.clone())),
            None => (self.teller.last, None),
        };
        // The parsers have the start of a sequence over the scanner's bound,
        // and the scanner no longer has it, so a new model could not read
        // the rest of it the same way.
        if self.scanner.overflowed() {
            return Err(RecordError::Unfit(
                "a sequence over the scanner's bound is open".into(),
            ));
        }
        let held = sync.as_ref().map(Vec::len);
        let parser_holds = self
            .sync_deadline()
            .map(|_| self.screen.parser.sync_bytes_count());
        if held != parser_holds {
            return Err(RecordError::Unfit(format!(
                "the synchronized update holds {parser_holds:?} bytes, the mirror {held:?}"
            )));
        }
        let term = catch_unwind(AssertUnwindSafe(|| term::capture(&self.screen.term)))
            .map_err(|_| RecordError::Unfit("reading the screen panicked".into()))?;
        Ok(ModelState {
            size: self.size,
            term,
            preceding,
            sync,
            teller: TellerState {
                title: self.teller.title.clone(),
                stack: self.teller.stack.iter().cloned().collect(),
                told: self.teller.told.clone(),
                last: self.teller.last,
            },
            scanner: self.scanner.export(),
        })
    }

    /// A model from [`Model::export`], read back to check that it is the
    /// same. An open synchronized update gets a new deadline, 150 ms from
    /// now, as a new update start would give it.
    pub fn import(state: &ModelState) -> Result<Self, RecordError> {
        let size = state.size;
        if !term::fits(usize::from(size.rows), usize::from(size.cols)) {
            return Err(RecordError::Bad("a screen size"));
        }
        let scanner =
            Scanner::import(state.scanner.clone()).ok_or(RecordError::Bad("a scanner"))?;
        if scanner.overflowed() {
            return Err(RecordError::Unfit(
                "a sequence over the scanner's bound is open".into(),
            ));
        }
        let model = catch_unwind(AssertUnwindSafe(|| {
            let mut model = Model::new(size);
            let screen = &mut model.screen;
            // The character `CSI b` repeats first: only printing it sets it,
            // and the cells it touched are written over next.
            if let Some(c) = state.preceding {
                let mut buf = [0; 4];
                screen
                    .parser
                    .advance(&mut screen.term, c.encode_utf8(&mut buf).as_bytes());
            }
            let modes = state.term.mode - TermMode::KITTY_KEYBOARD_PROTOCOL;
            let modes = repaint::mode_bytes(modes, CursorStyle::default());
            screen.parser.advance(&mut screen.term, &modes);
            term::restore(&mut screen.term, &state.term);
            if let Some(kept) = &state.sync {
                screen.parser.advance(&mut screen.term, BSU);
                screen.parser.advance(&mut screen.term, kept);
                model.sync = Some(SyncMirror {
                    kept: kept.clone(),
                    preceding: state.preceding,
                    exact: true,
                });
            }
            model.teller = Teller {
                title: state.teller.title.clone(),
                stack: state.teller.stack.iter().cloned().collect(),
                told: state.teller.told.clone(),
                last: state.teller.last,
            };
            model.scanner = scanner;
            // Nothing above asks the term a question; drop what it may say.
            model.take_answers();
            model
        }))
        .map_err(|_| RecordError::Unfit("restoring the screen panicked".into()))?;
        if model.export()? != *state {
            return Err(RecordError::Unfit(
                "the screen read back differs from the record".into(),
            ));
        }
        Ok(model)
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
    use tiller_panes::message::MAX_RECENT;

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

    fn notify(title: &str, body: &str) -> Told {
        Told::Notify {
            title: title.into(),
            body: body.into(),
        }
    }

    #[test]
    fn titles_bells_and_notifications_are_told_in_order() {
        let mut model = Model::new(size());
        model
            .feed(
                b"\x1b]2;build\x07a\x07\x1b]9;done;now\x1b\\\x1b]777;notify;Claude;needs; you\x07",
            )
            .unwrap();
        assert_eq!(
            model.take_told(),
            [
                Told::Title("build".into()),
                Told::Bell,
                notify("", "done;now"),
                notify("Claude", "needs; you"),
            ]
        );
        assert!(model.take_told().is_empty());
        model.feed(b"\x1b]0; spaced \x07").unwrap();
        assert_eq!(model.take_told(), [Told::Title("spaced".into())]);
        model.feed(b"\x1b]0;\x07").unwrap();
        assert_eq!(model.take_told(), [Told::Title(String::new())]);
    }

    #[test]
    fn the_title_stack_and_a_reset_set_the_title() {
        let mut model = Model::new(size());
        let mut title = |bytes: &[u8]| {
            model.feed(bytes).unwrap();
            model.take_told()
        };
        assert_eq!(
            title(b"\x1b]2;one\x07\x1b[22t"),
            [Told::Title("one".into())]
        );
        assert_eq!(title(b"\x1b]2;two\x07"), [Told::Title("two".into())]);
        // The pushed title comes back, and an empty stack pops nothing.
        assert_eq!(title(b"\x1b[23t\x1b[23t"), [Told::Title("one".into())]);
        assert_eq!(title(b"\x1bc"), [Told::Title(String::new())]);
    }

    #[test]
    fn a_flood_is_told_as_the_latest_title_one_bell_and_a_few_notifications() {
        let mut model = Model::new(size());
        let mut flood = Vec::new();
        for n in 0..1000 {
            flood.extend_from_slice(format!("\x07\x1b]2;t{n}\x07\x1b]9;n{n}\x07").as_bytes());
        }
        model.feed(&flood).unwrap();
        let mut want = vec![Told::Bell];
        want.extend((0..NOTIFY_MAX).map(|n| notify("", &format!("n{n}"))));
        want.push(Told::Title("t999".into()));
        assert_eq!(model.take_told(), want);
    }

    #[test]
    fn a_model_reset_keeps_the_title_the_app_set_and_what_it_told() {
        let mut model = Model::new(size());
        model.feed(b"\x1b]2;build\x07\x1b[22t").unwrap();
        model.take_told();
        // The model's parser panics on the update, and the teller reads it.
        model.fail_next();
        model.feed(b"\x1b]2;test\x07").unwrap_err();
        assert_eq!(model.take_told(), [Told::Title("test".into())]);
        model.feed(b"\x1b]2;other\x07\x1b[23t").unwrap();
        assert_eq!(model.take_told(), [Told::Title("build".into())]);
    }

    #[test]
    fn a_long_title_or_notification_is_cut_at_a_character() {
        let mut model = Model::new(size());
        let long = "é".repeat(TEXT_MAX);
        model
            .feed(format!("\x1b]2;{long}\x07\x1b]777;notify;{long};{long}\x07").as_bytes())
            .unwrap();
        let cut = "é".repeat(TEXT_MAX / 2);
        assert_eq!(
            model.take_told(),
            [Told::Title(cut.clone()), notify(&cut, &cut)]
        );
    }

    #[test]
    fn an_osc_ends_where_the_model_ends_it_and_a_cut_one_is_read_whole() {
        let mut model = Model::new(size());
        // CAN, SUB and ESC end the sequence, as in the model's parser: what
        // follows is text, and the BEL after it rings.
        model
            .feed(b"\x1b]2;one\x18two\x07\x1b]9;three\x1afour\x1b]9;five\x1b[1m")
            .unwrap();
        assert_eq!(
            model.take_told(),
            [
                Told::Title("one".into()),
                Told::Bell,
                notify("", "three"),
                notify("", "five"),
            ]
        );
        assert_eq!(row(&model, 0), "twofour");
        // A body longer than the scanner holds, cut over many reads.
        let body = "x".repeat(200 * 1024);
        model.feed(b"\x1b]777;notify;long;").unwrap();
        for part in body.as_bytes().chunks(4096) {
            model.feed(part).unwrap();
        }
        model.feed(b"\x07").unwrap();
        // Told once, at its end, cut to the most that is told.
        assert_eq!(model.take_told(), [notify("long", &body[..TEXT_MAX])]);
    }

    #[test]
    fn conemu_osc_9_commands_are_not_notifications() {
        let mut model = Model::new(size());
        model
            .feed(b"\x1b]9;4;1;50\x07\x1b]9;12\x07\x1b]9;13;x\x07\x1b]9;04\x07")
            .unwrap();
        assert_eq!(model.take_told(), [notify("", "13;x"), notify("", "04")]);
    }

    #[test]
    fn a_model_reset_keeps_the_modes_the_app_set() {
        let mut model = Model::new(size());
        model
            .feed(b"\x1b[?1049h\x1b[?2004h\x1b[?1h\x1b[>1u\x1b[?25l\x1b[?7l\x1b[?1007l\x1b[5 qtext")
            .unwrap();
        let before = model.mode();
        model.fail_next();
        model.feed(b"x").unwrap_err();
        assert_eq!(model.mode(), before);
        assert_eq!(
            model.term().cursor_style().shape,
            alacritty_terminal::vte::ansi::CursorShape::Beam
        );
        // The screen itself is fresh.
        assert_eq!(row(&model, 0), "");
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

    /// `model` through the handover's byte form and back.
    fn handed_over(model: &Model) -> Model {
        let state = model.export().unwrap();
        let mut w = crate::handover::codec::Writer::new();
        crate::handover::put_model(&mut w, &state);
        let bytes = w.into_bytes();
        let mut r = crate::handover::codec::Reader::new(&bytes);
        let read = crate::handover::get_model(&mut r).unwrap();
        assert!(r.done());
        assert_eq!(read, state);
        Model::import(&read).unwrap()
    }

    /// Feeds `bytes` to both models, and checks that they stay the same in
    /// everything they keep, what they draw, what they tell and what they
    /// answer.
    fn same_after(old: &mut Model, new: &mut Model, bytes: &[u8]) {
        for model in [&mut *old, &mut *new] {
            model.feed(bytes).unwrap();
        }
        assert_eq!(old.export().unwrap(), new.export().unwrap());
        assert_eq!(old.repaint().unwrap(), new.repaint().unwrap());
        assert_eq!(old.take_told(), new.take_told());
        assert_eq!(old.take_answers(), new.take_answers());
    }

    /// A screen with every part the handover carries set to something
    /// other than its default.
    fn busy_screen() -> Vec<u8> {
        let mut bytes = Vec::new();
        // Colors the app set, and titles on the stack.
        bytes.extend_from_slice(b"\x1b]4;1;rgb:12/34/56\x1b\\\x1b]10;rgb:aa/bb/cc\x07");
        bytes.extend_from_slice(b"\x1b]2;first\x07\x1b[22t\x1b]2;second\x07");
        // Scrollback, with every kind of cell.
        for n in 0..12 {
            bytes.extend_from_slice(
                format!("\x1b[1;3;31;4:3;58:5:200m{n}\x1b[m e\u{301}\u{302} 日 \x1b[2;5;7;8;9;38;5;100;48;2;1;2;3m!\x1b[m\r\n")
                    .as_bytes(),
            );
        }
        bytes.extend_from_slice(b"\x1b]8;id=x;https://example.com/a\x1b\\link\x1b]8;;\x1b\\ ");
        // A row that wraps, and the cursor waiting to wrap.
        bytes.extend_from_slice(&[b'w'; 39]);
        // Tab stops at columns 4 and 12 only.
        bytes.extend_from_slice(b"\x1b[3g\x1b[1;5H\x1bH\x1b[1;13H\x1bH");
        // A saved cursor with its pen, a scroll region and a cursor style.
        bytes.extend_from_slice(b"\x1b[3;4H\x1b[32;44m\x1b7\x1b[2;3r\x1b[5 q");
        // Kitty flags on the primary screen, then the alternate screen with
        // its own content and flags.
        bytes.extend_from_slice(b"\x1b[>1u\x1b[>3u\x1b[?1049h\x1b[35malt\x1b[>5u");
        // Modes, then G1 as line drawing, shifted in.
        bytes.extend_from_slice(
            b"\x1b[?2004h\x1b[?1h\x1b=\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?1007l\x1b[?1042l\x1b[20h\x1b[4h\x1b[?25l",
        );
        bytes.extend_from_slice(b"\x1b)0\x0eq\x1b[2;7HZ");
        bytes
    }

    #[test]
    fn every_part_of_the_model_reads_back_after_a_handover() {
        let mut old = Model::new(size());
        old.feed(&busy_screen()).unwrap();
        let mut new = handed_over(&old);
        assert_eq!(old.repaint().unwrap(), new.repaint().unwrap());
        assert_eq!(old.take_told(), new.take_told());
        // What each part does next is the same: the repeat of the last
        // character, both charsets, the tab stops, the kitty stacks, the
        // scroll region, the saved cursor, the title stack and the queries.
        same_after(
            &mut old,
            &mut new,
            b"\x1b[3b\x0fq\x0eq\t\tX\x1b[>4u\x1b[<2u\x1b[?u\x1b[6n",
        );
        same_after(
            &mut old,
            &mut new,
            b"\x1b[3;1H\n\n\nscrolled\x1b8saved\x1b[23t",
        );
        // Leaving the alternate screen shows the old primary screen.
        same_after(&mut old, &mut new, b"\x1b[?1049l\x1b[?u\x1b[<u\x1b[?u");
        assert!((0..4).any(|line| row(&new, line).starts_with('w')));
        same_after(
            &mut old,
            &mut new,
            b"\x1b[r\x1b[H\x1b[2J\x1b[4lnew\x1b[?1049h\x1b[?1049l",
        );
        // The scrollback came over whole.
        assert_eq!(
            old.screen(Read::Recent(MAX_RECENT), Format::Ansi).unwrap(),
            new.screen(Read::Recent(MAX_RECENT), Format::Ansi).unwrap()
        );
    }

    #[test]
    fn the_character_a_repeat_repeats_moves_with_the_model() {
        // A replacement character for a bad byte, a C1 character, a wide
        // one and a combining one.
        for last in [
            &b"a\xff"[..],
            "a\u{9b}".as_bytes(),
            "日".as_bytes(),
            "e\u{301}".as_bytes(),
        ] {
            let mut old = Model::new(size());
            old.feed(last).unwrap();
            let mut new = handed_over(&old);
            same_after(&mut old, &mut new, b"\x1b[3b");
        }
    }

    #[test]
    fn a_read_cut_inside_a_sequence_or_a_character_goes_on_after_a_handover() {
        for (cut, rest) in [
            (&b"ab\x1b[3"[..], &b"1mcd"[..]),
            (b"ab\xe6\x97", b"\xa5cd"),
            (b"\x1b]2;ti", b"tle\x07x"),
            (b"\x1bP1$", b"q\x1b\\x"),
        ] {
            let mut old = Model::new(size());
            old.feed(cut).unwrap();
            let mut new = handed_over(&old);
            same_after(&mut old, &mut new, rest);
        }
    }

    #[test]
    fn a_sequence_over_the_scanners_bound_does_not_fit_a_record_until_it_ends() {
        let mut model = Model::new(size());
        let mut long = b"x\x1b]2;".to_vec();
        long.extend(std::iter::repeat_n(b'a', crate::scanner::HELD_MAX + 10));
        model.feed(&long).unwrap();
        assert!(matches!(model.export(), Err(RecordError::Unfit(_))));
        model.feed(b"aa\x07after").unwrap();
        let mut new = handed_over(&model);
        same_after(&mut model, &mut new, b" more");
    }

    #[test]
    fn an_open_synchronized_update_moves_with_its_bytes() {
        let cases: [(&[u8], &[u8]); 4] = [
            (b"before\x1b[?2026hheld", b" more\x1b[?2026l"),
            // A new update started inside the open one.
            (b"\x1b[?2026ha\x1b[?2026hb", b"c\x1b[?2026l"),
            // An update that ended, and a new one after it in one read.
            (b"\x1b[?2026ha\x1b[?2026l\x1b[?2026hb", b"c\x1b[?2026l"),
            // The update's end cut between reads.
            (b"\x1b[?2026hb\x1b[?20", b"26lc"),
        ];
        for (open, rest) in cases {
            let mut old = Model::new(size());
            old.feed(open).unwrap();
            assert!(old.sync_deadline().is_some(), "{open:?}");
            let mut new = handed_over(&old);
            assert!(new.sync_deadline().is_some());
            same_after(&mut old, &mut new, rest);
            assert!(new.sync_deadline().is_none());
        }
        // An update that ends at its deadline.
        let mut old = Model::new(size());
        old.feed(b"\x1b[?2026hlate").unwrap();
        let mut new = handed_over(&old);
        old.end_sync().unwrap();
        new.end_sync().unwrap();
        assert_eq!(old.export().unwrap(), new.export().unwrap());
    }

    #[test]
    fn an_update_that_starts_inside_a_cut_sequence_does_not_fit_a_record() {
        // The update's start comes after an OSC that is still open where
        // the parser stopped: the parser's state there is not in the record.
        let mut model = Model::new(size());
        model
            .feed(b"\x1b[?2026hx\x1b[?2026l\x1b]2;a\x1b[?2026h")
            .unwrap();
        assert!(matches!(model.export(), Err(RecordError::Unfit(_))));
        // Once the update ends, the model fits again.
        model.end_sync().unwrap();
        model.export().unwrap();
    }
}
