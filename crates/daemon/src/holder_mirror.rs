//! The terminal mirror for a pane in the terminal holder
//! (`terminal-holder.md` 5.1).
//!
//! The view subscribes to the pane: a `size` line and a full frame with the
//! holder's repaint, then each burst of the PTY's writes as a partial
//! frame. Each new PTY size after the repaint is a `size` line, from the
//! holder's `resized` events on a watch of the view's own: the view's own
//! Fit and another client's lease alike. Without a watch, only the answers
//! to the view's own Fit give it a size. The view never subscribes again
//! for a size, since its lease would end with the subscription. The
//! child's exit is `closed`, with how it ended, and a holder that stops
//! answering is `unavailable`, after which the board opens the view again.
//!
//! A view asked for a Fit holds a size lease on its own subscription
//! (`terminal-holder.md` 2.6) at the board's size, and moves it as the
//! board refits. Every viewer's Fit is a lease of its own, and the top one
//! sets the size. The lease ends with the subscription: when the view
//! ends, and when the daemon dies, since the subscription is a connection
//! of its own. The size then falls back to the next lease, so nothing is
//! kept on disk to give back.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::{Body, Bytes};
use base64::Engine as _;
use tiller_panes::message::Event;
use tiller_panes::{EventsEnd, PaneId, PanesError, StreamHandle, Subscribe, Update};
use tokio::sync::{mpsc, watch};

use crate::dto::TerminalLine;
use crate::holder::{Holder, exit_reason};
use crate::mirror::{Viewer, queue, stopping, unavailable};

/// Updates a subscription's thread can hand on before it waits for the
/// view to take them.
const UPDATES: usize = 64;

/// The most output bytes one frame gathers from a burst, so a fast writer
/// cannot make one frame without end or keep the view from its stop.
const BURST: usize = 64 * 1024;

/// One view's Fit: its id, and the size its lease asks for.
type Fit = (u64, watch::Sender<(u16, u16)>);

/// The board's Fits on holder panes, one per view that asked for one. Each
/// is the size its view's lease asks for; a refit moves the newest Fit on
/// the pane, since a refit does not say which view it is for.
#[derive(Debug, Default)]
pub struct Leases {
    panes: Mutex<HashMap<PaneId, Vec<Fit>>>,
    next: AtomicU64,
}

impl Leases {
    /// A new Fit on `pane` at `fit`, until the returned hold drops.
    fn hold(self: &Arc<Self>, pane: &PaneId, fit: (u16, u16)) -> Held {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, board) = watch::channel(fit);
        self.panes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(pane.clone())
            .or_default()
            .push((id, sender));
        Held {
            leases: Arc::clone(self),
            pane: pane.clone(),
            id,
            board,
        }
    }

    /// Gives the newest Fit on `pane` the board's new size; false when no
    /// view holds a Fit on it.
    pub fn refit(&self, pane: &PaneId, fit: (u16, u16)) -> bool {
        let panes = self.panes.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((_, sender)) = panes.get(pane).and_then(|held| held.last()) else {
            return false;
        };
        sender.send_if_modified(|now| std::mem::replace(now, fit) != fit);
        true
    }
}

/// One view's Fit, listed in [`Leases`] until it drops.
struct Held {
    leases: Arc<Leases>,
    pane: PaneId,
    id: u64,
    /// The board's size, as it refits.
    board: watch::Receiver<(u16, u16)>,
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut panes = self
            .leases
            .panes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(held) = panes.get_mut(&self.pane) {
            held.retain(|(id, _)| *id != self.id);
            if held.is_empty() {
                panes.remove(&self.pane);
            }
        }
    }
}

/// A subscription and a watch, each read on a thread of its own, since
/// their `recv` blocks.
struct Pumped {
    updates: mpsc::Receiver<Result<Option<Update>, PanesError>>,
    /// The pane's size from the watch's `resized` events.
    sizes: watch::Receiver<Option<(u16, u16)>>,
    handle: StreamHandle,
    events: Option<EventsEnd>,
}

impl Pumped {
    /// Watches the holder's events, then subscribes to `pane`, so no
    /// resize between the repaint and the watch goes unseen. A watch the
    /// holder refuses leaves the view without other clients' sizes, which
    /// it logs.
    fn open(holder: &dyn Holder, pane: &PaneId) -> Result<Self, String> {
        let (sized, sizes) = watch::channel(None);
        let events = match holder.watch() {
            Ok(mut events) => {
                let ender = events.ender();
                let ours = pane.clone();
                std::thread::Builder::new()
                    .name("tiller-holder-watch".into())
                    .spawn(move || {
                        while let Ok(Some(event)) = events.recv() {
                            if let Event::Resized { pane, cols, rows } = event
                                && pane == ours
                                && sized.send(Some((cols, rows))).is_err()
                            {
                                return;
                            }
                        }
                    })
                    .map_err(|error| error.to_string())?;
                Some(ender)
            }
            Err(error) => {
                tracing::warn!(pane = %pane, %error, "the terminal view cannot watch the holder; another client's resize is not shown");
                None
            }
        };
        let mut subscription = match holder.subscribe(pane, Subscribe::default()) {
            Ok(subscription) => subscription,
            Err(error) => {
                if let Some(events) = &events {
                    events.end();
                }
                return Err(error.to_string());
            }
        };
        let handle = subscription.handle();
        let (tx, updates) = mpsc::channel(UPDATES);
        let pumped = Self {
            updates,
            sizes,
            handle,
            events,
        };
        std::thread::Builder::new()
            .name("tiller-holder-view".into())
            .spawn(move || {
                loop {
                    let update = subscription.recv();
                    // A refused resize leaves the stream open.
                    let last = match &update {
                        Ok(Some(_)) | Err(PanesError::Refused { .. }) => false,
                        Ok(None) | Err(_) => true,
                    };
                    if tx.blocking_send(update).is_err() || last {
                        return;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(pumped)
    }
}

impl Drop for Pumped {
    /// Ends the subscription, and with it the view's lease, and the watch.
    fn drop(&mut self) {
        self.handle.end();
        if let Some(events) = &self.events {
            events.end();
        }
    }
}

/// Opens the view off the async threads.
async fn open(holder: &Arc<dyn Holder>, pane: &PaneId) -> Result<Pumped, String> {
    let holder = Arc::clone(holder);
    let pane = pane.clone();
    tokio::task::spawn_blocking(move || Pumped::open(holder.as_ref(), &pane))
        .await
        .map_err(|error| error.to_string())?
}

/// Asks the holder for `size` under the view's lease, off the async
/// threads. The holder's answer comes through the subscription.
async fn fit_to(handle: &StreamHandle, pane: &PaneId, size: (u16, u16)) {
    let handle = handle.clone();
    let asked = tokio::task::spawn_blocking(move || handle.resize(size.0, size.1)).await;
    match asked {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::info!(pane = %pane, %error, "could not ask the terminal holder for the Fit's size");
        }
        Err(error) => {
            tracing::info!(pane = %pane, %error, "could not ask the terminal holder for the Fit's size");
        }
    }
}

/// The NDJSON body that streams the holder's `pane` to one viewer, with a
/// lease at the board's size `fit` while the view is open.
pub fn stream(
    holder: Arc<dyn Holder>,
    pane: PaneId,
    fit: Option<(u16, u16)>,
    leases: Arc<Leases>,
    stop: watch::Receiver<bool>,
    viewer: Viewer,
) -> Body {
    let (tx, mut rx) = mpsc::channel::<Bytes>(crate::mirror::QUEUE);
    tokio::spawn(async move {
        let _viewer = viewer;
        let held = fit.map(|size| leases.hold(&pane, size));
        view(&holder, &pane, held, stop, &tx).await;
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

/// The board's next size for the Fit; never for a view without one.
async fn refitted(held: &mut Option<Held>) -> (u16, u16) {
    if let Some(held) = held
        && held.board.changed().await.is_ok()
    {
        return *held.board.borrow_and_update();
    }
    std::future::pending().await
}

/// The next size a `resized` event gave; never once the watch is gone.
async fn watched(sizes: &mut watch::Receiver<Option<(u16, u16)>>) -> (u16, u16) {
    loop {
        if sizes.changed().await.is_err() {
            return std::future::pending().await;
        }
        if let Some(size) = *sizes.borrow_and_update() {
            return size;
        }
    }
}

/// What woke a view.
enum Woke {
    Update(Option<Result<Option<Update>, PanesError>>),
    /// The board asked the Fit for a new size.
    Refit((u16, u16)),
    /// Another client's lease set the PTY's size.
    Watched((u16, u16)),
}

/// Streams `pane` to `tx` until the browser leaves, the child exits, the
/// holder stops answering or the daemon stops. The Fit's lease ends with
/// the view.
async fn view(
    holder: &Arc<dyn Holder>,
    pane: &PaneId,
    mut held: Option<Held>,
    mut stop: watch::Receiver<bool>,
    tx: &mpsc::Sender<Bytes>,
) {
    let mut view = match open(holder, pane).await {
        Ok(view) => view,
        Err(error) => return unavailable(tx, &error),
    };
    // The repaint comes first, with the pane's size.
    let first = tokio::select! {
        update = view.updates.recv() => update,
        () = stopping(&mut stop) => return,
        () = tx.closed() => return,
    };
    let Some(first) = first else { return };
    // With a watch, every new size comes from its `resized` events, the
    // Fit's own too, in the holder's order. Without one, the answers to
    // the Fit's resizes are all the view hears of a size.
    let watching = view.events.is_some();
    if let Some(fit) = held.as_mut() {
        let size = *fit.board.borrow_and_update();
        fit_to(&view.handle, pane, size).await;
    }
    let mut seq = 0;
    // The size the board was last told, so each size goes once.
    let mut shown: Option<(u16, u16)> = None;
    // An update read while gathering a burst, for the next turn.
    let mut later = Some(first);
    loop {
        let update = match later.take() {
            Some(update) => Some(update),
            None => match tokio::select! {
                update = view.updates.recv() => Woke::Update(update),
                size = refitted(&mut held) => Woke::Refit(size),
                size = watched(&mut view.sizes) => Woke::Watched(size),
                () = stopping(&mut stop) => return,
                () = tx.closed() => return,
            } {
                Woke::Update(update) => update,
                Woke::Refit(size) => {
                    fit_to(&view.handle, pane, size).await;
                    continue;
                }
                Woke::Watched(size) => {
                    if !resize(tx, &mut stop, &mut shown, size).await {
                        return;
                    }
                    continue;
                }
            },
        };
        // A burst of output already waiting goes as one frame, so a fast
        // writer costs the browser fewer lines.
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
        let line = match update {
            Some(Ok(Some(Update::Repaint { cols, rows, bytes }))) => {
                if !resize(tx, &mut stop, &mut shown, (cols, rows)).await {
                    return;
                }
                frame(&mut seq, true, &bytes)
            }
            Some(Ok(Some(Update::Output(bytes)))) => frame(&mut seq, false, &bytes),
            // The holder's answer to the Fit: the size the top lease set.
            Some(Ok(Some(Update::Resized { cols, rows }))) => {
                if !watching && !resize(tx, &mut stop, &mut shown, (cols, rows)).await {
                    return;
                }
                continue;
            }
            Some(Ok(Some(Update::Exited(exit)))) => TerminalLine::Closed {
                reason: Some(exit_reason(Some(&exit))),
            },
            Some(Ok(Some(Update::Lagged))) => {
                return unavailable(tx, "the view fell behind the terminal holder");
            }
            // A Fit the holder refused (its lease limit, or a pane that is
            // exiting): the view goes on at the pane's size.
            Some(Err(error @ PanesError::Refused { .. })) => {
                tracing::info!(pane = %pane, %error, "the terminal holder refused the Fit's size");
                continue;
            }
            Some(Err(error)) => return unavailable(tx, &error.to_string()),
            // The holder ended the stream without an exit: the board opens
            // it again.
            Some(Ok(None)) | None => return,
        };
        let closed = matches!(line, TerminalLine::Closed { .. });
        if !deliver(tx, &mut stop, &line).await || closed {
            return;
        }
    }
}

/// Tells the board the PTY is `size` now, once per size. False when the
/// browser left or the daemon stops.
async fn resize(
    tx: &mpsc::Sender<Bytes>,
    stop: &mut watch::Receiver<bool>,
    shown: &mut Option<(u16, u16)>,
    size: (u16, u16),
) -> bool {
    if *shown == Some(size) {
        return true;
    }
    *shown = Some(size);
    let (cols, rows) = size;
    deliver(tx, stop, &TerminalLine::Size { cols, rows }).await
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
