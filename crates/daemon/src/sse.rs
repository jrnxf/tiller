//! The change stream (`protocol.md` 7.5): every `events` row, in `seq`
//! order, as one SSE message, resumable from `Last-Event-ID`.
//!
//! The daemon learns about rows written by any process (the CLI, hooks, its
//! own workers) by polling `PRAGMA data_version` every 200 ms on a
//! connection of its own ([`watch_changes`]). Each stream reads the rows
//! after its cursor straight from `state.db`, so a restarted daemon replays
//! exactly what a browser missed.
//!
//! Paired peers' changes and link states come from their link tasks
//! ([`Peers::subscribe`]) and go out with no `id`: a peer's `seq` is not
//! this host's. Each connect sends every peer's cached rows as
//! `peer_reset` after the replay, so a browser's gap never needs them.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde_json::json;
use tiller_core::{Store, Timestamp};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, watch};

use crate::peer::link::{PeerMessage, Peers};
use crate::views::{self, LocalHealth};

/// How often the feed asks SQLite whether another connection committed.
pub const POLL: Duration = Duration::from_millis(200);
/// A resume further behind than this gets `reset` instead of a replay.
pub const MAX_REPLAY: i64 = 5000;
/// The keepalive comment interval.
pub const KEEPALIVE: Duration = Duration::from_secs(15);
/// The reconnect delay the browser is told to use.
pub const RETRY: Duration = Duration::from_secs(1);
/// Rows read per batch.
const BATCH: i64 = 200;

/// Watches `state.db` and publishes the newest `seq` whenever another
/// connection commits. Runs until `stop` turns true. A missing database is
/// retried each second, so the feed starts once `tiller init` has run.
///
/// A commit that leaves the schema newer than this binary (a newer CLI
/// migrated) calls `newer` with that schema and ends the watch: an older
/// daemon must stop writing (`recovery.md` R11).
pub fn watch_changes(
    db: PathBuf,
    newest: watch::Sender<i64>,
    stop: watch::Receiver<bool>,
    newer: impl Fn(usize),
) {
    let mut store: Option<Store> = None;
    let mut version: Option<i64> = None;
    while !*stop.borrow() {
        if store.is_none() && db.exists() {
            store = Store::open_current(&db).ok();
        }
        let Some(open) = &store else {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        };
        let now: Option<i64> = open
            .conn()
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .ok();
        if now.is_none() {
            store = None;
            continue;
        }
        if now != version {
            version = now;
            let schema: Option<usize> = open
                .conn()
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .ok()
                .and_then(|schema| usize::try_from(schema).ok());
            if let Some(schema) = schema.filter(|schema| *schema > tiller_core::migrate::LATEST) {
                newer(schema);
                return;
            }
            if let Ok(seq) = open.last_seq() {
                newest.send_if_modified(|current| {
                    let changed = *current != seq;
                    *current = seq;
                    changed
                });
            }
        }
        std::thread::sleep(POLL);
    }
}

/// Where a stream starts: after `Last-Event-ID` when the browser resumes,
/// else after `since` (the boot snapshot's `last_event_seq`), else live.
pub fn start(last_event_id: Option<&str>, since: Option<i64>) -> Option<i64> {
    last_event_id
        .and_then(|id| id.trim().parse().ok())
        .or(since)
}

/// What a stream sends before its first change.
#[derive(Debug, PartialEq, Eq)]
pub enum Opening {
    /// Replay every row after this seq.
    Replay(i64),
    /// The gap is too wide, or the id is newer than the database (a
    /// restored database): refetch the snapshot, then stream from `newest`.
    Reset { newest: i64 },
}

pub fn opening(after: Option<i64>, newest: i64) -> Opening {
    match after {
        None => Opening::Replay(newest),
        Some(seq) if seq > newest || newest - seq > MAX_REPLAY => Opening::Reset { newest },
        Some(seq) => Opening::Replay(seq),
    }
}

fn reset(newest: i64, now: Timestamp) -> Event {
    Event::default()
        .event("reset")
        .id(newest.to_string())
        .data(json!({"seq": newest, "server_time": now.to_string()}).to_string())
}

/// The rows after `cursor` as `(seq, change message)`, up to one batch.
fn read_batch(
    store: &Store,
    local: &LocalHealth,
    cursor: i64,
) -> tiller_core::Result<Vec<(i64, String)>> {
    let now = Timestamp::now();
    let mut batch = Vec::new();
    for event in store.events_since(cursor, BATCH)? {
        let change = views::change(store, local, &event, now);
        batch.push((event.seq, serde_json::to_string(&change)?));
    }
    Ok(batch)
}

/// A link's message as SSE: `peer`, `link` or `peer_reset`, with no `id`.
fn peer_event(peers: &Peers, message: PeerMessage) -> Option<Event> {
    let (name, data) = match message {
        PeerMessage::Change(change) => ("peer", serde_json::to_string(&change)),
        PeerMessage::Link(link) => ("link", serde_json::to_string(&link)),
        PeerMessage::Reset(host_id) => (
            "peer_reset",
            serde_json::to_string(&peers.reset_event(&host_id)?),
        ),
    };
    Some(Event::default().event(name).data(data.ok()?))
}

/// A `peer_reset` for every peer.
fn peer_resets(peers: &Peers) -> Vec<Event> {
    peers
        .federation()
        .peers
        .iter()
        .filter_map(|view| peer_event(peers, PeerMessage::Reset(view.host_id.clone())))
        .collect()
}

/// One browser's stream. `local` is read per batch, for host rows.
pub fn stream(
    db: PathBuf,
    after: Option<i64>,
    mut newest: watch::Receiver<i64>,
    mut stop: watch::Receiver<bool>,
    peers: Arc<Peers>,
    local: impl Fn() -> LocalHealth + Send + Sync + 'static,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, mut rx) = mpsc::channel::<Event>(64);
    // Subscribed before the replay: what a link sends meanwhile waits here
    // and follows the `peer_reset`s.
    let mut from_peers = peers.subscribe();
    tokio::spawn(async move {
        if tx
            .send(Event::default().retry(RETRY).comment("tiller"))
            .await
            .is_err()
        {
            return;
        }
        let opened = tokio::task::spawn_blocking(move || {
            let store = Store::open_current(&db)?;
            let newest = store.last_seq()?;
            Ok::<_, tiller_core::Error>((store, newest))
        })
        .await;
        let Ok(Ok((mut store, at_open))) = opened else {
            return;
        };
        let mut cursor = match opening(after, at_open) {
            Opening::Replay(seq) => seq,
            Opening::Reset { newest } => {
                if tx.send(reset(newest, Timestamp::now())).await.is_err() {
                    return;
                }
                newest
            }
        };
        let mut replaying = true;
        loop {
            let local_now = local();
            let read = tokio::task::spawn_blocking(move || {
                let batch = read_batch(&store, &local_now, cursor);
                let last = store.last_seq();
                (store, batch, last)
            })
            .await;
            let Ok((back, batch, last)) = read else {
                return;
            };
            store = back;
            let batch = match batch {
                Ok(batch) => batch,
                Err(error) => {
                    tracing::warn!(%error, "change stream read failed");
                    tokio::select! {
                        () = tokio::time::sleep(RETRY) => continue,
                        () = tx.closed() => return,
                        _ = stop.wait_for(|stop| *stop) => return,
                    }
                }
            };
            // The database went backwards under us (a restore): the client
            // must refetch, and the stream goes on from the new newest row.
            if let Ok(last) = last
                && last < cursor
            {
                if tx.send(reset(last, Timestamp::now())).await.is_err() {
                    return;
                }
                cursor = last;
                continue;
            }
            let full = i64::try_from(batch.len()).unwrap_or(i64::MAX) >= BATCH;
            for (seq, data) in batch {
                let message = Event::default()
                    .id(seq.to_string())
                    .event("change")
                    .data(data);
                if tx.send(message).await.is_err() {
                    return;
                }
                cursor = seq;
            }
            if full {
                continue;
            }
            if replaying {
                replaying = false;
                for event in peer_resets(&peers) {
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
            // Wait for a commit, passing on what the links send meanwhile;
            // a dropped feed ends the stream, and the browser reconnects
            // with its `Last-Event-ID`.
            loop {
                let events = tokio::select! {
                    changed = newest.changed() => if changed.is_err() { return; } else { break },
                    message = from_peers.recv() => match message {
                        Ok(message) => peer_event(&peers, message).into_iter().collect(),
                        // Too far behind the links: send their rows whole.
                        Err(RecvError::Lagged(_)) => peer_resets(&peers),
                        Err(RecvError::Closed) => return,
                    },
                    () = tx.closed() => return,
                    // A stopping daemon ends every stream so the server can
                    // drain; the browser resumes from `Last-Event-ID`.
                    _ = stop.wait_for(|stop| *stop) => return,
                };
                for event in events {
                    if tx.send(event).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    let events = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx).map(|e| e.map(Ok)));
    Sse::new(events).keep_alive(KeepAlive::new().interval(KEEPALIVE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use tiller_core::domain::CardKind;
    use tiller_core::state::cards::NewCard;
    use tiller_herdr::client::Cli;
    use tiller_herdr::poller::Registry;
    use tiller_herdr::testing::{Fleet, owner};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use crate::api::{AppState, Inner, router};
    use crate::assets::Assets;

    /// One daemon's HTTP side and change feed on `listener`, until `stop`.
    fn serve(home: &std::path::Path, listener: TcpListener) -> watch::Sender<bool> {
        let port = listener.local_addr().unwrap().port();
        let (stop_tx, stop) = watch::channel(false);
        let (newest_tx, newest) = watch::channel(0);
        {
            let (db, stop) = (home.join("state.db"), stop.clone());
            std::thread::spawn(move || watch_changes(db, newest_tx, stop, |_| {}));
        }
        let app = router(AppState::new(Inner {
            home: home.to_owned(),
            port,
            allowed_hosts: Vec::new(),
            assets: Assets::board(),
            newest,
            stop: stop.clone(),
            pollers: Registry::new(),
            peers: crate::peer::link::Peers::idle(),
            herdr: Cli::local(),
            terminal: None,
            started_at: Timestamp::now(),
            tools: Arc::new(Mutex::new(serde_json::Value::Null)),
            damage: crate::readonly::Damage::default(),
            orchestrators: None,
            mirrors: crate::mirror::Mirrors::default(),
        }));
        let mut stopping = stop.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stopping.wait_for(|stop| *stop).await;
                })
                .await;
        });
        stop_tx
    }

    /// A raw SSE client: HTTP/1.0, so the body is the bare stream.
    async fn connect(addr: SocketAddr, headers: &str, query: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET /api/v1/events{query} HTTP/1.0\r\nHost: 127.0.0.1:{}\r\nAccept: text/event-stream\r\n{headers}\r\n",
            addr.port()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        stream
    }

    /// The ids of the `change` messages read until `done` holds, or `None`
    /// when the stream ends first.
    async fn read_ids(stream: &mut TcpStream, done: impl Fn(&[i64]) -> bool) -> Option<Vec<i64>> {
        let mut text = String::new();
        let mut buf = [0_u8; 8192];
        loop {
            let ids: Vec<i64> = text
                .split("\n\n")
                .filter(|message| message.lines().any(|line| line == "event: change"))
                .filter_map(|message| message.lines().find_map(|line| line.strip_prefix("id: ")))
                .map(|id| id.parse().unwrap())
                .collect();
            if done(&ids) {
                return Some(ids);
            }
            let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
                .await
                .expect("the stream stalled");
            match read.unwrap() {
                0 => return None,
                n => text.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    }

    fn add_card(fleet: &Fleet, title: &str) -> i64 {
        let mut store = fleet.open();
        store
            .board_add(
                &owner(),
                None,
                NewCard {
                    project_id: "alpha".into(),
                    kind: CardKind::Ship,
                    title: title.into(),
                    ..NewCard::default()
                },
            )
            .unwrap();
        store.last_seq().unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resume_after_restart_replays_from_last_event_id() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let booted = fleet.open().last_seq().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let first = serve(home, listener);
        // The first connect resumes from the boot snapshot's seq.
        let mut browser = connect(addr, "", &format!("?since={booted}")).await;
        let seen_live = add_card(&fleet, "One");
        let before = read_ids(&mut browser, |ids| ids.last() == Some(&seen_live))
            .await
            .unwrap();
        assert_eq!(before, ((booted + 1)..=seen_live).collect::<Vec<_>>());

        // The daemon stops: the stream ends, and rows land while it is down.
        first.send(true).unwrap();
        assert_eq!(read_ids(&mut browser, |_| false).await, None);
        add_card(&fleet, "Two");
        let while_down = add_card(&fleet, "Three");

        let listener = TcpListener::bind(addr).await.unwrap();
        let _second = serve(home, listener);
        // The browser resumes with its Last-Event-ID; the header wins over a
        // stale `since`.
        let mut browser = connect(
            addr,
            &format!("Last-Event-ID: {seen_live}\r\n"),
            &format!("?since={booted}"),
        )
        .await;
        let replayed = read_ids(&mut browser, |ids| ids.last() == Some(&while_down))
            .await
            .unwrap();
        assert_eq!(replayed, ((seen_live + 1)..=while_down).collect::<Vec<_>>());

        // And it stays live after the replay.
        let after = add_card(&fleet, "Four");
        let live = read_ids(&mut browser, |ids| ids.last() == Some(&after))
            .await
            .unwrap();
        assert_eq!(live, ((while_down + 1)..=after).collect::<Vec<_>>());
    }

    #[test]
    fn the_header_wins_over_since() {
        assert_eq!(start(Some("41"), Some(7)), Some(41));
        assert_eq!(start(None, Some(7)), Some(7));
        assert_eq!(start(Some("junk"), None), None);
    }

    #[test]
    fn a_wide_gap_or_a_newer_id_resets() {
        assert_eq!(opening(None, 90), Opening::Replay(90));
        assert_eq!(opening(Some(80), 90), Opening::Replay(80));
        assert_eq!(opening(Some(91), 90), Opening::Reset { newest: 90 });
        assert_eq!(opening(Some(10), 10 + MAX_REPLAY), Opening::Replay(10));
        assert_eq!(
            opening(Some(9), 10 + MAX_REPLAY),
            Opening::Reset {
                newest: 10 + MAX_REPLAY
            }
        );
    }
}
