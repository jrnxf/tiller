//! The terminal mirror for a pane in the terminal holder
//! (`terminal-holder.md` 5.1), the first slice (S1c).
//!
//! The view subscribes to the pane: a `size` line and a full frame with the
//! holder's repaint, then each of the PTY's writes as a frame of its own.
//! The child's exit is `closed`, with how it ended, and a holder that stops
//! answering is `unavailable`, after which the board opens the view again.
//!
//! A Fit holds a second subscription that only sets the size: the holder
//! gives the pane its size back when that subscription ends, so a view that
//! ends, a browser that leaves and a daemon that dies all give it back. Each
//! size the Fit sets is answered with a new view subscription, whose
//! repaint is the board's new full screen.

use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use base64::Engine as _;
use tiller_panes::{PaneId, PanesError, Update};
use tokio::sync::{mpsc, watch};

use crate::dto::TerminalLine;
use crate::holder::{Holder, Live, LiveControl, exit_reason};
use crate::mirror::{Fits, Viewer, queue, stopping, unavailable};

/// Updates a subscription's thread can hand on before it waits for the
/// view to take them.
const UPDATES: usize = 64;

/// A subscription read on a thread of its own, since its `recv` blocks.
struct Pumped {
    updates: mpsc::Receiver<Result<Option<Update>, PanesError>>,
    control: Arc<dyn LiveControl>,
}

impl Pumped {
    /// Reads `live` until it ends, fails or nobody takes its updates. With
    /// `sizes_only`, the PTY's output is dropped on the thread: a Fit's
    /// subscription reads only to keep up.
    fn start(mut live: Box<dyn Live>, sizes_only: bool) -> std::io::Result<Self> {
        let control = live.control();
        let (tx, updates) = mpsc::channel(UPDATES);
        std::thread::Builder::new()
            .name("tiller-holder-view".into())
            .spawn(move || {
                loop {
                    let update = live.recv();
                    let last = !matches!(update, Ok(Some(_)));
                    if sizes_only && matches!(update, Ok(Some(Update::Output(_)))) {
                        continue;
                    }
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
async fn open(holder: &Arc<dyn Holder>, pane: &PaneId, sizes_only: bool) -> Result<Pumped, String> {
    let holder = Arc::clone(holder);
    let pane = pane.clone();
    tokio::task::spawn_blocking(move || {
        let live = holder.subscribe(&pane).map_err(|error| error.to_string())?;
        Pumped::start(live, sizes_only).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The NDJSON body that streams the holder's `pane` to one viewer, at the
/// board's `fit` when it asks for one.
pub fn stream(
    holder: Arc<dyn Holder>,
    pane: PaneId,
    fit: Option<(u16, u16)>,
    fits: Arc<Fits>,
    stop: watch::Receiver<bool>,
    viewer: Viewer,
) -> Body {
    let (tx, mut rx) = mpsc::channel::<Bytes>(crate::mirror::QUEUE);
    tokio::spawn(async move {
        let _viewer = viewer;
        mirror(&holder, &pane, fit, &fits, stop, &tx).await;
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

/// What a view's wait woke on.
enum Woke {
    View(Option<Result<Option<Update>, PanesError>>),
    Fit(Option<Result<Option<Update>, PanesError>>),
    Refit((u16, u16)),
    Over,
}

/// The board's next size for a Fit; never without one.
async fn refitted(board: &mut Option<watch::Receiver<(u16, u16)>>) -> (u16, u16) {
    if let Some(board) = board
        && board.changed().await.is_ok()
    {
        return *board.borrow_and_update();
    }
    std::future::pending().await
}

/// The next update of a Fit's subscription; never without one.
async fn fit_update(lease: &mut Option<Pumped>) -> Option<Result<Option<Update>, PanesError>> {
    match lease {
        Some(lease) => lease.updates.recv().await,
        None => std::future::pending().await,
    }
}

/// Waits for the Fit's subscription to say the holder took its size.
async fn sized(lease: &mut Pumped) -> Result<(), String> {
    loop {
        match lease.updates.recv().await {
            Some(Ok(Some(Update::Resized { .. }))) => return Ok(()),
            Some(Ok(Some(Update::Repaint { .. } | Update::Output(_)))) => {}
            Some(Err(error)) => return Err(error.to_string()),
            _ => return Err("the terminal holder ended the Fit".to_owned()),
        }
    }
}

/// Asks the Fit's subscription for a size, off the async threads since it
/// writes to the holder's socket.
async fn resize(lease: &Pumped, (cols, rows): (u16, u16)) -> Result<(), String> {
    let control = Arc::clone(&lease.control);
    tokio::task::spawn_blocking(move || control.resize(cols, rows))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Streams `pane` to `tx` until the browser leaves, the child exits, the
/// holder stops answering or the daemon stops.
async fn mirror(
    holder: &Arc<dyn Holder>,
    pane: &PaneId,
    fit: Option<(u16, u16)>,
    fits: &Arc<Fits>,
    mut stop: watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) {
    let mut lease = None;
    let mut board = None;
    if let Some(fit) = fit {
        let opened = async {
            let mut opened = open(holder, pane, true).await?;
            resize(&opened, fit).await?;
            sized(&mut opened).await?;
            Ok::<_, String>(opened)
        };
        match opened.await {
            Ok(opened) => lease = Some(opened),
            Err(error) => return unavailable(tx, &error),
        }
        board = Some(fits.board(pane.as_str(), fit));
    }
    let mut seq = 0;
    'views: loop {
        let mut view = match open(holder, pane, false).await {
            Ok(view) => view,
            Err(error) => return unavailable(tx, &error),
        };
        loop {
            let woke = tokio::select! {
                update = view.updates.recv() => Woke::View(update),
                update = fit_update(&mut lease) => Woke::Fit(update),
                size = refitted(&mut board) => Woke::Refit(size),
                () = stopping(&mut stop) => Woke::Over,
                () = tx.closed() => Woke::Over,
            };
            let line = match woke {
                Woke::View(Some(Ok(Some(Update::Repaint { cols, rows, bytes })))) => {
                    if !queue(tx, &TerminalLine::Size { cols, rows }) {
                        return;
                    }
                    frame(&mut seq, true, &bytes)
                }
                Woke::View(Some(Ok(Some(Update::Output(bytes))))) => frame(&mut seq, false, &bytes),
                Woke::View(Some(Ok(Some(Update::Resized { .. })))) => continue,
                Woke::View(Some(Ok(Some(Update::Exited(exit))))) => TerminalLine::Closed {
                    reason: Some(exit_reason(Some(&exit))),
                },
                Woke::View(Some(Ok(Some(Update::Lagged)))) => {
                    return unavailable(tx, "the view fell behind the terminal holder");
                }
                Woke::View(Some(Err(error))) => return unavailable(tx, &error.to_string()),
                // The holder ended the stream without an exit: the board
                // opens it again.
                Woke::View(Some(Ok(None)) | None) => return,
                Woke::Fit(Some(Ok(Some(Update::Resized { .. })))) => continue 'views,
                // The view says how the child ended; the Fit has nothing
                // left to hold.
                Woke::Fit(Some(Ok(Some(Update::Exited(_))))) => {
                    lease = None;
                    continue;
                }
                Woke::Fit(Some(Ok(Some(_)))) => continue,
                Woke::Fit(Some(Err(error))) => return unavailable(tx, &error.to_string()),
                Woke::Fit(Some(Ok(None)) | None) => {
                    return unavailable(tx, "the terminal holder ended the Fit");
                }
                Woke::Refit(size) => {
                    if let Some(lease) = &lease
                        && let Err(error) = resize(lease, size).await
                    {
                        return unavailable(tx, &error);
                    }
                    continue;
                }
                Woke::Over => return,
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
