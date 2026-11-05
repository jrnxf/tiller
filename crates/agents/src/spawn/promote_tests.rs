//! P07: a scout is promoted to a ship run in its own worktree.

use tiller_core::domain::{AttentionKind, CardKind};
use tiller_core::state::attention::NewAttention;
use tiller_herdr::testing::owner;

use super::promote::{self, Request as PromoteRequest};
use super::tests::{Rig, refusal, world};
use super::*;

impl Rig {
    fn try_promote(&mut self, card: &Card) -> Result<Outcome> {
        let ctx = self.orchestrator();
        let world = world!(self);
        promote::promote(
            &mut self.fleet.store,
            &ctx,
            None,
            &world,
            PromoteRequest {
                card: card.display_id.clone(),
                asked: None,
                brief: None,
                harness: "claude".into(),
            },
        )
        .map(|applied| applied.value)
    }

    fn report_choices(&mut self, card: &Card) {
        self.fleet
            .store
            .attention_open(
                &owner(),
                None,
                NewAttention {
                    card: card.display_id.clone(),
                    kind: AttentionKind::Report,
                    question: "Build option A?".into(),
                    ..NewAttention::default()
                },
            )
            .unwrap();
    }

    fn card_row(&self, card: &Card) -> Card {
        get(self.fleet.store.conn(), "card", &card.id).unwrap()
    }
}

/// A scout card whose scout reported with a choice open, still holding its
/// worktree.
fn reported_scout(rig: &mut Rig) -> (Card, Spawned) {
    let card = rig.card(CardKind::Scout, None);
    let scout = rig.spawn(&card, RunRole::Scout);
    std::fs::write(PathBuf::from(&scout.worktree).join(".notes"), "").unwrap();
    std::fs::remove_file(PathBuf::from(&scout.worktree).join(".notes")).unwrap();
    rig.report_choices(&card);
    rig.done(&scout.run.id, RunOutcome::Reported).unwrap();
    (card, scout)
}

#[test]
fn promote_scout_reuses_worktree_as_ship_run() {
    let mut rig = Rig::new();
    let (card, scout) = reported_scout(&mut rig);

    // The scout reported, but its card waits on the developer: its
    // worktree is not returned under the promotion.
    let error = rig.cleanup(&scout.run.id).unwrap_err();
    assert!(
        refusal(&error).contains("waits for the developer"),
        "{error}"
    );

    let Outcome::Spawned(builder) = rig.try_promote(&card).unwrap() else {
        panic!("expected a new run");
    };
    assert_eq!(builder.run.role, RunRole::Builder);
    assert_eq!(builder.worktree, scout.worktree);
    assert_eq!(builder.branch, scout.branch);
    assert_eq!(
        builder.run.predecessor_run_id.as_deref(),
        Some(scout.run.id.as_str())
    );
    assert_eq!(builder.base_sha, scout.base_sha);
    // One lease: the pool was asked for a worktree once.
    assert_eq!(rig.pool.taken().len(), 1);
    // The builder works in a pane of its own, in the scout's directory.
    assert_ne!(builder.pane_id, scout.pane_id);

    let card_now = rig.card_row(&card);
    assert_eq!(card_now.kind, CardKind::Ship);
    assert_eq!(rig.events("card.promoted", &card.id), 1);
    let open: i64 = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM attention WHERE card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL",
            [&card.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(open, 0);

    // The scout's run cannot hand the worktree back while the builder
    // works in it, and the builder's cleanup uses the scout's lease.
    let error = rig.cleanup(&scout.run.id).unwrap_err();
    assert!(refusal(&error).contains("in use by run"), "{error}");
    rig.done(&builder.run.id, RunOutcome::Abandoned).unwrap();
    let cleaned = rig.cleanup(&builder.run.id).unwrap();
    assert_eq!(cleaned.returned.as_deref(), Some(scout.worktree.as_str()));
    assert_eq!(
        rig.pool.returned(),
        vec![(
            PathBuf::from(&scout.worktree),
            format!("tiller:{}", scout.run.id)
        )]
    );
}

#[test]
fn promote_refuses_what_is_not_a_reported_scout() {
    let mut rig = Rig::new();

    // A ship card has no scout to promote.
    let ship = rig.card(CardKind::Ship, None);
    let error = rig.try_promote(&ship).unwrap_err();
    assert!(refusal(&error).contains("no reported scout run"), "{error}");

    // A scout still working is not promoted.
    let card = rig.card(CardKind::Scout, None);
    let scout = rig.spawn(&card, RunRole::Scout);
    let error = rig.try_promote(&card).unwrap_err();
    assert!(refusal(&error).contains("no reported scout run"), "{error}");

    // Another question on the card comes first.
    rig.done(&scout.run.id, RunOutcome::Reported).unwrap();
    rig.fleet
        .store
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: card.display_id.clone(),
                kind: AttentionKind::Decision,
                question: "Which database?".into(),
                ..NewAttention::default()
            },
        )
        .unwrap();
    let error = rig.try_promote(&card).unwrap_err();
    assert!(
        refusal(&error).contains("answer it before promoting"),
        "{error}"
    );
    assert_eq!(rig.card_row(&card).kind, CardKind::Scout);
}

#[test]
fn promote_refuses_a_worktree_already_returned() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Scout, None);
    let scout = rig.spawn(&card, RunRole::Scout);
    rig.done(&scout.run.id, RunOutcome::Reported).unwrap();
    // No report waits, so cleanup returns the clean worktree.
    rig.cleanup(&scout.run.id).unwrap();
    let error = rig.try_promote(&card).unwrap_err();
    assert!(refusal(&error).contains("already returned"), "{error}");
    assert_eq!(rig.card_row(&card).kind, CardKind::Scout);
}
