//! The `Panes` and `Live` traits (`terminal-holder.md` 4.2): what the
//! daemon asks of the holder.
//!
//! The traits follow the holder. They have no workspaces, tabs, layout or
//! ordering. A pane is a child process with a key, metadata and a
//! screen.

use std::time::Duration;

use crate::error::Result;
use crate::keys::Key;
use crate::live::{Events, Subscribe, Subscription};
use crate::message::{Exit, Format, Listing, Meta, Pane, PaneId, Read, Screen, Signal, Spawn};

pub trait Panes: Send + Sync {
    /// Starts a child, or returns the running pane that already holds `req.key`.
    fn spawn(&self, req: &Spawn) -> Result<Pane>;
    /// Every pane, with the holder instance it came from.
    fn list(&self) -> Result<Listing>;
    /// One pane, or None when the holder has no such pane.
    fn get(&self, pane: &PaneId) -> Result<Option<Pane>>;
    fn screen(&self, pane: &PaneId, read: Read, format: Format) -> Result<Screen>;
    /// Types text as keystrokes, with no paste brackets.
    fn input(&self, pane: &PaneId, text: &str) -> Result<()>;
    /// Sends text inside bracketed paste when the app turned it on.
    fn paste(&self, pane: &PaneId, text: &str) -> Result<()>;
    fn keys(&self, pane: &PaneId, keys: &[Key]) -> Result<()>;
    /// Merges `set` into the pane's metadata and drops the keys in `remove`.
    fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> Result<Pane>;
    fn signal(&self, pane: &PaneId, signal: Signal) -> Result<()>;
    /// `hup` to the pane's group, then `kill` after `grace`. It returns
    /// once the child is reaped.
    fn close(&self, pane: &PaneId, grace: Duration) -> Result<Exit>;
    /// Drops an exited pane and its screen.
    fn forget(&self, pane: &PaneId) -> Result<()>;
    /// Tells every attach client on the host to show `pane`, and says how
    /// many it told.
    fn switch(&self, pane: &PaneId) -> Result<usize>;
}

/// Streams that hold a connection open. Separate from `Panes` so that a fake
/// for most tests does not have to implement them.
pub trait Live: Send + Sync {
    /// A pane's output: one repaint, then live output.
    fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> Result<Subscription>;
    /// The holder's events, from now on.
    fn watch(&self) -> Result<Events>;
}
