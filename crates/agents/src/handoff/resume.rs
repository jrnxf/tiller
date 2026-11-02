//! The resume brief: the newest handoff row for a project, resolved.

use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use tiller_core::domain::{
    Attention, AttentionKind, Card, CardOutcome, CardState, Handoff, Run, RunOutcome, RunRole,
};
use tiller_core::state::{find, get, list};
use tiller_core::{Result, Timestamp};

/// What a fresh orchestrator resumes from (`recovery.md` R02). Everything
/// in `cards`, `decisions` and `workers` is what the handoff named; what
/// those rows have become since is on each line, and what the handoff did
/// not know about is in `changed_since`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Brief {
    pub handoff_id: String,
    pub project_id: String,
    pub written_at: Timestamp,
    pub written_by_session_id: String,
    pub conclusions: String,
    pub next_action: String,
    pub cards: Vec<CardLine>,
    pub decisions: Vec<DecisionLine>,
    pub workers: Vec<WorkerLine>,
    pub changed_since: Changed,
    /// Ids the record names that no longer resolve. Never expected; kept
    /// visible rather than dropped, so a damaged record is not read as an
    /// empty fleet.
    pub unresolved: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CardLine {
    pub card: String,
    pub title: String,
    pub state: CardState,
    pub outcome: Option<CardOutcome>,
}

/// Whether an attention item the handoff listed still stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Pending,
    Answered,
    Withdrawn,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DecisionLine {
    pub attention_id: String,
    pub card: String,
    pub kind: AttentionKind,
    pub question: String,
    pub recommendation: Option<String>,
    pub standing: Standing,
    pub answer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WorkerLine {
    pub run_id: String,
    pub card: String,
    pub role: RunRole,
    /// The line the handoff recorded for the run.
    pub line: String,
    pub ended: bool,
    pub outcome: Option<RunOutcome>,
    /// The run's newest status line now, when it has moved on from `line`.
    pub status_now: Option<String>,
}

/// Open now, absent from the record.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Changed {
    pub cards: Vec<CardLine>,
    pub decisions: Vec<DecisionLine>,
    pub workers: Vec<WorkerLine>,
}

impl Brief {
    /// The brief from the project's newest handoff, or `None` when the
    /// project has never written one (the caller then starts from
    /// `tiller card list`).
    pub fn load(conn: &Connection, project_id: &str) -> Result<Option<Self>> {
        let Some(handoff) = find::<Handoff>(
            conn,
            "project_id = ?1 ORDER BY written_at DESC, id DESC",
            &[&project_id],
        )?
        else {
            return Ok(None);
        };
        let mut unresolved = Vec::new();

        let mut cards = Vec::new();
        for id in strings(&handoff.active_cards) {
            match find::<Card>(conn, "id = ?1", &[&id])? {
                Some(card) => cards.push(card_line(&card)),
                None => unresolved.push(id),
            }
        }

        let mut decisions = Vec::new();
        for id in strings(&handoff.pending_attention) {
            match find::<Attention>(conn, "id = ?1", &[&id])? {
                Some(item) => decisions.push(decision_line(conn, &item)?),
                None => unresolved.push(id),
            }
        }

        let mut workers = Vec::new();
        let recorded: Vec<(String, String)> = handoff
            .workers
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|worker| {
                let id = worker.get("run_id")?.as_str()?;
                let line = worker.get("line")?.as_str()?;
                Some((id.to_owned(), line.to_owned()))
            })
            .collect();
        for (id, line) in &recorded {
            match find::<Run>(conn, "id = ?1", &[id])? {
                Some(run) => workers.push(worker_line(conn, &run, line)?),
                None => unresolved.push(id.clone()),
            }
        }

        let changed_since = changed_since(conn, &handoff, &cards, &decisions, &workers)?;
        Ok(Some(Self {
            handoff_id: handoff.id,
            project_id: handoff.project_id,
            written_at: handoff.written_at,
            written_by_session_id: handoff.session_id,
            conclusions: handoff.conclusions,
            next_action: handoff.next_action,
            cards,
            decisions,
            workers,
            changed_since,
            unresolved,
        }))
    }
}

/// The ids in a JSON list of strings; anything else in it is skipped.
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn card_line(card: &Card) -> CardLine {
    CardLine {
        card: card.display_id.clone(),
        title: card.title.clone(),
        state: card.state,
        outcome: card.outcome,
    }
}

fn display_id(conn: &Connection, card_id: &str) -> Result<String> {
    Ok(get::<Card>(conn, "card", card_id)?.display_id)
}

fn decision_line(conn: &Connection, item: &Attention) -> Result<DecisionLine> {
    let standing = if item.withdrawn_at.is_some() {
        Standing::Withdrawn
    } else if item.answered_at.is_some() {
        Standing::Answered
    } else {
        Standing::Pending
    };
    Ok(DecisionLine {
        attention_id: item.id.clone(),
        card: display_id(conn, &item.card_id)?,
        kind: item.kind,
        question: item.question.clone(),
        recommendation: item.recommendation.clone(),
        standing,
        answer: item.answer.clone(),
    })
}

fn worker_line(conn: &Connection, run: &Run, recorded: &str) -> Result<WorkerLine> {
    let status_now = run
        .last_status_line
        .clone()
        .filter(|now| now.as_str() != recorded);
    Ok(WorkerLine {
        run_id: run.id.clone(),
        card: display_id(conn, &run.card_id)?,
        role: run.role,
        line: recorded.to_owned(),
        ended: run.ended_at.is_some(),
        outcome: run.outcome,
        status_now,
    })
}

/// The project's open cards, standing attention and active runs that the
/// handoff did not list.
fn changed_since(
    conn: &Connection,
    handoff: &Handoff,
    cards: &[CardLine],
    decisions: &[DecisionLine],
    workers: &[WorkerLine],
) -> Result<Changed> {
    let project = &handoff.project_id;
    let mut changed = Changed::default();
    for card in list::<Card>(
        conn,
        "project_id = ?1 AND state = 'open' ORDER BY priority, created_at",
        &[project],
    )? {
        if !cards.iter().any(|known| known.card == card.display_id) {
            changed.cards.push(card_line(&card));
        }
    }
    for item in list::<Attention>(
        conn,
        "answered_at IS NULL AND withdrawn_at IS NULL
         AND card_id IN (SELECT id FROM cards WHERE project_id = ?1) ORDER BY opened_at",
        &[project],
    )? {
        if !decisions.iter().any(|known| known.attention_id == item.id) {
            changed.decisions.push(decision_line(conn, &item)?);
        }
    }
    for run in list::<Run>(
        conn,
        "ended_at IS NULL AND card_id IN (SELECT id FROM cards WHERE project_id = ?1)
         ORDER BY started_at",
        &[project],
    )? {
        if !workers.iter().any(|known| known.run_id == run.id) {
            let line = run
                .last_status_line
                .clone()
                .unwrap_or_else(|| format!("{} started {}", run.role, run.started_at));
            changed.workers.push(worker_line(conn, &run, &line)?);
        }
    }
    Ok(changed)
}
