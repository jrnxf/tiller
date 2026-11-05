//! Fakes for every source and a small world to run passes against (feature
//! `testing`): the herdr test fleet from `tiller_herdr::testing`, a fake
//! herdr server, and in-memory probes, scans, forge answers, launches,
//! readings and harness records that tests set between passes.
//!
//! This file is also compiled into the crate's own integration tests through
//! `tests/common/mod.rs`, so it names the crate as `tiller_reconcile`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::json;
use tiller_core::domain::{Activity, Attention, ObservationSource, OutboxMessage, Run, Session};
use tiller_core::state::runs::NewObservation;
use tiller_core::state::{get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_herdr::client::PaneInfo;
use tiller_herdr::fake::{FakeHerdr, Screen};
pub use tiller_herdr::testing::{Fleet, owner};

use tiller_reconcile::pass::{Config, Reconciler, Sources};
use tiller_reconcile::sources::{
    AgentProbe, Cut, Forge, Harness, HostReadings, Launched, Launcher, PrRead, Probe, Readings,
    Scanner, WorktreeScan,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The wall clock, truncated as the store stores it. Staleness in the
/// `session_activity` view reads the database's own clock, so tests
/// backdate observations from here instead of moving time forward.
pub fn now() -> Timestamp {
    Timestamp::now()
}

/// `minutes` before now.
pub fn ago(minutes: i64) -> Timestamp {
    now().plus_minutes(-minutes)
}

/// A probe that answers from herdr's reading unless a pane is overridden.
#[derive(Clone, Default)]
pub struct FakeProbe {
    overrides: Arc<Mutex<HashMap<String, Probe>>>,
}

impl FakeProbe {
    pub fn set(&self, pane_id: &str, probe: Probe) {
        lock(&self.overrides).insert(pane_id.to_owned(), probe);
    }
}

impl AgentProbe for FakeProbe {
    fn agent(&self, pane: &PaneInfo) -> Probe {
        if let Some(probe) = lock(&self.overrides).get(&pane.pane_id) {
            return *probe;
        }
        if pane.agent.is_some() {
            Probe::Alive
        } else {
            Probe::Dead
        }
    }
}

/// Scans the tests set per path; an unset path is missing. A head among the
/// merged heads the caller passes has nothing unlanded, as `git rev-list
/// HEAD --not <head>` finds nothing.
#[derive(Clone, Default)]
pub struct FakeScanner {
    scans: Arc<Mutex<HashMap<PathBuf, WorktreeScan>>>,
    listed: Arc<Mutex<Option<Vec<PathBuf>>>>,
    released: Arc<Mutex<Vec<PathBuf>>>,
}

impl FakeScanner {
    /// Paths returned to the pool, in order.
    pub fn released(&self) -> Vec<PathBuf> {
        lock(&self.released).clone()
    }

    pub fn set(&self, scan: WorktreeScan) {
        lock(&self.scans).insert(scan.path.clone(), scan);
    }

    /// A clean worktree on `branch` at `head` with `unlanded` commits.
    pub fn worktree(
        &self,
        path: &str,
        branch: &str,
        head: &str,
        dirty: bool,
        unlanded: Option<i64>,
    ) {
        self.set(WorktreeScan {
            path: PathBuf::from(path),
            exists: true,
            is_worktree: true,
            branch: Some(branch.into()),
            head_sha: Some(head.into()),
            dirty: Some(dirty),
            unlanded_commits: unlanded,
        });
    }

    pub fn remove(&self, path: &str) {
        lock(&self.scans).remove(Path::new(path));
    }

    /// What treehouse lists; `None` makes the list unreadable.
    pub fn list(&self, paths: Option<Vec<&str>>) {
        *lock(&self.listed) = paths.map(|p| p.into_iter().map(PathBuf::from).collect());
    }
}

impl Scanner for FakeScanner {
    fn listed(&self) -> Result<Vec<PathBuf>, String> {
        lock(&self.listed)
            .clone()
            .ok_or_else(|| "treehouse is not available".into())
    }

    fn scan(
        &self,
        path: &Path,
        _project: &tiller_core::domain::Project,
        merged: &[String],
    ) -> WorktreeScan {
        let mut scan = lock(&self.scans)
            .get(path)
            .cloned()
            .unwrap_or_else(|| WorktreeScan::missing(path));
        if scan
            .head_sha
            .as_ref()
            .is_some_and(|head| merged.contains(head))
        {
            scan.unlanded_commits = Some(0);
        }
        scan
    }

    /// The pool takes the worktree back: the path is gone from the next
    /// scan and from the list.
    fn release(&self, path: &Path) -> Result<(), String> {
        lock(&self.scans).remove(path);
        if let Some(listed) = lock(&self.listed).as_mut() {
            listed.retain(|p| p != path);
        }
        lock(&self.released).push(path.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct ForgeWorld {
    prs: HashMap<(String, i64), Result<PrRead, String>>,
    default_heads: HashMap<String, String>,
    ancestry: HashSet<(String, String)>,
    down: bool,
}

/// Forge answers the tests set: PR reads by repo and number, default branch
/// heads by project, and which SHAs descend from which.
#[derive(Clone, Default)]
pub struct FakeForge {
    world: Arc<Mutex<ForgeWorld>>,
}

impl FakeForge {
    pub fn pr(&self, repo: &str, number: i64, read: PrRead) {
        lock(&self.world)
            .prs
            .insert((repo.to_owned(), number), Ok(read));
    }

    pub fn pr_error(&self, repo: &str, number: i64, error: &str) {
        lock(&self.world)
            .prs
            .insert((repo.to_owned(), number), Err(error.to_owned()));
    }

    pub fn default_head(&self, project: &str, head: &str) {
        lock(&self.world)
            .default_heads
            .insert(project.to_owned(), head.to_owned());
    }

    /// `descendant` descends from `ancestor` (transitively, as given).
    pub fn descends(&self, ancestor: &str, descendant: &str) {
        lock(&self.world)
            .ancestry
            .insert((ancestor.to_owned(), descendant.to_owned()));
    }

    pub fn set_down(&self, down: bool) {
        lock(&self.world).down = down;
    }
}

impl Forge for FakeForge {
    fn pr(&self, repo: &str, number: i64) -> Result<PrRead, String> {
        let w = lock(&self.world);
        if w.down {
            return Err("forge unreachable".into());
        }
        w.prs
            .get(&(repo.to_owned(), number))
            .cloned()
            .unwrap_or_else(|| Err(format!("no fake answer for {repo}#{number}")))
    }

    fn default_branch_head(
        &self,
        project: &tiller_core::domain::Project,
    ) -> Result<String, String> {
        let w = lock(&self.world);
        if w.down {
            return Err("remote unreachable".into());
        }
        w.default_heads
            .get(&project.id)
            .cloned()
            .ok_or_else(|| format!("no default head for {}", project.id))
    }

    fn is_ancestor(
        &self,
        _project: &tiller_core::domain::Project,
        ancestor: &str,
        descendant: &str,
    ) -> Result<bool, String> {
        let w = lock(&self.world);
        if w.down {
            return Err("remote unreachable".into());
        }
        Ok(ancestor == descendant
            || w.ancestry
                .contains(&(ancestor.to_owned(), descendant.to_owned())))
    }
}

/// One launch the fake made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchCall {
    pub session_id: String,
    pub cwd: PathBuf,
    pub resume: Option<String>,
    pub pane_id: String,
}

#[derive(Default)]
struct LauncherWorld {
    launches: Vec<LaunchCall>,
    closed: Vec<String>,
    notifications: Vec<(String, String)>,
    fail_next: Option<String>,
    /// The screen a launched pane shows.
    screen: Option<Screen>,
}

/// A launcher over the fake herdr: each launch opens a workspace whose pane
/// runs Claude (idle, by default) and names the session in its tokens.
#[derive(Clone)]
pub struct FakeLauncher {
    herdr: FakeHerdr,
    world: Arc<Mutex<LauncherWorld>>,
}

impl FakeLauncher {
    pub fn new(herdr: FakeHerdr) -> Self {
        Self {
            herdr,
            world: Arc::new(Mutex::new(LauncherWorld::default())),
        }
    }

    pub fn launches(&self) -> Vec<LaunchCall> {
        lock(&self.world).launches.clone()
    }

    pub fn closed(&self) -> Vec<String> {
        lock(&self.world).closed.clone()
    }

    pub fn notifications(&self) -> Vec<(String, String)> {
        lock(&self.world).notifications.clone()
    }

    /// The next launch fails with `error`.
    pub fn fail_next(&self, error: &str) {
        lock(&self.world).fail_next = Some(error.to_owned());
    }

    /// What a launched pane shows (default idle Claude; `Permission` for a
    /// trust prompt; `Shell` for a launch that died at once).
    pub fn launched_screen(&self, screen: Screen) {
        lock(&self.world).screen = Some(screen);
    }
}

impl Launcher for FakeLauncher {
    fn launch(
        &mut self,
        session: &Session,
        cwd: &Path,
        resume: Option<&str>,
    ) -> Result<Launched, String> {
        if let Some(error) = lock(&self.world).fail_next.take() {
            return Err(error);
        }
        let n = lock(&self.world).launches.len() + 1;
        let workspace = format!("launch{n}");
        let pane_id = format!("{workspace}:p1");
        let screen = lock(&self.world).screen.unwrap_or(Screen::Idle);
        self.herdr
            .add_workspace(&workspace, &format!("{} resuming", session.project_id));
        self.herdr.add_pane(&workspace, &pane_id, screen);
        self.herdr.set_tokens(
            &pane_id,
            BTreeMap::from([
                ("session".to_owned(), session.id.clone()),
                ("project".to_owned(), session.project_id.clone()),
            ]),
        );
        lock(&self.world).launches.push(LaunchCall {
            session_id: session.id.clone(),
            cwd: cwd.to_owned(),
            resume: resume.map(str::to_owned),
            pane_id: pane_id.clone(),
        });
        Ok(Launched { pane_id })
    }

    fn close_pane(&mut self, pane_id: &str) -> Result<(), String> {
        self.herdr.remove_pane(pane_id);
        lock(&self.world).closed.push(pane_id.to_owned());
        Ok(())
    }

    fn notify(&mut self, title: &str, body: &str) {
        lock(&self.world)
            .notifications
            .push((title.to_owned(), body.to_owned()));
    }
}

#[derive(Clone, Default)]
pub struct FakeReadings {
    readings: Arc<Mutex<HostReadings>>,
}

impl FakeReadings {
    pub fn set(&self, readings: HostReadings) {
        *lock(&self.readings) = readings;
    }
}

impl Readings for FakeReadings {
    fn read(&self) -> HostReadings {
        lock(&self.readings).clone()
    }
}

#[derive(Clone, Default)]
pub struct FakeHarness {
    cuts: Arc<Mutex<HashMap<String, Cut>>>,
}

impl FakeHarness {
    pub fn cut(&self, run_id: &str, cut: Cut) {
        lock(&self.cuts).insert(run_id.to_owned(), cut);
    }
}

impl Harness for FakeHarness {
    fn cut(&self, run: &Run) -> Option<Cut> {
        lock(&self.cuts).get(&run.id).cloned()
    }
}

/// A fleet, a fake herdr it is laid out in, every fake source, and the
/// engine over them.
pub struct World {
    pub fleet: Fleet,
    pub herdr: FakeHerdr,
    pub probe: FakeProbe,
    pub scanner: FakeScanner,
    pub forge: FakeForge,
    pub launcher: FakeLauncher,
    pub readings: FakeReadings,
    pub harness: FakeHarness,
    pub config: Config,
    reconciler: Option<Reconciler>,
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

impl World {
    /// Project `alpha` with a live orchestrator in `w1:p1`; no workers yet.
    pub fn new() -> Self {
        let fleet = Fleet::new();
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        let home = fleet.dir.path().join("tiller-home");
        std::fs::create_dir_all(&home).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut config = Config::new(fleet.host.clone(), &home);
        config
            .project_paths
            .insert("alpha".into(), fleet.dir.path().join("alpha-checkout"));
        std::fs::create_dir_all(fleet.dir.path().join("alpha-checkout")).unwrap();
        Self {
            launcher: FakeLauncher::new(herdr.clone()),
            herdr,
            probe: FakeProbe::default(),
            scanner: FakeScanner::default(),
            forge: FakeForge::default(),
            readings: FakeReadings::default(),
            harness: FakeHarness::default(),
            config,
            fleet,
            reconciler: None,
        }
    }

    /// A builder run bound to `pane_id`, with a clean worktree scan on its
    /// branch. Returns the session id.
    pub fn worker(&mut self, pane_id: &str) -> String {
        let session = self.fleet.worker(pane_id);
        let run = self.run_of(&session);
        let path = run.worktree_path.clone().unwrap();
        self.scanner
            .worktree(&path, "fm/work", "aaaa1111", false, Some(0));
        session
    }

    /// Lays the fleet out in the fake herdr (orchestrator, an unrelated
    /// workspace, the workers) with every Claude pane idle and tokened.
    pub fn place(&self) {
        self.fleet.place(&self.herdr);
        let bindings =
            tiller_herdr::layout::bindings(self.fleet.store.conn(), &self.fleet.host).unwrap();
        for binding in bindings {
            self.herdr.set_tokens(&binding.pane_id, binding.tokens());
        }
    }

    /// The engine, built on first use from the config as it stands.
    pub fn reconciler(&mut self) -> &mut Reconciler {
        if self.reconciler.is_none() {
            let sources = Sources {
                herdr: Box::new(self.herdr.clone()),
                probe: Box::new(self.probe.clone()),
                scanner: Box::new(self.scanner.clone()),
                forge: Box::new(self.forge.clone()),
                launcher: Box::new(self.launcher.clone()),
                readings: Box::new(self.readings.clone()),
                harness: Box::new(self.harness.clone()),
            };
            self.reconciler = Some(Reconciler::new(
                self.fleet.open(),
                self.config.clone(),
                sources,
            ));
        }
        self.reconciler.as_mut().unwrap()
    }

    /// Drops the engine, as a daemon restart does: its memory is gone, the
    /// records remain.
    pub fn restart(&mut self) {
        self.reconciler = None;
    }

    pub fn pass(&mut self, now: Timestamp) -> tiller_reconcile::Report {
        self.reconciler().pass(now).unwrap()
    }

    pub fn store(&self) -> &Store {
        &self.fleet.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.fleet.store
    }

    pub fn session(&self, id: &str) -> Session {
        get(self.store().conn(), "session", id).unwrap()
    }

    pub fn run(&self, id: &str) -> Run {
        get(self.store().conn(), "run", id).unwrap()
    }

    pub fn run_of(&self, session: &str) -> Run {
        let session = self.session(session);
        self.run(session.run_id.as_deref().unwrap())
    }

    /// Records a hook observation for `session` observed at `at`.
    pub fn observe(&mut self, session: &str, verdict: Activity, at: Timestamp) {
        let run_id = self.session(session).run_id;
        self.store_mut()
            .observation_record(
                &Ctx::at(Actor::Hook, at),
                NewObservation {
                    session_id: Some(session.to_owned()),
                    run_id,
                    source: ObservationSource::Hook,
                    event: format!("test:{verdict}"),
                    verdict: Some(verdict),
                    observed_at: at,
                    payload: json!({}),
                },
            )
            .unwrap();
    }

    /// A tool hook, with the tool's name, as `PreToolUse` records it.
    pub fn tool_hook(&mut self, session: &str, tool: &str, at: Timestamp) {
        let run_id = self.session(session).run_id;
        self.store_mut()
            .observation_record(
                &Ctx::at(Actor::Hook, at),
                NewObservation {
                    session_id: Some(session.to_owned()),
                    run_id,
                    source: ObservationSource::Hook,
                    event: "PreToolUse".into(),
                    verdict: Some(Activity::Working),
                    observed_at: at,
                    payload: json!({"tool_name": tool}),
                },
            )
            .unwrap();
    }

    /// Open attention items on a card, oldest first.
    pub fn open_items(&self, card_id: &str) -> Vec<Attention> {
        list(
            self.store().conn(),
            "card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL ORDER BY opened_at, id",
            &[&card_id],
        )
        .unwrap()
    }

    /// Every attention item, oldest first.
    pub fn items(&self) -> Vec<Attention> {
        list(self.store().conn(), "1 = 1 ORDER BY opened_at, id", &[]).unwrap()
    }

    /// Messages to `session`, oldest first.
    pub fn messages_to(&self, session: &str) -> Vec<OutboxMessage> {
        list(
            self.store().conn(),
            "target_session_id = ?1 ORDER BY created_at, id",
            &[&session],
        )
        .unwrap()
    }

    /// Notes to the orchestrator, oldest first.
    pub fn notes(&self) -> Vec<OutboxMessage> {
        self.messages_to(&self.fleet.orchestrator_session.clone())
            .into_iter()
            .filter(|m| m.kind == tiller_core::domain::OutboxKind::Note)
            .collect()
    }

    /// Event kinds written so far, in order.
    pub fn event_kinds(&self) -> Vec<String> {
        let mut stmt = self
            .store()
            .conn()
            .prepare("SELECT kind FROM events ORDER BY seq")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub fn count(&self, sql: &str) -> i64 {
        self.store()
            .conn()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    /// The `fleet-1` card id.
    pub fn health_card(&self) -> String {
        self.store()
            .conn()
            .query_row(
                "SELECT id FROM cards WHERE display_id = 'fleet-1'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}
