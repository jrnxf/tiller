//! What a session was launched with, and a restart waiting for it to go idle
//! (`state.md`, Session). A launch records the tiller version and a hash of
//! the role prompt, settings template and MCP config it wrote. Those decide
//! whether the session runs an older tiller than the host does now, and a
//! person's request to restart a working session waits in
//! `restart_queued_at` until it is idle.

use rusqlite::params;
use serde_json::json;

use super::get;
use crate::domain::{Run, Session};
use crate::error::{Error, Result};
use crate::store::{NewEvent, Store};
use crate::{Actor, Applied, Ctx};

impl Store {
    /// `session.stamp` (every launch): records the tiller version and launch
    /// hash a live session started with, and drops any queued restart, since
    /// a launch is what the restart was waiting for. Natural: the same stamp
    /// on an unqueued session writes nothing.
    pub fn session_stamp(
        &mut self,
        ctx: &Ctx,
        session: &str,
        tiller_version: &str,
        launch_hash: &str,
    ) -> Result<Applied<Session>> {
        self.write(ctx, "session.stamp", None, |tx| {
            let session: Session = get(tx.conn(), "session", session)?;
            if session.tiller_version.as_deref() == Some(tiller_version)
                && session.launch_hash.as_deref() == Some(launch_hash)
                && session.restart_queued_at.is_none()
            {
                return Ok(session);
            }
            tx.conn().execute(
                "UPDATE sessions SET tiller_version = ?2, launch_hash = ?3,
                        restart_queued_at = NULL, rev = rev + 1 WHERE id = ?1",
                params![session.id, tiller_version, launch_hash],
            )?;
            let stamped: Session = get(tx.conn(), "session", &session.id)?;
            let card = card_of(tx.conn(), &stamped)?;
            tx.event(
                NewEvent::new("session", &stamped.id, "session.stamped")
                    .rev(stamped.rev)
                    .card_opt(card)
                    .payload(json!({"tiller_version": tiller_version, "launch_hash": launch_hash})),
            )?;
            Ok(stamped)
        })
    }

    /// `session.restart_queue` (the board's Restart, `tiller agent restart
    /// --stale`): asks for a working session to restart when it next goes
    /// idle, or with `queued` false withdraws the ask. Owner, board or
    /// daemon only. Natural: a session already in that state is returned.
    pub fn session_restart_queue(
        &mut self,
        ctx: &Ctx,
        session: &str,
        queued: bool,
    ) -> Result<Applied<Session>> {
        self.write(ctx, "session.restart_queue", None, |tx| {
            if !matches!(tx.actor(), Actor::Owner | Actor::Board | Actor::Daemon) {
                return Err(Error::Forbidden(format!(
                    "{} may not queue a restart; the owner does",
                    tx.actor()
                )));
            }
            let session: Session = get(tx.conn(), "session", session)?;
            if session.ended_at.is_some() {
                return Err(Error::refused("session", &session.id, "session has ended"));
            }
            if session.restart_queued_at.is_some() == queued {
                return Ok(session);
            }
            let now = tx.now();
            tx.conn().execute(
                "UPDATE sessions SET restart_queued_at = ?2, rev = rev + 1 WHERE id = ?1",
                params![session.id, queued.then_some(now)],
            )?;
            let changed: Session = get(tx.conn(), "session", &session.id)?;
            let card = card_of(tx.conn(), &changed)?;
            let event = if queued {
                "session.restart_queued"
            } else {
                "session.restart_unqueued"
            };
            tx.event(
                NewEvent::new("session", &changed.id, event)
                    .rev(changed.rev)
                    .card_opt(card),
            )?;
            Ok(changed)
        })
    }

    /// The live sessions with a restart waiting, oldest ask first. A read.
    pub fn restart_queued(&self) -> Result<Vec<Session>> {
        super::list(
            self.conn(),
            "ended_at IS NULL AND restart_queued_at IS NOT NULL ORDER BY restart_queued_at, id",
            &[],
        )
    }
}

fn card_of(conn: &rusqlite::Connection, session: &Session) -> Result<Option<String>> {
    match &session.run_id {
        Some(run) => Ok(Some(get::<Run>(conn, "run", run)?.card_id)),
        None => Ok(None),
    }
}
