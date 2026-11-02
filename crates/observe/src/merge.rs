//! Merging herdr's reading with the hooks' (`protocol.md` section 4.5).
//!
//! The projection rule itself (newest `observed_at` wins, except that a
//! `herdr:idle` never replaces a hook `working` less than 30 s old) lives in
//! `observation.record`, so every writer gets it.

use tiller_core::domain::Activity;

/// The verdict for herdr's `agent_status`: `working` stays `working`; `idle`
/// and `done` are `idle`; `blocked` is `waiting_input` unless the newest hook
/// verdict is already `waiting_permission`, which says more. Any other status
/// implies none.
pub fn herdr_verdict(status: &str, newest_hook_verdict: Option<Activity>) -> Option<Activity> {
    match status {
        "working" => Some(Activity::Working),
        "idle" | "done" => Some(Activity::Idle),
        "blocked" if newest_hook_verdict == Some(Activity::WaitingPermission) => {
            Some(Activity::WaitingPermission)
        }
        "blocked" => Some(Activity::WaitingInput),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tiller_core::domain::{Activity, ObservationSource};
    use tiller_core::state::runs::NewObservation;
    use tiller_core::{Actor, Ctx, Store, Timestamp};

    use super::herdr_verdict;
    use crate::testing::{fleet, session};

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_millis(1_793_620_000_000 + seconds * 1000).unwrap()
    }

    fn observation(
        session: &str,
        source: ObservationSource,
        verdict: Activity,
        observed_at: Timestamp,
    ) -> NewObservation {
        NewObservation {
            session_id: Some(session.to_owned()),
            run_id: None,
            source,
            event: format!("{source}:{verdict}"),
            verdict: Some(verdict),
            observed_at,
            payload: json!({}),
        }
    }

    #[test]
    fn herdr_idle_does_not_override_fresh_hook_working() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let s = &fleet.orchestrator;
        let ctx = Ctx::new(Actor::Daemon);
        store
            .observation_record(
                &ctx,
                observation(s, ObservationSource::Hook, Activity::Working, at(0)),
            )
            .unwrap();

        // herdr reads idle 10 s into the turn: newer, but the hook wins.
        let held = store
            .observation_record(
                &ctx,
                observation(s, ObservationSource::Herdr, Activity::Idle, at(10)),
            )
            .unwrap();
        assert!(!held.value.verdict_changed);
        assert_eq!(session(&store, s).activity, Activity::Working);
        assert_eq!(
            session(&store, s).activity_source,
            Some(ObservationSource::Hook)
        );

        // At 30 s the hook's working is no longer fresh: herdr's idle lands.
        let landed = store
            .observation_record(
                &ctx,
                observation(s, ObservationSource::Herdr, Activity::Idle, at(30)),
            )
            .unwrap();
        assert!(landed.value.verdict_changed);
        assert_eq!(session(&store, s).activity, Activity::Idle);
    }

    #[test]
    fn herdr_working_and_hook_idle_follow_the_newest() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let s = &fleet.orchestrator;
        let ctx = Ctx::new(Actor::Daemon);
        store
            .observation_record(
                &ctx,
                observation(s, ObservationSource::Hook, Activity::Working, at(0)),
            )
            .unwrap();
        store
            .observation_record(
                &ctx,
                observation(s, ObservationSource::Hook, Activity::Idle, at(5)),
            )
            .unwrap();
        // The hold is only for herdr idle over hook working.
        store
            .observation_record(
                &ctx,
                observation(s, ObservationSource::Herdr, Activity::Working, at(6)),
            )
            .unwrap();
        assert_eq!(session(&store, s).activity, Activity::Working);
    }

    #[test]
    fn herdr_status_maps_to_one_verdict() {
        assert_eq!(herdr_verdict("working", None), Some(Activity::Working));
        assert_eq!(herdr_verdict("idle", None), Some(Activity::Idle));
        assert_eq!(herdr_verdict("done", None), Some(Activity::Idle));
        assert_eq!(herdr_verdict("blocked", None), Some(Activity::WaitingInput));
        assert_eq!(
            herdr_verdict("blocked", Some(Activity::WaitingPermission)),
            Some(Activity::WaitingPermission)
        );
        assert_eq!(herdr_verdict("rebooting", None), None);
    }
}
