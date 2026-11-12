//! The terminal mirror for a pane in the terminal holder
//! (`terminal-holder.md` 5.1), the first slice (S1c).
//!
//! The view subscribes to the pane: a `size` line and a full frame with the
//! holder's repaint, then each of the PTY's writes as a frame of its own.
//! When the subscription reports a new size, the view subscribes again, so
//! its new repaint is the board's next `size` line and full screen. The
//! slice's client reports only its own resizes, which the view never makes:
//! a resize by another client reaches the view once `watch` events land. The child's
//! exit is `closed`, with how it ended, and a holder that stops answering
//! is `unavailable`, after which the board opens the view again.
//!
//! A view asked for a Fit sets the pane to the board's size, follows the
//! board's refits, and gives the pane back the size it had before the Fit
//! when the view ends. It is single-viewer: one Fit holds a pane at a
//! time, and a second view that asks for one says `unfit` and goes on at
//! the pane's size. Each daemon resize reopens every view on the pane, as
//! the holder tells no other subscription. H11 makes the Fit a size lease
//! on the view's own subscription (`terminal-holder.md` 2.6), shared by
//! many viewers and dropped by the holder when the daemon dies; until
//! then, a daemon that dies mid-Fit leaves the pane at the board's size.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use base64::Engine as _;
use tiller_panes::{PaneId, PanesError, Update};
use tokio::sync::{OwnedMutexGuard, mpsc, watch};

use crate::dto::TerminalLine;
use crate::holder::{Holder, Live, LiveControl, exit_reason};
use crate::mirror::{Fits, Viewer, queue, stopping, unavailable};

/// Updates a subscription's thread can hand on before it waits for the
/// view to take them.
const UPDATES: usize = 64;

/// The most output bytes one frame gathers from a burst, so a fast writer
/// cannot make one frame without end or keep the view from its stop.
const BURST: usize = 64 * 1024;

/// How long the daemon waits for the holder to take a Fit's size.
const RESIZE_WAIT: Duration = Duration::from_secs(5);

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

/// The NDJSON body that streams the holder's `pane` to one viewer, with
/// the pane at the board's size `fit` while the view is open.
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

/// What a second view that asks for a Fit on the same pane gets.
const BUSY: &str = "another view holds a Fit on this pane, or is still giving it back";

/// A Fit's hold on the pane, one at a time per pane until H11.
struct Fit {
    _turn: OwnedMutexGuard<()>,
    /// The board's size, as it refits.
    board: watch::Receiver<(u16, u16)>,
    /// The size to give the pane back: its size before the Fit, from the
    /// first repaint, or the size another client set since.
    back: Option<(u16, u16)>,
    /// The size this Fit last set.
    set: Option<(u16, u16)>,
    /// The pane's size at the view's last repaint.
    now: Option<(u16, u16)>,
}

/// Sets the pane to `size` off the async threads, and tells every view on
/// the pane to open again.
async fn set_size(
    holder: &Arc<dyn Holder>,
    fits: &Fits,
    pane: &PaneId,
    size: (u16, u16),
) -> Result<(), String> {
    let (sizer, asked) = (Arc::clone(holder), pane.clone());
    let task = tokio::task::spawn_blocking(move || {
        sizer
            .set_size(&asked, size.0, size.1)
            .map_err(|error| error.to_string())
    });
    let result = match tokio::time::timeout(RESIZE_WAIT, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("the terminal holder did not take the Fit's size in time".to_owned()),
    };
    if result.is_ok() {
        fits.resized(pane.as_str());
    }
    result
}

/// Gives the pane back the size it had before the Fit, or the size another
/// client set since, if the pane is not at it now. A Fit that never set a
/// size, a pane that has exited, or a holder that is gone, has nothing to
/// give back.
async fn give_back(holder: &Arc<dyn Holder>, fits: &Fits, pane: &PaneId, fit: Fit) {
    let (Some(back), Some(set)) = (fit.back, fit.set) else {
        return;
    };
    if back == fit.now.unwrap_or(set) {
        return;
    }
    match set_size(holder, fits, pane, back).await {
        Ok(()) => {
            tracing::info!(pane = %pane, cols = back.0, rows = back.1, "gave a holder pane back its size after a Fit")
        }
        Err(error) => {
            tracing::info!(pane = %pane, %error, "could not give a holder pane back its size after a Fit")
        }
    }
}

/// The board's next size for the Fit; never for a view without one.
async fn refitted(fit: &mut Option<Fit>) -> (u16, u16) {
    if let Some(fit) = fit
        && fit.board.changed().await.is_ok()
    {
        return *fit.board.borrow_and_update();
    }
    std::future::pending().await
}

/// The next count of the daemon's resizes of the pane; never once nothing
/// can resize it.
async fn resized(sized: &mut watch::Receiver<u64>) {
    if sized.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Streams `pane` to `tx` with the board's Fit, if it asked for one, and
/// gives the pane back its size when the view ends.
async fn mirror(
    holder: &Arc<dyn Holder>,
    pane: &PaneId,
    fit: Option<(u16, u16)>,
    fits: &Arc<Fits>,
    stop: watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) {
    let mut held = None;
    if let Some(size) = fit {
        match fits.turn(pane.as_str()).await {
            Some(turn) => {
                held = Some(Fit {
                    _turn: turn,
                    board: fits.board(pane.as_str(), size),
                    back: None,
                    set: None,
                    now: None,
                });
            }
            None => {
                if !queue(
                    tx,
                    &TerminalLine::Unfit {
                        message: BUSY.to_owned(),
                    },
                ) {
                    return;
                }
            }
        }
    }
    view(holder, pane, &mut held, fits, stop, tx).await;
    if let Some(fit) = held {
        give_back(holder, fits, pane, fit).await;
    }
}

/// What woke a view.
enum Woke {
    Update(Option<std::result::Result<Option<Update>, PanesError>>),
    /// The board asked the Fit for a new size.
    Refit((u16, u16)),
    /// The daemon resized the pane: the view opens again at its new size.
    Resized,
}

/// Sets the Fit's size, or ends the Fit with an `unfit` line when the
/// holder would not take it. False when the browser left.
async fn fit_to(
    holder: &Arc<dyn Holder>,
    fits: &Fits,
    pane: &PaneId,
    held: &mut Option<Fit>,
    size: (u16, u16),
    tx: &mpsc::Sender<Bytes>,
) -> bool {
    match set_size(holder, fits, pane, size).await {
        Ok(()) => {
            if let Some(fit) = held.as_mut() {
                fit.set = Some(size);
            }
            true
        }
        Err(error) => {
            if let Some(fit) = held.take() {
                give_back(holder, fits, pane, fit).await;
            }
            queue(
                tx,
                &TerminalLine::Unfit {
                    message: format!(
                        "the terminal holder would not fit the pane ({error}); the view shows the pane at its own size"
                    ),
                },
            )
        }
    }
}

/// Streams `pane` to `tx` until the browser leaves, the child exits, the
/// holder stops answering or the daemon stops.
async fn view(
    holder: &Arc<dyn Holder>,
    pane: &PaneId,
    held: &mut Option<Fit>,
    fits: &Fits,
    mut stop: watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) {
    let mut sized = fits.sized(pane.as_str());
    let mut seq = 0;
    'views: loop {
        sized.borrow_and_update();
        let mut view = match open(holder, pane).await {
            Ok(view) => view,
            Err(error) => return unavailable(tx, &error),
        };
        // An update read while gathering a burst, for the next turn.
        let mut later = None;
        loop {
            let update = match later.take() {
                Some(update) => Some(update),
                None => match tokio::select! {
                    update = view.updates.recv() => Woke::Update(update),
                    size = refitted(held) => Woke::Refit(size),
                    () = resized(&mut sized) => Woke::Resized,
                    () = stopping(&mut stop) => return,
                    () = tx.closed() => return,
                } {
                    Woke::Update(update) => update,
                    Woke::Refit(size) => {
                        // Before the first repaint the size to give back is
                        // not known yet: the repaint fits to the board's
                        // newest size.
                        if held
                            .as_ref()
                            .is_none_or(|fit| fit.back.is_none() || fit.set == Some(size))
                        {
                            continue;
                        }
                        if !fit_to(holder, fits, pane, held, size, tx).await {
                            return;
                        }
                        continue 'views;
                    }
                    Woke::Resized => continue 'views,
                },
            };
            // A burst of output already waiting goes as one frame, so a
            // fast writer costs the browser fewer lines.
            let update = match update {
                Some(Ok(Some(Update::Output(mut bytes)))) => {
                    while bytes.len() < BURST
                        && let Ok(next) = view.updates.try_recv()
                    {
                        match next {
                            Ok(Some(Update::Output(more))) => bytes.extend_from_slice(&more),
                            other => {
                                later = Some(other);
                                break;
                            }
                        }
                    }
                    Some(Ok(Some(Update::Output(bytes))))
                }
                other => other,
            };
            // The first repaint of a Fit is the size to give back. A Fit
            // sets its size once: a pane another client sized since shows
            // at that size, and that size is the one to give back.
            if let Some(Ok(Some(Update::Repaint { cols, rows, .. }))) = &update
                && let Some(fit) = held.as_mut()
            {
                let own = (*cols, *rows);
                fit.now = Some(own);
                match fit.set {
                    Some(set) if set != own => fit.back = Some(own),
                    _ => {
                        fit.back.get_or_insert(own);
                    }
                }
                let board = *fit.board.borrow_and_update();
                if own != board && fit.set != Some(board) {
                    if !fit_to(holder, fits, pane, held, board, tx).await {
                        return;
                    }
                    continue 'views;
                }
            }
            let line = match update {
                Some(Ok(Some(Update::Repaint { cols, rows, bytes }))) => {
                    if !deliver(tx, &mut stop, &TerminalLine::Size { cols, rows }).await {
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
            if !deliver(tx, &mut stop, &line).await || closed {
                return;
            }
        }
    }
}

/// Sends one NDJSON line, waiting while the browser's queue is full: the
/// holder's output is a byte stream, so no frame may drop. A browser that
/// stays behind backs the subscription up until the holder says `Lagged`,
/// and the view opens again with a repaint. False when the browser left or
/// the daemon stops.
async fn deliver(
    tx: &mpsc::Sender<Bytes>,
    stop: &mut watch::Receiver<bool>,
    message: &TerminalLine,
) -> bool {
    let Ok(mut line) = serde_json::to_vec(message) else {
        return false;
    };
    line.push(b'\n');
    tokio::select! {
        sent = tx.send(Bytes::from(line)) => sent.is_ok(),
        () = stopping(stop) => false,
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
        "tab" if shift && alt => Some("\x1b\x1b[Z".to_owned()),
        "tab" if shift => Some("\x1b[Z".to_owned()),
        _ => None,
    };
    if let Some(sequence) = sequence {
        return Some(sequence.into_bytes());
    }
    let mut bytes = match rest {
        "enter" => vec![b'\r'],
        "tab" => vec![b'\t'],
        // xterm sends ^H for Ctrl+Backspace, which line editors read apart
        // from Backspace.
        "backspace" if ctrl => vec![0x08],
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
            keys(&["ctrl+backspace", "alt+shift+tab"]).unwrap(),
            b"\x08\x1b\x1b[Z"
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
