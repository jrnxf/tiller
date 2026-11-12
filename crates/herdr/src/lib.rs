//! The herdr adapter (T2.5): socket and CLI calls, one poller per source, and the layout rules from `docs/design/herdr-layout.md`.
//!
//! The parts about panes but not about herdr live in `tiller-panes`, and the
//! generic poller in `tiller-poll` (`terminal-holder.md` 4.1). `composer`,
//! `process` and `testing` re-export them, and `front`, `poller` and
//! `submit` wrap them, so their users need no change.

pub mod client;
pub use tiller_panes::composer;
pub mod front;
pub mod layout;
pub mod poller;
pub use tiller_panes::process;
pub mod submit;

#[cfg(any(test, feature = "testing"))]
pub mod fake;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
