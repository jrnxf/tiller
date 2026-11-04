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
fn hook(home: &Path, event: &str, input: &str) -> (Duration, Duration, std::process::Output) {
    let cpu_before = children_cpu();
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
    (started.elapsed(), children_cpu() - cpu_before, output)
}

/// CPU time (user plus system) of every child this process has reaped. The
/// difference across one call is that call's own work: unlike wall time it
/// does not grow when other processes compete for the cores.
#[cfg(test)]
fn children_cpu() -> Duration {
    use nix::sys::resource::{UsageWho, getrusage};
    let usage = getrusage(UsageWho::RUSAGE_CHILDREN).unwrap();
    let timeval =
        |t: nix::sys::time::TimeVal| Duration::new(t.tv_sec() as u64, t.tv_usec() as u32 * 1000);
    timeval(usage.user_time()) + timeval(usage.system_time())
}

/// The T2.6 budget is 10 ms for a hook call with no daemon. This is that
/// budget for the debug binary this test runs, about 1.5x slower than the
/// release binary the budget is for.
const DEBUG_BUDGET: Duration = Duration::from_millis(15);
/// A wall-clock ceiling that only a wait would reach: a hook blocked on a
/// daemon that is down waits far longer, while load adds a few milliseconds.
const WALL_CEILING: Duration = Duration::from_millis(50);
/// Unmeasured calls that warm the page cache first.
const WARM_UP: usize = 10;
/// Measured calls. Each call adds about 80 KB of WAL, so SQLite's automatic
/// checkpoint (at 1000 pages) stalls two or three calls in every 45 or so on
/// the fsync. 200 calls span four checkpoints, so the figures below do not
/// depend on where one checkpoint lands in the window.
const MEASURED: usize = 200;

/// T2.6 acceptance: a hook call finishes in under 10 ms with no daemon
/// running (no daemon is started here; the hook never needs one). The time
/// includes process start, opening `state.db` and the commit.
///
/// The test runs the debug binary, about 1.5x slower than the release binary
/// the budget is for (`DEBUG_BUDGET`), and wall time moves with whatever else the machine is
/// doing (a cold build beside it took the median from 7 ms to 10 ms), so it
/// cannot gate the 10 ms. The budget is checked against the hook's own CPU
/// time, which does not: the median must stay inside it, which fails if every
/// call gets more work. The lower quartile of wall time must stay inside a
/// ceiling that only a wait reaches, which fails if the hook blocks on a
/// daemon. SQLite checkpoint stalls (about 5% of calls) and the percentiles
/// are printed, not asserted.
#[test]
fn hook_is_inside_the_ten_ms_budget_with_the_daemon_down() {
    let dir = tempfile::tempdir().unwrap();
    let orchestrator = home_with_orchestrator(dir.path());
    let mut times = Vec::new();
    let mut cpus = Vec::new();
    for n in 0..WARM_UP + MEASURED {
        // Alternate events so every call writes a row (no replay, no coalescing).
        let (event, input) = if n % 2 == 0 {
            (
                "UserPromptSubmit",
                format!(r#"{{"session_id":"claude-orch","prompt":"p{n}"}}"#),
            )
        } else {
            ("Stop", format!(r#"{{"session_id":"claude-orch","n":{n}}}"#))
        };
        let (took, cpu, output) = hook(dir.path(), event, &input);
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        if event == "Stop" {
            assert!(output.stdout.is_empty(), "observe mode prints nothing");
        } else {
            // Each prompt is an ask, and the orchestrator is told its id.
            let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let context = printed["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap();
            assert!(context.starts_with("tiller: ask "), "{context}");
        }
        times.push(took);
        cpus.push(cpu);
    }
    let mut steady = times.split_off(WARM_UP);
    let mut cpu = cpus.split_off(WARM_UP);
    steady.sort();
    cpu.sort();
    let at = |sorted: &[Duration], percent: usize| sorted[sorted.len() * percent / 100 - 1];
    let (wall_quartile, wall_median, wall_p95) =
        (at(&steady, 25), at(&steady, 50), at(&steady, 95));
    let (cpu_median, cpu_p95) = (at(&cpu, 50), at(&cpu, 95));
    eprintln!(
        "tiller hook over {MEASURED} calls: wall p25 {wall_quartile:?}, median {wall_median:?}, p95 {wall_p95:?}, max {:?}; cpu median {cpu_median:?}, p95 {cpu_p95:?}",
        steady[MEASURED - 1]
    );
    assert!(
        cpu_median < DEBUG_BUDGET,
        "cpu median {cpu_median:?}: {cpu:?}"
    );
    assert!(
        wall_quartile < WALL_CEILING,
        "wall lower quartile {wall_quartile:?}: {steady:?}"
    );

    let store = Store::open(dir.path().join("state.db")).unwrap();
    let rows: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM observations WHERE session_id = ?1",
            [&orchestrator],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, (WARM_UP + MEASURED) as i64);
    let asks: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM asks WHERE source = 'prompt'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(asks, (WARM_UP + MEASURED) as i64 / 2);
    let session: Session = tiller_core::state::get(store.conn(), "session", &orchestrator).unwrap();
    assert_eq!(session.activity, Activity::Idle);
}

/// A hook never fails the session: bad input, a missing database and an
/// unknown event all exit 0 with no output.
#[test]
fn hook_exits_zero_whatever_happens() {
    let dir = tempfile::tempdir().unwrap();
    for (event, input) in [("Stop", "not json"), ("Stop", "{}"), ("FutureEvent", "{}")] {
        let (_, _, output) = hook(dir.path(), event, input);
        assert!(output.status.success(), "{event} {input}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }
    assert!(
        !dir.path().join("state.db").exists(),
        "a hook never creates state.db"
    );
    assert!(dir.path().join("log/hook.log").exists());
}
