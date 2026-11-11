use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use proptest::prelude::*;
use rusqlite::types::Value as SqlValue;
use serde_json::json;

use super::approvals::{Decision, Execution, NewApproval, NewPr, PrFetch};
use super::asks::{NewAsk, Resolution};
use super::attention::NewAttention;
use super::cards::{
    BugRoute, CardEdit, Closed, MARK_LANDED, NewBug, NewCard, SUPERSEDED, TILLER_BUG,
};
use super::projects::{HostEdit, Init, NewProject, ProjectEdit};
use super::runs::{Bind, NewObservation, NewOrchestrator, NewRun, Relaunch, SpawnChecks};
use crate::domain::*;
use crate::error::Error;
use crate::store::{Actor, Applied, Ctx, Store};
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
            match store.card_edit(&owner(), None, &shared, current.rev, edit) {
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
        .card_edit(&owner(), None, &card.display_id, 1, edit("new"))
        .unwrap();
    let error = store
        .card_edit(&owner(), None, &card.display_id, 1, edit("newer"))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "conflict: card {} is at rev 2, you sent 1; title is now \"new\"",
            card.id
        )
    );
    // At the current rev, an edit to the values already there is a no-op.
    let same = store
        .card_edit(&owner(), None, &card.display_id, 2, edit("new"))
        .unwrap();
    assert!(same.replayed);
    assert!(same.events.is_empty());
}

/// A stale `rev` is a conflict even when the card already holds the sent
/// values: two writers incrementing from one read both send the same value,
/// and the second must not succeed. Only a retry under the same idempotency
/// key replays.
#[test]
fn stale_rev_with_matching_values_conflicts_unless_its_key_replays() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "x"), None)
        .unwrap()
        .value;
    let body = |text: &str| CardEdit {
        body: Some(Some(text.into())),
        ..CardEdit::default()
    };
    let first = store
        .card_edit(&owner(), Some("edit-a"), &card.id, 1, body("1"))
        .unwrap();
    assert!(!first.replayed);

    let before = snapshot(store.conn(), &[]);
    let error = store
        .card_edit(&owner(), Some("edit-b"), &card.id, 1, body("1"))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "conflict: card {} is at rev 2, you sent 1; body is now \"1\"",
            card.id
        )
    );
    let error = store
        .card_edit(&owner(), None, &card.id, 1, body("1"))
        .unwrap_err();
    assert_eq!(error.code(), "conflict");

    let replay = store
        .card_edit(&owner(), Some("edit-a"), &card.id, 1, body("1"))
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.value.rev, 2);
    assert_eq!(replay.events, first.events);
    assert_eq!(snapshot(store.conn(), &[]), before);
    // An edit that names no field changes nothing and races with no one.
    let empty = store
        .card_edit(&owner(), None, &card.id, 1, CardEdit::default())
        .unwrap();
    assert!(empty.replayed);

    // Project and host edits keep the same rule.
    let rename = || ProjectEdit {
        name: Some("Alpha 2".into()),
        ..ProjectEdit::default()
    };
    let rev = store.project("alpha").unwrap().rev;
    store
        .project_edit(&owner(), "alpha", rev, rename())
        .unwrap();
    let error = store
        .project_edit(&owner(), "alpha", rev, rename())
        .unwrap_err();
    assert_eq!(error.code(), "conflict");
    let host = store.hosts().unwrap().remove(0);
    let lanes = || HostEdit {
        lane_capacity: Some(3),
        ..HostEdit::default()
    };
    store
        .host_edit(&owner(), &host.id, host.rev, lanes())
        .unwrap();
    let error = store
        .host_edit(&owner(), &host.id, host.rev, lanes())
        .unwrap_err();
    assert_eq!(error.code(), "conflict");
}

/// A project keeps when its `rev` last moved, and a stale-rev conflict on it
/// says when that was, in Lisbon time with the weekday: an edit, a new card
/// and a stage change each move it.
#[test]
fn a_project_conflict_says_when_the_other_change_happened() {
    let (mut store, _) = memory_store();
    let at = |text: &str| Ctx {
        now: text.parse::<Timestamp>().unwrap(),
        ..owner()
    };
    let added = store.project("alpha").unwrap();
    assert_eq!(added.edited_at, added.added_at, "a new project");

    // Summer time: 14:05 UTC is 15:05 in Lisbon.
    let edited = store
        .project_edit(
            &at("2026-07-14T14:05:09.000Z"),
            "alpha",
            added.rev,
            ProjectEdit {
                capacity: Some(5),
                ..ProjectEdit::default()
            },
        )
        .unwrap()
        .value;
    assert_eq!(edited.edited_at.to_string(), "2026-07-14T14:05:09.000Z");
    let error = store
        .project_edit(
            &owner(),
            "alpha",
            added.rev,
            ProjectEdit {
                capacity: Some(6),
                ..ProjectEdit::default()
            },
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "conflict: project alpha is at rev {}, you sent {}; it changed Tue 2026-07-14 15:05; capacity is now 5",
            edited.rev, added.rev
        )
    );
    let Error::Conflict { edited_at, .. } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(edited_at.as_deref(), Some(&edited.edited_at));

    // Winter time: Lisbon is UTC. A new card moves the project's rev.
    store
        .card_create(
            &at("2026-11-03T14:00:00.000Z"),
            None,
            card(CardKind::Ship, "moves the rev"),
            None,
        )
        .unwrap();
    let carded = store.project("alpha").unwrap();
    assert_eq!(carded.rev, edited.rev + 1);
    assert_eq!(carded.edited_at.to_string(), "2026-11-03T14:00:00.000Z");
    let error = store
        .project_stage(&owner(), "alpha", edited.rev, Stage::PreLaunch, None)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("; it changed Tue 2026-11-03 14:00;"),
        "{error}"
    );
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
            None,
            &card.id,
            1,
            edit.clone(),
        )
        .unwrap_err();
    assert_eq!(error.code(), "forbidden");
    let edited = store
        .card_edit(&owner(), None, &card.id, 1, edit)
        .unwrap()
        .value;
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

/// A dependency already released by its card's close can still be removed
/// by hand, so the card stops listing it: the reason becomes `removed`, the
/// release time stays, and the event names the reason it had. A second
/// remove writes nothing.
#[test]
fn a_released_dependency_can_be_removed() {
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
    store
        .card_close(
            &owner(),
            &first.id,
            CardOutcome::Completed,
            None,
            Some("done: ok"),
        )
        .unwrap();
    let released = |store: &Store| -> (Option<String>, Option<String>) {
        store
            .conn()
            .query_row(
                "SELECT released_at, release_reason FROM card_deps WHERE card_id = ?1",
                [&second.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    let (at, reason) = released(&store);
    assert_eq!(reason.as_deref(), Some("completed"));

    let removed = store
        .dep_release(&owner(), &second.display_id, &first.display_id)
        .unwrap();
    let dep = removed.value.expect("the released dependency is removed");
    assert_eq!(dep.release_reason, Some(DepReleaseReason::Removed));
    assert_eq!(released(&store), (at, Some("removed".to_owned())));
    let payload: String = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'dep.released' ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(payload["reason"], "removed");
    assert_eq!(payload["was"], "completed");

    let again = store
        .dep_release(&owner(), &second.display_id, &first.display_id)
        .unwrap();
    assert!(again.value.is_none());
    assert!(again.events.is_empty());
}

/// A loop through a third card is refused too, and `waiting_on` names every
/// card that waits on a card, through the chain, so the card page can show
/// which choices would make a loop. A released dependency breaks the chain.
#[test]
fn a_dependency_loop_through_another_card_is_refused() {
    let (mut store, _) = memory_store();
    let [a, b, c] = ["a", "b", "c"].map(|title| {
        store
            .card_create(&owner(), None, card(CardKind::Ops, title), None)
            .unwrap()
            .value
    });
    store
        .dep_add(&owner(), &b.display_id, &a.display_id)
        .unwrap();
    store
        .dep_add(&owner(), &c.display_id, &b.display_id)
        .unwrap();
    let mut waiting = vec![b.id.clone(), c.id.clone()];
    waiting.sort();
    assert_eq!(
        super::cards::waiting_on(store.conn(), &a.id).unwrap(),
        waiting
    );
    let error = store
        .dep_add(&owner(), &a.display_id, &c.display_id)
        .unwrap_err();
    assert!(
        error.to_string().contains(&format!(
            "{} already waits on {}",
            c.display_id, a.display_id
        )),
        "{error}"
    );
    store
        .dep_release(&owner(), &c.display_id, &b.display_id)
        .unwrap();
    assert_eq!(
        super::cards::waiting_on(store.conn(), &a.id).unwrap(),
        vec![b.id.clone()]
    );
    store
        .dep_add(&owner(), &a.display_id, &c.display_id)
        .unwrap();
}

/// The Done column shows a closed card for three days (`state.md` section 6):
/// one closed two days ago is on the board, one closed four days ago is not.
#[test]
fn a_closed_card_leaves_the_board_after_three_days() {
    let (mut store, _) = memory_store();
    let recent = store
        .card_create(&owner(), None, card(CardKind::Ops, "recent"), None)
        .unwrap()
        .value;
    let old = store
        .card_create(&owner(), None, card(CardKind::Ops, "old"), None)
        .unwrap()
        .value;
    for (card, days) in [(&recent, 2), (&old, 4)] {
        store
            .card_close(
                &owner(),
                &card.id,
                CardOutcome::Completed,
                None,
                Some("done: ok"),
            )
            .unwrap();
        let closed_at = Timestamp::now().plus_minutes(-days * 24 * 60);
        store
            .conn()
            .execute(
                "UPDATE cards SET closed_at = ?2 WHERE id = ?1",
                rusqlite::params![card.id, closed_at],
            )
            .unwrap();
    }
    assert!(store.card_column(&recent.id).unwrap().is_some());
    assert!(store.card_column(&old.id).unwrap().is_none());
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

fn daemon() -> Ctx {
    Ctx::new(Actor::Daemon)
}

/// A ship card with open PR `number` at `head` and a merge approval
/// requested on it. Returns the card and the approval.
pub(crate) fn requested_merge(store: &mut Store, number: i64, head: &str) -> (Card, Approval) {
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "ship it"), None)
        .unwrap()
        .value;
    let pr = store
        .pr_link(
            &owner(),
            NewPr {
                card: card.id.clone(),
                run_id: None,
                number,
                url: format!("https://example.com/example/alpha/pull/{number}"),
                head_sha: Some(head.into()),
                base_ref: Some("main".into()),
            },
        )
        .unwrap()
        .value;
    let approval = store
        .approval_request(
            &owner(),
            NewApproval {
                kind: ApprovalKind::Merge,
                card: card.id.clone(),
                run_id: None,
                pr_id: Some(pr.id),
                branch: None,
                head_sha: head.into(),
                action: ApprovalAction::Squash,
                memory_paths: None,
                grant_id: None,
            },
        )
        .unwrap()
        .value;
    (card, approval)
}

pub(crate) fn accept() -> Decision {
    Decision {
        accept: true,
        via: AnsweredVia::Board,
        memory: Vec::new(),
        ask_id: None,
    }
}

fn recovery(store: &mut Store, card: &Card) -> Attention {
    store
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: card.id.clone(),
                kind: AttentionKind::Recovery,
                question: "PR merged outside Tiller. Mark landed or reopen work?".into(),
                options: Some(serde_json::json!([MARK_LANDED, "Reopen work"])),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value
}

/// Only a "Mark landed" answer to a recovery item is evidence for
/// `landed`; "Reopen work" is not.
#[test]
fn only_mark_landed_confirms_an_outside_merge() {
    let (mut store, _) = memory_store();
    let ship = store
        .card_create(&owner(), None, card(CardKind::Ship, "outside"), None)
        .unwrap()
        .value;
    let item = recovery(&mut store, &ship);
    store
        .attention_answer(&owner(), None, &item.id, "Reopen work", AnsweredVia::Board)
        .unwrap();
    let error = store
        .card_close(&owner(), &ship.id, CardOutcome::Landed, None, None)
        .unwrap_err();
    assert_eq!(error.code(), "refused");

    let item = recovery(&mut store, &ship);
    store
        .attention_answer(&owner(), None, &item.id, MARK_LANDED, AnsweredVia::Board)
        .unwrap();
    let closed = store
        .card_close(&owner(), &ship.id, CardOutcome::Landed, None, None)
        .unwrap()
        .value;
    assert_eq!(closed.card.outcome, Some(CardOutcome::Landed));
}

/// R07's outside-merge item on `card`, answered `answer` by the owner. With
/// `r07` false the item carries no R07 evidence.
fn answered_outside_merge(store: &mut Store, card: &Card, r07: bool, answer: &str) {
    let item = store
        .attention_open(
            &owner(),
            None,
            NewAttention {
                card: card.id.clone(),
                kind: AttentionKind::Recovery,
                question: "PR #7 was merged outside Tiller. Was that you?".into(),
                options: Some(json!([MARK_LANDED, SUPERSEDED])),
                evidence: Some(
                    if r07 {
                        json!({"r07_pr": "pr-7"})
                    } else {
                        json!({})
                    }
                    .to_string(),
                ),
                ..NewAttention::default()
            },
        )
        .unwrap()
        .value;
    store
        .attention_answer(&owner(), None, &item.id, answer, AnsweredVia::Board)
        .unwrap();
}

/// The daemon closes a card `superseded` only to carry out the owner's
/// "Superseded" to an R07 item answered since the card's last reopen: never
/// on its own, nor on another answer or another recovery item. Its other
/// closes stay refused, and the owner's own close keeps its full reach.
#[test]
fn daemon_closes_superseded_only_on_the_owner_s_r07_answer() {
    let (mut store, _) = memory_store();
    let ship = store
        .card_create(&owner(), None, card(CardKind::Ship, "outside"), None)
        .unwrap()
        .value;
    let close = |store: &mut Store, ctx: &Ctx, outcome| {
        store.card_close(ctx, &ship.id, outcome, Some("the owner's answer"), None)
    };
    let forbidden = |result: Result<Applied<Closed>, Error>| {
        let error = result.unwrap_err();
        assert_eq!(error.code(), "forbidden", "{error}");
        error
    };

    let error = forbidden(close(&mut store, &daemon(), CardOutcome::Superseded));
    assert!(
        error.to_string().contains("only on the owner's answer"),
        "{error}"
    );
    answered_outside_merge(&mut store, &ship, true, "Keep open");
    forbidden(close(&mut store, &daemon(), CardOutcome::Superseded));
    answered_outside_merge(&mut store, &ship, false, SUPERSEDED);
    forbidden(close(&mut store, &daemon(), CardOutcome::Superseded));

    answered_outside_merge(&mut store, &ship, true, SUPERSEDED);
    forbidden(close(&mut store, &daemon(), CardOutcome::Cancelled));
    let closed = close(&mut store, &daemon(), CardOutcome::Superseded)
        .unwrap()
        .value;
    assert_eq!(closed.card.outcome, Some(CardOutcome::Superseded));
    assert_eq!(closed.card.closed_by.as_deref(), Some("daemon"));

    // A reopen starts the card over: the answer before it is not evidence.
    store
        .card_reopen(&owner(), &ship.id, "the other change was reverted")
        .unwrap();
    let error = forbidden(close(&mut store, &daemon(), CardOutcome::Superseded));
    assert!(
        error
            .to_string()
            .contains("since the card was last reopened"),
        "{error}"
    );
    answered_outside_merge(&mut store, &ship, true, SUPERSEDED);
    close(&mut store, &daemon(), CardOutcome::Superseded).unwrap();

    store.card_reopen(&owner(), &ship.id, "once more").unwrap();
    let closed = close(&mut store, &owner(), CardOutcome::Superseded)
        .unwrap()
        .value;
    assert_eq!(closed.card.closed_by.as_deref(), Some("owner"));
}

/// A reopen starts the card over: a "Mark landed" answer given before it is
/// no evidence for closing the card `landed` again. A new answer is.
#[test]
fn a_mark_landed_answer_before_a_reopen_is_not_evidence() {
    let (mut store, _) = memory_store();
    let ship = store
        .card_create(&owner(), None, card(CardKind::Ship, "outside"), None)
        .unwrap()
        .value;
    let item = recovery(&mut store, &ship);
    store
        .attention_answer(&owner(), None, &item.id, MARK_LANDED, AnsweredVia::Board)
        .unwrap();
    store
        .card_close(&owner(), &ship.id, CardOutcome::Landed, None, None)
        .unwrap();
    store
        .card_reopen(&owner(), &ship.id, "the merge was reverted")
        .unwrap();

    let error = store
        .card_close(&owner(), &ship.id, CardOutcome::Landed, None, None)
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(
        error
            .to_string()
            .contains("since the card was last reopened"),
        "{error}"
    );
    assert_eq!(store.card(&ship.id).unwrap().outcome, None);

    let item = recovery(&mut store, &ship);
    store
        .attention_answer(&owner(), None, &item.id, MARK_LANDED, AnsweredVia::Board)
        .unwrap();
    let closed = store
        .card_close(&owner(), &ship.id, CardOutcome::Landed, None, None)
        .unwrap()
        .value;
    assert_eq!(closed.card.outcome, Some(CardOutcome::Landed));
}

/// A merge that succeeded before a reopen is no evidence for closing the
/// card `landed` again.
#[test]
fn a_merge_before_a_reopen_is_not_evidence() {
    let (mut store, _) = memory_store();
    let (ship, approval) = requested_merge(&mut store, 1, "a1b2c3d4e5f6a7b8");
    store
        .approval_decide(&owner(), None, &approval.id, accept())
        .unwrap();
    store
        .approval_execute(&daemon(), &approval.id, "a1b2c3d4e5f6a7b8")
        .unwrap();
    store
        .approval_finish(
            &daemon(),
            &approval.id,
            Finish::Succeeded,
            json!({"sha": "f".repeat(40)}),
        )
        .unwrap();
    if store.card(&ship.id).unwrap().outcome.is_none() {
        store
            .card_close(&owner(), &ship.id, CardOutcome::Landed, None, None)
            .unwrap();
    }
    store
        .card_reopen(&owner(), &ship.id, "the merge was reverted")
        .unwrap();

    let error = store
        .card_close(&owner(), &ship.id, CardOutcome::Landed, None, None)
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert_eq!(store.card(&ship.id).unwrap().outcome, None);
}

/// A report recorded before a reopen is no evidence for closing the card
/// `reported` again. A new report is.
#[test]
fn a_report_before_a_reopen_is_not_evidence() {
    let (mut store, _) = memory_store();
    let scout = store
        .card_create(&owner(), None, card(CardKind::Scout, "look around"), None)
        .unwrap()
        .value;
    let report = |store: &mut Store, title: &str| {
        store
            .artifact_add(
                &owner(),
                super::records::NewArtifact {
                    card: scout.id.clone(),
                    run_id: None,
                    kind: ArtifactKind::Report,
                    title: title.into(),
                    url: Some("https://example.com/report".into()),
                    path: None,
                },
            )
            .unwrap();
    };
    report(&mut store, "findings");
    store
        .card_close(&owner(), &scout.id, CardOutcome::Reported, None, None)
        .unwrap();
    store
        .card_reopen(&owner(), &scout.id, "needs a second look")
        .unwrap();

    let error = store
        .card_close(&owner(), &scout.id, CardOutcome::Reported, None, None)
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(
        error
            .to_string()
            .contains("since the card was last reopened"),
        "{error}"
    );

    report(&mut store, "second look");
    let closed = store
        .card_close(&owner(), &scout.id, CardOutcome::Reported, None, None)
        .unwrap()
        .value;
    assert_eq!(closed.card.outcome, Some(CardOutcome::Reported));
}

/// Cancelling a card stales its requested and accepted approvals, so the
/// daemon can never execute one for a closed card.
#[test]
fn closing_a_card_stales_its_approvals() {
    let (mut store, _) = memory_store();
    let (requested_card, requested) = requested_merge(&mut store, 1, "a1b2c3d4e5f6a7b8");
    store
        .card_cancel(&owner(), &requested_card.id, "not needed")
        .unwrap();
    let approval: Approval = super::get(store.conn(), "approval", &requested.id).unwrap();
    assert_eq!(approval.state, ApprovalState::Stale);

    let (accepted_card, accepted) = requested_merge(&mut store, 2, "0123456789abcdef");
    store
        .approval_decide(&owner(), None, &accepted.id, accept())
        .unwrap();
    store
        .card_cancel(&owner(), &accepted_card.id, "not needed")
        .unwrap();
    let approval: Approval = super::get(store.conn(), "approval", &accepted.id).unwrap();
    assert_eq!(approval.state, ApprovalState::Stale);
    let error = store
        .approval_execute(&daemon(), &accepted.id, "0123456789abcdef")
        .unwrap_err();
    assert_eq!(error.code(), "refused");
}

/// A deploy watch on `card`, as a direct push to a project with a deploy
/// workflow opens it.
fn deploy_watch(store: &mut Store, card: &Card) -> Obligation {
    store
        .obligation_open(
            &owner(),
            super::records::NewObligation {
                project_id: card.project_id.clone(),
                card_id: Some(card.id.clone()),
                what: "watch the deploy of a1b2c3d4 to success".into(),
                trigger_kind: TriggerKind::Event,
                trigger_event: Some(json!({
                    "kind": "deploy.finished",
                    "sha": "a1b2c3d4",
                    "environment": null,
                })),
                due_at: None,
            },
        )
        .unwrap()
        .value
}

/// Cancelling a card cancels its deploy watch, so a deploy that finishes
/// after the card is reopened cannot close it as landed.
#[test]
fn cancelling_a_card_cancels_its_deploy_watch() {
    let (mut store, _) = memory_store();
    let ship = store
        .card_create(&owner(), None, card(CardKind::Ship, "deploy"), None)
        .unwrap()
        .value;
    let watch = deploy_watch(&mut store, &ship);
    store.card_cancel(&owner(), &ship.id, "not needed").unwrap();
    let obligation: Obligation = super::get(store.conn(), "obligation", &watch.id).unwrap();
    assert_eq!(obligation.state, ObligationState::Cancelled);

    store.card_reopen(&owner(), &ship.id, "try again").unwrap();
    let error = store
        .deploy_record(&daemon(), None, &ship.id, DeployResult::Success, json!({}))
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert_eq!(store.card(&ship.id).unwrap().outcome, None);
}

/// "Reopen work" on a failed deploy's recovery item cancels the watch, so a
/// later deploy run cannot close the card the owner sent back to work.
#[test]
fn reopen_work_cancels_the_deploy_watch() {
    let (mut store, _) = memory_store();
    let ship = store
        .card_create(&owner(), None, card(CardKind::Ship, "deploy"), None)
        .unwrap()
        .value;
    let watch = deploy_watch(&mut store, &ship);
    let item = store
        .deploy_record(&daemon(), None, &ship.id, DeployResult::Failed, json!({}))
        .unwrap()
        .value
        .attention
        .unwrap();
    store
        .attention_answer(&owner(), None, &item.id, "Reopen work", AnsweredVia::Board)
        .unwrap();
    let obligation: Obligation = super::get(store.conn(), "obligation", &watch.id).unwrap();
    assert_eq!(obligation.state, ObligationState::Cancelled);

    let error = store
        .deploy_record(&daemon(), None, &ship.id, DeployResult::Success, json!({}))
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert_eq!(store.card(&ship.id).unwrap().outcome, None);
}

/// When the forge head moved after an approval, the old approval goes stale
/// and a new requested approval at the new head asks the owner again; that
/// one can be accepted and executed.
#[test]
fn a_moved_head_asks_again_on_a_new_approval() {
    let (mut store, _) = memory_store();
    let (card, old) = requested_merge(&mut store, 1, "aaaaaaaaaaaa1111");
    store
        .approval_decide(&owner(), None, &old.id, accept())
        .unwrap();
    let moved = "bbbbbbbbbbbb2222";
    let execution = store
        .approval_execute(&daemon(), &old.id, moved)
        .unwrap()
        .value;
    let Execution::Stale(stale) = execution else {
        panic!("a moved head must not execute: {execution:?}");
    };
    assert_eq!(stale.state, ApprovalState::Stale);

    let fresh: Approval = super::find(
        store.conn(),
        "card_id = ?1 AND state = 'requested'",
        &[&card.id],
    )
    .unwrap()
    .expect("a new approval at the moved head");
    assert_eq!(fresh.head_sha.as_deref(), Some(moved));
    assert_eq!(fresh.pr_id, old.pr_id);
    let item: Attention = super::find(
        store.conn(),
        "card_id = ?1 AND kind = 'merge' AND answered_at IS NULL AND withdrawn_at IS NULL",
        &[&card.id],
    )
    .unwrap()
    .expect("a merge item asks again");
    let evidence: serde_json::Value =
        serde_json::from_str(item.evidence.as_deref().unwrap()).unwrap();
    assert_eq!(evidence["approval_id"], fresh.id.as_str());

    // The forge poller records the new head; the fresh approval matches it.
    store
        .pr_refresh(
            &daemon(),
            fresh.pr_id.as_deref().unwrap(),
            PrFetch {
                state: PrState::Open,
                head_sha: Some(moved.into()),
                base_ref: Some("main".into()),
                checks: Checks::Success,
                review_decision: ReviewDecision::None,
                mergeable: Mergeable::Clean,
                merged_at: None,
                merge_sha: None,
                pipeline_state: PipelineState::None,
                fetch_error: None,
            },
        )
        .unwrap();

    store
        .approval_decide(&owner(), None, &fresh.id, accept())
        .unwrap();
    let execution = store
        .approval_execute(&daemon(), &fresh.id, moved)
        .unwrap()
        .value;
    assert!(matches!(execution, Execution::Executing(_)));
}

/// The board decides on the head it showed: another bound head refuses in
/// the decision's own transaction, and nothing changes.
#[test]
fn a_decision_on_a_shown_head_refuses_a_different_bound_head() {
    let (mut store, _) = memory_store();
    let (_, approval) = requested_merge(&mut store, 1, "aaaaaaaaaaaa1111");
    let error = store
        .approval_decide_shown(&owner(), None, &approval.id, accept(), "bbbbbbbbbbbb2222")
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(
        error
            .to_string()
            .contains("the head moved since it was shown")
    );
    let still: Approval = super::get(store.conn(), "approval", &approval.id).unwrap();
    assert_eq!(still.state, ApprovalState::Requested);

    store
        .approval_decide_shown(&owner(), None, &approval.id, accept(), "aaaaaaaaaaaa1111")
        .unwrap();
    let accepted: Approval = super::get(store.conn(), "approval", &approval.id).unwrap();
    assert_eq!(accepted.state, ApprovalState::Accepted);
}

/// A head sha that is not hex is a usage error, never a slicing panic.
#[test]
fn a_malformed_head_sha_is_refused() {
    let (mut store, _) = memory_store();
    let (_, approval) = requested_merge(&mut store, 1, "abcdef0123456789");
    let error = store
        .approval_execute(&daemon(), &approval.id, "héad-moved")
        .unwrap_err();
    assert_eq!(error.code(), "usage");
}

/// A resume can carry an ended session's Claude id into a new row: the ended
/// row gives it up. A live row keeps it.
#[test]
fn a_claude_id_moves_from_an_ended_session_only() {
    let (mut store, orchestrator) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let started = store
        .run_start(&owner(), None, builder_run(&card.id))
        .unwrap()
        .value;
    let bind = |claude: &str| Bind {
        session_id: started.session.id.clone(),
        claude_session_id: claude.into(),
        pane_id: None,
        transcript_path: None,
        source: "resume".into(),
    };
    let error = store
        .run_bind_session(&Ctx::new(Actor::Hook), bind("claude-orchestrator"))
        .unwrap_err();
    assert_eq!(error.code(), "refused");

    store
        .session_end(&owner(), &orchestrator, "restarted")
        .unwrap();
    let resumed = store
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
        .session;
    assert_eq!(
        resumed.claude_session_id.as_deref(),
        Some("claude-orchestrator")
    );
    let old: Session = super::get(store.conn(), "session", &orchestrator).unwrap();
    assert_eq!(old.claude_session_id, None);
}

/// An orchestrator start may name the session id its launch files carry,
/// and only a ULID.
#[test]
fn an_orchestrator_session_takes_the_id_its_caller_minted() {
    let (mut store, orchestrator) = memory_store();
    store
        .session_end(&owner(), &orchestrator, "restarted")
        .unwrap();
    let start = |id: &str| NewOrchestrator {
        id: Some(id.into()),
        project_id: "alpha".into(),
        ..NewOrchestrator::default()
    };
    let error = store
        .session_start(&owner(), start("not-a-ulid"))
        .unwrap_err();
    assert_eq!(error.code(), "usage");
    let id = crate::ids::new_ulid();
    let session = store
        .session_start(&owner(), start(&id))
        .unwrap()
        .value
        .session;
    assert_eq!(session.id, id);
}

/// A new attempt is supervised by the project's live orchestrator, and is
/// refused when there is none.
#[test]
fn a_relaunched_attempt_takes_the_live_orchestrator() {
    let (mut store, orchestrator) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let started = store
        .run_start(
            &owner(),
            None,
            NewRun {
                worktree_path: Some("/home/user/work/alpha-1".into()),
                ..builder_run(&card.id)
            },
        )
        .unwrap()
        .value;
    let relaunch = || Relaunch {
        note: "fresh brief".into(),
        new_attempt: true,
        stop_proof: Some("pane exited".into()),
        model: None,
        checks: None,
    };
    store
        .session_end(&owner(), &orchestrator, "restarted")
        .unwrap();
    let error = store
        .run_relaunch(&owner(), &started.run.id, relaunch())
        .unwrap_err();
    assert_eq!(error.code(), "refused");

    let successor = store
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "alpha".into(),
                claude_session_id: Some("claude-orchestrator-2".into()),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value
        .session;
    let relaunched = store
        .run_relaunch(&owner(), &started.run.id, relaunch())
        .unwrap()
        .value;
    assert_ne!(relaunched.run.id, started.run.id);
    assert_eq!(relaunched.run.supervisor_session_id, successor.id);
}

/// The board's add card keys under its own command: its key never replays
/// as a plain `ask.record`, whose response has a different shape.
#[test]
fn a_board_add_key_is_not_an_ask_record_key() {
    let (mut store, _) = memory_store();
    store
        .board_add(
            &Ctx::new(Actor::Board),
            Some("click-9"),
            card(CardKind::Ship, "from the board"),
        )
        .unwrap();
    let again = store
        .board_add(
            &Ctx::new(Actor::Board),
            Some("click-9"),
            card(CardKind::Ship, "from the board"),
        )
        .unwrap();
    assert!(again.replayed);
    let error = store
        .ask_record(
            &owner(),
            Some("click-9"),
            NewAsk {
                project_id: "alpha".into(),
                session_id: None,
                source: AskSource::Script,
                text: "note".into(),
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(error.to_string().contains("board.add"), "{error}");
}

/// A quota window the feed stops reporting is dropped, so the spawn floor
/// never reads a dead window; an empty feed is a failed read and keeps all.
#[test]
fn a_quota_window_the_feed_drops_is_gone() {
    let (mut store, _) = memory_store();
    let window = |label: &str| super::caches::QuotaReading {
        label: label.into(),
        percent_remaining: Some(50.0),
        resets_at: None,
        pace: None,
        reserve: None,
        burn_multiple: None,
        elapsed_percent: None,
        projected_exhaustion_at: None,
        title: None,
        kind: Some("weekly".into()),
        fetch_error: None,
    };
    let labels = |store: &Store| -> Vec<String> {
        super::list::<QuotaWindow>(store.conn(), "1 = 1 ORDER BY label", &[])
            .unwrap()
            .into_iter()
            .map(|w| w.label)
            .collect()
    };
    store
        .cache_quota(
            &daemon(),
            vec![window("claude-weekly"), window("codex-weekly")],
        )
        .unwrap();
    store.cache_quota(&daemon(), Vec::new()).unwrap();
    assert_eq!(labels(&store), ["claude-weekly", "codex-weekly"]);
    store
        .cache_quota(&daemon(), vec![window("claude-weekly")])
        .unwrap();
    assert_eq!(labels(&store), ["claude-weekly"]);
}

/// `run.place` records where spawn put a run once, for its supervisor.
#[test]
fn run_place_records_once_and_only_for_the_supervisor() {
    let (mut store, session) = memory_store();
    let orchestrator = Ctx::new(Actor::Session(session));
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "placed"), None)
        .unwrap()
        .value;
    let started = store
        .run_start(&orchestrator, None, builder_run(&card.id))
        .unwrap()
        .value;
    let run = started.run.id;
    let worker = Ctx::new(Actor::Session(started.session.id.clone()));
    let worktree = super::runs::Placement {
        worktree_path: Some("/home/user/worktrees/alpha-1".into()),
        branch: Some("tiller/alpha-1-build".into()),
        config_hash: Some("ab".repeat(32)),
        ..Default::default()
    };

    // The worker may not place its own run.
    let before = snapshot(store.conn(), &[]);
    let error = store
        .run_place(&worker, &run, worktree.clone())
        .unwrap_err();
    assert!(matches!(error, Error::Forbidden(_)), "{error}");
    assert_eq!(snapshot(store.conn(), &[]), before);

    let placed = store
        .run_place(&orchestrator, &run, worktree.clone())
        .unwrap();
    assert_eq!(placed.events.len(), 1);
    assert_eq!(placed.value.branch.as_deref(), Some("tiller/alpha-1-build"));
    let session: Session = super::get(store.conn(), "session", &started.session.id).unwrap();
    assert_eq!(session.config_hash, worktree.config_hash);

    // The same values again write nothing; a different value is refused.
    let again = store
        .run_place(&orchestrator, &run, worktree.clone())
        .unwrap();
    assert!(again.events.is_empty());
    let moved = super::runs::Placement {
        worktree_path: Some("/home/user/worktrees/elsewhere".into()),
        ..Default::default()
    };
    let error = store.run_place(&orchestrator, &run, moved).unwrap_err();
    assert!(error.to_string().contains("already"), "{error}");

    // The pane goes on the run's live session, once.
    let pane = |id: &str| super::runs::Placement {
        pane_id: Some(id.into()),
        ..Default::default()
    };
    store.run_place(&orchestrator, &run, pane("w2:p1")).unwrap();
    let session: Session = super::get(store.conn(), "session", &started.session.id).unwrap();
    assert_eq!(session.pane_id.as_deref(), Some("w2:p1"));
    assert!(store.run_place(&orchestrator, &run, pane("w3:p1")).is_err());

    // An ended run is not placed.
    store
        .run_end(
            &owner(),
            &run,
            super::runs::EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    let late = super::runs::Placement {
        base_sha: Some("0".repeat(40)),
        ..Default::default()
    };
    assert!(store.run_place(&orchestrator, &run, late).is_err());
}

/// `run.clean` refuses a worktree with uncommitted changes, as it does one
/// with unlanded commits (`recovery.md` R13).
#[test]
fn run_clean_refuses_uncommitted_changes() {
    let (mut store, session) = memory_store();
    let orchestrator = Ctx::new(Actor::Session(session));
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "dirty"), None)
        .unwrap()
        .value;
    let path = "/home/user/worktrees/alpha-1".to_owned();
    let mut new = builder_run(&card.id);
    new.worktree_path = Some(path.clone());
    let run = store.run_start(&orchestrator, None, new).unwrap().value.run;
    store
        .run_end(
            &owner(),
            &run.id,
            super::runs::EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    let reading = |dirty: Option<bool>| super::caches::WorktreeReading {
        path: path.clone(),
        project_id: Some("alpha".into()),
        run_id: Some(run.id.clone()),
        branch: None,
        head_sha: None,
        dirty,
        unlanded_commits: Some(0),
    };
    for dirty in [Some(true), None] {
        store
            .cache_worktree(&Ctx::new(Actor::Daemon), &run.host_id, reading(dirty))
            .unwrap();
        let error = store.run_clean(&owner(), &run.id, true).unwrap_err();
        assert!(error.to_string().contains("uncommitted changes"), "{error}");
    }
    store
        .cache_worktree(&Ctx::new(Actor::Daemon), &run.host_id, reading(Some(false)))
        .unwrap();
    store.run_clean(&owner(), &run.id, true).unwrap();
}

/// A new attempt works in its predecessor's worktree, so `run.clean` of the
/// predecessor refuses while the new attempt is active and leaves the
/// worktree held. Once the worktree is cleaned, no run of the chain can be
/// relaunched in it.
#[test]
fn run_clean_refuses_a_worktree_a_successor_works_in() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let path = "/home/user/worktrees/alpha-1".to_owned();
    let old = store
        .run_start(
            &owner(),
            None,
            NewRun {
                worktree_path: Some(path.clone()),
                ..builder_run(&card.id)
            },
        )
        .unwrap()
        .value
        .run;
    let relaunch = || Relaunch {
        note: "fresh brief".into(),
        new_attempt: true,
        stop_proof: Some("pane exited".into()),
        model: None,
        checks: None,
    };
    let new = store
        .run_relaunch(&owner(), &old.id, relaunch())
        .unwrap()
        .value
        .run;
    assert_eq!(new.worktree_path.as_deref(), Some(path.as_str()));
    let clean_reading = super::caches::WorktreeReading {
        path: path.clone(),
        project_id: Some("alpha".into()),
        run_id: Some(new.id.clone()),
        branch: None,
        head_sha: None,
        dirty: Some(false),
        unlanded_commits: Some(0),
    };
    store
        .cache_worktree(
            &Ctx::new(Actor::Daemon),
            &old.host_id,
            clean_reading.clone(),
        )
        .unwrap();

    let error = store.run_clean(&owner(), &old.id, true).unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(error.to_string().contains(&new.id), "{error}");
    let gone: Option<String> = store
        .conn()
        .query_row(
            "SELECT gone_at FROM worktrees WHERE host_id = ?1 AND path = ?2",
            [&old.host_id, &path],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(gone, None);
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'run.cleaned'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );

    // The last run of the chain returns the worktree. After that, neither
    // run may be relaunched in it.
    store
        .run_end(
            &owner(),
            &new.id,
            super::runs::EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    store
        .cache_worktree(&Ctx::new(Actor::Daemon), &new.host_id, clean_reading)
        .unwrap();
    store.run_clean(&owner(), &new.id, true).unwrap();
    for run in [&old.id, &new.id] {
        let error = store.run_relaunch(&owner(), run, relaunch()).unwrap_err();
        assert_eq!(error.code(), "refused");
        assert!(error.to_string().contains("already returned"), "{error}");
    }
}

/// A cache reading that sees the path again (the pool reissued it) clears
/// `gone_at`, but the run that cleaned it still may not be relaunched there.
#[test]
fn a_cleaned_run_is_not_relaunched_after_a_new_reading() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let path = "/home/user/worktrees/alpha-1".to_owned();
    let run = store
        .run_start(
            &owner(),
            None,
            NewRun {
                worktree_path: Some(path.clone()),
                ..builder_run(&card.id)
            },
        )
        .unwrap()
        .value
        .run;
    store
        .run_end(
            &owner(),
            &run.id,
            super::runs::EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    let reading = super::caches::WorktreeReading {
        path: path.clone(),
        project_id: Some("alpha".into()),
        run_id: None,
        branch: None,
        head_sha: None,
        dirty: Some(false),
        unlanded_commits: Some(0),
    };
    store
        .cache_worktree(&Ctx::new(Actor::Daemon), &run.host_id, reading.clone())
        .unwrap();
    store.run_clean(&owner(), &run.id, true).unwrap();
    store
        .cache_worktree(&Ctx::new(Actor::Daemon), &run.host_id, reading)
        .unwrap();

    let error = store
        .run_relaunch(
            &owner(),
            &run.id,
            Relaunch {
                note: "fresh brief".into(),
                new_attempt: true,
                stop_proof: None,
                model: None,
                checks: None,
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(error.to_string().contains("already returned"), "{error}");
    assert_eq!(
        store
            .conn()
            .query_row(
                "SELECT count(*) FROM runs WHERE card_id = ?1",
                [&card.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
}

fn orchestrator_notes(store: &Store, session: &str) -> Vec<String> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT body FROM outbox WHERE target_session_id = ?1 AND kind = 'note' ORDER BY id",
        )
        .unwrap();
    stmt.query_map([session], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

pub(crate) fn add_project(store: &mut Store, id: &str) {
    store
        .project_add(&owner(), project_input(id, None, Delivery::LocalOnly))
        .unwrap();
}

/// A card another principal files into a project notes that project's live
/// orchestrator in the same transaction; its own orchestrator's card does not.
#[test]
fn a_card_filed_by_another_principal_notes_the_orchestrator() {
    let (mut store, alpha) = memory_store();
    add_project(&mut store, "beta");
    let beta = store
        .session_start(
            &owner(),
            NewOrchestrator {
                project_id: "beta".into(),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value
        .session
        .id;

    // alpha's orchestrator files into beta: beta is told, alpha is not.
    let filed = store
        .card_create(
            &Ctx::new(Actor::Session(alpha.clone())),
            None,
            NewCard {
                project_id: "beta".into(),
                ..card(CardKind::Ship, "cross")
            },
            None,
        )
        .unwrap()
        .value;
    assert_eq!(
        orchestrator_notes(&store, &beta),
        vec![format!("new card {}: cross", filed.display_id)]
    );
    assert!(orchestrator_notes(&store, &alpha).is_empty());

    // The owner files into alpha: alpha is told.
    let owned = store
        .card_create(&owner(), None, card(CardKind::Ship, "from owner"), None)
        .unwrap()
        .value;
    assert_eq!(
        orchestrator_notes(&store, &alpha),
        vec![format!("new card {}: from owner", owned.display_id)]
    );

    // Its own orchestrator filing into alpha tells nobody.
    store
        .card_create(
            &Ctx::new(Actor::Session(alpha.clone())),
            None,
            card(CardKind::Ship, "own"),
            None,
        )
        .unwrap();
    assert_eq!(orchestrator_notes(&store, &alpha).len(), 1);
    assert_eq!(orchestrator_notes(&store, &beta).len(), 1);
}

/// With no live orchestrator the create enqueues nothing and the next
/// orchestrator is told of the card once, at start.
#[test]
fn a_card_filed_with_no_orchestrator_is_told_at_start() {
    let (mut store, _alpha) = memory_store();
    add_project(&mut store, "beta");
    let early = store
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "beta".into(),
                ..card(CardKind::Ship, "waiting")
            },
            None,
        )
        .unwrap()
        .value;
    let outbox: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM outbox WHERE card_id = ?1",
            [&early.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outbox, 0);

    let start = |store: &mut Store| {
        store
            .session_start(
                &owner(),
                NewOrchestrator {
                    project_id: "beta".into(),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap()
            .value
    };
    let first = start(&mut store);
    assert_eq!(
        orchestrator_notes(&store, &first.session.id),
        vec![format!("new card {}: waiting", early.display_id)]
    );

    // A restart does not tell it again.
    store
        .session_end(&Ctx::new(Actor::Daemon), &first.session.id, "test")
        .unwrap();
    let second = start(&mut store);
    assert!(orchestrator_notes(&store, &second.session.id).is_empty());
}

/// A raised capacity and a run ending each note the live orchestrator, so it
/// retries a spawn that was refused as full.
#[test]
fn a_freed_project_slot_wakes_the_orchestrator() {
    let (mut store, orchestrator) = memory_store();
    let rev = store.project("alpha").unwrap().rev;
    store
        .project_edit(
            &owner(),
            "alpha",
            rev,
            ProjectEdit {
                capacity: Some(1),
                ..ProjectEdit::default()
            },
        )
        .unwrap();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let run = store
        .run_start(&owner(), None, builder_run(&card.id))
        .unwrap()
        .value
        .run;
    let full = store
        .run_start(&owner(), None, builder_run(&card.id))
        .unwrap_err();
    assert_eq!(full.code(), "refused");
    let before = orchestrator_notes(&store, &orchestrator).len();

    let rev = store.project("alpha").unwrap().rev;
    store
        .project_edit(
            &owner(),
            "alpha",
            rev,
            ProjectEdit {
                capacity: Some(2),
                ..ProjectEdit::default()
            },
        )
        .unwrap();
    let notes = orchestrator_notes(&store, &orchestrator);
    assert_eq!(notes.len(), before + 1);
    assert!(notes[before].contains("capacity raised from 1 to 2"));

    // Lowering it frees nothing.
    let rev = store.project("alpha").unwrap().rev;
    store
        .project_edit(
            &owner(),
            "alpha",
            rev,
            ProjectEdit {
                capacity: Some(1),
                ..ProjectEdit::default()
            },
        )
        .unwrap();
    assert_eq!(orchestrator_notes(&store, &orchestrator).len(), before + 1);

    store
        .run_end(
            &owner(),
            &run.id,
            super::runs::EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            },
        )
        .unwrap();
    let notes = orchestrator_notes(&store, &orchestrator);
    assert_eq!(notes.len(), before + 2);
    assert!(notes[before + 1].contains("a run slot opened"));
}

/// A spawn that fails frees its slot but does not wake the orchestrator: the
/// wake would prompt a retry that fails the same way, forever.
#[test]
fn a_failed_spawn_does_not_wake_the_orchestrator() {
    let (mut store, orchestrator) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let run = store
        .run_start(&owner(), None, builder_run(&card.id))
        .unwrap()
        .value
        .run;
    let before = orchestrator_notes(&store, &orchestrator).len();

    store
        .run_spawn_failed(&owner(), &run.id, "no worktree")
        .unwrap();
    assert_eq!(orchestrator_notes(&store, &orchestrator).len(), before);
}

/// R16: more than five spawns in ten minutes on a project, or ten on the
/// host, refuses the next one; the window passing lifts it. Every run row
/// counts, ended or not.
#[test]
fn a_respawn_storm_trips_the_spawn_rate_limit() {
    let (mut store, _) = memory_store();
    let at = |minutes: i64| Timestamp::from_millis(1_900_000_000_000 + minutes * 60_000).unwrap();
    for id in ["beta", "gamma"] {
        store
            .project_add(
                &owner(),
                NewProject {
                    id: id.into(),
                    name: id.into(),
                    repo: Some(format!("example/{id}")),
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
                    project_id: id.into(),
                    claude_session_id: Some(format!("claude-{id}")),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap();
    }
    let spawn = |store: &mut Store, project: &str, minute: i64| {
        let new = NewCard {
            project_id: project.into(),
            ..card(CardKind::Ship, "work")
        };
        let card = store.card_create(&owner(), None, new, None).unwrap().value;
        let ctx = Ctx::at(Actor::Owner, at(minute));
        let started = store.run_start(&ctx, None, builder_run(&card.id));
        if let Ok(started) = &started {
            let end = super::runs::EndRun {
                outcome: RunOutcome::Abandoned,
                note: None,
            };
            store.run_end(&ctx, &started.value.run.id, end).unwrap();
        }
        started.map(drop)
    };

    for _ in 0..5 {
        spawn(&mut store, "alpha", 0).unwrap();
    }
    let error = spawn(&mut store, "alpha", 1).unwrap_err().to_string();
    assert!(
        error.contains("spawn rate limit: 5 spawns in the last 10 minutes, limit 5"),
        "{error}"
    );
    // Another project is still served, until the host has had ten.
    for _ in 0..5 {
        spawn(&mut store, "beta", 2).unwrap();
    }
    let error = spawn(&mut store, "gamma", 3).unwrap_err().to_string();
    assert!(
        error.contains("spawn rate limit: 10 spawns in the last 10 minutes, limit 10"),
        "{error}"
    );
    // The window passes.
    spawn(&mut store, "alpha", 11).unwrap();
    spawn(&mut store, "gamma", 11).unwrap();
}

/// P48: a relaunch in place starts a worker process too, so a loop of
/// relaunches trips the spawn rate limit the way a loop of spawns does.
#[test]
fn a_relaunch_in_place_counts_toward_the_spawn_rate() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let started = store
        .run_start(
            &owner(),
            None,
            NewRun {
                worktree_path: Some("/home/user/work/alpha-1".into()),
                ..builder_run(&card.id)
            },
        )
        .unwrap()
        .value;
    let relaunch = || Relaunch {
        note: "again".into(),
        new_attempt: false,
        stop_proof: Some("pane exited".into()),
        model: None,
        checks: None,
    };
    // The spawn was the first of five.
    for _ in 0..4 {
        store
            .run_relaunch(&owner(), &started.run.id, relaunch())
            .unwrap();
    }
    let error = store
        .run_relaunch(&owner(), &started.run.id, relaunch())
        .unwrap_err()
        .to_string();
    assert!(error.contains("spawn rate limit"), "{error}");
}

/// P48: retiring an artifact is recorded once; retiring it again changes
/// nothing. P47: feedback on a page is recorded on the card, once per key,
/// and by the daemon only.
#[test]
fn artifact_retire_and_feedback_are_recorded_once() {
    let (mut store, _) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "mock"), None)
        .unwrap()
        .value;
    let page = store
        .artifact_add(
            &owner(),
            super::records::NewArtifact {
                card: card.id.clone(),
                run_id: None,
                kind: ArtifactKind::Page,
                title: "Mock".into(),
                url: None,
                path: Some("/home/user/pages/mock.html".into()),
            },
        )
        .unwrap()
        .value;

    let entry = serde_json::json!({"prompt": "bigger"});
    let denied = store
        .artifact_feedback(&owner(), "page:1", &page.id, entry.clone())
        .unwrap_err();
    assert_eq!(denied.code(), "forbidden");
    let first = store
        .artifact_feedback(&daemon(), "page:1", &page.id, entry.clone())
        .unwrap();
    assert!(!first.replayed);
    let again = store
        .artifact_feedback(&daemon(), "page:1", &page.id, entry)
        .unwrap();
    assert!(again.replayed);

    let retired = store.artifact_retire(&owner(), &page.id).unwrap();
    assert_eq!(retired.events.len(), 1);
    assert!(retired.value.retired_at.is_some());
    let twice = store.artifact_retire(&owner(), &page.id).unwrap();
    assert!(twice.replayed);
    assert!(twice.events.is_empty());
}

fn project_input(id: &str, remote: Option<&str>, delivery: Delivery) -> NewProject {
    NewProject {
        id: id.into(),
        name: id.into(),
        repo: None,
        remote: remote.map(Into::into),
        default_branch: None,
        delivery,
        checks: None,
        capacity: None,
        deploy_workflow: None,
        deploy_environment: None,
        deploy_timeout_minutes: None,
    }
}

/// P10: a GitHub remote names the repo, so `project.add` fills `repo` from it
/// and records which forge it found; an explicit `repo` is kept.
#[test]
fn project_add_reads_the_repo_from_a_github_remote() {
    let (mut store, _) = memory_store();
    let added = store
        .project_add(
            &owner(),
            project_input(
                "gamma",
                Some(&format!("git{}github.com:example/gamma.git", '@')),
                Delivery::DirectPush,
            ),
        )
        .unwrap();
    assert_eq!(added.value.repo.as_deref(), Some("example/gamma"));

    let mut named = project_input(
        "delta",
        Some("https://github.com/example/delta.git"),
        Delivery::DirectPr,
    );
    named.repo = Some("example/other".into());
    let added = store.project_add(&owner(), named).unwrap();
    assert_eq!(added.value.repo.as_deref(), Some("example/other"));

    let events: Vec<String> = store
        .conn()
        .prepare("SELECT payload FROM events WHERE kind = 'project.added' AND entity_id = ?1")
        .unwrap()
        .query_map(["gamma"], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].contains("\"forge\":\"github\""), "{}", events[0]);
}

/// P10: a Gerrit remote is not a forge Tiller publishes to, so such a project
/// can only be local-only; the add says why instead of storing a delivery
/// mode that could never land.
#[test]
fn project_add_refuses_a_gerrit_remote_unless_local_only() {
    let (mut store, _) = memory_store();
    let remote = "ssh://gerrit.example.com:29418/epsilon";
    for delivery in [
        Delivery::NoMistakes,
        Delivery::DirectPr,
        Delivery::DirectPush,
    ] {
        let error = store
            .project_add(&owner(), project_input("epsilon", Some(remote), delivery))
            .unwrap_err();
        assert!(matches!(error, Error::Usage(_)), "{error}");
        assert!(error.to_string().contains("Gerrit"), "{error}");
    }
    assert!(store.project("epsilon").is_err(), "nothing was stored");

    let added = store
        .project_add(
            &owner(),
            project_input("epsilon", Some(remote), Delivery::LocalOnly),
        )
        .unwrap();
    assert_eq!(added.value.delivery, Delivery::LocalOnly);
    assert_eq!(added.value.repo, None);
}

/// P10: an add replayed with the same values still returns the existing row
/// after forge detection filled the repo.
#[test]
fn project_add_with_a_detected_repo_is_idempotent() {
    let (mut store, _) = memory_store();
    let input = project_input(
        "gamma",
        Some("https://github.com/example/gamma"),
        Delivery::DirectPush,
    );
    let first = store.project_add(&owner(), input.clone()).unwrap();
    let again = store.project_add(&owner(), input).unwrap();
    assert_eq!(first.value.id, again.value.id);
    assert_eq!(first.value.rev, again.value.rev);
}

/// P10: `checks` is an array of commands, or `{full, no_logic}`; anything
/// else is refused on add and on edit rather than stored.
#[test]
fn project_checks_take_two_shapes_and_refuse_the_rest() {
    let (mut store, _) = memory_store();
    for (id, checks) in [
        ("zeta", json!(["cargo test"])),
        (
            "eta",
            json!({"full": ["cargo test"], "no_logic": ["cargo fmt --check"]}),
        ),
    ] {
        let mut input = project_input(id, None, Delivery::DirectPush);
        input.checks = Some(checks.clone());
        assert_eq!(
            store.project_add(&owner(), input).unwrap().value.checks,
            checks
        );
    }

    let mut input = project_input("theta", None, Delivery::DirectPush);
    input.checks = Some(json!("cargo test"));
    assert!(matches!(
        store.project_add(&owner(), input).unwrap_err(),
        Error::Usage(_)
    ));

    let rev = store.project("zeta").unwrap().rev;
    let error = store
        .project_edit(
            &owner(),
            "zeta",
            rev,
            ProjectEdit {
                checks: Some(json!({"light": ["x"]})),
                ..ProjectEdit::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::Usage(_)), "{error}");
    assert_eq!(store.project("zeta").unwrap().checks, json!(["cargo test"]));
}

fn bug(title: &str) -> NewBug {
    NewBug {
        title: title.into(),
        body: "what happened".into(),
        evidence: None,
    }
}

fn set_bug_project(store: &mut Store, project: Option<&str>) -> Result<Host, Error> {
    let host = store.hosts().unwrap().remove(0);
    let edit = HostEdit {
        bug_project_id: Some(project.map(Into::into)),
        ..HostEdit::default()
    };
    store
        .host_edit(&owner(), &host.id, host.rev, edit)
        .map(|applied| applied.value)
}

fn routed_payload(store: &Store, card_id: &str) -> serde_json::Value {
    let payload: String = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'card.routed' AND card_id = ?1",
            [card_id],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&payload).unwrap()
}

/// `bug.file` routes to the host's bug project while it is open, else to
/// `fleet`, never to `fleet-1`, and records who filed it from where.
#[test]
fn a_tiller_bug_routes_to_the_bug_project_or_waits_in_fleet() {
    let (mut store, alpha) = memory_store();
    add_project(&mut store, "beta");
    let filer = Ctx::new(Actor::Session(alpha.clone()));

    // No bug project: fleet, with the filer recorded.
    let filed = store.bug_file(&filer, None, bug("no home")).unwrap().value;
    assert_eq!(filed.route, BugRoute::NoBugProject);
    assert_eq!(filed.card.project_id, crate::ids::FLEET_PROJECT);
    assert_ne!(filed.card.id, crate::ids::HOST_HEALTH_CARD);
    assert_eq!(filed.card.category.as_deref(), Some(TILLER_BUG));
    assert_eq!(filed.card.kind, CardKind::Ship);
    assert_eq!(
        routed_payload(&store, &filed.card.id),
        json!({
            "project_id": "fleet",
            "route": "no_bug_project",
            "bug_project_id": null,
            "filed_by_session_id": alpha,
            "filed_from_project_id": "alpha",
            "evidence": null,
        })
    );

    // A bug project: it goes there, and its orchestrator would be told.
    set_bug_project(&mut store, Some("beta")).unwrap();
    let evidence = json!({"exit": 3, "command": "tiller card list"});
    let filed = store
        .bug_file(
            &filer,
            Some("bug-a"),
            NewBug {
                evidence: Some(evidence.clone()),
                ..bug("routed")
            },
        )
        .unwrap();
    assert_eq!(filed.value.route, BugRoute::BugProject);
    assert_eq!(filed.value.card.project_id, "beta");
    assert_eq!(filed.value.bug_project_id.as_deref(), Some("beta"));
    let body = filed.value.card.body.clone().unwrap();
    assert!(
        body.starts_with("what happened\n\nEvidence:\n\n```json\n"),
        "{body}"
    );
    assert!(body.contains("\"exit\": 3"), "{body}");
    assert_eq!(
        routed_payload(&store, &filed.value.card.id)["evidence"],
        evidence
    );

    // A retry under the same key replays the same card.
    let replay = store
        .bug_file(
            &filer,
            Some("bug-a"),
            NewBug {
                evidence: Some(evidence),
                ..bug("routed")
            },
        )
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.value.card.id, filed.value.card.id);

    // The owner files with no session: nobody is recorded as the filer.
    let owned = store
        .bug_file(&owner(), None, bug("by hand"))
        .unwrap()
        .value;
    let payload = routed_payload(&store, &owned.card.id);
    assert_eq!(payload["filed_by_session_id"], json!(null));
    assert_eq!(payload["filed_from_project_id"], json!(null));

    // A bug project retired after it was picked sends bugs back to fleet.
    add_project(&mut store, "gamma");
    set_bug_project(&mut store, Some("gamma")).unwrap();
    store.project_retire(&owner(), "gamma").unwrap();
    let filed = store.bug_file(&filer, None, bug("retired")).unwrap().value;
    assert_eq!(filed.route, BugRoute::BugProjectRetired);
    assert_eq!(filed.card.project_id, crate::ids::FLEET_PROJECT);
    assert_eq!(filed.bug_project_id.as_deref(), Some("gamma"));
}

/// A worker reports a Tiller bug through its orchestrator; `bug.file` keeps
/// the card-create scope, and an empty title or body is a usage error.
#[test]
fn a_tiller_bug_needs_a_filer_that_may_create_cards() {
    let (mut store, alpha) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let worker = store
        .run_start(
            &Ctx::new(Actor::Session(alpha)),
            None,
            builder_run(&card.id),
        )
        .unwrap()
        .value
        .session
        .id;
    let before = snapshot(store.conn(), &[]);
    let error = store
        .bug_file(
            &Ctx::new(Actor::Session(worker)),
            None,
            bug("from a worker"),
        )
        .unwrap_err();
    assert_eq!(error.code(), "forbidden", "{error}");
    let error = store
        .bug_file(
            &owner(),
            None,
            NewBug {
                body: " ".into(),
                ..bug("x")
            },
        )
        .unwrap_err();
    assert_eq!(error.code(), "usage", "{error}");
    assert_eq!(snapshot(store.conn(), &[]), before);
}

/// The bug project is an open project this host owns, never `fleet`; it
/// clears, and deleting the project clears it with a host edit.
#[test]
fn the_bug_project_is_an_open_project_this_host_owns() {
    let (mut store, _) = memory_store();
    add_project(&mut store, "beta");
    add_project(&mut store, "gamma");
    add_project(&mut store, "delta");
    store.project_retire(&owner(), "gamma").unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO hosts (id, label, kind) VALUES ('01JC0000000000000000REM0TE', 'host-b', 'remote');
             UPDATE projects SET owner_host_id = '01JC0000000000000000REM0TE' WHERE id = 'delta';",
        )
        .unwrap();

    let before = snapshot(store.conn(), &[]);
    for (project, reason) in [
        ("fleet", "fleet cannot be the bug project"),
        ("gamma", "gamma is retired"),
        ("delta", "delta belongs to another host"),
    ] {
        let error = set_bug_project(&mut store, Some(project)).unwrap_err();
        assert_eq!(error.code(), "refused", "{error}");
        assert!(error.to_string().contains(reason), "{error}");
    }
    let error = set_bug_project(&mut store, Some("nope")).unwrap_err();
    assert_eq!(error.code(), "not_found", "{error}");
    let remote = store
        .hosts()
        .unwrap()
        .into_iter()
        .find(|host| host.kind == HostKind::Remote)
        .unwrap();
    let error = store
        .host_edit(
            &owner(),
            &remote.id,
            remote.rev,
            HostEdit {
                bug_project_id: Some(Some("beta".into())),
                ..HostEdit::default()
            },
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("picks its bug project on that host"),
        "{error}"
    );
    assert_eq!(snapshot(store.conn(), &[]), before);

    let host = set_bug_project(&mut store, Some("beta")).unwrap();
    assert_eq!(host.bug_project_id.as_deref(), Some("beta"));
    let host = set_bug_project(&mut store, None).unwrap();
    assert_eq!(host.bug_project_id, None);

    let picked = set_bug_project(&mut store, Some("beta")).unwrap();
    store.project_delete(&owner(), "beta").unwrap();
    let host = store
        .hosts()
        .unwrap()
        .into_iter()
        .find(|host| host.id == picked.id)
        .unwrap();
    assert_eq!(host.bug_project_id, None);
    // The clear is a host edit the board's Machines row follows.
    assert_eq!(host.rev, picked.rev + 1);
    let payload: String = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'host.edited' AND entity_id = ?1 ORDER BY seq DESC LIMIT 1",
            [&picked.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(payload.contains("project beta was deleted"), "{payload}");
}

/// `run.pause` keeps its reason: the same date and reason again is natural,
/// a new reason on the same date is a new pause (`protocol.md` 6.4).
#[test]
fn a_pause_records_its_reason_and_repeats_naturally() {
    let (mut store, session) = memory_store();
    let card = store
        .card_create(&owner(), None, card(CardKind::Ship, "work"), None)
        .unwrap()
        .value;
    let run = store
        .run_start(
            &Ctx::new(Actor::Session(session)),
            None,
            builder_run(&card.id),
        )
        .unwrap()
        .value
        .run;
    let until = Timestamp::now().plus_minutes(60);
    let board = Ctx::new(Actor::Board);
    let first = store
        .run_pause(&board, &run.id, until, Some(" the API key "))
        .unwrap();
    assert!(!first.events.is_empty());
    let again = store
        .run_pause(&board, &run.id, until, Some("the API key"))
        .unwrap();
    assert!(again.events.is_empty(), "the same pause is natural");
    let changed = store
        .run_pause(&board, &run.id, until, Some("a review"))
        .unwrap();
    assert!(!changed.events.is_empty());
    assert_eq!(
        super::runs::pause_reason(store.conn(), &run.id)
            .unwrap()
            .as_deref(),
        Some("a review")
    );
}

/// P16: an edit checks the forge as an add does. A Gerrit remote takes only
/// local-only, on a remote change or a delivery change, and nothing is stored
/// when it refuses.
#[test]
fn project_edit_refuses_a_gerrit_remote_unless_local_only() {
    let (mut store, _) = memory_store();
    let remote = "ssh://gerrit.example.com:29418/theta";
    store
        .project_add(&owner(), project_input("theta", None, Delivery::DirectPush))
        .unwrap();
    let rev = store.project("theta").unwrap().rev;
    let error = store
        .project_edit(
            &owner(),
            "theta",
            rev,
            ProjectEdit {
                remote: Some(Some(remote.into())),
                ..ProjectEdit::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::Usage(_)), "{error}");
    assert!(error.to_string().contains("Gerrit"), "{error}");
    assert_eq!(store.project("theta").unwrap().remote, None);

    store
        .project_edit(
            &owner(),
            "theta",
            rev,
            ProjectEdit {
                remote: Some(Some(remote.into())),
                delivery: Some(Delivery::LocalOnly),
                ..ProjectEdit::default()
            },
        )
        .unwrap();
    let rev = store.project("theta").unwrap().rev;
    let error = store
        .project_edit(
            &owner(),
            "theta",
            rev,
            ProjectEdit {
                delivery: Some(Delivery::DirectPr),
                ..ProjectEdit::default()
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("Gerrit"), "{error}");
}

/// P16: an edit that sets a GitHub remote fills `repo` from it, as an add does.
#[test]
fn project_edit_reads_the_repo_from_a_github_remote() {
    let (mut store, _) = memory_store();
    store
        .project_add(&owner(), project_input("iota", None, Delivery::DirectPush))
        .unwrap();
    let rev = store.project("iota").unwrap().rev;
    let edited = store
        .project_edit(
            &owner(),
            "iota",
            rev,
            ProjectEdit {
                remote: Some(Some("https://github.com/example/iota.git".into())),
                ..ProjectEdit::default()
            },
        )
        .unwrap();
    assert_eq!(edited.value.repo.as_deref(), Some("example/iota"));
}

/// P16: a new GitHub remote names the repo again when the old remote named
/// it, so the repo follows a moved remote. A repo set in the same edit wins,
/// a repo set apart from its remote stays, and a remote that names no GitHub
/// repo leaves the repo as it was.
#[test]
fn project_edit_reads_the_repo_again_when_the_remote_changes() {
    let (mut store, _) = memory_store();
    let input = project_input(
        "lambda",
        Some("https://github.com/example/lambda.git"),
        Delivery::DirectPush,
    );
    store.project_add(&owner(), input).unwrap();
    assert_eq!(
        store.project("lambda").unwrap().repo.as_deref(),
        Some("example/lambda")
    );
    let edit = |store: &mut crate::Store, edit: ProjectEdit| {
        let rev = store.project("lambda").unwrap().rev;
        store
            .project_edit(&owner(), "lambda", rev, edit)
            .unwrap()
            .value
    };

    let moved = edit(
        &mut store,
        ProjectEdit {
            remote: Some(Some("https://github.com/other/lambda-moved".into())),
            ..ProjectEdit::default()
        },
    );
    assert_eq!(moved.repo.as_deref(), Some("other/lambda-moved"));

    let named = edit(
        &mut store,
        ProjectEdit {
            remote: Some(Some("https://github.com/third/lambda.git".into())),
            repo: Some(Some("example/mirror".into())),
            ..ProjectEdit::default()
        },
    );
    assert_eq!(named.repo.as_deref(), Some("example/mirror"));

    // A repo set apart from its remote (a fork whose PRs go upstream) stays
    // when only the remote's address changes.
    let fork = edit(
        &mut store,
        ProjectEdit {
            remote: Some(Some("ssh://github.com/third/lambda.git".into())),
            ..ProjectEdit::default()
        },
    );
    assert_eq!(
        fork.remote.as_deref(),
        Some("ssh://github.com/third/lambda.git")
    );
    assert_eq!(fork.repo.as_deref(), Some("example/mirror"));

    let other = edit(
        &mut store,
        ProjectEdit {
            remote: Some(Some("https://git.example.com/lambda.git".into())),
            ..ProjectEdit::default()
        },
    );
    assert_eq!(other.repo.as_deref(), Some("example/mirror"));
}

/// P16: GitHub names ignore case, so a repo that differs from the one its
/// remote names only in case still counts as named by the remote, and
/// follows a moved remote.
#[test]
fn project_edit_follows_a_remote_that_names_the_repo_in_another_case() {
    let (mut store, _) = memory_store();
    let input = project_input(
        "lambda",
        Some("https://github.com/example/lambda.git"),
        Delivery::DirectPush,
    );
    store.project_add(&owner(), input).unwrap();
    let edit = |store: &mut crate::Store, edit: ProjectEdit| {
        let rev = store.project("lambda").unwrap().rev;
        store
            .project_edit(&owner(), "lambda", rev, edit)
            .unwrap()
            .value
    };
    let typed = edit(
        &mut store,
        ProjectEdit {
            repo: Some(Some("Example/Lambda".into())),
            ..ProjectEdit::default()
        },
    );
    assert_eq!(typed.repo.as_deref(), Some("Example/Lambda"));

    // A new address of the same repo keeps the case the owner chose.
    let same = edit(
        &mut store,
        ProjectEdit {
            remote: Some(Some("ssh://github.com/example/lambda.git".into())),
            ..ProjectEdit::default()
        },
    );
    assert_eq!(same.repo.as_deref(), Some("Example/Lambda"));

    let moved = edit(
        &mut store,
        ProjectEdit {
            remote: Some(Some("https://github.com/other/lambda-moved".into())),
            ..ProjectEdit::default()
        },
    );
    assert_eq!(moved.repo.as_deref(), Some("other/lambda-moved"));
}

/// P16: an add refuses an empty default branch, as an edit does.
#[test]
fn project_add_refuses_an_empty_default_branch() {
    let (mut store, _) = memory_store();
    for branch in ["", "  "] {
        let mut input = project_input("kappa", None, Delivery::DirectPush);
        input.default_branch = Some(branch.into());
        let error = store.project_add(&owner(), input).unwrap_err();
        assert!(matches!(error, Error::Usage(_)), "{error}");
        assert!(error.to_string().contains("default branch"), "{error}");
    }
    assert!(store.project("kappa").is_err(), "nothing was stored");
}
