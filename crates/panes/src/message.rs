//! The JSON messages of kind 0 frames (`terminal-holder.md` 2.4): requests,
//! replies and events.
//!
//! Every message is an object with a `type`. Each request carries an `id`
//! that the client picks, and its reply carries the same `id`. A side ignores
//! fields it does not know. A holder answers a request type it does not know
//! with an `unsupported` error ([`Request::decode`]), and a client skips a
//! message type it does not know ([`HolderMessage::Unknown`]).
//!
//! Raw input is a kind 2 frame and has no message.

use std::collections::BTreeMap;
use std::fmt;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::handshake::{Hello, Refused, Welcome};
use crate::keys::Key;

/// A pane's metadata: string keys and values, merged by the holder.
pub type Meta = BTreeMap<String, String>;

/// A holder pane id: `p` and a ULID. The holder never reuses one.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(String);

impl PaneId {
    /// A new pane id, for the holder (and its fake) to give out.
    pub fn generate() -> Self {
        Self(format!("p{}", ulid::Ulid::generate()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PaneId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for PaneId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A terminal size in character cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

/// The size a pane starts at when `spawn` names none (`[holder]
/// default_size`).
pub const DEFAULT_SIZE: Size = Size {
    cols: 120,
    rows: 40,
};

/// The `spawn` request's body: start a child on a new PTY.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spawn {
    /// The session id. A `spawn` with a key that a running pane holds
    /// returns that pane and starts nothing.
    pub key: String,
    /// `argv[0]` is an absolute path: the holder does no `PATH` search.
    pub argv: Vec<String>,
    /// The child's working directory, an absolute path.
    pub cwd: String,
    /// The child's whole environment. The holder adds only `TERM`,
    /// `COLORTERM` and `TILLER_PANE`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The base size; the holder's default (120 by 40) when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Size>,
    #[serde(default)]
    pub meta: Meta,
}

/// Whether a pane's child still runs.
///
/// There is no catch-all: a new state would change what `running` and
/// `exited` mean to every caller, which is a new major version
/// (`terminal-holder.md` 2.4), so a state this build does not know is a
/// protocol error, not a value to guess at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneState {
    Running,
    Exited,
}

/// How a child ended, once the holder reaped it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    /// The exit code, when the child exited.
    pub code: Option<i32>,
    /// The signal number, when a signal ended the child.
    pub signal: Option<i32>,
    pub at: Timestamp,
}

/// A pane record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    pub pane: PaneId,
    /// The short number that kind 2 frames address, unique within one holder
    /// `instance`.
    pub num: u32,
    /// The session id the pane was spawned with.
    pub key: String,
    pub pid: u32,
    pub argv: Vec<String>,
    pub cwd: String,
    pub started_at: Timestamp,
    pub state: PaneState,
    /// Set once the holder has reaped the child. An exited pane keeps its
    /// last screen until `forget`.
    pub exit: Option<Exit>,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub last_output_at: Option<Timestamp>,
    #[serde(default)]
    pub attach_clients: u32,
    #[serde(default)]
    pub subscribers: u32,
}

/// Every pane of one holder `instance`, running or exited.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub instance: String,
    pub panes: Vec<Pane>,
}

/// What a `screen` request reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Read {
    /// The visible screen: `"visible"` on the wire.
    Visible,
    /// The last `n` rows, scrollback included (1 to [`MAX_RECENT`]):
    /// `{"recent": n}`.
    Recent(u16),
}

/// The most rows a `recent` read takes (spec 2.4).
pub const MAX_RECENT: u16 = 10_000;

impl Read {
    /// Whether the holder takes this read: a `recent` read is 1 to
    /// [`MAX_RECENT`] rows.
    pub fn in_range(self) -> bool {
        match self {
            Read::Visible => true,
            Read::Recent(n) => (1..=MAX_RECENT).contains(&n),
        }
    }
}

/// How a `screen` reply draws its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// Plain text.
    Text,
    /// Text with its colors and styles as ANSI escapes.
    Ansi,
}

/// Where the cursor is, in cells from the top left of the visible screen
/// (0-based). A `recent` read's rows start above the screen, so its first
/// row is not row 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    #[serde(default = "shown")]
    pub visible: bool,
}

fn shown() -> bool {
    true
}

/// The `screen` reply: a read of the pane's model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    /// The terminal modes the app turned on, by name (such as
    /// `bracketed_paste` or `app_cursor`).
    #[serde(default)]
    pub modes: Vec<String>,
    /// The rows read, top first, in the asked format.
    pub rows_text: Vec<String>,
}

impl Screen {
    /// The rows as one string, split by `\n`.
    pub fn text(&self) -> String {
        self.rows_text.join("\n")
    }
}

/// A signal to a pane's process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Int,
    Term,
    Hup,
    Kill,
}

/// How long `close` waits after `hup` before `kill`, when it names no wait.
pub const CLOSE_GRACE_MS: u64 = 3_000;

fn close_grace_ms() -> u64 {
    CLOSE_GRACE_MS
}

/// How a subscriber gets synchronized output (mode 2026).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    /// The holder holds a frame from its start marker to its end marker and
    /// sends it as one. The board asks for this.
    Buffer,
    /// The bytes pass as the child wrote them, markers and all. `tiller
    /// attach` asks for this.
    #[default]
    Pass,
}

/// A client's message to the holder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// The first message on a connection, and only the first.
    Hello(Hello),
    Spawn {
        id: u64,
        #[serde(flatten)]
        spawn: Spawn,
    },
    List {
        id: u64,
    },
    /// Starts kind 1 frames for the pane: one repaint, then live output.
    Subscribe {
        id: u64,
        pane: PaneId,
        #[serde(default)]
        sync: SyncMode,
    },
    /// Sets the PTY size through the connection's lease `lease`, an id the
    /// client picks.
    Resize {
        id: u64,
        pane: PaneId,
        cols: u16,
        rows: u16,
        lease: String,
    },
    Get {
        id: u64,
        pane: PaneId,
    },
    Screen {
        id: u64,
        pane: PaneId,
        read: Read,
        format: Format,
    },
    /// Types text as keystrokes, with no paste brackets.
    Input {
        id: u64,
        pane: PaneId,
        text: String,
    },
    /// Sends text inside bracketed paste when the app turned it on.
    Paste {
        id: u64,
        pane: PaneId,
        text: String,
    },
    /// Named keys, encoded for the app's keyboard mode.
    Keys {
        id: u64,
        pane: PaneId,
        keys: Vec<Key>,
    },
    /// Drops the connection's lease `lease`.
    Release {
        id: u64,
        pane: PaneId,
        lease: String,
    },
    Unsubscribe {
        id: u64,
        stream: u32,
    },
    /// Merges `set` into the pane's metadata and drops the keys in `remove`.
    Meta {
        id: u64,
        pane: PaneId,
        #[serde(default)]
        set: Meta,
        #[serde(default)]
        remove: Vec<String>,
    },
    Signal {
        id: u64,
        pane: PaneId,
        signal: Signal,
    },
    /// `hup` to the group, then `kill` after `grace_ms`. The reply is the
    /// pane's `exited`, once the child is reaped.
    Close {
        id: u64,
        pane: PaneId,
        #[serde(default = "close_grace_ms")]
        grace_ms: u64,
    },
    /// Drops an exited pane and its screen.
    Forget {
        id: u64,
        pane: PaneId,
    },
    /// Starts events on this connection.
    Watch {
        id: u64,
    },
    /// Tells every attach client on the host to show `pane`.
    Switch {
        id: u64,
        pane: PaneId,
    },
    /// Re-executes the holder from `binary` (`terminal-holder.md` 6.2).
    Upgrade {
        id: u64,
        binary: String,
        #[serde(default)]
        dry_run: bool,
    },
    Ping {
        id: u64,
    },
}

/// Why the holder cannot take a message, and what it answers.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Undecodable {
    /// The request's `id`, when the message had one.
    pub id: Option<u64>,
    /// `unsupported` for a type this build does not know, `bad_request` for
    /// anything else.
    pub code: ErrorCode,
    pub message: String,
}

impl Undecodable {
    /// The `error` reply to send.
    pub fn reply(&self) -> HolderMessage {
        HolderMessage::Error {
            id: self.id,
            code: self.code.clone(),
            message: self.message.clone(),
        }
    }
}

impl Request {
    /// Every request `type` this build knows.
    pub const TYPES: &[&str] = &[
        "hello",
        "spawn",
        "list",
        "subscribe",
        "resize",
        "get",
        "screen",
        "input",
        "paste",
        "keys",
        "release",
        "unsubscribe",
        "meta",
        "signal",
        "close",
        "forget",
        "watch",
        "switch",
        "upgrade",
        "ping",
    ];

    /// The request's `type` on the wire, one of [`Request::TYPES`].
    pub fn kind(&self) -> &'static str {
        match self {
            Request::Hello(_) => "hello",
            Request::Spawn { .. } => "spawn",
            Request::List { .. } => "list",
            Request::Subscribe { .. } => "subscribe",
            Request::Resize { .. } => "resize",
            Request::Get { .. } => "get",
            Request::Screen { .. } => "screen",
            Request::Input { .. } => "input",
            Request::Paste { .. } => "paste",
            Request::Keys { .. } => "keys",
            Request::Release { .. } => "release",
            Request::Unsubscribe { .. } => "unsubscribe",
            Request::Meta { .. } => "meta",
            Request::Signal { .. } => "signal",
            Request::Close { .. } => "close",
            Request::Forget { .. } => "forget",
            Request::Watch { .. } => "watch",
            Request::Switch { .. } => "switch",
            Request::Upgrade { .. } => "upgrade",
            Request::Ping { .. } => "ping",
        }
    }

    /// The request's `id`; a hello has none.
    pub fn id(&self) -> Option<u64> {
        match self {
            Request::Hello(_) => None,
            Request::Spawn { id, .. }
            | Request::List { id }
            | Request::Subscribe { id, .. }
            | Request::Resize { id, .. }
            | Request::Get { id, .. }
            | Request::Screen { id, .. }
            | Request::Input { id, .. }
            | Request::Paste { id, .. }
            | Request::Keys { id, .. }
            | Request::Release { id, .. }
            | Request::Unsubscribe { id, .. }
            | Request::Meta { id, .. }
            | Request::Signal { id, .. }
            | Request::Close { id, .. }
            | Request::Forget { id, .. }
            | Request::Watch { id }
            | Request::Switch { id, .. }
            | Request::Upgrade { id, .. }
            | Request::Ping { id } => Some(*id),
        }
    }

    /// Reads a kind 0 frame as the holder does. A type this build does not
    /// know is `unsupported` and a known type it cannot read is
    /// `bad_request`, each with the message's `id` when it has one, so the
    /// holder can answer it.
    pub fn decode(text: &str) -> Result<Self, Undecodable> {
        // An object only: serde would also take an array, with its first
        // element as the type.
        let object: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text)
            .map_err(|err| Undecodable {
                id: None,
                code: ErrorCode::BadRequest,
                message: format!("not a JSON object: {err}"),
            })?;
        // An id that is not a request id is answered without one; the full
        // decode below names what is wrong with it.
        let id = object.get("id").and_then(serde_json::Value::as_u64);
        let Some(kind) = object
            .get("type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        else {
            return Err(Undecodable {
                id,
                code: ErrorCode::BadRequest,
                message: "the message has no type".into(),
            });
        };
        if !Self::TYPES.contains(&kind.as_str()) {
            return Err(Undecodable {
                id,
                code: ErrorCode::Unsupported,
                message: format!("unknown request type {kind:?}"),
            });
        }
        serde_json::from_value(serde_json::Value::Object(object)).map_err(|err| Undecodable {
            id,
            code: ErrorCode::BadRequest,
            message: format!("{kind}: {err}"),
        })
    }
}

/// An error reply's `code`. On the wire it is a snake_case string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ErrorCode {
    NotFound,
    Exited,
    BadRequest,
    Unsupported,
    /// A limit (`terminal-holder.md` 2.8).
    Busy,
    Internal,
    /// A code from a newer holder, kept as it came.
    Other(String),
}

impl ErrorCode {
    pub fn as_str(&self) -> &str {
        match self {
            ErrorCode::NotFound => "not_found",
            ErrorCode::Exited => "exited",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Busy => "busy",
            ErrorCode::Internal => "internal",
            ErrorCode::Other(code) => code,
        }
    }
}

impl From<String> for ErrorCode {
    fn from(code: String) -> Self {
        match code.as_str() {
            "not_found" => ErrorCode::NotFound,
            "exited" => ErrorCode::Exited,
            "bad_request" => ErrorCode::BadRequest,
            "unsupported" => ErrorCode::Unsupported,
            "busy" => ErrorCode::Busy,
            "internal" => ErrorCode::Internal,
            _ => ErrorCode::Other(code),
        }
    }
}

impl From<ErrorCode> for String {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Other(code) => code,
            known => known.as_str().to_owned(),
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The holder's message to a client: a handshake answer, a reply (with the
/// request's `id`), a stream message or an event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HolderMessage {
    Welcome(Welcome),
    Refused(Refused),
    /// The reply to `spawn`, `get` and `meta`. `existing` is true when a
    /// running pane already held a spawn's key, and nothing started.
    Pane {
        id: u64,
        pane: Pane,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        existing: bool,
    },
    /// The reply to `list`.
    Panes {
        id: u64,
        #[serde(flatten)]
        listing: Listing,
    },
    /// The reply to `subscribe`: kind 1 frames for this pane carry `stream`.
    Subscribed {
        id: u64,
        stream: u32,
        cols: u16,
        rows: u16,
    },
    /// Comes just before a subscription's first kind 1 frame, which redraws
    /// the whole screen on an empty terminal of this size.
    Repaint {
        stream: u32,
        cols: u16,
        rows: u16,
    },
    /// The reply to `screen`.
    Screen {
        id: u64,
        #[serde(flatten)]
        screen: Screen,
    },
    /// A request done. The reply to `resize` and `release` holds the PTY's
    /// size after it, and the reply to `switch` the attach clients told.
    Ok {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clients: Option<usize>,
    },
    /// A pane's child was reaped: an event, a subscription's last message,
    /// or, with the request's `id`, the reply to `close`.
    Exited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        pane: PaneId,
        exit: Exit,
    },
    /// The reply to `watch`: events follow on this connection.
    Watching {
        id: u64,
    },
    /// The reply to `upgrade`: the holder re-executed, and the new image
    /// sends it. For a dry run, the old one sends it: every check passed.
    Upgrading {
        id: u64,
    },
    /// The reply to `upgrade` when the holder will not re-execute.
    UpgradeRefused {
        id: u64,
        reason: String,
    },
    Pong {
        id: u64,
    },
    /// Event: a pane started.
    Spawned {
        pane: PaneId,
    },
    /// Event: the top lease set the pane's size.
    Resized {
        pane: PaneId,
        cols: u16,
        rows: u16,
    },
    /// Event: the pane's metadata changed; `get` reads it.
    Meta {
        pane: PaneId,
    },
    /// Event: the app set the pane's title.
    Title {
        pane: PaneId,
        title: String,
    },
    /// Event: the app rang the bell.
    Bell {
        pane: PaneId,
    },
    /// Event: the app sent a notification (OSC 9 or OSC 777).
    Notify {
        pane: PaneId,
        #[serde(default)]
        title: String,
        #[serde(default)]
        body: String,
    },
    /// Event: no output for 500 ms after output.
    Quiet {
        pane: PaneId,
        at: Timestamp,
    },
    /// Event: the pane's screen model was rebuilt after it failed
    /// (`terminal-holder.md` 3.5).
    ModelReset {
        pane: PaneId,
    },
    /// Event: an exited pane was dropped, by `forget` or by a limit.
    Forgotten {
        pane: PaneId,
        #[serde(default)]
        reason: String,
    },
    /// Event: the holder re-executed as this version.
    Upgraded {
        holder: String,
    },
    /// The holder cut subscription `stream`, which fell behind its queue.
    /// The subscriber may subscribe again for a fresh repaint.
    Lagged {
        stream: u32,
    },
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        code: ErrorCode,
        #[serde(default)]
        message: String,
    },
    /// A message type from a newer holder, skipped.
    #[serde(other)]
    Unknown,
}

impl HolderMessage {
    /// The `id` of the request this message answers, if it answers one.
    pub fn reply_to(&self) -> Option<u64> {
        match self {
            HolderMessage::Pane { id, .. }
            | HolderMessage::Panes { id, .. }
            | HolderMessage::Subscribed { id, .. }
            | HolderMessage::Screen { id, .. }
            | HolderMessage::Ok { id, .. }
            | HolderMessage::Watching { id }
            | HolderMessage::Upgrading { id }
            | HolderMessage::UpgradeRefused { id, .. }
            | HolderMessage::Pong { id } => Some(*id),
            HolderMessage::Error { id, .. } | HolderMessage::Exited { id, .. } => *id,
            HolderMessage::Welcome(_)
            | HolderMessage::Refused(_)
            | HolderMessage::Repaint { .. }
            | HolderMessage::Lagged { .. }
            | HolderMessage::Spawned { .. }
            | HolderMessage::Resized { .. }
            | HolderMessage::Meta { .. }
            | HolderMessage::Title { .. }
            | HolderMessage::Bell { .. }
            | HolderMessage::Notify { .. }
            | HolderMessage::Quiet { .. }
            | HolderMessage::ModelReset { .. }
            | HolderMessage::Forgotten { .. }
            | HolderMessage::Upgraded { .. }
            | HolderMessage::Unknown => None,
        }
    }

    /// The event this message is, when it is one.
    pub fn event(&self) -> Option<Event> {
        self.clone().into_event().ok()
    }

    /// The event this message is, or the message back when it is none.
    pub fn into_event(self) -> std::result::Result<Event, Box<Self>> {
        Ok(match self {
            HolderMessage::Spawned { pane } => Event::Spawned { pane },
            HolderMessage::Exited {
                id: None,
                pane,
                exit,
            } => Event::Exited { pane, exit },
            HolderMessage::Resized { pane, cols, rows } => Event::Resized { pane, cols, rows },
            HolderMessage::Meta { pane } => Event::Meta { pane },
            HolderMessage::Title { pane, title } => Event::Title { pane, title },
            HolderMessage::Bell { pane } => Event::Bell { pane },
            HolderMessage::Notify { pane, title, body } => Event::Notify { pane, title, body },
            HolderMessage::Quiet { pane, at } => Event::Quiet { pane, at },
            HolderMessage::ModelReset { pane } => Event::ModelReset { pane },
            HolderMessage::Forgotten { pane, reason } => Event::Forgotten { pane, reason },
            HolderMessage::Upgraded { holder } => Event::Upgraded { holder },
            message => return Err(Box::new(message)),
        })
    }
}

/// What a `watch` connection yields (`terminal-holder.md` 2.4, events).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Spawned {
        pane: PaneId,
    },
    Exited {
        pane: PaneId,
        exit: Exit,
    },
    Resized {
        pane: PaneId,
        cols: u16,
        rows: u16,
    },
    Meta {
        pane: PaneId,
    },
    Title {
        pane: PaneId,
        title: String,
    },
    Bell {
        pane: PaneId,
    },
    Notify {
        pane: PaneId,
        title: String,
        body: String,
    },
    Quiet {
        pane: PaneId,
        at: Timestamp,
    },
    ModelReset {
        pane: PaneId,
    },
    Forgotten {
        pane: PaneId,
        reason: String,
    },
    Upgraded {
        holder: String,
    },
}

impl Event {
    /// The event as the holder sends it, for a holder or a fake to send.
    pub fn message(&self) -> HolderMessage {
        match self.clone() {
            Event::Spawned { pane } => HolderMessage::Spawned { pane },
            Event::Exited { pane, exit } => HolderMessage::Exited {
                id: None,
                pane,
                exit,
            },
            Event::Resized { pane, cols, rows } => HolderMessage::Resized { pane, cols, rows },
            Event::Meta { pane } => HolderMessage::Meta { pane },
            Event::Title { pane, title } => HolderMessage::Title { pane, title },
            Event::Bell { pane } => HolderMessage::Bell { pane },
            Event::Notify { pane, title, body } => HolderMessage::Notify { pane, title, body },
            Event::Quiet { pane, at } => HolderMessage::Quiet { pane, at },
            Event::ModelReset { pane } => HolderMessage::ModelReset { pane },
            Event::Forgotten { pane, reason } => HolderMessage::Forgotten { pane, reason },
            Event::Upgraded { holder } => HolderMessage::Upgraded { holder },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handshake::ProtoRange;
    use serde_json::{Value, json};

    fn at() -> Timestamp {
        "2026-11-11T12:00:00Z".parse().unwrap()
    }

    fn pane() -> Pane {
        Pane {
            pane: "p01JD0000000000000000000000".into(),
            num: 7,
            key: "01JDSESSION000000000000000".into(),
            pid: 4242,
            argv: vec!["/bin/sh".into()],
            cwd: "/home/user/src/alpha".into(),
            started_at: at(),
            state: PaneState::Running,
            exit: None,
            cols: 120,
            rows: 40,
            meta: Meta::from([("label".into(), "alpha".into())]),
            title: "alpha".into(),
            last_output_at: None,
            attach_clients: 0,
            subscribers: 1,
        }
    }

    fn round_trip_request(request: Request, wire: Value) {
        assert_eq!(serde_json::to_value(&request).unwrap(), wire);
        assert_eq!(Request::decode(&wire.to_string()).unwrap(), request);
    }

    fn round_trip_reply(message: HolderMessage, wire: Value) {
        assert_eq!(serde_json::to_value(&message).unwrap(), wire);
        let back: HolderMessage = serde_json::from_value(wire).unwrap();
        assert_eq!(back, message);
    }

    #[test]
    fn requests_have_the_spec_shapes() {
        round_trip_request(
            Request::Hello(Hello {
                proto: ProtoRange { min: 1, max: 1 },
                client: "daemon".into(),
                version: "0.1.0-pre.30".into(),
            }),
            json!({"type":"hello","proto":[1,1],"client":"daemon","version":"0.1.0-pre.30"}),
        );
        round_trip_request(
            Request::Spawn {
                id: 1,
                spawn: Spawn {
                    key: "s1".into(),
                    argv: vec!["/bin/sh".into(), "-l".into()],
                    cwd: "/home/user".into(),
                    env: BTreeMap::from([("HOME".into(), "/home/user".into())]),
                    size: Some(Size {
                        cols: 100,
                        rows: 30,
                    }),
                    meta: Meta::from([("role".into(), "worker".into())]),
                },
            },
            json!({"type":"spawn","id":1,"key":"s1","argv":["/bin/sh","-l"],"cwd":"/home/user",
                "env":{"HOME":"/home/user"},"size":{"cols":100,"rows":30},"meta":{"role":"worker"}}),
        );
        round_trip_request(Request::List { id: 2 }, json!({"type":"list","id":2}));
        round_trip_request(
            Request::Subscribe {
                id: 3,
                pane: "p1".into(),
                sync: SyncMode::Buffer,
            },
            json!({"type":"subscribe","id":3,"pane":"p1","sync":"buffer"}),
        );
        round_trip_request(
            Request::Resize {
                id: 4,
                pane: "p1".into(),
                cols: 80,
                rows: 24,
                lease: "fit".into(),
            },
            json!({"type":"resize","id":4,"pane":"p1","cols":80,"rows":24,"lease":"fit"}),
        );
    }

    /// The requests past S1a's five, as they read on the wire
    /// (`terminal-holder.md` 2.4).
    fn later_requests() -> Vec<(Request, serde_json::Value)> {
        let p1 = || PaneId::from("p1");
        vec![
            (
                Request::Get { id: 5, pane: p1() },
                json!({"type":"get","id":5,"pane":"p1"}),
            ),
            (
                Request::Screen {
                    id: 6,
                    pane: p1(),
                    read: Read::Recent(200),
                    format: Format::Ansi,
                },
                json!({"type":"screen","id":6,"pane":"p1","read":{"recent":200},"format":"ansi"}),
            ),
            (
                Request::Screen {
                    id: 6,
                    pane: p1(),
                    read: Read::Visible,
                    format: Format::Text,
                },
                json!({"type":"screen","id":6,"pane":"p1","read":"visible","format":"text"}),
            ),
            (
                Request::Input {
                    id: 7,
                    pane: p1(),
                    text: "ls".into(),
                },
                json!({"type":"input","id":7,"pane":"p1","text":"ls"}),
            ),
            (
                Request::Paste {
                    id: 8,
                    pane: p1(),
                    text: "a\nb".into(),
                },
                json!({"type":"paste","id":8,"pane":"p1","text":"a\nb"}),
            ),
            (
                Request::Keys {
                    id: 9,
                    pane: p1(),
                    keys: vec!["ctrl+c".parse().unwrap(), Key::ENTER],
                },
                json!({"type":"keys","id":9,"pane":"p1","keys":["ctrl+c","enter"]}),
            ),
            (
                Request::Release {
                    id: 10,
                    pane: p1(),
                    lease: "fit".into(),
                },
                json!({"type":"release","id":10,"pane":"p1","lease":"fit"}),
            ),
            (
                Request::Unsubscribe { id: 11, stream: 4 },
                json!({"type":"unsubscribe","id":11,"stream":4}),
            ),
            (
                Request::Meta {
                    id: 12,
                    pane: p1(),
                    set: Meta::from([("label".into(), "alpha".into())]),
                    remove: vec!["role".into()],
                },
                json!({"type":"meta","id":12,"pane":"p1","set":{"label":"alpha"},"remove":["role"]}),
            ),
            (
                Request::Signal {
                    id: 13,
                    pane: p1(),
                    signal: Signal::Int,
                },
                json!({"type":"signal","id":13,"pane":"p1","signal":"int"}),
            ),
            (
                Request::Close {
                    id: 14,
                    pane: p1(),
                    grace_ms: 500,
                },
                json!({"type":"close","id":14,"pane":"p1","grace_ms":500}),
            ),
            (
                Request::Forget { id: 15, pane: p1() },
                json!({"type":"forget","id":15,"pane":"p1"}),
            ),
            (Request::Watch { id: 16 }, json!({"type":"watch","id":16})),
            (
                Request::Switch { id: 17, pane: p1() },
                json!({"type":"switch","id":17,"pane":"p1"}),
            ),
            (
                Request::Upgrade {
                    id: 18,
                    binary: "/home/user/bin/tiller".into(),
                    dry_run: true,
                },
                json!({"type":"upgrade","id":18,"binary":"/home/user/bin/tiller","dry_run":true}),
            ),
            (Request::Ping { id: 19 }, json!({"type":"ping","id":19})),
        ]
    }

    #[test]
    fn the_later_requests_round_trip_and_their_defaults_fill_in() {
        for (request, wire) in later_requests() {
            round_trip_request(request, wire);
        }
        let close: Request =
            serde_json::from_value(json!({"type":"close","id":1,"pane":"p1"})).unwrap();
        assert_eq!(
            close,
            Request::Close {
                id: 1,
                pane: "p1".into(),
                grace_ms: CLOSE_GRACE_MS,
            }
        );
        let meta: Request =
            serde_json::from_value(json!({"type":"meta","id":2,"pane":"p1"})).unwrap();
        assert_eq!(
            meta,
            Request::Meta {
                id: 2,
                pane: "p1".into(),
                set: Meta::new(),
                remove: Vec::new(),
            }
        );
        // A key name that does not parse makes the request unreadable.
        assert!(
            serde_json::from_value::<Request>(
                json!({"type":"keys","id":3,"pane":"p1","keys":["ctrl+"]})
            )
            .is_err()
        );
    }

    #[test]
    fn every_request_type_is_known_to_decode() {
        let mut requests = vec![
            Request::Hello(Hello::new("daemon")),
            Request::List { id: 1 },
            Request::Subscribe {
                id: 1,
                pane: "p1".into(),
                sync: SyncMode::Pass,
            },
            Request::Resize {
                id: 1,
                pane: "p1".into(),
                cols: 1,
                rows: 1,
                lease: "l".into(),
            },
            Request::Spawn {
                id: 1,
                spawn: Spawn {
                    key: "k".into(),
                    argv: vec!["/bin/true".into()],
                    cwd: "/".into(),
                    env: BTreeMap::new(),
                    size: None,
                    meta: Meta::new(),
                },
            },
        ];
        let mut later = later_requests();
        // One `screen` is enough.
        later.remove(2);
        requests.extend(later.into_iter().map(|(request, _)| request));
        assert_eq!(requests.len(), Request::TYPES.len());
        for request in requests {
            let wire = serde_json::to_value(&request).unwrap();
            assert_eq!(wire["type"], request.kind());
            assert!(
                Request::TYPES.contains(&wire["type"].as_str().unwrap()),
                "{wire}"
            );
            assert_eq!(Request::decode(&wire.to_string()).unwrap(), request);
        }
    }

    #[test]
    fn a_spawn_takes_the_defaults_for_what_it_leaves_out() {
        let request = Request::decode(
            r#"{"type":"spawn","id":5,"key":"k","argv":["/bin/sh"],"cwd":"/","later":1}"#,
        )
        .unwrap();
        let Request::Spawn { id: 5, spawn } = request else {
            panic!("{request:?}");
        };
        assert!(spawn.env.is_empty() && spawn.meta.is_empty() && spawn.size.is_none());
        let request = Request::decode(r#"{"type":"subscribe","id":6,"pane":"p1"}"#).unwrap();
        assert!(matches!(
            request,
            Request::Subscribe {
                sync: SyncMode::Pass,
                ..
            }
        ));
    }

    #[test]
    fn an_unknown_request_type_is_unsupported_with_its_id() {
        let err = Request::decode(r#"{"type":"teleport","id":9}"#).unwrap_err();
        assert_eq!(err.id, Some(9));
        assert_eq!(err.code, ErrorCode::Unsupported);
        assert_eq!(
            serde_json::to_value(err.reply()).unwrap()["code"],
            "unsupported"
        );
    }

    #[test]
    fn a_bad_request_keeps_its_id() {
        let err = Request::decode(r#"{"type":"spawn","id":3,"key":"k"}"#).unwrap_err();
        assert_eq!((err.id, err.code), (Some(3), ErrorCode::BadRequest));
        for text in ["", "[1]", r#"["list",7]"#, "not json", r#"{"id":4}"#] {
            let err = Request::decode(text).unwrap_err();
            assert_eq!(err.code, ErrorCode::BadRequest, "{text:?}");
        }
        // A known type with an id that is not a request id is still read as
        // that type, and answered without the id.
        let err = Request::decode(r#"{"type":"list","id":"7"}"#).unwrap_err();
        assert_eq!((err.id, err.code), (None, ErrorCode::BadRequest));
        assert!(err.message.starts_with("list: "), "{}", err.message);
    }

    #[test]
    fn replies_and_events_have_the_spec_shapes() {
        let pane = pane();
        let record = json!({"pane":"p01JD0000000000000000000000","num":7,"key":"01JDSESSION000000000000000",
            "pid":4242,"argv":["/bin/sh"],"cwd":"/home/user/src/alpha","started_at":"2026-11-11T12:00:00Z",
            "state":"running","exit":null,"cols":120,"rows":40,"meta":{"label":"alpha"},"title":"alpha",
            "last_output_at":null,"attach_clients":0,"subscribers":1});
        round_trip_reply(
            HolderMessage::Pane {
                id: 1,
                pane: pane.clone(),
                existing: false,
            },
            json!({"type":"pane","id":1,"pane":record}),
        );
        round_trip_reply(
            HolderMessage::Pane {
                id: 1,
                pane: pane.clone(),
                existing: true,
            },
            json!({"type":"pane","id":1,"pane":record,"existing":true}),
        );
        round_trip_reply(
            HolderMessage::Panes {
                id: 2,
                listing: Listing {
                    instance: "01JDINSTANCE00000000000000".into(),
                    panes: vec![pane],
                },
            },
            json!({"type":"panes","id":2,"instance":"01JDINSTANCE00000000000000","panes":[record]}),
        );
        round_trip_reply(
            HolderMessage::Subscribed {
                id: 3,
                stream: 11,
                cols: 120,
                rows: 40,
            },
            json!({"type":"subscribed","id":3,"stream":11,"cols":120,"rows":40}),
        );
        round_trip_reply(
            HolderMessage::Repaint {
                stream: 11,
                cols: 120,
                rows: 40,
            },
            json!({"type":"repaint","stream":11,"cols":120,"rows":40}),
        );
        round_trip_reply(
            HolderMessage::Ok {
                id: 4,
                cols: Some(80),
                rows: Some(24),
                clients: None,
            },
            json!({"type":"ok","id":4,"cols":80,"rows":24}),
        );
        round_trip_reply(
            HolderMessage::Exited {
                id: None,
                pane: "p1".into(),
                exit: Exit {
                    code: Some(3),
                    signal: None,
                    at: at(),
                },
            },
            json!({"type":"exited","pane":"p1","exit":{"code":3,"signal":null,"at":"2026-11-11T12:00:00Z"}}),
        );
        round_trip_reply(
            HolderMessage::Lagged { stream: 11 },
            json!({"type":"lagged","stream":11}),
        );
        round_trip_reply(
            HolderMessage::Error {
                id: Some(5),
                code: ErrorCode::NotFound,
                message: "no pane p9".into(),
            },
            json!({"type":"error","id":5,"code":"not_found","message":"no pane p9"}),
        );
    }

    #[test]
    fn the_later_replies_round_trip() {
        round_trip_reply(
            HolderMessage::Screen {
                id: 6,
                screen: Screen {
                    cols: 80,
                    rows: 2,
                    cursor: Cursor {
                        col: 2,
                        row: 1,
                        visible: false,
                    },
                    modes: vec!["bracketed_paste".into()],
                    rows_text: vec!["one".into(), "$ ".into()],
                },
            },
            json!({"type":"screen","id":6,"cols":80,"rows":2,
                "cursor":{"col":2,"row":1,"visible":false},
                "modes":["bracketed_paste"],"rows_text":["one","$ "]}),
        );
        round_trip_reply(
            HolderMessage::Ok {
                id: 17,
                cols: None,
                rows: None,
                clients: Some(2),
            },
            json!({"type":"ok","id":17,"clients":2}),
        );
        let closed = HolderMessage::Exited {
            id: Some(14),
            pane: "p1".into(),
            exit: Exit {
                code: None,
                signal: Some(1),
                at: at(),
            },
        };
        assert_eq!(closed.reply_to(), Some(14));
        // The reply to `close` is not an event.
        assert_eq!(closed.event(), None);
        round_trip_reply(
            closed,
            json!({"type":"exited","id":14,"pane":"p1","exit":{"code":null,"signal":1,"at":"2026-11-11T12:00:00Z"}}),
        );
        round_trip_reply(
            HolderMessage::Watching { id: 16 },
            json!({"type":"watching","id":16}),
        );
        round_trip_reply(
            HolderMessage::Upgrading { id: 18 },
            json!({"type":"upgrading","id":18}),
        );
        round_trip_reply(
            HolderMessage::UpgradeRefused {
                id: 18,
                reason: "a client holds an old protocol".into(),
            },
            json!({"type":"upgrade_refused","id":18,"reason":"a client holds an old protocol"}),
        );
        round_trip_reply(
            HolderMessage::Pong { id: 19 },
            json!({"type":"pong","id":19}),
        );
        // A cursor with no `visible` is shown.
        let cursor: Cursor = serde_json::from_value(json!({"col":0,"row":0})).unwrap();
        assert!(cursor.visible);
    }

    #[test]
    fn every_event_round_trips_and_is_no_reply() {
        let p1 = || PaneId::from("p1");
        let events = [
            (
                Event::Spawned { pane: p1() },
                json!({"type":"spawned","pane":"p1"}),
            ),
            (
                Event::Exited {
                    pane: p1(),
                    exit: Exit {
                        code: Some(0),
                        signal: None,
                        at: at(),
                    },
                },
                json!({"type":"exited","pane":"p1","exit":{"code":0,"signal":null,"at":"2026-11-11T12:00:00Z"}}),
            ),
            (
                Event::Resized {
                    pane: p1(),
                    cols: 80,
                    rows: 24,
                },
                json!({"type":"resized","pane":"p1","cols":80,"rows":24}),
            ),
            (
                Event::Meta { pane: p1() },
                json!({"type":"meta","pane":"p1"}),
            ),
            (
                Event::Title {
                    pane: p1(),
                    title: "vim".into(),
                },
                json!({"type":"title","pane":"p1","title":"vim"}),
            ),
            (
                Event::Bell { pane: p1() },
                json!({"type":"bell","pane":"p1"}),
            ),
            (
                Event::Notify {
                    pane: p1(),
                    title: "done".into(),
                    body: "tests pass".into(),
                },
                json!({"type":"notify","pane":"p1","title":"done","body":"tests pass"}),
            ),
            (
                Event::Quiet {
                    pane: p1(),
                    at: at(),
                },
                json!({"type":"quiet","pane":"p1","at":"2026-11-11T12:00:00Z"}),
            ),
            (
                Event::ModelReset { pane: p1() },
                json!({"type":"model_reset","pane":"p1"}),
            ),
            (
                Event::Forgotten {
                    pane: p1(),
                    reason: "limit".into(),
                },
                json!({"type":"forgotten","pane":"p1","reason":"limit"}),
            ),
            (
                Event::Upgraded {
                    holder: "0.2.0".into(),
                },
                json!({"type":"upgraded","holder":"0.2.0"}),
            ),
        ];
        for (event, wire) in events {
            let message = event.message();
            assert_eq!(message.reply_to(), None, "{event:?}");
            assert_eq!(message.event().as_ref(), Some(&event));
            round_trip_reply(message, wire);
        }
        // An event's optional text may be absent.
        let notify: HolderMessage =
            serde_json::from_value(json!({"type":"notify","pane":"p1"})).unwrap();
        assert_eq!(
            notify.event(),
            Some(Event::Notify {
                pane: p1(),
                title: String::new(),
                body: String::new(),
            })
        );
    }

    #[test]
    fn a_client_skips_what_a_newer_holder_adds() {
        let message: HolderMessage =
            serde_json::from_str(r#"{"type":"sparkle","pane":"p1","at":"2026-11-11T12:00:00Z"}"#)
                .unwrap();
        assert_eq!(message, HolderMessage::Unknown);
        let message: HolderMessage =
            serde_json::from_str(r#"{"type":"error","id":1,"code":"tea_time"}"#).unwrap();
        let HolderMessage::Error { code, .. } = &message else {
            panic!("{message:?}");
        };
        assert_eq!(code, &ErrorCode::Other("tea_time".into()));
        assert_eq!(code.to_string(), "tea_time");
        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            json!({"type":"error","id":1,"code":"tea_time","message":""})
        );
        let message: HolderMessage =
            serde_json::from_str(r#"{"type":"ok","id":2,"new_field":[1]}"#).unwrap();
        assert_eq!(message.reply_to(), Some(2));
    }

    #[test]
    fn pane_ids_are_p_and_a_ulid() {
        let id = PaneId::generate();
        let ulid = id.as_str().strip_prefix('p').unwrap();
        assert!(ulid.parse::<ulid::Ulid>().is_ok(), "{id}");
        assert_ne!(PaneId::generate(), id);
    }
}
