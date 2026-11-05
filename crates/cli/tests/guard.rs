//! `tiller guard pretool` end to end: the real binary, the hook payload on
//! stdin, and the transport Claude Code reads (`authority.md` section 9): a
//! deny exits 2 with the decision JSON on stderr and nothing on stdout, an
//! allow exits 0 and prints nothing, and an allowed bare `tiller ...` exits 0
//! with the call rewritten to the packaged path on stdout.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use tiller_core::Store;

fn guard(home: &Path, role: &str, command: &str) -> Output {
    guard_with(
        home,
        &[
            "guard",
            "pretool",
            "--role",
            role,
            "--checkouts",
            "/home/user/checkouts",
            "--pool",
            "/home/user/pool",
        ],
        command,
    )
}

fn guard_with(home: &Path, args: &[&str], command: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tiller"))
        .args(args)
        .env("TILLER_HOME", home)
        .env("HOME", "/home/user")
        .env_remove("TILLER_SESSION_ID")
        .env_remove("TILLER_RUN_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = json!({
        "session_id": "claude-1",
        "cwd": "/home/user/tiller-homes/alpha",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    // A guard that decides before it reads the payload may exit first, which
    // closes the pipe under this write.
    let _ = child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes());
    child.wait_with_output().unwrap()
}

#[track_caller]
fn assert_denied(output: &Output, code: &str) -> Value {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let decision: Value = serde_json::from_slice(&output.stderr).unwrap();
    let reason = decision["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.starts_with(&format!("[{code}] ")), "{reason}");
    assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
    decision
}

#[track_caller]
fn assert_allowed(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[track_caller]
fn assert_rewritten(output: &Output, command: &str) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    let packaged = env!("CARGO_BIN_EXE_tiller");
    assert_eq!(
        answer,
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {"command": format!("{packaged}{command}")},
            },
        })
    );
}

#[test]
fn deny_exits_two_with_the_decision_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    assert_denied(&guard(home.path(), "builder", "gh pr merge 12"), "merge");
    assert_denied(
        &guard(home.path(), "orchestrator", "cd /home/user/checkouts/alpha"),
        "persistent-cd",
    );
    // No `state.db` under this home: the deny still holds, it is only not
    // recorded, and the guard never creates one.
    assert!(!home.path().join("state.db").exists());
}

#[test]
fn allow_exits_zero_and_prints_nothing() {
    let home = tempfile::tempdir().unwrap();
    assert_allowed(&guard(home.path(), "builder", "cargo nextest run"));
    assert_allowed(&guard(
        home.path(),
        "orchestrator",
        "git -C /home/user/checkouts/alpha status",
    ));
    // An unknown role is a broken fragment: allowed, like any broken hook.
    assert_allowed(&guard(home.path(), "pirate", "gh pr merge 12"));
}

/// The sandbox exempts only the packaged path (`authority.md` section 7,
/// "Routes and their controls"), so an allowed lone bare `tiller` is handed
/// back naming it, each argument quoted; anything around it is left alone
/// and runs sandboxed.
#[test]
fn bare_tiller_is_rewritten_to_the_running_binary() {
    let home = tempfile::tempdir().unwrap();
    for role in ["orchestrator", "builder", "scout", "reviewer", "verifier"] {
        assert_rewritten(
            &guard(home.path(), role, "tiller handoff show"),
            " 'handoff' 'show'",
        );
        assert_rewritten(&guard(home.path(), role, "tiller"), "");
        for command in [
            "cd /home/user && tiller handoff show",
            "tiller handoff show | cat",
            "tiller handoff show > /home/user/out",
            "FOO=1 tiller handoff show",
        ] {
            assert_allowed(&guard(home.path(), role, command));
        }
    }
    // A denied bare call is denied, never rewritten.
    assert_denied(
        &guard(home.path(), "builder", "tiller grant 01JABC"),
        "owner-only",
    );
}

/// Exit 2 would deny every tool call of the session, so a command line the
/// guard cannot parse allows, silently, like an unknown role.
#[test]
fn unparsable_guard_arguments_allow() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        // An unquoted root with a space splits into a stray argument.
        &[
            "guard",
            "pretool",
            "--role",
            "orchestrator",
            "--checkouts",
            "/home/user/my",
            "checkouts",
        ][..],
        &["guard", "pretool"],
        &["guard", "pretool", "--role", "builder", "--bogus"],
        &["hook", "PreToolUse", "--bogus"],
    ] {
        assert_allowed(&guard_with(home.path(), args, "gh pr merge 12"));
    }
}

/// The CP2 decision on G06, at the hook layer: the orchestrator may cite an
/// ask, and the bare form is refused before it reaches `tiller` (whose own
/// refusal is `behavior::chat_approval_needs_a_newer_prompt_from_the_calling_orchestrator`).
#[test]
fn orchestrator_bare_approve_is_refused_by_the_hook() {
    let home = tempfile::tempdir().unwrap();
    assert_denied(
        &guard(home.path(), "orchestrator", "tiller approve 01JABC"),
        "owner-only",
    );
    assert_rewritten(
        &guard(
            home.path(),
            "orchestrator",
            "tiller approve 01JABC --ask 01JASK",
        ),
        " 'approve' '01JABC' '--ask' '01JASK'",
    );
    let packaged = env!("CARGO_BIN_EXE_tiller");
    assert_denied(
        &guard(
            home.path(),
            "orchestrator",
            &format!("{packaged} approve 01JABC"),
        ),
        "owner-only",
    );
    assert_denied(
        &guard(home.path(), "builder", "tiller approve 01JABC --ask 01JASK"),
        "owner-only",
    );
}

#[test]
fn deny_is_recorded_when_the_home_has_a_database() {
    let home = tempfile::tempdir().unwrap();
    drop(Store::open(home.path().join("state.db")).unwrap());
    assert_denied(
        &guard(home.path(), "scout", "git push origin HEAD"),
        "foreign-branch",
    );
    assert_allowed(&guard(home.path(), "scout", "git status"));
    // A redelivered hook, the same payload again: still one record.
    assert_denied(
        &guard(home.path(), "scout", "git push origin HEAD"),
        "foreign-branch",
    );
    let store = Store::open(home.path().join("state.db")).unwrap();
    let rows: Vec<(String, String)> = store
        .conn()
        .prepare("SELECT source, payload FROM observations WHERE event = 'PreToolUse:denied'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0, "hook");
    let payload: Value = serde_json::from_str(&rows[0].1).unwrap();
    assert_eq!(payload["code"], "foreign-branch");
    assert_eq!(payload["role"], "scout");
    assert_eq!(payload["claude_session_id"], "claude-1");
}

#[test]
fn a_custom_tiller_home_is_protected_like_the_default() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join("state.db");
    assert_denied(
        &guard(home.path(), "builder", &format!("cat {}", db.display())),
        "state-write",
    );
    assert_denied(
        &guard(
            home.path(),
            "builder",
            &format!("sqlite3 {} .dump", db.display()),
        ),
        "state-write",
    );
    assert_allowed(&guard(home.path(), "builder", "git status"));
}
