//! The breakpoint signal: a `PreCompact` in a live orchestrator session
//! becomes one `note` for that session, asking for the handoff.

use rusqlite::params;
use tiller_core::domain::{OutboxKind, OutboxMessage};
use tiller_core::state::outbox::NewMessage;
use tiller_core::{Actor, Ctx, Result, Store, Timestamp};

/// How long a `PreCompact` stays worth a note. After a daemon outage an old
/// one is moot: the compaction it announced is long over.
const STALE_MINUTES: i64 = 30;

/// The note's body. The orchestrator writes the handoff, then tells the
/// owner; clearing is the owner's act (`orchestrator.md`, Breakpoints).
pub const BREAKPOINT_NOTE: &str = "compaction is starting: write the handoff now with \
`tiller handoff write --conclusions <t> --next <t>`, then tell the owner it is a good time \
to /clear. Only the owner clears a session.";

/// The help line `tiller handoff write` prints, so the orchestrator ends a
/// natural breakpoint with the word for the owner.
pub const CLEAR_HELP: &str = "handoff written: tell the owner in one line that it is a good \
time to /clear; only the owner clears a session";

/// Sends one note per `PreCompact` observation of a live orchestrator
/// session that has not had one, unless that session wrote a handoff since.
/// Idempotent: the observation id is the message key, so the hook and the
/// daemon's pass may both call it. Returns the notes written now.
pub fn signal_compactions(store: &mut Store, now: Timestamp) -> Result<Vec<OutboxMessage>> {
    let cutoff = now.plus_minutes(-STALE_MINUTES);
    let due: Vec<(String, String)> = {
        let mut stmt = store.conn().prepare(
            "SELECT o.id, o.session_id
               FROM observations o
               JOIN sessions s ON s.id = o.session_id
              WHERE o.source = 'hook' AND o.event = 'PreCompact'
                AND s.role = 'orchestrator' AND s.ended_at IS NULL
                AND o.observed_at >= ?1
                AND NOT EXISTS (SELECT 1 FROM outbox m WHERE m.idempotency_key = 'breakpoint:' || o.id)
                AND NOT EXISTS (SELECT 1 FROM handoffs h
                                 WHERE h.session_id = o.session_id AND h.written_at >= o.observed_at)
              ORDER BY o.observed_at, o.id",
        )?;
        stmt.query_map(params![cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let ctx = Ctx::new(Actor::Daemon);
    let mut sent = Vec::new();
    for (observation, session) in due {
        let key = format!("breakpoint:{observation}");
        let applied = store.outbox_enqueue(
            &ctx,
            Some(&key),
            NewMessage {
                target_session_id: session,
                kind: OutboxKind::Note,
                body: BREAKPOINT_NOTE.into(),
                card_id: None,
                reply_to: None,
            },
        )?;
        sent.push(applied.value);
    }
    Ok(sent)
}
