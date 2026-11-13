//! The liveness ladder (`recovery.md` section 2). Reconcile never acts on
//! one source: a session that reads `stale`, or whose pane left its
//! holder's list, climbs hooks, the holder and the agent process before it
//! is called anything but `unknown`.

use rusqlite::{Connection, OptionalExtension};
use tiller_core::Timestamp;
use tiller_core::domain::{Activity, Session};
use tiller_herdr::client::{PaneInfo, Snapshot};
use tiller_panes::message::{Listing, PaneState as HolderPaneState};

use crate::sources::{AgentProbe, Probe};
use crate::thresholds::Thresholds;

/// Why a session is dead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Death {
    /// A successful snapshot lacks the pane, or the pane names another
    /// session.
    PaneGone,
    /// The pane is there but no agent process runs in it.
    AgentDead,
}

impl Death {
    /// The `liveness` observation event that records it.
    pub fn event(self) -> &'static str {
        match self {
            Death::PaneGone => "pane_gone",
            Death::AgentDead => "agent_dead",
        }
    }
}

/// What the ladder reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// An observation newer than the threshold, and the pane is where the
    /// records say.
    Alive,
    /// No observation past the threshold, but the agent process is alive
    /// (R12).
    Silent,
    /// Confirmed by a successful snapshot (R02, R03).
    Dead(Death),
    /// A source failed; nothing happens until the next pass.
    Unknown,
}

/// The session's effective activity at `now`, the way the `session_activity`
/// view derives it, with the configured thresholds in place of the view's
/// defaults and the pass's clock in place of the database's: `ended`,
/// `unknown` with no observation, a prompt is never stale, a dated pause
/// silences staleness until 15 minutes after it, else `stale` once no
/// observation is newer than the role's threshold.
pub fn effective_activity(
    conn: &Connection,
    session: &Session,
    thresholds: &Thresholds,
    now: Timestamp,
) -> tiller_core::Result<String> {
    if session.ended_at.is_some() {
        return Ok("ended".into());
    }
    let Some(observed) = session.activity_observed_at else {
        return Ok("unknown".into());
    };
    if matches!(
        session.activity,
        Activity::WaitingPermission | Activity::WaitingInput
    ) {
        return Ok(session.activity.to_string());
    }
    if let Some(run) = &session.run_id {
        let pause_until: Option<Timestamp> = conn
            .query_row("SELECT pause_until FROM runs WHERE id = ?1", [run], |row| {
                row.get(0)
            })
            .optional()?
            .flatten();
        if pause_until.is_some_and(|until| until.plus_minutes(15) > now) {
            return Ok(session.activity.to_string());
        }
    }
    if observed.plus_minutes(thresholds.stale_minutes(session.role)) < now {
        return Ok("stale".into());
    }
    Ok(session.activity.to_string())
}

/// Whether the session has stayed `errored` for the silent threshold with no
/// newer observation (R12, decided at CP2, C07).
pub fn errored_long(session: &Session, thresholds: &Thresholds, now: Timestamp) -> bool {
    session.activity == Activity::Errored
        && session
            .activity_observed_at
            .is_some_and(|at| at.plus_minutes(thresholds.errored_silent) <= now)
}

/// The pane the records bind the session to, when the snapshot has it and
/// it does not name another session. A pane with no `session` token is
/// trusted: R18 re-reports tokens after a herdr restart.
pub fn bound_pane<'a>(snapshot: &'a Snapshot, session: &Session) -> Option<&'a PaneInfo> {
    let pane_id = session.pane_id.as_deref()?;
    let pane = snapshot.pane(pane_id)?;
    match pane.tokens.get("session") {
        Some(named) if *named != session.id => None,
        _ => Some(pane),
    }
}

/// The terminal holder's side of one session's ladder: this pass's
/// successful `list` of the recorded instance, and the pid the `panes`
/// cache last recorded for the session's pane.
#[derive(Clone, Copy, Debug)]
pub struct HolderReading<'a> {
    pub listing: &'a Listing,
    pub recorded_pid: Option<u32>,
}

/// Climbs the ladder for one live session.
///
/// `effective` is the view's word; `snapshot` is this pass's successful herdr
/// read and `holder` its successful terminal holder `list`, each `None` when
/// it failed. A session that is not stale and whose pane is present is alive
/// without any probe. A session with no pane yet (a spawn that has not
/// bound) is unknown: the spawn path owns it.
pub fn climb(
    session: &Session,
    effective: &str,
    errored_long: bool,
    snapshot: Option<&Snapshot>,
    holder: Option<HolderReading<'_>>,
    probe: &dyn AgentProbe,
) -> Verdict {
    let Some(pane_id) = session.pane_id.as_deref() else {
        return Verdict::Unknown;
    };
    let stale = effective == "stale" || errored_long;
    if tiller_core::ids::is_holder_pane(pane_id) {
        return match holder {
            Some(holder) => climb_holder(session, pane_id, stale, holder, probe),
            None => Verdict::Unknown,
        };
    }
    let Some(snapshot) = snapshot else {
        return Verdict::Unknown;
    };
    let pane = bound_pane(snapshot, session);
    match pane {
        None => Verdict::Dead(Death::PaneGone),
        Some(_) if !stale => Verdict::Alive,
        Some(pane) => match probe.agent(pane) {
            Probe::Alive => Verdict::Silent,
            Probe::Dead => Verdict::Dead(Death::AgentDead),
            Probe::Unknown => Verdict::Unknown,
        },
    }
}

/// The ladder for a pane in the terminal holder (`recovery.md` section 2
/// and R18): step 2 is the holder's `list` and the pane's key, step 3 the
/// pane's state, then the child's pid when the session reads stale. A pane
/// the list lacks, or that names another session, is gone once the
/// recorded pid runs no process (`kill(pid, 0)`).
fn climb_holder(
    session: &Session,
    pane_id: &str,
    stale: bool,
    holder: HolderReading<'_>,
    probe: &dyn AgentProbe,
) -> Verdict {
    let pane = holder
        .listing
        .panes
        .iter()
        .find(|p| p.pane.as_str() == pane_id && p.key == session.id);
    let (pid, gone) = match pane {
        Some(pane) if pane.state == HolderPaneState::Exited => {
            return Verdict::Dead(Death::AgentDead);
        }
        Some(pane) => (Some(pane.pid), false),
        None => (holder.recorded_pid, true),
    };
    if !stale && !gone {
        return Verdict::Alive;
    }
    let Some(pid) = pid else {
        return Verdict::Dead(Death::PaneGone);
    };
    let dead = if gone {
        Death::PaneGone
    } else {
        Death::AgentDead
    };
    match probe.process(pid) {
        Probe::Alive if stale => Verdict::Silent,
        Probe::Alive => Verdict::Alive,
        Probe::Dead => Verdict::Dead(dead),
        Probe::Unknown => Verdict::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tiller_core::domain::{ObservationSource, Role};

    use super::*;

    struct Fixed(Probe);

    impl AgentProbe for Fixed {
        fn agent(&self, _pane: &PaneInfo) -> Probe {
            self.0
        }

        fn process(&self, _pid: u32) -> Probe {
            self.0
        }
    }

    fn session(pane: Option<&str>) -> Session {
        Session {
            id: "S1".into(),
            claude_session_id: Some("c1".into()),
            role: Role::Builder,
            project_id: "alpha".into(),
            run_id: Some("R1".into()),
            host_id: "H".into(),
            pane_id: pane.map(str::to_owned),
            transcript_path: None,
            harness: "claude".into(),
            config_hash: None,
            tiller_version: None,
            launch_hash: None,
            restart_queued_at: None,
            started_at: Timestamp::now(),
            ended_at: None,
            end_reason: None,
            activity: Activity::Working,
            activity_observed_at: Some(Timestamp::now()),
            activity_source: Some(ObservationSource::Hook),
            rev: 1,
        }
    }

    fn snapshot(panes: &[(&str, Option<&str>)]) -> Snapshot {
        Snapshot {
            version: None,
            workspaces: Vec::new(),
            panes: panes
                .iter()
                .map(|(id, named)| PaneInfo {
                    pane_id: (*id).to_owned(),
                    workspace_id: "w".into(),
                    agent: Some("claude".into()),
                    agent_status: Some("idle".into()),
                    tokens: named
                        .map(|s| BTreeMap::from([("session".to_owned(), s.to_owned())]))
                        .unwrap_or_default(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_failed_snapshot_is_unknown_whatever_the_records_say() {
        let s = session(Some("p1"));
        assert_eq!(
            climb(&s, "stale", false, None, None, &Fixed(Probe::Dead)),
            Verdict::Unknown
        );
    }

    #[test]
    fn a_missing_pane_is_dead_even_when_not_stale() {
        let s = session(Some("p1"));
        let snap = snapshot(&[("p2", None)]);
        assert_eq!(
            climb(
                &s,
                "working",
                false,
                Some(&snap),
                None,
                &Fixed(Probe::Alive)
            ),
            Verdict::Dead(Death::PaneGone)
        );
    }

    #[test]
    fn a_pane_naming_another_session_counts_as_gone() {
        let s = session(Some("p1"));
        let snap = snapshot(&[("p1", Some("S9"))]);
        assert_eq!(
            climb(
                &s,
                "working",
                false,
                Some(&snap),
                None,
                &Fixed(Probe::Alive)
            ),
            Verdict::Dead(Death::PaneGone)
        );
    }

    #[test]
    fn stale_with_a_live_agent_is_silent_and_a_dead_agent_is_dead() {
        let s = session(Some("p1"));
        let snap = snapshot(&[("p1", Some("S1"))]);
        assert_eq!(
            climb(&s, "stale", false, Some(&snap), None, &Fixed(Probe::Alive)),
            Verdict::Silent
        );
        assert_eq!(
            climb(&s, "stale", false, Some(&snap), None, &Fixed(Probe::Dead)),
            Verdict::Dead(Death::AgentDead)
        );
        assert_eq!(
            climb(
                &s,
                "stale",
                false,
                Some(&snap),
                None,
                &Fixed(Probe::Unknown)
            ),
            Verdict::Unknown
        );
        assert_eq!(
            climb(&s, "idle", false, Some(&snap), None, &Fixed(Probe::Dead)),
            Verdict::Alive
        );
    }

    #[test]
    fn a_session_with_no_pane_is_unknown() {
        let s = session(None);
        let snap = snapshot(&[]);
        assert_eq!(
            climb(&s, "stale", false, Some(&snap), None, &Fixed(Probe::Dead)),
            Verdict::Unknown
        );
    }

    fn holder_pane() -> String {
        format!("p{}", tiller_core::ids::new_ulid())
    }

    fn listing(panes: &[(&str, &str, HolderPaneState)]) -> Listing {
        Listing {
            instance: "I1".into(),
            panes: panes
                .iter()
                .map(|(pane, key, state)| tiller_panes::message::Pane {
                    pane: (*pane).to_owned().into(),
                    num: 1,
                    key: (*key).to_owned(),
                    pid: 4242,
                    argv: vec!["/bin/claude".into()],
                    cwd: "/home/user".into(),
                    started_at: jiff::Timestamp::UNIX_EPOCH,
                    state: *state,
                    exit: None,
                    cols: 120,
                    rows: 40,
                    meta: BTreeMap::new(),
                    title: String::new(),
                    last_output_at: None,
                    attach_clients: 0,
                    subscribers: 0,
                })
                .collect(),
        }
    }

    fn reading(listing: &Listing, recorded_pid: Option<u32>) -> Option<HolderReading<'_>> {
        Some(HolderReading {
            listing,
            recorded_pid,
        })
    }

    /// R18: with no successful `list` a holder pane is unknown, never gone,
    /// whatever the records and the probe say.
    #[test]
    fn a_holder_pane_with_no_list_is_unknown_not_gone() {
        let pane = holder_pane();
        let s = session(Some(&pane));
        let snap = snapshot(&[("w1:p1", None)]);
        for effective in ["working", "stale"] {
            assert_eq!(
                climb(&s, effective, true, Some(&snap), None, &Fixed(Probe::Dead)),
                Verdict::Unknown
            );
        }
    }

    #[test]
    fn a_running_holder_pane_is_alive_and_a_stale_one_probes_its_child() {
        let pane = holder_pane();
        let s = session(Some(&pane));
        let list = listing(&[(&pane, "S1", HolderPaneState::Running)]);
        assert_eq!(
            climb(
                &s,
                "working",
                false,
                None,
                reading(&list, None),
                &Fixed(Probe::Dead)
            ),
            Verdict::Alive,
            "no probe when not stale"
        );
        for (probe, verdict) in [
            (Probe::Alive, Verdict::Silent),
            (Probe::Dead, Verdict::Dead(Death::AgentDead)),
            (Probe::Unknown, Verdict::Unknown),
        ] {
            assert_eq!(
                climb(
                    &s,
                    "stale",
                    false,
                    None,
                    reading(&list, None),
                    &Fixed(probe)
                ),
                verdict,
                "{probe:?}"
            );
        }
    }

    /// Step 3: an exited child is positive evidence, stale or not.
    #[test]
    fn an_exited_holder_pane_is_a_dead_agent() {
        let pane = holder_pane();
        let s = session(Some(&pane));
        let list = listing(&[(&pane, "S1", HolderPaneState::Exited)]);
        for effective in ["working", "stale"] {
            assert_eq!(
                climb(
                    &s,
                    effective,
                    false,
                    None,
                    reading(&list, Some(1)),
                    &Fixed(Probe::Alive)
                ),
                Verdict::Dead(Death::AgentDead)
            );
        }
    }

    /// A pane the list lacks, or one whose key names another session, goes
    /// through the agent probe on the recorded pid before it is gone.
    #[test]
    fn a_missing_holder_pane_probes_the_recorded_pid_before_it_is_gone() {
        let pane = holder_pane();
        let s = session(Some(&pane));
        let other = listing(&[(&pane, "S9", HolderPaneState::Running)]);
        let empty = listing(&[]);
        for list in [&other, &empty] {
            assert_eq!(
                climb(
                    &s,
                    "working",
                    false,
                    None,
                    reading(list, Some(7)),
                    &Fixed(Probe::Dead)
                ),
                Verdict::Dead(Death::PaneGone)
            );
            assert_eq!(
                climb(
                    &s,
                    "working",
                    false,
                    None,
                    reading(list, Some(7)),
                    &Fixed(Probe::Alive)
                ),
                Verdict::Alive
            );
            assert_eq!(
                climb(
                    &s,
                    "stale",
                    false,
                    None,
                    reading(list, Some(7)),
                    &Fixed(Probe::Alive)
                ),
                Verdict::Silent
            );
            assert_eq!(
                climb(
                    &s,
                    "working",
                    false,
                    None,
                    reading(list, Some(7)),
                    &Fixed(Probe::Unknown)
                ),
                Verdict::Unknown
            );
            assert_eq!(
                climb(
                    &s,
                    "working",
                    false,
                    None,
                    reading(list, None),
                    &Fixed(Probe::Alive)
                ),
                Verdict::Dead(Death::PaneGone),
                "no recorded pid"
            );
        }
    }
}
