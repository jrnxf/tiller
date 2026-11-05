//! Observations (T2.6): hook events, transcript tailing and liveness, written
//! straight to `state.db` without the daemon (`protocol.md` sections 4 and 5).

pub mod harness;
pub mod hooks;
pub mod liveness;
pub mod merge;
pub mod session;
pub mod tailer;
pub mod wake;

#[cfg(test)]
pub(crate) mod testing;
