//! What the engine reads the world through (`recovery.md` section 2,
//! Sources). Every read is a trait, so the daemon wires the real holder, git,
//! forge and process table while tests drive the same passes against fakes
//! (`crate::testing`).
//!
//! Every reader returns `Err` or `None` for "could not tell": the engine
//! treats that as unknown and changes nothing, never as gone.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tiller_core::domain::{Project, Run, Session};
use tiller_core::state::approvals::PrFetch;
pub use tiller_observe::liveness::Probe;
use tiller_panes::{Meta, PaneId, Panes, PanesError, Spawn};

/// Step 3 of the liveness ladder: is the child's process `pid` in a holder
/// pane alive (`kill(pid, 0)`)?
pub trait AgentProbe: Send {
    fn process(&self, pid: u32) -> Probe {
        tiller_observe::liveness::process_alive(pid)
    }
}

/// The process table itself.
pub struct ProcessProbe;

impl AgentProbe for ProcessProbe {}

/// One worktree as a scan found it ("What unlanded means").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeScan {
    pub path: PathBuf,
    /// The path exists on disk. `false` is positive evidence that it is gone.
    pub exists: bool,
    /// The path is a git worktree (not just a directory).
    pub is_worktree: bool,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    /// `None` when the scan could not run; `None` blocks cleanup.
    pub dirty: Option<bool>,
    /// `None` when the scan could not run; `None` blocks cleanup.
    pub unlanded_commits: Option<i64>,
}

impl WorktreeScan {
    /// A path that does not exist.
    pub fn missing(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            exists: false,
            is_worktree: false,
            branch: None,
            head_sha: None,
            dirty: None,
            unlanded_commits: None,
        }
    }

    /// Everything `run.clean` needs from the scan: a clean worktree with
    /// nothing unlanded, positively known.
    pub fn provably_clean(&self) -> bool {
        self.exists
            && self.is_worktree
            && self.dirty == Some(false)
            && self.unlanded_commits == Some(0)
    }
}

/// The worktree scanner: treehouse's list plus git.
pub trait Scanner: Send {
    /// The worktrees of this host's project checkouts, each checkout's own
    /// working tree left out. `Err` means some checkout's list could not be
    /// read; the scan then covers only the paths records name, and R15
    /// returns nothing.
    fn listed(&self) -> Result<Vec<PathBuf>, String>;
    /// One worktree against its project's default branch and the heads of
    /// its merged PRs.
    fn scan(&self, path: &Path, project: &Project, merged_heads: &[String]) -> WorktreeScan;
    /// Returns the worktree to treehouse, only while `holder` still leases
    /// it, after `run.clean` recorded that it provably holds nothing. The
    /// default has nowhere to return it to.
    fn release(&self, path: &Path, _holder: &str) -> Result<(), String> {
        Err(format!(
            "no worktree pool is configured to take back {}",
            path.display()
        ))
    }
}

/// A scanner that lists nothing and finds every path as the file system
/// reports it, with git questions unanswered: cleanup stays blocked.
pub struct FilesystemScanner;

impl Scanner for FilesystemScanner {
    fn listed(&self) -> Result<Vec<PathBuf>, String> {
        Err("no worktree lister is configured".into())
    }

    fn scan(&self, path: &Path, _project: &Project, _merged_heads: &[String]) -> WorktreeScan {
        WorktreeScan {
            path: path.to_owned(),
            exists: path.exists(),
            is_worktree: path.join(".git").exists(),
            branch: None,
            head_sha: None,
            dirty: None,
            unlanded_commits: None,
        }
    }
}

/// One PR as the forge reports it, with the scrub of its body (R19) that
/// `tiller land` step 4 runs too.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrRead {
    pub fetch: PrFetch,
    /// Lines of the body that fail the scrub: a quoted ask, a scrub-list
    /// term, a stale pipeline claim. Empty when the body is clean.
    pub body_leaks: Vec<String>,
    /// Whether the repository is public, when the forge says.
    pub repo_public: Option<bool>,
    /// The login that merged the PR, when the forge says (R07).
    pub merged_by: Option<String>,
    /// The PR's head branch name, when the forge says (grant matching).
    pub head_ref: Option<String>,
    /// The paths the PR changes, when the forge says (grant matching).
    pub changed_paths: Option<Vec<String>>,
}

/// The forge (`gh-axi`) and the project's remote.
pub trait Forge: Send {
    fn pr(&self, repo: &str, number: i64) -> Result<PrRead, String>;
    /// The head of the project's default branch on its remote.
    fn default_branch_head(&self, project: &Project) -> Result<String, String>;
    /// Whether `ancestor` is reachable from `descendant` on the remote.
    fn is_ancestor(
        &self,
        project: &Project,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, String>;
}

/// A forge that answers nothing: every read fails, so caches keep their
/// values with a `fetch_error` and nothing is called merged or moved.
pub struct NoForge;

impl Forge for NoForge {
    fn pr(&self, _repo: &str, _number: i64) -> Result<PrRead, String> {
        Err("no forge is configured".into())
    }

    fn default_branch_head(&self, _project: &Project) -> Result<String, String> {
        Err("no forge is configured".into())
    }

    fn is_ancestor(
        &self,
        _project: &Project,
        _ancestor: &str,
        _descendant: &str,
    ) -> Result<bool, String> {
        Err("no forge is configured".into())
    }
}

/// What a launch produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launched {
    pub pane_id: String,
}

/// Writes the script that resumes a session, with every flag of its first
/// launch: the daemon wires the builder `tiller agent resume` writes its
/// script with, so the two never differ. `resume` names the Claude session
/// to `--resume`; `None` writes a plain start. On `Err` nothing was
/// started ([`Prepare`]).
pub type ScriptWriter =
    Box<dyn FnMut(&Session, &Path, Option<&str>) -> Result<PathBuf, Prepare> + Send>;

/// Why a resume script was not written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Prepare {
    /// The launch cannot be repeated, such as for a session whose launch
    /// files are gone: a later try would fail the same way.
    Refused(String),
    /// Something that may pass by itself failed, such as a `config.toml`
    /// that does not parse or a store error: a later pass tries again.
    Failed(String),
}

/// Starts and stops Claude Code sessions in holder panes (`session.resume`,
/// R02, R04 and R21) and closes dead panes (R15).
pub trait Launcher: Send {
    /// Writes the script that starts `claude` for `session` in `cwd`:
    /// `--resume <id>` when `resume` names the Claude session, a fresh
    /// session otherwise ([`ScriptWriter`]). On `Err` nothing was started.
    fn prepare(
        &mut self,
        session: &Session,
        cwd: &Path,
        resume: Option<&str>,
    ) -> Result<PathBuf, Prepare>;
    /// Runs `script`, which [`Launcher::prepare`] wrote, in a new pane for
    /// `session` in `cwd`.
    fn launch(&mut self, session: &Session, cwd: &Path, script: &Path) -> Result<Launched, String>;
    /// Closes a pane whose agent is dead and whose run has ended.
    fn close_pane(&mut self, pane_id: &str) -> Result<(), String>;
    /// One desktop notification (R04's reboot summary).
    fn notify(&mut self, title: &str, body: &str);
}

/// How long a closed holder pane's child has between `hup` and `kill`.
pub const HOLDER_CLOSE_GRACE: Duration = Duration::from_secs(3);

/// One desktop notification; the daemon wires `[notify] command`.
pub type Notifier = Box<dyn FnMut(&str, &str) + Send>;

/// A launcher over the terminal holder (`terminal-holder.md` 4.4): one
/// `spawn` keyed by the session, whose child runs the resume script
/// through `/bin/sh` in `cwd`, with the session named in the pane's `meta`.
/// A spawn for a key the holder already runs returns that pane, so a second
/// try starts nothing.
pub struct HolderLauncher {
    holder: Arc<dyn Panes>,
    scripts: ScriptWriter,
    notify: Notifier,
}

impl HolderLauncher {
    pub fn new(
        holder: Arc<dyn Panes>,
        scripts: ScriptWriter,
        notify: impl FnMut(&str, &str) + Send + 'static,
    ) -> Self {
        Self {
            holder,
            scripts,
            notify: Box::new(notify),
        }
    }

    /// The `spawn` that runs `script` for `session` in `cwd`. The pane gets
    /// what any pane gets ([`tiller_panes::env::current`]); the script
    /// exports the session's own environment, as a first launch's does.
    pub fn request(session: &Session, cwd: &Path, script: &Path) -> Spawn {
        let mut meta = Meta::from([
            ("session".to_owned(), session.id.clone()),
            ("project".to_owned(), session.project_id.clone()),
            ("role".to_owned(), session.role.as_str().to_owned()),
            (
                "label".to_owned(),
                format!("{} resuming", session.project_id),
            ),
        ]);
        if let Some(run) = &session.run_id {
            meta.insert("run".to_owned(), run.clone());
        }
        Spawn {
            key: session.id.clone(),
            argv: vec![
                tiller_panes::env::SHELL.to_owned(),
                script.to_string_lossy().into_owned(),
            ],
            cwd: cwd.to_string_lossy().into_owned(),
            env: tiller_panes::env::current(),
            size: None,
            meta,
        }
    }
}

impl Launcher for HolderLauncher {
    fn prepare(
        &mut self,
        session: &Session,
        cwd: &Path,
        resume: Option<&str>,
    ) -> Result<PathBuf, Prepare> {
        (self.scripts)(session, cwd, resume)
    }

    fn launch(&mut self, session: &Session, cwd: &Path, script: &Path) -> Result<Launched, String> {
        let pane = self
            .holder
            .spawn(&Self::request(session, cwd, script))
            .map_err(|e| e.to_string())?;
        Ok(Launched {
            pane_id: pane.pane.as_str().to_owned(),
        })
    }

    /// `close`, which sends `hup` and then `kill` after the grace, then
    /// `forget`. A pane that has exited or is gone is only forgotten.
    fn close_pane(&mut self, pane_id: &str) -> Result<(), String> {
        let pane = PaneId::from(pane_id);
        match self.holder.close(&pane, HOLDER_CLOSE_GRACE) {
            Ok(_) | Err(PanesError::NotFound(_) | PanesError::Exited(_)) => {}
            Err(e) => return Err(format!("closing pane {pane}: {e}")),
        }
        match self.holder.forget(&pane) {
            Ok(()) | Err(PanesError::NotFound(_)) => Ok(()),
            Err(e) => Err(format!("forgetting pane {pane}: {e}")),
        }
    }

    fn notify(&mut self, title: &str, body: &str) {
        (self.notify)(title, body);
    }
}

/// What the host check reads every five minutes (R01, R16).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostReadings {
    pub ram_free_mb: Option<i64>,
    pub disk_free_mb: Option<i64>,
    /// A version skew between a tool and its daemon, as one line, or none.
    pub version_skew: Option<String>,
    /// Tool versions for `daemon.started` and Health (P43).
    pub versions: serde_json::Value,
}

pub trait Readings: Send {
    fn read(&self) -> HostReadings;
}

/// Readings nobody takes: floors with no reading fail closed at every launch
/// (`run.start`'s rule), and nothing is reported as skewed.
pub struct NoReadings;

impl Readings for NoReadings {
    fn read(&self) -> HostReadings {
        HostReadings::default()
    }
}

/// A cut the harness itself recorded (R14): a `no-mistakes` fix agent cut at
/// its limit, a `--max-turns` stop, the daemon's own spend cap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cut {
    pub reason: String,
    /// What the harness says was done, when it says.
    pub done: Option<String>,
    /// What the harness says remains, when it says.
    pub remains: Option<String>,
}

/// The harness records a run may have been cut by.
pub trait Harness: Send {
    fn cut(&self, run: &Run) -> Option<Cut>;
}

/// No harness record: nothing is ever read as cut.
pub struct NoHarness;

impl Harness for NoHarness {
    fn cut(&self, _run: &Run) -> Option<Cut> {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use tiller_core::Timestamp;
    use tiller_core::domain::{Activity, Role};

    fn worker() -> Session {
        Session {
            id: "S1".into(),
            claude_session_id: Some("claude-S1".into()),
            role: Role::Builder,
            project_id: "alpha".into(),
            run_id: Some("R1".into()),
            host_id: "H1".into(),
            pane_id: None,
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
            activity_observed_at: None,
            activity_source: None,
            rev: 1,
        }
    }

    #[test]
    fn a_resumed_holder_pane_runs_the_written_script() {
        let session = worker();
        let req = HolderLauncher::request(
            &session,
            Path::new("/home/user/pool/w1"),
            Path::new("/home/user/runs/R1/resume-S1.sh"),
        );
        assert_eq!(
            req.argv,
            [tiller_panes::env::SHELL, "/home/user/runs/R1/resume-S1.sh"]
        );
        assert_eq!(req.key, "S1");
        assert_eq!(req.cwd, "/home/user/pool/w1");
        assert_eq!(req.meta.get("run").map(String::as_str), Some("R1"));
    }
}
