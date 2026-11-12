//! The federated board (ADR 0003, `protocol.md` section 10): the peer side
//! of a link ([`serve`], run by `tiller peer serve` under sshd) and the board
//! host's link client ([`link`], one task per paired peer in the daemon),
//! and the owner's mutations forwarded over it ([`forward`]).
//!
//! A link is one SSH session carrying JSON lines, one [`Frame`] per line,
//! protocol `tiller-peer/1`. The board host keeps what a peer sends in memory
//! only ([`PeerView`]); nothing a peer owns is ever written to this host's
//! `state.db`, apart from the link state on the peer's `hosts` row.

pub mod forward;
pub mod link;
mod nonces;
pub mod pair;
pub mod pane;
pub mod serve;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_core::Timestamp;
use tiller_core::domain::PeerScope;
use tiller_core::ids;
use tiller_core::state::peers::{PEER_PROTOCOL, parse_public_key};

use crate::dto::{
    AgentsResponse, AttentionItem, AttentionResponse, BoardResponse, ChangeEvent, LinkState,
    Machine, MachinesResponse, Project, ProjectsResponse,
};
use crate::views::SNAPSHOT_PATHS;

// -- frames -------------------------------------------------------------------

/// One line on a peer link (`protocol.md` 10.2). `mutate` belongs to
/// forwarded mutations (10.3); a frame this build does not know is read as
/// [`Frame::Unknown`] and ignored, so a newer peer never breaks the link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Hello(Hello),
    Welcome(Welcome),
    Refused {
        /// `revoked`, `unknown_key`, `bad_sig`, `clock_skew`, `replayed`,
        /// `version` or `accept_off`.
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Snapshot(Box<PeerSnapshot>),
    Event(Box<ChangeEvent>),
    Beat {
        server_time: String,
        last_event_seq: i64,
    },
    Query {
        id: String,
        /// `cards/{id}`, `cards/{id}/timeline` (with `?before=`) or
        /// `runs/{id}`.
        path: String,
    },
    /// A forwarded mutation, signed by the board host (10.3).
    Mutate(Box<forward::Mutate>),
    /// The one request on a pane link, signed by the board host (10.7).
    PaneOpen(Box<pane::PaneOpen>),
    /// The owner's keys or paste for an open pane stream, signed (10.7).
    PaneInput(Box<pane::PaneInput>),
    /// The board's new size for an open pane stream's Fit, signed (10.7).
    PaneFit(Box<pane::PaneFit>),
    Result {
        id: String,
        http_status: u16,
        body: Value,
    },
    /// The board host asks for a fresh `snapshot`, so a column the peer
    /// derives never drifts: at most once every 2 s while events arrive.
    Refresh,
    #[serde(other)]
    Unknown,
}

/// The board host's opening frame. `sig` is Ed25519 over every other field
/// and the `host_id` the board host expects the peer to have.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub v: String,
    /// The board host's label, as the peer's `peer_keys.viewer_label`.
    pub from: String,
    /// The peer's `peer_keys.id` for this board host.
    pub key_id: String,
    pub viewer_host_id: String,
    pub nonce: String,
    pub ts: String,
    pub sig: String,
    /// The peer `seq` the board host last applied, or null.
    pub last_seq: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub host_id: String,
    pub label: String,
    pub schema_version: usize,
    pub tiller_version: String,
    pub server_time: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerSnapshot {
    pub server_time: String,
    pub last_event_seq: i64,
    pub queries: PeerQueries,
}

/// A peer's own rows, keyed by the paths the boot snapshot uses (7.6), each
/// built by the same view as that GET. `machines` holds the peer's local row
/// only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerQueries {
    #[serde(rename = "/api/v1/board?filter=all")]
    pub board: BoardResponse,
    #[serde(rename = "/api/v1/attention?state=open")]
    pub attention: AttentionResponse,
    #[serde(rename = "/api/v1/agents")]
    pub agents: AgentsResponse,
    #[serde(rename = "/api/v1/machines")]
    pub machines: MachinesResponse,
    #[serde(rename = "/api/v1/projects")]
    pub projects: ProjectsResponse,
}

impl PeerQueries {
    /// The rows as the boot snapshot keys them.
    pub fn to_value(&self) -> Value {
        json!({
            SNAPSHOT_PATHS[0]: self.board,
            SNAPSHOT_PATHS[1]: self.attention,
            SNAPSHOT_PATHS[2]: self.agents,
            SNAPSHOT_PATHS[3]: self.machines,
            SNAPSHOT_PATHS[5]: self.projects,
        })
    }
}

impl Frame {
    /// The frame as one JSON line, newline included.
    pub fn line(&self) -> serde_json::Result<String> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }
}

// -- the hello signature ---------------------------------------------------------

/// The bytes a `hello` signature covers: every field but `sig`, and the
/// peer `host_id` the signer expects, so a frame meant for one peer is
/// refused by every other.
fn hello_message(hello: &Hello, peer_host_id: &str) -> Vec<u8> {
    json!([
        "tiller-peer/1 hello",
        hello.v,
        hello.from,
        hello.key_id,
        hello.viewer_host_id,
        hello.nonce,
        hello.ts,
        hello.last_seq,
        peer_host_id,
    ])
    .to_string()
    .into_bytes()
}

/// Who signs a `hello` or a `mutate`: the board host, with the key `tiller
/// peer add` made.
pub struct HelloKey<'a> {
    pub key: &'a SigningKey,
    pub from: &'a str,
    pub key_id: &'a str,
    pub viewer_host_id: &'a str,
}

impl Hello {
    /// A signed `hello` for the peer whose `meta.host_id` is
    /// `peer_host_id`, at `now`, with a fresh nonce.
    pub fn signed(
        signer: &HelloKey<'_>,
        peer_host_id: &str,
        last_seq: Option<i64>,
        now: Timestamp,
    ) -> Self {
        let mut hello = Hello {
            v: PEER_PROTOCOL.to_owned(),
            from: signer.from.to_owned(),
            key_id: signer.key_id.to_owned(),
            viewer_host_id: signer.viewer_host_id.to_owned(),
            nonce: ids::new_ulid(),
            ts: now.to_string(),
            sig: String::new(),
            last_seq,
        };
        let signature = signer.key.sign(&hello_message(&hello, peer_host_id));
        hello.sig = BASE64.encode(signature.to_bytes());
        hello
    }

    /// True when `sig` is valid for `public_key` (base64, as `peer_keys`
    /// stores it) and names `peer_host_id`.
    pub fn verifies(&self, public_key: &str, peer_host_id: &str) -> bool {
        let Ok(key) = parse_public_key(public_key) else {
            return false;
        };
        let Some(signature) = BASE64
            .decode(&self.sig)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
        else {
            return false;
        };
        key.verify(&hello_message(self, peer_host_id), &signature)
            .is_ok()
    }
}

// -- timing -------------------------------------------------------------------

/// Every interval a link keeps (`protocol.md` 10.2). Tests shrink them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// A frame this recent keeps the link `live`.
    pub live: Duration,
    /// No frame for this long makes the link `down`, and the SSH child is
    /// killed.
    pub down: Duration,
    /// How often `peer serve` sends a `beat`.
    pub beat: Duration,
    /// How often `peer serve` looks for new `events` rows.
    pub poll: Duration,
    /// The least time between two `refresh` requests.
    pub refresh: Duration,
    /// How long a `query` waits for its `result`.
    pub query: Duration,
    /// How far a `hello`'s `ts` may be from the peer's clock.
    pub skew: Duration,
    /// Reconnect delays; the last repeats.
    pub backoff: [Duration; 4],
    /// The retry delay for a `clock_skew` or `version` refusal.
    pub refused_retry: Duration,
    /// Retry delays after any other refusal (`accept_off`, `revoked` and
    /// the rest the peer's owner must clear); the last repeats. Slow, so a
    /// revoked key never hammers the peer.
    pub refused_backoff: [Duration; 4],
    /// How often a link checks its frame age, and `peer serve` re-reads
    /// `federation.accept` and the key row.
    pub tick: Duration,
    /// How often the daemon re-reads which hosts are paired.
    pub supervise: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        let s = Duration::from_secs;
        Self {
            live: s(25),
            down: s(120),
            beat: s(10),
            poll: Duration::from_millis(200),
            refresh: s(2),
            query: s(10),
            skew: s(120),
            backoff: [s(5), s(15), s(30), s(60)],
            refused_retry: s(60),
            refused_backoff: [s(300), s(600), s(1200), s(1800)],
            tick: s(1),
            supervise: s(2),
        }
    }
}

impl Timing {
    /// The link state a frame `age` old gives, while connected.
    pub fn state(&self, age: Duration) -> LinkState {
        if age < self.live {
            LinkState::Live
        } else if age < self.down {
            LinkState::Lagging
        } else {
            LinkState::Down
        }
    }

    /// The delay before reconnect attempt `attempt` (0 is the first).
    pub fn backoff(&self, attempt: usize) -> Duration {
        self.backoff[attempt.min(self.backoff.len() - 1)]
    }
}

// -- config -------------------------------------------------------------------

fn config(home: &Path) -> Option<toml::Table> {
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    text.parse().ok()
}

/// Config `federation.accept`: absent or `true` accepts. A `config.toml`
/// that does not parse refuses, since the owner may have meant `false`.
pub fn federation_accept(home: &Path) -> bool {
    let path = home.join("config.toml");
    if !path.exists() {
        return true;
    }
    let Some(config) = config(home) else {
        return false;
    };
    match config
        .get("federation")
        .and_then(|table| table.get("accept"))
    {
        None => true,
        Some(value) => value.as_bool() == Some(true),
    }
}

/// Config `peers.<label>.ssh`: the SSH alias the link dials.
pub fn peer_alias(home: &Path, label: &str) -> Option<String> {
    config(home)?
        .get("peers")?
        .get(label)?
        .get("ssh")?
        .as_str()
        .map(str::to_owned)
}

fn upsert<T>(rows: &mut Vec<T>, row: T, same: impl Fn(&T) -> bool) {
    match rows.iter_mut().find(|held| same(held)) {
        Some(held) => *held = row,
        None => rows.push(row),
    }
}

// -- the board host's copy -------------------------------------------------------

/// One paired peer as the board host holds it: the link and the rows the
/// peer last sent, in memory only.
#[derive(Clone, Debug, PartialEq)]
pub struct PeerView {
    pub label: String,
    /// The peer's `hosts` row id on this host: the id its Machines row and
    /// every one of its rows carry here.
    pub host_id: String,
    /// The peer's own `meta.host_id`.
    pub peer_host_id: String,
    pub scope: PeerScope,
    pub link: LinkState,
    /// The refusal reason while `refused`.
    pub link_reason: Option<String>,
    /// The peer's `server_time` of the newest frame the copy took.
    pub as_of: Option<String>,
    /// When the newest frame arrived, by this host's clock.
    pub last_frame_at: Option<Timestamp>,
    /// The peer's clock minus this host's.
    pub offset_ms: i64,
    /// The peer `seq` this host last applied.
    pub last_event_seq: Option<i64>,
    pub last_error: Option<String>,
    /// The peer's rows as its newest snapshot and the events since left
    /// them. Shared, so a read takes a copy of every peer cheaply.
    pub queries: Option<Arc<PeerQueries>>,
}

impl PeerView {
    pub fn new(label: &str, host_id: &str, peer_host_id: &str, scope: PeerScope) -> Self {
        Self {
            label: label.to_owned(),
            host_id: host_id.to_owned(),
            peer_host_id: peer_host_id.to_owned(),
            scope,
            link: LinkState::Never,
            link_reason: None,
            as_of: None,
            last_frame_at: None,
            offset_ms: 0,
            last_event_seq: None,
            last_error: None,
            queries: None,
        }
    }

    /// Takes a `snapshot`: the peer's rows are replaced whole.
    pub fn apply_snapshot(&mut self, snapshot: PeerSnapshot) {
        self.newer_as_of(&snapshot.server_time);
        self.last_event_seq = Some(snapshot.last_event_seq);
        self.queries = Some(Arc::new(snapshot.queries));
    }

    /// Takes one `event` at once (`protocol.md` 10.2, Fresh): its card row,
    /// attention item, project or the peer's own machine row. What an event
    /// does not carry (agents, permission prompts) the next coalesced
    /// `snapshot` brings.
    pub fn apply_event(&mut self, change: &ChangeEvent) {
        self.newer_as_of(&change.at);
        self.last_event_seq = Some(change.seq);
        let Some(queries) = &mut self.queries else {
            return;
        };
        let queries = Arc::make_mut(queries);
        if let Some(card_id) = &change.card_id {
            let rows = &mut queries.board.rows;
            match &change.card {
                Some(card) => upsert(rows, card.clone(), |row| row.id == card.id),
                None => rows.retain(|row| &row.id != card_id),
            }
        }
        let id = change.entity_id.as_str();
        match change.entity.as_str() {
            "attention" => {
                let items = &mut queries.attention.items;
                match serde_json::from_value::<AttentionItem>(change.entity_state.clone()) {
                    Ok(item) if item.answered_at.is_none() && item.withdrawn_at.is_none() => {
                        upsert(items, item, |row| row.id == id);
                    }
                    _ => items.retain(|row| row.id != id),
                }
            }
            "project" => {
                if let Ok(project) = serde_json::from_value::<Project>(change.entity_state.clone())
                {
                    upsert(&mut queries.projects.projects, project, |row| row.id == id);
                }
            }
            "host" if id == self.peer_host_id => {
                if let Ok(machine) = serde_json::from_value::<Machine>(change.entity_state.clone())
                {
                    queries.machines.machines = vec![machine];
                }
            }
            _ => {}
        }
    }

    /// Takes a `beat`: the copy is as of the peer's `server_time`.
    pub fn apply_beat(&mut self, server_time: &str) {
        self.newer_as_of(server_time);
    }

    /// `as_of` moves forward only: an event's `at` can be older than the
    /// snapshot that already included it.
    fn newer_as_of(&mut self, time: &str) {
        let newer = match (&self.as_of, time.parse::<Timestamp>()) {
            (_, Err(_)) => false,
            (None, Ok(_)) => true,
            (Some(held), Ok(time)) => held.parse::<Timestamp>().map_or(true, |held| time > held),
        };
        if newer {
            self.as_of = Some(time.to_owned());
        }
    }

    /// `hosts.stale_reason` and the Machines row's reason for this link.
    pub fn stale_reason(&self) -> Option<&'static str> {
        stale_reason(self.link, self.link_reason.as_deref())
    }

    /// Whether `host` (a label or either host id) names this peer.
    pub fn is(&self, host: &str) -> bool {
        self.label == host || self.host_id == host || self.peer_host_id == host
    }
}

/// The stale reason a link state gives (`protocol.md` section 9).
pub fn stale_reason(link: LinkState, refusal: Option<&str>) -> Option<&'static str> {
    match link {
        LinkState::Live | LinkState::Never => None,
        LinkState::Lagging | LinkState::Down => Some("peer_link_down"),
        LinkState::Refused if refusal == Some("version") => Some("peer_version"),
        LinkState::Refused => Some("peer_refused"),
    }
}

/// Every paired peer, as the federated reads see them at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Federation {
    pub peers: Vec<PeerView>,
}

impl Federation {
    pub fn peer(&self, host: &str) -> Option<&PeerView> {
        self.peers.iter().find(|peer| peer.is(host))
    }
}

#[cfg(test)]
mod tests;
