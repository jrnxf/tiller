//! The terminal mirror for a pane in the terminal holder
//! (`terminal-holder.md` 5.1), the first slice (S1c).
//!
//! The view subscribes to the pane: a `size` line and a full frame with the
//! holder's repaint, then each of the PTY's writes as a frame of its own.
//! When the PTY's size changes, the view subscribes again, so its new
//! repaint is the board's next `size` line and full screen. The child's
//! exit is `closed`, with how it ended, and a holder that stops answering
//! is `unavailable`, after which the board opens the view again.
//!
//! Fit waits for its own slice (H11, a Fit as a lease): a view asked for
//! one says `unfit` and goes on at the pane's own size, as it does when
//! herdr refuses a Fit.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use base64::Engine as _;
use tiller_panes::{PaneId, PanesError, Update};
use tokio::sync::{mpsc, watch};

use crate::dto::TerminalLine;
use crate::holder::{Holder, Live, LiveControl, exit_reason};
use crate::mirror::{Viewer, queue, stopping, unavailable};

/// Updates a subscription's thread can hand on before it waits for the
/// view to take them.
const UPDATES: usize = 64;

/// A subscription read on a thread of its own, since its `recv` blocks.
struct Pumped {
    updates: mpsc::Receiver<Result<Option<Update>, PanesError>>,
    control: Arc<dyn LiveControl>,
}

impl Pumped {
    /// Reads `live` until it ends, fails or nobody takes its updates.
    fn start(mut live: Box<dyn Live>) -> std::io::Result<Self> {
        let control = live.control();
        let (tx, updates) = mpsc::channel(UPDATES);
        std::thread::Builder::new()
            .name("tiller-holder-view".into())
            .spawn(move || {
                loop {
                    let update = live.recv();
                    let last = !matches!(update, Ok(Some(_)));
                    if tx.blocking_send(update).is_err() || last {
                        return;
                    }
                }
            })?;
        Ok(Self { updates, control })
    }
}

impl Drop for Pumped {
    fn drop(&mut self) {
        self.control.end();
    }
}

/// Subscribes to `pane` off the async threads, and starts reading it.
async fn open(holder: &Arc<dyn Holder>, pane: &PaneId) -> Result<Pumped, String> {
    let holder = Arc::clone(holder);
    let pane = pane.clone();
    tokio::task::spawn_blocking(move || {
        let live = holder.subscribe(&pane).map_err(|error| error.to_string())?;
        Pumped::start(live).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The NDJSON body that streams the holder's `pane` to one viewer. A view
/// the board `fitted` says `unfit` first.
pub fn stream(
    holder: Arc<dyn Holder>,
    pane: PaneId,
    fitted: bool,
    stop: watch::Receiver<bool>,
    viewer: Viewer,
) -> Body {
    let (tx, mut rx) = mpsc::channel::<Bytes>(crate::mirror::QUEUE);
    tokio::spawn(async move {
        let _viewer = viewer;
        mirror(&holder, &pane, fitted, stop, &tx).await;
    });
    let lines = futures_util::stream::poll_fn(move |cx| {
        rx.poll_recv(cx).map(|line| line.map(Ok::<_, Infallible>))
    });
    Body::from_stream(lines)
}

/// A body with one `closed` line, for a pane whose child has exited.
pub fn closed(reason: String) -> Body {
    let (tx, mut rx) = mpsc::channel::<Bytes>(1);
    queue(
        &tx,
        &TerminalLine::Closed {
            reason: Some(reason),
        },
    );
    drop(tx);
    let lines = futures_util::stream::poll_fn(move |cx| {
        rx.poll_recv(cx).map(|line| line.map(Ok::<_, Infallible>))
    });
    Body::from_stream(lines)
}

/// The board's frame of `bytes`.
fn frame(seq: &mut u64, full: bool, bytes: &[u8]) -> TerminalLine {
    *seq += 1;
    TerminalLine::Frame {
        seq: *seq,
        full,
        bytes: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

/// What a Fit on a holder pane gets until H11.
const UNFIT: &str = "Fit is not ready yet for an agent in the terminal holder; the view shows the pane at its own size";

/// Streams `pane` to `tx` until the browser leaves, the child exits, the
/// holder stops answering or the daemon stops.
async fn mirror(
    holder: &Arc<dyn Holder>,
    pane: &PaneId,
    fitted: bool,
    mut stop: watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) {
    if fitted
        && !queue(
            tx,
            &TerminalLine::Unfit {
                message: UNFIT.to_owned(),
            },
        )
    {
        return;
    }
    let mut seq = 0;
    'views: loop {
        let mut view = match open(holder, pane).await {
            Ok(view) => view,
            Err(error) => return unavailable(tx, &error),
        };
        // An update read while gathering a burst, for the next turn.
        let mut held = None;
        loop {
            let update = match held.take() {
                Some(update) => Some(update),
                None => tokio::select! {
                    update = view.updates.recv() => update,
                    () = stopping(&mut stop) => return,
                    () = tx.closed() => return,
                },
            };
            // A burst of output already waiting goes as one frame, so a
            // fast writer does not fill the view's queue line by line.
            let update = match update {
                Some(Ok(Some(Update::Output(mut bytes)))) => {
                    while let Ok(next) = view.updates.try_recv() {
                        match next {
                            Ok(Some(Update::Output(more))) => bytes.extend_from_slice(&more),
                            other => {
                                held = Some(other);
                                break;
                            }
                        }
                    }
                    Some(Ok(Some(Update::Output(bytes))))
                }
                other => other,
            };
            let line = match update {
                Some(Ok(Some(Update::Repaint { cols, rows, bytes }))) => {
                    if !queue(tx, &TerminalLine::Size { cols, rows }) {
                        return;
                    }
                    frame(&mut seq, true, &bytes)
                }
                Some(Ok(Some(Update::Output(bytes)))) => frame(&mut seq, false, &bytes),
                // The PTY took a new size: a new subscription's repaint is
                // the board's new size and full screen.
                Some(Ok(Some(Update::Resized { .. }))) => continue 'views,
                Some(Ok(Some(Update::Exited(exit)))) => TerminalLine::Closed {
                    reason: Some(exit_reason(Some(&exit))),
                },
                Some(Ok(Some(Update::Lagged))) => {
                    return unavailable(tx, "the view fell behind the terminal holder");
                }
                Some(Err(error)) => return unavailable(tx, &error.to_string()),
                // The holder ended the stream without an exit: the board
                // opens it again.
                Some(Ok(None)) | None => return,
            };
            let closed = matches!(line, TerminalLine::Closed { .. });
            if !queue(tx, &line) || closed {
                return;
            }
        }
    }
}

/// The bytes a terminal sends for each of `keys`, named as the board names
/// them (`terminal-keys.ts`): modifiers `ctrl+`, `alt+` and `shift+` before
/// a name or one character. The holder writes bytes, not key names, so the
/// legacy encodings stand in until H02 brings the holder's own key names.
pub fn key_bytes(keys: &[String]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for key in keys {
        let bytes = one_key(key)
            .ok_or_else(|| format!("{key:?} is not a key the terminal holder can send"))?;
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

fn one_key(key: &str) -> Option<Vec<u8>> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut rest = key;
    while rest.len() > 1 {
        if let Some(after) = rest.strip_prefix("ctrl+") {
            ctrl = true;
            rest = after;
        } else if let Some(after) = rest.strip_prefix("alt+") {
            alt = true;
            rest = after;
        } else if let Some(after) = rest.strip_prefix("shift+") {
            shift = true;
            rest = after;
        } else {
            break;
        }
    }
    // xterm's modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let csi = |end: char| match param {
        1 => format!("\x1b[{end}"),
        param => format!("\x1b[1;{param}{end}"),
    };
    let tilde = |number: u8| match param {
        1 => format!("\x1b[{number}~"),
        param => format!("\x1b[{number};{param}~"),
    };
    let ss3 = |end: char| match param {
        1 => format!("\x1bO{end}"),
        _ => csi(end),
    };
    let sequence = match rest {
        "up" => Some(csi('A')),
        "down" => Some(csi('B')),
        "right" => Some(csi('C')),
        "left" => Some(csi('D')),
        "home" => Some(csi('H')),
        "end" => Some(csi('F')),
        "insert" => Some(tilde(2)),
        "delete" => Some(tilde(3)),
        "pageup" => Some(tilde(5)),
        "pagedown" => Some(tilde(6)),
        "f1" => Some(ss3('P')),
        "f2" => Some(ss3('Q')),
        "f3" => Some(ss3('R')),
        "f4" => Some(ss3('S')),
        "f5" => Some(tilde(15)),
        "f6" => Some(tilde(17)),
        "f7" => Some(tilde(18)),
        "f8" => Some(tilde(19)),
        "f9" => Some(tilde(20)),
        "f10" => Some(tilde(21)),
        "f11" => Some(tilde(23)),
        "f12" => Some(tilde(24)),
        "tab" if shift => Some("\x1b[Z".to_owned()),
        _ => None,
    };
    if let Some(sequence) = sequence {
        return Some(sequence.into_bytes());
    }
    let mut bytes = match rest {
        "enter" => vec![b'\r'],
        "tab" => vec![b'\t'],
        "backspace" => vec![0x7f],
        "esc" | "escape" => vec![0x1b],
        "space" if ctrl => vec![0],
        "space" => vec![b' '],
        _ => {
            let mut chars = rest.chars();
            let (Some(char), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if char.is_control() {
                return None;
            }
            if ctrl {
                vec![control_byte(char)?]
            } else if shift {
                char.to_uppercase().collect::<String>().into_bytes()
            } else {
                char.to_string().into_bytes()
            }
        }
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

/// The byte Ctrl with `char` sends, where a terminal has one.
fn control_byte(char: char) -> Option<u8> {
    match char {
        'a'..='z' | 'A'..='Z' => Some((char as u8) & 0x1f),
        '@' | ' ' | '2' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' | '/' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(keys: &[&str]) -> Result<Vec<u8>, String> {
        key_bytes(&keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn the_boards_key_names_are_the_bytes_a_terminal_sends() {
        assert_eq!(
            keys(&["e", "c", "h", "o", "space", "enter"]).unwrap(),
            b"echo \r"
        );
        assert_eq!(
            keys(&["ctrl+c", "ctrl+space", "ctrl+\\"]).unwrap(),
            b"\x03\x00\x1c"
        );
        assert_eq!(
            keys(&["alt+b", "alt+enter", "esc"]).unwrap(),
            b"\x1bb\x1b\r\x1b"
        );
        assert_eq!(
            keys(&["up", "ctrl+left", "shift+tab"]).unwrap(),
            b"\x1b[A\x1b[1;5D\x1b[Z"
        );
        assert_eq!(
            keys(&["f1", "ctrl+f1", "f5", "shift+f12"]).unwrap(),
            b"\x1bOP\x1b[1;5P\x1b[15~\x1b[24;2~"
        );
        assert_eq!(
            keys(&["tab", "backspace", "-", "+", "é"]).unwrap(),
            "\t\x7f-+é".as_bytes()
        );
    }

    #[test]
    fn a_name_no_terminal_sends_is_refused() {
        assert!(keys(&["menu"]).is_err());
        assert!(keys(&["ctrl+é"]).is_err());
        assert!(keys(&["ctrl+"]).is_err());
    }
}
