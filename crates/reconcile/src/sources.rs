//! What the engine reads the world through (`recovery.md` section 2,
//! Sources). Every read is a trait, so the daemon wires the real herdr, git,
//! forge and process table while tests drive the same passes against fakes
//! (`crate::testing`).
//!
//! Every reader returns `Err` or `None` for "could not tell": the engine
//! treats that as unknown and changes nothing, never as gone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tiller_core::domain::{Project, Run, Session};
use tiller_core::state::approvals::PrFetch;
use tiller_herdr::client::{Herdr, PaneInfo};
pub use tiller_observe::liveness::Probe;

/// Step 3 of the liveness ladder: is the agent process in a pane alive?
pub trait AgentProbe: Send {
    fn agent(&self, pane: &PaneInfo) -> Probe;
}

/// herdr's own reading: a pane with a recognized agent runs one; a pane
/// herdr sees as a plain shell has lost it. herdr reads the pane's process,
/// so this is the "agent status" half of the ladder's step 3.
pub struct HerdrStatusProbe;

impl AgentProbe for HerdrStatusProbe {
    fn agent(&self, pane: &PaneInfo) -> Probe {
        if pane.agent.is_some() {
            Probe::Alive
        } else {
            Probe::Dead
        }
    }
}

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

/// Starts and stops Claude Code sessions in herdr panes (`session.resume`,
/// R02 and R04) and closes dead panes (R15).
pub trait Launcher: Send {
    /// Launches `claude` for `session` in `cwd`: `--resume <id>` when
    /// `resume` names the Claude session, a fresh session otherwise.
    fn launch(
        &mut self,
        session: &Session,
        cwd: &Path,
        resume: Option<&str>,
    ) -> Result<Launched, String>;
    /// Closes a pane whose agent is dead and whose run has ended.
    fn close_pane(&mut self, pane_id: &str) -> Result<(), String>;
    /// One desktop notification (R04's reboot summary).
    fn notify(&mut self, title: &str, body: &str);
}

/// A launcher over the herdr adapter: a new workspace in `cwd`, the row
/// tokens reported so the pane names its session before anything else reads
/// it, and the launch command typed as one line. Closing a pane needs a
/// herdr call the adapter does not expose yet, so `close` is injected.
pub struct HerdrLauncher<H: Herdr> {
    herdr: H,
    close: PaneCloser,
}

/// Closes one pane by id; the daemon wires the herdr call.
pub type PaneCloser = Box<dyn FnMut(&str) -> Result<(), String> + Send>;

impl<H: Herdr + Send> HerdrLauncher<H> {
    pub fn new(herdr: H, close: impl FnMut(&str) -> Result<(), String> + Send + 'static) -> Self {
        Self {
            herdr,
            close: Box::new(close),
        }
    }

    /// The command line a resume types.
    pub fn command(session: &Session, resume: Option<&str>) -> String {
        match resume {
            Some(id) => format!("TILLER_SESSION_ID={} claude --resume {id}", session.id),
            None => format!("TILLER_SESSION_ID={} claude", session.id),
        }
    }
}

impl<H: Herdr + Send> Launcher for HerdrLauncher<H> {
    fn launch(
        &mut self,
        session: &Session,
        cwd: &Path,
        resume: Option<&str>,
    ) -> Result<Launched, String> {
        let label = format!("{} resuming", session.project_id);
        let created = self
            .herdr
            .create_workspace(&label, Some(cwd))
            .map_err(|e| e.to_string())?;
        let tokens = BTreeMap::from([
            ("session".to_owned(), session.id.clone()),
            ("project".to_owned(), session.project_id.clone()),
        ]);
        self.herdr
            .report_tokens(&created.pane_id, &tokens)
            .map_err(|e| e.to_string())?;
        let line = Self::command(session, resume);
        self.herdr
            .send_text(&created.pane_id, &line)
            .and_then(|()| self.herdr.send_enter(&created.pane_id))
            .map_err(|e| e.to_string())?;
        Ok(Launched {
            pane_id: created.pane_id,
        })
    }

    fn close_pane(&mut self, pane_id: &str) -> Result<(), String> {
        (self.close)(pane_id)
    }

    fn notify(&mut self, title: &str, body: &str) {
        if let Err(error) = self.herdr.notify(title, body) {
            tracing::warn!(%error, "herdr notification failed");
        }
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
