//! `tiller ahoy`: the recap and the decision walkthrough the owner asks for
//! with `/ahoy` (P35). It reads the record, so it needs no chat history: the
//! recap is what happened since the owner's previous ask, and the decisions
//! are every open attention item that still applies, most blocking first.

use clap::Args;
use serde_json::json;
use tiller_core::Timestamp;
use tiller_core::domain::{Ask, AskSource, Attention, AttentionKind, Card, Event};
use tiller_core::state::attention::stale;
use tiller_core::state::{get, list};

use crate::app::{App, Done, Failure, Outcome};

/// The recap window when the owner has no earlier ask to measure from.
const DEFAULT_WINDOW_MINUTES: i64 = 24 * 60;
/// The most events one recap lists.
const RECAP_LIMIT: i64 = 100;
/// The events worth a recap line: what the owner would want to hear happened.
const RECAP_KINDS: &str = "'card.created', 'card.closed', 'run.started', 'run.ended', \
    'attention.opened', 'attention.answered', 'attention.withdrawn', 'approval.requested', \
    'approval.decided', 'approval.finished', 'pr.opened', 'pr.merged', 'deploy.finished'";

#[derive(Args, Debug)]
pub struct Ahoy {
    /// Only this project; the calling orchestrator's project by default
    #[arg(long)]
    pub project: Option<String>,
    /// Recap from this time instead of the owner's previous ask
    #[arg(long)]
    pub since: Option<String>,
}

/// How much an item blocks, lowest first: a credential stops a worker, a
/// decision stops a card, a merge waits on the owner's word.
fn rank(kind: AttentionKind) -> u8 {
    match kind {
        AttentionKind::Credential => 0,
        AttentionKind::Decision => 1,
        AttentionKind::Merge => 2,
        AttentionKind::Recovery => 3,
        AttentionKind::Review => 4,
        AttentionKind::Report => 5,
    }
}

pub fn run(app: &App, args: Ahoy) -> Done {
    let store = app.read()?;
    let project = match args.project {
        Some(project) => Some(project),
        None => app.session(&store)?.map(|session| session.project_id),
    };
    let now = Timestamp::now();
    let since = match &args.since {
        Some(text) => crate::lisbon::parse_when(text)?,
        None => previous_ask(&store, project.as_deref())?
            .unwrap_or_else(|| now.plus_minutes(-DEFAULT_WINDOW_MINUTES)),
    };

    let events: Vec<Event> = list(
        store.conn(),
        &format!("at > ?1 AND kind IN ({RECAP_KINDS}) ORDER BY seq DESC LIMIT ?2"),
        &[&since, &RECAP_LIMIT],
    )?;
    let mut recap = Vec::new();
    for event in events.into_iter().rev() {
        let card: Option<Card> = match &event.card_id {
            Some(id) => Some(get(store.conn(), "card", id)?),
            None => None,
        };
        if project
            .as_deref()
            .is_some_and(|p| card.as_ref().is_some_and(|c| c.project_id != p))
        {
            continue;
        }
        recap.push(json!({
            "at": event.at,
            "kind": event.kind,
            "card": card.as_ref().map(|c| &c.display_id),
            "title": card.as_ref().map(|c| &c.title),
        }));
    }

    let items: Vec<Attention> = list(
        store.conn(),
        "answered_at IS NULL AND withdrawn_at IS NULL ORDER BY opened_at, id",
        &[],
    )?;
    let mut decisions = Vec::new();
    let mut superseded = 0;
    for item in items {
        let card: Card = get(store.conn(), "card", &item.card_id)?;
        if project.as_deref().is_some_and(|p| card.project_id != p) {
            continue;
        }
        if stale(store.conn(), &item)?.is_some() {
            superseded += 1;
            continue;
        }
        decisions.push((rank(item.kind), item, card));
    }
    // Most blocking first, then oldest; the sort is stable over `opened_at`.
    decisions.sort_by_key(|(rank, ..)| *rank);
    let decisions: Vec<_> = decisions
        .into_iter()
        .enumerate()
        .map(|(n, (_, item, card))| {
            json!({
                "position": n + 1,
                "id": item.id,
                "card": card.display_id,
                "title": card.title,
                "kind": item.kind,
                "question": item.question,
                "evidence": item.evidence,
                "recommendation": item.recommendation,
                "consequence": item.consequence,
                "options": item.options,
                "opened_at": item.opened_at,
            })
        })
        .collect();

    Ok(Outcome::read(json!({
        "since": since,
        "recap": recap,
        "decisions": decisions,
        "superseded": superseded,
        "walkthrough": "Say the order is your pick. Present decisions[0] alone with its \
            question, why it matters, its options and your recommendation. When the owner \
            answers, record it with `tiller attention answer`, then present the next.",
    })))
}

/// When the owner asked something before their latest ask, which is the
/// `/ahoy` itself while it is still unresolved.
fn previous_ask(
    store: &tiller_core::Store,
    project: Option<&str>,
) -> Result<Option<Timestamp>, Failure> {
    let asks: Vec<Ask> = list(
        store.conn(),
        "source IN ('prompt', 'board') AND (?1 IS NULL OR project_id = ?1)
         ORDER BY received_at DESC, id DESC LIMIT 2",
        &[&project],
    )?;
    let skip = usize::from(
        asks.first()
            .is_some_and(|ask| ask.source == AskSource::Prompt && ask.resolved_at.is_none()),
    );
    Ok(asks.get(skip).map(|ask| ask.received_at))
}
