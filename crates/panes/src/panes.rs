//! The `Panes` trait (`terminal-holder.md` 4.2): what the daemon asks of the
//! holder.
//!
//! The trait follows the holder, not herdr. It has no workspaces, tabs,
//! layout or ordering. A pane is a child process with a key, metadata and a
//! screen. This holds the slice's methods; the rest of section 4.2 joins it
//! later. Live output (`subscribe`) is on [`HolderClient`](crate::HolderClient)
//! for now.

use crate::error::Result;
use crate::message::{Listing, Pane, Spawn};

pub trait Panes: Send + Sync {
    /// Starts a child, or returns the running pane that already holds `req.key`.
    fn spawn(&self, req: &Spawn) -> Result<Pane>;
    /// Every pane, with the holder instance it came from.
    fn list(&self) -> Result<Listing>;
}
