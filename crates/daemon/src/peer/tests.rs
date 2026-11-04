//! The federated board end to end (`protocol.md` 10.2): two homes, a board
//! host and its peer, linked in memory instead of over SSH, on shrunk
//! timings.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use tiller_core::domain::{CardKind, PeerScope};
use tiller_core::state::cards::NewCard;
use tiller_core::state::peers::{
    KeyFile, NewPairing, NewPeerKey, generate_key, key_path, public_key_text, write_key_file,
};
use tiller_core::{Store, Timestamp};
use tiller_herdr::testing::{Fleet, owner};
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use super::link::{Connecting, Connection, Connector, PeerMessage, Peers, Target};
use super::serve::{ServeConfig, own_snapshot, serve};
use super::{Frame, Hello, HelloKey, Timing, Welcome};
use crate::api::tests::{app_with_peers, get};
use crate::dto::{BoardResponse, ErrorBody, LinkState};
use crate::readonly::Damage;

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// Every interval shrunk, in the same order as the real ones.
fn quick() -> Timing {
    Timing {
        live: ms(400),
        down: ms(1200),
        beat: ms(100),
        poll: ms(20),
        refresh: ms(100),
        query: ms(1000),
        skew: Duration::from_secs(120),
        backoff: [ms(50); 4],
        refused_retry: ms(200),
        tick: ms(25),
        supervise: ms(100),
    }
}

/// A board host and the peer it paired as `peer`.
struct Pair {
    board: Fleet,
    peer: Fleet,
    key: SigningKey,
    /// The peer's `peer_keys.id`.
    key_id: String,
}

fn pair() -> Pair {
    let board = Fleet::new();
    let peer = Fleet::new();
    let (key, key_id) = pair_onto(&board, &peer, "peer");
    Pair {
        board,
        peer,
        key,
        key_id,
    }
}

/// Pairs `peer` onto `board` under `label`, as `tiller peer add` and
/// `tiller peer accept` leave both homes. Returns the board host's key and
/// its `key_id`.
fn pair_onto(board: &Fleet, peer: &Fleet, label: &str) -> (SigningKey, String) {
    let key = generate_key().unwrap();
    let accepted = peer
        .open()
        .peer_accept(
            &owner(),
            true,
            NewPeerKey {
                viewer_label: "test-host".into(),
                viewer_host_id: board.host.clone(),
                public_key: public_key_text(&key),
                scope: PeerScope::Act,
            },
        )
        .unwrap()
        .value;
    board
        .open()
        .peer_pair(
            &owner(),
            NewPairing {
                label: label.into(),
                herdr_machine: None,
                scope: PeerScope::Act,
                peer_host_id: peer.host.clone(),
            },
        )
        .unwrap();
    let file = KeyFile::new(label, PeerScope::Act, &key).accepted(&peer.host, &accepted.id);
    write_key_file(&key_path(board.dir.path(), label).unwrap(), &file).unwrap();
    (key, accepted.id)
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

fn hello(pair: &Pair, last_seq: Option<i64>) -> Frame {
    Frame::Hello(Hello::signed(
        &HelloKey {
            key: &pair.key,
            from: "test-host",
            key_id: &pair.key_id,
            viewer_host_id: &pair.board.host,
        },
        &pair.peer.host,
        last_seq,
        Timestamp::now(),
    ))
}

/// `tiller peer serve` for `home` on one end of a socket pair; the other
/// end is returned.
fn serve_on_socket(home: &Path, from: &str) -> UnixStream {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let config = ServeConfig {
        home: home.to_owned(),
        from: from.to_owned(),
        timing: quick(),
        herdr: None,
    };
    let input = BufReader::new(theirs.try_clone().unwrap());
    std::thread::spawn(move || {
        let _ = serve(&config, input, theirs);
    });
    ours
}

/// A blocking link to `peer serve`: write frames, read frames.
struct SyncLink {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
}

impl SyncLink {
    fn open(home: &Path) -> Self {
        let socket = serve_on_socket(home, "test-host");
        socket.set_read_timeout(Some(ms(5000))).unwrap();
        Self {
            reader: BufReader::new(socket.try_clone().unwrap()),
            writer: socket,
        }
    }

    fn send(&mut self, frame: &Frame) {
        self.writer
            .write_all(frame.line().unwrap().as_bytes())
            .unwrap();
    }

    /// The next frame that is not a `beat`.
    fn next(&mut self) -> Frame {
        loop {
            let mut line = String::new();
            assert!(
                self.reader.read_line(&mut line).unwrap() > 0,
                "the link closed"
            );
            match serde_json::from_str(&line).unwrap() {
                Frame::Beat { .. } => continue,
                frame => return frame,
            }
        }
    }
}

/// Dials `peer serve` in process over a socket pair, as SSH would.
struct Socket {
    home: PathBuf,
    connects: Arc<AtomicUsize>,
}

impl Connector for Socket {
    fn connect(&self, target: &Target) -> Connecting {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let socket = serve_on_socket(&self.home, &target.from);
        Box::pin(async move {
            socket.set_nonblocking(true)?;
            let (reader, writer) = tokio::net::UnixStream::from_std(socket)?.into_split();
            Ok(Connection {
                reader: Box::new(tokio::io::BufReader::new(reader)),
                writer: Box::new(writer),
                child: None,
            })
        })
    }
}

/// A peer that sends `welcome` and one `snapshot`, then freezes: it never
/// sends another frame and never reads what it is sent.
struct Frozen {
    home: PathBuf,
    connects: Arc<AtomicUsize>,
    /// The peer's ends, held open so the link sees silence, not a hangup.
    held: Arc<Mutex<Vec<tokio::io::DuplexStream>>>,
}

impl Connector for Frozen {
    fn connect(&self, _target: &Target) -> Connecting {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let opening = (|| {
            let store =
                Store::open_current(self.home.join("state.db")).map_err(std::io::Error::other)?;
            let host = store.this_host().map_err(std::io::Error::other)?;
            let now = Timestamp::now();
            let welcome = Frame::Welcome(Welcome {
                host_id: host.id,
                label: host.label,
                schema_version: tiller_core::migrate::LATEST,
                tiller_version: "test".into(),
                server_time: now.to_string(),
            });
            let snapshot = own_snapshot(&store, now).map_err(std::io::Error::other)?;
            let snapshot = Frame::Snapshot(Box::new(snapshot));
            Ok::<_, std::io::Error>(welcome.line()? + &snapshot.line()?)
        })();
        let held = self.held.clone();
        Box::pin(async move {
            let (mut peer, ours) = tokio::io::duplex(1 << 20);
            peer.write_all(opening?.as_bytes()).await?;
            held.lock().unwrap().push(peer);
            Ok(Connection {
                reader: Box::new(tokio::io::BufReader::new(ours)),
                writer: Box::new(tokio::io::sink()),
                child: None,
            })
        })
    }
}

/// The board host's links, until the returned sender is dropped.
fn start(home: &Path, connector: impl Connector + 'static) -> (Arc<Peers>, watch::Sender<bool>) {
    let (stop_tx, stop) = watch::channel(false);
    let peers = Peers::start(
        home.to_owned(),
        Arc::new(connector),
        quick(),
        Damage::default(),
        stop,
    );
    (peers, stop_tx)
}

/// Polls `check` until it gives a value, for up to 10 s.
async fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(ms(10)).await;
    }
}

/// The next message on `rx` that `pick` takes, within 10 s.
async fn next_message<T>(
    rx: &mut broadcast::Receiver<PeerMessage>,
    mut pick: impl FnMut(PeerMessage) -> Option<T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = pick(rx.recv().await.unwrap()) {
                return value;
            }
        }
    })
    .await
    .expect("timed out waiting for a peer message")
}

fn titles(peers: &Peers) -> Vec<String> {
    peers
        .federation()
        .peers
        .iter()
        .filter_map(|view| view.queries.as_ref())
        .flat_map(|queries| queries.board.rows.iter().map(|row| row.title.clone()))
        .collect()
}

fn live_with_rows(peers: &Peers) -> Option<()> {
    let federation = peers.federation();
    let view = federation.peers.first()?;
    (view.link == LinkState::Live && view.queries.is_some()).then_some(())
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_then_events_fill_cache() {
    let pair = pair();
    add_card(&pair.peer, "made before the link");
    add_card(&pair.board, "a local card");
    let connects = Arc::new(AtomicUsize::new(0));
    let (peers, _stop) = start(
        pair.board.dir.path(),
        Socket {
            home: pair.peer.dir.path().to_owned(),
            connects: connects.clone(),
        },
    );
    let mut rx = peers.subscribe();
    until("the snapshot", || live_with_rows(&peers)).await;
    let held = titles(&peers);
    assert!(
        held.contains(&"made before the link".to_owned()),
        "{held:?}"
    );
    assert!(!held.contains(&"a local card".to_owned()), "{held:?}");

    // A change on the peer is one `event` frame: the copy takes it and
    // browsers get `event: peer` at once, before any refresh.
    let seq = add_card(&pair.peer, "made on the peer");
    let event = next_message(&mut rx, |message| match message {
        PeerMessage::Change(event)
            if event
                .change
                .card
                .as_ref()
                .is_some_and(|card| card.title == "made on the peer") =>
        {
            Some(event)
        }
        _ => None,
    })
    .await;
    assert_eq!(event.host, "peer");
    let card = event.change.card.as_ref().unwrap();
    assert_eq!(card.host.as_deref(), Some("peer"));
    assert!(event.as_of.is_some());
    assert!(titles(&peers).contains(&"made on the peer".to_owned()));
    until("every row of the change", || {
        (peers.federation().peers[0].last_event_seq >= Some(seq)).then_some(())
    })
    .await;

    // The board reads carry both hosts' rows, each tagged.
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let answer = get(&app, "/api/v1/board?filter=all").await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let board: BoardResponse = serde_json::from_str(&answer.body).unwrap();
    let host_of = |title: &str| {
        let row = board.rows.iter().find(|row| row.title == title).unwrap();
        (row.host.clone(), row.link)
    };
    assert_eq!(host_of("a local card"), (Some("test-host".into()), None));
    assert_eq!(
        host_of("made on the peer"),
        (Some("peer".into()), Some(LinkState::Live))
    );
    let answer = get(&app, "/api/v1/board?filter=all&host=peer").await;
    let theirs: BoardResponse = serde_json::from_str(&answer.body).unwrap();
    assert!(
        theirs
            .rows
            .iter()
            .all(|row| row.host.as_deref() == Some("peer"))
    );
    assert_eq!(theirs.rows.len(), titles(&peers).len());

    // A single-entity read with `?host=` is a `query` over the link.
    let id = &theirs.rows[0].id;
    let answer = get(&app, &format!("/api/v1/cards/{id}?host=peer")).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);

    // `?host=` naming this host reads the local card; a label that names
    // no host is 404, never the local card of the same id.
    let local = board
        .rows
        .iter()
        .find(|row| row.title == "a local card")
        .unwrap();
    let answer = get(&app, &format!("/api/v1/cards/{}?host=test-host", local.id)).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let answer = get(
        &app,
        &format!("/api/v1/cards/{}?host=no-such-host", local.id),
    )
    .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND, "{}", answer.body);
    assert_eq!(connects.load(Ordering::SeqCst), 1);
}

#[test]
fn resume_from_last_seq_replays_gap() {
    let pair = pair();
    let seen = add_card(&pair.peer, "seen before the gap");
    add_card(&pair.peer, "missed one");
    let newest = add_card(&pair.peer, "missed two");

    let mut link = SyncLink::open(pair.peer.dir.path());
    link.send(&hello(&pair, Some(seen)));
    assert!(matches!(link.next(), Frame::Welcome(welcome) if welcome.host_id == pair.peer.host));
    let mut replayed = Vec::new();
    while replayed.last() != Some(&newest) {
        match link.next() {
            Frame::Event(change) => replayed.push(change.seq),
            frame => panic!("expected the replay, got {frame:?}"),
        }
    }
    // Every row after `last_seq`, in order, and nothing else: no snapshot.
    assert_eq!(replayed, ((seen + 1)..=newest).collect::<Vec<_>>());
}

#[test]
fn gap_over_5000_sends_snapshot() {
    let pair = pair();
    let old = add_card(&pair.peer, "seen long ago");
    pair.peer
        .open()
        .conn()
        .execute(
            "UPDATE sqlite_sequence SET seq = seq + 6000 WHERE name = 'events'",
            [],
        )
        .unwrap();
    let newest = add_card(&pair.peer, "made after the gap");
    assert!(newest - old > 5000);

    // Too old a `last_seq`, one ahead of the peer's newest (a restored
    // database), and none at all each get a snapshot.
    for last_seq in [Some(old), Some(newest + 10), None] {
        let mut link = SyncLink::open(pair.peer.dir.path());
        link.send(&hello(&pair, last_seq));
        assert!(matches!(link.next(), Frame::Welcome(_)));
        match link.next() {
            Frame::Snapshot(snapshot) => {
                assert_eq!(snapshot.last_event_seq, newest, "after {last_seq:?}");
                let titles: Vec<&str> = snapshot
                    .queries
                    .board
                    .rows
                    .iter()
                    .map(|row| row.title.as_str())
                    .collect();
                assert!(titles.contains(&"made after the gap"), "{titles:?}");
            }
            frame => panic!("expected a snapshot after {last_seq:?}, got {frame:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn link_states_follow_frame_age() {
    let real = Timing::default();
    let s = Duration::from_secs;
    assert_eq!(real.state(s(0)), LinkState::Live);
    assert_eq!(real.state(s(24)), LinkState::Live);
    assert_eq!(real.state(s(25)), LinkState::Lagging);
    assert_eq!(real.state(s(119)), LinkState::Lagging);
    assert_eq!(real.state(s(120)), LinkState::Down);
    assert_eq!(
        (0..6)
            .map(|attempt| real.backoff(attempt))
            .collect::<Vec<_>>(),
        [s(5), s(15), s(30), s(60), s(60), s(60)]
    );

    // A peer that goes quiet walks the link from live through lagging to
    // down, and each step is an `event: link`.
    let pair = pair();
    let (peers, _stop) = start(
        pair.board.dir.path(),
        Frozen {
            home: pair.peer.dir.path().to_owned(),
            connects: Arc::default(),
            held: Arc::default(),
        },
    );
    let mut rx = peers.subscribe();
    let mut states = Vec::new();
    let begun = Instant::now();
    while states.last() != Some(&LinkState::Down) {
        let link = next_message(&mut rx, |message| match message {
            PeerMessage::Link(link) => Some(link),
            _ => None,
        })
        .await;
        assert_eq!(link.host, "peer");
        let reason = link.stale_reason.as_deref();
        match link.link {
            LinkState::Live => assert_eq!(reason, None),
            LinkState::Lagging | LinkState::Down => {
                assert_eq!(reason, Some("peer_link_down"));
                assert!(link.as_of.is_some());
            }
            other => panic!("unexpected link state {other:?}"),
        }
        states.push(link.link);
    }
    assert_eq!(
        states,
        [LinkState::Live, LinkState::Lagging, LinkState::Down]
    );
    let timing = quick();
    assert!(
        begun.elapsed() >= timing.down,
        "down after {:?}",
        begun.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn frozen_serve_reaches_down_and_reconnects() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    add_card(&pair.board, "a local card");
    let connects = Arc::new(AtomicUsize::new(0));
    let (peers, _stop) = start(
        pair.board.dir.path(),
        Frozen {
            home: pair.peer.dir.path().to_owned(),
            connects: connects.clone(),
            held: Arc::default(),
        },
    );
    let mut rx = peers.subscribe();
    until("the snapshot", || live_with_rows(&peers)).await;
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let peer_card = {
        let federation = peers.federation();
        let queries = federation.peers[0].queries.clone().unwrap();
        let row = queries
            .board
            .rows
            .iter()
            .find(|row| row.title == "a peer card");
        row.unwrap().id.clone()
    };

    // A query the frozen peer never answers waits `timing.query`, then
    // answers 503 with the cached row. Local reads and writes meanwhile
    // answer at once.
    let query = async {
        let started = Instant::now();
        let answer = get(&app, &format!("/api/v1/cards/{peer_card}?host=peer")).await;
        (answer, started.elapsed())
    };
    let local = async {
        tokio::time::sleep(ms(100)).await;
        let started = Instant::now();
        let answer = get(&app, "/api/v1/board?filter=all").await;
        add_card(&pair.board, "written while the peer hangs");
        (answer, started.elapsed())
    };
    let ((query, waited), (board, took)) = tokio::join!(query, local);
    assert_eq!(
        query.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        query.body
    );
    let error: ErrorBody = serde_json::from_str(&query.body).unwrap();
    assert_eq!(error.error.code, "peer_unreachable");
    let current = error.error.current.unwrap();
    assert_eq!(current["title"], Value::from("a peer card"));
    assert_eq!(current["host"], Value::from("peer"));
    assert!(waited >= quick().query, "answered after {waited:?}");
    assert_eq!(board.status, StatusCode::OK);
    assert!(took < ms(500), "a local read took {took:?}");

    // No frame for `timing.down`: down, then a new connection.
    next_message(&mut rx, |message| match message {
        PeerMessage::Link(link) if link.link == LinkState::Down => Some(()),
        _ => None,
    })
    .await;
    until("a reconnect", || {
        (connects.load(Ordering::SeqCst) >= 2).then_some(())
    })
    .await;
    next_message(&mut rx, |message| match message {
        PeerMessage::Link(link) if link.link == LinkState::Live => Some(()),
        _ => None,
    })
    .await;

    // The copy outlived the outage, and the Machines row follows the link.
    assert!(titles(&peers).contains(&"a peer card".to_owned()));
    let machines = get(&app, "/api/v1/machines").await;
    assert_eq!(machines.status, StatusCode::OK, "{}", machines.body);
    let machines: Value = serde_json::from_str(&machines.body).unwrap();
    let row = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["label"] == "peer")
        .unwrap_or_else(|| panic!("no peer row in {machines}"))
        .clone();
    assert_eq!(row["link"], "live", "{row}");
}

#[test]
fn serve_never_opens_onward_link() {
    // The peer is itself a board host with a peer of its own.
    let pair = pair();
    let third = Fleet::new();
    pair_onto(&pair.peer, &third, "third");
    add_card(&pair.peer, "the peer's own card");
    add_card(&third, "a card on the third host");
    let third_card = {
        let store = third.open();
        let snapshot = own_snapshot(&store, Timestamp::now()).unwrap();
        let rows = snapshot.queries.board.rows;
        rows.into_iter()
            .find(|row| row.title == "a card on the third host")
            .unwrap()
            .id
    };

    let mut link = SyncLink::open(pair.peer.dir.path());
    link.send(&hello(&pair, None));
    assert!(matches!(link.next(), Frame::Welcome(_)));
    let Frame::Snapshot(snapshot) = link.next() else {
        panic!("expected a snapshot");
    };
    // Its own rows only, untagged, and only its own machine row.
    let rows = &snapshot.queries.board.rows;
    let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
    assert!(titles.contains(&"the peer's own card"), "{titles:?}");
    assert!(!titles.contains(&"a card on the third host"), "{titles:?}");
    assert!(
        rows.iter()
            .all(|row| row.host.is_none() && row.link.is_none())
    );
    let machines = &snapshot.queries.machines.machines;
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].id, pair.peer.host);

    // It never reads on another host's behalf.
    let mut ask = |id: &str, path: String| {
        link.send(&Frame::Query {
            id: id.into(),
            path,
        });
        loop {
            match link.next() {
                Frame::Result {
                    id: answered,
                    http_status,
                    ..
                } if answered == id => return http_status,
                Frame::Event(_) => {}
                frame => panic!("expected a result, got {frame:?}"),
            }
        }
    };
    let own = rows
        .iter()
        .find(|row| row.title == "the peer's own card")
        .unwrap()
        .id
        .clone();
    assert_eq!(ask("q1", format!("cards/{own}")), 200);
    assert_eq!(ask("q2", format!("cards/{own}?host=third")), 400);
    assert_eq!(ask("q3", format!("cards/{third_card}")), 404);
}
