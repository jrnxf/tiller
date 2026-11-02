//! `tiller hook` end to end: the real binary, a real `state.db`, no daemon.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tiller_core::domain::{Activity, Boundary, Delivery, Session};
use tiller_core::state::projects::{Init, NewProject};
use tiller_core::state::runs::NewOrchestrator;
use tiller_core::{Actor, Ctx, Store};

/// A home with an initialized `state.db` and a live orchestrator; returns
/// its session id.
#[cfg(test)]
fn home_with_orchestrator(home: &Path) -> String {
    let mut store = Store::open(home.join("state.db")).unwrap();
    let owner = Ctx::new(Actor::Owner);
    store
        .init(
            &owner,
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
            &owner,
            NewProject {
                id: "alpha".into(),
                name: "Alpha".into(),
                repo: None,
                remote: None,
                default_branch: None,
                delivery: Delivery::DirectPr,
                checks: None,
                capacity: None,
                deploy_workflow: None,
                deploy_environment: None,
                deploy_timeout_minutes: None,
            },
        )
        .unwrap();
    store
        .session_start(
            &owner,
            NewOrchestrator {
                project_id: "alpha".into(),
                claude_session_id: Some("claude-orch".into()),
                ..NewOrchestrator::default()
            },
        )
        .unwrap()
        .value
        .session
        .id
}

#[cfg(test)]
fn hook(home: &Path, event: &str, input: &str) -> (Duration, std::process::Output) {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tiller"))
        .args(["hook", event])
        .env("TILLER_HOME", home)
        .env_remove("TILLER_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (started.elapsed(), output)
}

/// T2.6 acceptance: a hook call finishes in under 10 ms at p95 with no
/// daemon running (no daemon is started here; the hook never needs one). The
/// time includes process start, opening `state.db` and the commit.
#[test]
fn hook_p95_is_under_ten_ms_with_the_daemon_down() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = home_with_orchestrator(dir.path());
    let mut times = Vec::new();
    for n in 0..60 {
        // Alternate events so every call writes a row (no replay, no coalescing).
        let (event, input) = if n % 2 == 0 {
            (
                "UserPromptSubmit",
                format!(r#"{{"session_id":"claude-orch","prompt":"p{n}"}}"#),
            )
        } else {
            ("Stop", format!(r#"{{"session_id":"claude-orch","n":{n}}}"#))
        };
        let (took, output) = hook(dir.path(), event, &input);
        assert!(output.status.success(), "{output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
        times.push(took);
    }
    // The first calls warm the page cache; measure the steady state.
    let mut steady = times.split_off(10);
    steady.sort();
    let p95 = steady[steady.len() * 95 / 100 - 1];
    eprintln!("tiller hook p95 {p95:?} over {} calls", steady.len());
    assert!(p95 < Duration::from_millis(10), "p95 {p95:?}: {steady:?}");

    let store = Store::open(dir.path().join("state.db")).unwrap();
    let rows: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM observations WHERE session_id = ?1",
            [&orchestrator],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 60);
    let session: Session = tiller_core::state::get(store.conn(), "session", &orchestrator).unwrap();
    assert_eq!(session.activity, Activity::Idle);
}

/// A hook never fails the session: bad input, a missing database and an
/// unknown event all exit 0 with no output.
#[test]
fn hook_exits_zero_whatever_happens() {
    let dir = tempfile::tempdir().unwrap();
    for (event, input) in [("Stop", "not json"), ("Stop", "{}"), ("FutureEvent", "{}")] {
        let (_, output) = hook(dir.path(), event, input);
        assert!(output.status.success(), "{event} {input}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    assert!(
        !dir.path().join("state.db").exists(),
        "a hook never creates state.db"
    );
    assert!(dir.path().join("log/hook.log").exists());
}
