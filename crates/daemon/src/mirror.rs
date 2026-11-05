//! The terminal mirror (`protocol.md` 7.4, Terminal mirror): an agent's
//! live herdr pane streamed to the board, and the owner's keys sent back.
//!
//! Each viewer gets its own `herdr terminal session observe` child, run at
//! the pane's own size so herdr never has to reflow the screen, and the
//! child dies with the viewer. Its NDJSON is drained as fast as herdr
//! writes it into a short queue; a browser that falls behind the queue has
//! its stream ended rather than buffered, and its reconnect starts from a
//! fresh full frame. The pane's size is read again every 2 seconds, and a
//! change restarts the child at the new size.

use std::convert::Infallible;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use serde::Deserialize;
use serde_json::Value;
use tiller_herdr::client::Cli;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};

use crate::dto::TerminalLine;

/// Terminal views open at once across the board, every tab included.
pub const MAX_VIEWERS: usize = 8;
/// The most keys one input carries.
pub const MAX_KEYS: usize = 64;
/// The most bytes of pasted text one input carries.
pub const MAX_TEXT: usize = 4096;
/// Inputs a second the owner may send on average, and in a burst.
const INPUT_RATE: f64 = 20.0;
const INPUT_BURST: f64 = 40.0;
/// Lines waiting for a browser before its stream is dropped.
const QUEUE: usize = 64;
/// The longest line herdr may write; a full frame of a large pane with
/// heavy styling stays well under it.
const MAX_LINE: usize = 4 << 20;
/// How often the pane's size is read again.
const SIZE_POLL: Duration = Duration::from_secs(2);
/// The size read's own deadline, so a stuck herdr cannot stall a stream.
const SIZE_TIMEOUT: Duration = Duration::from_secs(3);
/// What a failed observe's stderr is kept to.
const STDERR_KEEP: u64 = 2048;

/// The mirror's shared counters: open views and the input budget.
#[derive(Debug)]
pub struct Mirrors {
    viewers: Arc<AtomicUsize>,
    budget: Mutex<Budget>,
}

impl Default for Mirrors {
    fn default() -> Self {
        Self {
            viewers: Arc::new(AtomicUsize::new(0)),
            budget: Mutex::new(Budget {
                tokens: INPUT_BURST,
                at: Instant::now(),
            }),
        }
    }
}

/// A token bucket: `INPUT_RATE` inputs a second, `INPUT_BURST` at once.
#[derive(Debug)]
struct Budget {
    tokens: f64,
    at: Instant,
}

/// One open view, counted until it drops.
#[derive(Debug)]
pub struct Viewer(Arc<AtomicUsize>);

impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Mirrors {
    /// A slot for one more view, or `None` at the cap.
    pub fn viewer(&self) -> Option<Viewer> {
        self.viewers
            .try_update(Ordering::AcqRel, Ordering::Acquire, |open| {
                (open < MAX_VIEWERS).then_some(open + 1)
            })
            .ok()
            .map(|_| Viewer(self.viewers.clone()))
    }

    /// Views open now.
    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::Acquire)
    }

    /// Spends one input from the budget; false when it is spent.
    pub fn spend(&self) -> bool {
        self.spend_at(Instant::now())
    }

    fn spend_at(&self, now: Instant) -> bool {
        let mut budget = self.budget.lock().unwrap_or_else(PoisonError::into_inner);
        let earned = now.saturating_duration_since(budget.at).as_secs_f64() * INPUT_RATE;
        budget.tokens = (budget.tokens + earned).min(INPUT_BURST);
        budget.at = now;
        if budget.tokens < 1.0 {
            return false;
        }
        budget.tokens -= 1.0;
        true
    }
}

/// Why an input is refused before it reaches herdr.
pub fn check_keys(keys: &[String]) -> Result<(), String> {
    if keys.is_empty() || keys.len() > MAX_KEYS {
        return Err(format!("keys holds 1 to {MAX_KEYS} keys"));
    }
    match keys.iter().find(|key| !plausible_key(key)) {
        Some(key) => Err(format!("{key:?} is not a key")),
        None => Ok(()),
    }
}

/// A key herdr might know: a name or one character, after any modifiers,
/// with no space or control character. herdr refuses the rest itself.
fn plausible_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 32 && !key.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn check_text(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(format!("text holds 1 to {MAX_TEXT} bytes"));
    }
    Ok(())
}

/// The NDJSON body that streams `pane` to one viewer, starting at `size`.
/// The stream ends when the browser leaves, the pane closes, herdr stops
/// answering, or the daemon stops; the observe child is killed with it.
pub fn stream(
    herdr: Cli,
    pane: String,
    size: (u16, u16),
    stop: watch::Receiver<bool>,
    viewer: Viewer,
) -> Body {
    let (tx, mut rx) = mpsc::channel::<Bytes>(QUEUE);
    tokio::spawn(async move {
        let _viewer = viewer;
        mirror(&herdr, &pane, size, stop, &tx).await;
    });
    let lines = futures_util::stream::poll_fn(move |cx| {
        rx.poll_recv(cx).map(|line| line.map(Ok::<_, Infallible>))
    });
    Body::from_stream(lines)
}

/// What woke a stream.
enum Woke {
    Read(std::io::Result<bool>),
    Tick,
    /// The browser left or the daemon is stopping.
    Over,
}

/// How one observe child ended.
enum Ended {
    /// The pane changed size: observe again at this one.
    Resized((u16, u16)),
    /// The stream is over, for the browser or for herdr.
    Done,
}

async fn mirror(
    herdr: &Cli,
    pane: &str,
    mut size: (u16, u16),
    mut stop: watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) {
    loop {
        let (cols, rows) = size;
        if !queue(tx, &TerminalLine::Size { cols, rows }) {
            return;
        }
        let mut command = Command::from(herdr.observe_command(pane, cols, rows));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                unavailable(tx, &format!("cannot run herdr: {error}"));
                return;
            }
        };
        let ended = observe(herdr, pane, size, &mut child, &mut stop, tx).await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        match ended {
            Ended::Resized(new) => size = new,
            Ended::Done => return,
        }
    }
}

/// Passes one child's frames on until it, the browser or the pane ends.
async fn observe(
    herdr: &Cli,
    pane: &str,
    size: (u16, u16),
    child: &mut Child,
    stop: &mut watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) -> Ended {
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        unavailable(tx, "herdr observe has no output");
        return Ended::Done;
    };
    // A failing observe explains itself on stderr; only its start is kept.
    let errors = tokio::spawn(async move {
        let mut kept = String::new();
        let _ = (&mut stderr)
            .take(STDERR_KEEP)
            .read_to_string(&mut kept)
            .await;
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        kept
    });
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let mut first = true;
    let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + SIZE_POLL, SIZE_POLL);
    let sizer = herdr.clone().timeout(SIZE_TIMEOUT);
    loop {
        // The arms only say what happened; awaiting inside one would hold
        // the watch guard across the await.
        let woke = tokio::select! {
            read = next_line(&mut reader, &mut line) => Woke::Read(read),
            _ = poll.tick() => Woke::Tick,
            () = tx.closed() => Woke::Over,
            () = stopping(stop) => Woke::Over,
        };
        match woke {
            Woke::Read(Ok(true)) => {}
            Woke::Read(Ok(false)) => {
                let said = tokio::time::timeout(Duration::from_secs(1), errors).await;
                let said = said.ok().and_then(Result::ok).unwrap_or_default();
                let said = said.trim();
                unavailable(
                    tx,
                    if said.is_empty() {
                        "herdr observe ended"
                    } else {
                        said
                    },
                );
                return Ended::Done;
            }
            Woke::Read(Err(error)) => {
                unavailable(tx, &format!("herdr observe: {error}"));
                return Ended::Done;
            }
            Woke::Tick => {
                let (sizer, pane) = (sizer.clone(), pane.to_owned());
                match tokio::task::spawn_blocking(move || sizer.pane_size(&pane)).await {
                    Ok(Ok(Some(now))) if now != size => return Ended::Resized(now),
                    Ok(Ok(None)) => {
                        let reason = Some("pane_closed".to_owned());
                        queue(tx, &TerminalLine::Closed { reason });
                        return Ended::Done;
                    }
                    // herdr itself ends the observe when it goes away.
                    _ => {}
                }
                continue;
            }
            Woke::Over => return Ended::Done,
        }
        let parsed = serde_json::from_slice::<Observed>(&line);
        line.clear();
        let Ok(observed) = parsed else {
            continue;
        };
        match observed.kind.as_str() {
            "terminal.frame" => {
                // Diffs mean nothing without the full frame before them; a
                // child that starts on one is asked again.
                if first && !observed.full {
                    return Ended::Resized(size);
                }
                first = false;
                let frame = TerminalLine::Frame {
                    seq: observed.seq,
                    full: observed.full,
                    bytes: observed.bytes,
                };
                if !queue(tx, &frame) {
                    return Ended::Done;
                }
            }
            "terminal.closed" => {
                let reason = observed.reason.map(|reason| match reason {
                    Value::String(reason) => reason,
                    other => other.to_string(),
                });
                queue(tx, &TerminalLine::Closed { reason });
                return Ended::Done;
            }
            _ => {}
        }
    }
}

/// Returns once the daemon is stopping; never when nothing can stop it.
async fn stopping(stop: &mut watch::Receiver<bool>) {
    if stop.wait_for(|stop| *stop).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Reads up to the end of a line into `line`, without its newline; false
/// at the end of the output. The caller clears `line` once it has used it,
/// so a read cancelled midway keeps what it had. A line longer than
/// `MAX_LINE` is an error, never an unbounded buffer.
async fn next_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    line: &mut Vec<u8>,
) -> std::io::Result<bool> {
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(!line.is_empty());
        }
        let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
            Some(end) => (end, true),
            None => (chunk.len(), false),
        };
        if line.len() + take > MAX_LINE {
            return Err(std::io::Error::other(format!(
                "a frame is larger than {MAX_LINE} bytes"
            )));
        }
        line.extend_from_slice(&chunk[..take]);
        reader.consume(if done { take + 1 } else { take });
        if done {
            return Ok(true);
        }
    }
}

/// One line of `herdr terminal session observe` (herdr 0.9.5).
#[derive(Deserialize)]
struct Observed {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    full: bool,
    #[serde(default)]
    bytes: String,
    #[serde(default)]
    reason: Option<Value>,
}

/// Queues one NDJSON line without waiting; false when the browser left or
/// fell a whole queue behind, which ends its stream.
fn queue(tx: &mpsc::Sender<Bytes>, message: &TerminalLine) -> bool {
    let Ok(mut line) = serde_json::to_vec(message) else {
        return false;
    };
    line.push(b'\n');
    tx.try_send(Bytes::from(line)).is_ok()
}

fn unavailable(tx: &mpsc::Sender<Bytes>, message: &str) {
    let message = message.to_owned();
    queue(tx, &TerminalLine::Unavailable { message });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_input_budget_refills_at_its_rate() {
        let mirrors = Mirrors::default();
        let start = Instant::now();
        let spent = (0..100).filter(|_| mirrors.spend_at(start)).count();
        assert_eq!(spent, INPUT_BURST as usize);
        // Half a second earns ten more.
        let later = start + Duration::from_millis(500);
        let spent = (0..100).filter(|_| mirrors.spend_at(later)).count();
        assert_eq!(spent, 10);
    }

    #[test]
    fn the_viewer_cap_holds_and_frees() {
        let mirrors = Mirrors::default();
        let open: Vec<_> = (0..MAX_VIEWERS).filter_map(|_| mirrors.viewer()).collect();
        assert_eq!(open.len(), MAX_VIEWERS);
        assert!(mirrors.viewer().is_none());
        drop(open);
        assert_eq!(mirrors.viewers(), 0);
        assert!(mirrors.viewer().is_some());
    }

    #[test]
    fn keys_are_words_and_text_is_bounded() {
        let keys = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert!(check_keys(&keys(&["ctrl+c", "-", "a", "enter"])).is_ok());
        assert!(check_keys(&keys(&[])).is_err());
        assert!(check_keys(&keys(&["a b"])).is_err());
        assert!(check_keys(&keys(&["\u{1b}"])).is_err());
        assert!(check_keys(&vec!["a".to_owned(); MAX_KEYS + 1]).is_err());
        assert!(check_text("-rf").is_ok());
        assert!(check_text("").is_err());
        assert!(check_text(&"x".repeat(MAX_TEXT + 1)).is_err());
    }

    #[tokio::test]
    async fn a_line_past_the_cap_is_an_error_not_a_buffer() {
        let mut big = vec![b'x'; MAX_LINE + 1];
        big.push(b'\n');
        let mut reader = BufReader::new(&big[..]);
        let mut line = Vec::new();
        assert!(next_line(&mut reader, &mut line).await.is_err());
        assert!(line.len() <= MAX_LINE);
        line.clear();

        let mut reader = BufReader::new(&b"one\ntwo"[..]);
        assert!(next_line(&mut reader, &mut line).await.unwrap());
        assert_eq!(line, b"one");
        line.clear();
        assert!(next_line(&mut reader, &mut line).await.unwrap());
        assert_eq!(line, b"two");
        line.clear();
        assert!(!next_line(&mut reader, &mut line).await.unwrap());
    }
}
