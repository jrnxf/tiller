//! How a session's end is recorded (`recovery.md` R03).

use rusqlite::Connection;
use tiller_core::domain::{Run, Session};
use tiller_core::state::get;

/// The `end_reason` for a session that ended with `reason`. A worker that
/// exits while its run is still active crashed, however cleanly it exited:
/// only `run.end` ends a run, so the reason is `exited` (R03).
pub fn end_reason(
    conn: &Connection,
    session: &Session,
    reason: &str,
) -> tiller_core::Result<String> {
    if let Some(run) = &session.run_id {
        let run: Run = get(conn, "run", run)?;
        if run.outcome.is_none() {
            return Ok("exited".to_owned());
        }
    }
    Ok(reason.to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tiller_core::Store;
    use tiller_core::domain::Activity;

    use crate::hooks::{Env, Outcome, apply, observe};
    use crate::testing::{fleet, session};

    /// R03: a worker whose Claude Code exits cleanly with its run active is
    /// a crash: the session ends `exited`, and the run stays active.
    #[test]
    fn clean_exit_with_active_run_is_a_crash() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let env = Env {
            tiller_home: "/home/user/.tiller".into(),
            user_home: None,
            tiller_session_id: Some(fleet.worker.clone()),
        };
        let input = json!({"session_id": "claude-worker", "reason": "prompt_input_exit"});
        let observed = observe(
            "SessionEnd",
            input.to_string().as_bytes(),
            &env,
            tiller_core::Timestamp::now(),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            apply(&mut store, &env, &observed).unwrap(),
            Outcome::Recorded { .. }
        ));

        let ended = session(&store, &fleet.worker);
        assert_eq!(ended.end_reason.as_deref(), Some("exited"));
        assert_eq!(ended.activity, Activity::Ended);
        let run: tiller_core::domain::Run =
            tiller_core::state::get(store.conn(), "run", &fleet.run).unwrap();
        assert_eq!(run.outcome, None, "only run.end ends a run");
    }
}
