//! `notification.record`: the daemon's write-ahead record that it is about to
//! notify the owner of an attention item (`protocol.md` 3.4, P36).

use rusqlite::OptionalExtension;
use serde_json::json;

use super::projects::require_daemon;
use super::{get, resolve_id};
use crate::domain::Attention;
use crate::error::Result;
use crate::store::{Applied, Ctx, NewEvent, Store};

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
}
