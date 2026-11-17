//! How full a session's context window is (`state.md`, Session). The
//! transcript tailer reads the newest assistant `usage` row of a session's
//! Claude Code transcript and records the prompt's tokens and the model's
//! window here. It is a cache of the transcript, like `activity`: no event,
//! no rev bump, and a lost value comes back with the next assistant line.

use rusqlite::params;

use super::get;
use crate::domain::Session;
use crate::error::Result;
use crate::store::Store;
use crate::time::Timestamp;
use crate::{Applied, Ctx};

/// One usage row: `used` is input + cache read + cache creation tokens,
/// `window` the model's context window when the model is known, and `at` the
/// transcript time of the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUse {
    pub used: i64,
    pub window: Option<i64>,
    pub at: Timestamp,
}

impl Store {
    /// `session.context` (the transcript tailer): stores `usage` on the
    /// session when it is newer than the one stored. An older or equal row,
    /// such as a re-read after a daemon restart, writes nothing.
    pub fn session_context_record(
        &mut self,
        ctx: &Ctx,
        session: &str,
        usage: ContextUse,
    ) -> Result<Applied<Session>> {
        self.write(ctx, "session.context", None, |tx| {
            tx.rebuildable();
            let session: Session = get(tx.conn(), "session", session)?;
            if session.context_at.is_some_and(|at| usage.at <= at) {
                return Ok(session);
            }
            tx.conn().execute(
                "UPDATE sessions SET context_used = ?2, context_window = ?3, context_at = ?4 WHERE id = ?1",
                params![session.id, usage.used, usage.window, usage.at],
            )?;
            get(tx.conn(), "session", &session.id)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Actor;
    use crate::state::tests::memory_store;

    #[test]
    fn only_a_newer_usage_row_is_stored() {
        let (mut store, session) = memory_store();
        let daemon = Ctx::new(Actor::Daemon);
        let at = |millis| Timestamp::from_millis(millis).unwrap();
        let first = ContextUse {
            used: 82_771,
            window: Some(1_000_000),
            at: at(2_000),
        };
        let stored = store
            .session_context_record(&daemon, &session, first)
            .unwrap();
        assert!(!stored.replayed);
        assert_eq!(
            (
                stored.value.context_used,
                stored.value.context_window,
                stored.value.context_at
            ),
            (Some(82_771), Some(1_000_000), Some(at(2_000)))
        );
        assert_eq!(stored.value.rev, 1, "a cache write bumps no rev");
        assert!(stored.events.is_empty(), "a cache write logs no event");

        let older = ContextUse {
            used: 10,
            window: None,
            at: at(1_000),
        };
        let kept = store
            .session_context_record(&daemon, &session, older)
            .unwrap();
        assert!(kept.replayed);
        assert_eq!(kept.value.context_used, Some(82_771));
        let again = store
            .session_context_record(&daemon, &session, first)
            .unwrap();
        assert!(again.replayed);
    }
}
