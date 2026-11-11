//! The version handshake (`terminal-holder.md` 2.4).
//!
//! The client sends [`Hello`] first, with the range of major versions it
//! speaks. The holder picks the highest version both sides speak and answers
//! [`Welcome`], or answers [`Refused`] with its own range.
//!
//! Within one major version a side ignores unknown fields, and a new request
//! type or field is a minor change. A changed meaning is a new major version.
//! A holder supports its own major version and the one before it, and so does
//! a daemon, so either can upgrade first.

use std::fmt;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// The major protocol version this build speaks natively.
pub const PROTO: u32 = 1;

/// Every major version this build speaks: [`PROTO`] and the one before it,
/// once there is one.
pub const SUPPORTED: ProtoRange = ProtoRange {
    min: if PROTO > 1 { PROTO - 1 } else { PROTO },
    max: PROTO,
};

// This build speaks its own version and at most the one before it.
const _: () = assert!(SUPPORTED.max == PROTO && SUPPORTED.min + 1 >= PROTO && SUPPORTED.min >= 1);

/// A range of major versions, both ends included. On the wire it is the
/// array `[min, max]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "[u32; 2]", into = "[u32; 2]")]
pub struct ProtoRange {
    pub min: u32,
    pub max: u32,
}

impl From<[u32; 2]> for ProtoRange {
    fn from([min, max]: [u32; 2]) -> Self {
        Self { min, max }
    }
}

impl From<ProtoRange> for [u32; 2] {
    fn from(range: ProtoRange) -> Self {
        [range.min, range.max]
    }
}

impl ProtoRange {
    pub fn contains(self, version: u32) -> bool {
        self.min <= version && version <= self.max
    }
}

impl fmt::Display for ProtoRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{} to {}", self.min, self.max)
        }
    }
}

/// The highest major version in both ranges, or `None` when they do not
/// meet. An empty range (`min` over `max`) meets nothing.
pub fn negotiate(client: ProtoRange, holder: ProtoRange) -> Option<u32> {
    let low = client.min.max(holder.min);
    let high = client.max.min(holder.max);
    (low <= high).then_some(high)
}

/// The client's first message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub proto: ProtoRange,
    /// What the client is: `daemon`, `attach` or `peer`.
    pub client: String,
    /// The client's Tiller version.
    pub version: String,
}

impl Hello {
    /// A hello from this build, which speaks [`SUPPORTED`].
    pub fn new(client: impl Into<String>) -> Self {
        Self {
            proto: SUPPORTED,
            client: client.into(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// The holder's answer when the versions meet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Welcome {
    /// The major version the holder picked for this connection.
    pub proto: u32,
    /// The holder's Tiller version.
    pub holder: String,
    /// A new ULID each time a holder starts from nothing; a live upgrade
    /// keeps it. A changed instance means the old holder and all its panes
    /// are gone (R21).
    pub instance: String,
    pub boot_id: String,
    pub started_at: Timestamp,
    #[serde(default)]
    pub features: Vec<String>,
}

/// The holder's answer when the versions do not meet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    /// The holder's own range.
    pub proto: ProtoRange,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn range(min: u32, max: u32) -> ProtoRange {
        ProtoRange { min, max }
    }

    #[test]
    fn negotiate_picks_the_highest_shared_version() {
        assert_eq!(negotiate(range(1, 1), range(1, 1)), Some(1));
        assert_eq!(negotiate(range(1, 2), range(2, 3)), Some(2));
        assert_eq!(negotiate(range(2, 3), range(1, 2)), Some(2));
        assert_eq!(negotiate(range(1, 3), range(1, 3)), Some(3));
    }

    #[test]
    fn negotiate_refuses_ranges_that_do_not_meet() {
        assert_eq!(negotiate(range(1, 1), range(2, 3)), None);
        assert_eq!(negotiate(range(3, 4), range(1, 2)), None);
        // An empty range meets nothing.
        assert_eq!(negotiate(range(2, 1), range(1, 2)), None);
    }

    #[test]
    fn handshake_messages_match_the_spec_examples() {
        use crate::message::{HolderMessage, Request};

        let hello = Request::Hello(Hello {
            proto: range(1, 1),
            client: "daemon".into(),
            version: "0.1.0-pre.30".into(),
        });
        assert_eq!(
            serde_json::to_value(&hello).unwrap(),
            json!({"type":"hello","proto":[1,1],"client":"daemon","version":"0.1.0-pre.30"})
        );
        let welcome: HolderMessage = serde_json::from_value(json!({
            "type":"welcome","proto":1,"holder":"0.1.0-pre.30","instance":"01JD0000000000000000000000",
            "boot_id":"b","started_at":"2026-11-11T12:00:00Z","features":["kitty_keys","sync_buffer"],
            "a_later_field": true
        }))
        .unwrap();
        let HolderMessage::Welcome(welcome) = welcome else {
            panic!("{welcome:?}");
        };
        assert_eq!(welcome.proto, 1);
        assert_eq!(welcome.features, ["kitty_keys", "sync_buffer"]);
        let refused = HolderMessage::Refused(Refused {
            proto: range(2, 3),
            message: String::new(),
        });
        assert_eq!(
            serde_json::to_value(&refused).unwrap(),
            json!({"type":"refused","proto":[2,3]})
        );
    }
}
