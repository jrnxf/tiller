//! The herdr adapter (T2.5): socket and CLI calls, one poller per source, and the layout rules from `docs/design/herdr-layout.md`.

pub mod client;
pub mod composer;
pub mod layout;
pub mod poller;

#[cfg(any(test, feature = "testing"))]
pub mod fake;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
