//! The federated board end to end (`protocol.md` 10.2): two homes, a board
//! host and its peer, linked in memory instead of over SSH, on shrunk
//! timings.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tiller_core::domain::{CardKind, PeerScope};
use tiller_core::error::READ_ONLY_MARKER;
use tiller_core::state::cards::NewCard;
use tiller_core::state::peers::{
    KeyFile, NewPairing, NewPeerKey, generate_key, key_path, public_key_text, write_key_file,
};
use tiller_core::{Store, Timestamp};
use tiller_herdr::testing::{Fleet, owner};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use super::forward::{Mutate, Op, Order};
use super::link::{Connecting, Connection, Connector, PeerMessage, Peers, Target};
use super::serve::{ServeConfig, own_snapshot, serve};
use super::{Frame, Hello, HelloKey, Timing, Welcome};
use crate::api::tests::{Answer, app_with_peers, get, mutation, owner_cookie, send};
use crate::api::{OrchestratorStart, StartError};
use crate::dto::{BoardResponse, ErrorBody, ErrorDetail, LinkState, MutationResponse};
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
    serve_with_starter(home, from, None)
}

/// [`serve_on_socket`] with the starter a peer's binary would provide.
fn serve_with_starter(
    home: &Path,
    from: &str,
    orchestrators: Option<Arc<dyn OrchestratorStart>>,
) -> UnixStream {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let config = ServeConfig {
        home: home.to_owned(),
        from: from.to_owned(),
        timing: quick(),
        herdr: None,
        orchestrators,
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
    // The peer's own build reaches the board on its row, and this host's
    // row carries the board host's.
    assert_eq!(row["version"], env!("CARGO_PKG_VERSION"), "{row}");
    let local = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "local")
        .unwrap_or_else(|| panic!("no local row in {machines}"));
    assert_eq!(local["version"], env!("CARGO_PKG_VERSION"), "{local}");
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
    assert_eq!(
        machines[0].version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );

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

// -- forwarded mutations (`protocol.md` 10.3) -------------------------------------

fn card_id(fleet: &Fleet, title: &str) -> String {
    fleet
        .open()
        .conn()
        .query_row("SELECT id FROM cards WHERE title = ?1", [title], |row| {
            row.get(0)
        })
        .unwrap()
}

fn titled(fleet: &Fleet, title: &str) -> i64 {
    fleet
        .open()
        .conn()
        .query_row(
            "SELECT count(*) FROM cards WHERE title = ?1",
            [title],
            |row| row.get(0),
        )
        .unwrap()
}

fn last_seq(fleet: &Fleet) -> i64 {
    fleet.open().last_seq().unwrap()
}

/// Each event after `seq` on `fleet`, as `txn kind actor`.
fn events_since(fleet: &Fleet, seq: i64) -> Vec<String> {
    let store = fleet.open();
    let mut statement = store
        .conn()
        .prepare("SELECT txn || ' ' || kind || ' ' || actor || ' ' || payload FROM events WHERE seq > ?1")
        .unwrap();
    statement
        .query_map([seq], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn signer(pair: &Pair) -> HelloKey<'_> {
    HelloKey {
        key: &pair.key,
        from: "test-host",
        key_id: &pair.key_id,
        viewer_host_id: &pair.board.host,
    }
}

fn comment_order(card: &str, key: &str) -> Order {
    Order {
        op: Op::Comment,
        target: Some(card.to_owned()),
        body: json!({"idempotency_key": key, "text": "from the board"}),
        idempotency_key: key.to_owned(),
    }
}

/// A signed `comment` on `card` for the peer, as the board host sends it.
fn comment(pair: &Pair, card: &str, key: &str) -> Mutate {
    Mutate::signed(
        &signer(pair),
        &pair.peer.host,
        comment_order(card, key),
        Timestamp::now(),
    )
}

/// A link to the peer's `peer serve`, past `welcome` and the snapshot.
fn linked(pair: &Pair) -> SyncLink {
    let mut link = SyncLink::open(pair.peer.dir.path());
    link.send(&hello(pair, None));
    assert!(matches!(link.next(), Frame::Welcome(_)));
    assert!(matches!(link.next(), Frame::Snapshot(_)));
    link
}

impl SyncLink {
    /// Sends `frame` and returns its `result`'s status and body.
    fn mutate(&mut self, frame: Mutate) -> (u16, Value) {
        let id = frame.id.clone();
        self.send(&Frame::Mutate(Box::new(frame)));
        loop {
            match self.next() {
                Frame::Result {
                    id: answered,
                    http_status,
                    body,
                } if answered == id => return (http_status, body),
                Frame::Event(_) => {}
                frame => panic!("expected a result, got {frame:?}"),
            }
        }
    }
}

/// A 502 `peer_refused` naming `reason`, with no event written on the peer
/// since `before`.
#[track_caller]
fn assert_refused(pair: &Pair, (status, body): (u16, Value), reason: &str, before: i64) {
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "peer_refused", "{body}");
    assert_eq!(body["error"]["reason"], reason, "{body}");
    assert_eq!(last_seq(&pair.peer), before, "a refused mutate wrote");
}

#[test]
fn mutate_bad_sig_refused() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    let before = last_seq(&pair.peer);

    // A body changed after it was signed.
    let mut tampered = comment(&pair, &card, "k-1");
    tampered.body["text"] = json!("not what was signed");
    assert_refused(&pair, link.mutate(tampered), "bad_sig", before);

    // Signed by a key the peer never accepted, under this link's key id.
    let stranger = generate_key().unwrap();
    let forged = Mutate::signed(
        &HelloKey {
            key: &stranger,
            ..signer(&pair)
        },
        &pair.peer.host,
        comment_order(&card, "k-2"),
        Timestamp::now(),
    );
    assert_refused(&pair, link.mutate(forged), "bad_sig", before);

    // An unsigned frame.
    let mut unsigned = comment(&pair, &card, "k-3");
    unsigned.sig.clear();
    assert_refused(&pair, link.mutate(unsigned), "bad_sig", before);

    // A refusal answers that frame only: the link still applies a good one.
    let (status, body) = link.mutate(comment(&pair, &card, "k-4"));
    assert_eq!(status, 200, "{body}");
}

#[test]
fn mutate_replayed_ts_refused() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    let before = last_seq(&pair.peer);

    // Older or newer than the 120 s skew, with a valid signature.
    for minutes in [-10, -3, 3] {
        let stale = Mutate::signed(
            &signer(&pair),
            &pair.peer.host,
            comment_order(&card, &format!("k-{minutes}")),
            Timestamp::now().plus_minutes(minutes),
        );
        assert_refused(&pair, link.mutate(stale), "clock_skew", before);
    }

    // The same fresh frame sent twice applies once: the second answer is
    // the stored one.
    let frame = comment(&pair, &card, "k-fresh");
    let (status, first) = link.mutate(frame.clone());
    assert_eq!(status, 200, "{first}");
    let applied = last_seq(&pair.peer);
    assert!(applied > before);
    let (status, again) = link.mutate(frame);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["replayed"], true, "{again}");
    assert_eq!(again["events"], first["events"]);
    assert_eq!(last_seq(&pair.peer), applied);
}

#[test]
fn mutate_wrong_host_id_refused() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    let before = last_seq(&pair.peer);
    let third = Fleet::new();
    for host in [&pair.board.host, &third.host, &String::new()] {
        let elsewhere = Mutate::signed(
            &signer(&pair),
            host,
            comment_order(&card, "k-1"),
            Timestamp::now(),
        );
        assert_refused(&pair, link.mutate(elsewhere), "wrong_host", before);
    }
}

/// G26 on the peer: a marker naming another system refuses a forwarded
/// write, and one naming `tiller` takes it again.
#[test]
fn owner_marker_refuses_forwarded_mutations() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    let before = last_seq(&pair.peer);
    let marker = pair.peer.dir.path().join("owner");
    std::fs::write(&marker, "other-system\n").unwrap();
    assert_refused(
        &pair,
        link.mutate(comment(&pair, &card, "k-1")),
        "owner_marker",
        before,
    );
    std::fs::write(&marker, "tiller\n").unwrap();
    let (status, body) = link.mutate(comment(&pair, &card, "k-2"));
    assert_eq!(status, 200, "{body}");
}

/// G25 on the peer: a card in a project another host owns is not the
/// peer's to change.
#[test]
fn foreign_project_refused() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    {
        let store = pair.peer.open();
        let other = "01JHOSTC0000000000000000000";
        store
            .conn()
            .execute(
                "INSERT INTO hosts (id, label, kind) VALUES (?1, 'host-c', 'remote')",
                [other],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE projects SET owner_host_id = ?1 WHERE id = 'alpha'",
                [other],
            )
            .unwrap();
    }
    let before = last_seq(&pair.peer);
    assert_refused(
        &pair,
        link.mutate(comment(&pair, &card, "k-1")),
        "foreign_project",
        before,
    );
}

/// R10 on the peer: while its daemon holds `state.db` read-only, a
/// forwarded write gets the same 503 a local one does.
#[test]
fn damaged_peer_refuses_forwarded_mutations() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    let before = last_seq(&pair.peer);
    std::fs::write(
        pair.peer.dir.path().join(READ_ONLY_MARKER),
        "quick_check: test damage",
    )
    .unwrap();
    let (status, body) = link.mutate(comment(&pair, &card, "k-1"));
    assert_eq!(status, 503, "{body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert_eq!(body["error"]["code"], "unavailable", "{body}");
    assert!(
        message
            .contains("read only: state.db failed its integrity check (quick_check: test damage)"),
        "{body}"
    );
    assert_eq!(last_seq(&pair.peer), before, "a read-only peer wrote");
}

#[test]
fn revoked_key_refused() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    pair.peer.open().peer_revoke(&owner(), "test-host").unwrap();
    let before = last_seq(&pair.peer);

    // The revoke lands mid-link: the mutate is refused, or the link is
    // closed with `refused` first. Either way nothing is written.
    let frame = comment(&pair, &card, "k-1");
    let id = frame.id.clone();
    let _ = link
        .writer
        .write_all(Frame::Mutate(Box::new(frame)).line().unwrap().as_bytes());
    let reason = loop {
        match link.next() {
            Frame::Result {
                id: answered,
                http_status,
                body,
            } if answered == id => {
                assert_eq!(http_status, 502, "{body}");
                break body["error"]["reason"].as_str().unwrap().to_owned();
            }
            Frame::Refused { reason, .. } => break reason,
            Frame::Event(_) => {}
            frame => panic!("expected a refusal, got {frame:?}"),
        }
    };
    assert_eq!(reason, "revoked");
    assert_eq!(last_seq(&pair.peer), before);

    // A new link with the revoked key is refused at `hello`.
    let mut again = SyncLink::open(pair.peer.dir.path());
    again.send(&hello(&pair, None));
    assert!(
        matches!(again.next(), Frame::Refused { reason, .. } if reason == "revoked"),
        "a revoked key opened a link"
    );
}

#[test]
fn spawn_not_forwardable() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let mut link = linked(&pair);
    let before = last_seq(&pair.peer);
    for op in [
        "POST /api/v1/runs",
        "POST /api/v1/projects/{id}/stage",
        "POST /api/v1/interventions/{id}/label",
        "POST /api/v1/agents/{session}/focus",
        "POST /api/v1/memory/proposals/{id}/decide",
        "DELETE /api/v1/cards/{id}",
    ] {
        let mut frame = comment(&pair, &card, "k-1");
        frame.op = op.to_owned();
        frame.body = json!({"idempotency_key": "k-1", "project": "alpha", "role": "builder"});
        frame.sign(&pair.key);
        assert_refused(&pair, link.mutate(frame), "not_allowlisted", before);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_not_forwardable_from_the_board() {
    // Off the allowlist, the board refuses before it looks at the link:
    // the same 502 with no link task at all.
    let pair = pair();
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), Peers::idle());
    let before = (last_seq(&pair.board), last_seq(&pair.peer));
    for (path, body) in [
        (
            "/api/v1/projects/alpha/stage",
            json!({"idempotency_key": "k-1", "host": "peer", "rev": 1, "stage": "launched"}),
        ),
        (
            "/api/v1/agents/w1:p1/focus",
            json!({"idempotency_key": "k-2", "host": "peer"}),
        ),
        (
            "/api/v1/interventions/a-1/label",
            json!({"idempotency_key": "k-3", "host": "peer", "label": "planned"}),
        ),
    ] {
        let answer = send(&app, mutation("POST", path, body, &[("cookie", &cookie)])).await;
        let error = refusal(&answer, StatusCode::BAD_GATEWAY);
        assert_eq!(error.code, "peer_refused");
        assert_eq!(error.reason.as_deref(), Some("not_allowlisted"));
        assert_eq!(error.host.as_deref(), Some("peer"));
    }
    assert_eq!((last_seq(&pair.board), last_seq(&pair.peer)), before);
}

#[test]
fn read_scope_cannot_mutate() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    pair.peer
        .open()
        .conn()
        .execute("UPDATE peer_keys SET scope = 'read'", [])
        .unwrap();
    let mut link = linked(&pair);
    let before = last_seq(&pair.peer);
    assert_refused(
        &pair,
        link.mutate(comment(&pair, &card, "k-1")),
        "read_scope",
        before,
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn read_scope_cannot_mutate_from_the_board() {
    // A `read` pairing on the board host never sends a mutate at all.
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    for fleet in [&pair.board, &pair.peer] {
        let store = fleet.open();
        let conn = store.conn();
        conn.execute(
            "UPDATE hosts SET peer_scope = 'read' WHERE peer_scope IS NOT NULL",
            [],
        )
        .unwrap();
        conn.execute("UPDATE peer_keys SET scope = 'read'", [])
            .unwrap();
    }
    let wire = Wire::new(pair.peer.dir.path());
    let sent = wire.sent.clone();
    let (peers, _stop) = start(pair.board.dir.path(), wire);
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let before = (last_seq(&pair.board), last_seq(&pair.peer));
    let answer = forward_comment(&app, &cookie, &card, "k-1").await;
    let error = refusal(&answer, StatusCode::BAD_GATEWAY);
    assert_eq!(error.reason.as_deref(), Some("read_scope"));
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert_eq!((last_seq(&pair.board), last_seq(&pair.peer)), before);
    assert!(!wire_text(&sent).contains(r#""type":"mutate""#));
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_precondition_failure_passes_through() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    add_card(&pair.board, "a local card");
    let card = card_id(&pair.peer, "a peer card");
    let local = card_id(&pair.board, "a local card");
    let (peers, _stop) = start(pair.board.dir.path(), Wire::new(pair.peer.dir.path()));
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let before = last_seq(&pair.peer);

    // Closing a ship card `completed` is the peer's own refusal, exactly as
    // this host's board answers the same request for its own card.
    let close = |id: &str, host: Option<&str>| {
        let mut body = json!({"idempotency_key": format!("k-{id}"), "outcome": "completed"});
        if let Some(host) = host {
            body["host"] = json!(host);
        }
        mutation(
            "POST",
            &format!("/api/v1/cards/{id}/close"),
            body,
            &[("cookie", &cookie)],
        )
    };
    let here = send(&app, close(&local, None)).await;
    let there = send(&app, close(&card, Some("peer"))).await;
    assert!(here.status.is_client_error(), "{}", here.body);
    assert_eq!(there.status, here.status, "{}", there.body);
    let here = refusal(&here, here.status);
    let there = refusal(&there, there.status);
    assert_eq!(there.code, here.code);
    assert_eq!(there.message, here.message);
    assert_eq!(there.host.as_deref(), Some("peer"));
    assert_eq!(here.host, None);

    // A record the peer does not have is its 404.
    let missing = forward_comment(&app, &cookie, "01JNOSUCHCARD000000000000", "k-x").await;
    let error = refusal(&missing, StatusCode::NOT_FOUND);
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert_eq!(last_seq(&pair.peer), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn same_key_twice_applies_once() {
    let pair = pair();
    let (peers, _stop) = start(pair.board.dir.path(), Wire::new(pair.peer.dir.path()));
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let first = forward_create(&app, &cookie, "k-1", "made from the board").await;
    let first = applied(&first);
    assert!(!first.replayed);
    let again = forward_create(&app, &cookie, "k-1", "made from the board").await;
    let again = applied(&again);
    assert!(again.replayed);
    assert_eq!(again.events, first.events);
    assert_eq!(titled(&pair.peer, "made from the board"), 1);
    assert_eq!(titled(&pair.board, "made from the board"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn retried_504_applies_once() {
    // The peer applies the first try, and its `result` is lost on the way
    // back: the board cannot tell, so it answers 504. The retry with the
    // same key is the stored answer.
    let pair = pair();
    let wire = Wire::new(pair.peer.dir.path());
    wire.lose_results.store(1, Ordering::SeqCst);
    let (peers, _stop) = start(pair.board.dir.path(), wire);
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let started = Instant::now();
    let lost = forward_create(&app, &cookie, "k-1", "sent once").await;
    let error = refusal(&lost, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error.code, "forward_uncertain");
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert!(started.elapsed() >= quick().query);
    assert_eq!(titled(&pair.peer, "sent once"), 1);

    let retried = forward_create(&app, &cookie, "k-1", "sent once").await;
    assert!(applied(&retried).replayed);
    assert_eq!(titled(&pair.peer, "sent once"), 1);
    assert_eq!(titled(&pair.board, "sent once"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_returns_503_and_queues_nothing() {
    let pair = pair();
    let up = Arc::new(AtomicBool::new(false));
    let (peers, _stop) = start(
        pair.board.dir.path(),
        Switch {
            home: pair.peer.dir.path().to_owned(),
            up: up.clone(),
        },
    );
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let before = (last_seq(&pair.board), last_seq(&pair.peer));
    for _ in 0..3 {
        let started = Instant::now();
        let answer = forward_create(&app, &cookie, "k-1", "made while down").await;
        let error = refusal(&answer, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, "peer_unreachable");
        assert_eq!(error.host.as_deref(), Some("peer"));
        assert!(
            started.elapsed() < ms(500),
            "refused after {:?}",
            started.elapsed()
        );
        tokio::time::sleep(ms(100)).await;
    }
    assert_eq!(
        (last_seq(&pair.board), last_seq(&pair.peer)),
        before,
        "{:?}",
        events_since(&pair.board, before.0)
    );

    // The link comes up: nothing was held to send.
    up.store(true, Ordering::SeqCst);
    until("the snapshot", || live_with_rows(&peers)).await;
    tokio::time::sleep(ms(300)).await;
    assert_eq!(titled(&pair.peer, "made while down"), 0);
    assert_eq!(last_seq(&pair.peer), before.1);
    // The board host writes only the link's own Machines row.
    for event in events_since(&pair.board, before.0) {
        assert!(
            event.starts_with("cache.refresh host.changed daemon "),
            "{event}"
        );
    }

    // The owner's retry from the control applies.
    let retried = forward_create(&app, &cookie, "k-1", "made while down").await;
    assert!(!applied(&retried).replayed);
    assert_eq!(titled(&pair.peer, "made while down"), 1);
    assert_eq!(titled(&pair.board, "made while down"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_mutation_never_writes_viewer_db() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let (peers, _stop) = start(pair.board.dir.path(), Wire::new(pair.peer.dir.path()));
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let board_before = last_seq(&pair.board);
    let peer_before = last_seq(&pair.peer);

    let created = forward_create(&app, &cookie, "k-1", "made from the board").await;
    assert_eq!(applied(&created).host.as_deref(), Some("peer"));
    let commented = forward_comment(&app, &cookie, &card, "k-2").await;
    assert_eq!(applied(&commented).host.as_deref(), Some("peer"));
    let cancelled = send(
        &app,
        mutation(
            "POST",
            &format!("/api/v1/cards/{card}/cancel"),
            json!({"idempotency_key": "k-3", "host": "peer", "reason": "not needed"}),
            &[("cookie", &cookie)],
        ),
    )
    .await;
    assert_eq!(applied(&cancelled).host.as_deref(), Some("peer"));

    // Nothing on the board host's database; on the peer, every event is the
    // forwarded owner's, with the key that signed it.
    assert_eq!(last_seq(&pair.board), board_before);
    assert_eq!(titled(&pair.board, "made from the board"), 0);
    let store = pair.peer.open();
    let mut statement = store
        .conn()
        .prepare("SELECT actor, payload FROM events WHERE seq > ?1")
        .unwrap();
    let events: Vec<(String, String)> = statement
        .query_map([peer_before], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(events.len() >= 3, "{events:?}");
    for (actor, payload) in events {
        assert_eq!(actor, "board@test-host");
        let payload: Value = serde_json::from_str(&payload).unwrap();
        let via = &payload["via_peer"];
        assert_eq!(via["key_id"], Value::from(pair.key_id.clone()), "{payload}");
        assert_eq!(via["viewer_host_id"], Value::from(pair.board.host.clone()));
        assert_eq!(via["viewer_label"], "test-host");
        assert!(via["frame_id"].is_string(), "{payload}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn frames_never_carry_cookie() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    let card = card_id(&pair.peer, "a peer card");
    let wire = Wire::new(pair.peer.dir.path());
    let sent = wire.sent.clone();
    let (peers, _stop) = start(pair.board.dir.path(), wire);
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    applied(&forward_comment(&app, &cookie, &card, "k-1").await);
    applied(&forward_create(&app, &cookie, "k-2", "made from the board").await);
    let read = get(&app, &format!("/api/v1/cards/{card}?host=peer")).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);

    let text = wire_text(&sent);
    let secret = cookie.trim_start_matches("tiller_owner=");
    assert!(!secret.is_empty());
    assert!(!text.contains(secret), "a frame carried the owner cookie");
    assert!(!text.to_lowercase().contains("cookie"));
    assert!(!text.contains("tiller_owner"));
    let mutates: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|frame| frame["type"] == "mutate")
        .collect();
    assert_eq!(mutates.len(), 2, "{text}");
    for frame in mutates {
        // The body goes without `host`: the peer serves only its own.
        assert!(frame["body"].get("host").is_none(), "{frame}");
    }
}

// -- forwarding helpers -------------------------------------------------------------

async fn forward_create(app: &Router, cookie: &str, key: &str, title: &str) -> Answer {
    let body = json!({"idempotency_key": key, "host": "peer", "project": "alpha", "kind": "ship", "title": title});
    send(
        app,
        mutation("POST", "/api/v1/cards", body, &[("cookie", cookie)]),
    )
    .await
}

async fn forward_comment(app: &Router, cookie: &str, card: &str, key: &str) -> Answer {
    let body = json!({"idempotency_key": key, "host": "peer", "text": "from the board"});
    let path = format!("/api/v1/cards/{card}/comment");
    send(app, mutation("POST", &path, body, &[("cookie", cookie)])).await
}

#[track_caller]
fn applied(answer: &Answer) -> MutationResponse {
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    serde_json::from_str(&answer.body).unwrap()
}

#[track_caller]
fn refusal(answer: &Answer, status: StatusCode) -> ErrorDetail {
    assert_eq!(answer.status, status, "{}", answer.body);
    serde_json::from_str::<ErrorBody>(&answer.body)
        .unwrap()
        .error
}

fn wire_text(sent: &Mutex<Vec<u8>>) -> String {
    String::from_utf8(sent.lock().unwrap().clone()).unwrap()
}

/// Dials `peer serve` over a socket pair like [`Socket`], keeps every byte
/// the board host sends, and loses the next `lose_results` `result` frames
/// on the way back.
struct Wire {
    home: PathBuf,
    sent: Arc<Mutex<Vec<u8>>>,
    lose_results: Arc<AtomicUsize>,
    starter: Option<Arc<dyn OrchestratorStart>>,
}

impl Wire {
    fn new(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
            sent: Arc::default(),
            lose_results: Arc::default(),
            starter: None,
        }
    }

    fn with_starter(mut self, starter: Arc<dyn OrchestratorStart>) -> Self {
        self.starter = Some(starter);
        self
    }
}

impl Connector for Wire {
    fn connect(&self, target: &Target) -> Connecting {
        let socket = serve_with_starter(&self.home, &target.from, self.starter.clone());
        let sent = self.sent.clone();
        let lose = self.lose_results.clone();
        Box::pin(async move {
            socket.set_nonblocking(true)?;
            let (reader, mut writer) = tokio::net::UnixStream::from_std(socket)?.into_split();
            let (board_out, mut wire_in) = tokio::io::duplex(1 << 20);
            tokio::spawn(async move {
                let mut buf = vec![0; 1 << 16];
                while let Ok(read) = wire_in.read(&mut buf).await {
                    if read == 0 {
                        break;
                    }
                    sent.lock().unwrap().extend_from_slice(&buf[..read]);
                    if writer.write_all(&buf[..read]).await.is_err() {
                        break;
                    }
                }
            });
            let (board_in, mut wire_out) = tokio::io::duplex(1 << 20);
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let lost = line.starts_with(r#"{"type":"result""#)
                        && lose
                            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                            .is_ok();
                    if lost {
                        continue;
                    }
                    if wire_out
                        .write_all(format!("{line}\n").as_bytes())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Ok(Connection {
                reader: Box::new(tokio::io::BufReader::new(board_in)),
                writer: Box::new(board_out),
                child: None,
            })
        })
    }
}

/// A peer that cannot be reached until `up` is set.
struct Switch {
    home: PathBuf,
    up: Arc<AtomicBool>,
}

impl Connector for Switch {
    fn connect(&self, target: &Target) -> Connecting {
        if !self.up.load(Ordering::SeqCst) {
            return Box::pin(async { Err(std::io::Error::other("no route to host")) });
        }
        Wire::new(&self.home).connect(target)
    }
}

// -- asks and orchestrator starts ---------------------------------------------------

/// A peer's starter that records what it was asked and refuses, so a test
/// sees the forwarded start arrive without spawning anything.
#[derive(Default)]
struct RecordingStarter {
    seen: Mutex<Vec<(String, String, Option<String>)>>,
}

impl OrchestratorStart for RecordingStarter {
    fn start(
        &self,
        _store: &mut Store,
        ctx: &tiller_core::Ctx,
        project: &str,
    ) -> Result<tiller_core::Applied<tiller_agents::spawn::orchestrator::Orchestrator>, StartError>
    {
        self.seen.lock().unwrap().push((
            project.to_owned(),
            ctx.actor.to_string(),
            ctx.key.clone(),
        ));
        Err(StartError::Refused {
            exit: 5,
            code: "stub_refused",
            message: "the stub starter refuses".into(),
            hint: None,
        })
    }
}

fn ask_count(fleet: &Fleet, text: &str) -> i64 {
    fleet
        .open()
        .conn()
        .query_row("SELECT count(*) FROM asks WHERE text = ?1", [text], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_ask_is_recorded_on_the_peer_once() {
    let pair = pair();
    let (peers, _stop) = start(pair.board.dir.path(), Wire::new(pair.peer.dir.path()));
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let ask = |key: &str| {
        mutation(
            "POST",
            "/api/v1/projects/alpha/asks",
            json!({"idempotency_key": key, "host": "peer", "text": "optimize fonts"}),
            &[("cookie", &cookie)],
        )
    };
    let first = send(&app, ask("k-1")).await;
    let first = applied(&first);
    assert_eq!(first.host.as_deref(), Some("peer"));
    assert!(!first.replayed);
    assert_eq!(ask_count(&pair.peer, "optimize fonts"), 1);
    assert_eq!(ask_count(&pair.board, "optimize fonts"), 0);

    // The same key again is the stored answer, not a second ask.
    let again = send(&app, ask("k-1")).await;
    assert!(applied(&again).replayed);
    assert_eq!(ask_count(&pair.peer, "optimize fonts"), 1);

    // A project the peer does not have is its 404.
    let missing = send(
        &app,
        mutation(
            "POST",
            "/api/v1/projects/nope/asks",
            json!({"idempotency_key": "k-2", "host": "peer", "text": "hello"}),
            &[("cookie", &cookie)],
        ),
    )
    .await;
    let error = refusal(&missing, StatusCode::NOT_FOUND);
    assert_eq!(error.host.as_deref(), Some("peer"));
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_orchestrator_start_runs_the_peers_starter() {
    let pair = pair();
    let starter = Arc::new(RecordingStarter::default());
    let wire = Wire::new(pair.peer.dir.path()).with_starter(starter.clone());
    let (peers, _stop) = start(pair.board.dir.path(), wire);
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let answer = send(
        &app,
        mutation(
            "POST",
            "/api/v1/projects/alpha/orchestrator",
            json!({"idempotency_key": "k-1", "host": "peer"}),
            &[("cookie", &cookie)],
        ),
    )
    .await;
    // The stub's refusal comes back as the peer's own, with `host`.
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error.code, "stub_refused");
    assert_eq!(error.host.as_deref(), Some("peer"));
    let seen = starter.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![(
            "alpha".to_owned(),
            "board@test-host".to_owned(),
            Some("k-1".to_owned())
        )]
    );
}

#[test]
fn forwarded_orchestrator_start_without_a_starter_is_unavailable() {
    let pair = pair();
    let mut link = linked(&pair);
    let order = Order {
        op: Op::StartOrchestrator,
        target: Some("alpha".to_owned()),
        body: json!({"idempotency_key": "k-1"}),
        idempotency_key: "k-1".to_owned(),
    };
    let frame = Mutate::signed(&signer(&pair), &pair.peer.host, order, Timestamp::now());
    let (status, body) = link.mutate(frame);
    assert_eq!(status, 503, "{body}");
}

/// G25 on the peer: an ask or a start for a project another host owns is
/// refused before anything is written.
#[test]
fn foreign_project_ask_and_start_refused() {
    let pair = pair();
    let mut link = linked(&pair);
    {
        let store = pair.peer.open();
        let other = "01JHOSTC0000000000000000000";
        store
            .conn()
            .execute(
                "INSERT INTO hosts (id, label, kind) VALUES (?1, 'host-c', 'remote')",
                [other],
            )
            .unwrap();
        store
            .conn()
            .execute(
                "UPDATE projects SET owner_host_id = ?1 WHERE id = 'alpha'",
                [other],
            )
            .unwrap();
    }
    let before = last_seq(&pair.peer);
    for (op, body) in [
        (
            Op::AskNote,
            json!({"idempotency_key": "k-1", "text": "optimize fonts"}),
        ),
        (Op::StartOrchestrator, json!({"idempotency_key": "k-1"})),
    ] {
        let order = Order {
            op,
            target: Some("alpha".to_owned()),
            body,
            idempotency_key: "k-1".to_owned(),
        };
        let frame = Mutate::signed(&signer(&pair), &pair.peer.host, order, Timestamp::now());
        assert_refused(&pair, link.mutate(frame), "foreign_project", before);
    }
}
