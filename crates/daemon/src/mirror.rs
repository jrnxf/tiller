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
//!
//! The Fit is the one exception to "never resize": a view that asks for it
//! runs `herdr terminal session control` at the board's size instead, and
//! hands the pane back at its own size when the view ends. The size to give
//! back is kept on disk until then, so a daemon that dies mid-Fit gives it
//! back when it starts again. A board that changes size refits in place: the
//! new size goes to the open control session, so the pane is never given
//! back between two sizes.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use serde::Deserialize;
use serde_json::Value;
use tiller_herdr::client::{Cells, Cli, Tty};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{OwnedMutexGuard, mpsc, watch};
use tokio::task::JoinHandle;

use crate::dto::TerminalLine;

/// Terminal views open at once across the board, every tab included.
pub const MAX_VIEWERS: usize = 8;
/// The most keys one input carries.
pub const MAX_KEYS: usize = 64;
/// The most bytes of pasted text one input carries.
pub const MAX_TEXT: usize = 4096;
/// The most rows of scrollback one history read returns: herdr's own cap.
pub const MAX_HISTORY: u16 = 1000;
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
/// Observe children in a row that may start on a diff before the stream
/// gives up, so a herdr that never sends a full frame cannot spin.
const FIRST_FULL_TRIES: u32 = 3;
/// The sizes a Fit may ask for, in cells.
const FIT_COLS: std::ops::RangeInclusive<u16> = 20..=500;
const FIT_ROWS: std::ops::RangeInclusive<u16> = 5..=200;
/// How long herdr has to take the pane back before its child is killed.
const GIVE_BACK: Duration = Duration::from_secs(3);
/// How long a Fit waits for the one before it on the same pane to give the
/// pane back: longer than a give-back can take.
const TURN_WAIT: Duration = Duration::from_secs(8);
/// Diffs a Fit passes over while it waits for a full frame, before it gives
/// up on herdr ever sending one.
const FIT_DIFFS: u32 = 64;
/// Where the sizes to give back are kept, under the daemon's home.
const FITS_FILE: &str = "mirror-fits.json";

/// The mirror's shared state: open views, the input budget and the Fits.
#[derive(Debug)]
pub struct Mirrors {
    viewers: Arc<AtomicUsize>,
    budget: Mutex<Budget>,
    fits: Arc<Fits>,
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
    /// The mirror for a daemon whose home is `home`, with the Fits it left
    /// behind read back.
    pub fn new(home: &Path) -> Self {
        Self {
            viewers: Arc::new(AtomicUsize::new(0)),
            budget: Mutex::new(Budget {
                tokens: INPUT_BURST,
                at: Instant::now(),
            }),
            fits: Arc::new(Fits::at(home.join(FITS_FILE))),
        }
    }

    pub fn fits(&self) -> Arc<Fits> {
        self.fits.clone()
    }

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
/// with no space or control character. herdr refuses the rest itself, but
/// it reads a word that starts with a dash as one of its flags, so only
/// `-` itself (the minus key) may start with one.
fn plausible_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && (key == "-" || !key.starts_with('-'))
        && !key.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn check_text(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_TEXT {
        return Err(format!("text holds 1 to {MAX_TEXT} bytes"));
    }
    Ok(())
}

/// Why a Fit's size is refused before it reaches herdr.
pub fn check_fit(cols: u16, rows: u16) -> Result<(u16, u16), String> {
    if !FIT_COLS.contains(&cols) || !FIT_ROWS.contains(&rows) {
        return Err(format!(
            "fit is {} to {} cols by {} to {} rows",
            FIT_COLS.start(),
            FIT_COLS.end(),
            FIT_ROWS.start(),
            FIT_ROWS.end()
        ));
    }
    Ok((cols, rows))
}

/// The panes a Fit holds at the board's size, each with the size it goes
/// back to. One Fit runs per pane at a time.
#[derive(Debug)]
pub struct Fits {
    path: PathBuf,
    held: Mutex<BTreeMap<String, (u16, u16)>>,
    turns: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// The board's size for each pane a Fit streams now, for a refit.
    boards: Mutex<HashMap<String, watch::Sender<(u16, u16)>>>,
}

impl Fits {
    fn at(path: PathBuf) -> Self {
        let held = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(path = %path.display(), %error, "unreadable Fit sizes; none are given back");
                BTreeMap::new()
            }),
            Err(_) => BTreeMap::new(),
        };
        Self {
            path,
            held: Mutex::new(held),
            turns: Mutex::new(HashMap::new()),
            boards: Mutex::new(HashMap::new()),
        }
    }

    /// The panes still held, with the size each goes back to.
    pub fn held(&self) -> BTreeMap<String, (u16, u16)> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Notes that `pane` goes back to `size`, on disk before herdr resizes it.
    async fn hold(self: &Arc<Self>, pane: &str, size: (u16, u16)) {
        let pane = pane.to_owned();
        self.change(move |held| held.insert(pane, size) != Some(size))
            .await;
    }

    /// `pane` has its size back.
    async fn release(self: &Arc<Self>, pane: &str) {
        let pane = pane.to_owned();
        self.change(move |held| held.remove(&pane).is_some()).await;
    }

    /// Applies `change` and saves the set when it says it changed, off the
    /// async threads since it writes a file.
    async fn change(
        self: &Arc<Self>,
        change: impl FnOnce(&mut BTreeMap<String, (u16, u16)>) -> bool + Send + 'static,
    ) {
        let fits = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let mut held = fits.held.lock().unwrap_or_else(PoisonError::into_inner);
            if change(&mut held) {
                fits.save(&held);
            }
        })
        .await;
    }

    /// Starts following the board's size for the Fit on `pane`, at `fit`.
    fn board(&self, pane: &str, fit: (u16, u16)) -> watch::Receiver<(u16, u16)> {
        let (sender, receiver) = watch::channel(fit);
        let mut boards = self.boards.lock().unwrap_or_else(PoisonError::into_inner);
        boards.retain(|_, sender| !sender.is_closed());
        boards.insert(pane.to_owned(), sender);
        receiver
    }

    /// Gives the Fit streaming `pane` the board's new size; false when no
    /// Fit streams it.
    pub fn refit(&self, pane: &str, fit: (u16, u16)) -> bool {
        let boards = self.boards.lock().unwrap_or_else(PoisonError::into_inner);
        match boards.get(pane) {
            Some(sender) if !sender.is_closed() => {
                sender.send_if_modified(|now| std::mem::replace(now, fit) != fit);
                true
            }
            _ => false,
        }
    }

    /// Rewrites the file atomically; an empty set removes it.
    fn save(&self, held: &BTreeMap<String, (u16, u16)>) {
        if let Err(error) = self.write(held) {
            tracing::warn!(path = %self.path.display(), %error, "cannot keep the Fit sizes");
        }
    }

    fn write(&self, held: &BTreeMap<String, (u16, u16)>) -> std::io::Result<()> {
        if held.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        // A temporary file of its own for each write, so two writers never
        // rename each other's half-written file into place.
        let text = serde_json::to_string(held).map_err(std::io::Error::other)?;
        tiller_core::config_file::write_atomic(&self.path, &text)
    }

    /// This pane's turn, once the Fit before it has given the pane back;
    /// `None` when that takes longer than `TURN_WAIT`.
    async fn turn(&self, pane: &str) -> Option<OwnedMutexGuard<()>> {
        let turn = {
            let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
            turns.retain(|_, turn| Arc::strong_count(turn) > 1);
            turns.entry(pane.to_owned()).or_default().clone()
        };
        tokio::time::timeout(TURN_WAIT, turn.lock_owned())
            .await
            .ok()
    }

    /// Gives back every pane a Fit still held when the daemon last stopped,
    /// at the size it had before the Fit: herdr's layout can be smaller than
    /// the pane's PTY (see [`Tty`]), and a herdr client showing the pane lays
    /// it out again itself. A pane herdr no longer has, or that another
    /// client controls, is dropped from the list all the same: there is
    /// nothing left to undo.
    pub async fn restore_left(self: &Arc<Self>, herdr: &Cli) {
        for pane in self.held().into_keys() {
            let Some(_turn) = self.turn(&pane).await else {
                continue;
            };
            // A Fit that ran meanwhile gave the pane back itself.
            let Some(saved) = self.held().get(&pane).copied() else {
                continue;
            };
            let (sizer, asked) = (herdr.clone().timeout(SIZE_TIMEOUT), pane.clone());
            let gone = tokio::task::spawn_blocking(move || sizer.pane_size(&asked)).await;
            if !matches!(gone, Ok(Ok(None))) {
                let (cols, rows) = saved;
                let mut command = Command::from(herdr.control_command(&pane, cols, rows));
                command
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .kill_on_drop(true);
                if let Ok(mut child) = command.spawn()
                    && let Some(stdout) = child.stdout.take()
                {
                    let stdin = child.stdin.take();
                    give_back(&mut child, stdin, &mut BufReader::new(stdout), saved).await;
                    tracing::info!(pane, cols, rows, "gave a pane back its size after a Fit");
                }
            }
            self.release(&pane).await;
        }
    }
}

/// The NDJSON body that streams `pane` to one viewer, starting at the size
/// in `terminal` (read again from its PTY, when it has one), or at `fit`
/// with the pane resized to it. The stream ends when the browser
/// leaves, the pane closes, herdr stops answering, or the daemon stops; the
/// herdr child is killed with it, once a Fit has given the pane back.
pub fn stream(
    herdr: Cli,
    pane: String,
    terminal: (Cells, Option<Tty>),
    fit: Option<(u16, u16)>,
    fits: Arc<Fits>,
    stop: watch::Receiver<bool>,
    viewer: Viewer,
) -> Body {
    let (tx, mut rx) = mpsc::channel::<Bytes>(QUEUE);
    tokio::spawn(async move {
        let _viewer = viewer;
        let (size, tty) = terminal;
        mirror(&herdr, &pane, size, tty.as_ref(), fit, &fits, stop, &tx).await;
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
    /// The board asked a Fit for a new size.
    Refit((u16, u16)),
    /// The browser left or the daemon is stopping.
    Over,
}

/// How one herdr child ended.
enum Ended {
    /// The pane changed size: observe again at this one.
    Resized((u16, u16)),
    /// The child started on a diff: observe again for a full frame.
    NoFullFrame,
    /// herdr would not hand the pane to a Fit, and said why.
    Refused(String),
    /// The stream is over, for the browser or for herdr.
    Done,
}

/// A Fit's hold on the pane: where its size to give back is kept, the
/// board's size as it changes, herdr's control input to send it to, and the
/// pane's layout size last read, which alone tells that herdr laid the pane
/// out again while the Fit holds its PTY at the board's size.
struct Control<'a> {
    fits: &'a Arc<Fits>,
    board: watch::Receiver<(u16, u16)>,
    stdin: ChildStdin,
    layout: Option<(u16, u16)>,
}

/// The board's next size for a Fit; never for a plain observe, or once
/// nothing can refit it.
async fn refitted(control: &mut Option<&mut Control<'_>>) -> (u16, u16) {
    if let Some(control) = control
        && control.board.changed().await.is_ok()
    {
        return *control.board.borrow_and_update();
    }
    std::future::pending().await
}

/// herdr's control line that resizes the pane.
fn resize_line(cols: u16, rows: u16) -> String {
    format!("{{\"type\":\"terminal.resize\",\"cols\":{cols},\"rows\":{rows}}}\n")
}

#[allow(clippy::too_many_arguments)]
async fn mirror(
    herdr: &Cli,
    pane: &str,
    mut size: (u16, u16),
    tty: Option<&Tty>,
    fit: Option<(u16, u16)>,
    fits: &Arc<Fits>,
    mut stop: watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) {
    if let Some(fit) = fit {
        match fitted(herdr, pane, &mut size, tty, fit, fits, &mut stop, tx).await {
            // The view goes on unfitted, at the pane's own size.
            Some(message) => {
                if !queue(tx, &TerminalLine::Unfit { message }) {
                    return;
                }
            }
            None => return,
        }
    }
    let mut no_full = 0;
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
        let ended = match outputs(&mut child) {
            Some((mut reader, errors)) => {
                observe(
                    herdr,
                    pane,
                    &mut size,
                    tty,
                    None,
                    &mut reader,
                    errors,
                    &mut stop,
                    tx,
                )
                .await
            }
            None => {
                unavailable(tx, "herdr observe has no output");
                Ended::Done
            }
        };
        let _ = child.start_kill();
        let _ = child.wait().await;
        match ended {
            Ended::Resized(new) => {
                size = new;
                no_full = 0;
            }
            Ended::NoFullFrame => {
                no_full += 1;
                if no_full >= FIRST_FULL_TRIES {
                    unavailable(tx, "herdr observe never sent a full frame");
                    return;
                }
            }
            Ended::Done | Ended::Refused(_) => return,
        }
    }
}

/// Streams the pane at the board's size, `fit` and then any refit, through
/// herdr's control session, then gives it back at its own size, read into
/// `size` once the Fit has its turn, which becomes the layout's size if
/// herdr lays the pane out again meanwhile. `None` when the stream is over; herdr's reason when it would
/// not hand the pane over, for the caller to go on without the Fit.
#[allow(clippy::too_many_arguments)]
async fn fitted(
    herdr: &Cli,
    pane: &str,
    size: &mut (u16, u16),
    tty: Option<&Tty>,
    fit: (u16, u16),
    fits: &Arc<Fits>,
    stop: &mut watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) -> Option<String> {
    let Some(_turn) = fits.turn(pane).await else {
        return Some(
            "another view holds a Fit on this pane, or is still giving it back".to_owned(),
        );
    };
    // Read now, not when the view opened: a Fit before this one held the
    // pane at its board's size until it gave the pane back.
    let (sizer, asked, read) = (
        herdr.clone().timeout(SIZE_TIMEOUT),
        pane.to_owned(),
        tty.cloned(),
    );
    let sized = tokio::task::spawn_blocking(move || sizes(&sizer, &asked, read.as_ref())).await;
    let layout = match sized {
        Ok(Ok(Some((own, layout)))) => {
            *size = own;
            Some(layout)
        }
        _ => None,
    };
    fits.hold(pane, *size).await;
    let (cols, rows) = fit;
    let mut command = Command::from(herdr.control_command(pane, cols, rows));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            fits.release(pane).await;
            unavailable(tx, &format!("cannot run herdr: {error}"));
            return None;
        }
    };
    let ended = match (outputs(&mut child), child.stdin.take()) {
        (Some((mut reader, errors)), Some(stdin)) => {
            let mut control = Control {
                fits,
                board: fits.board(pane, fit),
                stdin,
                layout,
            };
            let ended = if queue(tx, &TerminalLine::Size { cols, rows }) {
                observe(
                    herdr,
                    pane,
                    size,
                    tty,
                    Some(&mut control),
                    &mut reader,
                    errors,
                    stop,
                    tx,
                )
                .await
            } else {
                Ended::Done
            };
            give_back(&mut child, Some(control.stdin), &mut reader, *size).await;
            ended
        }
        _ => {
            unavailable(tx, "herdr control has no output");
            Ended::Done
        }
    };
    let _ = child.start_kill();
    let _ = child.wait().await;
    fits.release(pane).await;
    match ended {
        Ended::Refused(message) => Some(message),
        _ => None,
    }
}

/// Asks herdr to put the pane back at `size` and let it go, then waits for
/// the child to leave. herdr drops its resize lock as the child detaches,
/// and a herdr client showing the pane lays it out again itself. Its output
/// is drained meanwhile, so the child never dies on a closed pipe before
/// the resize is sent.
async fn give_back(
    child: &mut Child,
    stdin: Option<ChildStdin>,
    reader: &mut BufReader<ChildStdout>,
    size: (u16, u16),
) {
    let (cols, rows) = size;
    if let Some(mut stdin) = stdin {
        let lines = resize_line(cols, rows) + "{\"type\":\"terminal.release\"}\n";
        let _ = tokio::time::timeout(GIVE_BACK, stdin.write_all(lines.as_bytes())).await;
    }
    let left = tokio::time::timeout(GIVE_BACK, async {
        let _ = tokio::io::copy(reader, &mut tokio::io::sink()).await;
        child.wait().await
    })
    .await;
    if left.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// A child's output: its stdout to read, and the start of its stderr.
fn outputs(child: &mut Child) -> Option<(BufReader<ChildStdout>, JoinHandle<String>)> {
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return None;
    };
    // A failing child explains itself on stderr; only its start is kept.
    let errors = tokio::spawn(async move {
        let mut kept = String::new();
        let _ = (&mut stderr)
            .take(STDERR_KEEP)
            .read_to_string(&mut kept)
            .await;
        let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        kept
    });
    Some((BufReader::new(stdout), errors))
}

/// Passes one child's frames on until it, the browser or the pane ends. A
/// Fit's `control` also takes the board's new sizes to herdr.
#[allow(clippy::too_many_arguments)]
async fn observe(
    herdr: &Cli,
    pane: &str,
    size: &mut (u16, u16),
    tty: Option<&Tty>,
    mut control: Option<&mut Control<'_>>,
    reader: &mut BufReader<ChildStdout>,
    errors: JoinHandle<String>,
    stop: &mut watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) -> Ended {
    let name = if control.is_some() {
        "herdr control"
    } else {
        "herdr observe"
    };
    let mut line = Vec::new();
    // Whether any frame has gone to the browser yet, and whether the next
    // one must be a full screen, as at the start and after a refit.
    let mut framed = false;
    let mut fresh = true;
    let mut passed = 0;
    let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + SIZE_POLL, SIZE_POLL);
    let sizer = herdr.clone().timeout(SIZE_TIMEOUT);
    loop {
        // The arms only say what happened; awaiting inside one would hold
        // the watch guard across the await.
        let woke = tokio::select! {
            read = next_line(reader, &mut line) => Woke::Read(read),
            _ = poll.tick() => Woke::Tick,
            fit = refitted(&mut control) => Woke::Refit(fit),
            () = tx.closed() => Woke::Over,
            () = stopping(stop) => Woke::Over,
        };
        match woke {
            Woke::Read(Ok(true)) => {}
            Woke::Read(Ok(false)) => {
                let said = tokio::time::timeout(Duration::from_secs(1), errors).await;
                let said = said.ok().and_then(Result::ok).unwrap_or_default();
                let said = said.trim();
                let said = if said.is_empty() {
                    format!("{name} ended")
                } else {
                    said.to_owned()
                };
                // A control that ends before its first frame never had the
                // pane: herdr refused it.
                if !framed && control.is_some() {
                    return Ended::Refused(said);
                }
                unavailable(tx, &said);
                return Ended::Done;
            }
            Woke::Read(Err(error)) => {
                unavailable(tx, &format!("{name}: {error}"));
                return Ended::Done;
            }
            Woke::Refit((cols, rows)) => {
                let Some(control) = control.as_mut() else {
                    continue;
                };
                let line = resize_line(cols, rows);
                let sent =
                    tokio::time::timeout(GIVE_BACK, control.stdin.write_all(line.as_bytes())).await;
                if !matches!(sent, Ok(Ok(()))) {
                    unavailable(tx, "herdr control stopped taking sizes");
                    return Ended::Done;
                }
                // herdr answers a resize with a full screen at the new size.
                if !queue(tx, &TerminalLine::Size { cols, rows }) {
                    return Ended::Done;
                }
                fresh = true;
                passed = 0;
                continue;
            }
            Woke::Tick => {
                let (sizer, asked, read) = (sizer.clone(), pane.to_owned(), tty.cloned());
                let read =
                    tokio::task::spawn_blocking(move || sizes(&sizer, &asked, read.as_ref()));
                match read.await {
                    Ok(Ok(Some((own, layout)))) => match control.as_mut() {
                        None if own != *size => return Ended::Resized(own),
                        None => {}
                        // A Fit holds the PTY at the board's size, so only
                        // the layout can say the pane moved; the Fit gives
                        // the pane back at its new layout's size.
                        Some(control) if control.layout != Some(layout) => {
                            if control.layout.is_some() {
                                *size = layout;
                                control.fits.hold(pane, layout).await;
                            }
                            control.layout = Some(layout);
                        }
                        Some(_) => {}
                    },
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
                // Diffs mean nothing without the full frame before them; an
                // observe that starts on one is asked again, and a Fit waits
                // a while for its full frame rather than resize the pane
                // twice.
                if fresh && !observed.full {
                    if control.is_none() {
                        return Ended::NoFullFrame;
                    }
                    passed += 1;
                    if passed > FIT_DIFFS {
                        unavailable(tx, "herdr control never sent a full frame");
                        return Ended::Done;
                    }
                    continue;
                }
                framed = true;
                fresh = false;
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

/// A pane's own size and its layout's, or `None` when herdr no longer has
/// the pane. Its own size is its PTY's, read from `tty` when there is one,
/// since herdr can lay a pane out smaller than its PTY (see [`Tty`]).
fn sizes(
    herdr: &Cli,
    pane: &str,
    tty: Option<&Tty>,
) -> tiller_herdr::client::Result<Option<(Cells, Cells)>> {
    let layout = herdr.pane_size(pane)?;
    Ok(layout.map(|layout| (tty.and_then(Tty::size).unwrap_or(layout), layout)))
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

    /// Two writers of one sizes file (two daemons on one home, or a
    /// restart racing the old one) each keep a whole file: no write fails
    /// and a reader never sees half of one.
    #[test]
    fn two_writers_never_share_a_temporary_file() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("mirror-fits.json");
        let writers: Vec<_> = (0..2_u16)
            .map(|writer| {
                let fits = Fits::at(path.clone());
                std::thread::spawn(move || {
                    let mut failed = Vec::new();
                    for round in 0..300_u16 {
                        let held = BTreeMap::from([(format!("pane-{writer}"), (round, writer))]);
                        if let Err(error) = fits.write(&held) {
                            failed.push(error.to_string());
                        }
                    }
                    failed
                })
            })
            .collect();
        while writers.iter().any(|writer| !writer.is_finished()) {
            if let Ok(bytes) = std::fs::read(&path) {
                let parsed: Result<BTreeMap<String, (u16, u16)>, _> =
                    serde_json::from_slice(&bytes);
                assert!(
                    parsed.is_ok(),
                    "a torn file: {:?}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
        for writer in writers {
            let failed = writer.join().unwrap();
            assert!(failed.is_empty(), "{failed:?}");
        }
        let left: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["mirror-fits.json"]);
    }

    #[test]
    fn the_input_budget_refills_at_its_rate() {
        let home = tempfile::tempdir().unwrap();
        let mirrors = Mirrors::new(home.path());
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
        let home = tempfile::tempdir().unwrap();
        let mirrors = Mirrors::new(home.path());
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
        assert!(check_keys(&keys(&["a", "--session", "other"])).is_err());
        assert!(check_keys(&keys(&["-h"])).is_err());
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
