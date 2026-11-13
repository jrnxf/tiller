//! Tiller's side of the terminal holder (`docs/design/terminal-holder.md`):
//! the protocol's messages and framing, the version handshake, the socket
//! client, the `Panes` and `Live` traits and key names (4.2, 4.3).
//!
//! It also holds the parts of the herdr adapter that are about panes but
//! not about herdr (4.1): reading a Claude composer, submitting a line,
//! process groups, "Open in terminal" and the test fleet. It depends on no
//! terminal crate.

pub mod client;
pub mod composer;
pub mod env;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod keys;
pub mod live;
pub mod message;
pub mod open;
pub mod panes;
pub mod paths;
pub mod process;
pub mod submit;

#[cfg(any(test, feature = "testing"))]
pub mod fake;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use client::{HolderClient, Upgrade};
pub use error::{PanesError, Result};
pub use keys::{BadKey, Key};
pub use live::{Events, EventsEnd, StreamHandle, Subscribe, Subscription, Update};
pub use message::{
    Cursor, Event, Exit, Format, Listing, MAX_RECENT, Meta, Pane, PaneId, PaneState, Read, Screen,
    Signal, Size, Spawn,
};
pub use panes::{Live, Panes};
pub use paths::{run_dir, socket_path};

#[cfg(any(test, feature = "testing"))]
pub use fake::FakePanes;
