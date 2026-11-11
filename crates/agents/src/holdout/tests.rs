use std::os::unix::fs::PermissionsExt;

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
    let result = run(dir.path(), out.path(), "the candidate\n", "m1".into()).unwrap();
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
    let error = run(dir.path(), out.path(), "c", "m".into()).unwrap_err();
    let HoldoutError::Unavailable(message) = error else {
        panic!("expected unavailable, got {error:?}");
    };
    assert!(message.contains("its result is at"), "{message}");

    let missing = tempfile::tempdir().unwrap();
    let error = run(missing.path(), out.path(), "c", "m".into()).unwrap_err();
    assert!(matches!(error, HoldoutError::Unavailable(m) if m.contains("cannot run")));
}
