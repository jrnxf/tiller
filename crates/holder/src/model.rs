//! A pane's screen model (`terminal-holder.md` 3.1, 3.4.1, 3.5).
//!
//! The model is an `alacritty_terminal` [`Term`] fed by a `vte`
//! [`Processor`]. The [`Scanner`] in front of it holds back a read's cut
//! sequence, so the parser is in its ground state between reads. Every
//! update runs inside `catch_unwind`: a panic resets this model only, and
//! the caller learns of it through [`ModelPanic`] to tell the child and the
//! pane's subscribers.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorStyle, Processor};
use alacritty_terminal::vte::{Params, Parser, Perform};
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

/// The most titles the title stack keeps, as `alacritty_terminal` keeps.
const TITLE_STACK: usize = 4096;

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
#[derive(Debug, Default)]
struct Teller {
    title: String,
    stack: VecDeque<String>,
    told: Vec<Told>,
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

/// The terminal state and its parsers.
struct Screen {
    term: Term<VoidListener>,
    parser: Processor,
    teller: Teller,
    teller_parser: Parser,
}

impl Screen {
    fn new(size: Size) -> Self {
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &GridSize(size), VoidListener),
            parser: Processor::new(),
            teller: Teller::default(),
            teller_parser: Parser::new(),
        }
    }

    /// A blank screen with `mode` and `style` set. Should setting them
    /// panic, the screen is blank with none.
    fn with_modes(size: Size, mode: TermMode, style: CursorStyle) -> Self {
        let bytes = repaint::mode_bytes(mode, style);
        let mut screen = Screen::new(size);
        let set = catch_unwind(AssertUnwindSafe(|| {
            screen.parser.advance(&mut screen.term, &bytes);
        }));
        if set.is_err() {
            screen = Screen::new(size);
        }
        screen
    }
}

/// One pane's screen.
pub struct Model {
    screen: Screen,
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
        Self {
            screen: Screen::new(size),
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
        let whole = self.scanner.split(bytes);
        if whole.is_empty() {
            return Ok(());
        }
        self.remember(&whole);
        self.guarded(|screen| {
            screen.teller_parser.advance(&mut screen.teller, &whole);
            screen.parser.advance(&mut screen.term, &whole);
        })
    }

    /// What the app told the terminal since the last call: titles, bells
    /// and notifications, in order, with a flood cut down as [`Teller`]
    /// says. What the update that panicked told is kept.
    pub fn take_told(&mut self) -> Vec<Told> {
        std::mem::take(&mut self.screen.teller.told)
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
                // The teller read the bytes whole, and what it read stands:
                // only the screen is lost. Its parser starts afresh, as the
                // model's does. The app set its modes once and does not set
                // them again when it redraws, so the fresh screen keeps them:
                // keys and pastes still reach the app in its form.
                let mode = *self.screen.term.mode();
                let style = self.screen.term.cursor_style();
                let teller = std::mem::take(&mut self.screen.teller);
                self.screen = Screen::with_modes(self.size, mode, style);
                self.screen.teller = teller;
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
    pub(crate) fn term(&self) -> &Term<VoidListener> {
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
        // The teller reads the update, and then the model's parser panics.
        model
            .guarded::<()>(|screen| {
                screen
                    .teller_parser
                    .advance(&mut screen.teller, b"\x1b]2;test\x07");
                panic!("the model broke");
            })
            .unwrap_err();
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
            .feed(b"\x1b[?1049h\x1b[?2004h\x1b[?1h\x1b[>1u\x1b[?25l\x1b[5 qtext")
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
}
