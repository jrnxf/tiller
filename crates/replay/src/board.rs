//! The board an orchestrator sees when an ask arrives. The live orchestrator
//! reads it with `tiller card list`; a replay rebuilds it as it stood on the
//! ask's day, so a model never sees a card filed after the ask.
//!
//! The bundle is an export of the end state, so this is an estimate: a card
//! is shown when it was filed on or before the ask's day and closed on or
//! after it (or never). A card with no filing day is shown from the first
//! ask that names it, the earliest proof it existed, and hidden before. A
//! card a replayed ask filed is hidden up to that ask, which had not filed
//! it yet, and after it when the replay filed its own card in its place,
//! which the board shows instead. Paired in order, as the comparison pairs
//! them; with no card in its place, Firstmate's is shown from then on.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use tiller_core::domain::Card;
use tiller_core::import::bundle::BundleCard;

use tiller_core::Timestamp;

use crate::baseline::{ReplayAsk, task_id};

/// One card as a turn shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BoardCard {
    /// The reference to use in a decision: the card's display id.
    pub id: String,
    pub kind: String,
    pub title: String,
    /// The day the card was filed, when known.
    pub since: Option<String>,
    /// True for a card an earlier replayed ask filed.
    pub replayed: bool,
}

/// When each bundle card was open, and which cards the asks filed.
pub struct History {
    spans: Vec<Span>,
    /// Each bundle card's filing day, by Firstmate task id.
    since: HashMap<String, Option<String>>,
    /// The Firstmate task ids each ask filed, by ask index.
    filed: Vec<Vec<String>>,
    /// The first ask that filed each card.
    filed_at: HashMap<String, usize>,
    /// The earliest ask's time.
    earliest: Option<Timestamp>,
}

/// One bundle card: its Firstmate task id, filing day and closing day, and
/// the first ask that names it.
struct Span {
    key: String,
    since: Option<String>,
    closed: Option<String>,
    named: Option<usize>,
}

impl History {
    pub fn new(cards: &[BundleCard], asks: &[ReplayAsk]) -> Self {
        let spans = cards
            .iter()
            .map(|card| {
                let key = task_id(card);
                Span {
                    named: asks.iter().position(|ask| ask.cards.contains(&key)),
                    key,
                    since: card.since.clone(),
                    closed: card.done.as_ref().and_then(|d| d.at.clone()),
                }
            })
            .collect::<Vec<_>>();
        let since: HashMap<String, Option<String>> = spans
            .iter()
            .map(|s| (s.key.clone(), s.since.clone()))
            .collect();
        let filed: Vec<Vec<String>> = asks.iter().map(|ask| filed_by(ask, &since)).collect();
        let mut filed_at = HashMap::new();
        for (index, cards) in filed.iter().enumerate() {
            for card in cards {
                filed_at.entry(card.clone()).or_insert(index);
            }
        }
        let earliest = asks.iter().filter_map(|a| a.ts.parse().ok()).min();
        Self {
            spans,
            since,
            filed,
            filed_at,
            earliest,
        }
    }

    /// The day the bundle says a card was filed.
    pub fn since(&self, key: &str) -> Option<String> {
        self.since.get(key).cloned().flatten()
    }

    /// The Firstmate task ids the ask at `index` filed, in order.
    pub fn filed(&self, index: usize) -> &[String] {
        self.filed.get(index).map_or(&[], Vec::as_slice)
    }

    /// When the earliest ask came.
    pub fn earliest(&self) -> Option<Timestamp> {
        self.earliest
    }

    /// The Firstmate task ids of the cards open on `day` (the ask's
    /// `YYYY-MM-DD`), as the ask at `index` would have found them.
    /// `replaced` holds the filed cards the replay filed its own card for.
    pub fn open(&self, index: usize, day: &str, replaced: &HashSet<String>) -> Vec<&str> {
        self.spans
            .iter()
            .filter(|s| match &s.since {
                Some(since) => since.as_str() <= day,
                None => s.named.is_some_and(|named| named <= index),
            })
            .filter(|s| s.closed.as_deref().is_none_or(|closed| closed >= day))
            .filter(|s| {
                self.filed_at
                    .get(&s.key)
                    .is_none_or(|&at| at < index && !replaced.contains(&s.key))
            })
            .map(|s| s.key.as_str())
            .collect()
    }
}

/// The cards an ask filed, read as [`crate::Recorded`] reads them: every
/// card of an ask that only filed, and of one that also followed up, the
/// cards the bundle dates to the ask's day.
pub fn filed_by(ask: &ReplayAsk, since: &HashMap<String, Option<String>>) -> Vec<String> {
    let new_only = !ask.handling.is_empty() && ask.handling.iter().all(|h| h == "new card");
    let mixed = !new_only && ask.handling.iter().any(|h| h == "new card");
    let day = ask.ts.get(..10);
    ask.cards
        .iter()
        .filter(|card| new_only || (mixed && since.get(*card).and_then(Option::as_deref) == day))
        .cloned()
        .collect()
}

pub fn shown(card: &Card, since: Option<String>, replayed: bool) -> BoardCard {
    BoardCard {
        id: card.display_id.clone(),
        kind: card.kind.to_string(),
        title: card.title.clone(),
        since,
        replayed,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn card(id: &str, since: Option<&str>, closed: &str) -> BundleCard {
        serde_json::from_value(json!({
            "id": id, "project": "web", "title": id, "column": "done", "kind": "ship",
            "since": since, "done": {"outcome": "merged", "at": closed},
            "body": [], "runs": [], "depends_on": [],
        }))
        .unwrap()
    }

    fn ask(day: &str, cards: &[&str], handling: &str) -> ReplayAsk {
        ReplayAsk {
            ts: format!("{day}T10:00:00Z"),
            cards: cards.iter().map(|c| (*c).to_owned()).collect(),
            handling: vec![handling.to_owned()],
            ..ReplayAsk::default()
        }
    }

    #[test]
    fn an_undated_card_shows_from_the_first_ask_naming_it() {
        let cards = [
            card("dated", Some("2026-03-01"), "2026-03-09"),
            // Closed when the bundle was exported, with no filing day: it
            // proves nothing about the boards before the ask naming it.
            card("undated", None, "2026-03-09"),
            card("never-named", None, "2026-03-09"),
        ];
        let asks = [
            ask("2026-03-02", &[], "chat"),
            ask("2026-03-03", &["undated"], "follow-up"),
            ask("2026-03-04", &[], "chat"),
        ];
        let history = History::new(&cards, &asks);
        let none = HashSet::new();
        assert_eq!(history.open(0, "2026-03-02", &none), ["dated"]);
        assert_eq!(history.open(1, "2026-03-03", &none), ["dated", "undated"]);
        assert_eq!(history.open(2, "2026-03-04", &none), ["dated", "undated"]);
    }

    #[test]
    fn a_filed_card_shows_after_its_ask_only_with_no_replay_card_in_its_place() {
        let cards = [
            card("old", Some("2026-03-01"), "2026-03-09"),
            card("filed", Some("2026-03-03"), "2026-03-09"),
        ];
        let asks = [
            ask("2026-03-02", &[], "chat"),
            ask("2026-03-03", &["filed"], "new card"),
            ask("2026-03-04", &[], "chat"),
        ];
        let history = History::new(&cards, &asks);
        assert_eq!(history.filed(1), ["filed"]);
        let none = HashSet::new();
        let replaced = HashSet::from(["filed".to_owned()]);
        // Up to ask 1 it is not filed yet, whatever the replay did.
        for (index, day) in [(0, "2026-03-02"), (1, "2026-03-03")] {
            assert_eq!(history.open(index, day, &none), ["old"]);
            assert_eq!(history.open(index, day, &replaced), ["old"]);
        }
        // After it, the replay's own card stands in for it, if it filed one.
        assert_eq!(history.open(2, "2026-03-04", &replaced), ["old"]);
        assert_eq!(history.open(2, "2026-03-04", &none), ["old", "filed"]);
        assert_eq!(
            history.earliest(),
            Some("2026-03-02T10:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn an_ask_with_cards_but_no_handling_filed_none() {
        let mut empty = ask("2026-03-03", &["web-1"], "new card");
        empty.handling.clear();
        assert!(filed_by(&empty, &HashMap::new()).is_empty());
    }
}
