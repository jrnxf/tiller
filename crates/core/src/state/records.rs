//! Obligations, artifacts, reviews and handoffs (`state.md` section 3).

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::outbox::{NewMessage, enqueue};
use super::projects::require_daemon;
use super::{get, list, refuse, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewObligation {
    pub project_id: String,
    pub card_id: Option<String>,
    pub what: String,
    pub trigger_kind: TriggerKind,
    pub trigger_event: Option<Value>,
    pub due_at: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewArtifact {
    pub card: String,
    pub run_id: Option<String>,
    pub kind: ArtifactKind,
    pub title: String,
    pub url: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewReview {
    pub run_id: String,
    pub reviewer: Reviewer,
    pub coverage: Coverage,
    pub findings_high: i64,
    pub findings_medium: i64,
    pub findings_low: i64,
    pub verdict: Verdict,
    pub source_url: Option<String>,
}

/// The new facts of a handoff; the rest is projected at write time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewHandoff {
    pub session_id: String,
    pub conclusions: String,
    pub next_action: String,
}

/// `obligation.open` inside a mutation.
pub(crate) fn open_obligation(tx: &mut Tx<'_>, new: &NewObligation) -> Result<Obligation> {
    required("what", &new.what)?;
    if let Some(card_id) = &new.card_id {
        let card: Card = get(tx.conn(), "card", card_id)?;
        if card.state != CardState::Open {
            return Err(Error::refused("card", &card.display_id, "card is closed"));
        }
    }
    match new.trigger_kind {
        TriggerKind::Event if new.trigger_event.is_none() => {
            return Err(Error::Usage(
                "an event obligation needs its trigger event".into(),
            ));
        }
        TriggerKind::Date if new.due_at.is_none() => {
            return Err(Error::Usage("a date obligation needs its due date".into()));
        }
        _ => {}
    }
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO obligations (id, project_id, card_id, what, trigger_kind, trigger_event, due_at, opened_at, opened_by)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            new.project_id,
            new.card_id,
            new.what,
            new.trigger_kind,
            new.trigger_event,
            new.due_at,
            tx.now(),
            tx.actor().to_string(),
        ],
    )?;
    tx.event(
        NewEvent::new("obligation", &id, "obligation.opened")
            .card_opt(new.card_id.clone())
            .payload(json!({"what": new.what, "trigger_kind": new.trigger_kind, "trigger_event": new.trigger_event, "due_at": new.due_at})),
    )?;
    get(tx.conn(), "obligation", &id)
}

/// `obligation.meet` inside the mutation that wrote the triggering event
/// `seq`. A standing obligation re-arms instead of closing. Enqueues the
/// note that fulfils it to the project's live orchestrator.
pub(crate) fn meet_obligation(
    tx: &mut Tx<'_>,
    obligation: &Obligation,
    seq: i64,
) -> Result<Obligation> {
    if !tx.events().contains(&seq) {
        return Err(Error::Internal(format!(
            "obligation {} met by event {seq}, which this transaction did not write",
            obligation.id
        )));
    }
    let to = obligation
        .state
        .meet(obligation.trigger_kind == TriggerKind::Standing)
        .map_err(refuse("obligation", &obligation.id))?;
    tx.conn().execute(
        "UPDATE obligations SET state = ?2, met_at = ?3, met_by_event_seq = ?4 WHERE id = ?1",
        params![obligation.id, to, tx.now(), seq],
    )?;
    tx.event(
        NewEvent::new("obligation", &obligation.id, "obligation.met")
            .card_opt(obligation.card_id.clone())
            .payload(json!({"event_seq": seq, "state": to})),
    )?;
    if let Some(orchestrator) = super::outbox::live_orchestrator(tx.conn(), &obligation.project_id)?
    {
        enqueue(
            tx,
            &NewMessage {
                target_session_id: orchestrator.id,
                kind: OutboxKind::Note,
                body: format!("obligation met: {}", obligation.what),
                card_id: obligation.card_id.clone(),
                reply_to: None,
            },
        )?;
    }
    get(tx.conn(), "obligation", &obligation.id)
}

/// Cancels an obligation in the caller's transaction: the state check, the
/// row and its `obligation.cancelled` event.
pub(crate) fn cancel_obligation(
    tx: &mut Tx<'_>,
    obligation: &Obligation,
    reason: &str,
) -> Result<()> {
    obligation
        .state
        .cancel()
        .map_err(refuse("obligation", &obligation.id))?;
    tx.conn().execute(
        "UPDATE obligations SET state = 'cancelled' WHERE id = ?1",
        [&obligation.id],
    )?;
    tx.event(
        NewEvent::new("obligation", &obligation.id, "obligation.cancelled")
            .card_opt(obligation.card_id.clone())
            .payload(json!({"reason": reason})),
    )?;
    Ok(())
}

impl Store {
    /// `obligation.open`.
    pub fn obligation_open(
        &mut self,
        ctx: &Ctx,
        new: NewObligation,
    ) -> Result<Applied<Obligation>> {
        self.write(ctx, "obligation.open", None, |tx| {
            get::<Project>(tx.conn(), "project", &new.project_id)?;
            let new = match &new.card_id {
                Some(card) => NewObligation {
                    card_id: Some(resolve_card(tx.conn(), card)?.id),
                    ..new.clone()
                },
                None => new.clone(),
            };
            open_obligation(tx, &new)
        })
    }

    /// `obligation.cancel`. Natural: a cancelled obligation is returned.
    pub fn obligation_cancel(
        &mut self,
        ctx: &Ctx,
        obligation: &str,
        reason: &str,
    ) -> Result<Applied<Obligation>> {
        required("reason", reason)?;
        self.write(ctx, "obligation.cancel", None, |tx| {
            let id = resolve_id(tx.conn(), "obligations", "obligation", obligation)?;
            let obligation: Obligation = get(tx.conn(), "obligation", &id)?;
            if obligation.state == ObligationState::Cancelled {
                return Ok(obligation);
            }
            cancel_obligation(tx, &obligation, reason)?;
            get(tx.conn(), "obligation", &obligation.id)
        })
    }

    /// `obligation.meet` (reconcile): writes the triggering event the caller
    /// names and meets the obligation in the same transaction.
    pub fn obligation_meet(
        &mut self,
        ctx: &Ctx,
        obligation: &str,
        trigger: NewEvent,
    ) -> Result<Applied<Obligation>> {
        self.write(ctx, "obligation.meet", None, |tx| {
            let id = resolve_id(tx.conn(), "obligations", "obligation", obligation)?;
            let obligation: Obligation = get(tx.conn(), "obligation", &id)?;
            let seq = tx.event(trigger)?;
            meet_obligation(tx, &obligation, seq)
        })
    }

    /// `artifact.add`.
    pub fn artifact_add(&mut self, ctx: &Ctx, new: NewArtifact) -> Result<Applied<Artifact>> {
        required("title", &new.title)?;
        if new.url.is_none() && new.path.is_none() {
            return Err(Error::Usage("an artifact needs a url or a path".into()));
        }
        self.write(ctx, "artifact.add", None, |tx| {
            let card = resolve_card(tx.conn(), &new.card)?;
            let id = ids::new_ulid();
            let session = tx.actor().session_id().map(str::to_owned);
            tx.conn().execute(
                "INSERT INTO artifacts (id, card_id, run_id, kind, title, url, path, created_at, created_by_session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![id, card.id, new.run_id, new.kind, new.title, new.url, new.path, tx.now(), session],
            )?;
            tx.event(
                NewEvent::new("artifact", &id, "artifact.added")
                    .card(&card.id)
                    .payload(json!({"kind": new.kind, "title": new.title, "url": new.url})),
            )?;
            get(tx.conn(), "artifact", &id)
        })
    }

    /// `artifact.retire`: the artifact's page is no longer served (cleanup
    /// ended its review listener), so nothing polls it any more. Retiring a
    /// retired artifact changes nothing.
    pub fn artifact_retire(&mut self, ctx: &Ctx, artifact: &str) -> Result<Applied<Artifact>> {
        self.write(ctx, "artifact.retire", None, |tx| {
            let id = resolve_id(tx.conn(), "artifacts", "artifact", artifact)?;
            let found: Artifact = get(tx.conn(), "artifact", &id)?;
            if found.retired_at.is_some() {
                return Ok(found);
            }
            tx.conn().execute(
                "UPDATE artifacts SET retired_at = ?2 WHERE id = ?1",
                params![found.id, tx.now()],
            )?;
            tx.event(
                NewEvent::new("artifact", &found.id, "artifact.retired")
                    .card(&found.card_id)
                    .payload(
                        json!({"kind": found.kind, "title": found.title, "run_id": found.run_id}),
                    ),
            )?;
            get(tx.conn(), "artifact", &found.id)
        })
    }

    /// `artifact.feedback` (daemon): an annotation on a review page that no
    /// session takes, because its card is closed or its run ended with no
    /// orchestrator to tell. It is recorded on the card's timeline and wakes
    /// nobody (`protocol.md` 3.5). Keyed by the entry, so a replay writes
    /// nothing.
    pub fn artifact_feedback(
        &mut self,
        ctx: &Ctx,
        key: &str,
        artifact: &str,
        entry: Value,
    ) -> Result<Applied<Artifact>> {
        require_daemon(&ctx.actor, "artifact.feedback")?;
        self.write(ctx, "artifact.feedback", Some(key), |tx| {
            let id = resolve_id(tx.conn(), "artifacts", "artifact", artifact)?;
            let found: Artifact = get(tx.conn(), "artifact", &id)?;
            tx.event(
                NewEvent::new("artifact", &found.id, "artifact.feedback")
                    .card(&found.card_id)
                    .payload(json!({"title": found.title, "run_id": found.run_id, "entry": entry})),
            )?;
            Ok(found)
        })
    }

    /// `review.record`.
    pub fn review_record(&mut self, ctx: &Ctx, new: NewReview) -> Result<Applied<Review>> {
        if new.findings_high < 0 || new.findings_medium < 0 || new.findings_low < 0 {
            return Err(Error::Usage("finding counts must not be negative".into()));
        }
        self.write(ctx, "review.record", None, |tx| {
            let run: Run = get(tx.conn(), "run", &resolve_id(tx.conn(), "runs", "run", &new.run_id)?)?;
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO reviews (id, run_id, card_id, reviewer, coverage, findings_high, findings_medium,
                                      findings_low, verdict, recorded_at, source_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    id,
                    run.id,
                    run.card_id,
                    new.reviewer,
                    new.coverage,
                    new.findings_high,
                    new.findings_medium,
                    new.findings_low,
                    new.verdict,
                    tx.now(),
                    new.source_url,
                ],
            )?;
            tx.event(
                NewEvent::new("review", &id, "review.recorded")
                    .card(&run.card_id)
                    .payload(json!({
                        "run_id": run.id,
                        "reviewer": new.reviewer,
                        "verdict": new.verdict,
                        "findings": [new.findings_high, new.findings_medium, new.findings_low],
                    })),
            )?;
            get(tx.conn(), "review", &id)
        })
    }

    /// `handoff.write`: the session's conclusions and next action, with the
    /// project's open cards, open attention and active runs projected now.
    pub fn handoff_write(&mut self, ctx: &Ctx, new: NewHandoff) -> Result<Applied<Handoff>> {
        required("conclusions", &new.conclusions)?;
        required("next action", &new.next_action)?;
        self.write(ctx, "handoff.write", None, |tx| {
            let session: Session = get(tx.conn(), "session", &new.session_id)?;
            if session.ended_at.is_some() {
                return Err(Error::refused("session", &session.id, "session has ended"));
            }
            let cards: Vec<Card> = list(
                tx.conn(),
                "project_id = ?1 AND state = 'open' ORDER BY priority, created_at",
                &[&session.project_id],
            )?;
            let attention: Vec<Attention> = list(
                tx.conn(),
                "answered_at IS NULL AND withdrawn_at IS NULL
                 AND card_id IN (SELECT id FROM cards WHERE project_id = ?1) ORDER BY opened_at",
                &[&session.project_id],
            )?;
            let runs: Vec<Run> = list(
                tx.conn(),
                "ended_at IS NULL AND card_id IN (SELECT id FROM cards WHERE project_id = ?1) ORDER BY started_at",
                &[&session.project_id],
            )?;
            let active_cards: Vec<&str> = cards.iter().map(|c| c.id.as_str()).collect();
            let pending: Vec<&str> = attention.iter().map(|a| a.id.as_str()).collect();
            let workers: Vec<Value> = runs
                .iter()
                .map(|r| json!({"run_id": r.id, "line": r.last_status_line.clone().unwrap_or_else(|| format!("{} started {}", r.role, r.started_at))}))
                .collect();
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO handoffs (id, project_id, session_id, written_at, active_cards, pending_attention, workers,
                                       conclusions, next_action)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    session.project_id,
                    session.id,
                    tx.now(),
                    json!(active_cards),
                    json!(pending),
                    json!(workers),
                    new.conclusions,
                    new.next_action,
                ],
            )?;
            tx.event(
                NewEvent::new("handoff", &id, "handoff.written")
                    .payload(json!({"session_id": session.id, "cards": active_cards.len(), "workers": workers.len()})),
            )?;
            get(tx.conn(), "handoff", &id)
        })
    }
}
