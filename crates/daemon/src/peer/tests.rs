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
use super::pane::{self, PaneInput, PaneKind, PaneOpen, PaneSigner};
use super::serve::{ServeConfig, own_snapshot, serve};
use super::{Frame, Hello, HelloKey, Timing, Welcome};
use crate::api::tests::{
    Answer, app_with_peers, b64, get, holder_calls, holder_pane, mirror_herdr, mutation, next_line,
    owner_cookie, send, waiting,
};
use crate::api::{BoardRelaunch, Controlled, OrchestratorStart, StartError};
use crate::dto::{BoardResponse, ErrorBody, ErrorDetail, LinkState, MutationResponse};
use crate::readonly::Damage;
use tiller_core::domain::{Run, Session};
use tiller_panes::FakePanes;

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
        proof: ms(3000),
        skew: Duration::from_secs(120),
        backoff: [ms(50); 4],
        refused_retry: ms(200),
        refused_backoff: [ms(200); 4],
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
                unattended: false,
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
    serve_timed(home, from, orchestrators, quick())
}

/// [`serve_with_starter`] on other timings.
fn serve_timed(
    home: &Path,
    from: &str,
    orchestrators: Option<Arc<dyn OrchestratorStart>>,
    timing: Timing,
) -> UnixStream {
    serve_config(ServeConfig {
        home: home.to_owned(),
        from: from.to_owned(),
        timing,
        herdr: None,
        orchestrators,
        holder: None,
    })
}

/// `tiller peer serve` on `config`, on one end of a socket pair; the other
/// end is returned.
fn serve_config(config: ServeConfig) -> UnixStream {
    let (ours, theirs) = UnixStream::pair().unwrap();
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
        Self::on(serve_on_socket(home, "test-host"))
    }

    fn on(socket: UnixStream) -> Self {
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
            match self.any() {
                Frame::Beat { .. } => continue,
                frame => return frame,
            }
        }
    }

    /// The next frame, a `beat` too.
    fn any(&mut self) -> Frame {
        let mut line = String::new();
        assert!(
            self.reader.read_line(&mut line).unwrap() > 0,
            "the link closed"
        );
        serde_json::from_str(&line).unwrap()
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
    start_timed(home, connector, quick())
}

/// [`start`] on other timings.
fn start_timed(
    home: &Path,
    connector: impl Connector + 'static,
    timing: Timing,
) -> (Arc<Peers>, watch::Sender<bool>) {
    let (stop_tx, stop) = watch::channel(false);
    let peers = Peers::start(
        home.to_owned(),
        Arc::new(connector),
        timing,
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
    welcomed(pair, SyncLink::open(pair.peer.dir.path()))
}

fn welcomed(pair: &Pair, mut link: SyncLink) -> SyncLink {
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

/// A revoke closes a live link before any frame after it goes out, its own
/// `peer.revoked` event among them, not on the next tick (`protocol.md`
/// 10.1: "an open link from that key closes at once").
#[test]
fn a_revoke_closes_the_link_before_its_next_frame() {
    let pair = pair();
    let slow_tick = Timing {
        tick: Duration::from_secs(60),
        ..quick()
    };
    let socket = serve_timed(pair.peer.dir.path(), "test-host", None, slow_tick);
    let mut link = welcomed(&pair, SyncLink::on(socket));
    pair.peer.open().peer_revoke(&owner(), "test-host").unwrap();
    match link.next() {
        Frame::Refused { reason, .. } => assert_eq!(reason, "revoked"),
        frame => panic!("the revoked key got {frame:?} first"),
    }
}

/// A captured `hello` sent again is refused, even inside its skew window:
/// the peer keeps the nonces it took (`protocol.md` 10.2). Each `peer
/// serve` is its own process, so the set lives on disk, not in memory.
#[test]
fn a_replayed_hello_is_refused() {
    let pair = pair();
    let captured = hello(&pair, None);
    let mut first = SyncLink::open(pair.peer.dir.path());
    first.send(&captured);
    assert!(matches!(first.next(), Frame::Welcome(_)));

    let mut again = SyncLink::open(pair.peer.dir.path());
    again.send(&captured);
    let frame = again.next();
    assert!(
        matches!(&frame, Frame::Refused { reason, .. } if reason == "replayed"),
        "a replayed hello got {frame:?}"
    );

    // A fresh hello from the same board host still opens a link.
    linked(&pair);
}

/// A pane request is taken once: the same signed request again is
/// refused before it reaches herdr.
#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_pane_request_is_refused() {
    let mut pair = pair();
    let session = pair.peer.worker("w2:p1");
    let scratch = tempfile::tempdir().unwrap();
    let open = pane_open(&pair, &session, PaneKind::History);
    let lines = open_pane(&pair, scratch.path(), open.clone(), "").await;
    assert!(
        matches!(
            serde_json::from_str::<Frame>(&lines[0]).unwrap(),
            Frame::Result { .. }
        ),
        "{lines:?}"
    );
    let calls = std::fs::read_to_string(scratch.path().join("calls")).unwrap_or_default();

    let lines = open_pane(&pair, scratch.path(), open, "").await;
    let frame = serde_json::from_str::<Frame>(&lines[0]).unwrap();
    assert!(
        matches!(&frame, Frame::Refused { reason, .. } if reason == "replayed"),
        "a replayed pane request got {frame:?}"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.path().join("calls")).unwrap_or_default(),
        calls,
        "a replayed pane request reached herdr"
    );
}

/// A refusal the owner clears on the peer (here `federation.accept`
/// turned back on) does not end the link for good: it tries again on its
/// slow backoff and goes `live`, with no restart of the daemon.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_link_tries_again_slowly() {
    let pair = pair();
    let config = pair.peer.dir.path().join("config.toml");
    std::fs::write(&config, "[federation]\naccept = false\n").unwrap();
    let connects = Arc::new(AtomicUsize::new(0));
    let (peers, _stop) = start(
        pair.board.dir.path(),
        Socket {
            home: pair.peer.dir.path().to_owned(),
            connects: connects.clone(),
        },
    );
    until("the accept_off refusal", || {
        let federation = peers.federation();
        let view = federation.peers.first()?;
        (view.link == LinkState::Refused && view.link_reason.as_deref() == Some("accept_off"))
            .then_some(())
    })
    .await;
    std::fs::write(&config, "[federation]\naccept = true\n").unwrap();
    until("the link to come back", || live_with_rows(&peers)).await;

    // The real delays are slow: a revoked key or a peer that turned links
    // off is asked again after minutes, never in a tight loop.
    let real = Timing::default();
    assert!(real.refused_backoff[0] >= Duration::from_secs(300));
    assert!(
        real.refused_backoff
            .windows(2)
            .all(|pair| pair[0] <= pair[1])
    );
}

/// A peer that refuses the first `before` hellos, then welcomes one link
/// and refuses it mid-session, then refuses every hello after. It records
/// when each connection began.
struct Scripted {
    home: PathBuf,
    before: usize,
    began: Arc<Mutex<Vec<Instant>>>,
}

impl Connector for Scripted {
    fn connect(&self, _target: &Target) -> Connecting {
        let n = {
            let mut began = self.began.lock().unwrap();
            began.push(Instant::now());
            began.len()
        };
        let refused = |reason: &str| Frame::Refused {
            reason: reason.into(),
            message: None,
        };
        let opening = (|| {
            if n != self.before + 1 {
                return Ok::<_, std::io::Error>(refused("accept_off").line()?);
            }
            let store =
                Store::open_current(self.home.join("state.db")).map_err(std::io::Error::other)?;
            let host = store.this_host().map_err(std::io::Error::other)?;
            let welcome = Frame::Welcome(Welcome {
                host_id: host.id,
                label: host.label,
                schema_version: tiller_core::migrate::LATEST,
                tiller_version: "test".into(),
                server_time: Timestamp::now().to_string(),
            });
            Ok(welcome.line()? + &refused("revoked").line()?)
        })();
        Box::pin(async move {
            let (mut peer, ours) = tokio::io::duplex(1 << 16);
            peer.write_all(opening?.as_bytes()).await?;
            Ok(Connection {
                reader: Box::new(tokio::io::BufReader::new(ours)),
                writer: Box::new(tokio::io::sink()),
                child: None,
            })
        })
    }
}

/// protocol.md: the refused backoff starts over once a link is welcomed.
/// A welcomed session that the peer then refuses waits the first step, not
/// the step the refusals before the welcome had reached.
#[tokio::test(flavor = "multi_thread")]
async fn a_welcomed_link_starts_the_refused_backoff_over() {
    let pair = pair();
    let began = Arc::new(Mutex::new(Vec::new()));
    let (stop_tx, stop) = watch::channel(false);
    let mut timing = quick();
    timing.refused_backoff = [ms(50), ms(50), ms(50), Duration::from_secs(5)];
    let _peers = Peers::start(
        pair.board.dir.path().to_owned(),
        Arc::new(Scripted {
            home: pair.peer.dir.path().to_owned(),
            before: 3,
            began: began.clone(),
        }),
        timing,
        Damage::default(),
        stop,
    );
    // Three refused hellos, the welcomed session, and the try after it.
    until("the try after the welcomed session", || {
        (began.lock().unwrap().len() >= 5).then_some(())
    })
    .await;
    let began = began.lock().unwrap().clone();
    let gap = began[4] - began[3];
    assert!(gap < Duration::from_secs(2), "waited {gap:?}");
    drop(stop_tx);
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
    for (method, path, body) in [
        (
            "POST",
            "/api/v1/projects/alpha/stage",
            json!({"idempotency_key": "k-1", "host": "peer", "rev": 1, "stage": "launched"}),
        ),
        (
            "POST",
            "/api/v1/agents/w1:p1/focus",
            json!({"idempotency_key": "k-2", "host": "peer"}),
        ),
        (
            "POST",
            "/api/v1/interventions/a-1/label",
            json!({"idempotency_key": "k-3", "host": "peer", "label": "planned"}),
        ),
        (
            "PATCH",
            "/api/v1/projects/alpha",
            json!({"idempotency_key": "k-4", "host": "peer", "rev": 1, "capacity": 1}),
        ),
        (
            "POST",
            "/api/v1/projects/alpha/grants",
            json!({"idempotency_key": "k-5", "host": "peer", "kind": "merge",
                "conditions": {"v": 1, "actions": ["squash"]}}),
        ),
        (
            "DELETE",
            "/api/v1/grants/0000001",
            json!({"idempotency_key": "k-6", "host": "peer"}),
        ),
    ] {
        let answer = send(&app, mutation(method, path, body, &[("cookie", &cookie)])).await;
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

/// A forwarded key is bound to its request, as a local one is (`protocol.md`
/// 6.3): the same key with another body is the peer's 422 refusal and
/// writes nothing, and a true retry still replays.
#[tokio::test(flavor = "multi_thread")]
async fn a_forwarded_key_replays_only_its_own_request() {
    let pair = pair();
    let (peers, _stop) = start(pair.board.dir.path(), Wire::new(pair.peer.dir.path()));
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let first = forward_create(&app, &cookie, "k-1", "made from the board").await;
    let first = applied(&first);
    assert!(!first.replayed);
    let before = last_seq(&pair.peer);

    let other = forward_create(&app, &cookie, "k-1", "another title").await;
    let error = refusal(&other, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error.code, "refused");
    assert!(
        error.message.contains("other arguments"),
        "{}",
        error.message
    );
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert_eq!(titled(&pair.peer, "another title"), 0);
    assert_eq!(last_seq(&pair.peer), before);

    let again = forward_create(&app, &cookie, "k-1", "made from the board").await;
    let again = applied(&again);
    assert!(again.replayed);
    assert_eq!(again.events, first.events);
    assert_eq!(titled(&pair.peer, "made from the board"), 1);
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
    herdr: Option<tiller_herdr::client::Cli>,
    holder: Option<Arc<dyn crate::holder::Holder>>,
}

impl Wire {
    fn new(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
            sent: Arc::default(),
            lose_results: Arc::default(),
            starter: None,
            herdr: None,
            holder: None,
        }
    }

    fn with_starter(mut self, starter: Arc<dyn OrchestratorStart>) -> Self {
        self.starter = Some(starter);
        self
    }

    /// The peer's herdr and terminal holder, which its agent verbs reach.
    fn with_panes(mut self, herdr: tiller_herdr::client::Cli, holder: &FakePanes) -> Self {
        self.herdr = Some(herdr);
        self.holder = Some(Arc::new(holder.clone()));
        self
    }
}

impl Connector for Wire {
    fn connect(&self, target: &Target) -> Connecting {
        let socket = serve_config(ServeConfig {
            home: self.home.clone(),
            from: target.from.clone(),
            timing: quick(),
            herdr: self.herdr.clone(),
            orchestrators: self.starter.clone(),
            holder: self.holder.clone(),
        });
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

// -- card dependencies --------------------------------------------------------------

/// A card dependency on a peer's card (`protocol.md` 7.4): the board host
/// forwards the add and the remove, the peer applies them as
/// `board@<viewer>`, its card read lists the dependency, and the board
/// host's own database is untouched. A retry with the key bound to another
/// card is refused.
#[tokio::test(flavor = "multi_thread")]
async fn forwarded_dependency_is_added_listed_and_removed_on_the_peer() {
    let pair = pair();
    add_card(&pair.peer, "the waiting card");
    add_card(&pair.peer, "the card waited on");
    let waiting = card_id(&pair.peer, "the waiting card");
    let waited = card_id(&pair.peer, "the card waited on");
    let (peers, _stop) = start(pair.board.dir.path(), Wire::new(pair.peer.dir.path()));
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let board_before = last_seq(&pair.board);
    let peer_before = last_seq(&pair.peer);
    let deps = format!("/api/v1/cards/{waiting}/deps");
    let dep = |method: &str, key: &str, on: &str| {
        mutation(
            method,
            &deps,
            json!({"idempotency_key": key, "host": "peer", "depends_on": on}),
            &[("cookie", &cookie)],
        )
    };

    let added = send(&app, dep("POST", "k-add", &waited)).await;
    let added = applied(&added);
    assert_eq!(added.host.as_deref(), Some("peer"));
    assert_eq!(added.data["depends_on_card_id"], waited);
    let read = get(&app, &format!("/api/v1/cards/{waiting}?host=peer")).await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.body);
    let detail: Value = serde_json::from_str(&read.body).unwrap();
    assert_eq!(detail["deps"][0]["released_at"], Value::Null, "{detail}");
    assert_eq!(detail["column"]["badge"], "blocked", "{detail}");

    // The key is bound to its arguments: the same key for another card is
    // the peer's refusal and writes nothing.
    let other = send(&app, dep("POST", "k-add", &waiting)).await;
    assert!(other.status.is_client_error(), "{}", other.body);

    let removed = send(&app, dep("DELETE", "k-rm", &waited)).await;
    let removed = applied(&removed);
    assert_eq!(removed.data["release_reason"], "removed");
    let read = get(&app, &format!("/api/v1/cards/{waiting}?host=peer")).await;
    let detail: Value = serde_json::from_str(&read.body).unwrap();
    assert_eq!(detail["deps"][0]["release_reason"], "removed", "{detail}");

    let events = events_since(&pair.peer, peer_before);
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("dep.add dep.added board@test-host") && e.contains("via_peer")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| e.starts_with("dep.release dep.released board@test-host")),
        "{events:?}"
    );
    assert_eq!(last_seq(&pair.board), board_before);
}

/// G25 on the peer for a dependency: a card in a project another host owns
/// is refused before anything is written.
#[test]
fn foreign_project_dependency_refused() {
    let pair = pair();
    add_card(&pair.peer, "a peer card");
    add_card(&pair.peer, "another peer card");
    let card = card_id(&pair.peer, "a peer card");
    let on = card_id(&pair.peer, "another peer card");
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
    for op in [Op::AddDep, Op::RemoveDep] {
        let frame = Mutate::signed(
            &signer(&pair),
            &pair.peer.host,
            Order {
                op,
                target: Some(card.clone()),
                body: json!({"idempotency_key": "k-1", "depends_on": on}),
                idempotency_key: "k-1".into(),
            },
            Timestamp::now(),
        );
        assert_refused(&pair, link.mutate(frame), "foreign_project", before);
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
        _restart: bool,
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
            current: None,
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

/// G42 on the peer: a `read` key sends no ask and starts no orchestrator.
#[test]
fn read_scope_cannot_ask_or_start() {
    let pair = pair();
    pair.peer
        .open()
        .conn()
        .execute("UPDATE peer_keys SET scope = 'read'", [])
        .unwrap();
    let starter = Arc::new(RecordingStarter::default());
    let socket = serve_with_starter(pair.peer.dir.path(), "test-host", Some(starter.clone()));
    let mut link = welcomed(&pair, SyncLink::on(socket));
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
        assert_refused(&pair, link.mutate(frame), "read_scope", before);
    }
    assert_eq!(ask_count(&pair.peer, "optimize fonts"), 0);
    assert!(starter.seen.lock().unwrap().is_empty(), "the starter ran");
}

// -- a peer's checks -----------------------------------------------------------------

/// A peer's binary that runs its checks: one missing tool and one gate on
/// detect, so the board host can tell the peer's report from its own.
struct PeerDoctor;

impl OrchestratorStart for PeerDoctor {
    fn start(
        &self,
        _store: &mut Store,
        _ctx: &tiller_core::Ctx,
        _project: &str,
        _restart: bool,
    ) -> Result<tiller_core::Applied<tiller_agents::spawn::orchestrator::Orchestrator>, StartError>
    {
        Err(StartError::Refused {
            exit: 5,
            code: "stub_refused",
            message: "the stub starter refuses".into(),
            hint: None,
            current: None,
        })
    }

    fn doctor(&self) -> Option<crate::dto::DoctorReport> {
        use crate::dto::{CheckStatus, DoctorCheck, DoctorGate, DoctorReport};
        Some(DoctorReport {
            ok: false,
            checks: vec![DoctorCheck::missing(
                "gh",
                true,
                "gh is not on PATH of the peer",
            )],
            merge_gates: vec![DoctorGate {
                project: "alpha".into(),
                merge_gate: "detect".into(),
                status: CheckStatus::Warn,
                last_changed_at: None,
                detail: "detection only".into(),
            }],
        })
    }
}

/// `GET /api/v1/doctor?host=<peer>` is a `query` the peer answers with its
/// own checks, run there. Neither host writes anything for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_peers_checks_are_read_over_the_link() {
    let pair = pair();
    let wire = Wire::new(pair.peer.dir.path()).with_starter(Arc::new(PeerDoctor));
    let (peers, _stop) = start(pair.board.dir.path(), wire);
    until("the snapshot", || live_with_rows(&peers)).await;
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    let before = (last_seq(&pair.board), last_seq(&pair.peer));

    let answer = get(&app, "/api/v1/doctor?host=peer").await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let report: crate::dto::Doctor = serde_json::from_str(&answer.body).unwrap();
    assert!(!report.ok);
    assert_eq!(report.checks[0].detail, "gh is not on PATH of the peer");
    assert_eq!(report.merge_gates[0].project, "alpha");
    assert!(!report.checked_at.is_empty());
    assert_eq!((last_seq(&pair.board), last_seq(&pair.peer)), before);

    // This host by its label is its own checks (this test app has none to
    // run). A label that names no host is 404, never this host's checks.
    let own = get(&app, "/api/v1/doctor?host=test-host").await;
    assert_eq!(own.status, StatusCode::SERVICE_UNAVAILABLE, "{}", own.body);
    let error: ErrorBody = serde_json::from_str(&own.body).unwrap();
    assert_eq!(error.error.code, "unavailable");
    let missing = get(&app, "/api/v1/doctor?host=no-such-host").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.body);
}

/// A peer's binary whose checks hang until the test lets them finish, and
/// count how often they ran.
struct HeldDoctor {
    runs: Arc<AtomicUsize>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl OrchestratorStart for HeldDoctor {
    fn start(
        &self,
        store: &mut Store,
        ctx: &tiller_core::Ctx,
        project: &str,
        restart: bool,
    ) -> Result<tiller_core::Applied<tiller_agents::spawn::orchestrator::Orchestrator>, StartError>
    {
        PeerDoctor.start(store, ctx, project, restart)
    }

    fn doctor(&self) -> Option<crate::dto::DoctorReport> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let _ = self
            .release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(20));
        PeerDoctor.doctor()
    }
}

/// The peer runs its checks off the link: while they run (a hung tool can
/// take seconds), the link keeps beating and answers other queries. Two
/// `doctor` queries in flight share one run.
#[test]
fn a_peers_checks_run_off_the_link_one_run_at_a_time() {
    let pair = pair();
    let (release, held) = std::sync::mpsc::channel();
    let runs = Arc::new(AtomicUsize::new(0));
    let doctor = Arc::new(HeldDoctor {
        runs: runs.clone(),
        release: Mutex::new(held),
    });
    let mut link = SyncLink::on(serve_with_starter(
        pair.peer.dir.path(),
        "test-host",
        Some(doctor),
    ));
    link.send(&hello(&pair, None));
    assert!(matches!(link.next(), Frame::Welcome(_)));
    assert!(matches!(link.next(), Frame::Snapshot(_)));
    for id in ["d1", "d2"] {
        link.send(&Frame::Query {
            id: id.into(),
            path: "doctor".into(),
        });
    }
    link.send(&Frame::Query {
        id: "c1".into(),
        path: "cards/no-such-card".into(),
    });

    // The checks are held: the card query is answered and beats still come.
    let (mut beat, mut card) = (false, false);
    while !(beat && card) {
        match link.any() {
            Frame::Beat { .. } => beat = true,
            Frame::Result {
                id, http_status, ..
            } => {
                assert_eq!((id.as_str(), http_status), ("c1", 404));
                card = true;
            }
            Frame::Event(_) => {}
            frame => panic!("expected a beat or the card's result, got {frame:?}"),
        }
    }

    release.send(()).unwrap();
    let mut answered = Vec::new();
    while answered.len() < 2 {
        match link.any() {
            Frame::Result {
                id,
                http_status,
                body,
            } => {
                assert_eq!(http_status, 200, "{body}");
                assert_eq!(body["checks"][0]["detail"], "gh is not on PATH of the peer");
                answered.push(id);
            }
            Frame::Beat { .. } | Frame::Event(_) => {}
            frame => panic!("expected the checks, got {frame:?}"),
        }
    }
    answered.sort();
    assert_eq!(answered, ["d1", "d2"]);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

/// A peer that is not live is 503 `peer_unreachable` at once. A peer whose
/// binary has no checks to run says so in its own words.
#[tokio::test(flavor = "multi_thread")]
async fn a_peers_checks_need_a_live_peer_that_can_run_them() {
    let pair = pair();
    let up = Arc::new(AtomicBool::new(false));
    let switch = Switch {
        home: pair.peer.dir.path().to_owned(),
        up: up.clone(),
    };
    let (peers, _stop) = start(pair.board.dir.path(), switch);
    let app = app_with_peers(pair.board.dir.path(), peers.clone());
    until("the down view", || {
        peers.federation().peer("peer").map(|_| ())
    })
    .await;
    let down = get(&app, "/api/v1/doctor?host=peer").await;
    assert_eq!(
        down.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        down.body
    );
    let error: ErrorBody = serde_json::from_str(&down.body).unwrap();
    assert_eq!(error.error.code, "peer_unreachable");

    // `Switch` serves with no starter: the peer's own 503 comes back.
    up.store(true, Ordering::SeqCst);
    until("the snapshot", || live_with_rows(&peers)).await;
    let answer = get(&app, "/api/v1/doctor?host=peer").await;
    assert_eq!(
        answer.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        answer.body
    );
    let error: ErrorBody = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(error.error.code, "unavailable");
    assert!(
        error.error.message.contains("cannot run its checks"),
        "{}",
        answer.body
    );
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

// -- watching a peer's pane (`protocol.md` 10.7) --------------------------------------

/// `peer serve` over a socket pair for the link, and `peer pane` over an
/// in-memory pipe for each watch, on a peer whose herdr is [`mirror_herdr`].
struct PaneWire {
    home: PathBuf,
    herdr: tiller_herdr::client::Cli,
    /// The peer's terminal holder, for a peer holder pane.
    holder: Option<Arc<dyn crate::holder::Holder>>,
    /// Pane links dialed.
    dialed: Arc<AtomicUsize>,
}

impl Connector for PaneWire {
    fn connect(&self, target: &Target) -> Connecting {
        Wire::new(&self.home).connect(target)
    }

    fn connect_pane(&self, target: &Target) -> Connecting {
        self.dialed.fetch_add(1, Ordering::SeqCst);
        let config = ServeConfig {
            home: self.home.clone(),
            from: target.from.clone(),
            timing: quick(),
            herdr: Some(self.herdr.clone()),
            orchestrators: None,
            holder: self.holder.clone(),
        };
        let (board_out, peer_in) = tokio::io::duplex(1 << 20);
        let (peer_out, board_in) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            let _ = pane::serve(&config, tokio::io::BufReader::new(peer_in), peer_out).await;
        });
        Box::pin(async move {
            Ok(Connection {
                reader: Box::new(tokio::io::BufReader::new(board_in)),
                writer: Box::new(board_out),
                child: None,
            })
        })
    }
}

/// A paired board and peer with an agent in pane `w2:p1` on the peer, and
/// the board's app over live links. Returns the peer's session id.
async fn watching(
    scratch: &Path,
) -> (
    Pair,
    String,
    Router,
    String,
    Arc<AtomicUsize>,
    watch::Sender<bool>,
) {
    watching_on(scratch, None, |_| {}).await
}

/// [`watching`] with the agent in a pane of `holder` on the peer, when one
/// is given, and `prepare` run on the pair before the link starts.
async fn watching_on(
    scratch: &Path,
    holder: Option<&FakePanes>,
    prepare: impl FnOnce(&Pair),
) -> (
    Pair,
    String,
    Router,
    String,
    Arc<AtomicUsize>,
    watch::Sender<bool>,
) {
    let mut pair = pair();
    let pane = holder.map_or_else(
        || "w2:p1".to_owned(),
        |holder| holder_pane(holder, "s-1").as_str().to_owned(),
    );
    let session = pair.peer.worker(&pane);
    prepare(&pair);
    let dialed = Arc::new(AtomicUsize::new(0));
    let wire = PaneWire {
        home: pair.peer.dir.path().to_owned(),
        herdr: mirror_herdr(scratch),
        holder: holder.map(|holder| Arc::new(holder.clone()) as Arc<dyn crate::holder::Holder>),
        dialed: dialed.clone(),
    };
    let (peers, stop) = start(pair.board.dir.path(), wire);
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers);
    (pair, session, app, cookie, dialed, stop)
}

fn pane_path(session: &str, tail: &str) -> String {
    format!("/api/v1/agents/{session}/{tail}")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_pane_streams_to_the_board() {
    let scratch = tempfile::tempdir().unwrap();
    let (_pair, session, app, cookie, dialed, _stop) = watching(scratch.path()).await;
    let response = tower::ServiceExt::oneshot(
        app.clone(),
        mutation(
            "POST",
            &pane_path(&session, "terminal"),
            json!({"host": "peer"}),
            &[("cookie", cookie.as_str())],
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut pending = String::new();
    assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");
    assert_eq!(next_line(&mut body, &mut pending).await["full"], true);
    assert_eq!(dialed.load(Ordering::SeqCst), 1);
    // The peer only observed: it never sent a key or text to the pane.
    let calls = std::fs::read_to_string(scratch.path().join("calls")).unwrap();
    assert!(calls.contains("terminal session observe"), "{calls}");
    assert!(
        !calls.contains("send-keys") && !calls.contains("send-text"),
        "{calls}"
    );
    assert!(!scratch.path().join("controllers").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_pane_history_comes_from_the_peer() {
    let scratch = tempfile::tempdir().unwrap();
    let (_pair, session, app, cookie, _dialed, _stop) = watching(scratch.path()).await;
    let answer = send(
        &app,
        mutation(
            "POST",
            &pane_path(&session, "terminal/history"),
            json!({"host": "peer", "lines": 50}),
            &[("cookie", cookie.as_str())],
        ),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let body: Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(body["session_id"], session);
    assert_eq!(body["text"], "one\n\u{1b}[34mtwo\u{1b}[0m\n");
    let calls = std::fs::read_to_string(scratch.path().join("calls")).unwrap();
    assert!(
        calls.contains("pane read w2:p1 --source recent --lines 50"),
        "{calls}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pane_link_takes_no_forwarded_verb() {
    // An agent verb goes on the signed sync link as a `mutate` and its
    // reads as a `query` (7.4); a pane link carries neither, only signed
    // keys and the Fit (10.7).
    let mut pair = pair();
    let session = pair.peer.worker("w2:p1");
    let scratch = tempfile::tempdir().unwrap();
    let before = last_seq(&pair.peer);
    let verb = |op, body: Value| {
        let order = Order {
            op,
            target: Some(session.clone()),
            body,
            idempotency_key: "k-1".to_owned(),
        };
        Frame::Mutate(Box::new(Mutate::signed(
            &signer(&pair),
            &pair.peer.host,
            order,
            Timestamp::now(),
        )))
        .line()
        .unwrap()
    };
    let mut extra = verb(Op::Interrupt, json!({"idempotency_key": "k-1"}))
        + &verb(
            Op::PromptAnswer,
            json!({"idempotency_key": "k-1", "option": 1, "screen": "x"}),
        );
    for (id, tail) in [("q1", "terminal/tail"), ("q2", "prompt")] {
        extra += &Frame::Query {
            id: id.into(),
            path: format!("agents/{session}/{tail}"),
        }
        .line()
        .unwrap();
    }
    let lines = open_pane_lines(
        &pair,
        scratch.path(),
        pane_open(&pair, &session, PaneKind::Stream),
        &extra,
    )
    .await;
    assert!(lines[0].contains(r#""type":"size""#), "{lines:?}");
    assert!(
        !lines.iter().any(|line| line.contains(r#""type":"result""#)),
        "{lines:?}"
    );
    let calls = std::fs::read_to_string(scratch.path().join("calls")).unwrap();
    assert!(
        !calls.contains("send-keys") && !calls.contains("send-text"),
        "{calls}"
    );
    assert_eq!(last_seq(&pair.peer), before, "a pane link wrote");
    let keys: i64 = pair
        .peer
        .open()
        .conn()
        .query_row("SELECT count(*) FROM idempotency_keys", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(keys, 0);
}

/// [`open_pane`], with every line the peer sends until it is quiet for a
/// second.
async fn open_pane_lines(pair: &Pair, scratch: &Path, open: PaneOpen, extra: &str) -> Vec<String> {
    let config = ServeConfig {
        home: pair.peer.dir.path().to_owned(),
        from: "test-host".into(),
        timing: quick(),
        herdr: Some(mirror_herdr(scratch)),
        orchestrators: None,
        holder: None,
    };
    let (mut board_out, peer_in) = tokio::io::duplex(1 << 16);
    let (peer_out, board_in) = tokio::io::duplex(1 << 16);
    let serving = tokio::spawn(async move {
        let _ = pane::serve(&config, tokio::io::BufReader::new(peer_in), peer_out).await;
    });
    let line = Frame::PaneOpen(Box::new(open)).line().unwrap();
    board_out.write_all(line.as_bytes()).await.unwrap();
    board_out.write_all(extra.as_bytes()).await.unwrap();
    let mut lines = tokio::io::BufReader::new(board_in).lines();
    let mut seen = Vec::new();
    while let Ok(Ok(Some(line))) =
        tokio::time::timeout(Duration::from_secs(1), lines.next_line()).await
    {
        seen.push(line);
    }
    drop(board_out);
    let _ = tokio::time::timeout(Duration::from_secs(10), serving).await;
    seen
}

/// The owner's view of the peer agent `session`, opened with `body`.
async fn view_peer(
    app: &Router,
    cookie: &str,
    session: &str,
    body: Value,
) -> axum::body::BodyDataStream {
    let response = tower::ServiceExt::oneshot(
        app.clone(),
        mutation(
            "POST",
            &pane_path(session, "terminal"),
            body,
            &[("cookie", cookie)],
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.into_body().into_data_stream()
}

/// Sends `body` to the peer agent's `tail` as the owner.
async fn act_on_peer(app: &Router, cookie: &str, session: &str, tail: &str, body: Value) -> Answer {
    send(
        app,
        mutation(
            "POST",
            &pane_path(session, tail),
            body,
            &[("cookie", cookie)],
        ),
    )
    .await
}

/// The herdr calls that typed into a pane, in order.
fn typed(scratch: &Path) -> Vec<String> {
    std::fs::read_to_string(scratch.join("calls"))
        .unwrap_or_default()
        .lines()
        .filter(|call| call.contains("send-keys") || call.contains("send-text"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_typed_on_the_board_reach_a_peer_pane() {
    let scratch = tempfile::tempdir().unwrap();
    let (_pair, session, app, cookie, dialed, _stop) = watching(scratch.path()).await;
    let mut body = view_peer(&app, &cookie, &session, json!({"host": "peer"})).await;
    let mut pending = String::new();
    assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");

    for input in [
        json!({"host": "peer", "keys": ["l", "s"]}),
        json!({"host": "peer", "text": "echo hi"}),
        json!({"host": "peer", "keys": ["enter"]}),
    ] {
        let answer = act_on_peer(&app, &cookie, &session, "terminal/input", input).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        let sent: MutationResponse = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(sent.host.as_deref(), Some("peer"));
        assert_eq!(sent.data["session_id"], session);
    }
    // Through the peer's own herdr, on its own pane, in the order typed.
    let typed = typed(scratch.path());
    assert_eq!(typed.len(), 3, "{typed:?}");
    assert!(
        typed[0].contains("send-keys") && typed[0].ends_with("w2:p1 l s"),
        "{typed:?}"
    );
    assert!(
        typed[1].contains("send-text") && typed[1].contains("echo hi"),
        "{typed:?}"
    );
    assert!(typed[2].ends_with("w2:p1 enter"), "{typed:?}");
    assert_eq!(
        dialed.load(Ordering::SeqCst),
        1,
        "the keys rode the open link"
    );
}

/// The one pane in the peer's `holder`.
fn peer_pane_id(holder: &FakePanes) -> tiller_panes::PaneId {
    tiller_panes::Panes::list(holder).unwrap().panes[0]
        .pane
        .clone()
}

/// The next `size` line of a peer view, past its frames.
async fn next_size(body: &mut axum::body::BodyDataStream, pending: &mut String) -> Value {
    loop {
        let line = next_line(body, pending).await;
        if line["type"] != "frame" {
            return line;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_holder_pane_streams_and_takes_keys_over_the_pane_link() {
    let scratch = tempfile::tempdir().unwrap();
    let holder = FakePanes::new();
    let (_pair, session, app, cookie, _dialed, _stop) =
        watching_on(scratch.path(), Some(&holder), |_| {}).await;
    let pane = peer_pane_id(&holder);
    holder.set_screen(&pane, &["$ "]);
    let mut body = view_peer(&app, &cookie, &session, json!({"host": "peer"})).await;
    let mut pending = String::new();
    assert_eq!(
        next_line(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 120, "rows": 40})
    );
    assert_eq!(
        next_line(&mut body, &mut pending).await,
        json!({"type": "frame", "seq": 1, "full": true, "bytes": b64(b"$ ")})
    );
    // The peer's PTY writes come over the link as they come.
    assert!(holder.output(&pane, b"ls\r\n"));
    assert_eq!(
        next_line(&mut body, &mut pending).await,
        json!({"type": "frame", "seq": 2, "full": false, "bytes": b64(b"ls\r\n")})
    );
    for keys in [json!(["l", "s"]), json!(["enter"]), json!(["ctrl+c"])] {
        let answer = act_on_peer(
            &app,
            &cookie,
            &session,
            "terminal/input",
            json!({"host": "peer", "keys": keys}),
        )
        .await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    }
    assert_eq!(
        holder_calls(&holder, "keys"),
        [
            format!("keys {pane} l s"),
            format!("keys {pane} enter"),
            format!("keys {pane} ctrl+c"),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_holder_pane_history_comes_over_the_pane_link() {
    let scratch = tempfile::tempdir().unwrap();
    let holder = FakePanes::new();
    let (_pair, session, app, cookie, _dialed, _stop) =
        watching_on(scratch.path(), Some(&holder), |_| {}).await;
    let pane = peer_pane_id(&holder);
    holder.set_screen(&pane, &["one", "\u{1b}[34mtwo\u{1b}[0m", "$ "]);
    let answer = act_on_peer(
        &app,
        &cookie,
        &session,
        "terminal/history",
        json!({"host": "peer", "lines": 2}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let history: Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(history["session_id"], session);
    assert_eq!(history["text"], "\u{1b}[34mtwo\u{1b}[0m\n$ ");
    assert!(
        holder
            .calls()
            .contains(&format!("screen {pane} Recent(2) Ansi")),
        "{:?}",
        holder.calls()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn each_fit_on_a_peer_holder_pane_is_a_lease_and_the_size_falls_back() {
    let scratch = tempfile::tempdir().unwrap();
    let holder = FakePanes::new();
    let (_pair, session, app, cookie, _dialed, _stop) =
        watching_on(scratch.path(), Some(&holder), |_| {}).await;
    let pane = peer_pane_id(&holder);
    // An attach client on the peer holds the pane at its window's size.
    let attach = tiller_panes::Live::subscribe(&holder, &pane, Default::default()).unwrap();
    attach.resize(80, 24).unwrap();
    let leases = || {
        holder
            .leases(&pane)
            .into_iter()
            .map(|(_, cols, rows)| (cols, rows))
            .collect::<Vec<_>>()
    };

    let mut body = view_peer(
        &app,
        &cookie,
        &session,
        json!({"host": "peer", "fit": {"cols": 100, "rows": 30}}),
    )
    .await;
    let mut pending = String::new();
    assert_eq!(
        next_size(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 80, "rows": 24})
    );
    assert_eq!(
        next_size(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 100, "rows": 30})
    );
    assert_eq!(leases(), [(80, 24), (100, 30)]);

    // The board changes size: the Fit on the peer moves its lease.
    let refit = act_on_peer(
        &app,
        &cookie,
        &session,
        "terminal/fit",
        json!({"host": "peer", "cols": 110, "rows": 35}),
    )
    .await;
    assert_eq!(refit.status, StatusCode::OK, "{}", refit.body);
    assert_eq!(
        next_size(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 110, "rows": 35})
    );

    // A second viewer's Fit is a lease of its own, on top.
    let mut second = view_peer(
        &app,
        &cookie,
        &session,
        json!({"host": "peer", "fit": {"cols": 90, "rows": 20}}),
    )
    .await;
    let mut second_pending = String::new();
    assert_eq!(
        next_size(&mut second, &mut second_pending).await,
        json!({"type": "size", "cols": 110, "rows": 35})
    );
    assert_eq!(
        next_size(&mut second, &mut second_pending).await,
        json!({"type": "size", "cols": 90, "rows": 20})
    );
    assert_eq!(
        next_size(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 90, "rows": 20})
    );
    assert_eq!(leases(), [(80, 24), (110, 35), (90, 20)]);

    // A refit moves the newest Fit.
    let refit = act_on_peer(
        &app,
        &cookie,
        &session,
        "terminal/fit",
        json!({"host": "peer", "cols": 115, "rows": 38}),
    )
    .await;
    assert_eq!(refit.status, StatusCode::OK, "{}", refit.body);
    assert_eq!(
        next_size(&mut second, &mut second_pending).await,
        json!({"type": "size", "cols": 115, "rows": 38})
    );
    assert_eq!(
        next_size(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 115, "rows": 38})
    );
    assert_eq!(leases(), [(80, 24), (110, 35), (115, 38)]);

    // The second viewer leaves: the size falls back to the first's Fit,
    // and the first one's keys still reach the pane.
    drop(second);
    assert_eq!(
        next_size(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 110, "rows": 35})
    );
    let answer = act_on_peer(
        &app,
        &cookie,
        &session,
        "terminal/input",
        json!({"host": "peer", "keys": ["q"]}),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(holder_calls(&holder, "keys"), [format!("keys {pane} q")]);

    // The first viewer leaves: the size falls back to the attach client's.
    drop(body);
    let deadline = Instant::now() + Duration::from_secs(15);
    while leases() != [(80, 24)] {
        assert!(Instant::now() < deadline, "the Fit's lease never ended");
        tokio::time::sleep(ms(50)).await;
    }
    let size = tiller_panes::Panes::get(&holder, &pane).unwrap().unwrap();
    assert_eq!((size.cols, size.rows), (80, 24));
    drop(attach);
}

/// Turns every pairing on `fleet` to the `read` scope.
fn read_scope(fleet: &Fleet) {
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

#[tokio::test(flavor = "multi_thread")]
async fn a_read_pairing_cannot_type_into_or_fit_a_peer_pane() {
    // A `read` pairing on the board host never sends a key or a Fit.
    let scratch = tempfile::tempdir().unwrap();
    let holder = FakePanes::new();
    let (_pair, session, app, cookie, _dialed, _stop) =
        watching_on(scratch.path(), Some(&holder), |pair| {
            read_scope(&pair.board);
            read_scope(&pair.peer);
        })
        .await;
    // The view opens unfit, at the pane's own size.
    let mut body = view_peer(
        &app,
        &cookie,
        &session,
        json!({"host": "peer", "fit": {"cols": 100, "rows": 30}}),
    )
    .await;
    let mut pending = String::new();
    assert_eq!(next_line(&mut body, &mut pending).await["type"], "unfit");
    assert_eq!(
        next_line(&mut body, &mut pending).await,
        json!({"type": "size", "cols": 120, "rows": 40})
    );
    for (tail, input) in [
        (
            "terminal/input",
            json!({"host": "peer", "keys": ["ctrl+c"]}),
        ),
        (
            "terminal/fit",
            json!({"host": "peer", "cols": 100, "rows": 30}),
        ),
    ] {
        let answer = act_on_peer(&app, &cookie, &session, tail, input).await;
        let error = refusal(&answer, StatusCode::BAD_GATEWAY);
        assert_eq!(
            error.reason.as_deref(),
            Some("read_scope"),
            "{tail}: {}",
            answer.body
        );
        assert_eq!(error.host.as_deref(), Some("peer"));
    }
    assert!(holder_calls(&holder, "keys").is_empty());
    assert!(holder_calls(&holder, "resize").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_peer_checks_the_act_scope_again() {
    // The board host still thinks it may act; the peer has cut its key to
    // `read`, and refuses the keys itself. The cut comes before the link
    // starts: a cut while it runs ends the link at once (`serve.rs`), and
    // the board then answers unreachable.
    let scratch = tempfile::tempdir().unwrap();
    let (_pair, session, app, cookie, _dialed, _stop) = watching_on(scratch.path(), None, |pair| {
        pair.peer
            .open()
            .conn()
            .execute("UPDATE peer_keys SET scope = 'read'", [])
            .unwrap();
    })
    .await;
    let mut body = view_peer(&app, &cookie, &session, json!({"host": "peer"})).await;
    let mut pending = String::new();
    assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");
    let answer = act_on_peer(
        &app,
        &cookie,
        &session,
        "terminal/input",
        json!({"host": "peer", "keys": ["ctrl+c"]}),
    )
    .await;
    let error = refusal(&answer, StatusCode::BAD_GATEWAY);
    assert_eq!(
        error.reason.as_deref(),
        Some("read_scope"),
        "{}",
        answer.body
    );
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert!(typed(scratch.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fleet_caller_cannot_type_into_a_peer_pane() {
    let scratch = tempfile::tempdir().unwrap();
    let (_pair, session, app, cookie, _dialed, _stop) = watching(scratch.path()).await;
    // The owner watches, so a stream to type into is open.
    let mut body = view_peer(&app, &cookie, &session, json!({"host": "peer"})).await;
    let mut pending = String::new();
    assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");
    for headers in [vec![], vec![("cookie", "tiller_owner=forged")]] {
        for (tail, input) in [
            (
                "terminal/input",
                json!({"host": "peer", "keys": ["ctrl+c"]}),
            ),
            (
                "terminal/fit",
                json!({"host": "peer", "cols": 100, "rows": 30}),
            ),
        ] {
            let answer = send(
                &app,
                mutation("POST", &pane_path(&session, tail), input, &headers),
            )
            .await;
            assert_eq!(
                answer.status,
                StatusCode::FORBIDDEN,
                "{tail}: {}",
                answer.body
            );
        }
    }
    assert!(typed(scratch.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ended_session_takes_no_pane_input() {
    let scratch = tempfile::tempdir().unwrap();
    let (pair, session, app, cookie, _dialed, _stop) = watching(scratch.path()).await;
    let mut body = view_peer(&app, &cookie, &session, json!({"host": "peer"})).await;
    let mut pending = String::new();
    assert_eq!(next_line(&mut body, &mut pending).await["type"], "size");
    pair.peer
        .open()
        .session_end(
            &tiller_core::Ctx::new(tiller_core::Actor::Owner),
            &session,
            "exit",
        )
        .unwrap();
    let answer = act_on_peer(
        &app,
        &cookie,
        &session,
        "terminal/input",
        json!({"host": "peer", "keys": ["ctrl+c"]}),
    )
    .await;
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error.message.contains("has ended"), "{}", answer.body);
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert!(typed(scratch.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn pane_input_with_no_open_stream_is_refused_not_dropped() {
    let scratch = tempfile::tempdir().unwrap();
    let (_pair, session, app, cookie, dialed, _stop) = watching(scratch.path()).await;
    let answer = act_on_peer(
        &app,
        &cookie,
        &session,
        "terminal/input",
        json!({"host": "peer", "keys": ["ctrl+c"]}),
    )
    .await;
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error.code, "refused");
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert_eq!(dialed.load(Ordering::SeqCst), 0);
}

/// The board host's pane signer for `pair`.
fn pane_signer(pair: &Pair) -> PaneSigner {
    PaneSigner {
        key: pair.key.clone(),
        key_id: pair.key_id.clone(),
        viewer_host_id: pair.board.host.clone(),
        peer_host_id: pair.peer.host.clone(),
    }
}

/// Opens a stream on the peer's `pane::serve`, sends `frames` after the
/// open, and returns the `result` frames the peer answers, as
/// `(status, body)`.
async fn type_on_link(
    pair: &Pair,
    scratch: &Path,
    open: &PaneOpen,
    frames: &[Frame],
) -> Vec<(u16, Value)> {
    let config = ServeConfig {
        home: pair.peer.dir.path().to_owned(),
        from: "test-host".into(),
        timing: quick(),
        herdr: Some(mirror_herdr(scratch)),
        orchestrators: None,
        holder: None,
    };
    let (mut board_out, peer_in) = tokio::io::duplex(1 << 16);
    let (peer_out, board_in) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let _ = pane::serve(&config, tokio::io::BufReader::new(peer_in), peer_out).await;
    });
    let mut lines = Frame::PaneOpen(Box::new(open.clone())).line().unwrap();
    for frame in frames {
        lines += &frame.line().unwrap();
    }
    board_out.write_all(lines.as_bytes()).await.unwrap();
    let mut lines = tokio::io::BufReader::new(board_in).lines();
    let mut results = Vec::new();
    while results.len() < frames.len() {
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("the link ended before every frame was answered");
        if let Frame::Result {
            http_status, body, ..
        } = serde_json::from_str(&line).unwrap()
        {
            results.push((http_status, body));
        }
    }
    drop(board_out);
    results
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pane_link_refuses_forged_and_replayed_input() {
    let mut pair = pair();
    let session = pair.peer.worker("w2:p1");
    let scratch = tempfile::tempdir().unwrap();
    let open = pane_open(&pair, &session, PaneKind::Stream);
    let signer = pane_signer(&pair);
    let keys = |seq: u64, keys: &[&str]| {
        PaneInput::signed(
            &signer,
            &open.id,
            seq,
            &session,
            Ok(keys.iter().map(|key| (*key).to_owned()).collect()),
            Timestamp::now(),
        )
    };
    // Keys changed after signing, a stranger's signature, a replay, an older
    // seq and another stream's open: each is refused and none is typed.
    let mut forged = keys(1, &["l"]);
    forged.keys = Some(vec!["ctrl+c".into()]);
    let mut stranger = keys(2, &["l"]);
    stranger.sign(&generate_key().unwrap());
    let good = keys(3, &["l"]);
    let older = keys(2, &["s"]);
    let mut elsewhere = keys(4, &["s"]);
    elsewhere.open_id = "another-open".into();
    elsewhere.sign(&pair.key);
    let frames = [forged, stranger, good.clone(), good, older, elsewhere]
        .map(|frame| Frame::PaneInput(Box::new(frame)));
    let results = type_on_link(&pair, scratch.path(), &open, &frames).await;
    let reasons: Vec<_> = results
        .iter()
        .map(|(status, body)| (*status, body["error"]["reason"].as_str().map(str::to_owned)))
        .collect();
    assert_eq!(
        reasons,
        [
            (502, Some("bad_sig".to_owned())),
            (502, Some("bad_sig".to_owned())),
            (200, None),
            (502, Some("replayed".to_owned())),
            (502, Some("replayed".to_owned())),
            (502, Some("not_allowlisted".to_owned())),
        ],
        "{results:?}"
    );
    assert_eq!(typed(scratch.path()).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_pane_needs_the_owner_cookie() {
    let scratch = tempfile::tempdir().unwrap();
    let (_pair, session, app, _cookie, dialed, _stop) = watching(scratch.path()).await;
    for tail in [
        "terminal",
        "terminal/history",
        "terminal/input",
        "terminal/fit",
    ] {
        for headers in [vec![], vec![("cookie", "tiller_owner=forged")]] {
            let answer = send(
                &app,
                mutation(
                    "POST",
                    &pane_path(&session, tail),
                    json!({"host": "peer"}),
                    &headers,
                ),
            )
            .await;
            assert_eq!(
                answer.status,
                StatusCode::FORBIDDEN,
                "{tail}: {}",
                answer.body
            );
        }
    }
    assert_eq!(
        dialed.load(Ordering::SeqCst),
        0,
        "a pane link opened unauthenticated"
    );
    assert!(!scratch.path().join("calls").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_pane_on_a_down_link_says_unreachable() {
    let scratch = tempfile::tempdir().unwrap();
    let pair = {
        let mut pair = pair();
        pair.peer.worker("w2:p1");
        pair
    };
    let up = Arc::new(AtomicBool::new(false));
    let (peers, _stop) = start(
        pair.board.dir.path(),
        Switch {
            home: pair.peer.dir.path().to_owned(),
            up,
        },
    );
    let _ = scratch;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers);
    for tail in ["terminal", "terminal/history"] {
        let answer = send(
            &app,
            mutation(
                "POST",
                &pane_path("anything", tail),
                json!({"host": "peer"}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        let error = refusal(&answer, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code, "peer_unreachable", "{tail}");
        assert_eq!(error.host.as_deref(), Some("peer"));
    }
}

/// Opens a pane link on the peer's `pane::serve` with `open` and returns
/// the peer's lines, then the peer's end of herdr's calls.
async fn open_pane(pair: &Pair, scratch: &Path, open: PaneOpen, extra: &str) -> Vec<String> {
    let config = ServeConfig {
        home: pair.peer.dir.path().to_owned(),
        from: "test-host".into(),
        timing: quick(),
        herdr: Some(mirror_herdr(scratch)),
        orchestrators: None,
        holder: None,
    };
    let (mut board_out, peer_in) = tokio::io::duplex(1 << 16);
    let (peer_out, board_in) = tokio::io::duplex(1 << 16);
    let serving = tokio::spawn(async move {
        let _ = pane::serve(&config, tokio::io::BufReader::new(peer_in), peer_out).await;
    });
    let line = Frame::PaneOpen(Box::new(open)).line().unwrap();
    board_out.write_all(line.as_bytes()).await.unwrap();
    board_out.write_all(extra.as_bytes()).await.unwrap();
    let mut lines = tokio::io::BufReader::new(board_in).lines();
    let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    drop(board_out);
    let _ = tokio::time::timeout(Duration::from_secs(10), serving).await;
    first
}

fn pane_open(pair: &Pair, session: &str, kind: PaneKind) -> PaneOpen {
    PaneOpen::signed(
        &signer(pair),
        &pair.peer.host,
        session,
        kind,
        Some(10),
        None,
        Timestamp::now(),
    )
}

/// A revoke ends a pane stream that is open at once, not on the next tick:
/// the board host stops getting the pane's bytes as its link stops getting
/// frames.
#[tokio::test(flavor = "multi_thread")]
async fn a_revoke_ends_an_open_pane_stream_at_once() {
    let mut pair = pair();
    let session = pair.peer.worker("w2:p1");
    let scratch = tempfile::tempdir().unwrap();
    let config = ServeConfig {
        home: pair.peer.dir.path().to_owned(),
        from: "test-host".into(),
        timing: Timing {
            tick: Duration::from_secs(60),
            ..quick()
        },
        herdr: Some(mirror_herdr(scratch.path())),
        orchestrators: None,
        holder: None,
    };
    let (mut board_out, peer_in) = tokio::io::duplex(1 << 16);
    let (peer_out, board_in) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        let _ = pane::serve(&config, tokio::io::BufReader::new(peer_in), peer_out).await;
    });
    let open = Frame::PaneOpen(Box::new(pane_open(&pair, &session, PaneKind::Stream)));
    board_out
        .write_all(open.line().unwrap().as_bytes())
        .await
        .unwrap();
    let mut lines = tokio::io::BufReader::new(board_in).lines();
    let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap();
    assert!(first.is_some(), "the stream never began");

    pair.peer.open().peer_revoke(&owner(), "test-host").unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        while let Ok(Some(_)) = lines.next_line().await {}
    })
    .await;
    assert!(ended.is_ok(), "the revoked key's pane stream stayed open");
    drop(board_out);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pane_link_refuses_what_the_peer_would_refuse() {
    let mut pair = pair();
    let session = pair.peer.worker("w2:p1");
    let scratch = tempfile::tempdir().unwrap();
    let refused = |lines: &[String], reason: &str| {
        let frame: Frame = serde_json::from_str(&lines[0]).unwrap();
        match frame {
            Frame::Refused { reason: got, .. } => assert_eq!(got, reason),
            other => panic!("expected refused {reason}, got {other:?}"),
        }
    };

    // A signature over other fields.
    let mut forged = pane_open(&pair, &session, PaneKind::Stream);
    forged.session = "another".into();
    let lines = open_pane(&pair, scratch.path(), forged, "").await;
    refused(&lines, "bad_sig");

    // A request meant for another host.
    let mut elsewhere = pane_open(&pair, &session, PaneKind::Stream);
    elsewhere.peer_host_id = "h-other".into();
    let stranger = generate_key().unwrap();
    elsewhere.sign(&stranger);
    let lines = open_pane(&pair, scratch.path(), elsewhere, "").await;
    assert!(matches!(
        serde_json::from_str::<Frame>(&lines[0]).unwrap(),
        Frame::Refused { .. }
    ));

    // A revoked key.
    pair.peer
        .open()
        .conn()
        .execute(
            "UPDATE peer_keys SET revoked_at = ?1",
            [Timestamp::now().to_string()],
        )
        .unwrap();
    let lines = open_pane(
        &pair,
        scratch.path(),
        pane_open(&pair, &session, PaneKind::Stream),
        "",
    )
    .await;
    assert!(matches!(
        serde_json::from_str::<Frame>(&lines[0]).unwrap(),
        Frame::Refused { .. }
    ));
    assert!(
        !scratch.path().join("calls").exists(),
        "a refused link reached herdr"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pane_link_takes_no_mutate_and_no_unsigned_keys() {
    let mut pair = pair();
    let session = pair.peer.worker("w2:p1");
    let scratch = tempfile::tempdir().unwrap();
    // After the open, the board host sends a mutate and bare keys: the link
    // reads neither as input, only a signed `pane_input`.
    let card = {
        add_card(&pair.peer, "a peer card");
        card_id(&pair.peer, "a peer card")
    };
    let before = last_seq(&pair.peer);
    let extra = Frame::Mutate(Box::new(comment(&pair, &card, "k-1")))
        .line()
        .unwrap()
        + "{\"type\":\"keys\",\"keys\":[\"ctrl+c\"]}\n";
    let lines = open_pane(
        &pair,
        scratch.path(),
        pane_open(&pair, &session, PaneKind::Stream),
        &extra,
    )
    .await;
    assert!(lines[0].contains(r#""type":"size""#), "{lines:?}");
    let calls = std::fs::read_to_string(scratch.path().join("calls")).unwrap();
    assert!(
        !calls.contains("send-keys") && !calls.contains("send-text"),
        "{calls}"
    );
    assert!(!scratch.path().join("controllers").exists());
    assert_eq!(
        last_seq(&pair.peer),
        before,
        "a pane link wrote to the peer"
    );
}

#[test]
fn a_pane_link_refuses_an_open_in_place_of_the_first_frame() {
    // Any first frame but `pane_open` gets no answer and no herdr call.
    let pair = pair();
    let scratch = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let config = ServeConfig {
        home: pair.peer.dir.path().to_owned(),
        from: "test-host".into(),
        timing: quick(),
        herdr: Some(mirror_herdr(scratch.path())),
        orchestrators: None,
        holder: None,
    };
    let hello = hello(&pair, None).line().unwrap();
    let mut output = Vec::new();
    runtime
        .block_on(pane::serve(
            &config,
            tokio::io::BufReader::new(std::io::Cursor::new(hello)),
            &mut output,
        ))
        .unwrap();
    assert!(output.is_empty());
    assert!(!scratch.path().join("calls").exists());
}

// -- agent verbs on a peer (D20) ------------------------------------------------------

/// A peer's starter for the agent verbs: it records each stop, resume and
/// relaunch it is asked for, as `(verb, actor, key)`, and answers it done,
/// or refuses it while `refuse` is set.
#[derive(Default)]
struct AgentStarter {
    seen: Mutex<Vec<(String, String, Option<String>)>>,
    refuse: AtomicBool,
}

impl AgentStarter {
    fn seen(&self) -> Vec<(String, String, Option<String>)> {
        self.seen.lock().unwrap().clone()
    }

    fn answer(&self, verb: String, ctx: &tiller_core::Ctx) -> Result<Controlled, StartError> {
        self.seen
            .lock()
            .unwrap()
            .push((verb.clone(), ctx.actor.to_string(), ctx.key.clone()));
        if self.refuse.load(Ordering::SeqCst) {
            return Err(StartError::Refused {
                exit: 5,
                code: "stub_refused",
                message: format!("the stub refuses the {verb}"),
                hint: None,
                current: None,
            });
        }
        Ok(Controlled {
            applied: tiller_core::Applied {
                value: json!({"verb": verb}),
                events: Vec::new(),
                replayed: false,
            },
            warning: None,
        })
    }
}

impl OrchestratorStart for AgentStarter {
    fn start(
        &self,
        _store: &mut Store,
        _ctx: &tiller_core::Ctx,
        _project: &str,
        _restart: bool,
    ) -> Result<tiller_core::Applied<tiller_agents::spawn::orchestrator::Orchestrator>, StartError>
    {
        Err(StartError::Refused {
            exit: 5,
            code: "stub_refused",
            message: "the stub starts no orchestrator".into(),
            hint: None,
            current: None,
        })
    }

    fn stop_agent(
        &self,
        _store: &mut Store,
        ctx: &tiller_core::Ctx,
        _session: &Session,
    ) -> Result<Controlled, StartError> {
        self.answer("stop".into(), ctx)
    }

    fn resume_run(
        &self,
        _store: &mut Store,
        ctx: &tiller_core::Ctx,
        _run: &Run,
    ) -> Result<Controlled, StartError> {
        self.answer("resume".into(), ctx)
    }

    fn relaunch_run(
        &self,
        _store: &mut Store,
        ctx: &tiller_core::Ctx,
        _run: &Run,
        relaunch: BoardRelaunch,
    ) -> Result<Controlled, StartError> {
        self.answer(format!("relaunch: {}", relaunch.note), ctx)
    }
}

/// A paired board and peer with a builder in a terminal holder pane on the
/// peer, and the board's app over a live link whose `peer serve` holds that
/// holder and an [`AgentStarter`].
struct PeerAgent {
    pair: Pair,
    session: String,
    holder: FakePanes,
    pane: tiller_panes::PaneId,
    starter: Arc<AgentStarter>,
    app: Router,
    cookie: String,
    /// Every byte the board host sent on the link.
    sent: Arc<Mutex<Vec<u8>>>,
    /// The next `result` frames lost on the way back.
    lose_results: Arc<AtomicUsize>,
    _scratch: tempfile::TempDir,
    _stop: watch::Sender<bool>,
}

/// [`PeerAgent`] on the board's `timing`, with `prepare` run on the pair
/// before the link starts.
async fn peer_agent(timing: Timing, prepare: impl FnOnce(&Pair)) -> PeerAgent {
    let mut pair = pair();
    let holder = FakePanes::new();
    let pane = holder_pane(&holder, "s-1");
    let session = pair.peer.worker(pane.as_str());
    prepare(&pair);
    let scratch = tempfile::tempdir().unwrap();
    let starter = Arc::new(AgentStarter::default());
    let wire = Wire::new(pair.peer.dir.path())
        .with_starter(starter.clone())
        .with_panes(mirror_herdr(scratch.path()), &holder);
    let (sent, lose_results) = (wire.sent.clone(), wire.lose_results.clone());
    let (peers, stop) = start_timed(pair.board.dir.path(), wire, timing);
    until("the snapshot", || live_with_rows(&peers)).await;
    let cookie = owner_cookie(pair.board.dir.path());
    let app = app_with_peers(pair.board.dir.path(), peers);
    PeerAgent {
        pair,
        session,
        holder,
        pane,
        starter,
        app,
        cookie,
        sent,
        lose_results,
        _scratch: scratch,
        _stop: stop,
    }
}

impl PeerAgent {
    /// The owner's `body` to the peer agent's `tail`.
    async fn post(&self, tail: &str, body: Value) -> Answer {
        act_on_peer(&self.app, &self.cookie, &self.session, tail, body).await
    }

    /// Draws Claude's permission dialog for a Bash call on the peer's pane,
    /// with the agent waiting on it.
    fn ask_permission(&self) {
        waiting(&self.pair.peer, &self.session, "Bash");
        self.draw_permission();
    }

    /// Draws the permission dialog again, with the agent still waiting.
    fn draw_permission(&self) {
        self.set_screen(include_str!(
            "../../../panes/tests/fixtures/claude/permission-dialog.ansi"
        ));
    }

    fn set_screen(&self, ansi: &str) {
        let rows: Vec<&str> = ansi.lines().collect();
        self.holder.set_screen(&self.pane, &rows);
    }

    /// The keys typed into the peer's pane.
    fn keys(&self) -> Vec<String> {
        holder_calls(&self.holder, "keys")
    }

    /// The `mutate` and `query` frames the board host sent, as `type op-or-path`.
    fn asked(&self) -> Vec<String> {
        wire_text(&self.sent)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|frame| match frame["type"].as_str()? {
                "mutate" => Some(format!("mutate {}", frame["op"].as_str()?)),
                "query" => Some(format!("query {}", frame["path"].as_str()?)),
                _ => None,
            })
            .collect()
    }
}

/// The verbs' endpoints and bodies, each with its own key.
fn verbs() -> Vec<(&'static str, Value)> {
    let until = Timestamp::now().plus_minutes(60).to_string();
    vec![
        (
            "interrupt",
            json!({"idempotency_key": "k-int", "host": "peer"}),
        ),
        (
            "steer",
            json!({"idempotency_key": "k-steer", "host": "peer", "text": "use the new API"}),
        ),
        ("stop", json!({"idempotency_key": "k-stop", "host": "peer"})),
        (
            "pause",
            json!({"idempotency_key": "k-pause", "host": "peer", "until": until, "reason": "waits on review"}),
        ),
        (
            "unpause",
            json!({"idempotency_key": "k-unpause", "host": "peer"}),
        ),
        (
            "resume",
            json!({"idempotency_key": "k-resume", "host": "peer"}),
        ),
        (
            "relaunch",
            json!({"idempotency_key": "k-relaunch", "host": "peer", "note": "start from the tests"}),
        ),
        (
            "prompt/answer",
            json!({"idempotency_key": "k-answer", "host": "peer", "option": 1, "screen": "x"}),
        ),
    ]
}

/// Claude's Stop hook on the peer once an Escape reached `holder`, as the
/// agent's turn ends after an interrupt.
fn stop_hook_after_escape(
    fleet: &Fleet,
    holder: &FakePanes,
    session: &str,
) -> std::thread::JoinHandle<()> {
    let mut store = fleet.open();
    let holder = holder.clone();
    let session = session.to_owned();
    std::thread::spawn(move || {
        for _ in 0..1000 {
            if holder_calls(&holder, "keys")
                .iter()
                .any(|call| call.ends_with(" esc"))
            {
                store
                    .observation_record(
                        &tiller_core::Ctx::new(tiller_core::Actor::Hook),
                        tiller_core::state::runs::NewObservation {
                            session_id: Some(session),
                            run_id: None,
                            source: tiller_core::domain::ObservationSource::Hook,
                            event: "Stop".into(),
                            verdict: None,
                            observed_at: Timestamp::now(),
                            payload: json!({}),
                        },
                    )
                    .unwrap();
                return;
            }
            std::thread::sleep(ms(10));
        }
    })
}

const WORKING: &str = include_str!("../../../panes/tests/fixtures/claude/working.ansi");

#[tokio::test(flavor = "multi_thread")]
async fn the_board_reads_a_peer_agents_tail() {
    // A `read` pairing may read a tail too (G46).
    for scope in [PeerScope::Act, PeerScope::Read] {
        let agent = peer_agent(quick(), |pair| {
            if scope == PeerScope::Read {
                read_scope(&pair.board);
                read_scope(&pair.peer);
            }
        })
        .await;
        agent.holder.set_screen(&agent.pane, &["one", "two", "$ "]);
        let tail = agent.post("terminal/tail", json!({"host": "peer"})).await;
        assert_eq!(tail.status, StatusCode::OK, "{scope:?}: {}", tail.body);
        let tail: Value = serde_json::from_str(&tail.body).unwrap();
        assert_eq!(tail["session_id"], agent.session);
        assert_eq!(
            (tail["cols"].as_u64(), tail["rows"].as_u64()),
            (Some(120), Some(40)),
            "{tail}"
        );
        assert_eq!(
            agent.asked(),
            [format!("query agents/{}/terminal/tail", agent.session)]
        );
        // Read off the peer's holder: nothing was typed.
        assert!(agent.keys().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_board_reads_and_answers_a_peer_agents_prompt() {
    // From the stage or the Inbox alike: no stream is open, and no pane
    // link is dialed.
    let agent = peer_agent(quick(), |_| {}).await;
    let idle = agent.post("prompt", json!({"host": "peer"})).await;
    assert_eq!(idle.status, StatusCode::OK, "{}", idle.body);
    let idle: Value = serde_json::from_str(&idle.body).unwrap();
    assert_eq!(idle["reason"], "not_waiting");

    agent.ask_permission();
    let shown = agent.post("prompt", json!({"host": "peer"})).await;
    assert_eq!(shown.status, StatusCode::OK, "{}", shown.body);
    let shown: Value = serde_json::from_str(&shown.body).unwrap();
    assert_eq!(shown["session_id"], agent.session);
    assert_eq!(shown["reason"], Value::Null, "{shown}");
    assert_eq!(
        shown["prompt"]["command"],
        "rm -f nothing-here.txt && touch made.txt"
    );
    let screen = shown["prompt"]["screen"].as_str().unwrap();

    let sent = agent
        .post(
            "prompt/answer",
            json!({"idempotency_key": "k-1", "host": "peer", "option": 4, "screen": screen}),
        )
        .await;
    let sent = applied(&sent);
    assert_eq!(sent.host.as_deref(), Some("peer"));
    assert!(!sent.replayed);
    assert_eq!(sent.data["sent"]["option"], 4);
    assert_eq!(agent.keys(), [format!("keys {} 4", agent.pane)]);
    // The prompt read is a query and the answer a forwarded mutation.
    let session = &agent.session;
    assert_eq!(
        agent.asked(),
        [
            format!("query agents/{session}/prompt"),
            format!("query agents/{session}/prompt"),
            "mutate POST /api/v1/agents/{session}/prompt/answer".to_owned(),
        ]
    );
    // A peer answer needs its key, so a retry types nothing twice.
    let keyless = agent
        .post(
            "prompt/answer",
            json!({"host": "peer", "option": 4, "screen": screen}),
        )
        .await;
    assert_eq!(keyless.status, StatusCode::BAD_REQUEST, "{}", keyless.body);
    assert_eq!(agent.keys().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_answer_on_a_changed_dialog_is_screen_changed() {
    let agent = peer_agent(quick(), |_| {}).await;
    agent.ask_permission();
    let shown = agent.post("prompt", json!({"host": "peer"})).await;
    let shown: Value = serde_json::from_str(&shown.body).unwrap();
    let screen = shown["prompt"]["screen"].as_str().unwrap().to_owned();

    // The peer's dialog moved on after the buttons were drawn here.
    agent.set_screen(WORKING);
    let stale = agent
        .post(
            "prompt/answer",
            json!({"idempotency_key": "k-1", "host": "peer", "option": 1, "screen": screen}),
        )
        .await;
    let error = refusal(&stale, StatusCode::CONFLICT);
    assert_eq!(error.code, "screen_changed", "{}", stale.body);
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert!(agent.keys().is_empty());

    // So did a screen string the peer never drew.
    agent.draw_permission();
    let forged = agent
        .post(
            "prompt/answer",
            json!({"idempotency_key": "k-2", "host": "peer", "option": 1, "screen": "other"}),
        )
        .await;
    assert_eq!(
        refusal(&forged, StatusCode::CONFLICT).code,
        "screen_changed"
    );
    assert!(agent.keys().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_pairing_cannot_read_or_answer_a_peer_prompt() {
    // The board host's own pairing is `read`: nothing goes out.
    let agent = peer_agent(quick(), |pair| {
        read_scope(&pair.board);
        read_scope(&pair.peer);
    })
    .await;
    agent.ask_permission();
    for (tail, body) in [
        ("prompt", json!({"host": "peer"})),
        (
            "prompt/answer",
            json!({"idempotency_key": "k-1", "host": "peer", "option": 1, "screen": "x"}),
        ),
    ] {
        let answer = agent.post(tail, body).await;
        let error = refusal(&answer, StatusCode::BAD_GATEWAY);
        assert_eq!(error.reason.as_deref(), Some("read_scope"), "{tail}");
        assert_eq!(error.host.as_deref(), Some("peer"));
    }
    assert!(agent.asked().is_empty(), "{:?}", agent.asked());

    // The board host still thinks it may act; the peer's key row says
    // `read`, and the peer refuses the read itself.
    let agent = peer_agent(quick(), |pair| {
        pair.peer
            .open()
            .conn()
            .execute("UPDATE peer_keys SET scope = 'read'", [])
            .unwrap();
    })
    .await;
    agent.ask_permission();
    let read = agent.post("prompt", json!({"host": "peer"})).await;
    let error = refusal(&read, StatusCode::BAD_GATEWAY);
    assert_eq!(error.reason.as_deref(), Some("read_scope"), "{}", read.body);
    assert_eq!(error.host.as_deref(), Some("peer"));
    let answer = agent
        .post(
            "prompt/answer",
            json!({"idempotency_key": "k-1", "host": "peer", "option": 1, "screen": "x"}),
        )
        .await;
    let error = refusal(&answer, StatusCode::BAD_GATEWAY);
    assert_eq!(
        error.reason.as_deref(),
        Some("read_scope"),
        "{}",
        answer.body
    );
    assert!(agent.keys().is_empty());
    // A read spends no input: the peer read no screen for it.
    assert!(
        !agent
            .holder
            .calls()
            .iter()
            .any(|call| call.starts_with("screen")),
        "{:?}",
        agent.holder.calls()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_peer_answer_types_nothing_twice() {
    let agent = peer_agent(quick(), |_| {}).await;
    agent.ask_permission();
    let shown = agent.post("prompt", json!({"host": "peer"})).await;
    let shown: Value = serde_json::from_str(&shown.body).unwrap();
    let screen = shown["prompt"]["screen"].as_str().unwrap().to_owned();
    let answer = json!({"idempotency_key": "k-1", "host": "peer", "option": 4, "screen": screen});

    // The peer typed, but its result was lost on the way back.
    agent.lose_results.store(1, Ordering::SeqCst);
    let lost = agent.post("prompt/answer", answer.clone()).await;
    let error = refusal(&lost, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(error.code, "forward_uncertain", "{}", lost.body);
    assert_eq!(agent.keys().len(), 1);

    // The dialog is up again, the same one: a retry with the same key gets
    // the first answer back and types nothing.
    agent.draw_permission();
    let retry = agent.post("prompt/answer", answer).await;
    let retry = applied(&retry);
    assert!(retry.replayed);
    assert_eq!(retry.host.as_deref(), Some("peer"));
    assert_eq!(retry.data["sent"]["option"], 4);
    assert_eq!(agent.keys(), [format!("keys {} 4", agent.pane)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_board_interrupts_steers_and_stops_a_peer_agent() {
    let agent = peer_agent(quick(), |_| {}).await;
    let session = agent.session.clone();
    agent.set_screen(WORKING);
    let hook = stop_hook_after_escape(&agent.pair.peer, &agent.holder, &session);
    let interrupted = agent
        .post(
            "interrupt",
            json!({"idempotency_key": "k-int", "host": "peer"}),
        )
        .await;
    hook.join().unwrap();
    let interrupted = applied(&interrupted);
    assert_eq!(interrupted.host.as_deref(), Some("peer"));
    assert_eq!(
        interrupted.data["cancel"], "confirmed",
        "{:?}",
        interrupted.data
    );
    assert_eq!(interrupted.data["session"], session);
    assert_eq!(agent.keys(), [format!("keys {} esc", agent.pane)]);

    let steered = agent
        .post(
            "steer",
            json!({"idempotency_key": "k-steer", "host": "peer", "text": "use the new API"}),
        )
        .await;
    let steered = applied(&steered);
    assert_eq!(steered.host.as_deref(), Some("peer"));
    let steers = |fleet: &Fleet| -> i64 {
        fleet
            .open()
            .conn()
            .query_row(
                "SELECT count(*) FROM outbox WHERE kind = 'steer' AND body = 'use the new API'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(steers(&agent.pair.peer), 1);
    assert_eq!(steers(&agent.pair.board), 0);

    let stopped = agent
        .post("stop", json!({"idempotency_key": "k-stop", "host": "peer"}))
        .await;
    let stopped = applied(&stopped);
    assert_eq!(stopped.host.as_deref(), Some("peer"));
    assert_eq!(stopped.data["verb"], "stop");
    assert_eq!(
        agent.starter.seen(),
        [(
            "stop".to_owned(),
            "board@test-host".to_owned(),
            Some("k-stop".to_owned())
        )]
    );
    assert_eq!(
        agent.asked(),
        [
            "mutate POST /api/v1/agents/{session}/interrupt",
            "mutate POST /api/v1/agents/{session}/steer",
            "mutate POST /api/v1/agents/{session}/stop",
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_board_pauses_resumes_and_relaunches_a_peer_run() {
    let agent = peer_agent(quick(), |_| {}).await;
    let session = agent.session.clone();
    let paused_until = |fleet: &Fleet| -> Option<String> {
        fleet
            .open()
            .conn()
            .query_row(
                "SELECT pause_until FROM runs WHERE id = (SELECT run_id FROM sessions WHERE id = ?1)",
                [&session],
                |row| row.get(0),
            )
            .unwrap()
    };
    let until = Timestamp::now().plus_minutes(60).to_string();
    let paused = agent
        .post(
            "pause",
            json!({"idempotency_key": "k-pause", "host": "peer", "until": until, "reason": "waits on review"}),
        )
        .await;
    assert_eq!(applied(&paused).host.as_deref(), Some("peer"));
    assert!(paused_until(&agent.pair.peer).is_some());

    let unpaused = agent
        .post(
            "unpause",
            json!({"idempotency_key": "k-unpause", "host": "peer"}),
        )
        .await;
    assert_eq!(applied(&unpaused).host.as_deref(), Some("peer"));
    assert_eq!(paused_until(&agent.pair.peer), None);

    for (tail, body) in [
        (
            "resume",
            json!({"idempotency_key": "k-resume", "host": "peer"}),
        ),
        (
            "relaunch",
            json!({"idempotency_key": "k-relaunch", "host": "peer", "note": "start from the tests"}),
        ),
    ] {
        let answer = agent.post(tail, body).await;
        assert_eq!(applied(&answer).host.as_deref(), Some("peer"), "{tail}");
    }
    assert_eq!(
        agent.starter.seen(),
        [
            (
                "resume".to_owned(),
                "board@test-host".to_owned(),
                Some("k-resume".to_owned())
            ),
            (
                "relaunch: start from the tests".to_owned(),
                "board@test-host".to_owned(),
                Some("k-relaunch".to_owned())
            ),
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_board_withdraws_a_peer_attention_item() {
    let agent = peer_agent(quick(), |pair| {
        // An item about a session that is gone: it no longer applies.
        add_card(&pair.peer, "a peer card");
        pair.peer
            .open()
            .conn()
            .execute(
                "INSERT INTO attention (id, card_id, kind, question, opened_at, subject_session_id)
                 SELECT '01M7AEQD3P56M1JB0XWF6BRAT1', id, 'recovery', 'Backup failed', ?1, 'gone' FROM cards LIMIT 1",
                [Timestamp::now().to_string()],
            )
            .unwrap();
    })
    .await;
    let path = "/api/v1/attention/01M7AEQD3P56M1JB0XWF6BRAT1/withdraw";
    let body = json!({"idempotency_key": "k-1", "host": "peer"});
    let withdrawn = send(
        &agent.app,
        mutation("POST", path, body.clone(), &[("cookie", &agent.cookie)]),
    )
    .await;
    let withdrawn = applied(&withdrawn);
    assert_eq!(withdrawn.host.as_deref(), Some("peer"));
    let withdrawn_at = |fleet: &Fleet| -> Option<String> {
        fleet
            .open()
            .conn()
            .query_row(
                "SELECT withdrawn_at FROM attention WHERE id = '01M7AEQD3P56M1JB0XWF6BRAT1'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(None)
    };
    assert!(withdrawn_at(&agent.pair.peer).is_some());
    assert_eq!(withdrawn_at(&agent.pair.board), None);
    // The same key again is the stored answer.
    let again = send(
        &agent.app,
        mutation("POST", path, body, &[("cookie", &agent.cookie)]),
    )
    .await;
    assert!(applied(&again).replayed);
    assert_eq!(
        agent.asked(),
        ["mutate POST /api/v1/attention/{id}/withdraw"; 2]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_pairing_cannot_send_an_agent_verb() {
    let agent = peer_agent(quick(), |pair| {
        read_scope(&pair.board);
        read_scope(&pair.peer);
    })
    .await;
    agent.set_screen(WORKING);
    let before = last_seq(&agent.pair.peer);
    for (tail, body) in verbs() {
        let answer = agent.post(tail, body).await;
        let error = refusal(&answer, StatusCode::BAD_GATEWAY);
        assert_eq!(error.reason.as_deref(), Some("read_scope"), "{tail}");
        assert_eq!(error.host.as_deref(), Some("peer"), "{tail}");
    }
    // Nothing went out, but it may still tail.
    assert!(agent.asked().is_empty(), "{:?}", agent.asked());
    let tail = agent.post("terminal/tail", json!({"host": "peer"})).await;
    assert_eq!(tail.status, StatusCode::OK, "{}", tail.body);

    // The peer refuses a signed verb from a `read` key itself.
    let mut link = linked(&agent.pair);
    for (op, body) in [
        (Op::Interrupt, json!({"idempotency_key": "k-1"})),
        (Op::Stop, json!({"idempotency_key": "k-1"})),
        (
            Op::Steer,
            json!({"idempotency_key": "k-1", "text": "use the new API"}),
        ),
    ] {
        let order = Order {
            op,
            target: Some(agent.session.clone()),
            body,
            idempotency_key: "k-1".to_owned(),
        };
        let frame = Mutate::signed(
            &signer(&agent.pair),
            &agent.pair.peer.host,
            order,
            Timestamp::now(),
        );
        assert_refused(&agent.pair, link.mutate(frame), "read_scope", before);
    }
    assert!(agent.keys().is_empty());
    assert!(agent.starter.seen().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fleet_caller_cannot_send_a_peer_agent_verb() {
    let agent = peer_agent(quick(), |_| {}).await;
    agent.ask_permission();
    let mut endpoints = verbs();
    endpoints.push(("prompt", json!({"host": "peer"})));
    endpoints.push(("terminal/tail", json!({"host": "peer"})));
    for headers in [vec![], vec![("cookie", "tiller_owner=forged")]] {
        for (tail, body) in &endpoints {
            let answer = send(
                &agent.app,
                mutation(
                    "POST",
                    &pane_path(&agent.session, tail),
                    body.clone(),
                    &headers,
                ),
            )
            .await;
            assert_eq!(
                answer.status,
                StatusCode::FORBIDDEN,
                "{tail}: {}",
                answer.body
            );
        }
    }
    assert!(agent.asked().is_empty(), "{:?}", agent.asked());
    assert!(agent.keys().is_empty());
    assert!(agent.starter.seen().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forwarded_agent_verb_records_the_forwarded_owner() {
    let agent = peer_agent(quick(), |_| {}).await;
    let before = last_seq(&agent.pair.peer);
    let board_before = last_seq(&agent.pair.board);
    let until = Timestamp::now().plus_minutes(60).to_string();
    for (tail, body) in [
        (
            "pause",
            json!({"idempotency_key": "k-pause", "host": "peer", "until": until, "reason": "waits on review"}),
        ),
        (
            "steer",
            json!({"idempotency_key": "k-steer", "host": "peer", "text": "use the new API"}),
        ),
    ] {
        applied(&agent.post(tail, body).await);
    }
    // Written on the peer as the forwarded owner, with the key that signed
    // it; nothing on the board host.
    assert_eq!(last_seq(&agent.pair.board), board_before);
    let store = agent.pair.peer.open();
    let mut statement = store
        .conn()
        .prepare("SELECT kind, actor, payload FROM events WHERE seq > ?1")
        .unwrap();
    let events: Vec<(String, String, String)> = statement
        .query_map([before], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        events.iter().any(|(kind, ..)| kind == "run.paused"),
        "{events:?}"
    );
    assert!(events.len() >= 2, "{events:?}");
    for (kind, actor, payload) in events {
        assert_eq!(actor, "board@test-host", "{kind}");
        let payload: Value = serde_json::from_str(&payload).unwrap();
        let via = &payload["via_peer"];
        assert_eq!(
            via["key_id"],
            Value::from(agent.pair.key_id.clone()),
            "{kind}: {payload}"
        );
        assert_eq!(via["viewer_label"], "test-host");
        assert!(via["frame_id"].is_string(), "{payload}");
    }
    // The starter's verbs get the same actor and key.
    applied(
        &agent
            .post("stop", json!({"idempotency_key": "k-stop", "host": "peer"}))
            .await,
    );
    assert_eq!(
        agent.starter.seen(),
        [(
            "stop".to_owned(),
            "board@test-host".to_owned(),
            Some("k-stop".to_owned())
        )]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forwarded_agent_verb_passes_the_peers_refusal_back() {
    let agent = peer_agent(quick(), |_| {}).await;
    let before = last_seq(&agent.pair.peer);
    // The peer's store refuses a pause that is not in the future.
    let past = Timestamp::now().plus_minutes(-60).to_string();
    let paused = agent
        .post(
            "pause",
            json!({"idempotency_key": "k-pause", "host": "peer", "until": past, "reason": "waits on review"}),
        )
        .await;
    let error = refusal(&paused, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error.message.contains("is not in the future"),
        "{}",
        paused.body
    );
    assert_eq!(error.host.as_deref(), Some("peer"));

    // The peer's controls refuse an interrupt of an idle agent.
    agent.set_screen(include_str!(
        "../../../panes/tests/fixtures/claude/empty.ansi"
    ));
    let idle = agent
        .post(
            "interrupt",
            json!({"idempotency_key": "k-int", "host": "peer"}),
        )
        .await;
    let error = refusal(&idle, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error.message.contains("is not working"), "{}", idle.body);
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert!(agent.keys().is_empty());

    // And its starter's refusal comes back as its own.
    agent.starter.refuse.store(true, Ordering::SeqCst);
    let stopped = agent
        .post("stop", json!({"idempotency_key": "k-stop", "host": "peer"}))
        .await;
    let error = refusal(&stopped, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error.code, "stub_refused");
    assert_eq!(error.host.as_deref(), Some("peer"));

    // An unknown agent is the peer's 404.
    let missing = act_on_peer(
        &agent.app,
        &agent.cookie,
        "nope",
        "stop",
        json!({"idempotency_key": "k-nope", "host": "peer"}),
    )
    .await;
    let error = refusal(&missing, StatusCode::NOT_FOUND);
    assert_eq!(error.host.as_deref(), Some("peer"));
    assert_eq!(last_seq(&agent.pair.peer), before, "a refusal wrote");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forwarded_relaunch_starts_its_agent_on_the_owning_host_only() {
    let agent = peer_agent(quick(), |_| {}).await;
    let board_before = last_seq(&agent.pair.board);
    let relaunched = agent
        .post(
            "relaunch",
            json!({"idempotency_key": "k-1", "host": "peer", "note": "start from the tests", "new_attempt": true}),
        )
        .await;
    assert_eq!(applied(&relaunched).host.as_deref(), Some("peer"));
    // The peer's own starter ran it; the board host, which has none, wrote
    // nothing and started nothing.
    assert_eq!(
        agent.starter.seen(),
        [(
            "relaunch: start from the tests".to_owned(),
            "board@test-host".to_owned(),
            Some("k-1".to_owned())
        )]
    );
    assert_eq!(last_seq(&agent.pair.board), board_before);

    // A relaunch for a third host that is not paired goes nowhere.
    agent.pair.board.remote_host("third");
    let third = agent
        .post(
            "relaunch",
            json!({"idempotency_key": "k-2", "host": "third", "note": "start from the tests"}),
        )
        .await;
    let error = refusal(&third, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error.message.contains("not a paired peer"),
        "{}",
        third.body
    );

    // The peer forwards no frame on (G42): a relaunch that names a host is
    // refused there, and nothing starts.
    let before = last_seq(&agent.pair.peer);
    let mut link = linked(&agent.pair);
    let order = Order {
        op: Op::Relaunch,
        target: Some(agent.session.clone()),
        body: json!({"idempotency_key": "k-3", "host": "third", "note": "start from the tests"}),
        idempotency_key: "k-3".to_owned(),
    };
    let frame = Mutate::signed(
        &signer(&agent.pair),
        &agent.pair.peer.host,
        order,
        Timestamp::now(),
    );
    let (status, body) = link.mutate(frame);
    assert_eq!(status, 400, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("names no host"),
        "{body}"
    );
    assert_eq!(last_seq(&agent.pair.peer), before);
    assert_eq!(agent.starter.seen().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_peer_agent_verb_waits_for_its_proof_past_ten_seconds() {
    // An interrupt waits up to 10 s for the turn to end, so the board host
    // waits for a proof verb's result longer than for a query (PROOF_WAIT).
    assert_eq!(super::PROOF_WAIT, Duration::from_secs(60));
    assert_eq!(Timing::default().proof, super::PROOF_WAIT);
    assert!(Timing::default().query <= Duration::from_secs(10));
    let timing = Timing {
        proof: super::PROOF_WAIT,
        ..quick()
    };
    let agent = peer_agent(timing, |_| {}).await;
    agent.set_screen(WORKING);
    // No Stop hook comes: the peer waits the whole 10 s, then answers.
    let started = std::time::Instant::now();
    let interrupted = agent
        .post(
            "interrupt",
            json!({"idempotency_key": "k-int", "host": "peer"}),
        )
        .await;
    let interrupted = applied(&interrupted);
    assert!(started.elapsed() >= Duration::from_secs(10));
    assert_eq!(interrupted.host.as_deref(), Some("peer"));
    assert_eq!(
        interrupted.data["cancel"], "unconfirmed",
        "{:?}",
        interrupted.data
    );
    assert_eq!(agent.keys(), [format!("keys {} esc", agent.pane)]);
}

/// A peer's starter whose stop takes a while: it marks the stop begun,
/// waits, then marks it done.
#[derive(Default)]
struct SlowStop {
    begun: AtomicBool,
    done: AtomicBool,
}

impl OrchestratorStart for SlowStop {
    fn start(
        &self,
        _store: &mut Store,
        _ctx: &tiller_core::Ctx,
        _project: &str,
        _restart: bool,
    ) -> Result<tiller_core::Applied<tiller_agents::spawn::orchestrator::Orchestrator>, StartError>
    {
        Err(StartError::Refused {
            exit: 5,
            code: "stub_refused",
            message: "the stub starts no orchestrator".into(),
            hint: None,
            current: None,
        })
    }

    fn stop_agent(
        &self,
        _store: &mut Store,
        _ctx: &tiller_core::Ctx,
        _session: &Session,
    ) -> Result<Controlled, StartError> {
        self.begun.store(true, Ordering::SeqCst);
        std::thread::sleep(ms(500));
        self.done.store(true, Ordering::SeqCst);
        Ok(Controlled {
            applied: tiller_core::Applied {
                value: json!({"verb": "stop"}),
                events: Vec::new(),
                replayed: false,
            },
            warning: None,
        })
    }
}

#[test]
fn a_link_that_ends_mid_proof_lets_the_verb_finish() {
    let mut pair = pair();
    let session = pair.peer.worker("p-1");
    let starter = Arc::new(SlowStop::default());
    let (ours, theirs) = UnixStream::pair().unwrap();
    let config = ServeConfig {
        home: pair.peer.dir.path().to_owned(),
        from: "test-host".to_owned(),
        timing: quick(),
        herdr: None,
        orchestrators: Some(starter.clone()),
        holder: None,
    };
    let input = BufReader::new(theirs.try_clone().unwrap());
    let served = std::thread::spawn(move || serve(&config, input, theirs));
    let mut link = welcomed(&pair, SyncLink::on(ours));
    let order = Order {
        op: Op::Stop,
        target: Some(session),
        body: json!({"idempotency_key": "k-stop"}),
        idempotency_key: "k-stop".to_owned(),
    };
    let frame = Mutate::signed(&signer(&pair), &pair.peer.host, order, Timestamp::now());
    link.send(&Frame::Mutate(Box::new(frame)));
    for _ in 0..500 {
        if starter.begun.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(ms(10));
    }
    assert!(starter.begun.load(Ordering::SeqCst), "the stop never began");

    // The board host's daemon restarts: the link ends mid-proof.
    drop(link);
    let _ = served.join().unwrap();
    assert!(
        starter.done.load(Ordering::SeqCst),
        "peer serve returned before the stop finished"
    );
}
