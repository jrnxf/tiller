//! Sessions that run an older tiller than the host does now.
//!
//! A session keeps the role prompt and hook settings its launch wrote, so one
//! launched under an older release runs them until it is restarted. Each
//! launch records the tiller version and a hash of the packaged role file,
//! settings template and MCP config ([`current`]). A session is stale when
//! either differs from what this binary would write now. A session with no
//! record (launched before the record existed) is stale with version
//! "unknown". A restart is never automatic: a person asks for it, and a
//! working session waits until it is idle (`restart_queued_at`).

use rusqlite::params;
use serde::{Deserialize, Serialize};
use tiller_core::domain::{Activity, Role as SessionRole, Session};
use tiller_core::state::{get, list};
use tiller_core::{Applied, Ctx, Store};

use crate::roles::{MCP_CONFIG, Role};
use crate::spawn::launch::sha256_hex;

/// The version recorded for a session with no launch record.
pub const UNKNOWN: &str = "unknown";

/// What a launch by this binary writes, for one role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchStamp {
    pub tiller_version: &'static str,
    pub launch_hash: String,
}

/// The stamp a launch of `role` by this binary records.
pub fn current(role: Role) -> LaunchStamp {
    let parts = [role.file(), role.settings_template(), MCP_CONFIG];
    LaunchStamp {
        tiller_version: env!("CARGO_PKG_VERSION"),
        launch_hash: sha256_hex(parts.join("\0").as_bytes()),
    }
}

/// The tiller version this binary is.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Why a session is stale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stale {
    /// The version it started on, or [`UNKNOWN`].
    pub since: String,
    /// Whether the role prompt or hook settings it was launched with differ
    /// from the packaged ones.
    pub prompt_changed: bool,
}

/// Whether `session` runs an older tiller than this binary, and why.
pub fn staleness(session: &Session) -> Option<Stale> {
    let role = Role::parse(&session.role.to_string())?;
    let now = current(role);
    let (Some(version), Some(hash)) = (&session.tiller_version, &session.launch_hash) else {
        return Some(Stale {
            since: session
                .tiller_version
                .clone()
                .unwrap_or_else(|| UNKNOWN.to_owned()),
            prompt_changed: false,
        });
    };
    let prompt_changed = *hash != now.launch_hash;
    (version != now.tiller_version || prompt_changed).then(|| Stale {
        since: version.clone(),
        prompt_changed,
    })
}

/// Records what a launch of `role` wrote on its live session.
pub fn record(
    store: &mut Store,
    ctx: &Ctx,
    session: &str,
    role: Role,
) -> tiller_core::Result<Applied<Session>> {
    let stamp = current(role);
    store.session_stamp(ctx, session, stamp.tiller_version, &stamp.launch_hash)
}

/// One stale session with the facts a restart and its listing need.
#[derive(Clone, Debug, PartialEq)]
pub struct StaleSession {
    pub session: Session,
    pub stale: Stale,
    /// Whether it is idle now, so a restart may run at once.
    pub idle: bool,
}

/// The live sessions on this host that run an older tiller, orchestrators
/// first and the oldest launch first within a role.
pub fn stale_sessions(store: &Store) -> tiller_core::Result<Vec<StaleSession>> {
    let live: Vec<Session> = list(
        store.conn(),
        "ended_at IS NULL
         AND host_id = (SELECT value FROM meta WHERE key = 'host_id')
         AND project_id NOT IN (SELECT id FROM projects WHERE retired_at IS NOT NULL)
         AND (role = 'orchestrator'
              OR run_id IN (SELECT id FROM runs WHERE ended_at IS NULL))
         ORDER BY role != 'orchestrator', started_at, id",
        &[],
    )?;
    let mut stale = Vec::new();
    for session in live {
        let Some(why) = staleness(&session) else {
            continue;
        };
        let idle = is_idle(store, &session)?;
        stale.push(StaleSession {
            session,
            stale: why,
            idle,
        });
    }
    Ok(stale)
}

/// Whether the session's effective activity is idle: a stale or unknown
/// reading is not, since the session may be at work.
pub fn is_idle(store: &Store, session: &Session) -> tiller_core::Result<bool> {
    let effective: Option<String> = store
        .conn()
        .query_row(
            "SELECT effective_activity FROM session_activity WHERE session_id = ?1",
            params![session.id],
            |row| row.get(0),
        )
        .ok();
    Ok(effective.as_deref() == Some(&Activity::Idle.to_string()))
}

/// The session as the store holds it now.
pub fn reload(store: &Store, id: &str) -> tiller_core::Result<Session> {
    get(store.conn(), "session", id)
}

/// Whether a session of this role is the orchestrator.
pub fn is_orchestrator(session: &Session) -> bool {
    session.role == SessionRole::Orchestrator
}

/// What happened to one stale session when a restart was asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It was idle and has been restarted.
    Restarted,
    /// It is working: the restart waits for it to go idle.
    Queued,
    /// The restart failed; the session is untouched.
    Failed(String),
}

/// The outcome for one session of a bulk restart.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub session: Session,
    pub stale: Stale,
    pub outcome: Outcome,
}

/// Restarts through the one restart path the caller owns (`tiller
/// orchestrator start --restart`, or a worker relaunch). It gets the session
/// to restart and returns the failure text, if any.
pub type Restart<'a> = dyn FnMut(&mut Store, &Session) -> Result<(), String> + 'a;

/// Restarts every idle stale session now and queues the working ones to
/// restart when they next go idle. Only the sessions in `only` when it is
/// `Some`, so a person confirms the set they saw.
pub fn restart_stale(
    store: &mut Store,
    ctx: &Ctx,
    only: Option<&[String]>,
    restart: &mut Restart<'_>,
) -> tiller_core::Result<Vec<Report>> {
    let mut reports = Vec::new();
    for item in stale_sessions(store)? {
        if only.is_some_and(|ids| !ids.contains(&item.session.id)) {
            continue;
        }
        let outcome = if item.idle {
            match restart(store, &item.session) {
                Ok(()) => Outcome::Restarted,
                Err(why) => Outcome::Failed(why),
            }
        } else {
            store.session_restart_queue(ctx, &item.session.id, true)?;
            Outcome::Queued
        };
        reports.push(Report {
            session: item.session,
            stale: item.stale,
            outcome,
        });
    }
    Ok(reports)
}

/// Restarts the queued sessions that are idle now. A queued session that is
/// no longer stale has its ask withdrawn, so it is never restarted on a later
/// upgrade without a new request.
pub fn drain_queue(
    store: &mut Store,
    ctx: &Ctx,
    restart: &mut Restart<'_>,
) -> tiller_core::Result<Vec<Report>> {
    let mut reports = Vec::new();
    for session in store.restart_queued()? {
        let Some(stale) = staleness(&session) else {
            // Best effort: the session may have ended meanwhile.
            let _ = store.session_restart_queue(ctx, &session.id, false);
            continue;
        };
        if !is_idle(store, &session)? {
            continue;
        }
        let outcome = match restart(store, &session) {
            Ok(()) => Outcome::Restarted,
            Err(why) => {
                // Drop the queue entry so a failing restart is not retried
                // on every tick; the session shows as stale again.
                // The failed launch may have ended the session, so a refused
                // unqueue is not an error here.
                let _ = store.session_restart_queue(ctx, &session.id, false);
                Outcome::Failed(why)
            }
        };
        reports.push(Report {
            session,
            stale,
            outcome,
        });
    }
    Ok(reports)
}
