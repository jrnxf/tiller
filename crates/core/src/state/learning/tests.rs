//! The learning loop's enforced rules (`docs/design/learning-loop.md`,
//! section 11).

use rusqlite::params;
use serde_json::json;

use super::*;
use crate::state::attention::NewAttention;
use crate::state::runs::EndRun;
use crate::state::tests::{builder_run, card, memory_store, owner};

fn config(sample_every: u64) -> Learning {
    Learning {
        sample_every,
        ..Learning::default()
    }
}

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn sql(store: &Store, text: &str) {
    store.conn().execute_batch(text).unwrap();
}

/// A closed card `alpha-<n>` with id `c<n>`, inserted straight into the
/// table, with `extra` giving `size, outcome, created_at, closed_at`.
fn closed(store: &Store, n: u32, extra: &str) {
    sql(
        store,
        &format!(
            "INSERT INTO cards (id, display_id, project_id, kind, title, state, size, outcome, created_at, closed_at)
             VALUES ('c{n}', 'alpha-{n}', 'alpha', 'ship', 't', 'closed', {extra})"
        ),
    );
}

/// The retros of a card by display id, oldest first.
fn retros_of(store: &Store, display_id: &str) -> Vec<Retro> {
    list(
        store.conn(),
        "card_id IN (SELECT id FROM cards WHERE display_id = ?1) ORDER BY created_at, id",
        &[&display_id],
    )
    .unwrap()
}

const P1: &str = "01JC00000000000000000000P1";
const P2: &str = "01JC00000000000000000000P2";
const P3: &str = "01JC00000000000000000000P3";

/// A proposal row with the given diff, in `proposed` state.
fn proposal(store: &Store, id: &str, diff: &str) {
    sql(
        store,
        &format!(
            "INSERT OR IGNORE INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo, canonical_path,
                                        apply_method, load_mode, added_at)
             VALUES ('src', 'alpha', 'personal', 'tiller_doc', 'alpha.md', '/home/user/memory', 'alpha.md',
                     'tiller_commit', 'rendered', '2026-11-01T00:00:00.000Z');
             INSERT OR IGNORE INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub, recorded_at, recorded_by)
             VALUES ('rev', 'src', 'e69de29bb2d1d6434b8b29ae775ad8c2e48c5391', 'approved', 'baseline', 'passed',
                     '2026-11-01T00:00:00.000Z', 'owner');
             INSERT INTO proposals (id, project_id, boundary, target_source_id, op, base_revision_id, base_blob, diff,
                                    diff_sha256, expected_effect, created_at, created_by)
             VALUES ('{id}', 'alpha', 'personal', 'src', 'add', 'rev', 'e69de29bb2d1d6434b8b29ae775ad8c2e48c5391',
                     '{diff}', '{}', 'fewer corrections', '2026-11-02T00:00:00.000Z', 'owner');",
            sha256_hex(diff.as_bytes())
        ),
    );
}

/// Marks a proposal applied the way `mem.apply` leaves it: an accepted
/// memory approval decided at 09:00, and the applied revision recorded at
/// 10:00, which is when the change reached the file.
fn apply(store: &Store, proposal: &str) {
    sql(
        store,
        &format!(
            "INSERT INTO approvals (id, kind, project_id, diff_sha256, base_rev, action, state, requested_at, requested_by,
                                    decided_at, decided_by, executed_at)
             VALUES ('ap-{proposal}', 'memory', 'alpha', 'x', 'y', 'apply', 'succeeded', '2026-11-03T09:00:00.000Z', 'owner',
                     '2026-11-03T09:00:00.000Z', 'owner', '2026-11-03T10:00:00.000Z');
             INSERT INTO memory_revisions (id, source_id, blob_sha, parent_revision_id, review, provenance, approval_id,
                                           scrub, recorded_at, recorded_by)
             VALUES ('applied-{proposal}', 'src', 'blob-{proposal}', 'rev', 'approved', 'tiller', 'ap-{proposal}',
                     'passed', '2026-11-03T10:00:00.000Z', 'daemon');
             UPDATE proposals SET state = 'applied', decided_at = '2026-11-03T09:00:00.000Z',
                 approval_id = 'ap-{proposal}', applied_revision_id = 'applied-{proposal}' WHERE id = '{proposal}'"
        ),
    );
}

fn passing() -> EvalResult {
    EvalResult {
        cases_total: 4,
        cases_passed: 4,
        ..EvalResult::default()
    }
}

fn nothing() -> Finish {
    Finish {
        outcome: RetroOutcome::Nothing,
        learning: None,
        evidence: None,
        proposal: None,
    }
}

fn evidence_count(store: &Store) -> i64 {
    store
        .conn()
        .query_row("SELECT COUNT(*) FROM evidence", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn sweep_is_deterministic_and_complete() {
    let (mut store, _) = memory_store();
    closed(
        &store,
        1,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    closed(
        &store,
        2,
        "'M', 'reported', '2026-11-01T09:00:00.000Z', '2026-11-01T12:00:00.000Z'",
    );
    closed(
        &store,
        3,
        "'M', 'cancelled', '2026-11-01T09:00:00.000Z', '2026-11-01T12:00:00.000Z'",
    );
    let first = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert_eq!(
        first.value.scheduled.len(),
        2,
        "cancelled cards get no retro"
    );
    assert!(
        first
            .value
            .scheduled
            .iter()
            .all(|r| r.depth == RetroDepth::Light)
    );
    assert!(
        first
            .value
            .scheduled
            .iter()
            .all(|r| r.state == RetroState::Done)
    );
    assert!(
        first
            .value
            .scheduled
            .iter()
            .all(|r| r.outcome == Some(RetroOutcome::Nothing))
    );
    assert_eq!(first.value.scheduled[0].counts["elapsed_minutes"], 120);
    assert!(!first.events.is_empty());

    // The same sweep again writes nothing.
    let again = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert_eq!(again.value, Swept::default());
    assert!(again.events.is_empty());
    assert_eq!(retros_of(&store, "alpha-1").len(), 1);

    // The host health card exists and never gets one.
    let health: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM cards WHERE display_id = 'fleet-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(health, 1);
    assert_eq!(retros_of(&store, "fleet-1").len(), 0);
}

#[test]
fn sample_follows_the_hash() {
    assert!(sampled("anything", 1));
    assert!(sampled("anything", 0));
    // Over many ids, about one in five is sampled, and each id is stable.
    let ids: Vec<String> = (0..500).map(|n| format!("01JC{n:022}")).collect();
    let picked = ids.iter().filter(|id| sampled(id, 5)).count();
    assert!(
        (60..=140).contains(&picked),
        "{picked} of 500 sampled at 1 in 5"
    );
    for id in &ids {
        assert_eq!(sampled(id, 5), sampled(id, 5));
    }

    let (mut store, _) = memory_store();
    closed(
        &store,
        1,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    let swept = store.learning_sweep(&owner(), &config(1)).unwrap();
    assert_eq!(swept.value.scheduled[0].depth, RetroDepth::Full);
    assert_eq!(swept.value.scheduled[0].reason, RetroReason::Sample);
    assert_eq!(swept.value.scheduled[0].state, RetroState::Pending);
}

#[test]
fn incident_and_win_are_full() {
    let (mut store, session) = memory_store();
    // An incident: the card failed.
    closed(
        &store,
        11,
        "'M', 'failed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    // A win: five routine peers at two hours, this one in 30 minutes.
    for n in 0..5 {
        closed(
            &store,
            20 + n,
            "'S', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
        );
    }
    closed(
        &store,
        31,
        "'S', 'landed', '2026-11-02T09:00:00.000Z', '2026-11-02T09:30:00.000Z'",
    );
    // A card as fast, but the developer was needed: routine.
    let needed = store
        .card_create(&owner(), None, card(CardKind::Ship, "needed"), None)
        .unwrap()
        .value;
    store
        .attention_open(
            &Ctx::new(Actor::Session(session.clone())),
            None,
            NewAttention {
                card: needed.id.clone(),
                kind: AttentionKind::Decision,
                question: "which way?".into(),
                ..NewAttention::default()
            },
        )
        .unwrap();
    sql(
        &store,
        &format!(
            "UPDATE attention SET answered_at = '2026-11-02T09:10:00.000Z', answer = 'left', answered_via = 'board'
             WHERE card_id = '{}';
             UPDATE cards SET state = 'closed', outcome = 'landed', size = 'S',
                 created_at = '2026-11-02T09:00:00.000Z', closed_at = '2026-11-02T09:20:00.000Z' WHERE id = '{}'",
            needed.id, needed.id
        ),
    );
    let swept = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    let by_card = |id: &str| {
        swept
            .value
            .scheduled
            .iter()
            .find(|r| r.card_id == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        (by_card("c11").depth, by_card("c11").reason),
        (RetroDepth::Full, RetroReason::Incident)
    );
    assert_eq!(
        (by_card("c31").depth, by_card("c31").reason),
        (RetroDepth::Full, RetroReason::SurprisingWin)
    );
    assert_eq!(
        (by_card(&needed.id).depth, by_card(&needed.id).reason),
        (RetroDepth::Light, RetroReason::Routine)
    );
    assert_eq!(by_card(&needed.id).counts["attention"]["decision"], 1);

    // A run that was cut makes an incident of a landed card.
    let cut = store
        .card_create(&owner(), None, card(CardKind::Ship, "cut"), None)
        .unwrap()
        .value;
    let run = store
        .run_start(&owner(), None, builder_run(&cut.id))
        .unwrap()
        .value
        .run;
    store
        .run_end(
            &owner(),
            &run.id,
            EndRun {
                outcome: RunOutcome::Cut,
                note: Some("30 minutes".into()),
            },
        )
        .unwrap();
    sql(
        &store,
        &format!(
            "UPDATE cards SET state = 'closed', outcome = 'landed', closed_at = '2026-11-03T09:00:00.000Z' WHERE id = '{}'",
            cut.id
        ),
    );
    let swept = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert_eq!(swept.value.scheduled.len(), 1);
    assert_eq!(swept.value.scheduled[0].reason, RetroReason::Incident);
    assert_eq!(swept.value.scheduled[0].counts["run_outcomes"]["cut"], 1);
    assert_eq!(swept.value.scheduled[0].counts["attempts"]["builder"], 1);
}

#[test]
fn escaped_defect_reopens_the_origin() {
    let (mut store, _) = memory_store();
    closed(
        &store,
        51,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    let first = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert_eq!(first.value.scheduled[0].depth, RetroDepth::Light);

    let mut fix = card(CardKind::Ship, "the export skips a row");
    fix.fixes_card = Some("alpha-51".into());
    let fix = store.card_create(&owner(), None, fix, None).unwrap().value;
    let second = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert_eq!(second.value.scheduled.len(), 1);
    let reopened = &second.value.scheduled[0];
    assert_eq!(reopened.card_id, "c51");
    assert_eq!(reopened.trigger.as_deref(), Some(fix.id.as_str()));
    assert_eq!(
        (reopened.depth, reopened.reason),
        (RetroDepth::Full, RetroReason::Incident)
    );
    assert_eq!(reopened.counts["fix_cards"], 1);
    // Once per fix card.
    let third = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert!(third.value.scheduled.iter().all(|r| r.card_id != "c51"));
    assert_eq!(retros_of(&store, "alpha-51").len(), 2);
}

#[test]
fn finish_rules_and_learning_becomes_evidence() {
    let (mut store, session) = memory_store();
    closed(
        &store,
        41,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    closed(
        &store,
        42,
        "'M', 'failed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    let light = retros_of(&store, "alpha-41").remove(0);
    let full = retros_of(&store, "alpha-42").remove(0);
    let curator = Ctx::new(Actor::Curator(full.id.clone()));

    // A light retro is already done.
    let error = store
        .retro_finish(&owner(), None, &light.id, nothing())
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");
    // A fleet session may not finish one, and a curator finishes only its
    // own retro.
    let error = store
        .retro_finish(
            &Ctx::new(Actor::Session(session)),
            None,
            &full.id,
            nothing(),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error}");
    let error = store
        .retro_finish(
            &Ctx::new(Actor::Curator(light.id.clone())),
            None,
            &full.id,
            nothing(),
        )
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error}");

    // A start claims the retro for one curator at a time.
    let started = store
        .retro_start(&owner(), &full.id, Some("session-a"), 20)
        .unwrap();
    assert_eq!(started.value.curator_session.as_deref(), Some("session-a"));
    let error = store
        .retro_start(&owner(), &full.id, Some("session-b"), 20)
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");
    // A start older than the timeout was a run that died: a new one may claim it.
    assert!(
        store
            .retro_start(&owner(), &full.id, Some("session-b"), 0)
            .is_ok()
    );

    // A learning becomes one evidence row pointing at the retro.
    let before = evidence_count(&store);
    let learning = Finish {
        outcome: RetroOutcome::Evidence,
        learning: Some("the builder rebased a pushed branch; the brief never said not to".into()),
        evidence: Some(json!({"events": [12, 14]})),
        proposal: None,
    };
    let done = store
        .retro_finish(&curator, Some("k1"), &full.id, learning.clone())
        .unwrap();
    assert!(!done.replayed);
    let retro = &done.value;
    assert_eq!(retro.state, RetroState::Done);
    assert_eq!(retro.outcome, Some(RetroOutcome::Evidence));
    assert_eq!(
        retro.finished_by.as_deref(),
        Some(format!("curator:{}", full.id).as_str())
    );
    let evidence_id = retro.evidence_id.clone().unwrap();
    let (kind, reference, excerpt, independence, by): (String, String, String, String, String) =
        store
            .conn()
            .query_row(
                "SELECT kind, ref, excerpt, independence_key, captured_by FROM evidence WHERE id = ?1",
                [&evidence_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
    assert_eq!(kind, "incident");
    let reference: Value = serde_json::from_str(&reference).unwrap();
    assert_eq!(reference["retro"], full.id);
    assert_eq!(reference["pointers"]["events"][0], 12);
    assert!(excerpt.starts_with("the builder rebased"));
    assert_eq!(independence, "c42");
    assert_eq!(by, format!("curator:{}", full.id));
    assert_eq!(evidence_count(&store), before + 1);

    // The same key replays; another outcome is refused; the same outcome
    // without the key is a natural replay that writes nothing.
    let replay = store
        .retro_finish(&curator, Some("k1"), &full.id, learning)
        .unwrap();
    assert!(replay.replayed);
    let error = store
        .retro_finish(&curator, None, &full.id, nothing())
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");
    let natural = store
        .retro_finish(
            &curator,
            None,
            &full.id,
            Finish {
                outcome: RetroOutcome::Evidence,
                learning: None,
                evidence: None,
                proposal: None,
            },
        )
        .unwrap();
    assert!(natural.replayed);
    assert_eq!(evidence_count(&store), before + 1);

    // A learning without text is a usage error.
    closed(
        &store,
        43,
        "'M', 'abandoned', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    let full2 = retros_of(&store, "alpha-43").remove(0);
    let error = store
        .retro_finish(
            &owner(),
            None,
            &full2.id,
            Finish {
                outcome: RetroOutcome::Evidence,
                learning: Some("  ".into()),
                evidence: None,
                proposal: None,
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::Usage(_)), "{error}");
    // The owner may finish by card reference, as nothing.
    let done = store
        .retro_finish(&owner(), None, "alpha-43", nothing())
        .unwrap();
    assert_eq!(done.value.id, full2.id);
}

#[test]
fn an_unfinished_retro_starts_again() {
    let (mut store, _session) = memory_store();
    for n in [44, 45] {
        closed(
            &store,
            n,
            "'M', 'failed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
        );
    }
    store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    let retro = retros_of(&store, "alpha-44").remove(0);
    let other = retros_of(&store, "alpha-45").remove(0);
    let unfinished = |note: &str| Finish {
        outcome: RetroOutcome::Unfinished,
        learning: Some(note.into()),
        evidence: None,
        proposal: None,
    };

    // The owner, the board and the daemon may each run it again once it
    // ended unfinished; each run resets it to pending first.
    for (round, actor) in [Actor::Owner, Actor::Board, Actor::Daemon]
        .into_iter()
        .enumerate()
    {
        let ctx = Ctx::new(actor);
        let session = format!("session-{round}");
        store
            .retro_start(&ctx, &retro.id, Some(&session), 20)
            .unwrap();
        let done = store
            .retro_finish(&ctx, None, &retro.id, unfinished("the curator exited 3"))
            .unwrap();
        assert_eq!(done.value.outcome, Some(RetroOutcome::Unfinished));
        // A run that ended unfinished is over, so its start time does not
        // hold the next one back.
        let again = store
            .retro_start(&ctx, &retro.id, Some(&format!("{session}-again")), 20)
            .unwrap();
        let started = &again.value;
        assert_eq!(started.state, RetroState::Pending);
        assert_eq!(
            (
                started.outcome,
                started.learning.as_deref(),
                started.finished_at,
                started.finished_by.as_deref()
            ),
            (None, None, None, None)
        );
        assert_eq!(
            started.curator_session.as_deref(),
            Some(format!("{session}-again").as_str())
        );
        let events: Vec<(String, i64, Value)> = again
            .events
            .iter()
            .map(|seq| {
                store
                    .conn()
                    .query_row(
                        "SELECT kind, rev_after, payload FROM events WHERE seq = ?1",
                        [seq],
                        |r| {
                            let payload: String = r.get(2)?;
                            Ok((
                                r.get(0)?,
                                r.get(1)?,
                                serde_json::from_str(&payload).unwrap(),
                            ))
                        },
                    )
                    .unwrap()
            })
            .collect();
        assert_eq!(
            events
                .iter()
                .map(|(kind, rev, _)| (kind.as_str(), *rev))
                .collect::<Vec<_>>(),
            [
                ("retro.reset", done.value.rev + 1),
                ("retro.started", done.value.rev + 2)
            ]
        );
        assert_eq!(started.rev, done.value.rev + 2);
        assert_eq!(events[0].2["outcome"], "unfinished");
        assert_eq!(events[0].2["learning"], "the curator exited 3");
        assert_eq!(events[0].2["curator_session"], session.as_str());
        // The reset run is claimed: a second start while it runs is refused.
        let error = store
            .retro_start(&ctx, &retro.id, Some("late"), 20)
            .unwrap_err();
        assert!(matches!(error, Error::Refused { .. }), "{error}");
        if round < 2 {
            // Back to unfinished for the next actor's round.
            store
                .retro_finish(&ctx, None, &retro.id, unfinished("cut again"))
                .unwrap();
        }
    }
    let finished = store
        .retro_finish(&owner(), None, &retro.id, nothing())
        .unwrap();
    assert_eq!(finished.value.outcome, Some(RetroOutcome::Nothing));
    // Any other outcome is final.
    let error = store
        .retro_start(&owner(), &retro.id, Some("late"), 20)
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");
    assert!(error.to_string().contains("outcome nothing"), "{error}");

    // A fleet session may not reset one.
    store
        .retro_start(&owner(), &other.id, Some("s"), 20)
        .unwrap();
    store
        .retro_finish(&owner(), None, &other.id, unfinished("cut"))
        .unwrap();
    let error = store
        .retro_start(
            &Ctx::new(Actor::Curator(other.id.clone())),
            &other.id,
            Some("c"),
            20,
        )
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error}");
}

#[test]
fn gate_rules() {
    let (mut store, _) = memory_store();
    proposal(&store, P1, "--- a\n+++ b\n@@ -0,0 +1 @@\n+one\n");
    proposal(&store, P2, "--- a\n+++ b\n@@ -0,0 +1 @@\n+two\n");
    proposal(&store, P3, "--- a\n+++ b\n@@ -0,0 +1 @@\n+three\n");

    // No set.
    let gate = store.learning_gate(P1).unwrap();
    assert!(!gate.ok);
    assert!(gate.reason.unwrap().contains("no change set"));

    // A set with no run.
    let set = store
        .change_set_create(&owner(), "tighter briefs", &[P1.into()], 10)
        .unwrap()
        .value;
    let gate = store.learning_gate(P1).unwrap();
    assert!(!gate.ok);
    assert!(gate.reason.unwrap().contains("no holdout run"));
    assert_eq!(gate.hint.unwrap(), "tiller learn eval tighter briefs");

    // A failed run.
    store
        .eval_run_record(
            &owner(),
            "tighter briefs",
            EvalResult {
                cases_total: 4,
                cases_passed: 3,
                ..EvalResult::default()
            },
        )
        .unwrap();
    let gate = store.learning_gate(P1).unwrap();
    assert!(!gate.ok);
    assert!(gate.reason.unwrap().contains("did not pass (3 of 4"));

    // A partial run never passes, even with every case.
    store
        .eval_run_record(
            &owner(),
            &set.id,
            EvalResult {
                partial: true,
                ..passing()
            },
        )
        .unwrap();
    assert!(!store.learning_gate(P1).unwrap().ok);

    // A passing run opens the gate.
    let run = store
        .eval_run_record(&owner(), &set.id, passing())
        .unwrap()
        .value;
    assert_eq!(run.passed, 1);
    let gate = store.learning_gate(P1).unwrap();
    assert!(gate.ok, "{gate:?}");
    assert_eq!(gate.eval_run.as_deref(), Some(run.id.as_str()));

    // A run rendered from an older member set is refused at record time.
    let stale_members = members_sha256(store.conn(), &set.id).unwrap();
    store.change_set_add(&owner(), &set.id, P2).unwrap();
    let error = store
        .eval_run_record(
            &owner(),
            &set.id,
            EvalResult {
                members_sha256: Some(stale_members),
                ..passing()
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");
    // A member added after the run makes it stale.
    let gate = store.learning_gate(P1).unwrap();
    assert!(!gate.ok);
    assert!(gate.reason.unwrap().contains("other members"));
    store.eval_run_record(&owner(), &set.id, passing()).unwrap();
    assert!(store.learning_gate(P2).unwrap().ok);

    // A proposal is in one set only; a used name is refused while open.
    let error = store
        .change_set_create(&owner(), "another", &[P2.into()], 10)
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");
    let error = store
        .change_set_create(&owner(), "tighter briefs", &[], 10)
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");

    // The set starts measuring once a member is applied; then nothing
    // outside it can be accepted until the window is complete.
    apply(&store, P1);
    let swept = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert_eq!(swept.value.measuring, vec![set.id.clone()]);
    let other = store
        .change_set_create(&owner(), "another", &[P3.into()], 10)
        .unwrap()
        .value;
    store
        .eval_run_record(&owner(), &other.id, passing())
        .unwrap();
    let gate = store.learning_gate(P3).unwrap();
    assert!(!gate.ok);
    assert_eq!(gate.measuring.as_ref().unwrap().name, "tighter briefs");
    assert_eq!(gate.measuring.as_ref().unwrap().cards_closed, 0);
    // A member of the measuring set itself is not blocked by it.
    assert!(store.learning_gate(P2).unwrap().ok);

    // Abandoning the measuring set clears the way.
    store
        .change_set_abandon(&owner(), "tighter briefs")
        .unwrap();
    assert!(store.learning_gate(P3).unwrap().ok);
    // An applied proposal is past the gate.
    assert!(!store.learning_gate(P1).unwrap().ok);
}

#[test]
fn window_closes_after_n_cards() {
    let (mut store, _) = memory_store();
    proposal(&store, P1, "--- a\n+++ b\n@@ -0,0 +1 @@\n+one\n");
    let set = store
        .change_set_create(&owner(), "w", &[P1.into()], 3)
        .unwrap()
        .value;
    apply(&store, P1);
    store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    let measuring: ChangeSet = get(store.conn(), "change set", &set.id).unwrap();
    assert_eq!(measuring.state, ChangeSetState::Measuring);
    // The window starts when the revision was applied, not when the owner
    // accepted the proposal an hour earlier.
    assert_eq!(measuring.measuring_from, Some(at("2026-11-03T10:00:00Z")));

    // Two before, two after: not complete. A card closed between the
    // owner's acceptance (09:00) and the apply (10:00) is a "before" card.
    closed(
        &store,
        60,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-03T09:30:00.000Z'",
    );
    closed(
        &store,
        61,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-02T11:00:00.000Z'",
    );
    closed(
        &store,
        62,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-02T12:00:00.000Z'",
    );
    closed(
        &store,
        63,
        "'M', 'landed', '2026-11-03T09:00:00.000Z', '2026-11-03T11:00:00.000Z'",
    );
    closed(
        &store,
        64,
        "'M', 'reported', '2026-11-03T09:00:00.000Z', '2026-11-03T12:00:00.000Z'",
    );
    let swept = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert!(swept.value.closed.is_empty());
    let status = store.learning_status(at("2026-11-04T00:00:00Z")).unwrap();
    let report = status.measuring.unwrap();
    assert_eq!(report.window.cards, 2);
    // The comparison takes the last three before the window.
    assert_eq!(report.before.cards, 3);
    assert!(!report.revert_suggested);

    // A worse window: rework on one of its cards.
    store
        .conn()
        .execute(
            "INSERT INTO runs (id, card_id, role, attempt, model, effort, host_id, supervisor_session_id, started_at)
             VALUES ('r2', 'c63', 'builder', 2, 'm', 'medium', (SELECT id FROM hosts LIMIT 1),
                     (SELECT id FROM sessions LIMIT 1), '2026-11-03T10:00:00.000Z')",
            params![],
        )
        .unwrap();
    let status = store.learning_status(at("2026-11-04T00:00:00Z")).unwrap();
    let report = status.measuring.unwrap();
    assert!(report.revert_suggested);
    assert_eq!(report.worse, vec!["rework_attempts"]);

    // A cancelled card does not count; the third accepted card closes it.
    closed(
        &store,
        65,
        "'M', 'cancelled', '2026-11-03T09:00:00.000Z', '2026-11-03T13:00:00.000Z'",
    );
    assert!(
        store
            .learning_sweep(&owner(), &config(1_000_000))
            .unwrap()
            .value
            .closed
            .is_empty()
    );
    closed(
        &store,
        66,
        "'M', 'landed', '2026-11-03T09:00:00.000Z', '2026-11-03T14:00:00.000Z'",
    );
    // A fourth card past the window is not in it: the window is the first
    // three since the apply.
    closed(
        &store,
        67,
        "'M', 'landed', '2026-11-03T09:00:00.000Z', '2026-11-03T15:00:00.000Z'",
    );
    let status = store.learning_status(at("2026-11-04T00:00:00Z")).unwrap();
    let report = status.measuring.unwrap();
    assert_eq!(report.window.cards, 3);
    let swept = store.learning_sweep(&owner(), &config(1_000_000)).unwrap();
    assert_eq!(swept.value.closed, vec![set.id.clone()]);
    let closed_set: ChangeSet = get(store.conn(), "change set", &set.id).unwrap();
    assert_eq!(
        closed_set.close_reason,
        Some(ChangeSetClose::WindowComplete)
    );
    assert!(
        store
            .learning_status(at("2026-11-04T00:00:00Z"))
            .unwrap()
            .measuring
            .is_none()
    );
}

#[test]
fn manifest_diff_and_record() {
    let (mut store, _) = memory_store();
    let a = json!({"role": "aaa", "settings": "s1", "model": "opus", "claude": "2.1.0"});
    let b = json!({"role": "aaa", "settings": "s2", "model": "opus", "claude": "2.1.1"});
    let diff = manifest_diff(&a, &b);
    assert_eq!(
        diff.keys().cloned().collect::<Vec<_>>(),
        vec!["claude", "settings"]
    );
    assert_eq!(diff["settings"], (json!("s1"), json!("s2")));

    let first = store.config_manifest_record(&owner(), "h1", &a).unwrap();
    assert!(first.value && !first.events.is_empty());
    let again = store.config_manifest_record(&owner(), "h1", &a).unwrap();
    assert!(!again.value && again.events.is_empty());
    let stored: ConfigManifest = find(store.conn(), "config_hash = ?1", &[&"h1"])
        .unwrap()
        .unwrap();
    assert_eq!(stored.parts, a);
}

#[test]
fn learning_config_reads_its_table() {
    let table: toml::Table = "[learning]\nsample_every = 3\nevals_dir = \"/home/user/tiller\"\n"
        .parse()
        .unwrap();
    let config = Learning::from_table(&table).unwrap();
    assert_eq!(config.sample_every, 3);
    assert_eq!(config.window_cards, 10);
    assert_eq!(config.evals_dir.as_deref(), Some("/home/user/tiller"));
    assert_eq!(
        Learning::from_table(&toml::Table::new()).unwrap(),
        Learning::default()
    );
    let bad: toml::Table = "[learning]\nsamples = 3\n".parse().unwrap();
    assert!(matches!(Learning::from_table(&bad), Err(Error::Usage(_))));
}
