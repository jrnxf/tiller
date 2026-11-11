//! Step 6a: the two sweeps that turn a record into an owner-facing fact.
//!
//! - **An ask-user finding goes to the owner (G33, P09).** A `no-mistakes`
//!   review whose verdict is `ask-user` opens one `decision` attention item
//!   on its card, keyed by the review, so a worker never answers it and a
//!   repeated pass opens nothing twice.
//! - **An event retires its promise (P15).** An open `event` obligation
//!   whose trigger is `{"kind": "pr.checks"}` is met when a PR on its card
//!   (or in its project, when it has no card) is open with the wanted
//!   checks, which defaults to `success`. The meet writes its own trigger
//!   event and the note to the orchestrator in one transaction.

use serde_json::{Value, json};
use tiller_core::domain::{
    CardState, Obligation, ObligationState, Pr, PrState, Review, Reviewer, TriggerKind, Verdict,
};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::cards::reopened_seq;
use tiller_core::state::{get, list};
use tiller_core::{NewEvent, Result, Timestamp};

use crate::pass::{Reconciler, Report};

/// The trigger kind an obligation names to wait for a PR's checks.
pub const PR_CHECKS: &str = "pr.checks";

impl Reconciler {
    pub(crate) fn gates(&mut self, now: Timestamp, report: &mut Report) {
        if let Err(error) = self.ask_user_findings(now, report) {
            report.error("ask-user findings", &error);
        }
        if let Err(error) = self.event_obligations(now, report) {
            report.error("event obligations", &error);
        }
    }

    fn ask_user_findings(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let reviews: Vec<Review> = list(
            self.conn(),
            "reviewer = ?1 AND verdict = ?2 ORDER BY recorded_at, id",
            &[&Reviewer::NoMistakes, &Verdict::AskUser],
        )?;
        for review in reviews {
            let card: tiller_core::domain::Card = get(self.conn(), "card", &review.card_id)?;
            if card.state != CardState::Open {
                continue;
            }
            // A reopen starts the card over (`state.md` 6): a finding
            // recorded before the card's last reopen gated the work before
            // it, and is not raised on the work after it.
            let since = reopened_seq(self.conn(), &card)?;
            if !self.recorded_since("review", &review.id, "review.recorded", since)? {
                continue;
            }
            let item = NewAttention {
                card: card.id.clone(),
                question: format!(
                    "The no-mistakes pipeline on {} asks the developer a question it cannot settle itself",
                    card.display_id
                ),
                evidence: Some(
                    json!({
                        "review_id": review.id,
                        "findings": [review.findings_high, review.findings_medium, review.findings_low],
                        "source_url": review.source_url,
                    })
                    .to_string(),
                ),
                recommendation: Some(
                    "Open the pipeline run, read the finding and answer it yourself".into(),
                ),
                consequence: Some(
                    "The run stays at the gate until you answer; no worker answers it for you"
                        .into(),
                ),
                run_id: Some(review.run_id.clone()),
                ..NewAttention::default()
            };
            let key = format!("nm-ask-user:{}", review.id);
            // A key another actor stored first is reported once, and the
            // findings after it are still raised.
            let applied = self.store.attention_open(&self.ctx(now), Some(&key), item);
            if self.planted(&key, applied)?.is_some_and(|a| !a.replayed) {
                report.repair(format!(
                    "G33 no-mistakes ask-user finding on {} opened for the owner",
                    card.display_id
                ));
            }
        }
        Ok(())
    }

    fn event_obligations(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let open: Vec<Obligation> = list(
            self.conn(),
            "state = ?1 AND trigger_kind = ?2 ORDER BY opened_at, id",
            &[&ObligationState::Open, &TriggerKind::Event],
        )?;
        for obligation in open {
            let Some(trigger) = &obligation.trigger_event else {
                continue;
            };
            if trigger.get("kind").and_then(Value::as_str) != Some(PR_CHECKS) {
                continue;
            }
            let wanted = trigger
                .get("checks")
                .and_then(Value::as_str)
                .unwrap_or("success");
            let number = trigger.get("number").and_then(Value::as_i64);
            let prs: Vec<Pr> = match &obligation.card_id {
                Some(card) => list(
                    self.conn(),
                    "card_id = ?1 AND state = ?2 ORDER BY number",
                    &[card, &PrState::Open],
                )?,
                None => list(
                    self.conn(),
                    "project_id = ?1 AND state = ?2 ORDER BY number",
                    &[&obligation.project_id, &PrState::Open],
                )?,
            };
            let Some(pr) = prs.iter().find(|pr| {
                pr.checks.to_string() == wanted && number.is_none_or(|n| n == pr.number)
            }) else {
                continue;
            };
            let event = NewEvent::new("pr", &pr.id, PR_CHECKS)
                .card(&pr.card_id)
                .payload(json!({
                    "obligation_id": obligation.id,
                    "number": pr.number,
                    "checks": pr.checks,
                    "head_sha": pr.head_sha,
                }));
            match self
                .store
                .obligation_meet(&self.ctx(now), &obligation.id, event)
            {
                Ok(_) => report.repair(format!(
                    "P15 obligation {} met: checks on PR #{} are {wanted}",
                    obligation.id, pr.number
                )),
                Err(error) => report.error("obligation.meet", &error),
            }
        }
        Ok(())
    }
}
