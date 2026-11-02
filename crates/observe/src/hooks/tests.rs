use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::{Value, json};
use tiller_core::domain::{Activity, ObservationSource};
use tiller_core::{Store, Timestamp};

use super::*;
use crate::testing::{count, file_store, fleet, session};

/// The 33 hook events Claude Code documents, as `protocol.md` section 4.2
/// lists them.
const DOCUMENTED: &[&str] = &[
    "SessionStart",
    "Setup",
    "UserPromptSubmit",
    "UserPromptExpansion",
    "PreToolUse",
    "PermissionRequest",
    "PermissionDenied",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "Notification",
    "MessageDisplay",
    "SubagentStart",
    "SubagentStop",
    "TaskCreated",
    "TaskCompleted",
    "Stop",
    "StopFailure",
    "TeammateIdle",
    "InstructionsLoaded",
    "ConfigChange",
    "CwdChanged",
    "DirectoryAdded",
    "FileChanged",
    "WorktreeCreate",
    "WorktreeRemove",
    "PreCompact",
    "PostCompact",
    "PreModelSwitch",
    "PostModelSwitch",
    "Elicitation",
    "ElicitationResult",
    "SessionEnd",
];

fn env(session: Option<&str>) -> Env {
    Env {
        tiller_home: "/home/user/.tiller".into(),
        user_home: Some("/home/user".into()),
        tiller_session_id: session.map(str::to_owned),
    }
}

fn input(claude: &str, fields: Value) -> Vec<u8> {
    let mut map = fields.as_object().cloned().unwrap_or_default();
    map.insert("session_id".into(), json!(claude));
    map.insert(
        "transcript_path".into(),
        json!("/home/user/.claude/projects/x/t.jsonl"),
    );
    map.insert("cwd".into(), json!("/home/user/alpha"));
    serde_json::to_vec(&Value::Object(map)).unwrap()
}

/// A clock that never repeats a millisecond, so consecutive hooks in a test
/// are strictly ordered the way real ones are.
fn tick() -> Timestamp {
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = Timestamp::now().as_millis();
    let prev = LAST.fetch_max(now, Ordering::SeqCst);
    let ms = if now > prev {
        now
    } else {
        LAST.fetch_add(1, Ordering::SeqCst) + 1
    };
    Timestamp::from_millis(ms).unwrap()
}

/// Maps and writes one hook call, the way `run` does once the store is open.
fn hook(store: &mut Store, env: &Env, event: &str, input: &[u8]) -> Outcome {
    match observe(event, input, env, tick()).unwrap() {
        Some(observed) => apply(store, env, &observed).unwrap(),
        None => Outcome::Ignored,
    }
}

fn newest(store: &Store) -> (String, Option<String>, Value) {
    store
        .conn()
        .query_row(
            "SELECT event, verdict, payload FROM observations ORDER BY rowid DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[test]
fn every_event_has_exactly_one_handler() {
    let mut seen = HashSet::new();
    for spec in EVENTS {
        assert!(seen.insert(spec.name), "{} has two entries", spec.name);
        assert!(
            DOCUMENTED.contains(&spec.name),
            "{} is not documented",
            spec.name
        );
    }
    for name in DOCUMENTED {
        assert!(seen.contains(name), "{name} has no handler");
    }
    assert_eq!(EVENTS.len(), 33);
    assert_eq!(handler("SomeEventFrom2027"), Handler::UnknownEvent);
    // An ignored event never touches the database or even parses its input.
    assert_eq!(
        observe("Setup", b"not json", &env(None), Timestamp::now()),
        Ok(None)
    );
}

#[test]
fn every_notification_type_has_one_verdict() {
    let names: HashSet<_> = NOTIFICATION_TYPES.iter().map(|(name, _)| *name).collect();
    assert_eq!(names.len(), 13, "13 documented types, none twice");
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.orchestrator));
    for (kind, verdict) in NOTIFICATION_TYPES.iter().chain(&[("brand_new_type", None)]) {
        hook(
            &mut store,
            &env,
            "Notification",
            &input(
                "claude-orch",
                json!({"notification_type": kind, "message": "m"}),
            ),
        );
        let (event, stored, _) = newest(&store);
        assert_eq!(event, format!("Notification:{kind}"));
        assert_eq!(stored, verdict.map(|v| v.to_string()), "{kind}");
    }
}

/// T2.6 acceptance: a permission prompt is detected. The request itself
/// does not flip the board; the notification about 6 s later does.
#[test]
fn permission_prompt_sets_waiting_permission() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    hook(
        &mut store,
        &env,
        "SessionStart",
        &input("claude-worker", json!({"source": "startup"})),
    );
    hook(
        &mut store,
        &env,
        "PreToolUse",
        &input(
            "claude-worker",
            json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf /tmp/x"}}),
        ),
    );
    hook(
        &mut store,
        &env,
        "PermissionRequest",
        &input(
            "claude-worker",
            json!({"tool_name": "Bash", "tool_input": {"command": "rm -rf /tmp/x"}}),
        ),
    );
    assert_eq!(session(&store, &fleet.worker).activity, Activity::Working);

    hook(
        &mut store,
        &env,
        "Notification",
        &input(
            "claude-worker",
            json!({"notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}),
        ),
    );
    let worker = session(&store, &fleet.worker);
    assert_eq!(worker.activity, Activity::WaitingPermission);
    assert_eq!(worker.activity_source, Some(ObservationSource::Hook));
    // Never an attention row: permission prompts live on the session.
    assert_eq!(count(&store, "SELECT count(*) FROM attention"), 0);
}

/// T2.6 acceptance: `StopFailure` is detected, with its error in the event.
#[test]
fn stop_failure_sets_errored() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    hook(
        &mut store,
        &env,
        "UserPromptSubmit",
        &input("claude-worker", json!({"prompt": "go"})),
    );
    hook(
        &mut store,
        &env,
        "StopFailure",
        &input(
            "claude-worker",
            json!({"error": "rate_limit", "error_details": "429 for org 1234"}),
        ),
    );
    let (event, verdict, payload) = newest(&store);
    assert_eq!(event, "StopFailure:rate_limit");
    assert_eq!(verdict.as_deref(), Some("errored"));
    assert!(payload.get("error_details").is_none());
    assert_eq!(session(&store, &fleet.worker).activity, Activity::Errored);
}

/// `recovery.md` R12: repeated API errors are `errored`, never a cut or a
/// death. Nothing ends the session or the run; the note to the orchestrator
/// is the daemon's (T2.7), and every failure is on record for it.
#[test]
fn repeated_stop_failure_notes_orchestrator_and_ends_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    for attempt in 0..3 {
        hook(
            &mut store,
            &env,
            "StopFailure",
            &input(
                "claude-worker",
                json!({"error": "overloaded", "attempt": attempt}),
            ),
        );
    }
    let worker = session(&store, &fleet.worker);
    assert_eq!(worker.activity, Activity::Errored);
    assert!(worker.ended_at.is_none());
    let run: tiller_core::domain::Run =
        tiller_core::state::get(store.conn(), "run", &fleet.run).unwrap();
    assert_eq!(run.outcome, None);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM observations WHERE event = 'StopFailure:overloaded'"
        ),
        3
    );
}

/// T2.6 acceptance: session end is detected, and a real end ends the row.
#[test]
fn session_end_logout_ends_session() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    // The orchestrator has no TILLER_SESSION_ID here: found by its Claude id.
    let env = env(None);
    hook(
        &mut store,
        &env,
        "SessionEnd",
        &input("claude-orch", json!({"reason": "logout"})),
    );
    let ended = session(&store, &fleet.orchestrator);
    assert_eq!(ended.activity, Activity::Ended);
    assert_eq!(ended.end_reason.as_deref(), Some("logout"));
    assert!(ended.ended_at.is_some());
}

/// `clear` and `resume` are followed by a `SessionStart` that rebinds the same
/// row, so they record the end without ending anything.
#[test]
fn session_end_clear_keeps_the_session_and_start_rebinds_it() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.orchestrator));
    hook(&mut store, &env, "Stop", &input("claude-orch", json!({})));
    hook(
        &mut store,
        &env,
        "SessionEnd",
        &input("claude-orch", json!({"reason": "clear"})),
    );
    let kept = session(&store, &fleet.orchestrator);
    assert!(kept.ended_at.is_none());
    assert_eq!(kept.activity, Activity::Idle);

    hook(
        &mut store,
        &env,
        "SessionStart",
        &input(
            "claude-orch-2",
            json!({"source": "clear", "model": "test-model"}),
        ),
    );
    let rebound = session(&store, &fleet.orchestrator);
    assert_eq!(rebound.claude_session_id.as_deref(), Some("claude-orch-2"));
    let (event, verdict, payload) = newest(&store);
    assert_eq!(event, "SessionStart:clear");
    assert_eq!(verdict, None);
    assert_eq!(payload["model"], "test-model");
    let bound: Value = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'session.bound' ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(bound["previous_claude_session_id"], "claude-orch");
}

#[test]
fn session_start_binds_the_spawned_worker() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    hook(
        &mut store,
        &env,
        "SessionStart",
        &input("claude-worker", json!({"source": "startup"})),
    );
    let worker = session(&store, &fleet.worker);
    assert_eq!(worker.claude_session_id.as_deref(), Some("claude-worker"));
    assert_eq!(
        worker.transcript_path.as_deref(),
        Some("/home/user/.claude/projects/x/t.jsonl")
    );
    // Without the env var, the next hook still finds it by its Claude id.
    hook(
        &mut store,
        &self::env(None),
        "Stop",
        &input("claude-worker", json!({})),
    );
    let run: Option<String> = store
        .conn()
        .query_row(
            "SELECT run_id FROM observations ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(run.as_deref(), Some(fleet.run.as_str()));
}

#[test]
fn unmapped_event_is_still_recorded() {
    let mut store = Store::open_in_memory().unwrap();
    fleet(&mut store);
    let outcome = hook(
        &mut store,
        &env(None),
        "Stop",
        &input("claude-stranger", json!({})),
    );
    assert!(matches!(outcome, Outcome::Recorded { .. }));
    let session_id: Option<String> = store
        .conn()
        .query_row("SELECT session_id FROM observations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(session_id, None);
    let (_, _, payload) = newest(&store);
    assert_eq!(payload["claude_session_id"], "claude-stranger");
}

/// A hook delivered twice is written once, and a late arrival never moves
/// the session's activity backwards (`recovery.md` R14).
#[test]
fn duplicate_hook_rows_do_not_move_activity_backwards() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    let at = |ms: i64| Timestamp::from_millis(1_793_620_000_000 + ms).unwrap();
    let mut call = |event: &str, input: &[u8], when: i64| {
        let observed = observe(event, input, &env, at(when)).unwrap().unwrap();
        apply(&mut store, &env, &observed).unwrap()
    };
    call(
        "UserPromptSubmit",
        &input("claude-worker", json!({"prompt": "go"})),
        0,
    );
    let stop = input("claude-worker", json!({"stop_hook_active": false}));
    assert!(matches!(call("Stop", &stop, 10), Outcome::Recorded { .. }));
    // Redelivered later, even pretty-printed: the key matches, nothing written.
    let pretty =
        serde_json::to_vec_pretty(&serde_json::from_slice::<Value>(&stop).unwrap()).unwrap();
    assert_eq!(call("Stop", &pretty, 20), Outcome::Replayed);
    call(
        "UserPromptSubmit",
        &input("claude-worker", json!({"prompt": "more"})),
        30,
    );
    // A Stop observed before that prompt arrives late: stored, moves nothing.
    let late = input("claude-worker", json!({"stop_hook_active": true}));
    assert!(matches!(call("Stop", &late, 25), Outcome::Recorded { .. }));

    assert_eq!(session(&store, &fleet.worker).activity, Activity::Working);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM observations WHERE event = 'Stop'"
        ),
        2
    );
}

/// Tool events write a row only when the session is not already freshly
/// `working` (`protocol.md` section 4.3).
#[test]
fn tool_events_coalesce_while_working_is_fresh() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    let call = |n: u32| {
        input(
            "claude-worker",
            json!({"tool_name": "Read", "tool_use_id": format!("t{n}")}),
        )
    };
    assert!(matches!(
        hook(&mut store, &env, "PreToolUse", &call(1)),
        Outcome::Recorded { .. }
    ));
    assert_eq!(
        hook(&mut store, &env, "PostToolUse", &call(1)),
        Outcome::Coalesced
    );
    assert_eq!(
        hook(&mut store, &env, "PostToolUseFailure", &call(2)),
        Outcome::Coalesced
    );

    // A minute later the next tool call refreshes the verdict.
    let later = Timestamp::from_millis(tick().as_millis() + TOOL_COALESCE_MS).unwrap();
    let observed = observe("PreToolUse", &call(3), &env, later)
        .unwrap()
        .unwrap();
    assert!(matches!(
        apply(&mut store, &env, &observed).unwrap(),
        Outcome::Recorded { .. }
    ));

    // After a turn end the first tool call writes again.
    let at = |ms: i64| Timestamp::from_millis(later.as_millis() + ms).unwrap();
    let stop = observe("Stop", &input("claude-worker", json!({})), &env, at(1))
        .unwrap()
        .unwrap();
    apply(&mut store, &env, &stop).unwrap();
    let observed = observe("PreToolUse", &call(4), &env, at(2))
        .unwrap()
        .unwrap();
    assert!(matches!(
        apply(&mut store, &env, &observed).unwrap(),
        Outcome::Recorded { .. }
    ));
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM observations WHERE event LIKE '%ToolUse%'"
        ),
        3
    );
}

/// Neither a reply, a prompt, a tool input or output, nor error details are
/// ever stored, for a mapped or an unknown event.
#[test]
fn payload_allowlist_drops_reply_and_tool_fields() {
    let secret = "sk-test-SECRET";
    let fields = json!({
        "prompt": secret,
        "last_assistant_message": secret,
        "tool_input": {"command": secret},
        "tool_response": {"stdout": secret},
        "error_details": secret,
        "message": "Claude needs your permission to use Bash",
        "tool_name": "Bash",
        "notification_type": "permission_prompt",
        "error": "server_error",
    });
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    for spec in EVENTS.iter().filter(|spec| spec.handler != Handler::Ignore) {
        hook(
            &mut store,
            &env,
            spec.name,
            &input("claude-worker", fields.clone()),
        );
    }
    hook(
        &mut store,
        &env,
        "BrandNewEvent",
        &input("claude-worker", fields.clone()),
    );
    let payloads: String = store
        .conn()
        .query_row(
            "SELECT group_concat(payload, ' ') FROM observations",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !payloads.contains(secret),
        "a stored payload leaked: {payloads}"
    );
    // The unknown event keeps everything else, so the table can be extended.
    let (event, _, payload) = newest(&store);
    assert_eq!(event, "BrandNewEvent");
    assert_eq!(payload["tool_name"], "Bash");
}

/// The payload each event keeps, pinned. A change here is a protocol change.
#[test]
fn payload_allowlists_snapshot() {
    let table: BTreeMap<&str, Value> = EVENTS
        .iter()
        .map(|spec| {
            (
                spec.name,
                json!({"handler": spec.handler.as_str(), "keep": spec.keep}),
            )
        })
        .collect();
    insta::assert_json_snapshot!(json!({"events": table, "never_stored": NEVER_STORED}));
}

#[test]
fn unknown_fields_are_tolerated_and_odd_inputs_never_panic() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    let future = input(
        "claude-worker",
        json!({"notification_type": "idle_prompt", "brand_new_field": {"nested": [1, 2]}, "ui_hint": 3}),
    );
    assert!(matches!(
        hook(&mut store, &env, "Notification", &future),
        Outcome::Recorded { .. }
    ));
    // Odd shapes the docs do not promise: wrong types, missing fields.
    for (event, fields) in [
        ("Notification", json!({"notification_type": 7})),
        ("StopFailure", json!({})),
        ("SessionEnd", json!({"reason": null})),
        (
            "Stop",
            json!({"background_tasks": "two", "session_crons": [1]}),
        ),
        ("SessionStart", json!({})),
    ] {
        assert!(
            matches!(
                hook(&mut store, &env, event, &input("claude-worker", fields)),
                Outcome::Recorded { .. }
            ),
            "{event}"
        );
    }
    let unreadable = observe("Stop", b"{not json", &env, Timestamp::now());
    assert!(unreadable.is_err());
}

#[test]
fn stop_records_background_work_and_wake_lines_are_classified() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.orchestrator));
    hook(
        &mut store,
        &env,
        "Stop",
        &input(
            "claude-orch",
            json!({"background_tasks": [{"id": "b1"}], "session_crons": [], "last_assistant_message": "[alpha-1] done"}),
        ),
    );
    let (_, _, payload) = newest(&store);
    assert_eq!(payload["waiting_on_own_work"], true);
    assert_eq!(payload["background_tasks"], 1);

    // A wake-up line for an outbox row targeting this session, another
    // session, or no row at all.
    let mine = enqueue(&mut store, &fleet.orchestrator);
    let theirs = enqueue(&mut store, &fleet.worker);
    for (id, want) in [
        (&mine, "wake"),
        (&theirs, "wake_mismatch"),
        (&"01JC2ZB6N0E2K4M6P8R0S2T4V6".to_owned(), "wake_mismatch"),
    ] {
        hook(
            &mut store,
            &env,
            "UserPromptSubmit",
            &input(
                "claude-orch",
                json!({"prompt": format!("tiller inbox {id} # wake")}),
            ),
        );
        let (event, verdict, payload) = newest(&store);
        assert_eq!(event, want);
        assert_eq!(verdict.as_deref(), Some("working"));
        assert_eq!(payload["outbox_id"], json!(id));
        assert!(payload.get("prompt").is_none());
    }
}

fn enqueue(store: &mut Store, target: &str) -> String {
    let id = tiller_core::ids::new_ulid();
    store
        .conn()
        .execute(
            "INSERT INTO outbox (id, target_session_id, kind, body, created_at, created_by)
             VALUES (?1, ?2, 'wake', 'b', '2026-11-02T10:00:00.000Z', 'daemon')",
            [&id, target],
        )
        .unwrap();
    id
}

#[test]
fn instructions_loaded_records_relative_path_and_hash() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("alpha");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("AGENTS.md"), "be kind\n").unwrap();
    let env = Env {
        tiller_home: dir.path().join(".tiller"),
        user_home: Some(dir.path().to_owned()),
        tiller_session_id: None,
    };
    let input = serde_json::to_vec(&json!({
        "session_id": "c",
        "cwd": project,
        "file_path": project.join("AGENTS.md"),
        "memory_type": "Project",
    }))
    .unwrap();
    let observed = observe("InstructionsLoaded", &input, &env, Timestamp::now())
        .unwrap()
        .unwrap();
    assert_eq!(observed.payload["file_path"], "AGENTS.md");
    let hash = observed.payload["sha256"].as_str().unwrap();
    assert_eq!(hash, super::hex(&Sha256::digest(b"be kind\n")));

    let home_file = dir.path().join(".claude/CLAUDE.md");
    let shown = super::instructions_file(
        home_file.to_str().unwrap(),
        Some(project.to_str().unwrap()),
        &env,
    );
    assert_eq!(shown, (String::from("~/.claude/CLAUDE.md"), None));
}

/// `recovery.md` R11: a hook that finds a schema it cannot write spools the
/// observation and exits 0; the daemon's next pass imports it once.
#[test]
fn hook_spools_on_unwritable_schema_and_daemon_imports_spool() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".tiller");
    fs::create_dir_all(&home).unwrap();
    let (store, fleet) = file_store(&home);
    drop(store);
    let env = Env {
        tiller_home: home.clone(),
        user_home: None,
        tiller_session_id: Some(fleet.worker.clone()),
    };

    // A newer binary migrated the database past this one's schema.
    let conn = rusqlite::Connection::open(home.join("state.db")).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {}", version + 1))
        .unwrap();

    let failure = input(
        "claude-worker",
        json!({"error": "overloaded", "error_details": "secret"}),
    );
    let Outcome::Spooled(path) = run("StopFailure", &failure, &env) else {
        panic!("expected a spool");
    };
    let spooled = fs::read_to_string(&path).unwrap();
    assert!(
        !spooled.contains("secret"),
        "the spool holds only allowlisted fields"
    );
    assert!(
        fs::read_to_string(home.join("log/hook.log"))
            .unwrap()
            .contains("spooled StopFailure")
    );

    // The daemon migrates (here: the version goes back) and imports.
    conn.execute_batch(&format!("PRAGMA user_version = {version}"))
        .unwrap();
    drop(conn);
    let mut store = Store::open(home.join("state.db")).unwrap();
    assert_eq!(import_spool(&mut store, &env).unwrap(), 1);
    assert!(!path.exists());
    assert_eq!(session(&store, &fleet.worker).activity, Activity::Errored);
    assert_eq!(import_spool(&mut store, &env).unwrap(), 0);
    assert_eq!(count(&store, "SELECT count(*) FROM observations"), 1);
}

#[test]
fn run_writes_straight_to_state_db_and_respects_the_owner_marker() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".tiller");
    fs::create_dir_all(&home).unwrap();
    let (store, fleet) = file_store(&home);
    drop(store);
    let env = Env {
        tiller_home: home.clone(),
        user_home: None,
        tiller_session_id: Some(fleet.orchestrator.clone()),
    };
    let stop = input("claude-orch", json!({}));

    fs::write(home.join("owner"), "firstmate\n").unwrap();
    assert_eq!(run("Stop", &stop, &env), Outcome::NotOwner);
    assert!(
        !home.join("hook-spool").exists(),
        "nothing spooled for another system"
    );

    fs::write(home.join("owner"), "tiller\n").unwrap();
    assert!(matches!(run("Stop", &stop, &env), Outcome::Recorded { .. }));
    assert_eq!(run("Setup", b"", &env), Outcome::Ignored);
    assert!(matches!(run("Stop", b"garbage", &env), Outcome::Failed(_)));
    let store = Store::open(home.join("state.db")).unwrap();
    assert_eq!(
        session(&store, &fleet.orchestrator).activity,
        Activity::Idle
    );
}

#[test]
fn run_without_a_database_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env {
        tiller_home: dir.path().to_owned(),
        user_home: None,
        tiller_session_id: None,
    };
    assert_eq!(
        run("Stop", &input("c", json!({})), &env),
        Outcome::NoDatabase
    );
    assert!(
        !dir.path().join("state.db").exists(),
        "a hook never creates state.db"
    );
    assert!(
        !dir.path().join("hook-spool").exists(),
        "nothing to replay later"
    );
}

/// Claude Code sends byte-identical input for every permission prompt of a
/// session. Each real prompt is recorded; only a redelivery of the same
/// moment is a replay.
#[test]
fn identical_input_later_in_the_session_is_a_new_event() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.worker));
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("t.jsonl");
    fs::write(&transcript, "{}\n").unwrap();
    let prompt = serde_json::to_vec(&json!({
        "session_id": "claude-worker",
        "transcript_path": transcript,
        "notification_type": "permission_prompt",
        "message": "Claude needs your permission to use Bash",
    }))
    .unwrap();
    let approved = input(
        "claude-worker",
        json!({"tool_name": "Bash", "tool_use_id": "t1"}),
    );

    assert!(matches!(
        hook(&mut store, &env, "Notification", &prompt),
        Outcome::Recorded { .. }
    ));
    assert_eq!(
        hook(&mut store, &env, "Notification", &prompt),
        Outcome::Replayed
    );
    hook(&mut store, &env, "PostToolUse", &approved);
    fs::write(&transcript, "{}\n{\"type\":\"assistant\"}\n").unwrap();
    assert!(matches!(
        hook(&mut store, &env, "Notification", &prompt),
        Outcome::Recorded { .. }
    ));
    assert_eq!(
        session(&store, &fleet.worker).activity,
        Activity::WaitingPermission
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM observations WHERE event = 'Notification:permission_prompt'"
        ),
        2
    );
}

/// A SessionEnd whose observation committed but whose end did not (a crash
/// between the two writes) still ends the session when it is redelivered.
#[test]
fn redelivered_session_end_finishes_the_end() {
    let mut store = Store::open_in_memory().unwrap();
    let fleet = fleet(&mut store);
    let env = env(Some(&fleet.orchestrator));
    let end = observe(
        "SessionEnd",
        &input("claude-orch", json!({"reason": "other"})),
        &env,
        tick(),
    )
    .unwrap()
    .unwrap();
    // Only the first write of the first delivery happened.
    store
        .observation_record_keyed(
            &tiller_core::Ctx::new(tiller_core::Actor::Hook),
            Some(&end.key),
            tiller_core::state::runs::NewObservation {
                session_id: Some(fleet.orchestrator.clone()),
                run_id: None,
                source: ObservationSource::Hook,
                event: end.event.clone(),
                verdict: end.verdict,
                observed_at: end.observed_at,
                payload: end.payload.clone(),
            },
        )
        .unwrap();
    assert!(session(&store, &fleet.orchestrator).ended_at.is_none());

    assert_eq!(apply(&mut store, &env, &end).unwrap(), Outcome::Replayed);
    let ended = session(&store, &fleet.orchestrator);
    assert_eq!(ended.end_reason.as_deref(), Some("other"));
    assert_eq!(count(&store, "SELECT count(*) FROM observations"), 1);
}

/// One spooled observation that cannot apply is moved aside; the rest import.
#[test]
fn a_spool_file_that_fails_to_apply_does_not_block_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join(".tiller");
    fs::create_dir_all(home.join("hook-spool")).unwrap();
    let (mut store, fleet) = file_store(&home);
    let env = Env {
        tiller_home: home.clone(),
        user_home: None,
        tiller_session_id: Some(fleet.worker.clone()),
    };
    let mut bad = observe("Stop", &input("claude-worker", json!({})), &env, tick())
        .unwrap()
        .unwrap();
    bad.event = String::new();
    let good = observe(
        "StopFailure",
        &input("claude-worker", json!({"error": "overloaded"})),
        &env,
        tick(),
    )
    .unwrap()
    .unwrap();
    fs::write(
        home.join("hook-spool/01A.json"),
        serde_json::to_vec(&bad).unwrap(),
    )
    .unwrap();
    fs::write(
        home.join("hook-spool/01B.json"),
        serde_json::to_vec(&good).unwrap(),
    )
    .unwrap();

    assert_eq!(import_spool(&mut store, &env).unwrap(), 1);
    assert!(home.join("hook-spool/01A.json.bad").exists());
    assert!(!home.join("hook-spool/01B.json").exists());
    assert_eq!(session(&store, &fleet.worker).activity, Activity::Errored);
}
