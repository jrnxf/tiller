//! The difference report: every difference with its verdict and quoted
//! evidence, a summary by facet and verdict, what the summary cannot know,
//! and every "worse" with its fix or a line for the owner's acceptance.
//!
//! The report quotes the bundle, so it is written beside the private
//! records, never into a repository.

use std::collections::BTreeMap;
use std::fmt::Write;

use tiller_core::Timestamp;
use tiller_core::time::lisbon;

use crate::compare::{Difference, Facet};
use crate::judge::{Blame, Judged, Judgment, Verdict};

/// What the report header names.
pub struct Header {
    pub bundle: String,
    pub asks: usize,
    pub orchestrator: String,
    /// True when a model decided, so a second replay may decide otherwise.
    pub model: bool,
    pub judge: String,
    pub rubric_sha256: String,
    pub orchestrator_cost_usd: f64,
    pub judge_cost_usd: f64,
    /// Asks the replay could not run, or ran without a model.
    pub notes: Vec<String>,
}

/// An instant as Lisbon time with its weekday, `Thu 2026-11-05 14:00`.
pub fn lisbon_time(at: Timestamp) -> String {
    lisbon::local(at.as_jiff())
        .strftime("%a %Y-%m-%d %H:%M")
        .to_string()
}

pub fn render(
    header: &Header,
    differences: &[Difference],
    judged: &BTreeMap<usize, Judged>,
    generated: Timestamp,
) -> String {
    let verdict_of = |d: &Difference| -> Option<&Judgment> {
        judged
            .get(&d.index)
            .and_then(|j| j.judgments.iter().find(|j| j.id == d.id))
    };
    let verdict = |d: &Difference| verdict_of(d).map_or(Verdict::Unjudged, |j| j.verdict);
    let mut out = String::new();
    let w = &mut out;

    let _ = writeln!(w, "# Replay difference report\n");
    let _ = writeln!(w, "- Generated: {} (Europe/Lisbon)", lisbon_time(generated));
    let _ = writeln!(w, "- Bundle: `{}`, {} asks", header.bundle, header.asks);
    let _ = writeln!(w, "- Orchestrator: {}", header.orchestrator);
    let _ = writeln!(w, "- Judge: {}", header.judge);
    let _ = writeln!(
        w,
        "- Rubric: `prompts/judge-rubric.md`, sha256 `{}`",
        header.rubric_sha256
    );
    let _ = writeln!(
        w,
        "- Cost: ${:.2} replay, ${:.2} judging",
        header.orchestrator_cost_usd, header.judge_cost_usd
    );
    for note in &header.notes {
        let _ = writeln!(w, "- {note}");
    }

    // Summary.
    let mut table: BTreeMap<Facet, BTreeMap<Verdict, usize>> = BTreeMap::new();
    for d in differences {
        *table
            .entry(d.facet)
            .or_default()
            .entry(verdict(d))
            .or_default() += 1;
    }
    let verdicts = [
        Verdict::Better,
        Verdict::Worse,
        Verdict::Neutral,
        Verdict::Unjudged,
    ];
    let count = |v: Verdict| differences.iter().filter(|d| verdict(d) == v).count();
    let asks_with = {
        let mut set: Vec<usize> = differences.iter().map(|d| d.index).collect();
        set.dedup();
        set.len()
    };
    let _ = writeln!(w, "\n## Summary\n");
    let _ = writeln!(
        w,
        "{} differences across {} of {} asks: {} better, {} worse, {} neutral, {} unjudged.\n",
        differences.len(),
        asks_with,
        header.asks,
        count(Verdict::Better),
        count(Verdict::Worse),
        count(Verdict::Neutral),
        count(Verdict::Unjudged),
    );
    let _ = writeln!(w, "| Facet | Better | Worse | Neutral | Unjudged |");
    let _ = writeln!(w, "|---|---|---|---|---|");
    for facet in Facet::ALL {
        let row = table.get(&facet);
        let cell = |v| row.and_then(|r| r.get(&v)).copied().unwrap_or(0);
        let _ = writeln!(
            w,
            "| {} | {} | {} | {} | {} |",
            facet.name(),
            cell(verdicts[0]),
            cell(verdicts[1]),
            cell(verdicts[2]),
            cell(verdicts[3]),
        );
    }

    // Uncertainty.
    let assumed = differences.iter().filter(|d| d.assumed).count();
    let all: Vec<&Judgment> = judged.values().flat_map(|j| &j.judgments).collect();
    let low = all.iter().filter(|j| j.confidence == "low").count();
    let medium = all.iter().filter(|j| j.confidence == "medium").count();
    let quotes = all.iter().flat_map(|j| &j.evidence).count();
    let unverified = all
        .iter()
        .flat_map(|j| &j.evidence)
        .filter(|q| !q.verified)
        .count();
    let unquoted = all
        .iter()
        .filter(|j| j.verdict != Verdict::Unjudged && !j.evidence.iter().any(|q| q.verified))
        .count();
    let worse_by = |b: Blame| {
        all.iter()
            .filter(|j| j.verdict == Verdict::Worse && j.blame == b)
            .count()
    };
    let _ = writeln!(w, "\n## Uncertainty\n");
    if header.model {
        let _ = writeln!(
            w,
            "- One sample per ask. The orchestrator is a model; a second replay may decide some asks differently, and no difference here is measured for stability."
        );
    }
    let _ = writeln!(
        w,
        "- One judge, one call per ask. {low} of {} verdicts are low confidence and {medium} medium.",
        all.len()
    );
    let _ = writeln!(
        w,
        "- {assumed} of {} differences compare against an inferred Firstmate side (`assumed`): mostly dispatch, where the baseline names no model or effort and Firstmate's default rule is assumed.",
        differences.len()
    );
    let _ = writeln!(
        w,
        "- {unverified} of {quotes} quotes were not found verbatim in the judge's input; {unquoted} verdicts rest on no verified quote."
    );
    let _ = writeln!(
        w,
        "- Each ask was replayed alone. The bundle samples asks out of long conversations, so the orchestrator saw none of what came before an ask (Firstmate's proposals, earlier turns, workers' reports), which Firstmate and a live Tiller orchestrator both had. The judge blamed {} of {} \"worse\" verdicts on the replay rather than on Tiller.",
        worse_by(Blame::Replay),
        all.iter().filter(|j| j.verdict == Verdict::Worse).count()
    );
    let _ = writeln!(
        w,
        "- The board each ask was shown is rebuilt from the bundle's end state (filing and closing days, titles only), so it can differ from the board Firstmate saw that day. A card with no filing day is shown only from the first ask that names it, so it can be missing from earlier boards, and an undated card an ask follows up is shown on that ask's own board. A card Firstmate filed is shown after its ask only when the replay filed no card in its place."
    );
    let _ = writeln!(
        w,
        "- The baseline does not say whether a follow-up ask placed a hold or lifted it, and pairs new cards with the replay's by order only."
    );
    let _ = writeln!(
        w,
        "- Each ask goes to the orchestrator of its first Firstmate card's project, as the owner would have typed it there; that choice uses the baseline."
    );

    // Worse.
    let worse: Vec<(&Difference, &Judgment)> = differences
        .iter()
        .filter_map(|d| verdict_of(d).map(|j| (d, j)))
        .filter(|(_, j)| j.verdict == Verdict::Worse)
        .collect();
    let _ = writeln!(w, "\n## Every \"worse\"\n");
    if worse.is_empty() {
        let _ = writeln!(w, "None.");
    }
    for (d, j) in &worse {
        let possible = j
            .fix
            .as_deref()
            .map(|f| format!(" Possible fix: {}", one_line(f)))
            .unwrap_or_default();
        let status = match j.blame {
            Blame::Tiller => format!(
                "Tiller defect. Fix: {}",
                j.fix.as_deref().map_or("(none named)".into(), one_line)
            ),
            Blame::Replay => format!(
                "Replay limitation, for the owner's acceptance: the live orchestrator would have had what the replay lacked.{possible}"
            ),
            Blame::None => format!("Judgment call, for the owner's acceptance.{possible}"),
        };
        let _ = writeln!(w, "- **{}** ({}): {status}", d.id, d.facet.name());
    }

    // Every difference.
    let _ = writeln!(w, "\n## Every difference\n");
    let mut current = None;
    for d in differences {
        if current != Some(d.index) {
            current = Some(d.index);
            let _ = writeln!(w, "### Ask {}\n", d.index);
        }
        let card = d
            .card
            .as_deref()
            .map(|c| format!(" `{c}`"))
            .unwrap_or_default();
        let assumed = if d.assumed {
            " (Firstmate side assumed)"
        } else {
            ""
        };
        let _ = writeln!(
            w,
            "**{}** {}{card}: Firstmate `{}`, Tiller `{}`{assumed}",
            d.id,
            d.facet.name(),
            d.firstmate,
            d.tiller
        );
        if let Some(note) = &d.note {
            let _ = writeln!(w, "\n> {note}");
        }
        match verdict_of(d) {
            Some(j) => {
                let _ = writeln!(
                    w,
                    "\n- Verdict: **{}** ({} confidence){}",
                    j.verdict.name(),
                    if j.confidence.is_empty() {
                        "no"
                    } else {
                        &j.confidence
                    },
                    match j.blame {
                        Blame::Tiller => ", Tiller defect",
                        Blame::Replay => ", replay limitation",
                        Blame::None => "",
                    }
                );
                let _ = writeln!(w, "- Why: {}", one_line(&j.rationale));
                for q in &j.evidence {
                    let mark = if q.verified {
                        ""
                    } else {
                        " (not found verbatim)"
                    };
                    let _ = writeln!(
                        w,
                        "- Evidence ({}): \"{}\"{mark}",
                        q.source,
                        one_line(&q.quote)
                    );
                }
                if let Some(fix) = &j.fix {
                    let _ = writeln!(w, "- Fix: {}", one_line(fix));
                }
            }
            None => {
                let _ = writeln!(w, "\n- Verdict: **unjudged**");
            }
        }
        let _ = writeln!(w);
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::judge::Quote;

    #[test]
    fn the_report_lists_every_difference_and_each_worse() {
        let differences = vec![
            Difference {
                id: "a0.1".into(),
                index: 0,
                facet: Facet::Disposition,
                card: None,
                firstmate: "new".into(),
                tiller: "chat".into(),
                assumed: false,
                note: None,
            },
            Difference {
                id: "a3.1".into(),
                index: 3,
                facet: Facet::Dispatch,
                card: Some("web-docs".into()),
                firstmate: "builder sonnet medium".into(),
                tiller: "builder haiku low".into(),
                assumed: true,
                note: Some("assumed".into()),
            },
        ];
        let judged = BTreeMap::from([(
            0,
            Judged {
                index: 0,
                judgments: vec![Judgment {
                    id: "a0.1".into(),
                    verdict: Verdict::Worse,
                    evidence: vec![Quote {
                        source: "ask".into(),
                        quote: "write the docs".into(),
                        verified: true,
                    }],
                    rationale: "it asked for work".into(),
                    confidence: "high".into(),
                    blame: Blame::None,
                    fix: None,
                }],
                models: vec![],
                cost_usd: 0.5,
            },
        )]);
        let header = Header {
            bundle: "example".into(),
            asks: 5,
            orchestrator: "test".into(),
            model: true,
            judge: "test".into(),
            rubric_sha256: "0".into(),
            orchestrator_cost_usd: 1.0,
            judge_cost_usd: 0.5,
            notes: vec![],
        };
        let at = "2026-11-05T14:00:00Z".parse().unwrap();
        let text = render(&header, &differences, &judged, at);
        assert!(text.contains("Generated: Thu 2026-11-05 14:00"));
        assert!(text.contains(
            "2 differences across 2 of 5 asks: 0 better, 1 worse, 0 neutral, 1 unjudged."
        ));
        assert!(
            text.contains("**a0.1** (disposition): Judgment call, for the owner's acceptance.")
        );
        assert!(text.contains("**a3.1** dispatch `web-docs`: Firstmate `builder sonnet medium`, Tiller `builder haiku low` (Firstmate side assumed)"));
        assert!(text.contains("- Evidence (ask): \"write the docs\"\n"));
        assert!(text.contains("1 of 2 differences compare against an inferred Firstmate side"));
    }
}
