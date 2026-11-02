use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use proptest::prelude::*;
use rusqlite::types::Value as SqlValue;

use super::asks::{NewAsk, Resolution};
use super::attention::NewAttention;
use super::cards::{CardEdit, NewCard};
use super::projects::{Init, NewProject};
use super::runs::{NewObservation, NewOrchestrator, NewRun, SpawnChecks};
use crate::domain::*;
use crate::error::Error;
use crate::store::{Actor, Ctx, Store};
use crate::time::Timestamp;

pub(crate) const DB_ENV: &str = "TILLER_TEST_DB";
const OPS_ENV: &str = "TILLER_TEST_OPS";
const SESSION_ENV: &str = "TILLER_TEST_SESSION";
const SHARED_ENV: &str = "TILLER_TEST_SHARED";

pub(crate) fn owner() -> Ctx {
    Ctx::new(Actor::Owner)
}

/// An initialized store with project `alpha`, a live orchestrator and a host
/// with two lanes. Returns the orchestrator's session id.
pub(crate) fn setup(store: &mut Store) -> String {
    store
        .init(
            &owner(),
            Init {
                host_label: "test-host".into(),
                lane_capacity: 2,
                ram_floor_mb: None,
                disk_floor_mb: None,
                boundary: Boundary::Personal,
            },
        )
        .unwrap();
    store
        .project_add(
            &owner(),
            NewProject {
                id: "alpha".into(),
                name: "Alpha".into(),
                repo: Some("example/alpha".into()),
                remote: None,
                default_branch: None,
                delivery: Delivery::DirectPr,
                checks: None,
                capacity: Some(3),
                deploy_workflow: None,
                deploy_environment: None,
                deploy_timeout_minutes: None,
            },
        )
        .unwrap();
    store
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "alpha".into(),
                claude_session_id: Some("claude-orchestrator".into()),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value
        .session
        .id
}

pub(crate) fn card(kind: CardKind, title: &str) -> NewCard {
    NewCard {
        project_id: "alpha".into(),
        kind,
        title: title.into(),
        ..NewCard::default()
    }
}

pub(crate) fn builder_run(card: &str) -> NewRun {
    NewRun {
        card: card.into(),
        role: RunRole::Builder,
        model: "test-model".into(),
        effort: Effort::Medium,
        host_id: None,
        worktree_path: None,
        branch: None,
        base_branch: None,
        base_sha: None,
        config_hash: None,
        checks: SpawnChecks::default(),
    }
}

pub(crate) fn memory_store() -> (Store, String) {
    let mut store = Store::open_in_memory().unwrap();
    let session = setup(&mut store);
    (store, session)
}

/// Every row of every table, for before and after comparisons.
pub(crate) fn snapshot(
    conn: &rusqlite::Connection,
    skip: &[&str],
) -> Vec<(String, Vec<Vec<SqlValue>>)> {
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .filter(|table| !skip.contains(&table.as_str()))
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let width = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..width).map(|i| row.get::<_, SqlValue>(i)).collect()
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

/// Runs one `#[ignore]`d child test of this binary in its own process.
pub(crate) fn child(test: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            test,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

pub(crate) fn wait_for(path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// One writer process: `ops` card creates and `ops` read-modify-write
/// increments of a shared card, retried on conflict, then one spawn.
#[test]
#[ignore = "child process of concurrent_writers_lose_no_updates"]
fn writer_child() {
    let Ok(db) = std::env::var(DB_ENV) else {
        return;
    };
    let ops: usize = std::env::var(OPS_ENV).unwrap().parse().unwrap();
    let session = std::env::var(SESSION_ENV).unwrap();
    let shared = std::env::var(SHARED_ENV).unwrap();
    let mut store = Store::open(db).unwrap();
    let mut mine = Vec::new();
    for i in 0..ops {
        loop {
            match store.card_create(
                &owner(),
                None,
                card(CardKind::Ship, &format!("card {i}")),
                None,
            ) {
                Ok(created) => {
                    mine.push(created.value.id);
                    break;
                }
                Err(Error::Unavailable(_)) => continue,
                Err(other) => panic!("card create failed: {other}"),
            }
        }
        loop {
            let current = store.card(&shared).unwrap();
            let n: i64 = current.body.as_deref().unwrap_or("0").parse().unwrap();
            let edit = CardEdit {
                body: Some(Some((n + 1).to_string())),
                ..CardEdit::default()
            };
            match store.card_edit(&owner(), &shared, current.rev, edit) {
                Ok(_) => break,
                Err(Error::Conflict { .. } | Error::Unavailable(_)) => continue,
                Err(other) => panic!("card edit failed: {other}"),
            }
        }
    }
    loop {
        match store.run_start(
            &Ctx::new(Actor::Session(session.clone())),
            None,
            builder_run(&mine[0]),
        ) {
            Ok(_) | Err(Error::Refused { .. }) => break,
            Err(Error::Unavailable(_)) => continue,
            Err(other) => panic!("run start failed: {other}"),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 6, failure_persistence: None, ..ProptestConfig::default() })]

    /// T2.1 acceptance: N writer processes racing on one database lose no
    /// update, mint unique contiguous display ids, and never overfill the
    /// host (invariants 5, 6 and 9).
    #[test]
    fn concurrent_writers_lose_no_updates(writers in 2usize..6, ops in 1usize..6) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("state.db");
        let (session, shared) = {
            let mut store = Store::open(&db).unwrap();
            let session = setup(&mut store);
            let shared = store.card_create(&owner(), None, card(CardKind::Ops, "shared"), None).unwrap().value.id;
            (session, shared)
        };
        let children: Vec<_> = (0..writers)
            .map(|_| {
                child("state::tests::writer_child")
                    .env(DB_ENV, &db)
                    .env(OPS_ENV, ops.to_string())
                    .env(SESSION_ENV, &session)
                    .env(SHARED_ENV, &shared)
                    .spawn()
                    .unwrap()
            })
            .collect();
        for writer in children {
            let output = writer.wait_with_output().unwrap();
            prop_assert!(output.status.success(), "writer failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        let store = Store::open(&db).unwrap();
        let total = (writers * ops) as i64;
        let shared = store.card(&shared).unwrap();
        prop_assert_eq!(shared.body, Some(total.to_string()), "an increment was lost");
        prop_assert_eq!(shared.rev, 1 + total);

        let mut display_ids: Vec<i64> = store
            .conn()
            .prepare("SELECT display_id FROM cards WHERE project_id = 'alpha'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|id| crate::ids::parse_display_id(&id.unwrap()).unwrap().1)
            .collect();
        display_ids.sort_unstable();
        prop_assert_eq!(display_ids, (1..=1 + total).collect::<Vec<_>>());
        prop_assert_eq!(store.project("alpha").unwrap().next_card_seq, 2 + total);

        let runs: i64 = store.conn().query_row("SELECT count(*) FROM runs", [], |row| row.get(0)).unwrap();
        let open: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM reservations WHERE released_at IS NULL", [], |row| row.get(0))
            .unwrap();
        prop_assert_eq!(runs, writers.min(2) as i64, "the host has two lanes");
        prop_assert_eq!(open, runs);
    }
}

/// `state.md` invariant 10: a known key returns the stored response and
/// writes nothing; a key used for another command is refused.
#[test]
fn known_idempotency_key_replays_without_writing() {
    let (mut store, _) = memory_store();
    let first = store
        .card_create(&owner(), Some("key-1"), card(CardKind::Ship, "once"), None)
        .unwrap();
    assert!(!first.replayed);
    let before = snapshot(store.conn(), &[]);

    let again = store
        .card_create(
            &owner(),
            Some("key-1"),
            card(CardKind::Ship, "a different title"),
            None,
        )
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.value, first.value);
    assert_eq!(again.events, first.events);
    assert_eq!(snapshot(store.conn(), &[]), before);

    let error = store
        .ask_record(
            &owner(),
            Some("key-1"),
            NewAsk {
                project_id: "alpha".into(),
                session_id: None,
                source: AskSource::Script,
                text: "note".into(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert_eq!(snapshot(store.conn(), &[]), before);
}

/// `state.md` invariant 6, in one process: the second spawn on a full host
/// is refused and writes nothing.
#[test]
fn full_host_refuses_another_run() {
    let (mut store, session) = memory_store();
    let orchestrator = Ctx::new(Actor::Session(session));
    for title in ["one", "two"] {
        let card = store
            .card_create(&owner(), None, card(CardKind::Ship, title), None)
            .unwrap()
            .value;
        store
            .run_start(&orchestrator, None, builder_run(&card.id))
            .unwrap();
    }
    let third = store
        .card_create(&owner(), None, card(CardKind::Ship, "three"), None)
        .unwrap()
        .value;
    let before = snapshot(store.conn(), &[]);
    let error = store
        .run_start(&orchestrator, None, builder_run(&third.id))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("host is full: 2 of 2 lanes reserved"),
        "{error}"
    );
    assert_eq!(snapshot(store.conn(), &[]), before);
    assert_eq!(store.capacity().unwrap()[0].free, Some(0));
}

/// G28: a worker session cannot start a run.
#[test]
fn workers_cannot_spawn() {
    let (mut store, session) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let started = store
        .run_start(
            &Ctx::new(Actor::Session(session)),
            None,
            builder_run(&card.id),
        )
        .unwrap()
        .value;
    let other = store
        .card_create(&owner(), None, self::card(CardKind::Ship, "more"), None)
        .unwrap()
        .value;
    let error = store
        .run_start(
            &Ctx::new(Actor::Session(started.session.id)),
            None,
            builder_run(&other.id),
        )
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
}

/// `state.md` section 8: a stale `rev` is refused with the readable message.
#[test]
fn stale_rev_is_refused_with_the_current_value() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "old"), None)
        .unwrap()
        .value;
    let edit = |title: &str| CardEdit {
        title: Some(title.into()),
        ..CardEdit::default()
    };
    store
        .card_edit(&owner(), &card.display_id, 1, edit("new"))
        .unwrap();
    let error = store
        .card_edit(&owner(), &card.display_id, 1, edit("newer"))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "conflict: card {} is at rev 2, you sent 1; title is now \"new\"",
            card.id
        )
    );
    // An edit to the values already there is a no-op whatever rev it names.
    let same = store
        .card_edit(&owner(), &card.display_id, 1, edit("new"))
        .unwrap();
    assert!(same.replayed);
    assert!(same.events.is_empty());
}

/// The dispatch override is owner only (`authority.md` section 3).
#[test]
fn dispatch_override_is_owner_only() {
    let (mut store, session) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "x"), None)
        .unwrap()
        .value;
    let edit = CardEdit {
        dispatch_override: Some(Some(super::cards::DispatchOverride {
            model: "big".into(),
            effort: Effort::High,
        })),
        ..CardEdit::default()
    };
    let error = store
        .card_edit(
            &Ctx::new(Actor::Session(session)),
            &card.id,
            1,
            edit.clone(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
    let edited = store.card_edit(&owner(), &card.id, 1, edit).unwrap().value;
    assert_eq!(
        edited.dispatch_override,
        Some(serde_json::json!({"model": "big", "effort": "high"}))
    );
}

/// `state.md` invariant 4: a card closes only with no open attention and no
/// active run; a cancel ends the runs, withdraws the questions and closes.
#[test]
fn close_needs_no_open_attention_and_no_active_run() {
    let (mut store, session) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "x"), None)
        .unwrap()
        .value;
    let started = store
        .run_start(
            &Ctx::new(Actor::Session(session)),
            None,
            builder_run(&card.id),
        )
        .unwrap()
        .value;
    let item = store
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: card.display_id.clone(),
                question: "Which way?".into(),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value;
    let error = store
        .card_close(
            &owner(),
            &card.id,
            CardOutcome::Abandoned,
            Some("stop"),
            None,
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains(&format!("attention item {} is open", item.id)),
        "{error}"
    );
    store
        .attention_withdraw(&owner(), &item.id, "moot")
        .unwrap();
    let error = store
        .card_close(
            &owner(),
            &card.id,
            CardOutcome::Abandoned,
            Some("stop"),
            None,
        )
        .unwrap_err();
    assert!(error.to_string().contains("is active"), "{error}");
    let error = store
        .card_close(&owner(), &card.id, CardOutcome::Landed, None, None)
        .unwrap_err();
    assert!(error.to_string().contains("landed needs"), "{error}");

    let cancelled = store
        .card_cancel(&owner(), &card.id, "not needed")
        .unwrap()
        .value;
    assert_eq!(cancelled.ended_runs, vec![started.run.id.clone()]);
    assert_eq!(cancelled.closed.card.outcome, Some(CardOutcome::Cancelled));
    let open: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM reservations WHERE released_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(open, 0);
    let again = store.card_cancel(&owner(), &card.id, "not needed").unwrap();
    assert!(again.replayed);
}

/// A question card closes `answered` in the answer's transaction; the same
/// words again replay, different words are refused.
#[test]
fn answering_a_question_card_closes_it() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(
            &owner(),
            None,
            card(CardKind::Question, "Blue or green?"),
            None,
        )
        .unwrap()
        .value;
    let item = store
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: card.id.clone(),
                question: "Blue or green?".into(),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value;
    let answered = store
        .attention_answer(
            &owner(),
            Some("answer-1"),
            &item.id,
            "green",
            AnsweredVia::Board,
        )
        .unwrap()
        .value;
    assert_eq!(
        answered.closed_card.unwrap().outcome,
        Some(CardOutcome::Answered)
    );
    assert!(
        answered.message.is_some(),
        "the orchestrator hears the answer"
    );
    let same = store
        .attention_answer(&owner(), None, &item.id, "green", AnsweredVia::Cli)
        .unwrap();
    assert!(same.replayed);
    let error = store
        .attention_answer(&owner(), None, &item.id, "blue", AnsweredVia::Cli)
        .unwrap_err();
    assert_eq!(error.code(), "refused");
}

/// Closing a card with a satisfying outcome releases its dependants; a
/// cycle is refused.
#[test]
fn dependencies_release_on_a_satisfying_close() {
    let (mut store, _) = memory_store();
    let first = store
        .card_create(&owner(), None, card(CardKind::Ops, "first"), None)
        .unwrap()
        .value;
    let second = store
        .card_create(&owner(), None, card(CardKind::Ops, "second"), None)
        .unwrap()
        .value;
    store
        .dep_add(&owner(), &second.display_id, &first.display_id)
        .unwrap();
    assert!(
        store
            .dep_add(&owner(), &second.display_id, &first.display_id)
            .unwrap()
            .replayed
    );
    let error = store
        .dep_add(&owner(), &first.display_id, &second.display_id)
        .unwrap_err();
    assert!(error.to_string().contains("already waits on"), "{error}");
    assert_eq!(
        store.card_column(&second.id).unwrap().unwrap().badge,
        "blocked"
    );
    let closed = store
        .card_close(
            &owner(),
            &first.id,
            CardOutcome::Completed,
            None,
            Some("done: ok"),
        )
        .unwrap()
        .value;
    assert_eq!(closed.released_dependants, vec![second.id.clone()]);
    assert_eq!(
        store.card_column(&second.id).unwrap().unwrap().badge,
        "filed"
    );
}

/// A prompt typed in a worker pane is a follow-up on the run's card and is
/// noted to the supervisor; a script note with no orchestrator warns.
#[test]
fn asks_route_to_the_right_session() {
    let (mut store, session) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "x"), None)
        .unwrap()
        .value;
    let started = store
        .run_start(
            &Ctx::new(Actor::Session(session.clone())),
            None,
            builder_run(&card.id),
        )
        .unwrap()
        .value;
    let recorded = store
        .ask_record(
            &Ctx::new(Actor::Hook),
            Some("prompt:w:1"),
            NewAsk {
                project_id: "alpha".into(),
                session_id: Some(started.session.id.clone()),
                source: AskSource::Prompt,
                text: "also fix the footer".into(),
            },
        )
        .unwrap()
        .value;
    assert_eq!(recorded.ask.disposition, Disposition::Followup);
    assert_eq!(recorded.followup.unwrap().id, card.id);
    assert_eq!(recorded.message.unwrap().target_session_id, session);

    store.session_end(&owner(), &session, "exit").unwrap();
    let note = store
        .ask_record(
            &owner(),
            None,
            NewAsk {
                project_id: "alpha".into(),
                session_id: None,
                source: AskSource::Script,
                text: "weekly report".into(),
            },
        )
        .unwrap()
        .value;
    assert_eq!(note.warning.as_deref(), Some(super::asks::NO_ORCHESTRATOR));
    assert_eq!(store.asked(Some("alpha")).unwrap().len(), 1);

    // The next orchestrator is told about every unresolved ask.
    let next = store
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "alpha".into(),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value;
    assert_eq!(next.notes.len(), 1);
    let resolved = store
        .ask_resolve(&owner(), &note.ask.id, Resolution::Chat { reason: None })
        .unwrap();
    assert!(!resolved.replayed);
    let again = store
        .ask_resolve(&owner(), &note.ask.id, Resolution::Chat { reason: None })
        .unwrap();
    assert!(again.replayed);
    let error = store
        .ask_resolve(
            &owner(),
            &note.ask.id,
            Resolution::Cancelled { reason: None },
        )
        .unwrap_err();
    assert_eq!(error.code(), "refused");
}

/// An observation older than the session's current one is stored but does
/// not move the activity backwards; only a verdict change is an event.
#[test]
fn observations_never_move_activity_backwards() {
    let (mut store, session) = memory_store();
    let at = |minutes: i64| Timestamp::now().plus_minutes(minutes);
    let observe = |verdict: Activity, observed_at: Timestamp| NewObservation {
        session_id: Some(session.clone()),
        run_id: None,
        source: ObservationSource::Hook,
        event: "Stop".into(),
        verdict: Some(verdict),
        observed_at,
        payload: serde_json::json!({}),
    };
    let first = store
        .observation_record(&Ctx::new(Actor::Hook), observe(Activity::Working, at(0)))
        .unwrap();
    assert!(first.value.verdict_changed);
    let older = store
        .observation_record(&Ctx::new(Actor::Hook), observe(Activity::Idle, at(-5)))
        .unwrap();
    assert!(!older.value.verdict_changed);
    assert!(older.events.is_empty());
    let same = store
        .observation_record(&Ctx::new(Actor::Hook), observe(Activity::Working, at(1)))
        .unwrap();
    assert!(same.events.is_empty());
    let current: Session = super::get(store.conn(), "session", &session).unwrap();
    assert_eq!(current.activity, Activity::Working);
    let stored: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored, 3);
}

/// `Store::write` rolls back a mutation that changes an authoritative row
/// without writing its event (invariant 3, enforced at runtime).
#[test]
fn a_write_without_its_event_is_rolled_back() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "x"), None)
        .unwrap()
        .value;
    let error = store
        .write(&owner(), "card.edit", None, |tx| {
            tx.conn().execute(
                "UPDATE cards SET title = 'sneaky' WHERE id = ?1",
                [&card.id],
            )?;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.code(), "internal");
    assert_eq!(store.card(&card.id).unwrap().title, "x");
}
