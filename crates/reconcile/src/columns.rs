//! Board column derivation (`state.md` section 6). The one definition of a
//! column is the `card_column` view in `state.db`; this module reads it into
//! the shape the board, the CLI and the boot snapshot share: five columns, the
//! Asked column from unresolved asks, and the staleness thresholds the client
//! needs to age activity itself between events.
//!
//! Nothing here is stored. A column changes when the rows it derives from
//! change, and `stale` never moves a card by itself: reconcile decides, by
//! the recovery table, when a stale run becomes a `recovery` item.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tiller_core::domain::{CardColumn, Column};
use tiller_core::state::reads::AskedRow;
use tiller_core::{Store, Timestamp};

use crate::thresholds::Thresholds;

/// The staleness thresholds a client ages activity with, in minutes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientThresholds {
    pub worker_stale_minutes: i64,
    pub orchestrator_stale_minutes: i64,
    /// An unresolved ask older than this shows its age (section 6, row 5).
    pub ask_age_minutes: i64,
}

impl From<&Thresholds> for ClientThresholds {
    fn from(t: &Thresholds) -> Self {
        Self {
            worker_stale_minutes: t.worker_stale,
            orchestrator_stale_minutes: t.orchestrator_stale,
            ask_age_minutes: 1,
        }
    }
}

/// The board: every card in its derived column, plus the asks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Board {
    pub server_time: Timestamp,
    pub thresholds: ClientThresholds,
    /// Cards by column, each column oldest first. Every column is present,
    /// empty or not, so a client never guesses.
    pub columns: BTreeMap<Column, Vec<CardColumn>>,
    /// The Asked column: unresolved asks, oldest first.
    pub asked: Vec<AskedRow>,
}

/// The board for every project, or one.
pub fn board(
    store: &Store,
    project: Option<&str>,
    thresholds: &Thresholds,
    now: Timestamp,
) -> tiller_core::Result<Board> {
    let mut columns: BTreeMap<Column, Vec<CardColumn>> =
        Column::ALL.iter().map(|c| (*c, Vec::new())).collect();
    for card in store.card_columns(project)? {
        columns.entry(card.column).or_default().push(card);
    }
    Ok(Board {
        server_time: now,
        thresholds: ClientThresholds::from(thresholds),
        columns,
        asked: store.asked(project)?,
    })
}

/// One card's column and badge.
pub fn column(store: &Store, card_id: &str) -> tiller_core::Result<CardColumn> {
    store
        .card_column(card_id)?
        .ok_or_else(|| tiller_core::Error::NotFound {
            entity: "card",
            id: card_id.to_owned(),
            candidates: Vec::new(),
        })
}

/// The board filter a card's row first falls under: a card that waits on
/// the owner is "needs" in whatever column it is, everything else with an
/// active run or a landing is "active".
pub fn filter(card: &CardColumn) -> &'static str {
    if card.needs_you {
        return "needs";
    }
    match card.column {
        Column::Working | Column::Landing => "active",
        Column::Asked | Column::Queued | Column::Done => "all",
    }
}
