//! The timing values of `recovery.md` section 2, as the owner accepted them
//! at CP2 (C10, C26). They are starting values: the config file may tune
//! them, and T7.4 reports how often each escalation fired.

use serde::{Deserialize, Serialize};

/// Every "after N minutes" the recovery table names, in minutes unless the
/// field says otherwise. The `session_activity` view carries the first two.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thresholds {
    /// A worker with no observation this long reads `stale`.
    pub worker_stale: i64,
    /// An orchestrator with no observation this long reads `stale`.
    pub orchestrator_stale: i64,
    /// A silent worker (alive, stale this long) gets a note to its
    /// orchestrator (R12): twice the worker threshold.
    pub silent_note: i64,
    /// A silent worker with no orchestrator action this long after the note
    /// goes to the developer (R12).
    pub silent_developer: i64,
    /// An `errored` session that stays so this long with its agent alive is
    /// treated as silent (R12, C07).
    pub errored_silent: i64,
    /// A silent orchestrator with work in flight goes to the developer after
    /// this long (R12).
    pub orchestrator_silent_developer: i64,
    /// A crashed worker with no orchestrator action this long after
    /// `session.ended` goes to the developer (R03).
    pub worker_crash_developer: i64,
    /// A note to the orchestrator about an orphan unanswered this long goes
    /// to the developer (R15).
    pub orphan_note_developer: i64,
    /// A session that no `SessionStart` binds this long after a resume
    /// attempt counts as a failed attempt (section 2, "Resuming a session").
    pub bind_window: i64,
    /// A breached floor still breached this long after `host.degraded` goes
    /// to the developer (R16).
    pub floor_breach_developer: i64,
    /// A tool version skew this old goes to the developer (R01).
    pub version_skew_developer: i64,
    /// An `uncertain` merge execution unreadable this long after
    /// `executed_at` goes to the developer (R08).
    pub uncertain_merge: i64,
    /// Window for "twice means the developer" (principle 4), in hours.
    pub repeat_window_hours: i64,
    /// A worktree unreturned this many days after its run ended goes to the
    /// developer (R15).
    pub unreturned_days: i64,
    /// Orchestrator crashes in one hour before the automatic resume stops
    /// (R02): the third crash goes to the developer.
    pub orchestrator_crashes_per_hour: i64,
    /// Automatic orchestrator resumes allowed per project per hour (R02).
    pub resumes_per_project_hour: i64,
    /// Seconds between launches on one host (principle 5).
    pub launch_gap_seconds: i64,
    /// Minutes between host checks (section 3, step 2).
    pub host_check: i64,
    /// Minutes between full worktree scans (section 2, Sources).
    pub worktree_scan: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            worker_stale: 15,
            orchestrator_stale: 30,
            silent_note: 30,
            silent_developer: 60,
            errored_silent: 30,
            orchestrator_silent_developer: 60,
            worker_crash_developer: 30,
            orphan_note_developer: 30,
            bind_window: 2,
            floor_breach_developer: 30,
            version_skew_developer: 60,
            uncertain_merge: 10,
            repeat_window_hours: 24,
            unreturned_days: 7,
            orchestrator_crashes_per_hour: 3,
            resumes_per_project_hour: 2,
            launch_gap_seconds: 10,
            host_check: 5,
            worktree_scan: 5,
        }
    }
}

impl Thresholds {
    /// The staleness threshold for a session of `role`, in minutes.
    pub fn stale_minutes(&self, role: tiller_core::domain::Role) -> i64 {
        match role {
            tiller_core::domain::Role::Orchestrator => self.orchestrator_stale,
            _ => self.worker_stale,
        }
    }
}
