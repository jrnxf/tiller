//! The reconcile engine (T2.7): `docs/spec/recovery.md` as a loop over
//! cards, runs, panes, worktrees and PRs, plus the board column derivation.
//!
//! One [`Reconciler`] owns a `state.db` connection and the sources it reads
//! the world through ([`sources`]): the local herdr server, the agent
//! process probe, the worktree scanner, the forge, the session launcher and
//! the host readings. [`Reconciler::pass`] runs the eight steps of
//! `recovery.md` section 3 in order, each repair in its own transaction, so
//! a pass killed halfway leaves only committed single repairs and the next
//! pass repeats nothing.
//!
//! Three rules shape every row here:
//!
//! - **Reconcile repairs records, never work.** The only mutations a pass
//!   runs are the ones `recovery.md` section 6 lists, each behind its guard.
//!   Nothing here removes a worktree with unlanded or unknown commits, closes
//!   a card on inference, or types anything into a pane.
//! - **Absence of evidence is `unknown`, never `gone`.** A failed herdr read
//!   stops the liveness ladder for the pass; only a successful snapshot that
//!   positively lacks a pane, and an agent probe that positively finds no
//!   process, make a session *dead*.
//! - **Clocks start from events.** Every deadline counts from the event that
//!   first recorded the condition (`session.ended`, `outbox.enqueued`,
//!   `host.degraded`, `session.resume_attempted`), never from a pass's
//!   memory, so a daemon restart resets nothing.
//!
//! # The coalescing carry-over from T2.6
//!
//! Tool hooks coalesce for 60 s (`tiller_observe::hooks::TOOL_COALESCE_MS`)
//! while a `herdr:idle` replaces a hook `working` after 30 s
//! (`tiller_core::state::runs::HOOK_WORKING_HOLD_MS`). A herdr reading that
//! flips to idle in the middle of a long tool call therefore lands on the
//! session after 30 s and stays until the next tool hook, which the coalescing
//! no longer holds back once the activity is idle. The engine settles this by
//! never reading the idle or working verdict at all: liveness, silence and
//! every escalation here derive from the age of the newest observation of
//! any source (`session_activity.effective_activity = stale`), and the herdr
//! poller records an observation only when herdr's reading changes, so a
//! steady reading through a long turn writes nothing and a misread can only
//! delay staleness, never hasten an escalation. `tests/liveness.rs` proves it
//! on a 40-minute tool call.

// `src/testing.rs` is shared with the integration tests, so it names this
// crate the way they do.
extern crate self as tiller_reconcile;

pub mod columns;
pub mod git;
pub mod grants;
pub mod ladder;
pub mod pass;
pub mod recovery;
pub mod sources;
pub mod thresholds;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use pass::{Config, Reconciler, Report, Sources};
pub use tiller_core::{Error, Result};
