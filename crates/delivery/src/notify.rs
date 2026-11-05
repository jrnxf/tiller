//! Desktop notifications for the owner (`protocol.md` 3.4, P36): one when an
//! attention item opens, including the `merge` item for a PR ready for the
//! owner's word, and never for a routine turn.
//!
//! The record comes first. `notification.record` writes `attention.notified`
//! for the item, once, and only a call that applied sends: a restart or a
//! second pass finds the event and stays quiet, and a send that fails is
//! logged, never repeated. Only items opened within [`NEW_MINUTES`] and still
//! current count as new, so a daemon started on an old database does not
//! announce its backlog.

use std::time::Duration;

use tiller_core::domain::{Attention, AttentionKind, Card, Project};
use tiller_core::state::attention::stale;
use tiller_core::state::{get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_herdr::client::Herdr;
use tiller_herdr::poller::{PollError, Source};

/// How long after it opens an attention item still counts as new.
pub const NEW_MINUTES: i64 = 60;

/// What one pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The attention items a notification was sent for.
    pub notified: Vec<String>,
    /// `(item, error)` for each send herdr refused.
    pub failed: Vec<(String, String)>,
}

/// The notification loop for the local host.
pub struct Notifier<H: Herdr> {
    store: Store,
    herdr: H,
}

impl<H: Herdr> Notifier<H> {
    pub fn new(store: Store, herdr: H) -> Self {
        Self { store, herdr }
    }

    /// One pass: a notification for every new open item not yet notified.
    pub fn pass(&mut self, now: Timestamp) -> tiller_core::Result<Report> {
        let since = now.plus_minutes(-NEW_MINUTES);
        let items: Vec<Attention> = list(
            self.store.conn(),
            "answered_at IS NULL AND withdrawn_at IS NULL
               AND id IN (SELECT entity_id FROM events
                          WHERE kind = 'attention.opened' AND at >= ?1)
               AND id NOT IN (SELECT entity_id FROM events WHERE kind = 'attention.notified')
             ORDER BY opened_at, id",
            &[&since],
        )?;
        let ctx = Ctx::at(Actor::Daemon, now);
        let mut report = Report::default();
        for item in items {
            if stale(self.store.conn(), &item)?.is_some() {
                continue;
            }
            let (title, body) = text(&self.store, &item)?;
            if self.store.notification_record(&ctx, &item.id)?.replayed {
                continue;
            }
            match self.herdr.notify(&title, &body) {
                Ok(()) => report.notified.push(item.id),
                Err(error) => {
                    tracing::warn!(item = %item.id, %error, "herdr notification failed");
                    report.failed.push((item.id, error.to_string()));
                }
            }
        }
        Ok(report)
    }
}

/// The notification for `item`: the project and the card it names.
fn text(store: &Store, item: &Attention) -> tiller_core::Result<(String, String)> {
    let card: Card = get(store.conn(), "card", &item.card_id)?;
    let project: Project = get(store.conn(), "project", &card.project_id)?;
    let what = match item.kind {
        AttentionKind::Merge => "ready for your merge",
        AttentionKind::Decision => "needs a decision",
        AttentionKind::Credential => "needs a credential",
        AttentionKind::Review => "has a review for you",
        AttentionKind::Recovery => "needs recovery",
        AttentionKind::Report => "has a report",
    };
    Ok((
        format!("{}: {what}", project.name),
        format!("[{}] {}", card.display_id, item.question),
    ))
}

impl<H: Herdr + Send + 'static> Source for Notifier<H> {
    fn name(&self) -> String {
        "notify".to_owned()
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        self.pass(now).map(drop).map_err(PollError::State)
    }
}
