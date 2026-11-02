//! Lifecycle transitions as functions that return `Result` (`state.md`
//! sections 3 and 5), so an illegal transition is refused in one place and
//! never written. The repository layer calls these before it writes; it never
//! sets a lifecycle column any other way.

use super::enums::{
    ApprovalState, CardKind, CardOutcome, CardState, Disposition, ObligationState, OutboxState,
    RunOutcome,
};

/// A transition the lifecycle does not allow, with the fact that forbids it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Illegal(pub String);

fn illegal(text: impl Into<String>) -> Illegal {
    Illegal(text.into())
}

/// A card's work state: open, or closed with an outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardStatus {
    Open,
    Closed(CardOutcome),
}

impl CardStatus {
    /// Reads the status from the `cards.state` and `cards.outcome` columns.
    pub fn from_columns(state: CardState, outcome: Option<CardOutcome>) -> Result<Self, Illegal> {
        match (state, outcome) {
            (CardState::Open, None) => Ok(CardStatus::Open),
            (CardState::Closed, Some(outcome)) => Ok(CardStatus::Closed(outcome)),
            (state, outcome) => Err(illegal(format!(
                "card state {state} with outcome {outcome:?} is not a valid status"
            ))),
        }
    }

    pub fn state(self) -> CardState {
        match self {
            CardStatus::Open => CardState::Open,
            CardStatus::Closed(_) => CardState::Closed,
        }
    }

    pub fn outcome(self) -> Option<CardOutcome> {
        match self {
            CardStatus::Open => None,
            CardStatus::Closed(outcome) => Some(outcome),
        }
    }

    /// Closes an open card with an outcome its kind allows (`state.md` section 7).
    pub fn close(self, kind: CardKind, outcome: CardOutcome) -> Result<Self, Illegal> {
        match self {
            CardStatus::Closed(current) => {
                Err(illegal(format!("card is already closed {current}")))
            }
            CardStatus::Open if outcome_allowed(kind, outcome) => Ok(CardStatus::Closed(outcome)),
            CardStatus::Open => Err(illegal(format!(
                "a {kind} card cannot close {outcome}; it closes {}",
                allowed_outcomes(kind)
                    .iter()
                    .map(|o| o.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    pub fn reopen(self) -> Result<Self, Illegal> {
        match self {
            CardStatus::Open => Err(illegal("card is already open")),
            CardStatus::Closed(_) => Ok(CardStatus::Open),
        }
    }
}

/// The outcomes a card of `kind` may close with: its own done outcome, plus the
/// four any card may end with.
pub fn allowed_outcomes(kind: CardKind) -> Vec<CardOutcome> {
    let done = match kind {
        CardKind::Ship => CardOutcome::Landed,
        CardKind::Scout => CardOutcome::Reported,
        CardKind::Question => CardOutcome::Answered,
        CardKind::Ops => CardOutcome::Completed,
    };
    vec![
        done,
        CardOutcome::Cancelled,
        CardOutcome::Superseded,
        CardOutcome::Failed,
        CardOutcome::Abandoned,
    ]
}

pub fn outcome_allowed(kind: CardKind, outcome: CardOutcome) -> bool {
    allowed_outcomes(kind).contains(&outcome)
}

/// True for the outcomes that release a dependency on the card (`state.md`,
/// Dependency): the work it waited on was delivered.
pub fn outcome_satisfies_dependants(outcome: CardOutcome) -> bool {
    matches!(
        outcome,
        CardOutcome::Landed
            | CardOutcome::Reported
            | CardOutcome::Answered
            | CardOutcome::Completed
    )
}

/// A run: active, or ended with an outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Active,
    Ended(RunOutcome),
}

impl RunStatus {
    pub fn from_outcome(outcome: Option<RunOutcome>) -> Self {
        outcome.map_or(RunStatus::Active, RunStatus::Ended)
    }

    pub fn end(self, outcome: RunOutcome) -> Result<Self, Illegal> {
        match self {
            RunStatus::Active => Ok(RunStatus::Ended(outcome)),
            RunStatus::Ended(current) => Err(illegal(format!("run already ended {current}"))),
        }
    }
}

/// An attention item: open, answered or withdrawn. Answered and withdrawn are
/// final; one row holds one answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttentionStatus {
    Open,
    Answered,
    Withdrawn,
}

impl AttentionStatus {
    pub fn from_columns(answered: bool, withdrawn: bool) -> Self {
        match (answered, withdrawn) {
            (true, _) => AttentionStatus::Answered,
            (false, true) => AttentionStatus::Withdrawn,
            (false, false) => AttentionStatus::Open,
        }
    }

    pub fn answer(self) -> Result<Self, Illegal> {
        match self {
            AttentionStatus::Open => Ok(AttentionStatus::Answered),
            AttentionStatus::Answered => Err(illegal("item is already answered")),
            AttentionStatus::Withdrawn => Err(illegal("item was withdrawn")),
        }
    }

    pub fn withdraw(self) -> Result<Self, Illegal> {
        match self {
            AttentionStatus::Open => Ok(AttentionStatus::Withdrawn),
            AttentionStatus::Answered => Err(illegal("item is already answered")),
            AttentionStatus::Withdrawn => Err(illegal("item is already withdrawn")),
        }
    }
}

/// How an executing approval finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    Succeeded,
    Failed,
    Uncertain,
}

impl ApprovalState {
    /// An approval that still binds a future action.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            ApprovalState::Requested | ApprovalState::Accepted | ApprovalState::Executing
        )
    }

    /// The owner or a grant decides a requested approval.
    pub fn decide(self, accept: bool) -> Result<Self, Illegal> {
        match self {
            ApprovalState::Requested if accept => Ok(ApprovalState::Accepted),
            ApprovalState::Requested => Ok(ApprovalState::Rejected),
            other => Err(illegal(format!("approval is {other}, not requested"))),
        }
    }

    /// The daemon starts the bound action.
    pub fn execute(self) -> Result<Self, Illegal> {
        match self {
            ApprovalState::Accepted => Ok(ApprovalState::Executing),
            other => Err(illegal(format!("approval is {other}, not accepted"))),
        }
    }

    /// The daemon records the result; an `uncertain` approval is resolved by
    /// reconcile from the forge (recovery.md R08).
    pub fn finish(self, finish: Finish) -> Result<Self, Illegal> {
        match (self, finish) {
            (ApprovalState::Executing, Finish::Succeeded)
            | (ApprovalState::Uncertain, Finish::Succeeded) => Ok(ApprovalState::Succeeded),
            (ApprovalState::Executing, Finish::Failed)
            | (ApprovalState::Uncertain, Finish::Failed) => Ok(ApprovalState::Failed),
            (ApprovalState::Executing, Finish::Uncertain) => Ok(ApprovalState::Uncertain),
            (other, _) => Err(illegal(format!("approval is {other}, not executing"))),
        }
    }

    /// The binding no longer matches reality. Only an approval that has not
    /// started executing can go stale.
    pub fn mark_stale(self) -> Result<Self, Illegal> {
        match self {
            ApprovalState::Requested | ApprovalState::Accepted => Ok(ApprovalState::Stale),
            other => Err(illegal(format!(
                "approval is {other}; only requested or accepted goes stale"
            ))),
        }
    }
}

impl OutboxState {
    /// The wake-up line reached the pane; a delivered message may ring again.
    pub fn deliver(self) -> Result<Self, Illegal> {
        match self {
            OutboxState::Pending | OutboxState::Delivered => Ok(OutboxState::Delivered),
            other => Err(illegal(format!("message is {other}"))),
        }
    }

    /// The target read the body. A pending message may be acked when the
    /// session found it by polling.
    pub fn ack(self) -> Result<Self, Illegal> {
        match self {
            OutboxState::Pending | OutboxState::Delivered => Ok(OutboxState::Acked),
            other => Err(illegal(format!("message is {other}"))),
        }
    }

    pub fn fail(self) -> Result<Self, Illegal> {
        match self {
            OutboxState::Pending | OutboxState::Delivered => Ok(OutboxState::Failed),
            other => Err(illegal(format!("message is {other}"))),
        }
    }

    /// Retargeting keeps the state; only an undelivered or unacked message moves.
    pub fn retarget(self) -> Result<Self, Illegal> {
        match self {
            OutboxState::Pending | OutboxState::Delivered => Ok(OutboxState::Pending),
            other => Err(illegal(format!("message is {other}"))),
        }
    }
}

impl Disposition {
    pub fn resolve(self, to: Disposition) -> Result<Self, Illegal> {
        match (self, to) {
            (_, Disposition::Unresolved) => Err(illegal("an ask cannot be resolved to unresolved")),
            (Disposition::Unresolved, to) => Ok(to),
            (current, _) => Err(illegal(format!("ask is already resolved {current}"))),
        }
    }
}

impl ObligationState {
    pub fn meet(self, standing: bool) -> Result<Self, Illegal> {
        match self {
            // A standing obligation re-arms instead of closing.
            ObligationState::Open if standing => Ok(ObligationState::Open),
            ObligationState::Open => Ok(ObligationState::Met),
            other => Err(illegal(format!("obligation is {other}"))),
        }
    }

    pub fn cancel(self) -> Result<Self, Illegal> {
        match self {
            ObligationState::Open => Ok(ObligationState::Cancelled),
            other => Err(illegal(format!("obligation is {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_closes_with_its_own_done_outcome_only() {
        let open = CardStatus::Open;
        assert!(open.close(CardKind::Ship, CardOutcome::Landed).is_ok());
        assert!(open.close(CardKind::Ship, CardOutcome::Reported).is_err());
        assert!(open.close(CardKind::Scout, CardOutcome::Landed).is_err());
        assert!(
            open.close(CardKind::Question, CardOutcome::Answered)
                .is_ok()
        );
        assert!(open.close(CardKind::Ops, CardOutcome::Completed).is_ok());
        for kind in CardKind::ALL {
            for outcome in [
                CardOutcome::Cancelled,
                CardOutcome::Superseded,
                CardOutcome::Failed,
                CardOutcome::Abandoned,
            ] {
                assert!(open.close(*kind, outcome).is_ok(), "{kind} {outcome}");
            }
        }
        let closed = CardStatus::Closed(CardOutcome::Landed);
        assert!(closed.close(CardKind::Ship, CardOutcome::Landed).is_err());
        assert_eq!(closed.reopen(), Ok(CardStatus::Open));
    }

    #[test]
    fn approval_lifecycle_has_no_way_back() {
        use ApprovalState::*;
        assert_eq!(Requested.decide(true), Ok(Accepted));
        assert_eq!(Requested.decide(false), Ok(Rejected));
        assert_eq!(Accepted.execute(), Ok(Executing));
        assert_eq!(Executing.finish(Finish::Uncertain), Ok(Uncertain));
        assert_eq!(Uncertain.finish(Finish::Succeeded), Ok(Succeeded));
        assert!(Uncertain.finish(Finish::Uncertain).is_err());
        assert!(Executing.mark_stale().is_err());
        for terminal in [Rejected, Succeeded, Failed, Stale] {
            assert!(terminal.decide(true).is_err());
            assert!(terminal.execute().is_err());
            assert!(terminal.finish(Finish::Succeeded).is_err());
            assert!(terminal.mark_stale().is_err());
        }
    }

    #[test]
    fn answered_and_withdrawn_items_are_final() {
        assert!(AttentionStatus::Answered.answer().is_err());
        assert!(AttentionStatus::Answered.withdraw().is_err());
        assert!(AttentionStatus::Withdrawn.answer().is_err());
        assert_eq!(
            AttentionStatus::Open.answer(),
            Ok(AttentionStatus::Answered)
        );
    }

    #[test]
    fn acked_and_failed_messages_are_final() {
        for terminal in [OutboxState::Acked, OutboxState::Failed] {
            assert!(terminal.deliver().is_err());
            assert!(terminal.ack().is_err());
            assert!(terminal.fail().is_err());
            assert!(terminal.retarget().is_err());
        }
        assert_eq!(OutboxState::Delivered.deliver(), Ok(OutboxState::Delivered));
    }
}
