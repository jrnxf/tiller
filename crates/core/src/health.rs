//! The Health numbers as a report reads them (T7.1; `build-plan.md` T7.1).
//!
//! Tiller's own numbers come from the signals the Health page shows. The
//! same numbers are computed over Firstmate's history from an import bundle,
//! so the T7.4 report can set the two side by side. The baseline is for that
//! report only: nothing here feeds the Health page.
//!
//! Both sides produce one [`HealthNumbers`]: accepted outcomes, the
//! interventions that were needed, and avoidable interventions per accepted
//! outcome, each with its sample size.

use std::path::Path;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::import::Bundle;
use crate::state::interventions::InterventionCounts;
use crate::state::signals::Signals;

/// The days of Firstmate history the baseline covers
/// (`firstmate-export.md` Part C).
pub const BASELINE_DAYS: i64 = 21;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumbersSource {
    Tiller,
    Firstmate,
}

/// One figure and the sample it rests on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Figure {
    /// Absent when the sample is empty: a rate over nothing is not zero.
    pub value: Option<f64>,
    /// How many of `per` the value is over.
    pub sample: i64,
    pub per: String,
}

/// Accepted outcomes: cards that landed and scout cards that reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub landed: i64,
    pub reported: i64,
}

impl Accepted {
    pub fn total(&self) -> i64 {
        self.landed + self.reported
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HealthNumbers {
    pub source: NumbersSource,
    /// The window the numbers cover, first day to last day.
    pub from: String,
    pub to: String,
    pub accepted: Accepted,
    /// The times the developer was needed, by kind.
    pub interventions: InterventionCounts,
    /// Whether rework is counted. Firstmate's transcripts do not show it, so
    /// the baseline cannot say how much there was.
    pub rework_observed: bool,
    pub avoidable_per_outcome: Figure,
    pub caveats: Vec<String>,
}

/// The Lisbon calendar day of `millis`, the zone the weeks are cut in.
fn lisbon_day(millis: i64) -> String {
    let Some(ts) = crate::Timestamp::from_millis(millis) else {
        return String::new();
    };
    match jiff::tz::TimeZone::posix("WET0WEST,M3.5.0/1,M10.5.0") {
        Ok(zone) => ts.as_jiff().to_zoned(zone).date().to_string(),
        Err(_) => String::new(),
    }
}

fn per_outcome(avoidable: i64, accepted: i64) -> Figure {
    Figure {
        value: (accepted > 0).then(|| avoidable as f64 / accepted as f64),
        sample: accepted,
        per: "accepted outcomes".into(),
    }
}

impl HealthNumbers {
    /// Tiller's own numbers over the weeks in `signals`.
    pub fn from_signals(signals: &Signals) -> Self {
        let mut accepted = Accepted::default();
        let mut interventions = InterventionCounts::default();
        for week in &signals.weeks {
            accepted.landed += week.landed;
            accepted.reported += week.reported;
            interventions.correction += week.interventions.correction;
            interventions.unplanned_escalation += week.interventions.unplanned_escalation;
            interventions.rework += week.interventions.rework;
            interventions.planned += week.interventions.planned;
        }
        Self {
            source: NumbersSource::Tiller,
            from: signals
                .weeks
                .first()
                .map(|week| lisbon_day(week.start.as_millis()))
                .unwrap_or_default(),
            to: signals
                .weeks
                .last()
                .map(|week| lisbon_day(week.end.as_millis() - 1))
                .unwrap_or_default(),
            accepted,
            interventions,
            rework_observed: true,
            avoidable_per_outcome: per_outcome(interventions.avoidable(), accepted.total()),
            caveats: vec![
                "Each intervention is classified by rule when it is read; the developer's relabels win."
                    .into(),
                "The last week is still in progress.".into(),
            ],
        }
    }
}

/// One captain message about a landed card, from `baseline/touches.jsonl`.
#[derive(Clone, Debug, Deserialize)]
struct Touch {
    #[serde(default, alias = "card_id", alias = "id")]
    card: Option<String>,
    #[serde(default, alias = "class", alias = "kind")]
    classification: String,
}

fn refused(dir: &Path, fact: impl Into<String>) -> Error {
    Error::refused("bundle", dir.display().to_string(), fact)
}

fn day_of(text: &str) -> Option<Date> {
    text.get(..10)?.parse().ok()
}

impl HealthNumbers {
    /// Firstmate's numbers from `bundle`: landed cards from `cards.jsonl` and
    /// the captain's messages about them from `baseline/touches.jsonl`, over
    /// the last [`BASELINE_DAYS`] days up to the day the bundle was made (or
    /// the newest landed card when the manifest has no date).
    ///
    /// A bundle without a touches file is refused: with no touches the
    /// numbers would read as "no interventions", which is not what is known.
    pub fn from_baseline(bundle: &Bundle) -> Result<Self> {
        let path = bundle.dir.join("baseline").join("touches.jsonl");
        let text = std::fs::read_to_string(&path).map_err(|error| {
            refused(
                &bundle.dir,
                format!("cannot read baseline/touches.jsonl: {error}"),
            )
        })?;
        let mut touches = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let touch: Touch = serde_json::from_str(line).map_err(|error| {
                refused(
                    &bundle.dir,
                    format!(
                        "baseline/touches.jsonl line {} does not parse: {error}",
                        n + 1
                    ),
                )
            })?;
            touches.push(touch);
        }

        let landed_on = |outcome: &str| matches!(outcome, "merged" | "reported");
        let end = bundle
            .manifest
            .generated_at
            .as_deref()
            .and_then(day_of)
            .or_else(|| {
                bundle
                    .cards
                    .iter()
                    .filter_map(|card| card.done.as_ref())
                    .filter(|done| landed_on(&done.outcome))
                    .filter_map(|done| done.at.as_deref().and_then(day_of))
                    .max()
            })
            .ok_or_else(|| {
                refused(
                    &bundle.dir,
                    "no landed card and no generated_at, so the baseline has no window",
                )
            })?;
        let start = end
            .checked_sub(jiff::Span::new().days(BASELINE_DAYS - 1))
            .map_err(|error| {
                refused(&bundle.dir, format!("the window does not compute: {error}"))
            })?;

        let mut accepted = Accepted::default();
        let mut in_window = Vec::new();
        for card in &bundle.cards {
            let Some(done) = card.done.as_ref() else {
                continue;
            };
            let Some(at) = done.at.as_deref().and_then(day_of) else {
                continue;
            };
            if at < start || at > end {
                continue;
            }
            match done.outcome.as_str() {
                "merged" => accepted.landed += 1,
                "reported" => accepted.reported += 1,
                _ => continue,
            }
            in_window.push(card.id.as_str());
        }

        let mut interventions = InterventionCounts::default();
        let (mut unattributed, mut unrecognized) = (0, 0);
        for touch in &touches {
            let attributed = touch
                .card
                .as_deref()
                .is_some_and(|card| in_window.contains(&card));
            if !attributed {
                unattributed += 1;
                continue;
            }
            match touch.classification.as_str() {
                "correction" => interventions.correction += 1,
                "unplanned_escalation_answer" => interventions.unplanned_escalation += 1,
                "routine" => interventions.planned += 1,
                _ => unrecognized += 1,
            }
        }

        let mut caveats = vec![
            "Transcript attribution is approximate: a message counts for a card only when the transcript ties it to one.".to_owned(),
            "Dates are whole days, so the window edges are coarse.".to_owned(),
            "Rework is not visible in the transcripts and is not counted, so avoidable interventions are a floor.".to_owned(),
        ];
        if unattributed > 0 {
            caveats.push(format!(
                "{unattributed} messages name no card landed in the window and are left out."
            ));
        }
        if unrecognized > 0 {
            caveats.push(format!(
                "{unrecognized} messages carry a classification this tiller does not know and are left out."
            ));
        }
        Ok(Self {
            source: NumbersSource::Firstmate,
            from: start.to_string(),
            to: end.to_string(),
            accepted,
            interventions,
            rework_observed: false,
            avoidable_per_outcome: per_outcome(interventions.avoidable(), accepted.total()),
            caveats,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::Timestamp;
    use crate::state::signals::WeekCounts;

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn card(id: &str, outcome: &str, at: &str) -> serde_json::Value {
        json!({
            "id": id, "project": "alpha", "title": format!("card {id}"),
            "column": "done", "kind": "ship",
            "done": {"outcome": outcome, "at": at},
        })
    }

    /// A bundle made 2026-11-04 with four landed cards (one too old), one
    /// dropped card, and touches to match.
    fn bundle(touches: &[serde_json::Value]) -> (tempfile::TempDir, Bundle) {
        let dir = tempfile::tempdir().unwrap();
        let lines = |rows: &[serde_json::Value]| {
            rows.iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>()
        };
        let cards = lines(&[
            card("c1", "merged", "2026-11-03"),
            card("c2", "merged", "2026-10-20"),
            card("c3", "reported", "2026-10-28"),
            card("c4", "merged", "2026-09-01"),
            card("c5", "dropped", "2026-11-01"),
        ]);
        let projects = "[{\"id\":\"alpha\",\"delivery\":\"pr\"}]";
        fs::write(dir.path().join("projects.json"), projects).unwrap();
        fs::write(dir.path().join("cards.jsonl"), &cards).unwrap();
        fs::write(dir.path().join("memory.jsonl"), "").unwrap();
        fs::create_dir(dir.path().join("baseline")).unwrap();
        fs::write(
            dir.path().join("baseline").join("touches.jsonl"),
            lines(touches),
        )
        .unwrap();
        let manifest = json!({
            "schema": "tiller-import/v1", "mode": "dry", "host_label": "example",
            "generated_at": "2026-11-04T09:00:00Z",
            "counts": {"projects": 1, "cards": 5, "inflight": 0, "memory": 0},
            "files": {
                "projects.json": sha(projects.as_bytes()),
                "cards.jsonl": sha(cards.as_bytes()),
                "memory.jsonl": sha(b""),
            },
        });
        fs::write(dir.path().join("manifest.json"), manifest.to_string()).unwrap();
        let bundle = Bundle::read(dir.path()).unwrap();
        (dir, bundle)
    }

    #[test]
    fn the_baseline_counts_what_landed_in_the_window_and_the_touches_on_it() {
        let (_dir, bundle) = bundle(&[
            json!({"card": "c1", "classification": "correction", "quote": "no, use the other API"}),
            json!({"card": "c1", "classification": "routine", "quote": "merge it"}),
            json!({"card": "c3", "classification": "unplanned_escalation_answer", "quote": "yes go"}),
            // Landed outside the window, never landed, and named no card.
            json!({"card": "c4", "classification": "correction", "quote": "old"}),
            json!({"card": "c5", "classification": "correction", "quote": "dropped"}),
            json!({"card": null, "classification": "correction", "quote": "loose"}),
            json!({"card": "c2", "classification": "something_new", "quote": "?"}),
        ]);
        let numbers = HealthNumbers::from_baseline(&bundle).unwrap();

        assert_eq!(numbers.source, NumbersSource::Firstmate);
        assert_eq!(
            (numbers.from.as_str(), numbers.to.as_str()),
            ("2026-10-15", "2026-11-04")
        );
        assert_eq!(
            numbers.accepted,
            Accepted {
                landed: 2,
                reported: 1
            }
        );
        assert_eq!(
            numbers.interventions,
            InterventionCounts {
                correction: 1,
                unplanned_escalation: 1,
                rework: 0,
                planned: 1
            }
        );
        assert!(!numbers.rework_observed);
        assert_eq!(numbers.avoidable_per_outcome.sample, 3);
        assert_eq!(numbers.avoidable_per_outcome.value, Some(2.0 / 3.0));
        let caveats = numbers.caveats.join("\n");
        assert!(
            caveats.contains("3 messages name no card landed in the window"),
            "{caveats}"
        );
        assert!(
            caveats.contains("1 messages carry a classification"),
            "{caveats}"
        );
    }

    #[test]
    fn a_bundle_without_touches_is_refused_not_read_as_zero() {
        let (dir, bundle) = bundle(&[]);
        fs::remove_file(dir.path().join("baseline").join("touches.jsonl")).unwrap();
        let error = HealthNumbers::from_baseline(&bundle).unwrap_err();
        assert!(error.to_string().contains("touches.jsonl"), "{error}");
    }

    #[test]
    fn a_rate_over_no_outcomes_has_no_value() {
        let figure = per_outcome(3, 0);
        assert_eq!((figure.value, figure.sample), (None, 0));
    }

    #[test]
    fn tiller_numbers_sum_the_weeks_and_share_the_baseline_shape() {
        let start = Timestamp::from_millis(1_000).unwrap();
        let week = |landed, correction, planned| WeekCounts {
            start,
            end: start,
            landed,
            reported: 1,
            merged_prs: landed,
            needed: 0,
            answered: 0,
            median_answer_minutes: None,
            interventions: InterventionCounts {
                correction,
                unplanned_escalation: 0,
                rework: 1,
                planned,
            },
            reverts: 0,
            escaped: 0,
            rework: 0,
            lost_work: 0,
            duplicate_dispatches: 0,
            unauthorized: 0,
            cli_decisions: 0,
        };
        let signals = Signals {
            weeks: vec![week(2, 1, 4), week(1, 0, 2)],
            by_project: Vec::new(),
            interventions: Vec::new(),
            cli_decisions: Vec::new(),
            machines_live: 0,
            machines_total: 0,
        };
        let numbers = HealthNumbers::from_signals(&signals);
        assert_eq!(
            numbers.accepted,
            Accepted {
                landed: 3,
                reported: 2
            }
        );
        assert_eq!(numbers.interventions.avoidable(), 3);
        assert_eq!(numbers.avoidable_per_outcome.value, Some(3.0 / 5.0));
        assert_eq!(numbers.avoidable_per_outcome.sample, 5);
        let keys: Vec<_> = serde_json::to_value(&numbers)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys,
            [
                "accepted",
                "avoidable_per_outcome",
                "caveats",
                "from",
                "interventions",
                "rework_observed",
                "source",
                "to"
            ]
        );
    }
}
