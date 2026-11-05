//! The mechanical half of T5.2: what a replay did with each ask, set
//! against what Firstmate did, one [`Difference`] per disagreement. Five
//! facets are compared: the disposition, the cards (which were filed or
//! followed up), each new card's kind, each card's dispatch (role, model,
//! effort) and each card's hold. A refused decision is a difference too.
//!
//! The baseline is thinner than the replay, so some of Firstmate's side is
//! inferred; a difference built on an inference says so in `assumed`.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tiller_core::import::Bundle;

use crate::baseline::ReplayAsk;
use crate::baseline::task_id;
use crate::board::filed_by;
use crate::orchestrator::{card_kind, hold_until, run_role};

/// What Firstmate is assumed to have dispatched when the baseline names no
/// model: its default rule.
pub const FIRSTMATE_DEFAULT: (&str, &str) = ("sonnet", "medium");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Facet {
    Disposition,
    Card,
    Kind,
    Dispatch,
    Hold,
    Refused,
}

impl Facet {
    pub const ALL: [Self; 6] = [
        Self::Disposition,
        Self::Card,
        Self::Kind,
        Self::Dispatch,
        Self::Hold,
        Self::Refused,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Disposition => "disposition",
            Self::Card => "card",
            Self::Kind => "kind",
            Self::Dispatch => "dispatch",
            Self::Hold => "hold",
            Self::Refused => "refused",
        }
    }
}

/// One way the replay and Firstmate disagreed about an ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Difference {
    /// `a<ask>.<n>`: stable for one replay and one comparison.
    pub id: String,
    pub index: usize,
    pub facet: Facet,
    /// The card it is about: Firstmate's task id, or Tiller's display id
    /// for a card only the replay has.
    pub card: Option<String>,
    pub firstmate: String,
    pub tiller: String,
    /// True when Firstmate's side is inferred, not recorded.
    pub assumed: bool,
    pub note: Option<String>,
}

/// What a replay record says the replay did: the parts of a
/// [`crate::Record`] the comparison reads.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Replayed {
    pub index: usize,
    #[serde(default)]
    pub project: String,
    pub disposition: String,
    #[serde(default)]
    pub cards: Vec<ReplayedCard>,
    #[serde(default)]
    pub decisions: Vec<ReplayedStep>,
    #[serde(default)]
    pub runs: Vec<ReplayedRun>,
    /// Why the orchestrator gave no decision list, when it gave none.
    #[serde(default)]
    pub unanswered: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReplayedCard {
    pub display_id: String,
    pub legacy_id: Option<String>,
    pub kind: String,
    pub relation: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReplayedStep {
    pub decision: Value,
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ReplayedRun {
    pub card: String,
    pub role: String,
    pub model: String,
    pub effort: String,
}

/// Firstmate's cards: their filing days, by task id.
pub struct Cards {
    since: HashMap<String, Option<String>>,
}

impl Cards {
    pub fn new(bundle: &Bundle) -> Self {
        let since = bundle
            .cards
            .iter()
            .map(|c| (task_id(c), c.since.clone()))
            .collect();
        Self { since }
    }

    fn filed(&self, ask: &ReplayAsk) -> Vec<String> {
        filed_by(ask, &self.since)
    }
}

/// A card the replay filed for the ask, in decision order.
struct Filed {
    display: String,
    kind: String,
    cross: Option<String>,
}

/// Every difference between the replay's records and the baseline, in
/// ask order. A record and a baseline line pair up by index. A card the
/// replay filed in place of a Firstmate card stands in for it in the
/// asks after: a follow-up on one matches a follow-up on the other. An
/// ask the orchestrator left unanswered has nothing to compare.
pub fn compare(asks: &[ReplayAsk], records: &[Replayed], cards: &Cards) -> Vec<Difference> {
    let mut out = Vec::new();
    let mut stand_ins = HashMap::new();
    for record in records.iter().filter(|r| r.unanswered.is_none()) {
        let Some(ask) = asks.get(record.index) else {
            continue;
        };
        let mut found = Vec::new();
        ask_differences(ask, record, cards, &mut stand_ins, &mut found);
        out.extend(found.into_iter().enumerate().map(|(n, mut d)| {
            d.id = format!("a{}.{}", record.index, n + 1);
            d
        }));
    }
    out
}

/// One ask's differences. `stand_ins` maps each Firstmate task id an
/// earlier ask filed to the replay's card paired with it, and gains this
/// ask's pairs.
fn ask_differences(
    ask: &ReplayAsk,
    record: &Replayed,
    cards: &Cards,
    stand_ins: &mut HashMap<String, String>,
    out: &mut Vec<Difference>,
) {
    let index = record.index;
    let mut push = |facet, card: Option<&str>, firstmate: String, tiller: String, assumed, note| {
        out.push(Difference {
            id: String::new(),
            index,
            facet,
            card: card.map(str::to_owned),
            firstmate,
            tiller,
            assumed,
            note,
        });
    };

    // Firstmate's side.
    let fm_new = cards.filed(ask);
    let fm_follow: Vec<&String> = ask.cards.iter().filter(|c| !fm_new.contains(c)).collect();
    let fm_kind = |card: &str| {
        let i = ask.cards.iter().position(|c| c == card);
        card_kind(
            i.and_then(|i| ask.kind.get(i).cloned().flatten())
                .as_deref(),
        )
    };
    let fm_field = |field: &[Option<String>], card: &str| {
        let i = ask.cards.iter().position(|c| c == card);
        i.and_then(|i| field.get(i).cloned().flatten())
    };

    // The replay's side.
    let mut filed = Vec::new();
    let mut labels: HashMap<String, String> = HashMap::new();
    for step in record.decisions.iter().filter(|s| s.ok) {
        if step.decision["do"] == "new" {
            let display = text(&step.result["card"]);
            if let Some(label) = step.decision["label"].as_str() {
                labels.insert(label.to_owned(), display.clone());
            }
            filed.push(Filed {
                display,
                kind: text(&step.result["kind"]),
                cross: step.result["cross_project"].as_str().map(str::to_owned),
            });
        }
    }
    let followed: Vec<&ReplayedCard> = record
        .cards
        .iter()
        .filter(|c| c.relation == "followup")
        .collect();

    // Disposition: the set of ways the ask was handled.
    let fm_set = disposition_set(!fm_new.is_empty(), !fm_follow.is_empty(), || {
        if ask.handling.iter().any(|h| h == "chat") {
            "chat".into()
        } else {
            "cancelled".into()
        }
    });
    let tl_set = disposition_set(!filed.is_empty(), !followed.is_empty(), || {
        record.disposition.clone()
    });
    if fm_set != tl_set {
        push(
            Facet::Disposition,
            None,
            joined(&fm_set),
            joined(&tl_set),
            false,
            None,
        );
    }

    // Cards: follow-ups by task id (or the card standing in for it), new
    // cards paired in order.
    let same = |fm: &str, t: &ReplayedCard| {
        t.legacy_id.as_deref() == Some(fm) || stand_ins.get(fm) == Some(&t.display_id)
    };
    let mut pairs: Vec<(String, String, bool)> = Vec::new();
    for card in &fm_follow {
        match followed.iter().find(|t| same(card, t)) {
            Some(t) => pairs.push(((*card).clone(), t.display_id.clone(), false)),
            None => push(
                Facet::Card,
                Some(card),
                format!("follow-up on {card}"),
                "no follow-up on it".into(),
                false,
                None,
            ),
        }
    }
    for t in &followed {
        if !fm_follow.iter().any(|fm| same(fm, t)) {
            let name = t.legacy_id.clone().unwrap_or_else(|| t.display_id.clone());
            push(
                Facet::Card,
                Some(&name),
                "no follow-up on it".into(),
                format!("follow-up on {}", t.display_id),
                false,
                None,
            );
        }
    }
    for (i, card) in fm_new.iter().enumerate() {
        match filed.get(i) {
            Some(t) => {
                pairs.push((card.clone(), t.display.clone(), true));
                let fm = fm_kind(card).to_string();
                if fm != t.kind {
                    push(
                        Facet::Kind,
                        Some(card),
                        fm,
                        t.kind.clone(),
                        false,
                        Some(format!("paired with {}", t.display)),
                    );
                }
            }
            None => push(
                Facet::Card,
                Some(card),
                format!("filed {card} ({})", fm_kind(card)),
                "no card for it".into(),
                false,
                None,
            ),
        }
    }
    for t in filed.iter().skip(fm_new.len()) {
        let place = t
            .cross
            .as_ref()
            .map(|p| format!(" in {p}"))
            .unwrap_or_default();
        push(
            Facet::Card,
            Some(&t.display),
            "no such card".into(),
            format!("filed {} ({}){place}", t.display, t.kind),
            false,
            None,
        );
    }

    // Holds and waits, by the card they name.
    let tl_holds: Vec<(String, Option<String>, String)> = record
        .decisions
        .iter()
        .filter(|s| s.ok)
        .filter_map(|s| {
            let what = match s.decision["do"].as_str()? {
                "hold" => format!("hold until {}", day(&s.decision["until"])),
                "attention" => "waits on the owner (attention)".to_owned(),
                "dep" => format!(
                    "waits on {} (dependency)",
                    text(&s.result["depends_on"]["display_id"])
                ),
                _ => return None,
            };
            let display = s.result["card"]["display_id"].as_str()?.to_owned();
            let legacy = s.result["card"]["legacy_id"].as_str().map(str::to_owned);
            Some((display, legacy, what))
        })
        .collect();
    // A hold already over when the ask came waits on the owner, as
    // [`crate::Recorded`] replays it.
    let fm_hold = |card: &str| -> Option<String> {
        let hold = ask.holds.iter().find(|h| h.card == card)?;
        Some(match (&hold.until, hold_until(ask, hold)) {
            (Some(until), Some(_)) => format!("hold until {until}"),
            (Some(until), None) => format!(
                "waits on the owner ({}, until {until} had passed)",
                hold.kind
            ),
            (None, _) => format!("waits on the owner ({})", hold.kind),
        })
    };
    let category = |s: &str| {
        if s.starts_with("waits") {
            "waits".to_owned()
        } else {
            s.to_owned()
        }
    };
    for (fm, tl, _) in &pairs {
        let theirs = fm_hold(fm);
        let ours = tl_holds
            .iter()
            .find(|(d, l, _)| d == tl || l.as_deref() == Some(fm.as_str()))
            .map(|(_, _, w)| w.clone());
        if theirs.as_deref().map(category) != ours.as_deref().map(category) {
            let followup = !fm_new.contains(fm);
            push(
                Facet::Hold,
                Some(fm),
                theirs.unwrap_or_else(|| "no hold".into()),
                ours.unwrap_or_else(|| "no hold".into()),
                false,
                followup.then(|| {
                    "the baseline does not say whether this ask placed the hold or lifted it".into()
                }),
            );
        }
    }
    for (display, legacy, what) in &tl_holds {
        let paired = pairs
            .iter()
            .any(|(fm, tl, _)| tl == display || legacy.as_deref() == Some(fm.as_str()));
        if !paired {
            let name = legacy.clone().unwrap_or_else(|| display.clone());
            push(
                Facet::Hold,
                Some(&name),
                fm_hold(&name).unwrap_or_else(|| "no hold".into()),
                what.clone(),
                false,
                None,
            );
        }
    }

    // Dispatch, per paired card.
    for (fm, tl, new) in &pairs {
        let run = record.runs.iter().find(|r| &r.card == tl);
        let ours = run.map(|r| format!("{} {} {}", r.role, r.model, r.effort));
        let model = fm_field(&ask.model, fm);
        let effort = fm_field(&ask.effort, fm);
        let held = ask.holds.iter().any(|h| &h.card == fm);
        let role = run_role(fm_kind(fm));
        // What Firstmate dispatched, as far as the baseline tells.
        let (theirs, assumed) = match (&model, role) {
            (Some(model), role) => {
                let role = role.map_or_else(|| "a worker".to_owned(), |r| r.to_string());
                let effort = effort
                    .clone()
                    .unwrap_or_else(|| "(effort unrecorded)".into());
                (Some(format!("{role} {model} {effort}")), false)
            }
            (None, Some(_)) if held => (None, true),
            (None, Some(role)) if *new => (
                Some(format!(
                    "{role} {} {}",
                    FIRSTMATE_DEFAULT.0, FIRSTMATE_DEFAULT.1
                )),
                true,
            ),
            (None, None) if *new => (None, true),
            // A follow-up with no model named: the baseline does not say
            // whether Firstmate dispatched again.
            _ => continue,
        };
        let differs = match (&theirs, run) {
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
            (Some(_), Some(r)) => {
                let role_differs = role.is_some_and(|role| role.to_string() != r.role);
                let model_differs = model.as_deref().unwrap_or(FIRSTMATE_DEFAULT.0) != r.model;
                let effort_differs = match (&effort, &model) {
                    (Some(e), _) => e != &r.effort,
                    (None, None) => FIRSTMATE_DEFAULT.1 != r.effort,
                    // A named model with no effort: nothing to compare.
                    (None, Some(_)) => false,
                };
                role_differs || model_differs || effort_differs
            }
        };
        if differs {
            let note = match (&theirs, held) {
                (None, true) => Some("Firstmate held the card, so no run is assumed".into()),
                (Some(_), _) if assumed => {
                    Some("the baseline names no model; Firstmate's default rule is assumed".into())
                }
                _ => None,
            };
            push(
                Facet::Dispatch,
                Some(fm),
                theirs.unwrap_or_else(|| "no run".into()),
                ours.unwrap_or_else(|| "no run".into()),
                assumed,
                note,
            );
        }
    }

    // Refused decisions.
    for step in record.decisions.iter().filter(|s| !s.ok) {
        let what = step.decision["do"].as_str().unwrap_or("?");
        let card = step.decision["card"].as_str();
        let card = card.map(|c| labels.get(c).cloned().unwrap_or_else(|| c.to_owned()));
        push(
            Facet::Refused,
            card.as_deref(),
            "-".into(),
            format!("{what} refused: {}", text(&step.result)),
            false,
            None,
        );
    }
    for (fm, tl, new) in pairs {
        if new {
            stand_ins.insert(fm, tl);
        }
    }
}

fn disposition_set(
    new: bool,
    followup: bool,
    otherwise: impl FnOnce() -> String,
) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if new {
        set.insert("new".to_owned());
    }
    if followup {
        set.insert("followup".to_owned());
    }
    if set.is_empty() {
        set.insert(otherwise());
    }
    set
}

fn joined(set: &BTreeSet<String>) -> String {
    set.iter().cloned().collect::<Vec<_>>().join(" + ")
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn day(value: &Value) -> String {
    value
        .as_str()
        .and_then(|s| s.get(..10))
        .unwrap_or("?")
        .to_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;
    use crate::baseline::BaselineHold;

    fn cards(since: &[(&str, &str)]) -> Cards {
        Cards {
            since: since
                .iter()
                .map(|(k, v)| ((*k).to_owned(), Some((*v).to_owned())))
                .collect(),
        }
    }

    fn ask(cards: &[&str], handling: &[&str], kind: &[&str]) -> ReplayAsk {
        ReplayAsk {
            ask: "invented".into(),
            ts: "2026-03-10T10:00:00Z".into(),
            cards: cards.iter().map(|c| (*c).to_owned()).collect(),
            handling: handling.iter().map(|h| (*h).to_owned()).collect(),
            kind: kind.iter().map(|k| Some((*k).to_owned())).collect(),
            model: vec![None; cards.len()],
            effort: vec![None; cards.len()],
            ..ReplayAsk::default()
        }
    }

    fn step(decision: Value, ok: bool, result: Value) -> ReplayedStep {
        ReplayedStep {
            decision,
            ok,
            result,
        }
    }

    fn run(card: &str, role: &str, model: &str, effort: &str) -> ReplayedRun {
        ReplayedRun {
            card: card.into(),
            role: role.into(),
            model: model.into(),
            effort: effort.into(),
        }
    }

    fn new_card(display: &str, kind: &str) -> (ReplayedStep, ReplayedCard) {
        (
            step(
                json!({"do": "new", "label": display, "kind": kind}),
                true,
                json!({"card": display, "kind": kind, "cross_project": null}),
            ),
            ReplayedCard {
                display_id: display.into(),
                legacy_id: None,
                kind: kind.into(),
                relation: "created".into(),
            },
        )
    }

    #[test]
    fn an_identical_replay_has_no_differences() {
        let asks = [ask(&["web-docs"], &["new card"], &["ship"])];
        let (s, c) = new_card("web-1", "ship");
        let record = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "new".into(),
            cards: vec![c],
            decisions: vec![s],
            runs: vec![run("web-1", "builder", "sonnet", "medium")],
            unanswered: None,
        };
        let found = compare(&asks, &[record], &cards(&[("web-docs", "2026-03-10")]));
        assert_eq!(found, vec![]);
    }

    #[test]
    fn chat_for_a_filed_card_differs_in_disposition_and_card_only() {
        let asks = [ask(&["web-docs"], &["new card"], &["ship"])];
        let record = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "chat".into(),
            ..Replayed::default()
        };
        let found = compare(&asks, &[record], &cards(&[("web-docs", "2026-03-10")]));
        let facets: Vec<_> = found.iter().map(|d| d.facet).collect();
        assert_eq!(facets, vec![Facet::Disposition, Facet::Card]);
        assert_eq!(found[0].firstmate, "new");
        assert_eq!(found[0].tiller, "chat");
        assert_eq!(found[0].id, "a0.1");
        assert_eq!(found[1].id, "a0.2");
    }

    #[test]
    fn kind_and_dispatch_differences_are_per_paired_card() {
        let mut asks = [ask(&["web-docs"], &["new card"], &["ship"])];
        asks[0].model = vec![Some("opus".into())];
        let (s, c) = new_card("web-1", "scout");
        let record = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "new".into(),
            cards: vec![c],
            decisions: vec![s],
            runs: vec![run("web-1", "scout", "sonnet", "medium")],
            unanswered: None,
        };
        let found = compare(&asks, &[record], &cards(&[("web-docs", "2026-03-10")]));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].facet, Facet::Kind);
        assert_eq!(
            (found[0].firstmate.as_str(), found[0].tiller.as_str()),
            ("ship", "scout")
        );
        assert_eq!(found[1].facet, Facet::Dispatch);
        assert_eq!(found[1].firstmate, "builder opus (effort unrecorded)");
        assert_eq!(found[1].tiller, "scout sonnet medium");
        assert!(!found[1].assumed);
    }

    #[test]
    fn a_default_dispatch_is_assumed_and_says_so() {
        let asks = [ask(&["web-docs"], &["new card"], &["ship"])];
        let (s, c) = new_card("web-1", "ship");
        let record = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "new".into(),
            cards: vec![c],
            decisions: vec![s],
            runs: vec![run("web-1", "builder", "opus", "medium")],
            unanswered: None,
        };
        let found = compare(&asks, &[record], &cards(&[("web-docs", "2026-03-10")]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].firstmate, "builder sonnet medium");
        assert!(found[0].assumed);
        assert!(found[0].note.is_some());
    }

    #[test]
    fn a_follow_up_compares_its_target_and_its_hold() {
        let mut asks = [ask(&["orbit-plan"], &["follow-up"], &["captain"])];
        asks[0].holds = vec![BaselineHold {
            card: "orbit-plan".into(),
            kind: "captain".into(),
            until: None,
            reason: None,
        }];
        let record = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "followup".into(),
            cards: vec![ReplayedCard {
                display_id: "orbit-3".into(),
                legacy_id: Some("orbit-plan".into()),
                kind: "question".into(),
                relation: "followup".into(),
            }],
            decisions: vec![step(
                json!({"do": "hold", "card": "orbit-3", "until": "2026-03-12T09:00:00Z"}),
                true,
                json!({"card": {"display_id": "orbit-3", "legacy_id": "orbit-plan"}}),
            )],
            runs: vec![],
            unanswered: None,
        };
        let found = compare(&asks, &[record], &cards(&[("orbit-plan", "2026-03-01")]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].facet, Facet::Hold);
        assert_eq!(found[0].firstmate, "waits on the owner (captain)");
        assert_eq!(found[0].tiller, "hold until 2026-03-12");
        assert!(found[0].note.is_some());
    }

    #[test]
    fn a_wrong_follow_up_target_and_a_refusal_are_differences() {
        let asks = [ask(&["web-login"], &["follow-up"], &["ship"])];
        let record = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "followup".into(),
            cards: vec![ReplayedCard {
                display_id: "web-9".into(),
                legacy_id: Some("web-docs".into()),
                kind: "ship".into(),
                relation: "followup".into(),
            }],
            decisions: vec![step(
                json!({"do": "spawn", "card": "web-9"}),
                false,
                json!("card web-9 is closed"),
            )],
            runs: vec![],
            unanswered: None,
        };
        let found = compare(&asks, &[record], &cards(&[]));
        let facets: Vec<_> = found.iter().map(|d| d.facet).collect();
        assert_eq!(facets, vec![Facet::Card, Facet::Card, Facet::Refused]);
        assert_eq!(found[1].card.as_deref(), Some("web-docs"));
        assert_eq!(found[2].tiller, "spawn refused: card web-9 is closed");
    }

    #[test]
    fn a_follow_up_on_the_replays_own_card_matches_firstmates() {
        let mut later = ask(&["web-docs"], &["follow-up"], &["ship"]);
        later.ts = "2026-03-11T10:00:00Z".into();
        let asks = [ask(&["web-docs"], &["new card"], &["ship"]), later];
        let (s, c) = new_card("web-1", "ship");
        let filed = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "new".into(),
            cards: vec![c],
            decisions: vec![s],
            runs: vec![run("web-1", "builder", "sonnet", "medium")],
            unanswered: None,
        };
        let followed = Replayed {
            index: 1,
            project: "web".into(),
            disposition: "followup".into(),
            cards: vec![ReplayedCard {
                display_id: "web-1".into(),
                legacy_id: None,
                kind: "ship".into(),
                relation: "followup".into(),
            }],
            ..Replayed::default()
        };
        let found = compare(
            &asks,
            &[filed, followed],
            &cards(&[("web-docs", "2026-03-10")]),
        );
        assert_eq!(found, vec![]);
    }

    #[test]
    fn a_hold_over_before_the_ask_waits_on_the_owner() {
        let mut asks = [ask(&["orbit-plan"], &["follow-up"], &["captain"])];
        asks[0].holds = vec![BaselineHold {
            card: "orbit-plan".into(),
            kind: "time".into(),
            until: Some("2026-03-01".into()),
            reason: None,
        }];
        let record = Replayed {
            index: 0,
            project: "web".into(),
            disposition: "followup".into(),
            cards: vec![ReplayedCard {
                display_id: "orbit-3".into(),
                legacy_id: Some("orbit-plan".into()),
                kind: "question".into(),
                relation: "followup".into(),
            }],
            decisions: vec![step(
                json!({"do": "attention", "card": "orbit-3"}),
                true,
                json!({"card": {"display_id": "orbit-3", "legacy_id": "orbit-plan"}}),
            )],
            runs: vec![],
            unanswered: None,
        };
        let found = compare(&asks, &[record], &cards(&[("orbit-plan", "2026-02-01")]));
        assert_eq!(found, vec![]);
    }
}
