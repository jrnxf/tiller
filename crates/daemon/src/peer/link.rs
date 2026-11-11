//! The board host's link client (`protocol.md` 10.2): one task per paired
//! peer, each with its own SSH child, timeouts and backoff.
//!
//! Nothing local waits on a link. A task holds the shared map's lock only
//! to copy or change one [`PeerView`], never across an await, so a frozen
//! peer can stall nothing but its own task. Readers take a [`Federation`]
//! copy; browsers get the task's changes through [`Peers::subscribe`].

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::{Body, Bytes};
use serde_json::Value;
use tiller_core::domain::PeerScope;
use tiller_core::state::peers::{KeyFile, key_path, read_key_file};
use tiller_core::{Actor, Ctx, Store, Timestamp, ids};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};

use super::forward::{Mutate, Order};
use super::pane::{PaneKind, PaneOpen};
use super::{Federation, Frame, Hello, HelloKey, PeerView, Timing, peer_alias, stale_reason};
use crate::dto::{LinkEvent, LinkHealth, LinkState, PeerEvent, PeerResetEvent};
use crate::readonly::Damage;
use crate::views;

/// What a link tells open SSE streams.
#[derive(Clone, Debug)]
pub enum PeerMessage {
    /// A peer's change, already applied to the copy: `event: peer`.
    Change(Box<PeerEvent>),
    /// A link changed state: `event: link`.
    Link(LinkEvent),
    /// A peer's copy was replaced by a `snapshot`: `event: peer_reset` for
    /// the peer with this host id.
    Reset(String),
}

/// What a handler hands to a link: a single-entity read (10.2) or a
/// forwarded mutation (10.3).
enum Request {
    Query {
        path: String,
        reply: oneshot::Sender<(u16, Value)>,
    },
    Mutate {
        order: Order,
        /// [`PENDING`] until the link sends the frame ([`SENT`]) or the
        /// handler gives up first ([`ABANDONED`]): whichever comes first
        /// wins, so a mutation is never sent after its 503.
        sent: Arc<AtomicU8>,
        reply: oneshot::Sender<(u16, Value)>,
    },
}

const PENDING: u8 = 0;
const SENT: u8 = 1;
const ABANDONED: u8 = 2;

/// How a forwarded mutation ended on the board host.
#[derive(Debug, PartialEq)]
pub enum Forwarded {
    /// Not sent: the link is not live, or it never took the frame. 503.
    Unreachable,
    /// Sent, and no `result` came in time: it may have applied. 504.
    Uncertain,
    /// The peer's `result`.
    Answered(u16, Value),
}

/// How a pane link opened (`protocol.md` 10.7).
pub enum Paned {
    /// Not dialed or no answer in time: the link is not live, or the peer
    /// would not take a second session.
    Unreachable,
    /// The peer answered with a `result` instead of a stream: the scrollback,
    /// or a refusal with the status and body its own board API gives.
    Answered(u16, Value),
    /// The pane's mirror stream, its first line already read.
    Stream(Box<PaneStream>),
}

/// An open pane link. Dropping it closes the peer's stdin, which ends the
/// stream there, and kills the SSH child.
pub struct PaneStream {
    first: String,
    lines: tokio::io::Lines<Box<dyn AsyncBufRead + Send + Unpin>>,
    _writer: Box<dyn AsyncWrite + Send + Unpin>,
    _child: Option<Child>,
}

/// Lines waiting for a browser before its stream is dropped, as the local
/// mirror holds them.
const PANE_QUEUE: usize = 64;

impl PaneStream {
    /// The stream as the browser's NDJSON body, line for line as the peer
    /// sent them. It ends when the browser leaves, the peer closes, `stop`
    /// turns true or the browser falls a whole queue behind; a link that
    /// ends is followed by an `unavailable` line, so the browser
    /// opens again and learns whether the peer is still there.
    pub fn relay(
        self,
        label: String,
        stop: watch::Receiver<bool>,
        viewer: crate::mirror::Viewer,
    ) -> Body {
        let (tx, mut rx) = mpsc::channel::<Bytes>(PANE_QUEUE);
        tokio::spawn(async move {
            let _viewer = viewer;
            let PaneStream {
                first,
                mut lines,
                _writer,
                _child,
            } = self;
            let mut stop = stop;
            let queue = |line: String| {
                let mut bytes = line.into_bytes();
                bytes.push(b'\n');
                tx.try_send(Bytes::from(bytes)).is_ok()
            };
            if !queue(first) {
                return;
            }
            loop {
                tokio::select! {
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => {
                            if !queue(line) {
                                return;
                            }
                        }
                        Ok(None) | Err(_) => {
                            let ended = crate::dto::TerminalLine::Unavailable {
                                message: format!("the link to {label} ended"),
                            };
                            if let Ok(line) = serde_json::to_string(&ended) {
                                queue(line);
                            }
                            return;
                        }
                    },
                    _ = stop.wait_for(|stop| *stop) => return,
                    () = tx.closed() => return,
                }
            }
        });
        Body::from_stream(futures_util::stream::poll_fn(move |cx| {
            rx.poll_recv(cx)
                .map(|line| line.map(Ok::<_, std::convert::Infallible>))
        }))
    }
}

async fn dial_pane(
    connector: &dyn Connector,
    pairing: &Pairing,
    session: &str,
    kind: PaneKind,
    lines: Option<u16>,
) -> std::io::Result<Paned> {
    let key_file = pairing
        .key
        .as_ref()
        .map_err(|e| std::io::Error::other(e.clone()))?;
    let (Some(key_id), Ok(signing)) = (&key_file.key_id, key_file.signing_key()) else {
        return Err(std::io::Error::other("the key file has no accepted key"));
    };
    let signer = HelloKey {
        key: &signing,
        from: &pairing.local_label,
        key_id,
        viewer_host_id: &pairing.local_host_id,
    };
    let target = Target {
        label: pairing.label.clone(),
        alias: pairing.alias.clone(),
        from: pairing.local_label.clone(),
    };
    let Connection {
        reader,
        mut writer,
        child,
    } = connector.connect_pane(&target).await?;
    let open = PaneOpen::signed(
        &signer,
        &pairing.peer_host_id,
        session,
        kind,
        lines,
        Timestamp::now(),
    );
    write_frame(&mut writer, &Frame::PaneOpen(Box::new(open))).await?;
    let mut lines = reader.lines();
    let Some(first) = lines.next_line().await? else {
        return Err(std::io::Error::other("the peer closed the pane link"));
    };
    match serde_json::from_str::<Frame>(&first) {
        Ok(Frame::Result {
            http_status, body, ..
        }) => Ok(Paned::Answered(http_status, body)),
        Ok(Frame::Refused { reason, message }) => Ok(Paned::Answered(
            502,
            serde_json::json!({
                "ok": false,
                "error": {
                    "code": "peer_refused",
                    "reason": reason,
                    "message": message.unwrap_or_else(|| reason.clone()),
                },
            }),
        )),
        _ => Ok(Paned::Stream(Box::new(PaneStream {
            first,
            lines,
            _writer: writer,
            _child: child,
        }))),
    }
}

/// Every paired peer's link, shared by the read handlers and SSE streams.
pub struct Peers {
    /// By the peer's `hosts` row id here.
    views: Mutex<BTreeMap<String, PeerView>>,
    queries: Mutex<HashMap<String, mpsc::Sender<Request>>>,
    /// What a pane link dials with, by the peer's `hosts` row id.
    pairings: Mutex<HashMap<String, Pairing>>,
    /// Opens the pane links; none for [`Peers::idle`].
    connector: Option<Arc<dyn Connector>>,
    tx: broadcast::Sender<PeerMessage>,
    timing: Timing,
    /// A damaged `state.db` is never written, not even a link state
    /// (`recovery.md` R10).
    damage: Damage,
}

impl Peers {
    fn new(timing: Timing, damage: Damage, connector: Option<Arc<dyn Connector>>) -> Self {
        Self {
            views: Mutex::default(),
            queries: Mutex::default(),
            pairings: Mutex::default(),
            connector,
            tx: broadcast::channel(256).0,
            timing,
            damage,
        }
    }

    /// No links at all, for tests.
    pub fn idle() -> Arc<Self> {
        Arc::new(Self::new(Timing::default(), Damage::default(), None))
    }

    /// Starts the supervisor: it re-reads which hosts are paired every
    /// `timing.supervise`, runs one link task per peer, and restarts a
    /// task when its pairing changes. Everything ends when `stop` turns
    /// true.
    pub fn start(
        home: PathBuf,
        connector: Arc<dyn Connector>,
        timing: Timing,
        damage: Damage,
        stop: watch::Receiver<bool>,
    ) -> Arc<Self> {
        let peers = Arc::new(Self::new(timing, damage, Some(connector.clone())));
        tokio::spawn(supervise(peers.clone(), home, connector, stop));
        peers
    }

    /// Every peer as it is now, in label order.
    pub fn federation(&self) -> Federation {
        let mut peers: Vec<PeerView> = self.lock_views().values().cloned().collect();
        peers.sort_by(|a, b| a.label.cmp(&b.label));
        Federation { peers }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PeerMessage> {
        self.tx.subscribe()
    }

    /// Sends a `query` over a live link and waits for its `result`, up to
    /// `timing.query`. `None` when the link is not live or no answer came.
    pub async fn query(&self, host_id: &str, path: String) -> Option<(u16, Value)> {
        let sender = self.sender(host_id)?;
        let (reply, answer) = oneshot::channel();
        timeout(self.timing.query, async {
            sender.send(Request::Query { path, reply }).await.ok()?;
            answer.await.ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// Forwards a mutation over a live link (`protocol.md` 10.3) and waits
    /// up to `timing.query` for the peer's `result`. Nothing is queued: a
    /// link that is not live, or does not take the frame in time, is
    /// [`Forwarded::Unreachable`].
    pub async fn forward(&self, host_id: &str, order: Order) -> Forwarded {
        let Some(sender) = self.sender(host_id) else {
            return Forwarded::Unreachable;
        };
        let sent = Arc::new(AtomicU8::new(PENDING));
        let (reply, answer) = oneshot::channel();
        let request = Request::Mutate {
            order,
            sent: Arc::clone(&sent),
            reply,
        };
        let answered = timeout(self.timing.query, async {
            sender.send(request).await.ok()?;
            answer.await.ok()
        })
        .await
        .ok()
        .flatten();
        if let Some((status, body)) = answered {
            return Forwarded::Answered(status, body);
        }
        match sent.compare_exchange(PENDING, ABANDONED, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Forwarded::Unreachable,
            Err(_) => Forwarded::Uncertain,
        }
    }

    /// Opens a pane link to a peer whose link is live (`protocol.md`
    /// 10.7): dials a second session, sends one signed request and reads
    /// its first line. The pane's stream, or the `result` the peer answered
    /// in its place.
    pub async fn open_pane(
        &self,
        host_id: &str,
        session: &str,
        kind: PaneKind,
        lines: Option<u16>,
    ) -> Paned {
        let live = self
            .lock_views()
            .get(host_id)
            .is_some_and(|view| view.link == LinkState::Live);
        let pairing = self
            .pairings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(host_id)
            .cloned();
        let (true, Some(pairing), Some(connector)) = (live, pairing, self.connector.clone()) else {
            return Paned::Unreachable;
        };
        let dial = dial_pane(&*connector, &pairing, session, kind, lines);
        match timeout(self.timing.query, dial).await {
            Ok(Ok(paned)) => paned,
            Ok(Err(error)) => {
                tracing::warn!(peer = %pairing.label, %error, "pane link failed");
                Paned::Unreachable
            }
            Err(_) => Paned::Unreachable,
        }
    }

    /// A live link's request channel.
    fn sender(&self, host_id: &str) -> Option<mpsc::Sender<Request>> {
        let live = self
            .lock_views()
            .get(host_id)
            .is_some_and(|view| view.link == LinkState::Live);
        let sender = self
            .queries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(host_id)
            .cloned();
        sender.filter(|_| live)
    }

    /// The `peer_reset` for one peer: its copy as the federated reads
    /// return it.
    pub fn reset_event(&self, host_id: &str) -> Option<PeerResetEvent> {
        let views = self.lock_views();
        let view = views.get(host_id)?;
        Some(PeerResetEvent {
            host: view.label.clone(),
            host_id: view.host_id.clone(),
            link: view.link,
            as_of: view.as_of.clone(),
            queries: views::peer_queries(view),
        })
    }

    /// Each link's last success and last error, for `GET /api/v1/health`.
    pub fn links(&self) -> Vec<LinkHealth> {
        self.federation()
            .peers
            .into_iter()
            .map(|view| LinkHealth {
                host: view.label,
                link: view.link,
                last_success_at: view.last_frame_at.map(|at| at.to_string()),
                last_error: view.last_error,
            })
            .collect()
    }

    fn lock_pairings(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pairing>> {
        self.pairings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_views(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, PeerView>> {
        self.views.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes one peer's view; `None` when it is no longer paired.
    fn update<T>(&self, host_id: &str, change: impl FnOnce(&mut PeerView) -> T) -> Option<T> {
        self.lock_views().get_mut(host_id).map(change)
    }

    fn send(&self, message: PeerMessage) {
        // No open stream is not an error.
        let _ = self.tx.send(message);
    }
}

// -- connecting -------------------------------------------------------------------

/// Where a link dials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The peer's label here.
    pub label: String,
    /// Config `peers.<label>.ssh`.
    pub alias: String,
    /// This host's label: `--from`.
    pub from: String,
}

/// One open link: the peer's frames in, the board host's frames out, and
/// the SSH child, killed when the connection is dropped.
pub struct Connection {
    pub reader: Box<dyn AsyncBufRead + Send + Unpin>,
    pub writer: Box<dyn AsyncWrite + Send + Unpin>,
    pub child: Option<Child>,
}

pub type Connecting = Pin<Box<dyn Future<Output = std::io::Result<Connection>> + Send>>;

/// Opens links. The daemon's is [`SshConnector`]; tests connect in memory.
pub trait Connector: Send + Sync {
    fn connect(&self, target: &Target) -> Connecting;

    /// Opens a pane link (`tiller peer pane`); a connector that has none
    /// refuses it.
    fn connect_pane(&self, _target: &Target) -> Connecting {
        Box::pin(async { Err(std::io::Error::other("this connector has no pane links")) })
    }
}

/// `ssh -T -o BatchMode=yes -o ServerAliveInterval=15 -o
/// ServerAliveCountMax=2 <alias> tiller peer serve --from <label>`.
pub struct SshConnector {
    /// `ssh`, or `TILLER_SSH` split on whitespace.
    pub program: Vec<OsString>,
}

impl SshConnector {
    pub fn args(target: &Target) -> Vec<String> {
        Self::command(target, "serve")
    }

    /// The same session for `tiller peer pane`.
    pub fn pane_args(target: &Target) -> Vec<String> {
        Self::command(target, "pane")
    }

    fn command(target: &Target, subcommand: &str) -> Vec<String> {
        [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=2",
            &target.alias,
            "tiller",
            "peer",
            subcommand,
            "--from",
            &target.from,
        ]
        .map(str::to_owned)
        .to_vec()
    }

    fn dial(&self, args: Vec<String>) -> Connecting {
        let program = self.program.clone();
        Box::pin(async move {
            let Some((ssh, leading)) = program.split_first() else {
                return Err(std::io::Error::other("no ssh program"));
            };
            let mut child = Command::new(ssh)
                .args(leading)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                return Err(std::io::Error::other("ssh started without pipes"));
            };
            Ok(Connection {
                reader: Box::new(BufReader::new(stdout)),
                writer: Box::new(stdin),
                child: Some(child),
            })
        })
    }
}

impl Connector for SshConnector {
    fn connect(&self, target: &Target) -> Connecting {
        self.dial(Self::args(target))
    }

    fn connect_pane(&self, target: &Target) -> Connecting {
        self.dial(Self::pane_args(target))
    }
}

// -- the supervisor ---------------------------------------------------------------

/// One pairing as the supervisor reads it. Any change restarts its task.
#[derive(Clone, Debug)]
struct Pairing {
    host_id: String,
    label: String,
    peer_host_id: String,
    scope: PeerScope,
    /// Config `peers.<label>.ssh`, else the label.
    alias: String,
    /// The key file, or why it cannot be read.
    key: Result<KeyFile, String>,
    local_label: String,
    local_host_id: String,
}

impl PartialEq for Pairing {
    /// The key file compares by its public parts; the secret half changes
    /// only with them.
    fn eq(&self, other: &Self) -> bool {
        let public = |key: &Result<KeyFile, String>| {
            key.as_ref()
                .map(|key| {
                    (
                        key.key_id.clone(),
                        key.peer_host_id.clone(),
                        key.scope,
                        key.public_key.clone(),
                    )
                })
                .map_err(Clone::clone)
        };
        (
            &self.host_id,
            &self.label,
            &self.peer_host_id,
            self.scope,
            &self.alias,
            &self.local_label,
            &self.local_host_id,
        ) == (
            &other.host_id,
            &other.label,
            &other.peer_host_id,
            other.scope,
            &other.alias,
            &other.local_label,
            &other.local_host_id,
        ) && public(&self.key) == public(&other.key)
    }
}

/// Every pairing in `state.db`, or none before `tiller init`.
fn pairings(home: &Path) -> tiller_core::Result<Vec<Pairing>> {
    let db = home.join("state.db");
    if !db.exists() {
        return Ok(Vec::new());
    }
    let store = Store::open_current(&db)?;
    let local = store.this_host()?;
    let mut pairings = Vec::new();
    for host in store.paired_hosts()? {
        let (Some(scope), Some(peer_host_id)) = (host.peer_scope, host.peer_host_id) else {
            continue;
        };
        let key = key_path(home, &host.label)
            .and_then(|path| read_key_file(&path))
            .map_err(|error| error.to_string());
        pairings.push(Pairing {
            alias: peer_alias(home, &host.label).unwrap_or_else(|| host.label.clone()),
            host_id: host.id,
            label: host.label,
            peer_host_id,
            scope,
            key,
            local_label: local.label.clone(),
            local_host_id: local.id.clone(),
        });
    }
    Ok(pairings)
}

async fn supervise(
    peers: Arc<Peers>,
    home: PathBuf,
    connector: Arc<dyn Connector>,
    mut stop: watch::Receiver<bool>,
) {
    let mut tasks: HashMap<String, (Pairing, JoinHandle<()>)> = HashMap::new();
    loop {
        let read = {
            let home = home.clone();
            tokio::task::spawn_blocking(move || pairings(&home)).await
        };
        match read {
            Ok(Ok(wanted)) => {
                let gone: Vec<String> = (tasks.iter())
                    .filter(|(_, (pairing, _))| !wanted.contains(pairing))
                    .map(|(host_id, _)| host_id.clone())
                    .collect();
                for host_id in gone {
                    if let Some((_, task)) = tasks.remove(&host_id) {
                        // The old task is gone before its successor starts,
                        // so its last changes never land over the new link's.
                        task.abort();
                        let _ = task.await;
                        peers.lock_views().remove(&host_id);
                        peers.lock_pairings().remove(&host_id);
                    }
                }
                for pairing in wanted {
                    if tasks.contains_key(&pairing.host_id) {
                        continue;
                    }
                    peers.lock_views().insert(
                        pairing.host_id.clone(),
                        PeerView::new(
                            &pairing.label,
                            &pairing.host_id,
                            &pairing.peer_host_id,
                            pairing.scope,
                        ),
                    );
                    peers
                        .lock_pairings()
                        .insert(pairing.host_id.clone(), pairing.clone());
                    let task = tokio::spawn(run_link(
                        peers.clone(),
                        pairing.clone(),
                        connector.clone(),
                        home.clone(),
                    ));
                    tasks.insert(pairing.host_id.clone(), (pairing, task));
                }
            }
            Ok(Err(error)) => tracing::warn!(%error, "reading the pairings failed"),
            Err(error) => tracing::warn!(%error, "reading the pairings stopped"),
        }
        tokio::select! {
            () = tokio::time::sleep(peers.timing.supervise) => {}
            _ = stop.wait_for(|stop| *stop) => break,
        }
    }
    for (_, (_, task)) in tasks {
        task.abort();
    }
}

// -- one link ---------------------------------------------------------------------

/// How a session ended.
#[derive(Debug)]
enum Ended {
    /// It never got a `welcome`.
    Failed(String),
    /// It had a `welcome`, then the peer hung up or went quiet.
    Lost(String),
    Refused {
        reason: String,
        message: String,
    },
    /// The peer's `host_id` is not the paired one.
    HostChanged(String),
    /// The pairing cannot dial: no usable key.
    Unusable(String),
}

async fn run_link(
    peers: Arc<Peers>,
    pairing: Pairing,
    connector: Arc<dyn Connector>,
    home: PathBuf,
) {
    let timing = peers.timing;
    let link = LinkTask {
        peers,
        pairing,
        home,
    };
    let mut attempt = 0;
    // Refusals in a row since the last session that was welcomed.
    let mut refusals: usize = 0;
    let mut welcomed = false;
    loop {
        let began = Instant::now();
        let ended = link.session(&*connector, &mut welcomed).await;
        tracing::info!(peer = %link.pairing.label, ?ended, "peer link ended");
        let delay = match ended {
            Ended::Lost(error) => {
                // A session that held starts the backoff over; one that
                // drops right after `welcome` keeps backing off.
                if began.elapsed() >= timing.live {
                    attempt = 0;
                }
                refusals = 0;
                link.set(LinkState::Down, None, Some(error)).await;
                attempt += 1;
                timing.backoff(attempt - 1)
            }
            Ended::Failed(error) => {
                // Before the first welcome a link stays `never`; after it a
                // failing reconnect is `down`.
                let state = if welcomed {
                    LinkState::Down
                } else {
                    LinkState::Never
                };
                link.set(state, None, Some(error)).await;
                attempt += 1;
                timing.backoff(attempt - 1)
            }
            Ended::Refused { reason, message } => {
                let quick = matches!(reason.as_str(), "clock_skew" | "version");
                link.set(LinkState::Refused, Some(reason), Some(message))
                    .await;
                if quick {
                    timing.refused_retry
                } else {
                    // The peer's owner may clear it (accept turned back on,
                    // the key accepted again) with no word to this host, so
                    // the link asks again, slowly. Pairing again changes
                    // the key file, and the supervisor starts a new task.
                    refusals += 1;
                    timing.refused_backoff[(refusals - 1).min(timing.refused_backoff.len() - 1)]
                }
            }
            Ended::HostChanged(message) => {
                link.set(
                    LinkState::Refused,
                    Some("host_changed".into()),
                    Some(message),
                )
                .await;
                return;
            }
            Ended::Unusable(error) => {
                link.set(LinkState::Never, None, Some(error)).await;
                return;
            }
        };
        tokio::time::sleep(delay).await;
    }
}

struct LinkTask {
    peers: Arc<Peers>,
    pairing: Pairing,
    home: PathBuf,
}

/// Unregisters a session's query channel however the session ends.
struct Registered<'a> {
    peers: &'a Peers,
    host_id: &'a str,
    sender: mpsc::Sender<Request>,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        let mut queries = self
            .peers
            .queries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // A newer task for the same peer may have registered since.
        if queries
            .get(self.host_id)
            .is_some_and(|held| held.same_channel(&self.sender))
        {
            queries.remove(self.host_id);
        }
    }
}

impl LinkTask {
    fn host_id(&self) -> &str {
        &self.pairing.host_id
    }

    /// Sets the link state and error. A state change is an `event: link`
    /// and a `host.changed` row (`cache.refresh`).
    async fn set(&self, link: LinkState, reason: Option<String>, error: Option<String>) {
        let changed = self.peers.update(self.host_id(), |view| {
            let changed = view.link != link || view.link_reason != reason;
            view.link = link;
            view.link_reason = reason;
            if error.is_some() {
                view.last_error = error;
            }
            changed.then(|| view.clone())
        });
        let Some(Some(view)) = changed else {
            return;
        };
        self.peers.send(PeerMessage::Link(LinkEvent {
            host: view.label.clone(),
            link: view.link,
            as_of: view.as_of.clone(),
            stale_reason: view.stale_reason().map(str::to_owned),
        }));
        if self.peers.damage.reason().is_some() || tiller_core::owner::check(&self.home).is_err() {
            return;
        }
        let home = self.home.clone();
        let written = tokio::task::spawn_blocking(move || {
            let mut store = Store::open_current(home.join("state.db"))?;
            store.peer_link(
                &Ctx::new(Actor::Daemon),
                &view.host_id,
                view.link.as_str(),
                stale_reason(view.link, view.link_reason.as_deref()),
                view.last_frame_at,
            )
        })
        .await;
        match written {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "recording the link state failed"),
            Err(error) => tracing::warn!(%error, "recording the link state stopped"),
        }
    }

    /// A frame arrived: the link is fresh.
    async fn took_frame(&self) {
        let state = self.peers.update(self.host_id(), |view| {
            view.last_frame_at = Some(Timestamp::now());
            view.link
        });
        if state.is_some_and(|state| state != LinkState::Live) {
            self.set(LinkState::Live, None, None).await;
        }
    }

    /// One SSH session, from dialing to its end.
    async fn session(&self, connector: &dyn Connector, welcomed: &mut bool) -> Ended {
        let pairing = &self.pairing;
        let key_file = match &pairing.key {
            Ok(key) => key,
            Err(error) => return Ended::Unusable(error.clone()),
        };
        let (Some(key_id), Ok(signing)) = (&key_file.key_id, key_file.signing_key()) else {
            return Ended::Unusable(format!(
                "the key file for {} has no accepted key",
                pairing.label
            ));
        };
        let target = Target {
            label: pairing.label.clone(),
            alias: pairing.alias.clone(),
            from: pairing.local_label.clone(),
        };
        let connection = match connector.connect(&target).await {
            Ok(connection) => connection,
            Err(error) => return Ended::Failed(format!("dialing {}: {error}", target.alias)),
        };
        // Held to the end of the session: dropping it kills the SSH child.
        let Connection {
            reader,
            mut writer,
            child: _child,
        } = connection;
        let mut lines = reader.lines();
        let last_seq = self
            .peers
            .update(self.host_id(), |view| {
                view.queries.as_ref().and(view.last_event_seq)
            })
            .flatten();
        let signer = HelloKey {
            key: &signing,
            from: &pairing.local_label,
            key_id,
            viewer_host_id: &pairing.local_host_id,
        };
        let hello = Hello::signed(&signer, &pairing.peer_host_id, last_seq, Timestamp::now());
        if let Err(error) = write_frame(&mut writer, &Frame::Hello(hello)).await {
            return Ended::Failed(format!("sending hello: {error}"));
        }
        let timing = self.peers.timing;
        let first = match timeout(timing.down, lines.next_line()).await {
            Err(_) => return Ended::Failed("no welcome from the peer".into()),
            Ok(Err(error)) => return Ended::Failed(error.to_string()),
            Ok(Ok(None)) => return Ended::Failed("the peer closed the link before welcome".into()),
            Ok(Ok(Some(line))) => serde_json::from_str::<Frame>(&line),
        };
        match first {
            Ok(Frame::Welcome(welcome)) if welcome.host_id != pairing.peer_host_id => {
                return Ended::HostChanged(format!(
                    "the peer is host {}, not the paired {}",
                    welcome.host_id, pairing.peer_host_id
                ));
            }
            Ok(Frame::Welcome(welcome)) => {
                *welcomed = true;
                self.peers.update(self.host_id(), |view| {
                    view.offset_ms = offset_ms(&welcome.server_time);
                    view.last_error = None;
                });
            }
            Ok(Frame::Refused { reason, message }) => {
                return Ended::Refused {
                    message: message.unwrap_or_else(|| reason.clone()),
                    reason,
                };
            }
            Ok(_) | Err(_) => return Ended::Failed("the peer did not answer hello".into()),
        }
        self.took_frame().await;
        self.stream(lines, writer, &signer).await
    }

    /// The session after `welcome`.
    async fn stream(
        &self,
        mut lines: tokio::io::Lines<Box<dyn AsyncBufRead + Send + Unpin>>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
        signer: &HelloKey<'_>,
    ) -> Ended {
        let timing = self.peers.timing;
        let (frames, mut outgoing) = mpsc::channel::<Frame>(64);
        let writing = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(frame) = outgoing.recv().await {
                if write_frame(&mut writer, &frame).await.is_err() {
                    return;
                }
            }
        });
        let _writing = AbortOnDrop(writing);
        let (query_tx, mut queries) = mpsc::channel::<Request>(16);
        self.peers
            .queries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(self.host_id().to_owned(), query_tx.clone());
        let _registered = Registered {
            peers: &self.peers,
            host_id: self.host_id(),
            sender: query_tx,
        };
        let mut pending: HashMap<String, oneshot::Sender<(u16, Value)>> = HashMap::new();
        let mut tick = tokio::time::interval(timing.tick);
        let mut last_frame = Instant::now();
        let mut last_refresh: Option<Instant> = None;
        let mut refresh_wanted = false;
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let line = match line {
                        Ok(Some(line)) => line,
                        Ok(None) => return Ended::Lost("the peer closed the link".into()),
                        Err(error) => return Ended::Lost(error.to_string()),
                    };
                    last_frame = Instant::now();
                    self.took_frame().await;
                    let Ok(frame) = serde_json::from_str::<Frame>(&line) else {
                        tracing::warn!(peer = %self.pairing.label, "peer frame skipped");
                        continue;
                    };
                    match frame {
                        Frame::Snapshot(snapshot) => {
                            self.peers.update(self.host_id(), |view| view.apply_snapshot(*snapshot));
                            self.peers.send(PeerMessage::Reset(self.host_id().to_owned()));
                        }
                        Frame::Event(change) => {
                            let sent = self.peers.update(self.host_id(), |view| {
                                view.apply_event(&change);
                                PeerEvent {
                                    host: view.label.clone(),
                                    host_id: view.host_id.clone(),
                                    as_of: view.as_of.clone(),
                                    change: views::peer_change(view, *change),
                                }
                            });
                            if let Some(event) = sent {
                                self.peers.send(PeerMessage::Change(Box::new(event)));
                            }
                            refresh_wanted = true;
                        }
                        Frame::Beat { server_time, .. } => {
                            self.peers.update(self.host_id(), |view| {
                                view.apply_beat(&server_time);
                                view.offset_ms = offset_ms(&server_time);
                            });
                        }
                        Frame::Result { id, http_status, body } => {
                            if let Some(reply) = pending.remove(&id) {
                                let _ = reply.send((http_status, body));
                            }
                        }
                        Frame::Refused { reason, message } => {
                            return Ended::Refused {
                                message: message.unwrap_or_else(|| reason.clone()),
                                reason,
                            };
                        }
                        _ => {}
                    }
                }
                _ = tick.tick() => {
                    // A query that timed out no longer waits for its result.
                    pending.retain(|_, reply| !reply.is_closed());
                    let state = timing.state(last_frame.elapsed());
                    if state != LinkState::Live {
                        self.set(state, None, None).await;
                    }
                    if state == LinkState::Down {
                        return Ended::Lost("no frame from the peer in 2 min".into());
                    }
                    // Coalesced: at most one `refresh` per `timing.refresh`
                    // while events arrive.
                    let due = last_refresh.is_none_or(|at| at.elapsed() >= timing.refresh);
                    // Writes never wait here: a peer that stops reading
                    // fills the queue, and the frame is dropped rather than
                    // stalling the frame-age check.
                    if refresh_wanted && due {
                        refresh_wanted = false;
                        last_refresh = Some(Instant::now());
                        if let Err(TrySendError::Closed(_)) = frames.try_send(Frame::Refresh) {
                            return Ended::Lost("the link stopped writing".into());
                        }
                    }
                }
                Some(request) = queries.recv() => match request {
                    Request::Query { path, reply } => {
                        let id = ids::new_ulid();
                        match frames.try_send(Frame::Query { id: id.clone(), path }) {
                            // A dropped reply is a query with no answer: 503.
                            Ok(()) => {
                                pending.insert(id, reply);
                            }
                            Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Closed(_)) => {
                                return Ended::Lost("the link stopped writing".into());
                            }
                        }
                    }
                    Request::Mutate { order, sent, reply } => {
                        // A frame the queue cannot take is never sent: the
                        // dropped reply is a 503, and nothing is queued.
                        let permit = match frames.try_reserve() {
                            Ok(permit) => permit,
                            Err(TrySendError::Full(())) => continue,
                            Err(TrySendError::Closed(())) => {
                                return Ended::Lost("the link stopped writing".into());
                            }
                        };
                        // The handler gave up first and answered 503, so
                        // the frame must not go out.
                        let won = sent.compare_exchange(PENDING, SENT, Ordering::AcqRel, Ordering::Acquire);
                        if won.is_err() {
                            continue;
                        }
                        let frame = Mutate::signed(signer, &self.pairing.peer_host_id, order, Timestamp::now());
                        pending.insert(frame.id.clone(), reply);
                        permit.send(Frame::Mutate(Box::new(frame)));
                    }
                },
            }
        }
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn write_frame(writer: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> std::io::Result<()> {
    let line = frame.line().map_err(std::io::Error::other)?;
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await
}

/// The peer's clock minus this host's, from a `server_time`.
fn offset_ms(server_time: &str) -> i64 {
    server_time
        .parse::<Timestamp>()
        .map_or(0, |time| time.as_millis() - Timestamp::now().as_millis())
}
