use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tiller_panes::testing::{eventually, gone};

use super::*;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A checkout with an invented holdout split: one case at five runs with
/// one judged grader and one regex grader, one at the default run count
/// with no judge.
fn checkout() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let cases = dir.path().join("evals/cases/holdout");
    write(
        &cases.join("alpha_case/prompt.md"),
        "---\nrole: builder\nruns: 5\n---\nDo the thing.\n",
    );
    write(
        &cases.join("alpha_case/graders/judge.md"),
        "---\ntype: llm\n---\nWas it done?\n",
    );
    write(
        &cases.join("alpha_case/graders/said.md"),
        "---\ntype: regex\npattern: done\n---\n",
    );
    write(
        &cases.join("beta_case/prompt.md"),
        "---\nrole: reviewer\n---\nReview it.\n",
    );
    write(
        &cases.join("beta_case/graders/used.md"),
        "---\ntype: tool_used\n---\n",
    );
    // Not a case: no prompt.md.
    std::fs::create_dir_all(cases.join("notes")).unwrap();
    dir
}

#[test]
fn the_plan_counts_case_runs_and_judge_runs() {
    let dir = checkout();
    let plan = plan(dir.path()).unwrap();
    assert_eq!(
        plan,
        Plan {
            cases: 2,
            case_runs: 5 + DEFAULT_RUNS,
            judge_runs: 5,
        }
    );
    assert_eq!(plan.model_runs(), 13);
}

#[test]
fn a_checkout_without_holdout_cases_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        plan(dir.path()),
        Err(HoldoutError::Unavailable(_))
    ));
    std::fs::create_dir_all(dir.path().join("evals/cases/holdout")).unwrap();
    assert!(matches!(
        plan(dir.path()),
        Err(HoldoutError::Unavailable(_))
    ));
}

#[test]
fn tillers_own_documents_are_read_from_its_home() {
    let home = Path::new("/home/user/.tiller-test");
    assert_eq!(repo_dir(MEMORY_REPO, home), home.join("memory"));
    assert_eq!(
        repo_dir("/home/user/project", home),
        PathBuf::from("/home/user/project")
    );
}

fn runner(dir: &Path, body: &str) {
    let path = dir.join("evals/run.sh");
    write(&path, &format!("#!/bin/sh\n{body}\n"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_clean_run_is_bound_to_its_members_and_candidate() {
    let dir = checkout();
    write(
        &dir.path().join("evals/pins.env"),
        "# pins\nCLAUDE_CODE_VERSION=\"2.0.0\"\nJUDGE_MODEL=judge-1\n",
    );
    // The fake runner checks its arguments and writes a sealed result.
    runner(
        dir.path(),
        r#"[ "$1 $2 $3" = "--split holdout --candidate" ] || exit 9
grep -q 'the candidate' "$4" || exit 8
mkdir -p "$6/sealed"
printf '{"aggregates":{"casesTotal":2,"casesPassed":2,"overallScore":0.9},"costUsd":1.5}' > "$6/sealed/result.json""#,
    );
    let out = tempfile::tempdir().unwrap();
    // A stale sealed result from an earlier run is cleared first.
    write(&out.path().join("sealed/old.json"), "{}");
    let result = run(
        dir.path(),
        out.path(),
        "the candidate\n",
        "m1".into(),
        &idle(),
    )
    .unwrap();
    assert_eq!((result.cases_total, result.cases_passed), (2, 2));
    assert_eq!(result.members_sha256.as_deref(), Some("m1"));
    assert_eq!(
        result.candidate_sha256.as_deref(),
        Some(sha256_hex(b"the candidate\n").as_str())
    );
    assert_eq!(result.claude_version.as_deref(), Some("2.0.0"));
    assert_eq!(result.judge_model.as_deref(), Some("judge-1"));
    assert!(!out.path().join("sealed/old.json").exists());
}

/// The runner's output goes to `runner.log` in the run, not to the caller's
/// stdout (where `tiller --json` prints its answer) or its terminal, which
/// a runner outside the foreground group would stop on.
#[test]
fn the_runners_output_goes_to_its_log() {
    let dir = checkout();
    runner(
        dir.path(),
        r#"echo "holdout: 2 of 2 cases"
echo "a note" >&2
mkdir -p "$6/sealed"
printf '{"aggregates":{"casesTotal":2,"casesPassed":2}}' > "$6/sealed/result.json""#,
    );
    let out = tempfile::tempdir().unwrap();
    run(dir.path(), out.path(), "c", "m".into(), &idle()).unwrap();
    let log = std::fs::read_to_string(out.path().join("runner.log")).unwrap();
    assert_eq!(log, "holdout: 2 of 2 cases\na note\n");
}

/// The quote is the last line the terminal would show: after the last
/// carriage return, with colours and other control bytes left out, from a
/// log that need not be UTF-8 and is read only at its end.
#[test]
fn the_quoted_line_is_plain_text() {
    let dir = checkout();
    runner(
        dir.path(),
        r#"head -c 100000 /dev/zero | tr '\0' 'x'
printf '\377 not utf-8\n'
printf 'case 1 of 2\rcase 2 of 2\r\033[31mfailed: no model\033[0m\n'
exit 2"#,
    );
    let out = tempfile::tempdir().unwrap();
    let error = run(dir.path(), out.path(), "c", "m".into(), &idle()).unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with("(runner.log ends: failed: no model)"),
        "{error}"
    );
}

/// A runner that fails says why in its log, and the failure quotes it.
#[test]
fn a_failed_runner_s_last_line_is_in_the_failure() {
    let dir = checkout();
    runner(
        dir.path(),
        "echo starting\necho 'evals/run.sh: the holdout runs whole' >&2\necho\nexit 2",
    );
    let out = tempfile::tempdir().unwrap();
    let error = run(dir.path(), out.path(), "c", "m".into(), &idle()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "evals/run.sh exited with code 2 and left no sealed/result.json; nothing recorded \
         (runner.log ends: evals/run.sh: the holdout runs whole)"
    );
}

#[test]
fn a_failed_run_records_nothing() {
    let dir = checkout();
    runner(
        dir.path(),
        r#"mkdir -p "$6/sealed"
printf '{"aggregates":{"casesTotal":1,"casesPassed":1}}' > "$6/sealed/result.json"
exit 3"#,
    );
    let out = tempfile::tempdir().unwrap();
    let error = run(dir.path(), out.path(), "c", "m".into(), &idle()).unwrap_err();
    let HoldoutError::Unavailable(message) = error else {
        panic!("expected unavailable, got {error:?}");
    };
    assert_eq!(
        message,
        "evals/run.sh exited with code 3 and left sealed/result.json; nothing recorded"
    );

    let missing = tempfile::tempdir().unwrap();
    let error = run(missing.path(), out.path(), "c", "m".into(), &idle()).unwrap_err();
    let HoldoutError::Unavailable(message) = error else {
        panic!("expected unavailable, got {error:?}");
    };
    assert!(
        message.starts_with("cannot run evals/run.sh: "),
        "{message}"
    );
    // Errors never name where the checkout or the run's directory is.
    for place in [missing.path(), out.path()] {
        assert!(!message.contains(place.to_str().unwrap()), "{message}");
    }
}

#[test]
fn a_result_that_does_not_parse_is_named_by_its_place_in_the_run() {
    let dir = checkout();
    runner(
        dir.path(),
        r#"mkdir -p "$6/sealed"
printf 'not json' > "$6/sealed/result.json""#,
    );
    let out = tempfile::tempdir().unwrap();
    let error = run(dir.path(), out.path(), "c", "m".into(), &idle()).unwrap_err();
    let HoldoutError::Unavailable(message) = error else {
        panic!("expected unavailable, got {error:?}");
    };
    assert!(
        message.starts_with("sealed/result.json is not JSON"),
        "{message}"
    );
}

fn idle() -> Interrupt {
    Interrupt::flag(Arc::new(AtomicBool::new(false)))
}

#[test]
fn an_interrupted_run_stops_the_runner_and_what_it_started() {
    let dir = checkout();
    runner(
        dir.path(),
        "sleep 300 &\necho $! > \"$6/grandchild.pid\"\necho $$ > \"$6/runner.pid\"\nwait",
    );
    let out = tempfile::tempdir().unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let raise = Arc::clone(&flag);
    let pids = out.path().to_owned();
    let watcher = std::thread::spawn(move || {
        let read = |name: &str| {
            std::fs::read_to_string(pids.join(name))
                .ok()
                .map(|pid| pid.trim().to_owned())
                .filter(|pid| !pid.is_empty())
        };
        for _ in 0..200 {
            if let (Some(runner), Some(grandchild)) = (read("runner.pid"), read("grandchild.pid")) {
                raise.store(true, Ordering::SeqCst);
                return Some((runner, grandchild));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        raise.store(true, Ordering::SeqCst);
        None
    });
    let error = run(
        dir.path(),
        out.path(),
        "c",
        "m".into(),
        &Interrupt::flag(flag),
    )
    .unwrap_err();
    let (runner, grandchild) = watcher.join().unwrap().expect("the runner never started");
    // The stop reaps the runner; the rest of its group goes a moment later.
    let stopped = eventually(|| gone(&runner) && gone(&grandchild));
    // Whatever happened, take every process this test started down with it.
    for pid in [&runner, &grandchild] {
        let _ = Command::new("kill").args(["-KILL", pid]).status();
    }
    assert!(stopped, "the runner or its child outlived the interrupt");
    assert!(
        matches!(&error, HoldoutError::Unavailable(m) if m == "evals/run.sh was interrupted; nothing recorded"),
        "{error:?}"
    );
}
