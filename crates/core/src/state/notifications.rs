//! `notification.record`: the daemon's write-ahead record that it is about to
//! notify the owner of an attention item (`protocol.md` 3.4, P36), then
//! `notification.sent` or `notification.fail`, the record of how the send
//! ended.

use rusqlite::{Connection, OptionalExtension};
use serde_json::json;

use super::projects::require_daemon;
use super::{get, resolve_id};
use crate::domain::Attention;
use crate::error::Result;
use crate::store::{Applied, Ctx, NewEvent, Store};

/// The event kinds of a notification's outcome, as an SQL list:
/// `attention.notify_sent` (`notification.sent`) and
/// `attention.notify_failed` (`notification.fail`).
pub const OUTCOME_KINDS: &str = "'attention.notify_sent', 'attention.notify_failed'";

/// The SQL condition that leaves out a write-ahead `attention.notified`
/// record once its outcome follows it, for a query over `events`: one
/// notification is one timeline line (`protocol.md` 3.4).
pub(crate) const ONE_LINE_PER_NOTIFICATION: &str = "NOT (events.kind = 'attention.notified' AND EXISTS (
    SELECT 1 FROM events AS outcome
    WHERE outcome.entity = 'attention' AND outcome.entity_id = events.entity_id
      AND outcome.kind IN ('attention.notify_sent', 'attention.notify_failed')))";

impl Store {
    /// `notification.record` (daemon): writes `attention.notified` for an
    /// open item, once. The daemon sends the desktop notification only when
    /// the call applied, so a restart, a second pass or an expired
    /// idempotency key never notifies twice. An item already answered or
    /// withdrawn needs nobody and records nothing.
    pub fn notification_record(&mut self, ctx: &Ctx, item: &str) -> Result<Applied<Attention>> {
        require_daemon(&ctx.actor, "notification.record")?;
        self.write(ctx, "notification.record", None, |tx| {
            let id = resolve_id(tx.conn(), "attention", "attention item", item)?;
            let item: Attention = get(tx.conn(), "attention item", &id)?;
            if item.answered_at.is_some() || item.withdrawn_at.is_some() {
                return Ok(item);
            }
            let notified: Option<i64> = tx
                .conn()
                .query_row(
                    "SELECT seq FROM events WHERE kind = 'attention.notified' AND entity_id = ?1",
                    [&item.id],
                    |row| row.get(0),
                )
                .optional()?;
            if notified.is_some() {
                return Ok(item);
            }
            let now = tx.now();
            tx.event(
                NewEvent::new("attention", &item.id, "attention.notified")
                    .rev(item.rev)
                    .card(&item.card_id)
                    .payload(json!({"kind": item.kind, "notified_at": now})),
            )?;
            Ok(item)
        })
    }

    /// `notification.sent` (daemon): writes `attention.notify_sent` for an
    /// item whose `attention.notified` record exists and has no outcome yet,
    /// once. The record came first and only says the daemon tried. This is
    /// the outcome when the send went out, so the timeline can say the owner
    /// was notified.
    pub fn notification_sent(&mut self, ctx: &Ctx, item: &str) -> Result<Applied<Attention>> {
        require_daemon(&ctx.actor, "notification.sent")?;
        self.write(ctx, "notification.sent", None, |tx| {
            let id = resolve_id(tx.conn(), "attention", "attention item", item)?;
            let item: Attention = get(tx.conn(), "attention item", &id)?;
            if !awaiting_outcome(tx.conn(), &item.id)? {
                return Ok(item);
            }
            tx.event(
                NewEvent::new("attention", &item.id, "attention.notify_sent")
                    .rev(item.rev)
                    .card(&item.card_id)
                    .payload(json!({"kind": item.kind})),
            )?;
            Ok(item)
        })
    }

    /// `notification.fail` (daemon): writes `attention.notify_failed` for an
    /// item whose `attention.notified` record exists and has no outcome yet,
    /// once. The record came first so a crash never sends twice, which means
    /// it says the daemon tried. This is the outcome when the send was
    /// refused: the timeline then shows the owner was not pinged.
    pub fn notification_fail(
        &mut self,
        ctx: &Ctx,
        item: &str,
        error: &str,
    ) -> Result<Applied<Attention>> {
        require_daemon(&ctx.actor, "notification.fail")?;
        self.write(ctx, "notification.fail", None, |tx| {
            let id = resolve_id(tx.conn(), "attention", "attention item", item)?;
            let item: Attention = get(tx.conn(), "attention item", &id)?;
            if !awaiting_outcome(tx.conn(), &item.id)? {
                return Ok(item);
            }
            tx.event(
                NewEvent::new("attention", &item.id, "attention.notify_failed")
                    .rev(item.rev)
                    .card(&item.card_id)
                    .payload(json!({"kind": item.kind, "error": error})),
            )?;
            Ok(item)
        })
    }
}

/// Whether the item has its write-ahead `attention.notified` record and no
/// outcome (`attention.notify_sent` or `attention.notify_failed`) yet.
fn awaiting_outcome(conn: &Connection, item: &str) -> Result<bool> {
    Ok(conn.query_row(
        &format!(
            "SELECT EXISTS (SELECT 1 FROM events WHERE entity = 'attention' AND entity_id = ?1
                              AND kind = 'attention.notified')
                AND NOT EXISTS (SELECT 1 FROM events WHERE entity = 'attention' AND entity_id = ?1
                                  AND kind IN ({OUTCOME_KINDS}))"
        ),
        [item],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::{ONE_LINE_PER_NOTIFICATION, OUTCOME_KINDS};

    /// The timeline filter names the same outcome kinds as the constant.
    #[test]
    fn the_timeline_filter_names_the_outcome_kinds() {
        assert!(
            ONE_LINE_PER_NOTIFICATION.contains(&format!("IN ({OUTCOME_KINDS})")),
            "{ONE_LINE_PER_NOTIFICATION}"
        );
    }
}
