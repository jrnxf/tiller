//! Fakes for every source and a small world to run passes against (feature
//! `testing`): the test fleet from `tiller_panes::testing`, a fake terminal
//! holder link, and in-memory probes, scans, forge answers, launches,
//! readings and harness records that tests set between passes.
//!
//! This file is also compiled into the crate's own integration tests through
//! `tests/common/mod.rs`, so it names the crate as `tiller_reconcile`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::json;
use tiller_core::domain::{Activity, Attention, ObservationSource, OutboxMessage, Run, Session};
use tiller_core::state::runs::NewObservation;
use tiller_core::state::{get, list};
use tiller_core::{Actor, Ctx, Store, Timestamp};
pub use tiller_panes::fake_claude::Screen;
use tiller_panes::handshake::Welcome;
pub use tiller_panes::message::Meta;
use tiller_panes::message::{
    Exit, Format, Listing, Pane, PaneId, Read, Screen as PaneScreen, Signal, Spawn,
};
pub use tiller_panes::testing::{Fleet, owner};
use tiller_panes::{
    Events, FakePanes, Key, Live, Panes, PanesError, Subscribe, Subscription, Upgrade,
};

use tiller_reconcile::holder::Link;
use tiller_reconcile::pass::{Config, Reconciler, Sources};
use tiller_reconcile::sources::{
    AgentProbe, Cut, Forge, Harness, HolderLauncher, HostReadings, Launched, Launcher, PrRead,
    Prepare, Probe, Readings, Scanner, WorktreeScan,
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

/// A probe that answers from the process table unless a pid is set.
#[derive(Clone, Default)]
pub struct FakeProbe {
    pids: Arc<Mutex<HashMap<u32, Probe>>>,
}

impl FakeProbe {
    /// What `kill(pid, 0)` says of `pid`. A fake holder's pids are above
    /// any real one, so unset they read as dead.
    pub fn set_pid(&self, pid: u32, probe: Probe) {
        lock(&self.pids).insert(pid, probe);
    }
}

impl AgentProbe for FakeProbe {
    fn process(&self, pid: u32) -> Probe {
        lock(&self.pids)
            .get(&pid)
            .copied()
            .unwrap_or_else(|| tiller_observe::liveness::process_alive(pid))
    }
}

#[derive(Default)]
struct LinkWorld {
    panes: FakePanes,
    down: bool,
    version: String,
    next_version: String,
    boot_id: String,
    binary: Option<PathBuf>,
    refuse: Option<String>,
    upgrades: Vec<PathBuf>,
    welcomes: usize,
    started_at: Option<Timestamp>,
}

/// The daemon's link to a fake terminal holder: a [`FakePanes`] that can go
/// down, restart as a new instance, and take or refuse an upgrade.
#[derive(Clone)]
pub struct FakeLink {
    world: Arc<Mutex<LinkWorld>>,
}

impl Default for FakeLink {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeLink {
    /// A holder at version `0.1.0` on boot `boot-1`, running `/home/user/old/tiller`.
    pub fn new() -> Self {
        Self {
            world: Arc::new(Mutex::new(LinkWorld {
                version: "0.1.0".into(),
                next_version: "0.2.0".into(),
                boot_id: "boot-1".into(),
                binary: Some(PathBuf::from("/home/user/old/tiller")),
                started_at: Some(now().plus_minutes(-24 * 60)),
                ..LinkWorld::default()
            })),
        }
    }

    /// The holder instance in use now.
    pub fn panes(&self) -> FakePanes {
        lock(&self.world).panes.clone()
    }

    pub fn instance(&self) -> String {
        self.panes().instance()
    }

    pub fn version(&self) -> String {
        lock(&self.world).version.clone()
    }

    /// The holder stops answering (true) or answers again (false). Going
    /// down fails every watch and subscription.
    pub fn set_down(&self, down: bool) {
        let mut world = lock(&self.world);
        world.down = down;
        world.panes.set_unreachable(down);
    }

    /// The holder died and started again from nothing: a new instance with
    /// no panes. Returns it.
    pub fn restart(&self) -> FakePanes {
        let mut world = lock(&self.world);
        world.panes.set_unreachable(true);
        world.panes = FakePanes::new();
        world.down = false;
        world.started_at = Some(now());
        world.panes.clone()
    }

    /// The binary the holder process runs, as `/proc/<pid>/exe` reads.
    pub fn set_binary(&self, binary: Option<&Path>) {
        lock(&self.world).binary = binary.map(Path::to_owned);
    }

    /// The version a taken upgrade moves to. A rebuild to a new store path
    /// can keep the version it had.
    pub fn set_next_version(&self, version: &str) {
        lock(&self.world).next_version = version.to_owned();
    }

    /// Every `upgrade` refuses with `reason` (Some), or takes it (None).
    pub fn refuse_upgrades(&self, reason: Option<&str>) {
        lock(&self.world).refuse = reason.map(str::to_owned);
    }

    /// The binaries every `upgrade` named, oldest first.
    pub fn upgrades(&self) -> Vec<PathBuf> {
        lock(&self.world).upgrades.clone()
    }

    /// How many `welcome`s the link answered.
    pub fn welcomes(&self) -> usize {
        lock(&self.world).welcomes
    }
}

impl Panes for FakeLink {
    fn spawn(&self, req: &Spawn) -> tiller_panes::Result<Pane> {
        self.panes().spawn(req)
    }

    fn list(&self) -> tiller_panes::Result<Listing> {
        self.panes().list()
    }

    fn get(&self, pane: &PaneId) -> tiller_panes::Result<Option<Pane>> {
        self.panes().get(pane)
    }

    fn screen(
        &self,
        pane: &PaneId,
        read: Read,
        format: Format,
    ) -> tiller_panes::Result<PaneScreen> {
        self.panes().screen(pane, read, format)
    }

    fn input(&self, pane: &PaneId, text: &str) -> tiller_panes::Result<()> {
        self.panes().input(pane, text)
    }

    fn paste(&self, pane: &PaneId, text: &str) -> tiller_panes::Result<()> {
        self.panes().paste(pane, text)
    }

    fn keys(&self, pane: &PaneId, keys: &[Key]) -> tiller_panes::Result<()> {
        self.panes().keys(pane, keys)
    }

    fn set_meta(&self, pane: &PaneId, set: &Meta, remove: &[&str]) -> tiller_panes::Result<Pane> {
        self.panes().set_meta(pane, set, remove)
    }

    fn signal(&self, pane: &PaneId, signal: Signal) -> tiller_panes::Result<()> {
        self.panes().signal(pane, signal)
    }

    fn close(&self, pane: &PaneId, grace: Duration) -> tiller_panes::Result<Exit> {
        self.panes().close(pane, grace)
    }

    fn forget(&self, pane: &PaneId) -> tiller_panes::Result<()> {
        self.panes().forget(pane)
    }

    fn switch(&self, pane: &PaneId) -> tiller_panes::Result<usize> {
        self.panes().switch(pane)
    }
}

impl Live for FakeLink {
    fn subscribe(&self, pane: &PaneId, opts: Subscribe) -> tiller_panes::Result<Subscription> {
        self.panes().subscribe(pane, opts)
    }

    fn watch(&self) -> tiller_panes::Result<Events> {
        self.panes().watch()
    }
}

impl Link for FakeLink {
    fn welcome(&self) -> tiller_panes::Result<Welcome> {
        let mut world = lock(&self.world);
        if world.down {
            return Err(PanesError::Unreachable("the fake holder is down".into()));
        }
        world.welcomes += 1;
        Ok(Welcome {
            proto: 1,
            holder: world.version.clone(),
            instance: world.panes.instance(),
            boot_id: world.boot_id.clone(),
            started_at: world.started_at.unwrap_or_else(now).as_jiff(),
            features: Vec::new(),
        })
    }

    fn running_binary(&self) -> Option<PathBuf> {
        lock(&self.world).binary.clone()
    }

    /// A taken upgrade keeps the instance and its panes, moves to the next
    /// version and binary, and ends every watch with `upgraded`, as the
    /// re-exec does.
    fn upgrade(&self, binary: &Path) -> tiller_panes::Result<Upgrade> {
        let mut world = lock(&self.world);
        if world.down {
            return Err(PanesError::Unreachable("the fake holder is down".into()));
        }
        world.upgrades.push(binary.to_owned());
        if let Some(reason) = world.refuse.clone() {
            return Ok(Upgrade::Refused(reason));
        }
        world.version = world.next_version.clone();
        world.binary = Some(binary.to_owned());
        world.panes.emit(&tiller_panes::message::Event::Upgraded {
            holder: world.version.clone(),
        });
        Ok(Upgrade::Upgrading)
    }
}

/// Scans the tests set per path; an unset path is missing. A head among the
/// merged heads the caller passes has nothing unlanded, as `git rev-list
/// HEAD --not <head>` finds nothing.
#[derive(Clone, Default)]
pub struct FakeScanner {
    scans: Arc<Mutex<HashMap<PathBuf, WorktreeScan>>>,
    listed: Arc<Mutex<Option<Vec<PathBuf>>>>,
    released: Arc<Mutex<Vec<(PathBuf, String)>>>,
    refusal: Arc<Mutex<Option<String>>>,
}

impl FakeScanner {
    /// Paths returned to the pool, in order.
    pub fn released(&self) -> Vec<PathBuf> {
        lock(&self.released)
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// The lease holder each return named, in order.
    pub fn release_holders(&self) -> Vec<String> {
        lock(&self.released)
            .iter()
            .map(|(_, holder)| holder.clone())
            .collect()
    }

    /// The pool refuses every return with `error`.
    pub fn refuse_returns(&self, error: &str) {
        *lock(&self.refusal) = Some(error.to_owned());
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

    /// What the worktree list shows; `None` makes the list unreadable.
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
    fn release(&self, path: &Path, holder: &str) -> Result<(), String> {
        if let Some(error) = lock(&self.refusal).clone() {
            return Err(error);
        }
        lock(&self.scans).remove(path);
        if let Some(listed) = lock(&self.listed).as_mut() {
            listed.retain(|p| p != path);
        }
        lock(&self.released).push((path.to_owned(), holder.to_owned()));
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

/// The script a fake launch runs for `session`.
pub fn launch_script(session: &str) -> PathBuf {
    PathBuf::from(format!("/home/user/runs/resume-{session}.sh"))
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
    script_next: Option<Prepare>,
    /// The `--resume` id of each script written and not yet run, by session.
    prepared: BTreeMap<String, Option<String>>,
    /// The screen a launched pane shows.
    screen: Option<Screen>,
}

/// A launcher over the fake holder: each launch is the daemon's holder
/// `spawn`, keyed by the session, its pane showing Claude idle by default.
#[derive(Clone)]
pub struct FakeLauncher {
    holder: FakeLink,
    world: Arc<Mutex<LauncherWorld>>,
}

impl FakeLauncher {
    pub fn new(holder: FakeLink) -> Self {
        Self {
            holder,
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

    /// The next script is refused with `why`, as for a session whose launch
    /// files are gone.
    pub fn refuse_next(&self, why: &str) {
        lock(&self.world).script_next = Some(Prepare::Refused(why.to_owned()));
    }

    /// The next script fails with `error`, as for a `config.toml` that
    /// does not parse, which a later pass may find fixed.
    pub fn fail_script_next(&self, error: &str) {
        lock(&self.world).script_next = Some(Prepare::Failed(error.to_owned()));
    }

    /// What a launched pane shows (default idle Claude; `Permission` for a
    /// trust prompt).
    pub fn launched_screen(&self, screen: Screen) {
        lock(&self.world).screen = Some(screen);
    }
}

impl Launcher for FakeLauncher {
    fn prepare(
        &mut self,
        session: &Session,
        _cwd: &Path,
        resume: Option<&str>,
    ) -> Result<PathBuf, Prepare> {
        let mut world = lock(&self.world);
        if let Some(error) = world.script_next.take() {
            return Err(error);
        }
        world
            .prepared
            .insert(session.id.clone(), resume.map(str::to_owned));
        Ok(launch_script(&session.id))
    }

    fn launch(&mut self, session: &Session, cwd: &Path, script: &Path) -> Result<Launched, String> {
        let resume = {
            let mut world = lock(&self.world);
            if let Some(error) = world.fail_next.take() {
                return Err(error);
            }
            world.prepared.remove(&session.id).flatten()
        };
        let pane = self
            .holder
            .spawn(&HolderLauncher::request(session, cwd, script))
            .map_err(|e| e.to_string())?
            .pane;
        let screen = lock(&self.world).screen.unwrap_or(Screen::Idle);
        show(&self.holder.panes(), &pane, screen);
        let pane_id = pane.as_str().to_owned();
        lock(&self.world).launches.push(LaunchCall {
            session_id: session.id.clone(),
            cwd: cwd.to_owned(),
            resume,
            pane_id: pane_id.clone(),
        });
        Ok(Launched { pane_id })
    }

    fn close_pane(&mut self, pane_id: &str) -> Result<(), String> {
        let pane = PaneId::from(pane_id);
        let _ = self.holder.close(&pane, Duration::ZERO);
        let _ = self.holder.forget(&pane);
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

/// Draws `screen` on `pane`, as its Claude shows it.
fn show(panes: &FakePanes, pane: &PaneId, screen: Screen) {
    let ansi = screen.ansi();
    let rows: Vec<&str> = ansi.lines().collect();
    panes.set_screen(pane, &rows);
}

/// A fleet, the fake terminal holder it is laid out in, every fake source,
/// and the engine over them.
pub struct World {
    pub fleet: Fleet,
    /// The terminal holder: reconcile lists it, and resumes spawn in it.
    pub holder: FakeLink,
    pub probe: FakeProbe,
    pub scanner: FakeScanner,
    pub forge: FakeForge,
    pub launcher: FakeLauncher,
    pub readings: FakeReadings,
    pub harness: FakeHarness,
    pub config: Config,
    forge_override: Option<Box<dyn Forge>>,
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
        let holder = FakeLink::new();
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
            launcher: FakeLauncher::new(holder.clone()),
            holder,
            probe: FakeProbe::default(),
            scanner: FakeScanner::default(),
            forge: FakeForge::default(),
            readings: FakeReadings::default(),
            harness: FakeHarness::default(),
            config,
            fleet,
            forge_override: None,
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

    /// Records the holder's instance in `meta.holder_instance`, as the
    /// holder source's first `welcome` does.
    pub fn holder_seen(&mut self) {
        let welcome = self.holder.welcome().unwrap();
        self.store_mut()
            .holder_record(
                &Ctx::new(Actor::Daemon),
                &tiller_core::state::caches::HolderSeen {
                    instance: welcome.instance,
                    version: welcome.holder,
                    boot_id: welcome.boot_id,
                    previous_version: None,
                },
            )
            .unwrap();
    }

    /// A builder run whose session runs Claude in a new pane of the
    /// holder, keyed by the session as a spawn keys it. Returns the
    /// session and its pane.
    pub fn holder_worker(&mut self) -> (String, PaneId) {
        let n = self.fleet.workers.len() + 2;
        let session = self.worker(&format!("w{n}:p1"));
        // Its pane is the spawn's, never one [`World::place`] lays out.
        self.fleet.workers.retain(|(s, _, _)| *s != session);
        let record = self.session(&session);
        let pane = self
            .holder
            .spawn(&HolderLauncher::request(
                &record,
                Path::new("/home/user/worktrees"),
                &launch_script(&session),
            ))
            .unwrap();
        self.fleet.bind(&session, pane.pane.as_str());
        (session, pane.pane)
    }

    /// Lays the fleet out in the holder as a holder that already runs it
    /// lists it: the orchestrator's pane, an exited pane of no session
    /// (`w3:p1`), then each worker's, every Claude idle with a live child
    /// and the pane's meta naming its session, project and run, as a spawn
    /// names them. The holder's instance is the one on record.
    pub fn place(&mut self) {
        let mut bound = vec![(
            self.fleet.orchestrator_pane.clone(),
            self.fleet.orchestrator_session.clone(),
        )];
        bound.extend(
            self.fleet
                .workers
                .iter()
                .map(|(session, _, pane)| (pane.clone(), session.clone())),
        );
        for (pane, session) in &bound {
            let record = self.session(session);
            let meta =
                HolderLauncher::request(&record, Path::new("/home/user"), &launch_script(session))
                    .meta;
            self.add_pane(pane, session, meta, Screen::Idle);
        }
        let notes = self.add_pane("w3:p1", "notes", Meta::new(), Screen::Idle);
        self.holder.panes().exit(&notes, Some(0), None);
        self.holder_seen();
    }

    /// Puts a running pane `pane` keyed `key` in the holder, its child live
    /// and its Claude showing `screen`.
    pub fn add_pane(&self, pane: &str, key: &str, meta: Meta, screen: Screen) -> PaneId {
        let panes = self.holder.panes();
        let id = PaneId::from(pane);
        let record = panes.add(&id, key, meta);
        self.probe.set_pid(record.pid, Probe::Alive);
        show(&panes, &id, screen);
        id
    }

    /// What `pane`'s Claude shows from now on.
    pub fn set_screen(&self, pane: &str, screen: Screen) {
        show(&self.holder.panes(), &PaneId::from(pane), screen);
    }

    /// `pane`'s child exits, leaving the shell's last screen.
    pub fn exit_pane(&self, pane: &str) {
        let pane = PaneId::from(pane);
        let panes = self.holder.panes();
        if let Some(record) = panes.get(&pane).unwrap() {
            self.probe.set_pid(record.pid, Probe::Dead);
        }
        assert!(panes.exit(&pane, Some(0), None), "{pane} is not running");
    }

    /// Sets `set` on `pane`'s metadata and drops the keys in `remove`.
    pub fn set_meta(&self, pane: &str, set: &[(&str, &str)], remove: &[&str]) {
        let set = set
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Panes::set_meta(&self.holder.panes(), &PaneId::from(pane), &set, remove).unwrap();
    }

    /// The holder drops `pane` from its list, and its child is gone with it.
    pub fn remove_pane(&self, pane: &str) {
        let pane = PaneId::from(pane);
        let panes = self.holder.panes();
        if let Some(record) = panes.get(&pane).unwrap() {
            self.probe.set_pid(record.pid, Probe::Dead);
        }
        panes.remove(&pane);
    }

    /// The holder's record of `pane`.
    pub fn pane(&self, pane: &str) -> Pane {
        self.holder
            .panes()
            .get(&PaneId::from(pane))
            .unwrap()
            .unwrap()
    }

    /// The engine, built on first use from the config as it stands.
    pub fn reconciler(&mut self) -> &mut Reconciler {
        if self.reconciler.is_none() {
            let sources = Sources {
                holder: Arc::new(self.holder.clone()) as Arc<dyn Link>,
                probe: Box::new(self.probe.clone()),
                scanner: Box::new(self.scanner.clone()),
                forge: self
                    .forge_override
                    .take()
                    .unwrap_or_else(|| Box::new(self.forge.clone())),
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

    /// Reads the forge through `forge` instead of the fake, from the next
    /// engine on.
    pub fn use_forge(&mut self, forge: Box<dyn Forge>) {
        self.forge_override = Some(forge);
        self.reconciler = None;
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
