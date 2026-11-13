//! The sandbox a replay runs in. Everything lives under one root:
//!
//! - `home/`: the Tiller home, with `state.db` initialized and the bundle
//!   imported;
//! - `git/<project>/`: a bare `origin.git` seeded with one commit, and the
//!   `checkout` spawn cuts worktrees from;
//! - `pool/`: the stub pool's worktrees, `runs/`: each run's scratch;
//!
//! The fake terminal holder keeps its panes and call log in memory. Nothing
//! in it names a real remote, a real holder or a real pool.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use tiller_agents::spawn::World;
use tiller_agents::spawn::config::SpawnConfig;
use tiller_agents::spawn::listeners::NoListeners;
use tiller_agents::spawn::pool::Pool;
use tiller_agents::spawn::trust::ClaudeConfig;
use tiller_core::domain::{Boundary, Card, Project};
use tiller_core::import::Bundle;
use tiller_core::state::projects::Init;
use tiller_core::state::runs::NewOrchestrator;
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Ctx, Store};
use tiller_panes::{FakePanes, Meta, PaneId};
use tiller_reconcile::git::GitScanner;

use crate::Error;
use crate::baseline::task_id;

/// The prefix of every orchestrator's Claude session id in a sandbox, so an
/// audit of a real home can tell a replay's rows from the owner's.
pub const SESSION_PREFIX: &str = "tiller-replay-";

/// The free RAM and disk spawn is told: the sandbox never fills a host.
const FREE_MB: i64 = 1 << 20;

pub struct Sandbox {
    pub root: PathBuf,
    pub home: PathBuf,
    pub store: Store,
    pub config: SpawnConfig,
    pub holder: FakePanes,
    pub pool: StubPool,
    pub scanner: GitScanner,
    /// The `tiller` path written into launch lines; nothing runs it.
    pub tiller: PathBuf,
    pub scratch: PathBuf,
    /// The Claude config dir launches record trust in, inside the sandbox.
    pub claude: ClaudeConfig,
    /// The live orchestrator session of each project used so far.
    pub sessions: BTreeMap<String, String>,
    /// The last event before the replay started, so the audit judges only
    /// what the replay wrote, not the import.
    pub mark: i64,
}

impl Sandbox {
    /// Builds a sandbox under `root`, which must be empty or missing, and
    /// imports `bundle` into it. `dispatch` is the `[dispatch]` table spawn
    /// resolves models with; `None` uses the built-in rules.
    pub fn create(
        root: &Path,
        bundle: &Bundle,
        dispatch: Option<toml::Value>,
    ) -> Result<Self, Error> {
        fs::create_dir_all(root)?;
        if fs::read_dir(root)?.next().is_some() {
            return Err(Error::Sandbox(format!("{} is not empty", root.display())));
        }
        let root = root.canonicalize()?;
        let home = root.join("home");
        let scratch = root.join("runs");
        for dir in [&home, &scratch, &root.join("git"), &root.join("pool")] {
            fs::create_dir_all(dir)?;
        }
        let mut store = Store::open(home.join("state.db"))?;
        let owner = Ctx::new(Actor::Owner);
        store.init(
            &owner,
            Init {
                host_label: "replay".into(),
                lane_capacity: 8,
                ram_floor_mb: None,
                disk_floor_mb: None,
                boundary: Boundary::Personal,
            },
        )?;
        store.import_apply(&owner, bundle)?;
        let mark = store
            .conn()
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                row.get(0)
            })
            .map_err(tiller_core::Error::from)?;

        let mut table = toml::Table::new();
        table.insert(
            "quota".into(),
            toml::Value::Table(toml::Table::from_iter([(
                "enabled".to_owned(),
                toml::Value::Boolean(false),
            )])),
        );
        if let Some(dispatch) = dispatch {
            table.insert("dispatch".into(), dispatch);
        }
        let config = SpawnConfig::from_table(&table)?;
        let claude = ClaudeConfig::at(root.join("claude"));
        Ok(Self {
            pool: StubPool::new(root.join("pool")),
            tiller: root.join("bin").join("tiller"),
            root,
            home,
            store,
            config,
            holder: FakePanes::new(),
            scanner: GitScanner::new(),
            scratch,
            claude,
            sessions: BTreeMap::new(),
            mark,
        })
    }

    /// The live orchestrator session of `project`, started on first use with
    /// its own fake pane, sandbox origin and checkout.
    pub fn orchestrator(&mut self, project: &str) -> Result<String, Error> {
        if let Some(session) = self.sessions.get(project) {
            return Ok(session.clone());
        }
        self.checkout(project)?;
        let pane = PaneId::generate();
        let meta = Meta::from([("label".to_owned(), project.to_owned())]);
        self.holder.add(&pane, project, meta);
        let pane = pane.to_string();
        let started = self.store.session_start(
            &Ctx::new(Actor::Owner),
            NewOrchestrator {
                project_id: project.to_owned(),
                pane_id: Some(pane),
                claude_session_id: Some(format!("{SESSION_PREFIX}{project}")),
                ..NewOrchestrator::default()
            },
        )?;
        let session = started.value.session.id;
        self.sessions.insert(project.to_owned(), session.clone());
        Ok(session)
    }

    /// The checkout of `project`, seeded on first use: an orchestrator's own
    /// project, or the project of a card another project's orchestrator
    /// spawns on, as the live config names a checkout for every project.
    pub fn checkout(&mut self, project: &str) -> Result<PathBuf, Error> {
        if let Some(found) = self.config.checkouts.get(project) {
            return Ok(found.clone());
        }
        let found: Project = get(self.store.conn(), "project", project)?;
        let checkout = self.seed(project, &found.default_branch)?;
        self.config
            .checkouts
            .insert(project.to_owned(), checkout.clone());
        Ok(checkout)
    }

    /// A bare origin holding one commit on `branch`, and a clone of it.
    fn seed(&self, project: &str, branch: &str) -> Result<PathBuf, Error> {
        let dir = self.root.join("git").join(project);
        fs::create_dir_all(&dir)?;
        git(
            &dir,
            &["init", "--quiet", "--bare", "-b", branch, "origin.git"],
        )?;
        git(&dir, &["clone", "--quiet", "origin.git", "seed"])?;
        let seed = dir.join("seed");
        git(&seed, &["checkout", "--quiet", "-b", branch])?;
        fs::write(
            seed.join("README.md"),
            format!("{project}: replay sandbox\n"),
        )?;
        git(&seed, &["add", "README.md"])?;
        git(&seed, &["commit", "--quiet", "-m", "replay sandbox"])?;
        git(&seed, &["push", "--quiet", "origin", branch])?;
        fs::remove_dir_all(&seed)?;
        git(&dir, &["clone", "--quiet", "origin.git", "checkout"])?;
        Ok(dir.join("checkout"))
    }

    /// Each card the bundle names, keyed as `replay.jsonl` names it (its
    /// Firstmate task id), to the card the import made of it.
    pub fn imported(&self, bundle: &Bundle) -> Result<HashMap<String, Card>, Error> {
        let mut cards = HashMap::new();
        for card in &bundle.cards {
            let key = task_id(card);
            if let Some(found) = find::<Card>(self.store.conn(), "legacy_id = ?1", &[&card.id])? {
                cards.insert(key, found);
            }
        }
        Ok(cards)
    }

    /// The live (not retired) projects the bundle import created, by id.
    pub fn projects(&self) -> Result<Vec<String>, Error> {
        let projects: Vec<Project> =
            list(self.store.conn(), "retired_at IS NULL ORDER BY id", &[])?;
        Ok(projects.into_iter().map(|p| p.id).collect())
    }

    /// The store, and the world spawn and cleanup run against beside it.
    pub fn world(&mut self) -> (&mut Store, World<'_>) {
        let world = World {
            config: &self.config,
            pool: &self.pool,
            panes: Some(&self.holder),
            scanner: &self.scanner,
            listeners: &NoListeners,
            tiller: &self.tiller,
            scratch_root: &self.scratch,
            tiller_home: &self.home,
            claude_config: &self.claude,
            ram_free_mb: Some(FREE_MB),
            disk_free_mb: Some(FREE_MB),
        };
        (&mut self.store, world)
    }
}

/// Runs git in `dir` with an identity, unsigned commits and no hooks, so
/// the owner's git config never shapes the sandbox.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Tiller Replay",
            "-c",
            "user.email=replay@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(Error::Sandbox(format!(
            "git {} in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A take or a give-back, in order.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Lease {
    pub path: PathBuf,
    pub holder: String,
    pub returned: bool,
}

/// The worktree pool, stubbed: each take clones the project checkout into
/// a new slot under the sandbox, its origin pointed at the sandbox origin
/// beside that checkout, so spawn's own fetch brings it to the origin head.
pub struct StubPool {
    slots: PathBuf,
    leases: Mutex<Vec<Lease>>,
}

impl StubPool {
    pub fn new(slots: PathBuf) -> Self {
        Self {
            slots,
            leases: Mutex::default(),
        }
    }

    pub fn leases(&self) -> Vec<Lease> {
        self.leases.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Pool for StubPool {
    fn take(
        &self,
        checkout: &Path,
        branch: &str,
        base: &str,
        holder: &str,
    ) -> Result<PathBuf, String> {
        let mut leases = self.leases.lock().map_err(|e| e.to_string())?;
        let slot = self.slots.join(format!("slot-{}", leases.len() + 1));
        let origin = checkout
            .parent()
            .map(|dir| dir.join("origin.git"))
            .ok_or_else(|| format!("{} has no parent", checkout.display()))?;
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let step = |dir: &Path, args: &[&str]| git(dir, args).map(drop).map_err(|e| e.to_string());
        step(
            &self.slots,
            &[
                "clone",
                "--quiet",
                "--branch",
                base,
                &text(checkout),
                &text(&slot),
            ],
        )?;
        step(&slot, &["remote", "set-url", "origin", &text(&origin)])?;
        step(&slot, &["checkout", "--quiet", "-b", branch])?;
        leases.push(Lease {
            path: slot.clone(),
            holder: holder.to_owned(),
            returned: false,
        });
        Ok(slot)
    }

    fn give_back(&self, path: &Path, holder: &str) -> Result<(), String> {
        let mut leases = self.leases.lock().map_err(|e| e.to_string())?;
        let lease = leases
            .iter_mut()
            .find(|l| l.path == path && l.holder == holder && !l.returned)
            .ok_or_else(|| format!("{} is not leased to {holder}", path.display()))?;
        lease.returned = true;
        Ok(())
    }
}
