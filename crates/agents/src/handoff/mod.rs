//! `tiller handoff` and resume from the handoff record (T4.5).
//!
//! The orchestrator writes the record (`handoff.write`, in the state core);
//! this module is the other half. [`Brief`] is what a fresh orchestrator
//! reads to pick the project up again: the newest record for the project,
//! with each id resolved and every fact that moved since it was written
//! marked. [`signal_compactions`] is the breakpoint signal. Clearing a
//! session is never a Tiller action: the daemon types only a wake-up line,
//! `herdr` is denied to the orchestrator, and the note it receives tells it
//! to hand the owner the word, not to run it.

mod breakpoint;
mod resume;

pub use breakpoint::{BREAKPOINT_NOTE, CLEAR_HELP, signal_compactions};
pub use resume::{Brief, CardLine, Changed, DecisionLine, Standing, WorkerLine};

#[cfg(test)]
mod tests;
