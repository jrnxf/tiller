//! Tiller's side of the terminal holder (`docs/design/terminal-holder.md`):
//! the protocol's messages and framing, the version handshake, the socket
//! client and the `Panes` trait.
//!
//! This is the first slice (S1a): the handshake, `spawn`, `list`,
//! `subscribe`, raw input and `resize`. The rest of the protocol, the `Live`
//! trait and the modules that move from `tiller-herdr` come in H02.
//! It depends on no terminal crate.

pub mod client;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod message;
pub mod panes;
pub mod paths;

#[cfg(any(test, feature = "testing"))]
pub mod fake;

pub use client::{HolderClient, Resizer, Subscribe, Subscription, Update};
pub use error::{PanesError, Result};
pub use message::{Exit, Listing, Meta, Pane, PaneId, PaneState, Size, Spawn};
pub use panes::Panes;
pub use paths::{run_dir, socket_path};

#[cfg(any(test, feature = "testing"))]
pub use fake::FakePanes;
