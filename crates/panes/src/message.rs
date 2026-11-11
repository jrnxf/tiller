//! The JSON messages of kind 0 frames (`terminal-holder.md` 2.4): requests,
//! replies and events.
//!
//! Every message is an object with a `type`. Each request carries an `id`
//! that the client picks, and its reply carries the same `id`. A side ignores
//! fields it does not know. A holder answers a request type it does not know
//! with an `unsupported` error ([`Request::decode`]), and a client skips a
//! message type it does not know ([`HolderMessage::Unknown`]).
//!
//! This holds the slice's part of the protocol: the handshake, `spawn`,
//! `list`, `subscribe` (a `repaint`, then live output), `resize`, the pane
//! record with its exit, and the `exited` event. Raw input is a kind 2 frame
//! and has no message.

use std::collections::BTreeMap;
use std::fmt;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::handshake::{Hello, Refused, Welcome};

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
    pub const TYPES: &[&str] = &["hello", "spawn", "list", "subscribe", "resize"];

    /// The request's `id`; a hello has none.
    pub fn id(&self) -> Option<u64> {
        match self {
            Request::Hello(_) => None,
            Request::Spawn { id, .. }
            | Request::List { id }
            | Request::Subscribe { id, .. }
            | Request::Resize { id, .. } => Some(*id),
        }
    }

    /// Reads a kind 0 frame as the holder does. A type this build does not
    /// know is `unsupported` and a known type it cannot read is
    /// `bad_request`, each with the message's `id` when it has one, so the
    /// holder can answer it.
    pub fn decode(text: &str) -> Result<Self, Undecodable> {
        #[derive(Deserialize)]
        struct Probe {
            #[serde(rename = "type")]
            kind: Option<serde_json::Value>,
            id: Option<serde_json::Value>,
        }
        let probe: Probe = serde_json::from_str(text).map_err(|err| Undecodable {
            id: None,
            code: ErrorCode::BadRequest,
            message: format!("not a JSON object: {err}"),
        })?;
        // An id that is not a request id is answered without one; the full
        // decode below names what is wrong with it.
        let id = probe.id.as_ref().and_then(serde_json::Value::as_u64);
        let Some(kind) = probe.kind.as_ref().and_then(serde_json::Value::as_str) else {
            return Err(Undecodable {
                id,
                code: ErrorCode::BadRequest,
                message: "the message has no type".into(),
            });
        };
        if !Self::TYPES.contains(&kind) {
            return Err(Undecodable {
                id,
                code: ErrorCode::Unsupported,
                message: format!("unknown request type {kind:?}"),
            });
        }
        serde_json::from_str(text).map_err(|err| Undecodable {
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
    /// The reply to `spawn`. `existing` is true when a running pane already
    /// held the key, and nothing started.
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
    /// A request done. The reply to `resize` holds the PTY's size after it.
    Ok {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cols: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<u16>,
    },
    /// A pane's child was reaped.
    Exited {
        pane: PaneId,
        exit: Exit,
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
            | HolderMessage::Ok { id, .. } => Some(*id),
            HolderMessage::Error { id, .. } => *id,
            HolderMessage::Welcome(_)
            | HolderMessage::Refused(_)
            | HolderMessage::Repaint { .. }
            | HolderMessage::Exited { .. }
            | HolderMessage::Lagged { .. }
            | HolderMessage::Unknown => None,
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

    #[test]
    fn every_request_type_is_known_to_decode() {
        let requests = [
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
        assert_eq!(requests.len(), Request::TYPES.len());
        for request in requests {
            let wire = serde_json::to_value(&request).unwrap();
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
        for text in ["", "[1]", "not json", r#"{"id":4}"#] {
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
            },
            json!({"type":"ok","id":4,"cols":80,"rows":24}),
        );
        round_trip_reply(
            HolderMessage::Exited {
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
    fn a_client_skips_what_a_newer_holder_adds() {
        let message: HolderMessage =
            serde_json::from_str(r#"{"type":"quiet","pane":"p1","at":"2026-11-11T12:00:00Z"}"#)
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
