//! The judged half of T5.2: one call to the pinned judge per ask with
//! differences, which classifies each as better, worse or neutral under the
//! pinned rubric and quotes its evidence. Every quote is then checked
//! against the input the judge was given, so a paraphrase shows as one.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiller_core::import::Bundle;

use crate::Error;
use crate::baseline::{ReplayAsk, task_id};
use crate::claude::Session;
use crate::compare::{Difference, Replayed};

/// The rubric every verdict is held to.
pub const RUBRIC: &str = include_str!("../prompts/judge-rubric.md");
/// The rubric's SHA-256. A rubric edit changes it, and a test fails until
/// it is updated here on purpose: every judged report is comparable.
pub const RUBRIC_SHA256: &str = "51159561b5112507fad3051f74462dddf9ba3cf74128cd3cedb9af770c116887";
/// The pinned judge.
pub const JUDGE_MODEL: &str = "claude-opus-5-5";
pub const JUDGE_EFFORT: &str = "high";

pub fn rubric_sha256() -> String {
    Sha256::digest(RUBRIC.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Better,
    Worse,
    Neutral,
    /// The judge gave no verdict for the difference.
    Unjudged,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Self::Better => "better",
            Self::Worse => "worse",
            Self::Neutral => "neutral",
            Self::Unjudged => "unjudged",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub source: String,
    pub quote: String,
    /// True when the quote appears in the judge's input as written.
    #[serde(default)]
    pub verified: bool,
}

/// The judge's verdict on one difference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Judgment {
    pub id: String,
    pub verdict: Verdict,
    #[serde(default)]
    pub evidence: Vec<Quote>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub blame: Blame,
    #[serde(default)]
    pub fix: Option<String>,
}

/// What made a "worse" likely, as the rubric's Blame section defines it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blame {
    /// Tiller would make the same choice live.
    Tiller,
    /// The replay lacked or misstated what the live orchestrator would see.
    Replay,
    /// A judgment call.
    #[default]
    None,
}

/// One ask's verdicts, as `judgments.jsonl` keeps them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Judged {
    pub index: usize,
    pub judgments: Vec<Judgment>,
    pub models: Vec<String>,
    pub cost_usd: f64,
}

/// What a model-driven replay logged for one turn (`turns.jsonl`).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Logged {
    pub index: usize,
    #[serde(default)]
    pub decisions: Value,
    #[serde(default)]
    pub board: Value,
}

/// Everything one ask's judge call is shown.
pub struct Case<'a> {
    pub ask: &'a ReplayAsk,
    pub project: &'a str,
    pub record: &'a Replayed,
    pub turn: Option<&'a Logged>,
    pub differences: Vec<&'a Difference>,
    /// Firstmate's cards by task id: title and how each ended.
    pub cards: &'a HashMap<String, Value>,
}

/// Firstmate's cards as the judge reads them, by task id.
pub fn bundle_cards(bundle: &Bundle) -> HashMap<String, Value> {
    bundle
        .cards
        .iter()
        .map(|c| {
            let key = task_id(c);
            let card = json!({
                "id": key, "project": c.project, "title": c.title, "kind": c.kind,
                "since": c.since, "column": c.column,
                "done": c.done.as_ref().map(|d| json!({"outcome": d.outcome, "at": d.at})),
            });
            (key, card)
        })
        .collect()
}

/// The judge's input for one ask.
pub fn prompt(case: &Case) -> String {
    let ask = case.ask;
    let fm_cards = ask
        .cards
        .iter()
        .enumerate()
        .map(|(i, id)| {
            json!({
                "card": case.cards.get(id).cloned().unwrap_or_else(|| json!({"id": id})),
                "kind": ask.kind.get(i).cloned().flatten(),
                "model": ask.model.get(i).cloned().flatten(),
                "effort": ask.effort.get(i).cloned().flatten(),
            })
        })
        .collect::<Vec<_>>();
    let firstmate = json!({
        "handling": ask.handling,
        "cards": fm_cards,
        "holds": ask.holds,
        "outcomes": ask.final_outcome,
        "linkage": ask.linkage,
    });
    let decisions = case.turn.map(|t| t.decisions.clone()).unwrap_or_else(|| {
        Value::Array(
            case.record
                .decisions
                .iter()
                .map(|s| s.decision.clone())
                .collect(),
        )
    });
    let refused = case
        .record
        .decisions
        .iter()
        .filter(|s| !s.ok)
        .map(|s| json!({"decision": s.decision["do"], "error": s.result}))
        .collect::<Vec<_>>();
    let runs = case
        .record
        .runs
        .iter()
        .map(|r| json!({"card": r.card, "role": r.role, "model": r.model, "effort": r.effort}))
        .collect::<Vec<_>>();
    let tiller = json!({
        "decisions": decisions,
        "refused": refused,
        "runs": runs,
        "linked_cards": case.record.cards.iter().map(|c| json!({
            "card": c.display_id, "firstmate_id": c.legacy_id, "kind": c.kind, "relation": c.relation,
        })).collect::<Vec<_>>(),
    });
    let differences = case
        .differences
        .iter()
        .map(|d| {
            json!({
                "id": d.id, "facet": d.facet, "card": d.card,
                "firstmate": d.firstmate, "tiller": d.tiller,
                "assumed": d.assumed, "note": d.note,
            })
        })
        .collect::<Vec<_>>();
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    format!(
        "## The ask\n\nSent {} to the `{}` orchestrator:\n\n<ask>\n{}\n</ask>\n\n\
         ## The board the replay was shown\n\n```json\n{}\n```\n\n\
         ## What Firstmate did\n\n```json\n{}\n```\n\nFirstmate's reply, shortened:\n\n<reply>\n{}\n</reply>\n\n\
         ## What Tiller's replay did\n\n```json\n{}\n```\n\n\
         ## The differences to classify\n\n```json\n{}\n```\n",
        ask.ts,
        case.project,
        ask.ask,
        pretty(&case.turn.map(|t| t.board.clone()).unwrap_or(Value::Null)),
        pretty(&firstmate),
        ask.reply_excerpt.as_deref().unwrap_or("(none recorded)"),
        pretty(&tiller),
        pretty(&Value::Array(differences)),
    )
}

/// The shape every verdict list is held to.
pub fn schema() -> Value {
    let text = json!({"type": "string"});
    json!({
        "type": "object",
        "properties": {
            "verdicts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": text,
                        "verdict": {"enum": ["better", "worse", "neutral"]},
                        "evidence": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "properties": {"source": text, "quote": text},
                                "required": ["source", "quote"],
                            },
                        },
                        "rationale": text,
                        "confidence": {"enum": ["low", "medium", "high"]},
                        "blame": {"enum": ["tiller", "replay", "none"]},
                        "fix": text,
                    },
                    "required": ["id", "verdict", "evidence", "rationale", "confidence", "blame"],
                },
            },
        },
        "required": ["verdicts"],
    })
}

/// A judge call's answer, checked: one verdict per difference (an extra
/// one dropped, a missing one `unjudged`) and every quote looked up in the
/// input.
pub fn checked(answer: &Value, case: &Case, input: &str) -> Result<Vec<Judgment>, Error> {
    let given: Vec<Judgment> = serde_json::from_value(answer["verdicts"].clone())?;
    let haystack = squash(input);
    Ok(case
        .differences
        .iter()
        .map(|d| match given.iter().find(|j| j.id == d.id) {
            Some(j) => {
                let mut j = j.clone();
                for quote in &mut j.evidence {
                    let needle = squash(&quote.quote);
                    quote.verified = !needle.is_empty() && haystack.contains(&needle);
                }
                j
            }
            None => Judgment {
                id: d.id.clone(),
                verdict: Verdict::Unjudged,
                evidence: Vec::new(),
                rationale: "the judge gave no verdict for this difference".into(),
                confidence: "low".into(),
                blame: Blame::None,
                fix: None,
            },
        })
        .collect())
}

/// Whitespace runs as one space, and JSON's escaped quotes as plain ones,
/// so a quote taken from pretty-printed JSON still matches.
fn squash(text: &str) -> String {
    text.replace("\\\"", "\"")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Judges each ask with differences, appending to `log` and skipping the
/// asks it already holds, so a judging run that stops can resume.
pub fn judge_all(
    session: &Session,
    cases: &[Case],
    log: &Path,
) -> Result<BTreeMap<usize, Judged>, Error> {
    let mut done = read(log)?;
    let schema = schema();
    for case in cases {
        let index = case.record.index;
        if case.differences.is_empty() || done.contains_key(&index) {
            continue;
        }
        let input = prompt(case);
        let answer = session.ask(&schema, &input)?;
        let mut judgments = checked(&answer.structured, case, &input)?;
        let mut cost = answer.cost_usd;
        // One retry for the differences the judge passed over.
        if judgments.iter().any(|j| j.verdict == Verdict::Unjudged) {
            let again = session.ask(
                &schema,
                &format!("{input}\nGive a verdict for every difference id listed above.\n"),
            )?;
            cost += again.cost_usd;
            let second = checked(&again.structured, case, &input)?;
            for (first, second) in judgments.iter_mut().zip(second) {
                if first.verdict == Verdict::Unjudged {
                    *first = second;
                }
            }
        }
        let judged = Judged {
            index,
            judgments,
            models: answer.models,
            cost_usd: cost,
        };
        let mut file = OpenOptions::new().create(true).append(true).open(log)?;
        writeln!(file, "{}", serde_json::to_string(&judged)?)?;
        done.insert(index, judged);
    }
    Ok(done)
}

/// The verdicts a log holds, by ask.
pub fn read(log: &Path) -> Result<BTreeMap<usize, Judged>, Error> {
    let Ok(text) = fs::read_to_string(log) else {
        return Ok(BTreeMap::new());
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let judged: Judged = serde_json::from_str(l)?;
            Ok((judged.index, judged))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::compare::Facet;

    #[test]
    fn the_rubric_is_pinned() {
        assert_eq!(
            rubric_sha256(),
            RUBRIC_SHA256,
            "the rubric changed: update RUBRIC_SHA256 on purpose"
        );
    }

    fn difference(id: &str) -> Difference {
        Difference {
            id: id.into(),
            index: 0,
            facet: Facet::Disposition,
            card: None,
            firstmate: "new".into(),
            tiller: "chat".into(),
            assumed: false,
            note: None,
        }
    }

    #[test]
    fn quotes_are_checked_and_missing_verdicts_are_unjudged() {
        let ask = ReplayAsk {
            ask: "please write the \"docs\"   page".into(),
            ..ReplayAsk::default()
        };
        let record = Replayed::default();
        let (one, two) = (difference("a0.1"), difference("a0.2"));
        let cards = HashMap::new();
        let case = Case {
            ask: &ask,
            project: "web",
            record: &record,
            turn: None,
            differences: vec![&one, &two],
            cards: &cards,
        };
        let input = prompt(&case);
        let answer = json!({"verdicts": [{
            "id": "a0.1", "verdict": "worse", "confidence": "high", "blame": "replay",
            "rationale": "asked for work",
            "evidence": [
                {"source": "ask", "quote": "write the \"docs\" page"},
                {"source": "ask", "quote": "write the documentation"},
            ],
        }]});
        let judged = checked(&answer, &case, &input).unwrap();
        assert_eq!(judged.len(), 2);
        assert_eq!(judged[0].verdict, Verdict::Worse);
        assert_eq!(judged[0].blame, Blame::Replay);
        assert!(judged[0].evidence[0].verified);
        assert!(!judged[0].evidence[1].verified);
        assert_eq!(judged[1].verdict, Verdict::Unjudged);
    }
}
