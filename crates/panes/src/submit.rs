//! Submitting a line typed into a Claude Code composer, and proving it went.
//!
//! Claude Code can read an Enter that arrives right after typed text as part
//! of it, so the Enter adds a newline instead of submitting; a Claude just
//! relaunched in the same pane does this every time. Everything that types a
//! line into a Claude pane and presses Enter shares this: wait [`Pacing::settle`]
//! between the line and the first Enter, then read the composer back after
//! each Enter and press again while the line is still there.
//!
//! [`Submission`] is the steppable form, for a caller that serves many panes
//! and waits once for all of them; [`submit`] is the blocking form for one.
//! Both drive a [`Target`]: the holder through [`Panes`] (`terminal-holder.md`
//! 4.5).

use std::thread;
use std::time::Duration;

use crate::composer::{Composer, classify};
use crate::keys::Key;
use crate::message::{Format, PaneId, Read};
use crate::panes::Panes;

/// What a submission presses Enter on and reads back.
pub trait Target {
    type Error;
    fn press_enter(&self, pane: &str) -> Result<(), Self::Error>;
    /// The pane's visible viewport with its ANSI styling, rows split by `\n`.
    fn read_visible(&self, pane: &str) -> Result<String, Self::Error>;
}

impl<P: Panes + ?Sized> Target for P {
    type Error = crate::PanesError;

    fn press_enter(&self, pane: &str) -> Result<(), Self::Error> {
        self.keys(&PaneId::from(pane), &[Key::ENTER])
    }

    fn read_visible(&self, pane: &str) -> Result<String, Self::Error> {
        let screen = self.screen(&PaneId::from(pane), Read::Visible, Format::Ansi)?;
        Ok(screen.rows_text.join("\n"))
    }
}

/// How many times one submission presses Enter on a line that stays in the
/// composer.
pub const PRESSES: u32 = 4;
/// How many times one submission reads the composer after an Enter.
pub const READS: u32 = 2 * PRESSES;

/// The pauses that let a pane's agent take a typed line as one prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pacing {
    /// Between typing the line and the first Enter.
    pub settle: Duration,
    /// Between an Enter and the read that checks the line left the composer.
    pub recheck: Duration,
}

impl Pacing {
    /// No pauses, for tests against a fake.
    pub const INSTANT: Self = Self {
        settle: Duration::ZERO,
        recheck: Duration::ZERO,
    };
}

impl Default for Pacing {
    fn default() -> Self {
        Self {
            settle: Duration::from_millis(500),
            recheck: Duration::from_millis(500),
        }
    }
}

/// How a typed line's submit went.
#[derive(Debug)]
pub enum Submit<E> {
    /// The line left the composer, or the caller's own evidence says so.
    Left,
    /// The line is still in the composer after every press.
    Held,
    /// The target failed, so the composer could not be checked.
    Failed(E),
}

/// One line being submitted: the first Enter is pressed, and each
/// [`Submission::check`] reads the composer back.
#[derive(Debug)]
pub struct Submission<E> {
    pane_id: String,
    presses: u32,
    reads: u32,
    failed: Option<E>,
}

impl<E> Submission<E> {
    /// Presses the first Enter.
    pub fn begin<T: Target<Error = E> + ?Sized>(target: &T, pane_id: &str) -> Result<Self, E> {
        target.press_enter(pane_id)?;
        Ok(Self {
            pane_id: pane_id.to_owned(),
            presses: 1,
            reads: 0,
            failed: None,
        })
    }

    pub fn pane_id(&self) -> &str {
        &self.pane_id
    }

    /// One look, a [`Pacing::recheck`] after the last Enter. `left` is the
    /// caller's own evidence the line went (an ack, a session gone); `shows`
    /// says whether a composer holds exactly the line, which only is pressed
    /// again, never other text. Only a composer read as empty, busy or
    /// behind a dialog shows the line left; other text or an unreadable
    /// screen is read again. `None` while the line is still in doubt.
    pub fn check<T: Target<Error = E> + ?Sized>(
        &mut self,
        target: &T,
        left: bool,
        shows: impl Fn(&Composer) -> bool,
    ) -> Option<Submit<E>> {
        if left {
            return Some(Submit::Left);
        }
        self.reads += 1;
        let composer = match target.read_visible(&self.pane_id) {
            Ok(screen) => classify(&screen),
            Err(error) => {
                self.failed = Some(error);
                return self.spent();
            }
        };
        self.failed = None;
        match composer {
            Composer::Empty | Composer::Busy | Composer::Dialog => return Some(Submit::Left),
            held if shows(&held) => {
                if self.presses == PRESSES {
                    return Some(Submit::Held);
                }
                if let Err(error) = target.press_enter(&self.pane_id) {
                    return Some(Submit::Failed(error));
                }
                self.presses += 1;
            }
            Composer::Holds(_) | Composer::Unknown => {}
        }
        self.spent()
    }

    /// The verdict once every read is used.
    fn spent(&mut self) -> Option<Submit<E>> {
        (self.reads >= READS).then(|| self.failed.take().map_or(Submit::Held, Submit::Failed))
    }
}

/// Presses Enter on a line already typed into `pane_id` and waits until it
/// has left the composer, at most [`PRESSES`] presses and [`READS`] reads.
/// `left` is polled before each read; an error from it ends the wait.
pub fn submit<T: Target + ?Sized, E>(
    target: &T,
    pane_id: &str,
    pacing: Pacing,
    shows: impl Fn(&Composer) -> bool,
    mut left: impl FnMut() -> Result<bool, E>,
) -> Result<Submit<T::Error>, E> {
    let mut submission = match Submission::begin(target, pane_id) {
        Ok(submission) => submission,
        Err(error) => return Ok(Submit::Failed(error)),
    };
    loop {
        thread::sleep(pacing.recheck);
        let gone = left()?;
        if let Some(done) = submission.check(target, gone, &shows) {
            return Ok(done);
        }
    }
}
