//! The script that starts a later session of a session's launch with every
//! flag of its first one: `tiller agent resume` and the daemon's resumes
//! (`recovery.md` R02, R04 and R21) both write it here, so the two cannot
//! differ.
//!
//! A worker's role, model and effort come from its run, and its launch
//! files from `<scratch_root>/<run id>`. An orchestrator's model and effort
//! come from the manifest its start recorded (`config_hash`), and its
//! launch files from `<scratch_root>/<session id>`. A session whose launch
//! files are gone is refused ([`launch::write_again`]): nothing rebuilds
//! them, since a rebuild would give the agent a prompt and settings it did
//! not start with.

use std::path::Path;

use rusqlite::Connection;
use tiller_core::domain::{ConfigManifest, Effort, Run, Session};
use tiller_core::state::{find, get};

use super::launch::{self, AgainSpec, Launch, Start};
use super::trust::ClaudeConfig;
use super::{Result, SpawnError, role_of};
use crate::roles::Role;

/// What a resume needs from the host beyond the session's records.
#[derive(Clone, Copy, Debug)]
pub struct Host<'a> {
    /// The run scratch root ([`super::config::Paths::scratch_dir`]).
    pub scratch_root: &'a Path,
    /// The effective Tiller home, exported as `TILLER_HOME`.
    pub tiller_home: &'a Path,
    /// The Claude Code command (`spawn.claude`).
    pub claude: &'a str,
    pub claude_config: &'a ClaudeConfig,
    /// `spawn.review_allowed_hosts` (P49).
    pub review_hosts: &'a [String],
}

/// Writes the script that starts `session` again in `cwd` as `start` says.
pub fn write(
    conn: &Connection,
    host: &Host<'_>,
    session: &Session,
    cwd: &Path,
    start: Start<'_>,
) -> Result<Launch> {
    let (role, run_id, model, effort, scratch) = match &session.run_id {
        Some(run_id) => {
            let run: Run = get(conn, "run", run_id)?;
            let role = role_of(run.role)?;
            let scratch = host.scratch_root.join(&run.id);
            (role, run.id, run.model, run.effort, scratch)
        }
        None => {
            let (model, effort) = recorded_model(conn, session)?;
            let scratch = host.scratch_root.join(&session.id);
            (Role::Orchestrator, String::new(), model, effort, scratch)
        }
    };
    launch::write_again(
        &AgainSpec {
            role,
            session_id: &session.id,
            run_id: &run_id,
            worktree: cwd,
            scratch: &scratch,
            tiller_home: host.tiller_home,
            model: &model,
            effort,
            claude: host.claude,
            claude_config: host.claude_config,
            review_hosts: host.review_hosts,
        },
        start,
    )
}

/// The model and effort an orchestrator session started with, from the
/// manifest of its `config_hash`.
fn recorded_model(conn: &Connection, session: &Session) -> Result<(String, Effort)> {
    let refused = |what: &str| {
        SpawnError::Refused(format!(
            "session {} {what}, so its launch cannot be repeated",
            session.id
        ))
    };
    let hash = session
        .config_hash
        .as_deref()
        .ok_or_else(|| refused("recorded no launch manifest"))?;
    let manifest: ConfigManifest = find(conn, "config_hash = ?1", &[&hash])?
        .ok_or_else(|| refused(&format!("names manifest {hash}, which is not recorded")))?;
    let model = manifest.parts["model"]
        .as_str()
        .ok_or_else(|| refused(&format!("names manifest {hash}, which has no model")))?;
    let effort = manifest.parts["effort"]
        .as_str()
        .and_then(|effort| effort.parse().ok())
        .ok_or_else(|| refused(&format!("names manifest {hash}, which has no effort")))?;
    Ok((model.to_owned(), effort))
}
