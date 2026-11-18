//! Retros' enforced rules (`memory.md` section 5).

use serde_json::json;

use super::*;
use crate::state::attention::NewAttention;
use crate::state::runs::EndRun;
use crate::state::tests::{builder_run, card, memory_store, owner};

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

fn nothing() -> Finish {
    Finish {
        outcome: RetroOutcome::Nothing,
        learning: None,
        note: None,
    }
}

fn pending_count(store: &Store) -> i64 {
    store
        .conn()
        .query_row("SELECT COUNT(*) FROM memory_pending", [], |r| r.get(0))
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
    let first = store.learning_sweep(&owner()).unwrap();
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
    let again = store.learning_sweep(&owner()).unwrap();
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

/// A full retro runs only for an incident: a card closed failed or
/// abandoned, a revert, or a run that failed an attempt. A fast win and a
/// card a run was cut on are routine.
#[test]
fn only_an_incident_is_full() {
    let (mut store, session) = memory_store();
    closed(
        &store,
        11,
        "'M', 'failed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    closed(
        &store,
        12,
        "'M', 'abandoned', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    closed(
        &store,
        13,
        "'M', 'landed', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    sql(
        &store,
        "UPDATE cards SET category = 'revert' WHERE id = 'c13'",
    );
    // Five routine peers at two hours, and one in 30 minutes: no longer a
    // retro of its own.
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
            "UPDATE cards SET state = 'closed', outcome = 'landed', size = 'S',
                 created_at = '2026-11-02T09:00:00.000Z', closed_at = '2026-11-02T09:20:00.000Z' WHERE id = '{}'",
            needed.id
        ),
    );
    let swept = store.learning_sweep(&owner()).unwrap();
    let by_card = |id: &str| {
        let retro = swept
            .value
            .scheduled
            .iter()
            .find(|r| r.card_id == id)
            .cloned()
            .unwrap();
        (retro.depth, retro.reason)
    };
    let full = (RetroDepth::Full, RetroReason::Incident);
    let light = (RetroDepth::Light, RetroReason::Routine);
    assert_eq!(by_card("c11"), full);
    assert_eq!(by_card("c12"), full);
    assert_eq!(by_card("c13"), full);
    assert_eq!(by_card("c31"), light);
    assert_eq!(by_card(&needed.id), light);

    // A run that failed an attempt makes an incident of a landed card; a
    // cut run does not.
    for (title, outcome) in [("failed", RunOutcome::Failed), ("cut", RunOutcome::Cut)] {
        let new = store
            .card_create(&owner(), None, card(CardKind::Ship, title), None)
            .unwrap()
            .value;
        let run = store
            .run_start(&owner(), None, builder_run(&new.id))
            .unwrap()
            .value
            .run;
        store
            .run_end(
                &owner(),
                &run.id,
                EndRun {
                    outcome,
                    note: Some("it stopped".into()),
                },
            )
            .unwrap();
        sql(
            &store,
            &format!(
                "UPDATE cards SET state = 'closed', outcome = 'landed', closed_at = '2026-11-03T09:00:00.000Z' WHERE id = '{}'",
                new.id
            ),
        );
        let swept = store.learning_sweep(&owner()).unwrap();
        let retro = &swept.value.scheduled[0];
        let expected = if outcome == RunOutcome::Failed {
            full
        } else {
            light
        };
        assert_eq!((retro.depth, retro.reason), expected, "{title}");
        assert_eq!(retro.counts["attempts"]["builder"], 1);
    }
}

#[test]
fn finish_rules_and_a_note_joins_the_queue() {
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
    store.learning_sweep(&owner()).unwrap();
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

    // A note joins the pending queue as the curator's, in the same act.
    let before = pending_count(&store);
    let noted = Finish {
        outcome: RetroOutcome::Noted,
        learning: None,
        note: Some(NewNote {
            project: None,
            kind: "gotcha".into(),
            title: "Never rebase a pushed branch".into(),
            body: "The builder rebased a pushed branch; the brief never said not to.".into(),
        }),
    };
    let done = store
        .retro_finish(&curator, Some("k1"), &full.id, noted.clone())
        .unwrap();
    assert!(!done.replayed);
    let retro = &done.value;
    assert_eq!(retro.state, RetroState::Done);
    assert_eq!(retro.outcome, Some(RetroOutcome::Noted));
    assert_eq!(
        retro.finished_by.as_deref(),
        Some(format!("curator:{}", full.id).as_str())
    );
    let pending = store
        .pending_note(retro.pending_id.as_deref().unwrap())
        .unwrap();
    assert_eq!(pending.source, "curator");
    assert_eq!(pending.retro_id.as_deref(), Some(full.id.as_str()));
    assert_eq!(pending.card_id.as_deref(), Some("c42"));
    assert_eq!(pending.project_id, "alpha");
    assert_eq!(pending_count(&store), before + 1);

    // The same key replays; another outcome is refused; the same outcome
    // without the key is a natural replay that writes nothing.
    let replay = store
        .retro_finish(&curator, Some("k1"), &full.id, noted)
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
                outcome: RetroOutcome::Noted,
                learning: None,
                note: None,
            },
        )
        .unwrap();
    assert!(natural.replayed);
    assert_eq!(pending_count(&store), before + 1);
    // The curator of a done retro adds nothing more.
    let error = store
        .mem_note(
            &curator,
            None,
            &NewNote {
                kind: "gotcha".into(),
                title: "Late".into(),
                body: "Late.".into(),
                ..NewNote::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error}");

    // A noted retro names its note, and only a noted one has one; a note
    // that breaks the queue's rules is refused with the retro left pending.
    closed(
        &store,
        43,
        "'M', 'abandoned', '2026-11-01T09:00:00.000Z', '2026-11-01T11:00:00.000Z'",
    );
    store.learning_sweep(&owner()).unwrap();
    let full2 = retros_of(&store, "alpha-43").remove(0);
    for finish in [
        Finish {
            outcome: RetroOutcome::Noted,
            learning: None,
            note: None,
        },
        Finish {
            note: Some(NewNote {
                kind: "gotcha".into(),
                title: "T".into(),
                body: "B".into(),
                ..NewNote::default()
            }),
            ..nothing()
        },
        Finish {
            outcome: RetroOutcome::Noted,
            learning: None,
            note: Some(NewNote {
                kind: "open-work".into(),
                title: "T".into(),
                body: "B".into(),
                ..NewNote::default()
            }),
        },
    ] {
        let error = store
            .retro_finish(&owner(), None, &full2.id, finish)
            .unwrap_err();
        assert!(matches!(error, Error::Usage(_)), "{error}");
    }
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
    store.learning_sweep(&owner()).unwrap();
    let retro = retros_of(&store, "alpha-44").remove(0);
    let other = retros_of(&store, "alpha-45").remove(0);
    let unfinished = |note: &str| Finish {
        outcome: RetroOutcome::Unfinished,
        learning: Some(note.into()),
        note: None,
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
