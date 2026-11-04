//! The orchestrator a replay consults once per ask. It sees what the live
//! orchestrator sees (the ask and the line intake added to its turn) and
//! answers with the `tiller` commands it would run, as [`Decision`]s; the
//! harness applies them. It never sees what Firstmate did.
//!
//! [`Recorded`] decides from the baseline itself, so a replay runs with no
//! model. [`External`] hands each turn to a program, which is how a
//! model-driven orchestrator plugs in.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use tiller_core::Timestamp;
use tiller_core::domain::{CardKind, Effort, RunRole};
use tiller_core::import::Bundle;

use crate::Error;
use crate::baseline::ReplayAsk;

/// One ask, as the orchestrator's turn begins.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Turn {
    /// The ask's position in the replay, from 0.
    pub index: usize,
    pub ask_id: String,
    pub project: String,
    pub text: String,
    /// What the `UserPromptSubmit` hook added to the turn.
    pub context: Option<String>,
}

/// One command the orchestrator runs. A `card` is a label an earlier `new`
/// gave, a Firstmate task id the import kept as `legacy_id`, or any Tiller
/// card reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Decision {
    /// `tiller ask resolve <ask> new` for the ask's first card, `tiller ask
    /// link <ask> new` after it. A card in another project is created there
    /// (`tiller card create`) and is not linked to the ask.
    New {
        label: Option<String>,
        project: Option<String>,
        kind: CardKind,
        title: String,
    },
    /// `tiller ask resolve <ask> followup:<card>`, or `ask link` after the first.
    Followup { card: String },
    /// `tiller card reopen`; an open card is returned as it is.
    Reopen { card: String, reason: String },
    /// `tiller ask resolve <ask> chat`.
    Chat { reason: Option<String> },
    /// `tiller ask resolve <ask> cancelled`.
    Cancelled { reason: Option<String> },
    /// `tiller spawn <card> <role> [--model --effort]`.
    Spawn {
        card: String,
        role: RunRole,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        effort: Option<Effort>,
    },
    /// `tiller card hold <card> --until --reason`.
    Hold {
        card: String,
        until: Timestamp,
        reason: String,
    },
    /// `tiller attention open <card> --kind decision`.
    Attention { card: String, question: String },
    /// The turn's last message, which the `Stop` hook checks.
    Reply { text: String },
}

pub trait Orchestrator {
    fn decide(&mut self, turn: &Turn) -> Result<Vec<Decision>, Error>;
}

/// What the bundle says about a Firstmate card.
#[derive(Clone, Debug)]
struct Known {
    project: String,
    title: String,
    since: Option<String>,
}

/// Decides each ask as Firstmate handled it: the same dispositions, card
/// kinds and holds, with Tiller's own dispatch rules choosing the model.
/// It proves the pipeline end to end; it cannot judge the orchestrator.
pub struct Recorded {
    asks: Vec<ReplayAsk>,
    cards: HashMap<String, Known>,
    now: Timestamp,
}

impl Recorded {
    /// `projects` maps a baseline card to the Tiller project the import put
    /// it in, since the import renames a project slug Tiller cannot take.
    pub fn new(asks: &[ReplayAsk], bundle: &Bundle, projects: &HashMap<String, String>) -> Self {
        let cards = bundle
            .cards
            .iter()
            .map(|card| {
                let key = card.legacy_id.clone().unwrap_or_else(|| card.id.clone());
                let known = Known {
                    project: projects
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| card.project.clone()),
                    title: card.title.clone(),
                    since: card.since.clone(),
                };
                (key, known)
            })
            .collect();
        Self {
            asks: asks.to_vec(),
            cards,
            now: Timestamp::now(),
        }
    }
}

/// Firstmate's task kinds as Tiller card kinds: a `captain` task waits on
/// the owner, which Tiller files as a question.
pub fn card_kind(kind: Option<&str>) -> CardKind {
    match kind {
        Some("ship") => CardKind::Ship,
        Some("scout") => CardKind::Scout,
        Some("captain") => CardKind::Question,
        _ => CardKind::Ops,
    }
}

/// The run a new card of `kind` gets, if any.
pub fn run_role(kind: CardKind) -> Option<RunRole> {
    match kind {
        CardKind::Ship => Some(RunRole::Builder),
        CardKind::Scout => Some(RunRole::Scout),
        CardKind::Question | CardKind::Ops => None,
    }
}

impl Orchestrator for Recorded {
    fn decide(&mut self, turn: &Turn) -> Result<Vec<Decision>, Error> {
        let ask = self
            .asks
            .get(turn.index)
            .ok_or_else(|| Error::Orchestrator(format!("no baseline line {}", turn.index)))?;
        if ask.cards.is_empty() {
            let chat = ask.handling.iter().any(|h| h == "chat");
            return Ok(vec![if chat {
                Decision::Chat {
                    reason: Some("answered in chat".into()),
                }
            } else {
                Decision::Cancelled { reason: None }
            }]);
        }
        let new_only = ask.handling.iter().all(|h| h == "new card");
        let mixed = !new_only && ask.handling.iter().any(|h| h == "new card");
        let day = ask.ts.get(..10);
        let mut decisions = Vec::new();
        let mut spawns = Vec::new();
        for (i, card) in ask.cards.iter().enumerate() {
            let known = self.cards.get(card);
            // Of an ask that both followed up and filed, the cards the bundle
            // dates to the ask's day are the ones it filed.
            let new = new_only || (mixed && known.and_then(|k| k.since.as_deref()) == day);
            let kind = card_kind(ask.kind.get(i).cloned().flatten().as_deref());
            if new {
                decisions.push(Decision::New {
                    label: Some(card.clone()),
                    project: known.map(|k| k.project.clone()),
                    kind,
                    title: known.map_or_else(|| card.clone(), |k| k.title.clone()),
                });
            } else {
                decisions.push(Decision::Reopen {
                    card: card.clone(),
                    reason: "a follow-up ask".into(),
                });
                decisions.push(Decision::Followup { card: card.clone() });
            }
            let held = ask.holds.iter().any(|h| &h.card == card);
            if let Some(role) = run_role(kind).filter(|_| !held) {
                spawns.push(Decision::Spawn {
                    card: card.clone(),
                    role,
                    model: None,
                    effort: None,
                });
            }
        }
        for hold in &ask.holds {
            let reason = hold
                .reason
                .clone()
                .unwrap_or_else(|| format!("waiting ({})", hold.kind));
            let until = hold
                .until
                .as_deref()
                .and_then(|day| format!("{day}T09:00:00Z").parse::<Timestamp>().ok())
                .filter(|until| *until > self.now);
            decisions.push(match until {
                Some(until) => Decision::Hold {
                    card: hold.card.clone(),
                    until,
                    reason,
                },
                None => Decision::Attention {
                    card: hold.card.clone(),
                    question: reason,
                },
            });
        }
        decisions.extend(spawns);
        Ok(decisions)
    }
}

/// Hands each turn to a program: the [`Turn`] as JSON on stdin, a JSON
/// array of [`Decision`]s expected on stdout. Whatever the program calls is
/// its own business; the harness calls nothing but it.
pub struct External {
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl Orchestrator for External {
    fn decide(&mut self, turn: &Turn) -> Result<Vec<Decision>, Error> {
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&serde_json::to_vec(turn)?)?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(Error::Orchestrator(format!(
                "{} exited {} on ask {}",
                self.program.display(),
                out.status,
                turn.index
            )));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| {
            Error::Orchestrator(format!(
                "{} printed no decision list: {e}",
                self.program.display()
            ))
        })
    }
}
