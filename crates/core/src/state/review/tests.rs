use serde_json::json;

use super::*;
use crate::state::records::{NewArtifact, NewReview};
use crate::state::runs::{EndRun, NewRun, SpawnChecks};
use crate::state::tests::{card, memory_store, owner};
use crate::store::{Actor, NewEvent};

const BASE: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";
const LATER: &str = "3333333333333333333333333333333333333333";

fn at(text: &str) -> Ctx {
    Ctx::at(Actor::Owner, text.parse().unwrap())
}

fn rev(store: &Store) -> i64 {
    get::<Project>(store.conn(), "project", "alpha")
        .unwrap()
        .rev
}

fn obligations(store: &Store) -> Vec<Obligation> {
    list(
        store.conn(),
        "project_id = 'alpha' AND what = ?1 ORDER BY opened_at, id",
        &[&REVIEW_WHAT],
    )
    .unwrap()
}

/// Switches alpha to pre-launch at `now`, from `BASE`.
fn prelaunch_at(store: &mut Store, now: &str) -> Staged {
    let rev = rev(store);
    store
        .project_stage(&at(now), "alpha", rev, Stage::PreLaunch, Some(BASE))
        .unwrap()
        .value
}

/// The reconcile timer meeting the open weekly obligation.
fn meet_weekly(store: &mut Store, now: &str) -> Obligation {
    let weekly = open_weekly(store.conn(), "alpha").unwrap().unwrap();
    store
        .obligation_meet(
            &Ctx::at(Actor::Daemon, now.parse().unwrap()),
            &weekly.id,
            NewEvent::new("obligation", &weekly.id, "obligation.due"),
        )
        .unwrap()
        .value
}

fn plan(chunks: usize) -> Value {
    json!((0..chunks).map(|i| json!({"index": i})).collect::<Vec<_>>())
}

/// One reviewer run per chunk with its review row and its chunk report.
fn review_chunks(store: &mut Store, pass: &ReviewPass, chunks: &[usize], now: &str) {
    let ctx = at(now);
    let total = chunk_count(&pass.chunks).unwrap();
    for chunk in chunks {
        let started = store
            .run_start(
                &ctx,
                None,
                NewRun {
                    card: pass.card_id.clone(),
                    role: RunRole::Reviewer,
                    model: "test-model".into(),
                    effort: Effort::Medium,
                    host_id: None,
                    worktree_path: None,
                    branch: None,
                    base_branch: None,
                    base_sha: None,
                    config_hash: None,
                    checks: SpawnChecks::default(),
                },
            )
            .unwrap()
            .value;
        store
            .review_record(
                &ctx,
                NewReview {
                    run_id: started.run.id.clone(),
                    reviewer: Reviewer::CodeReview,
                    coverage: Coverage::Diff,
                    findings_high: 1,
                    findings_medium: 0,
                    findings_low: 2,
                    verdict: Verdict::Fail,
                    source_url: None,
                },
            )
            .unwrap();
        store
            .artifact_add(
                &ctx,
                NewArtifact {
                    card: pass.card_id.clone(),
                    run_id: Some(started.run.id.clone()),
                    kind: ArtifactKind::Report,
                    title: format!("chunk {chunk} of {total}"),
                    url: None,
                    path: Some("/home/user/review.md".into()),
                },
            )
            .unwrap();
        store
            .run_end(
                &ctx,
                &started.run.id,
                EndRun {
                    outcome: RunOutcome::Reported,
                    note: None,
                },
            )
            .unwrap();
    }
}

fn add_report(store: &mut Store, pass: &ReviewPass, now: &str) -> Artifact {
    store
        .artifact_add(
            &at(now),
            NewArtifact {
                card: pass.card_id.clone(),
                run_id: None,
                kind: ArtifactKind::Page,
                title: "pre-launch review".into(),
                url: Some("https://example.com/review.html".into()),
                path: None,
            },
        )
        .unwrap()
        .value
}

/// `docs/design/pre-launch-validation.md` section 8: only the owner, through
/// the CLI or the board, changes a stage.
#[test]
fn project_stage_is_owner_only() {
    let (mut store, orchestrator) = memory_store();
    let rev = rev(&store);
    let error = store
        .project_stage(
            &Ctx::new(Actor::Session(orchestrator)),
            "alpha",
            rev,
            Stage::PreLaunch,
            Some(BASE),
        )
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
    let error = store
        .project_stage(
            &Ctx::new(Actor::Daemon),
            "alpha",
            rev,
            Stage::PreLaunch,
            Some(BASE),
        )
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");

    let staged = store
        .project_stage(
            &Ctx::new(Actor::Board),
            "alpha",
            rev,
            Stage::PreLaunch,
            Some(BASE),
        )
        .unwrap();
    assert_eq!(staged.value.project.stage, Stage::PreLaunch);
    let rev = staged.value.project.rev;
    let staged = store
        .project_stage(&owner(), "alpha", rev, Stage::Launched, None)
        .unwrap();
    assert_eq!(staged.value.project.stage, Stage::Launched);

    let fleet: Project = get(store.conn(), "project", FLEET_PROJECT).unwrap();
    let error = store
        .project_stage(
            &owner(),
            FLEET_PROJECT,
            fleet.rev,
            Stage::PreLaunch,
            Some(BASE),
        )
        .unwrap_err();
    assert_eq!(error.code(), "refused");
}

#[test]
fn stage_to_prelaunch_opens_one_obligation() {
    let (mut store, _) = memory_store();
    let before = rev(&store);
    let error = store
        .project_stage(&owner(), "alpha", before, Stage::PreLaunch, None)
        .unwrap_err();
    assert_eq!(error.code(), "usage");

    // Tuesday 2026-11-03 14:00 Lisbon; the first slot is Saturday 06:00.
    let staged = prelaunch_at(&mut store, "2026-11-03T14:00:00Z");
    assert_eq!(staged.project.stage, Stage::PreLaunch);
    assert_eq!(staged.project.reviewed_through_sha.as_deref(), Some(BASE));
    assert_eq!(
        staged.project.stage_changed_at.unwrap().to_string(),
        "2026-11-03T14:00:00.000Z"
    );
    assert_eq!(
        staged.project.effective_delivery(),
        staged.project.delivery,
        "direct-pr stays direct-pr"
    );
    let weekly = obligations(&store);
    assert_eq!(weekly.len(), 1);
    assert_eq!(weekly[0].trigger_kind, TriggerKind::Date);
    assert_eq!(weekly[0].card_id, None);
    assert_eq!(
        weekly[0].due_at.unwrap().to_string(),
        "2026-11-07T06:00:00.000Z"
    );
    assert_eq!(staged.obligation.as_ref(), Some(&weekly[0]));

    // The same stage at the current rev writes nothing.
    let rev = staged.project.rev;
    let again = store
        .project_stage(&owner(), "alpha", rev, Stage::PreLaunch, Some(HEAD))
        .unwrap();
    assert!(again.replayed);
    assert!(again.events.is_empty());
    assert_eq!(
        again.value.project.reviewed_through_sha.as_deref(),
        Some(BASE)
    );
    assert_eq!(obligations(&store).len(), 1);

    // A stale rev is a conflict, even for a real change.
    let error = store
        .project_stage(&owner(), "alpha", before, Stage::Launched, None)
        .unwrap_err();
    assert_eq!(error.code(), "conflict");
    assert_eq!(
        get::<Project>(store.conn(), "project", "alpha")
            .unwrap()
            .rev,
        rev
    );
}

#[test]
fn review_finish_advances_and_reschedules() {
    let (mut store, _) = memory_store();
    // Friday 2026-03-27: the first slot is Saturday 2026-03-28, still winter.
    let staged = prelaunch_at(&mut store, "2026-03-27T12:00:00Z");
    assert_eq!(
        staged.obligation.unwrap().due_at.unwrap().to_string(),
        "2026-03-28T06:00:00.000Z"
    );
    let error = store
        .review_start(&at("2026-03-27T13:00:00Z"), "alpha", HEAD, plan(2))
        .unwrap_err();
    assert!(
        error.to_string().contains("no weekly review is due"),
        "{error}"
    );

    let met = meet_weekly(&mut store, "2026-03-28T06:00:00Z");
    let due = due_reviews(store.conn(), None, "2026-03-28T06:00:00Z".parse().unwrap()).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].obligation.id, met.id);

    let pass = store
        .review_start(&at("2026-03-28T07:00:00Z"), "alpha", HEAD, plan(2))
        .unwrap()
        .value;
    assert_eq!(pass.kind, ReviewPassKind::Weekly);
    assert_eq!(pass.state, ReviewPassState::Running);
    assert_eq!(pass.base_sha, BASE);
    assert_eq!(pass.head_sha.as_deref(), Some(HEAD));
    assert_eq!(pass.obligation_id.as_deref(), Some(met.id.as_str()));
    assert!(
        due_reviews(store.conn(), None, "2026-03-28T07:00:00Z".parse().unwrap())
            .unwrap()
            .is_empty()
    );
    let pass_card: Card = get(store.conn(), "card", &pass.card_id).unwrap();
    assert_eq!(pass_card.kind, CardKind::Scout);
    assert_eq!(pass_card.category.as_deref(), Some(REVIEW_CATEGORY));

    // One chunk of two has its review: refused.
    review_chunks(&mut store, &pass, &[1], "2026-03-28T08:00:00Z");
    add_report(&mut store, &pass, "2026-03-28T08:00:00Z");
    let error = store
        .review_finish(&at("2026-03-28T09:00:00Z"), &pass.id, None, &[])
        .unwrap_err();
    assert!(error.to_string().contains("1 of 2 chunks"), "{error}");
    // A re-run of chunk 1 does not cover chunk 2.
    review_chunks(&mut store, &pass, &[1], "2026-03-28T08:10:00Z");
    let error = store
        .review_finish(&at("2026-03-28T09:00:00Z"), &pass.id, None, &[])
        .unwrap_err();
    assert!(error.to_string().contains("chunk 2 has none"), "{error}");

    review_chunks(&mut store, &pass, &[2], "2026-03-28T08:30:00Z");
    // The last reviewed commit moved under the pass: compare-and-set refuses.
    store
        .conn()
        .execute(
            "UPDATE projects SET reviewed_through_sha = ?1 WHERE id = 'alpha'",
            [LATER],
        )
        .unwrap();
    let error = store
        .review_finish(&at("2026-03-28T09:00:00Z"), &pass.id, None, &[])
        .unwrap_err();
    assert!(error.to_string().contains("not the pass base"), "{error}");
    store
        .conn()
        .execute(
            "UPDATE projects SET reviewed_through_sha = ?1 WHERE id = 'alpha'",
            [BASE],
        )
        .unwrap();

    let fix = store
        .card_create(&owner(), None, card(CardKind::Ship, "fix the bug"), None)
        .unwrap()
        .value;
    // Saturday 2026-03-28 09:00; the clocks go forward on Sunday 03-29, so
    // the next slot is 06:00 Lisbon summer time, 05:00 UTC.
    let finished = store
        .review_finish(
            &at("2026-03-28T09:00:00Z"),
            &pass_card.display_id,
            None,
            std::slice::from_ref(&fix.display_id),
        )
        .unwrap()
        .value;
    assert_eq!(finished.pass.state, ReviewPassState::Finished);
    assert_eq!(finished.pass.fix_card_ids, json!([fix.id]));
    assert_eq!(finished.project.reviewed_through_sha.as_deref(), Some(HEAD));
    let next = finished.next_obligation.unwrap();
    assert_eq!(next.due_at.unwrap().to_string(), "2026-04-04T05:00:00.000Z");
    assert_eq!(finished.final_pass, None);
    let item = finished.attention.unwrap();
    assert_eq!(item.kind, AttentionKind::Review);
    assert!(item.question.contains("3 high"), "{}", item.question);

    // Finishing again is a no-op; the report gates nothing, and answering
    // its item closes the pass card reported.
    let again = store
        .review_finish(&at("2026-03-28T10:00:00Z"), &pass.id, None, &[])
        .unwrap();
    assert!(again.replayed);
    let answered = store
        .attention_answer(&owner(), None, &item.id, "read", AnsweredVia::Board)
        .unwrap()
        .value;
    assert_eq!(
        answered.closed_card.map(|c| c.outcome),
        Some(Some(CardOutcome::Reported))
    );

    // The October changeover: the next slot after 2026-10-24 is winter time.
    let met = meet_weekly(&mut store, "2026-10-24T05:00:00Z");
    let pass = store
        .review_start(&at("2026-10-24T05:30:00Z"), "alpha", LATER, plan(1))
        .unwrap()
        .value;
    assert_eq!(pass.base_sha, HEAD);
    assert_eq!(pass.obligation_id, Some(met.id));
    review_chunks(&mut store, &pass, &[1], "2026-10-24T06:00:00Z");
    add_report(&mut store, &pass, "2026-10-24T06:00:00Z");
    let finished = store
        .review_finish(&at("2026-10-24T07:00:00Z"), &pass.id, None, &[])
        .unwrap()
        .value;
    assert_eq!(
        finished
            .next_obligation
            .unwrap()
            .due_at
            .unwrap()
            .to_string(),
        "2026-10-31T06:00:00.000Z"
    );

    // After a switch to launched, a finish schedules no weekly date.
    meet_weekly(&mut store, "2026-10-31T06:00:00Z");
    let pass = store
        .review_start(&at("2026-10-31T07:00:00Z"), "alpha", LATER, plan(1))
        .unwrap()
        .value;
    let rev = rev(&store);
    let staged = store
        .project_stage(
            &at("2026-10-31T08:00:00Z"),
            "alpha",
            rev,
            Stage::Launched,
            None,
        )
        .unwrap()
        .value;
    assert_eq!(staged.final_pass, None, "the running weekly pass continues");
    review_chunks(&mut store, &pass, &[1], "2026-10-31T09:00:00Z");
    add_report(&mut store, &pass, "2026-10-31T09:00:00Z");
    let finished = store
        .review_finish(&at("2026-10-31T10:00:00Z"), &pass.id, None, &[])
        .unwrap()
        .value;
    assert_eq!(finished.next_obligation, None);
    let final_pass = finished.final_pass.unwrap();
    assert_eq!(final_pass.kind, ReviewPassKind::Final);
    assert_eq!(final_pass.base_sha, LATER);
    assert!(open_weekly(store.conn(), "alpha").unwrap().is_none());
}

#[test]
fn launch_cancels_weekly_and_opens_final_pass() {
    let (mut store, orchestrator) = memory_store();
    let rev = rev(&store);
    store
        .project_edit(
            &owner(),
            "alpha",
            rev,
            crate::state::projects::ProjectEdit {
                delivery: Some(Delivery::NoMistakes),
                ..Default::default()
            },
        )
        .unwrap();
    let staged = prelaunch_at(&mut store, "2026-11-03T14:00:00Z");
    assert_eq!(staged.project.effective_delivery(), Delivery::DirectPr);
    let weekly = staged.obligation.unwrap();

    let rev = staged.project.rev;
    let applied = store
        .project_stage(
            &at("2026-11-04T10:00:00Z"),
            "alpha",
            rev,
            Stage::Launched,
            None,
        )
        .unwrap();
    let staged = applied.value;
    assert_eq!(staged.project.stage, Stage::Launched);
    assert_eq!(staged.project.effective_delivery(), Delivery::NoMistakes);
    assert_eq!(staged.cancelled_obligations, vec![weekly.id.clone()]);
    let weekly: Obligation = get(store.conn(), "obligation", &weekly.id).unwrap();
    assert_eq!(weekly.state, ObligationState::Cancelled);
    let pass = staged.final_pass.unwrap();
    assert_eq!(pass.kind, ReviewPassKind::Final);
    assert_eq!(pass.state, ReviewPassState::Pending);
    assert_eq!(pass.base_sha, BASE);
    assert_eq!(
        live_pass(store.conn(), "alpha").unwrap(),
        Some(pass.clone())
    );

    // One transaction: every event the switch wrote shares one txn id.
    let txns: Vec<String> = applied
        .events
        .iter()
        .map(|seq| {
            store
                .conn()
                .query_row("SELECT txn FROM events WHERE seq = ?1", [seq], |row| {
                    row.get(0)
                })
                .unwrap()
        })
        .collect();
    assert!(txns.iter().all(|txn| txn == "project.stage"), "{txns:?}");
    let notes: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM outbox WHERE target_session_id = ?1 AND body LIKE '%final pre-launch review%'",
            [&orchestrator],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(notes, 1);

    // Back to pre-launch is refused while the final pass is open.
    let rev = staged.project.rev;
    let error = store
        .project_stage(&owner(), "alpha", rev, Stage::PreLaunch, Some(HEAD))
        .unwrap_err();
    assert!(error.to_string().contains("finish or abandon"), "{error}");
}

#[test]
fn final_pass_finish_schedules_nothing() {
    let (mut store, _) = memory_store();
    let staged = prelaunch_at(&mut store, "2026-11-03T14:00:00Z");
    let rev = staged.project.rev;
    let pass = store
        .project_stage(
            &at("2026-11-04T10:00:00Z"),
            "alpha",
            rev,
            Stage::Launched,
            None,
        )
        .unwrap()
        .value
        .final_pass
        .unwrap();
    // `review start` takes up the pending final pass.
    let started = store
        .review_start(&at("2026-11-04T11:00:00Z"), "alpha", HEAD, plan(1))
        .unwrap()
        .value;
    assert_eq!(started.id, pass.id);
    assert_eq!(started.state, ReviewPassState::Running);
    review_chunks(&mut store, &started, &[1], "2026-11-04T12:00:00Z");
    add_report(&mut store, &started, "2026-11-04T12:00:00Z");
    let finished = store
        .review_finish(&at("2026-11-04T13:00:00Z"), &pass.id, None, &[])
        .unwrap()
        .value;
    assert_eq!(finished.project.reviewed_through_sha.as_deref(), Some(HEAD));
    assert_eq!(finished.next_obligation, None);
    assert_eq!(finished.final_pass, None);
    assert!(open_weekly(store.conn(), "alpha").unwrap().is_none());
    assert_eq!(live_pass(store.conn(), "alpha").unwrap(), None);
}

#[test]
fn review_due_lists_a_met_obligation_with_no_pass() {
    let (mut store, _) = memory_store();
    prelaunch_at(&mut store, "2026-11-03T14:00:00Z");
    let now = |text: &str| text.parse::<Timestamp>().unwrap();
    assert!(
        due_reviews(store.conn(), None, now("2026-11-07T07:00:00Z"))
            .unwrap()
            .is_empty()
    );
    let met = meet_weekly(&mut store, "2026-11-07T06:00:00Z");

    let due = due_reviews(store.conn(), Some("alpha"), now("2026-11-07T07:00:00Z")).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].obligation.id, met.id);
    assert!(!due[0].overdue);
    let due = due_reviews(store.conn(), None, now("2026-11-08T06:00:00Z")).unwrap();
    assert!(
        due[0].overdue,
        "24 hours after the slot the review is overdue"
    );
    assert!(
        due_reviews(store.conn(), Some("fleet"), now("2026-11-08T06:00:00Z"))
            .unwrap()
            .is_empty()
    );

    // Abandoning the pass started from it schedules the next week and does
    // not list the old one again.
    let pass = store
        .review_start(&at("2026-11-07T08:00:00Z"), "alpha", HEAD, plan(1))
        .unwrap()
        .value;
    let abandoned = store
        .review_abandon(&at("2026-11-07T09:00:00Z"), &pass.id, "quota floor")
        .unwrap()
        .value;
    assert_eq!(abandoned.card.outcome, Some(CardOutcome::Abandoned));
    assert_eq!(
        abandoned
            .next_obligation
            .unwrap()
            .due_at
            .unwrap()
            .to_string(),
        "2026-11-14T06:00:00.000Z"
    );
    let project: Project = get(store.conn(), "project", "alpha").unwrap();
    assert_eq!(project.reviewed_through_sha.as_deref(), Some(BASE));
    assert!(
        due_reviews(store.conn(), None, now("2026-11-08T06:00:00Z"))
            .unwrap()
            .is_empty()
    );
}

/// A weekly review met but never started before launch is covered by the
/// final pass, so it is not due again when the project is pre-launch again.
#[test]
fn review_due_skips_an_obligation_met_before_a_launch() {
    let (mut store, _) = memory_store();
    prelaunch_at(&mut store, "2026-11-03T14:00:00Z");
    meet_weekly(&mut store, "2026-11-07T06:00:00Z");
    let rev = rev(&store);
    let launched = store
        .project_stage(
            &at("2026-11-07T07:00:00Z"),
            "alpha",
            rev,
            Stage::Launched,
            None,
        )
        .unwrap()
        .value;
    assert!(launched.final_pass.is_some());
    let started = store
        .review_start(&at("2026-11-07T08:00:00Z"), "alpha", HEAD, plan(1))
        .unwrap()
        .value;
    assert_eq!(started.kind, ReviewPassKind::Final);
    review_chunks(&mut store, &started, &[1], "2026-11-07T09:00:00Z");
    add_report(&mut store, &started, "2026-11-07T09:00:00Z");
    store
        .review_finish(&at("2026-11-07T10:00:00Z"), &started.id, None, &[])
        .unwrap();

    let staged = prelaunch_at(&mut store, "2026-12-01T10:00:00Z");
    assert!(staged.obligation.is_some());
    assert!(
        due_reviews(store.conn(), None, "2026-12-01T11:00:00Z".parse().unwrap())
            .unwrap()
            .is_empty()
    );
}
