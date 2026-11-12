//! The owner's terminal while `tiller attach` holds it: raw mode, the
//! sequences that take it back on detach, and the boxes drawn over the pane.

use std::io::{self, Write};
use std::os::fd::AsFd;

use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};

/// What attach writes first: the alternate screen, cleared, so the owner's
/// shell screen comes back on detach.
pub const ENTER: &[u8] = b"\x1b[?1049h\x1b[H\x1b[2J";

/// What detach writes, in this order: every mode an agent may have turned
/// on goes off, then the alternate screen goes, then the main screen's
/// kitty flags go too (each screen keeps its own stack).
pub const RESTORE: &[u8] = concat!(
    // The kitty keyboard flags: pop the stack, and zero what is left.
    "\x1b[<99u\x1b[=0;1u",
    // Bracketed paste, the mouse modes and focus reports.
    "\x1b[?2004l",
    "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l",
    "\x1b[?1004l",
    // Synchronized output, the cursor keys and the keypad.
    "\x1b[?2026l\x1b[?1l\x1b>",
    // Origin mode off, wrap on, the whole screen as the scroll region.
    "\x1b[?6l\x1b[?7h\x1b[r",
    // The cursor's shape, the colors, and the cursor shown.
    "\x1b[0 q\x1b[0m\x1b[?25h",
    "\x1b[?1049l",
    "\x1b[<99u\x1b[=0;1u",
)
.as_bytes();

/// The owner's terminal in raw mode. [`Raw::leave`] puts back the modes it
/// had, and a drop does too when attach ends some other way.
pub struct Raw {
    saved: Option<Termios>,
}

impl Raw {
    /// Puts stdin's terminal in raw mode.
    pub fn enter() -> io::Result<Self> {
        let stdin = io::stdin();
        let saved = tcgetattr(stdin.as_fd())?;
        let mut raw = saved.clone();
        raw.make_raw();
        tcsetattr(stdin.as_fd(), OptionalActions::Now, &raw)?;
        Ok(Self { saved: Some(saved) })
    }

    /// Writes [`RESTORE`], then leaves raw mode.
    pub fn leave(mut self) {
        self.restore();
    }

    fn restore(&mut self) {
        let Some(saved) = self.saved.take() else {
            return;
        };
        let mut out = io::stdout().lock();
        let _ = out.write_all(RESTORE);
        let _ = out.flush();
        // Drain: the restore reaches the terminal before its modes change.
        let _ = tcsetattr(io::stdin().as_fd(), OptionalActions::Drain, &saved);
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        self.restore();
    }
}

/// The owner's window, columns and rows. None when stdout is no terminal or
/// says it has no size.
pub fn window_size() -> Option<(u16, u16)> {
    let size = tcgetwinsize(io::stdout().as_fd()).ok()?;
    (size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
}

/// Writes `bytes` to the owner's terminal now.
pub fn write(bytes: &[u8]) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()
}

/// One row of a box: its text, and whether it is the selected one.
pub struct Line {
    pub text: String,
    pub selected: bool,
}

impl Line {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            selected: false,
        }
    }
}

/// A box titled `title` around `lines`, centered in a window this size and
/// drawn over what it shows. It sets plain colors and hides the cursor,
/// since the pane is repainted when the box closes.
pub fn draw_box(title: &str, lines: &[Line], (cols, rows): (u16, u16)) -> Vec<u8> {
    let cols = usize::from(cols);
    let width = lines
        .iter()
        .map(|line| line.text.chars().count())
        .chain([title.chars().count() + 2])
        .max()
        .unwrap_or(0)
        .saturating_add(4)
        .min(cols.max(4));
    let inner = width.saturating_sub(2);
    let height = (lines.len() + 2).min(usize::from(rows).max(2));
    let top = (usize::from(rows).saturating_sub(height)) / 2 + 1;
    let left = cols.saturating_sub(width) / 2 + 1;
    let mut out = String::from("\x1b[?6l\x1b[r\x1b[0m\x1b[?25l");
    let at = |out: &mut String, row: usize| out.push_str(&format!("\x1b[{row};{left}H"));
    at(&mut out, top);
    let head = format!(" {title} ");
    let head: String = head.chars().take(inner).collect();
    out.push('┌');
    out.push_str(&head);
    out.push_str(&"─".repeat(inner.saturating_sub(head.chars().count())));
    out.push('┐');
    for (n, line) in lines.iter().take(height.saturating_sub(2)).enumerate() {
        at(&mut out, top + 1 + n);
        let text: String = format!(" {}", line.text).chars().take(inner).collect();
        let pad = " ".repeat(inner.saturating_sub(text.chars().count()));
        out.push('│');
        if line.selected {
            out.push_str("\x1b[7m");
        }
        out.push_str(&text);
        out.push_str(&pad);
        if line.selected {
            out.push_str("\x1b[0m");
        }
        out.push('│');
    }
    at(&mut out, top + height.saturating_sub(1));
    out.push('└');
    out.push_str(&"─".repeat(inner));
    out.push('┘');
    out.into_bytes()
}

/// A one-line note on the bottom row of the window, over what is there.
pub fn note(text: &str, (cols, rows): (u16, u16)) -> Vec<u8> {
    let text: String = text.chars().take(usize::from(cols)).collect();
    format!("\x1b[?6l\x1b[r\x1b[{rows};1H\x1b[0m\x1b[2K\x1b[7m{text}\x1b[0m").into_bytes()
}

/// The screen switches an app may send. Attach keeps the whole session on
/// the owner's alternate screen, so the owner's own screen and scrollback
/// come back untouched on detach: these never reach the terminal.
const SWITCHES: [&[u8]; 6] = [
    b"\x1b[?1049h",
    b"\x1b[?1049l",
    b"\x1b[?1047h",
    b"\x1b[?1047l",
    b"\x1b[?47h",
    b"\x1b[?47l",
];

/// Takes the screen switches out of a pane's bytes. A switch cut at the end
/// of one read waits for the next.
#[derive(Debug, Default)]
pub struct Screens {
    carry: Vec<u8>,
}

impl Screens {
    /// The bytes to write, and whether a switch was taken out: the app moved
    /// between its screens, so the pane wants a repaint.
    pub fn filter(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(bytes);
        let mut out = Vec::with_capacity(buf.len());
        let mut switched = false;
        let mut at = 0;
        while at < buf.len() {
            if buf[at] == 0x1b {
                let rest = &buf[at..];
                if let Some(switch) = SWITCHES.iter().find(|switch| rest.starts_with(switch)) {
                    at += switch.len();
                    switched = true;
                    continue;
                }
                if SWITCHES.iter().any(|switch| switch.starts_with(rest)) {
                    self.carry = rest.to_vec();
                    break;
                }
            }
            out.push(buf[at]);
            at += 1;
        }
        (out, switched)
    }

    /// Drops what waits, for a new stream.
    pub fn reset(&mut self) {
        self.carry.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_switches_never_reach_the_terminal_even_when_cut() {
        let mut screens = Screens::default();
        assert_eq!(screens.filter(b"a\x1b[?1049lb"), (b"ab".to_vec(), true));
        assert_eq!(screens.filter(b"c\x1b[?10"), (b"c".to_vec(), false));
        assert_eq!(screens.filter(b"49hd"), (b"d".to_vec(), true));
        assert_eq!(
            screens.filter(b"\x1b[?1004h"),
            (b"\x1b[?1004h".to_vec(), false)
        );
    }

    #[test]
    fn a_box_is_drawn_centered_with_its_title() {
        let lines = [
            Line::plain("one"),
            Line {
                text: "two".into(),
                selected: true,
            },
        ];
        let drawn = String::from_utf8(draw_box("t", &lines, (20, 10))).unwrap_or_default();
        assert!(drawn.contains("\x1b[4;7H┌ t ──┐"), "{drawn:?}");
        assert!(drawn.contains("\x1b[5;7H│ one │"), "{drawn:?}");
        assert!(
            drawn.contains("\x1b[6;7H│\x1b[7m two \x1b[0m│"),
            "{drawn:?}"
        );
        assert!(drawn.contains("\x1b[7;7H└─────┘"), "{drawn:?}");
    }
}
