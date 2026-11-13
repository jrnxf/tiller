//! Spawn, `tiller done` and cleanup against real git repositories, a fake
//! treehouse pool, the fake herdr and a file-backed `state.db`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier, Mutex};

use tempfile::TempDir;
use tiller_core::domain::{
    AnsweredVia, Artifact, ArtifactKind, Attention, CardKind, Hold, HoldReleaseReason, Pr, PrState,
    Size,
};
use tiller_core::state::caches::QuotaReading;
use tiller_core::state::cards::NewCard;
use tiller_core::state::records::NewArtifact;
use tiller_core::{Actor, Ctx, Timestamp};
use tiller_herdr::fake::{FakeHerdr, Screen};
use tiller_herdr::testing::{Fleet, owner};
use tiller_reconcile::git::GitScanner;

use super::listeners::NoListeners;
use super::*;

pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A bare origin, a clone that pushes to it as other people do, and the
/// project checkout treehouse cuts worktrees from.
pub(crate) struct Repo {
    root: TempDir,
    origin: PathBuf,
    upstream: PathBuf,
    checkout: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin.git");
        git(
            root.path(),
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        git(root.path(), &["clone", "--quiet", "origin.git", "upstream"]);
        let upstream = root.path().join("upstream");
        git(&upstream, &["checkout", "--quiet", "-b", "main"]);
        std::fs::write(upstream.join("README.md"), "alpha\n").unwrap();
        git(&upstream, &["add", "README.md"]);
        git(&upstream, &["commit", "--quiet", "-m", "first"]);
        git(&upstream, &["push", "--quiet", "origin", "main"]);
        git(root.path(), &["clone", "--quiet", "origin.git", "checkout"]);
        let checkout = root.path().join("checkout");
        Self {
            root,
            origin,
            upstream,
            checkout,
        }
    }

    /// Lands a commit on `origin/main` as someone else would; returns it.
    fn advance(&self, file: &str) -> String {
        std::fs::write(self.upstream.join(file), "more\n").unwrap();
        git(&self.upstream, &["add", file]);
        git(&self.upstream, &["commit", "--quiet", "-m", file]);
        git(&self.upstream, &["push", "--quiet", "origin", "main"]);
        git(&self.upstream, &["rev-parse", "HEAD"])
    }
}

#[derive(Default)]
struct PoolLog {
    next: usize,
    /// `(path, branch, base, holder)` per take.
    taken: Vec<(PathBuf, String, String, String)>,
    /// `(path, holder)` per return.
    returned: Vec<(PathBuf, String)>,
}

/// A review server that records the pages it was asked to end, and refuses
/// them all while `refuse` is set.
#[derive(Clone, Default)]
pub(crate) struct FakeListeners {
    ended: Arc<Mutex<Vec<PathBuf>>>,
    refuse: Arc<Mutex<bool>>,
}

impl FakeListeners {
    pub(crate) fn ended(&self) -> Vec<PathBuf> {
        self.ended.lock().unwrap().clone()
    }

    pub(crate) fn refuse(&self, refuse: bool) {
        *self.refuse.lock().unwrap() = refuse;
    }
}

impl Listeners for FakeListeners {
    fn end(&self, page: &Path) -> Result<(), String> {
        if *self.refuse.lock().unwrap() {
            return Err("the review server is not answering".into());
        }
        self.ended.lock().unwrap().push(page.to_owned());
        Ok(())
    }
}

/// A treehouse that cuts each worktree from the checkout's local base
/// without fetching, as a pool whose refs are stale does, so spawn's own
/// fetch is what brings it to the head of origin.
#[derive(Clone)]
pub(crate) struct FakePool {
    slots: PathBuf,
    origin: PathBuf,
    log: Arc<Mutex<PoolLog>>,
}

impl FakePool {
    pub(crate) fn taken(&self) -> Vec<(PathBuf, String, String, String)> {
        self.log.lock().unwrap().taken.clone()
    }

    pub(crate) fn returned(&self) -> Vec<(PathBuf, String)> {
        self.log.lock().unwrap().returned.clone()
    }
}

impl Pool for FakePool {
    fn take(
        &self,
        checkout: &Path,
        branch: &str,
        base: &str,
        holder: &str,
    ) -> Result<PathBuf, String> {
        let slot = {
            let mut log = self.log.lock().unwrap();
            log.next += 1;
            self.slots.join(format!("slot-{}", log.next))
        };
        let run = |dir: &Path, args: &[&str]| -> Result<(), String> {
            let out = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .map_err(|e| e.to_string())?;
            out.status
                .success()
                .then_some(())
                .ok_or_else(|| String::from_utf8_lossy(&out.stderr).into_owned())
        };
        std::fs::create_dir_all(&self.slots).map_err(|e| e.to_string())?;
        run(
            &self.slots,
            &[
                "clone",
                "--quiet",
                "--branch",
                base,
                &checkout.to_string_lossy(),
                &slot.to_string_lossy(),
            ],
        )?;
        run(
            &slot,
            &[
                "remote",
                "set-url",
                "origin",
                &self.origin.to_string_lossy(),
            ],
        )?;
        run(&slot, &["checkout", "--quiet", "-b", branch])?;
        self.log.lock().unwrap().taken.push((
            slot.clone(),
            branch.into(),
            base.into(),
            holder.into(),
        ));
        Ok(slot)
    }

    fn give_back(&self, path: &Path, holder: &str) -> Result<(), String> {
        let mut log = self.log.lock().unwrap();
        let leased = log
            .taken
            .iter()
            .any(|(p, _, _, h)| p == path && h == holder);
        if !leased {
            return Err(format!("{} is not leased to {holder}", path.display()));
        }
        log.returned.push((path.into(), holder.into()));
        Ok(())
    }
}

fn config(checkout: &Path, extra: &str) -> SpawnConfig {
    let text = format!(
        "[checkouts]\nalpha = {:?}\n\n[quota]\nenabled = false\n{extra}",
        checkout.to_string_lossy()
    );
    SpawnConfig::from_table(&text.parse().unwrap()).unwrap()
}

/// Project `alpha` with a live orchestrator in pane `w1:p1`, its checkout,
/// a pool, a herdr laid out as a fresh server, and a scanner over git.
pub(crate) struct Rig {
    pub(crate) fleet: Fleet,
    pub(crate) repo: Repo,
    pub(crate) pool: FakePool,
    pub(crate) herdr: FakeHerdr,
    /// The terminal holder new sessions launch in; `None` launches them in
    /// herdr.
    pub(crate) panes: Option<tiller_panes::FakePanes>,
    pub(crate) scanner: GitScanner,
    pub(crate) listeners: FakeListeners,
    pub(crate) config: SpawnConfig,
    pub(crate) tiller: PathBuf,
    pub(crate) scratch: PathBuf,
    pub(crate) tiller_home: PathBuf,
    /// A Claude config dir of the rig's own, never the real one.
    pub(crate) claude: ClaudeConfig,
    pub(crate) ram_free_mb: Option<i64>,
    pub(crate) disk_free_mb: Option<i64>,
}

macro_rules! world {
    ($rig:expr) => {
        World {
            config: &$rig.config,
            pool: &$rig.pool,
            herdr: &$rig.herdr,
            panes: $rig
                .panes
                .as_ref()
                .map(|panes| panes as &dyn tiller_panes::Panes),
            scanner: &$rig.scanner,
            listeners: &$rig.listeners,
            tiller: &$rig.tiller,
            scratch_root: &$rig.scratch,
            tiller_home: &$rig.tiller_home,
            claude_config: &$rig.claude,
            ram_free_mb: $rig.ram_free_mb,
            disk_free_mb: $rig.disk_free_mb,
        }
    };
}
pub(crate) use world;

impl Rig {
    pub(crate) fn new() -> Self {
        Self::with_config("")
    }

    pub(crate) fn with_config(extra: &str) -> Self {
        let fleet = Fleet::new();
        let repo = Repo::new();
        let herdr = FakeHerdr::new(fleet.dir.path().join("herdr.json"));
        fleet.place(&herdr);
        let pool = FakePool {
            slots: repo.root.path().join("pool"),
            origin: repo.origin.clone(),
            log: Arc::default(),
        };
        Self {
            config: config(&repo.checkout, extra),
            tiller: PathBuf::from("/home/user/.nix-profile/bin/tiller"),
            scratch: fleet.dir.path().join("runs"),
            tiller_home: PathBuf::from("/data/tiller"),
            claude: ClaudeConfig::at(fleet.dir.path().join("claude")),
            fleet,
            repo,
            pool,
            herdr,
            panes: None,
            scanner: GitScanner::new(),
            listeners: FakeListeners::default(),
            ram_free_mb: Some(64_000),
            disk_free_mb: Some(64_000),
        }
    }

    pub(crate) fn orchestrator(&self) -> Ctx {
        Ctx::new(Actor::Session(self.fleet.orchestrator_session.clone()))
    }

    pub(crate) fn card(&mut self, kind: CardKind, size: Option<Size>) -> Card {
        self.fleet
            .store
            .card_create(
                &owner(),
                None,
                NewCard {
                    project_id: "alpha".into(),
                    kind,
                    size,
                    title: format!("a {kind} card"),
                    ..NewCard::default()
                },
                None,
            )
            .unwrap()
            .value
    }

    pub(crate) fn request(card: &Card, role: RunRole) -> Request {
        Request {
            card: card.display_id.clone(),
            role,
            asked: None,
            brief: Some(format!("Do the work on {}.", card.display_id)),
            harness: "claude".into(),
            past_quota_floor: false,
        }
    }

    fn try_spawn(&mut self, ctx: &Ctx, request: Request) -> Result<Outcome> {
        let world = world!(self);
        spawn(&mut self.fleet.store, ctx, None, &world, request).map(|a| a.value)
    }

    pub(crate) fn spawn(&mut self, card: &Card, role: RunRole) -> Spawned {
        let ctx = self.orchestrator();
        match self.try_spawn(&ctx, Self::request(card, role)).unwrap() {
            Outcome::Spawned(spawned) => *spawned,
            other => panic!("expected a new spawn, got {other:?}"),
        }
    }

    pub(crate) fn open_items(&self, card: &Card) -> Vec<Attention> {
        list(
            self.fleet.store.conn(),
            "card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL AND kind = 'decision'",
            &[&card.id],
        )
        .unwrap()
    }

    pub(crate) fn open_holds(&self, card: &Card) -> Vec<Hold> {
        list(
            self.fleet.store.conn(),
            "card_id = ?1 AND released_at IS NULL",
            &[&card.id],
        )
        .unwrap()
    }

    /// The owner answers the card's one open decision with `label`.
    pub(crate) fn owner_answers(&mut self, card: &Card, label: &str) {
        let open = self.open_items(card);
        assert_eq!(open.len(), 1, "{open:?}");
        self.fleet
            .store
            .attention_answer(&owner(), None, &open[0].id, label, AnsweredVia::Board)
            .unwrap();
    }

    pub(crate) fn run(&self, id: &str) -> Run {
        get(self.fleet.store.conn(), "run", id).unwrap()
    }

    pub(crate) fn done(&mut self, run: &str, outcome: RunOutcome) -> Result<Ended> {
        let run = self.run(run);
        done(
            &mut self.fleet.store,
            &owner(),
            &self.scanner,
            &run,
            outcome,
        )
        .map(|a| a.value)
    }

    pub(crate) fn cleanup(&mut self, run: &str) -> Result<Cleaned> {
        let run = self.run(run);
        let world = world!(self);
        cleanup(&mut self.fleet.store, &owner(), &world, &run).map(|a| a.value)
    }

    /// Whether the rig's Claude config records workspace trust for `dir`.
    pub(crate) fn trusted(&self, dir: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(self.claude.store().unwrap()) else {
            return false;
        };
        let config: Value = serde_json::from_str(&text).unwrap();
        let dir = std::fs::canonicalize(dir).unwrap();
        config["projects"][dir.to_string_lossy().as_ref()][trust::FLAG] == true
    }

    pub(crate) fn workspace_ids(&self) -> Vec<String> {
        self.herdr
            .snapshot()
            .unwrap()
            .workspaces
            .into_iter()
            .map(|w| w.workspace_id)
            .collect()
    }

    pub(crate) fn events(&self, kind: &str, entity: &str) -> i64 {
        self.fleet
            .store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = ?1 AND entity_id = ?2",
                rusqlite::params![kind, entity],
                |row| row.get(0),
            )
            .unwrap()
    }

    pub(crate) fn sql(&self, statement: &str) {
        self.fleet.store.conn().execute(statement, []).unwrap();
    }

    fn quota(&mut self, percent: f64) {
        self.quota_resetting(percent, None);
    }

    pub(crate) fn quota_resetting(&mut self, percent: f64, resets_at: Option<Timestamp>) {
        self.fleet
            .store
            .cache_quota(
                &Ctx::new(Actor::Daemon),
                vec![QuotaReading {
                    label: "claude-weekly".into(),
                    percent_remaining: Some(percent),
                    resets_at,
                    pace: None,
                    reserve: None,
                    burn_multiple: None,
                    elapsed_percent: None,
                    projected_exhaustion_at: None,
                    title: None,
                    kind: Some("weekly".into()),
                    fetch_error: None,
                }],
            )
            .unwrap();
    }
}

pub(crate) fn refusal(error: &SpawnError) -> String {
    error.to_string()
}

#[test]
fn spawn_creates_worktree_pane_and_run() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, Some(Size::M));
    let spawned = rig.spawn(&card, RunRole::Builder);

    // The run, its worktree and its pane are one record.
    let run = rig.run(&spawned.run.id);
    assert_eq!(
        run.worktree_path.as_deref(),
        Some(spawned.worktree.as_str())
    );
    assert_eq!(
        run.branch.as_deref(),
        Some(format!("tiller/{}-build", card.display_id).as_str())
    );
    assert_eq!(run.base_branch.as_deref(), Some("main"));
    assert_eq!(run.base_sha.as_deref(), Some(spawned.base_sha.as_str()));
    assert!(run.config_hash.as_deref().is_some_and(|h| h.len() == 64));
    assert_eq!(
        spawned.session.pane_id.as_deref(),
        Some(spawned.pane_id.as_str())
    );
    assert_eq!(spawned.session.run_id.as_deref(), Some(run.id.as_str()));
    assert_eq!(spawned.reservation.run_id, run.id);
    assert!(spawned.reservation.released_at.is_none());
    assert_eq!(spawned.name, format!("{}-build", card.display_id));
    // The lease, then the base and fragment, then the pane.
    assert_eq!(rig.events("run.placed", &run.id), 3);

    // Treehouse leased it to this run, on the run's branch, clean.
    let taken = rig.pool.taken();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].0, PathBuf::from(&spawned.worktree));
    assert_eq!(taken[0].1, spawned.branch);
    assert_eq!(taken[0].3, format!("tiller:{}", run.id));
    let row: Worktree = find(rig.fleet.store.conn(), "path = ?1", &[&spawned.worktree])
        .unwrap()
        .unwrap();
    assert_eq!(row.run_id.as_deref(), Some(run.id.as_str()));
    assert_eq!((row.dirty, row.unlanded_commits), (Some(false), Some(0)));

    // The workspace sits right under the project's orchestrator, labelled,
    // with the row tokens that name the session.
    let ids = rig.workspace_ids();
    let orchestrator = ids.iter().position(|id| id == "w1").unwrap();
    assert_eq!(ids[orchestrator + 1], spawned.workspace_id, "{ids:?}");
    let snapshot = rig.herdr.snapshot().unwrap();
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|w| w.workspace_id == spawned.workspace_id)
        .unwrap();
    assert_eq!(workspace.label, spawned.label);
    let pane = rig.herdr.fake_pane(&spawned.pane_id);
    assert_eq!(
        pane.tokens.get("session").map(String::as_str),
        Some(spawned.session.id.as_str())
    );
    assert_eq!(
        pane.tokens.get("run").map(String::as_str),
        Some(run.id.as_str())
    );

    // The tokens are reported before the launch line is typed, and the
    // line runs the launch script for this run.
    let calls = rig.herdr.calls();
    let at = |prefix: &str| {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
    };
    let script = rig.scratch.join(&run.id).join("launch.sh");
    let line = format!("send_text {} sh '{}'", spawned.pane_id, script.display());
    assert!(at("report_tokens") < at(&line));
    assert!(at(&line) < at("send_enter"));

    // The role and the brief are what the session starts from.
    let prompt = std::fs::read_to_string(rig.scratch.join(&run.id).join("prompt.md")).unwrap();
    assert!(prompt.contains(&format!("Do the work on {}.", card.display_id)));
    assert!(prompt.contains("# Your brief"));
    let script = std::fs::read_to_string(script).unwrap();
    assert!(script.contains(&format!("TILLER_RUN_ID='{}'", run.id)));
    assert!(script.contains(&format!("TILLER_SESSION_ID='{}'", spawned.session.id)));
    assert!(script.contains("TILLER_ROLE='builder'"));
    assert!(script.contains(&format!("cd '{}'", spawned.worktree)));

    // The session starts past the trust dialog: the worktree is trusted in
    // the Claude config the script hands it.
    assert!(rig.trusted(Path::new(&spawned.worktree)));
    let config = rig.claude.forwarded().unwrap().display().to_string();
    assert!(
        script.contains(&format!("CLAUDE_CONFIG_DIR='{config}'")),
        "{script}"
    );
    assert!(!script.contains("-u CLAUDE_CONFIG_DIR"), "{script}");
}

#[test]
fn spawn_refuses_when_trust_cannot_be_recorded() {
    let mut rig = Rig::new();
    let store = rig.claude.store().unwrap();
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::fs::write(&store, "{\"projects\": [").unwrap();
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err()
        .to_string();
    assert!(error.contains("workspace-trust dialog"), "{error}");
    // Nothing was typed and the owner's config was left as it was.
    let calls = rig.herdr.calls();
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("create_workspace") || c.starts_with("send_text")),
        "{calls:?}"
    );
    assert_eq!(std::fs::read_to_string(&store).unwrap(), "{\"projects\": [");
}

#[test]
fn spawn_replays_its_key_without_a_second_worktree() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let world = world!(rig);
    let first = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("spawn-1"),
        &world,
        Rig::request(&card, RunRole::Builder),
    )
    .unwrap()
    .value;
    let again = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("spawn-1"),
        &world,
        Rig::request(&card, RunRole::Builder),
    )
    .unwrap();
    assert!(again.replayed);
    let again = again.value;
    let (Outcome::Spawned(first), Outcome::Replayed { run, session }) = (first, again) else {
        panic!("expected a spawn, then its replay");
    };
    assert_eq!(run.id, first.run.id);
    assert_eq!(session.pane_id.as_deref(), Some(first.pane_id.as_str()));
    assert_eq!(rig.pool.taken().len(), 1);
}

#[test]
fn dispatch_resolves_model_effort_from_rules() {
    let mut rig = Rig::new();
    for (size, model, effort) in [
        (Some(Size::S), "haiku", Effort::Low),
        (Some(Size::M), "sonnet", Effort::Medium),
        (Some(Size::L), "opus", Effort::Medium),
        (None, "sonnet", Effort::Medium),
    ] {
        let card = rig.card(CardKind::Ship, size);
        let spawned = rig.spawn(&card, RunRole::Builder);
        assert_eq!(
            (spawned.run.model.as_str(), spawned.run.effort),
            (model, effort),
            "{size:?}"
        );
        assert_eq!(spawned.dispatch["model"], model);
        rig.sql("UPDATE projects SET capacity = capacity + 1");
        rig.sql("UPDATE hosts SET lane_capacity = lane_capacity + 1");
    }

    // The owner's per-card override wins over the rules.
    let card = rig.card(CardKind::Ship, Some(Size::S));
    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE cards SET dispatch_override = ?2 WHERE id = ?1",
            rusqlite::params![card.id, r#"{"model":"fable","effort":"high"}"#],
        )
        .unwrap();
    let card: Card = get(rig.fleet.store.conn(), "card", &card.id).unwrap();
    let resolved = dispatch::resolve(&rig.config.dispatch, &card, None, false).unwrap();
    assert_eq!(
        (resolved.model.as_str(), resolved.effort),
        ("fable", Effort::High)
    );
    assert_eq!(resolved.payload["rule"], "override");
}

#[test]
fn orchestrator_downgrade_refused() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, Some(Size::L));
    let ctx = rig.orchestrator();
    let mut request = Rig::request(&card, RunRole::Builder);

    // Lower model, or the same model at lower effort: refused, nothing made.
    for asked in [("haiku", Effort::High), ("opus", Effort::Low)] {
        request.asked = Some((asked.0.into(), asked.1));
        let error = rig.try_spawn(&ctx, request.clone()).unwrap_err();
        assert!(matches!(error, SpawnError::Downgrade(_)), "{error}");
        assert!(refusal(&error).contains("needs the owner"), "{error}");
        assert!(refusal(&error).contains("opus:medium"), "{error}");
    }
    let runs: i64 = rig
        .fleet
        .store
        .conn()
        .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 0);
    assert!(rig.pool.taken().is_empty());
}

#[test]
fn refuses_model_below_rule_without_owner_permission() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, Some(Size::L));
    let ctx = rig.orchestrator();
    let mut request = Rig::request(&card, RunRole::Builder);
    request.asked = Some(("sonnet".into(), Effort::High));
    assert!(matches!(
        rig.try_spawn(&ctx, request.clone()),
        Err(SpawnError::Downgrade(_))
    ));

    // A raise is the orchestrator's to make.
    request.asked = Some(("fable".into(), Effort::Max));
    let Outcome::Spawned(raised) = rig.try_spawn(&ctx, request.clone()).unwrap() else {
        panic!("expected a spawn");
    };
    assert_eq!(
        (raised.run.model.as_str(), raised.run.effort),
        ("fable", Effort::Max)
    );
    assert_eq!(raised.dispatch["by_owner"], false);

    // The owner may lower it.
    let other = rig.card(CardKind::Ship, Some(Size::L));
    let mut request = Rig::request(&other, RunRole::Builder);
    request.asked = Some(("haiku".into(), Effort::Low));
    let Outcome::Spawned(lowered) = rig.try_spawn(&owner(), request).unwrap() else {
        panic!("expected a spawn");
    };
    assert_eq!(lowered.run.model, "haiku");
    assert_eq!(lowered.dispatch["by_owner"], true);
}

/// Spawns that race from separate processes: each has its own connection,
/// pool handle and herdr, and starts at the same instant.
fn race(rig: &Rig, cards: &[Card]) -> Vec<Result<Outcome>> {
    let barrier = Arc::new(Barrier::new(cards.len()));
    let servers = tempfile::tempdir_in(rig.fleet.dir.path()).unwrap();
    std::thread::scope(|scope| {
        let handles: Vec<_> = cards
            .iter()
            .enumerate()
            .map(|(i, card)| {
                let barrier = Arc::clone(&barrier);
                let pool = rig.pool.clone();
                let db = rig.fleet.db.clone();
                let herdr_file = servers.path().join(format!("herdr-{i}.json"));
                let config = rig.config.clone();
                let scratch = rig.scratch.clone();
                let claude = rig.claude.clone();
                let tiller = rig.tiller.clone();
                let ctx = rig.orchestrator();
                let request = Rig::request(card, RunRole::Builder);
                scope.spawn(move || {
                    let mut store = Store::open(&db).unwrap();
                    let herdr = FakeHerdr::new(herdr_file);
                    herdr.add_workspace("w1", "alpha");
                    herdr.add_pane("w1", "w1:p1", Screen::Idle);
                    let scanner = GitScanner::new();
                    let world = World {
                        config: &config,
                        pool: &pool,
                        herdr: &herdr,
                        panes: None,
                        scanner: &scanner,
                        listeners: &NoListeners,
                        tiller: &tiller,
                        scratch_root: &scratch,
                        tiller_home: Path::new("/data/tiller"),
                        claude_config: &claude,
                        ram_free_mb: Some(64_000),
                        disk_free_mb: Some(64_000),
                    };
                    barrier.wait();
                    spawn(&mut store, &ctx, None, &world, request).map(|a| a.value)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

#[test]
fn two_spawns_cannot_oversubscribe_capacity() {
    // Two spawns for the last lane: exactly one wins.
    let mut rig = Rig::new();
    rig.sql("UPDATE projects SET capacity = 1");
    let cards = [
        rig.card(CardKind::Ship, None),
        rig.card(CardKind::Ship, None),
    ];
    for _ in 0..3 {
        let results = race(&rig, &cards);
        let won = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(won, 1, "{results:?}");
        let lost = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert!(refusal(lost).contains("project is full"), "{lost}");
        // The loser never took a worktree.
        assert_eq!(rig.pool.taken().len(), 1);
        let reserved: i64 = rig
            .fleet
            .store
            .conn()
            .query_row(
                "SELECT count(*) FROM reservations WHERE released_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(reserved, 1);
        // Free the lane for the next round.
        let Some(Ok(Outcome::Spawned(winner))) = results.into_iter().find(Result::is_ok) else {
            panic!("no winner");
        };
        rig.done(&winner.run.id, RunOutcome::Abandoned).unwrap();
        rig.pool.log.lock().unwrap().taken.clear();
    }

    // Eight at once for three lanes: three win, on the host's limit too.
    let mut rig = Rig::new();
    rig.sql("UPDATE projects SET capacity = 8");
    rig.sql("UPDATE hosts SET lane_capacity = 3");
    let cards: Vec<Card> = (0..8).map(|_| rig.card(CardKind::Ship, None)).collect();
    let results = race(&rig, &cards);
    let won: Vec<&Outcome> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(won.len(), 3, "{results:?}");
    for lost in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(refusal(lost).contains("host is full"), "{lost}");
    }
    assert_eq!(rig.pool.taken().len(), 3);
    let worktrees: std::collections::BTreeSet<_> =
        rig.pool.taken().into_iter().map(|t| t.0).collect();
    assert_eq!(worktrees.len(), 3, "each winner has its own worktree");
}

/// P03: spawn refuses when free RAM is below the host's RAM floor, or cannot
/// be read at all, and leaves no run, session, worktree or pane behind.
#[test]
fn spawn_refuses_below_ram_floor() {
    let mut rig = Rig::new();
    rig.sql("UPDATE hosts SET ram_floor_mb = 8000");
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let workspaces = rig.workspace_ids();
    let count = |rig: &Rig, table: &str| -> i64 {
        rig.fleet
            .store
            .conn()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let (runs, sessions) = (count(&rig, "runs"), count(&rig, "sessions"));
    for free in [Some(7_999), None] {
        rig.ram_free_mb = free;
        let error = rig
            .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
            .unwrap_err();
        let why = refusal(&error);
        assert!(why.contains("RAM free"), "{error}");
        assert!(why.contains("8000 MB floor"), "{error}");
        assert!(rig.pool.taken().is_empty());
        assert_eq!(rig.workspace_ids(), workspaces);
        assert_eq!(
            (count(&rig, "runs"), count(&rig, "sessions")),
            (runs, sessions)
        );
    }
    // Exactly at the floor is enough.
    rig.ram_free_mb = Some(8_000);
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap();
}

#[test]
fn spawn_refuses_below_disk_floor() {
    let mut rig = Rig::new();
    rig.sql("UPDATE hosts SET disk_floor_mb = 10000");
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    for free in [Some(9_999), None] {
        rig.disk_free_mb = free;
        let error = rig
            .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
            .unwrap_err();
        assert!(refusal(&error).contains("disk free"), "{error}");
    }
    assert!(rig.pool.taken().is_empty());
    rig.disk_free_mb = Some(10_000);
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap();
}

#[test]
fn quota_floor_is_the_account_floor_with_a_fixed_share_per_host() {
    let quota = |share: f64| config::Quota {
        account_floor_percent: 3.0,
        host_share_percent: share,
        ..config::Quota::default()
    };
    // The whole share stops at the account floor; a half share leaves half
    // the room above it to the other host.
    assert!((quota(100.0).host_floor() - 3.0).abs() < 1e-9);
    assert!((quota(50.0).host_floor() - 51.5).abs() < 1e-9);
    assert!((quota(25.0).host_floor() - 75.75).abs() < 1e-9);
    let mut config = SpawnConfig::from_table(&toml::Table::new()).unwrap();
    config.quota = quota(50.0);
    assert_eq!(quota_floor(&config, RunRole::Builder), Some(53.5));
    assert_eq!(quota_floor(&config, RunRole::Scout), Some(53.5));
    assert_eq!(quota_floor(&config, RunRole::Reviewer), Some(51.5));
    config.quota.enabled = false;
    assert_eq!(quota_floor(&config, RunRole::Builder), None);
}

#[test]
fn spawn_refuses_below_quota_floor() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);

    // No reading fails closed.
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(refusal(&error).contains("fails closed"), "{error}");

    // 53 left is under this host's 51.5 plus the 2 point review reserve.
    rig.quota(53.0);
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(refusal(&error).contains("53.5 percent floor"), "{error}");
    assert!(rig.pool.taken().is_empty());

    // A reviewer may use the reserve.
    let Outcome::Spawned(review) = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Reviewer))
        .unwrap()
    else {
        panic!("expected a spawn");
    };
    assert_eq!(review.run.role, RunRole::Reviewer);

    // A stale reading fails closed however much it shows.
    rig.quota(90.0);
    rig.sql("UPDATE quota_windows SET fetched_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-16 minutes')");
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(refusal(&error).contains("stale"), "{error}");
    rig.quota(90.0);
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap();
}

#[test]
fn a_start_the_quota_floor_refuses_is_held_with_one_open_decision() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);
    let reset = Timestamp::now().plus_minutes(3 * 24 * 60);
    rig.quota_resetting(9.0, Some(reset));

    for _ in 0..2 {
        let error = rig
            .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
            .unwrap_err();
        assert!(refusal(&error).contains("held until"), "{error}");
    }
    assert!(rig.pool.taken().is_empty());

    let holds: Vec<Hold> = list(
        rig.fleet.store.conn(),
        "card_id = ?1 AND released_at IS NULL",
        &[&card.id],
    )
    .unwrap();
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].until_at, reset);
    let open: Vec<Attention> = list(
        rig.fleet.store.conn(),
        "card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL",
        &[&card.id],
    )
    .unwrap();
    assert_eq!(open.len(), 1);
    let labels: Vec<_> = open[0]
        .options
        .as_ref()
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|option| option["label"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(labels, ["Start now anyway", "Wait for the reset"]);

    // Past the floor needs the owner's answer first.
    let mut request = Rig::request(&card, RunRole::Builder);
    request.past_quota_floor = true;
    let error = rig.try_spawn(&ctx, request.clone()).unwrap_err();
    assert!(
        refusal(&error).contains("no unused owner decision"),
        "{error}"
    );

    // The orchestrator may not pick the owner's option.
    let picked = rig.fleet.store.attention_answer(
        &ctx,
        None,
        &open[0].id,
        "Start now anyway",
        AnsweredVia::Cli,
    );
    assert!(picked.is_err());
    rig.fleet
        .store
        .attention_answer(
            &owner(),
            None,
            &open[0].id,
            "Start now anyway",
            AnsweredVia::Board,
        )
        .unwrap();
    let holds: Vec<Hold> = list(
        rig.fleet.store.conn(),
        "card_id = ?1 AND released_at IS NULL",
        &[&card.id],
    )
    .unwrap();
    assert!(holds.is_empty());
    let world = world!(rig);
    let first = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("past-1"),
        &world,
        request.clone(),
    )
    .unwrap();
    let Outcome::Spawned(spawned) = first.value else {
        panic!("expected a spawn");
    };
    assert_eq!(spawned.run.role, RunRole::Builder);
    // A retry with the same key replays the run instead of being refused.
    let again = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("past-1"),
        &world,
        request.clone(),
    )
    .unwrap();
    assert!(again.replayed);
    // One answer starts one run.
    rig.done(&spawned.run.id, RunOutcome::Failed).unwrap();
    let error = rig.try_spawn(&ctx, request).unwrap_err();
    assert!(
        refusal(&error).contains("no unused owner decision"),
        "{error}"
    );
}

#[test]
fn waiting_for_the_reset_keeps_the_hold() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);
    rig.quota_resetting(9.0, Some(Timestamp::now().plus_minutes(60)));
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    let item: Attention = find(
        rig.fleet.store.conn(),
        "card_id = ?1 AND answered_at IS NULL",
        &[&card.id],
    )
    .unwrap()
    .unwrap();
    rig.fleet
        .store
        .attention_answer(
            &owner(),
            None,
            &item.id,
            "Wait for the reset",
            AnsweredVia::Board,
        )
        .unwrap();
    let holds: Vec<Hold> = list(
        rig.fleet.store.conn(),
        "card_id = ?1 AND released_at IS NULL",
        &[&card.id],
    )
    .unwrap();
    assert_eq!(holds.len(), 1);

    // While that hold stands, a retry is refused without asking again.
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(refusal(&error).contains("chose to wait"), "{error}");
    assert!(rig.open_items(&card).is_empty());
    assert_eq!(rig.open_holds(&card).len(), 1);
}

#[test]
fn a_refusal_after_the_due_re_ask_replaces_it_with_one_decision() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);
    rig.quota_resetting(2.0, Some(Timestamp::now().plus_minutes(60)));
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(refusal(&error).contains(" 2 percent left"), "{error}");

    // The hold's date passes: the daemon releases it and re-asks.
    rig.sql("UPDATE holds SET until_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 minutes')");
    rig.fleet
        .store
        .hold_release(&Ctx::new(Actor::Daemon), &card.id, HoldReleaseReason::Due)
        .unwrap();
    let open = rig.open_items(&card);
    assert_eq!(open.len(), 1);
    assert!(open[0].options.is_none());

    // The reset moved, so a retry is refused again: one item stays open.
    rig.quota_resetting(2.0, Some(Timestamp::now().plus_minutes(120)));
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    let open = rig.open_items(&card);
    assert_eq!(open.len(), 1, "{open:?}");
    assert!(open[0].options.is_some());
    assert_eq!(rig.open_holds(&card).len(), 1);
}

/// A builder spawn past the floor, as `tiller spawn --past-quota-floor`.
fn past_floor(card: &Card) -> Request {
    let mut request = Rig::request(card, RunRole::Builder);
    request.past_quota_floor = true;
    request
}

#[test]
fn a_newer_wait_answer_shadows_an_older_start_answer() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);
    rig.quota_resetting(9.0, Some(Timestamp::now().plus_minutes(60)));
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    rig.owner_answers(&card, "Start now anyway");

    // The orchestrator retries without the flag: refused again, a new
    // decision, and the owner now waits.
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    rig.owner_answers(&card, "Wait for the reset");

    let error = rig.try_spawn(&ctx, past_floor(&card)).unwrap_err();
    assert!(
        refusal(&error).contains("no unused owner decision"),
        "{error}"
    );
    assert!(rig.pool.taken().is_empty());
}

#[test]
fn any_later_run_uses_up_a_start_answer() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);
    let reset = Timestamp::now().plus_minutes(60);
    rig.quota_resetting(9.0, Some(reset));
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    rig.owner_answers(&card, "Start now anyway");

    // The quota recovers and a plain spawn starts the run.
    rig.quota_resetting(80.0, Some(reset));
    let spawned = rig.spawn(&card, RunRole::Builder);
    rig.done(&spawned.run.id, RunOutcome::Failed).unwrap();

    rig.quota_resetting(9.0, Some(reset));
    let error = rig.try_spawn(&ctx, past_floor(&card)).unwrap_err();
    assert!(
        refusal(&error).contains("no unused owner decision"),
        "{error}"
    );
}

#[test]
fn a_quota_refusal_keeps_an_unrelated_hold() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);
    let vendor = Timestamp::now().plus_minutes(10 * 24 * 60);
    rig.fleet
        .store
        .card_hold(&owner(), &card.id, vendor, "the vendor ships on Friday")
        .unwrap();
    rig.quota_resetting(9.0, Some(Timestamp::now().plus_minutes(60)));

    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(
        refusal(&error).contains(&format!(
            "the card already has another hold, until {vendor} (the vendor ships on Friday)"
        )),
        "{error}"
    );
    assert_eq!(rig.open_items(&card).len(), 1);

    rig.owner_answers(&card, "Start now anyway");
    let holds = rig.open_holds(&card);
    assert_eq!(holds.len(), 1);
    assert_eq!(holds[0].reason, "the vendor ships on Friday");
    assert_eq!(holds[0].until_at, vendor);

    let Outcome::Spawned(spawned) = rig.try_spawn(&ctx, past_floor(&card)).unwrap() else {
        panic!("expected a spawn");
    };
    assert_eq!(spawned.run.role, RunRole::Builder);
}

#[test]
fn an_away_floor_refusal_is_plain_and_binds_past_the_standing_floor() {
    let mut rig = Rig::with_config("account_floor_percent = 3.0\nhost_share_percent = 50.0\n");
    rig.config.quota.enabled = true;
    let ctx = rig.orchestrator();
    let card = rig.card(CardKind::Ship, None);
    rig.fleet
        .store
        .away_start(
            &owner(),
            serde_json::json!({
                "until_at": Timestamp::now().plus_minutes(24 * 60),
                "projects": ["alpha"],
                "spend": {"quota_floor_percent": 90.0},
            }),
        )
        .unwrap();
    // Over the standing floor, under the away floor: no answer lifts it.
    let reset = Timestamp::now().plus_minutes(60);
    rig.quota_resetting(70.0, Some(reset));
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(refusal(&error).contains("90 percent floor"), "{error}");
    assert!(!refusal(&error).contains("held"), "{error}");
    assert!(rig.open_holds(&card).is_empty());
    assert!(rig.open_items(&card).is_empty());

    // Under both floors: the away floor would still refuse past the
    // standing one, so nothing is held or asked either.
    rig.quota_resetting(9.0, Some(reset));
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(!refusal(&error).contains("held"), "{error}");
    assert!(rig.open_holds(&card).is_empty());
    assert!(rig.open_items(&card).is_empty());

    // An answer from before the away contract still cannot pass its floor.
    rig.fleet.store.away_end(&owner(), "back").unwrap();
    rig.try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    rig.owner_answers(&card, "Start now anyway");
    rig.fleet
        .store
        .away_start(
            &owner(),
            serde_json::json!({
                "until_at": Timestamp::now().plus_minutes(24 * 60),
                "projects": ["alpha"],
                "spend": {"quota_floor_percent": 90.0},
            }),
        )
        .unwrap();
    rig.quota_resetting(70.0, Some(reset));
    let error = rig.try_spawn(&ctx, past_floor(&card)).unwrap_err();
    assert!(refusal(&error).contains("90 percent floor"), "{error}");
    assert!(rig.open_items(&card).is_empty());
    assert!(rig.pool.taken().is_empty());
}

#[test]
fn spawn_starts_from_current_origin_default_branch() {
    let mut rig = Rig::new();
    // Origin moved on after the checkout was cloned: the pool's refs are stale.
    let head = rig.repo.advance("second.md");
    assert_ne!(git(&rig.repo.checkout, &["rev-parse", "main"]), head);
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    assert_eq!(spawned.base_sha, head);
    let worktree = Path::new(&spawned.worktree);
    assert_eq!(git(worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(worktree, &["symbolic-ref", "--short", "HEAD"]),
        spawned.branch
    );
    assert!(worktree.join("second.md").exists());
}

#[test]
fn spawn_takes_treehouse_worktree_and_returns_it_on_cleanup() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let run = spawned.run.id.clone();

    // Cleanup waits for the run to end.
    let error = rig.cleanup(&run).unwrap_err();
    assert!(refusal(&error).contains("is active"), "{error}");

    // The work landed: the branch's commit is on origin's default branch.
    let worktree = PathBuf::from(&spawned.worktree);
    std::fs::write(worktree.join("feature.md"), "done\n").unwrap();
    git(&worktree, &["add", "feature.md"]);
    git(&worktree, &["commit", "--quiet", "-m", "feature"]);
    git(&worktree, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&worktree, &["fetch", "--quiet", "origin"]);
    let ended = rig.done(&run, RunOutcome::Abandoned).unwrap();
    assert_eq!(ended.kept, None);
    assert_eq!(
        ended.scan.as_ref().map(WorktreeScan::provably_clean),
        Some(true)
    );

    let cleaned = rig.cleanup(&run).unwrap();
    assert_eq!(cleaned.returned.as_deref(), Some(spawned.worktree.as_str()));
    assert_eq!(cleaned.closed_panes, vec![spawned.pane_id.clone()]);
    assert!(cleaned.warnings.is_empty(), "{:?}", cleaned.warnings);
    assert_eq!(
        rig.pool.returned(),
        vec![(worktree.clone(), format!("tiller:{run}"))]
    );
    assert!(!rig.workspace_ids().contains(&spawned.workspace_id));
    assert_eq!(rig.events("run.cleaned", &run), 1);
    let session: Session = get(rig.fleet.store.conn(), "session", &spawned.session.id).unwrap();
    assert!(session.ended_at.is_some());

    // Cleaning again finds it returned and touches nothing.
    let again = rig.cleanup(&run).unwrap();
    assert_eq!(again.returned, None);
    assert_eq!(rig.pool.returned().len(), 1);
}

/// What a worktree holds when cleanup looks.
#[derive(Clone, Copy, Debug)]
struct Holding {
    unpushed: bool,
    dirty: bool,
    stash: bool,
    live: bool,
}

#[test]
fn cleanup_refuses_with_unlanded_commits() {
    let mut rig = Rig::new();
    rig.sql("UPDATE projects SET capacity = 32");
    rig.sql("UPDATE hosts SET lane_capacity = 32");
    // Every combination: the worktree goes back only when it holds nothing.
    for bits in 0..16_u8 {
        // Sixteen spawns would trip the spawn rate limit: age the earlier
        // sessions out of its window.
        rig.sql(
            "UPDATE sessions SET started_at = strftime('%Y-%m-%dT%H:%M:%fZ', started_at, '-1 hour')",
        );
        let holding = Holding {
            unpushed: bits & 1 != 0,
            dirty: bits & 2 != 0,
            stash: bits & 4 != 0,
            live: bits & 8 != 0,
        };
        let card = rig.card(CardKind::Ship, None);
        let spawned = rig.spawn(&card, RunRole::Builder);
        let worktree = PathBuf::from(&spawned.worktree);
        if holding.stash {
            std::fs::write(worktree.join("stashed.md"), "kept\n").unwrap();
            git(&worktree, &["add", "stashed.md"]);
            git(&worktree, &["stash", "push", "--quiet"]);
        }
        if holding.unpushed {
            std::fs::write(worktree.join("unpushed.md"), "local\n").unwrap();
            git(&worktree, &["add", "unpushed.md"]);
            git(&worktree, &["commit", "--quiet", "-m", "local only"]);
        }
        if holding.dirty {
            std::fs::write(worktree.join("scratch.md"), "uncommitted\n").unwrap();
        }
        if holding.live {
            rig.herdr.set_screen(&spawned.pane_id, Screen::Idle);
        }
        let ended = rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
        let holds = holding.unpushed || holding.dirty || holding.stash;
        assert_eq!(ended.kept.is_some(), holds, "{holding:?}");

        let before = rig.herdr.calls().len();
        let result = rig.cleanup(&spawned.run.id);
        let clean = !holds && !holding.live;
        assert_eq!(result.is_ok(), clean, "{holding:?}: {result:?}");
        if let Err(error) = &result {
            let text = refusal(error);
            if holding.live {
                assert!(text.contains("a live pane is never closed"), "{text}");
            } else {
                assert!(text.contains("never removes unlanded work"), "{text}");
            }
            // A refusal closes nothing and returns nothing.
            let calls = rig.herdr.calls();
            assert!(
                !calls[before..].iter().any(|c| c.starts_with("close_pane")),
                "{holding:?}: {calls:?}"
            );
            assert!(rig.pool.returned().iter().all(|(p, _)| p != &worktree));
            assert!(worktree.exists());
            assert_eq!(rig.events("run.cleaned", &spawned.run.id), 0);
        } else {
            assert!(rig.pool.returned().iter().any(|(p, _)| p == &worktree));
        }
        // The shared stash would mark every later worktree; clear it.
        if holding.stash {
            git(&worktree, &["stash", "clear"]);
        }
    }
}

#[test]
fn done_refuses_while_commits_are_unlanded() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Scout, None);
    let spawned = rig.spawn(&card, RunRole::Scout);
    let worktree = PathBuf::from(&spawned.worktree);
    std::fs::write(worktree.join("notes.md"), "found\n").unwrap();
    git(&worktree, &["add", "notes.md"]);
    git(&worktree, &["commit", "--quiet", "-m", "notes"]);

    let error = rig.done(&spawned.run.id, RunOutcome::Reported).unwrap_err();
    assert!(refusal(&error).contains("1 unlanded commit"), "{error}");
    assert!(rig.run(&spawned.run.id).ended_at.is_none());
    let row: Worktree = find(rig.fleet.store.conn(), "path = ?1", &[&spawned.worktree])
        .unwrap()
        .unwrap();
    assert_eq!(row.unlanded_commits, Some(1));

    // Uncommitted changes refuse the same way.
    git(&worktree, &["reset", "--quiet", "--soft", "HEAD~1"]);
    let error = rig.done(&spawned.run.id, RunOutcome::Reported).unwrap_err();
    assert!(refusal(&error).contains("uncommitted changes"), "{error}");

    // Abandoning keeps the worktree for the orchestrator, and cleanup will
    // not take it.
    let ended = rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    assert!(ended.kept.as_deref().is_some_and(
        |k| k.contains("ended abandoned while") && k.ends_with("cleanup keeps the worktree")
    ));
    assert_eq!(ended.run.outcome_note, ended.kept);
    assert_eq!(ended.run.outcome, Some(RunOutcome::Abandoned));
    assert!(rig.cleanup(&spawned.run.id).is_err());
    assert!(rig.pool.returned().is_empty());

    // Once it is committed and on origin, done would pass and cleanup does.
    git(&worktree, &["commit", "--quiet", "-m", "notes"]);
    git(&worktree, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&worktree, &["fetch", "--quiet", "origin"]);
    rig.cleanup(&spawned.run.id).unwrap();
    assert_eq!(rig.pool.returned().len(), 1);
}

/// Whether the run's reservation still holds its project slot.
fn holds_slot(rig: &Rig, run: &str) -> bool {
    rig.fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM reservations WHERE run_id = ?1 AND released_at IS NULL",
            [run],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

/// A builder commits on a ship card and its branch goes to origin; it is
/// still unlanded, since only the default branch counts.
fn committed_builder(rig: &mut Rig) -> (Card, Spawned, PathBuf, String) {
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let worktree = PathBuf::from(&spawned.worktree);
    std::fs::write(worktree.join("fix.md"), "fixed\n").unwrap();
    git(&worktree, &["add", "fix.md"]);
    git(&worktree, &["commit", "--quiet", "-m", "fix"]);
    let head = git(&worktree, &["rev-parse", "HEAD"]).trim().to_owned();
    (card, spawned, worktree, head)
}

fn link_pr(rig: &mut Rig, card: &Card, run: &str, number: i64, head: Option<&str>) -> Pr {
    rig.fleet
        .store
        .pr_link(
            &Ctx::new(Actor::Daemon),
            tiller_core::state::approvals::NewPr {
                card: card.id.clone(),
                run_id: Some(run.to_owned()),
                number,
                url: format!("https://github.com/example/alpha/pull/{number}"),
                head_sha: head.map(str::to_owned),
                base_ref: Some("main".into()),
            },
        )
        .unwrap()
        .value
}

/// The direct-pr flow: the worker commits and describes its PR, the daemon
/// pushes the branch and opens the PR, and `tiller done` then ends the run
/// `reported`, which frees the project slot; the PR stays on the card.
#[test]
fn done_reports_a_builder_whose_head_the_daemon_published() {
    let mut rig = Rig::new();
    let (card, spawned, worktree, head) = committed_builder(&mut rig);
    let run = spawned.run.id.clone();

    // Nothing published yet: the commit is only in the worktree.
    let error = rig.done(&run, RunOutcome::Reported).unwrap_err();
    let text = refusal(&error);
    assert!(text.contains("1 unlanded commit, and no open PR"), "{text}");
    assert!(holds_slot(&rig, &run));

    // A PR whose head the forge has not reported, and no publish record:
    // still nothing says the head left the worktree.
    let pr = link_pr(&mut rig, &card, &run, 1, None);
    assert!(rig.done(&run, RunOutcome::Reported).is_err());

    rig.fleet
        .store
        .pr_describe(&owner(), &run, "Fix it", "Fixes it.")
        .unwrap();
    git(
        &worktree,
        &["push", "--quiet", "origin", "HEAD:refs/heads/fix"],
    );
    rig.fleet
        .store
        .publish_done(&Ctx::new(Actor::Daemon), &run, &head, &pr.id)
        .unwrap();

    let ended = rig.done(&run, RunOutcome::Reported).unwrap();
    assert_eq!(ended.run.outcome, Some(RunOutcome::Reported));
    assert_eq!(ended.kept, None);
    assert!(
        ended
            .run
            .outcome_note
            .as_deref()
            .is_some_and(|n| n.contains("PR #1") && n.contains("tracks it to landing")),
        "{:?}",
        ended.run.outcome_note
    );
    assert!(!holds_slot(&rig, &run));
    let pr: Pr = get(rig.fleet.store.conn(), "pr", &pr.id).unwrap();
    assert_eq!(pr.state, PrState::Open);
    assert_eq!(pr.card_id, card.id);
    // The commits are not landed, so cleanup keeps the worktree.
    assert!(rig.cleanup(&run).is_err());
    assert!(worktree.exists());
}

/// The forge's PR head stands for the push too, and anything the PR does
/// not hold refuses: a later commit, or uncommitted changes.
#[test]
fn done_reports_a_builder_only_while_its_open_pr_holds_the_head() {
    let mut rig = Rig::new();
    let (card, spawned, worktree, head) = committed_builder(&mut rig);
    let run = spawned.run.id.clone();
    link_pr(&mut rig, &card, &run, 2, Some(&head));

    // Uncommitted changes are on no PR, and the refusal says so.
    std::fs::write(worktree.join("scratch.md"), "uncommitted\n").unwrap();
    let text = refusal(&rig.done(&run, RunOutcome::Reported).unwrap_err());
    assert!(text.contains("uncommitted changes"), "{text}");
    assert!(!text.contains("no open PR"), "{text}");
    std::fs::remove_file(worktree.join("scratch.md")).unwrap();

    // A commit after the PR's head is not on it yet.
    std::fs::write(worktree.join("more.md"), "more\n").unwrap();
    git(&worktree, &["add", "more.md"]);
    git(&worktree, &["commit", "--quiet", "-m", "more"]);
    let text = refusal(&rig.done(&run, RunOutcome::Reported).unwrap_err());
    assert!(
        text.contains("2 unlanded commits, and no open PR"),
        "{text}"
    );
    assert!(holds_slot(&rig, &run));

    // Back at the PR's head, the run ends reported.
    git(&worktree, &["reset", "--quiet", "--hard", &head]);
    let ended = rig.done(&run, RunOutcome::Reported).unwrap();
    assert_eq!(ended.run.outcome, Some(RunOutcome::Reported));
    assert!(!holds_slot(&rig, &run));

    // A PR that is closed or merged holds nothing: a second builder whose
    // head is only on a closed PR is refused.
    let (card, spawned, _worktree, head) = committed_builder(&mut rig);
    let pr = link_pr(&mut rig, &card, &spawned.run.id, 3, Some(&head));
    rig.sql(&format!(
        "UPDATE prs SET state = 'closed' WHERE id = '{}'",
        pr.id
    ));
    assert!(rig.done(&spawned.run.id, RunOutcome::Reported).is_err());
}

/// A run on another host is judged by the last reading this host holds,
/// which may predate a commit, so it never ends `reported` on a PR.
#[test]
fn done_reports_a_builder_on_a_pr_only_from_a_fresh_reading() {
    let mut rig = Rig::new();
    let (card, spawned, _worktree, head) = committed_builder(&mut rig);
    let run = spawned.run.id.clone();
    link_pr(&mut rig, &card, &run, 5, Some(&head));
    // A refused `landed` records the reading: head on the PR, nothing dirty.
    assert!(rig.done(&run, RunOutcome::Landed).is_err());
    let local: String = rig
        .fleet
        .store
        .conn()
        .query_row("SELECT value FROM meta WHERE key = 'host_id'", [], |row| {
            row.get(0)
        })
        .unwrap();
    rig.sql("UPDATE meta SET value = 'another-host' WHERE key = 'host_id'");
    assert!(rig.done(&run, RunOutcome::Reported).is_err());
    assert!(rig.run(&run).ended_at.is_none());
    rig.sql(&format!(
        "UPDATE meta SET value = '{local}' WHERE key = 'host_id'"
    ));
    let ended = rig.done(&run, RunOutcome::Reported).unwrap();
    assert_eq!(ended.run.outcome, Some(RunOutcome::Reported));
}

#[test]
fn failed_spawn_ends_the_run_and_returns_the_worktree() {
    let mut rig = Rig::new();
    rig.herdr.refuse("create_workspace");
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(matches!(error, SpawnError::Herdr(_)), "{error}");

    let run: Run = find(rig.fleet.store.conn(), "card_id = ?1", &[&card.id])
        .unwrap()
        .unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Failed));
    let reason: String = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT release_reason FROM reservations WHERE run_id = ?1",
            [&run.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reason, "spawn_failed");
    let taken = rig.pool.taken();
    assert_eq!(
        rig.pool.returned(),
        vec![(taken[0].0.clone(), taken[0].3.clone())]
    );

    assert!(
        rig.workspace_ids()
            .iter()
            .all(|id| id == "w1" || id == "w3")
    );
}

/// P67: the developer's own skills and slash commands stay callable in a
/// role's session. The launch does not run Claude in `--bare` mode or switch
/// slash commands or skills off, for a worker or a settings fragment.
#[test]
fn launch_keeps_the_developers_skills_callable() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let dir = rig.scratch.join(&spawned.run.id);
    let script = std::fs::read_to_string(dir.join("launch.sh")).unwrap();
    for flag in [
        "--bare",
        "--disable-slash-commands",
        "--setting-sources",
        "--plugin-dir",
    ] {
        assert!(!script.contains(flag), "launch script passes {flag}");
    }
    let settings: Value =
        serde_json::from_slice(&std::fs::read(dir.join("settings.json")).unwrap()).unwrap();
    for key in ["skillOverrides", "disableSkillShellExecution", "skills"] {
        assert!(settings.get(key).is_none(), "settings fragment sets {key}");
    }
}

#[test]
fn worker_launch_has_remote_control_off() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let settings = std::fs::read(rig.scratch.join(&spawned.run.id).join("settings.json")).unwrap();
    let fragment: Value = serde_json::from_slice(&settings).unwrap();
    assert_eq!(fragment["disableRemoteControl"], true);
    assert_eq!(fragment["remoteControlAtStartup"], false);
    // The run records the hash of a manifest whose settings part is the
    // fragment it launched with (learning-loop.md, section 5).
    use sha2::Digest;
    let fragment_hash: String = sha2::Sha256::digest(&settings)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let hash = rig.run(&spawned.run.id).config_hash.unwrap();
    let manifest: tiller_core::domain::ConfigManifest =
        tiller_core::state::find(rig.fleet.store.conn(), "config_hash = ?1", &[&hash])
            .unwrap()
            .unwrap();
    assert_eq!(manifest.parts["settings"], fragment_hash);
    assert_eq!(hash, launch::hash_of(&manifest.parts));
}

/// Every spawned run carries a manifest-backed hash, and the parts are what
/// the design names; a different model is a different hash in exactly the
/// `model` part.
#[test]
fn config_hash_covers_the_manifest() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let hash = rig.run(&spawned.run.id).config_hash.unwrap();
    let manifest: tiller_core::domain::ConfigManifest =
        tiller_core::state::find(rig.fleet.store.conn(), "config_hash = ?1", &[&hash])
            .unwrap()
            .unwrap();
    let keys: Vec<&String> = manifest.parts.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "claude", "effort", "mcp", "memory", "model", "role", "settings", "tiller"
        ]
    );
    assert_eq!(manifest.parts["tiller"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest.parts["model"], spawned.run.model);
    assert_eq!(rig.events("config.manifest_recorded", &hash), 1);

    use crate::roles::Role;
    let other = launch::manifest(
        Role::Builder,
        b"{}",
        "other-model",
        spawned.run.effort,
        None,
        &[],
    );
    let diff = tiller_core::state::learning::manifest_diff(&manifest.parts, &other);
    assert!(diff.contains_key("model") && diff.contains_key("settings"));
    assert!(!diff.contains_key("role") && !diff.contains_key("tiller"));
    // The memory part follows the rendered revisions, in any order.
    let a = launch::manifest(
        Role::Builder,
        b"{}",
        "m",
        Effort::High,
        None,
        &[("s1".into(), "b1".into()), ("s2".into(), "b2".into())],
    );
    let b = launch::manifest(
        Role::Builder,
        b"{}",
        "m",
        Effort::High,
        None,
        &[("s2".into(), "b2".into()), ("s1".into(), "b1".into())],
    );
    assert_eq!(a["memory"], b["memory"]);
    assert_ne!(a["memory"], other["memory"]);
}

#[test]
fn auto_memory_off() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let script =
        std::fs::read_to_string(rig.scratch.join(&spawned.run.id).join("launch.sh")).unwrap();
    assert!(
        script.contains("CLAUDE_CODE_DISABLE_AUTO_MEMORY='1'"),
        "{script}"
    );
}

/// G35: a role launches only on a harness whose tool calls reach
/// `tiller guard` and that has a sandbox equivalent. The refusal comes before
/// any run, pane or worktree exists.
#[test]
fn refuses_harness_without_pretool_transport() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    for (harness, reason) in [
        ("cursor", "no pre-tool hook transport"),
        ("opencode", "no pre-tool hook transport"),
        ("codex", "no pre-tool hook transport"),
        ("gemini", "has no adapter"),
    ] {
        let mut request = Rig::request(&card, RunRole::Builder);
        request.harness = harness.into();
        let error = rig.try_spawn(&ctx, request).unwrap_err();
        assert!(
            matches!(error, SpawnError::Refused(_)),
            "{harness}: {error:?}"
        );
        assert!(error.to_string().contains(reason), "{harness}: {error}");
    }
    let runs: i64 = rig
        .fleet
        .store
        .conn()
        .query_row("SELECT count(*) FROM runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs, 0, "a refused harness starts no run");
    // Claude Code, which has both, still spawns.
    let spawned = rig.spawn(&card, RunRole::Builder);
    assert_eq!(rig.run(&spawned.run.id).role, RunRole::Builder);
}

#[test]
fn role_mcp_config_is_packaged() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let dir = rig.scratch.join(&spawned.run.id);
    assert_eq!(
        std::fs::read_to_string(dir.join("mcp.json")).unwrap(),
        roles::MCP_CONFIG
    );
    let script = std::fs::read_to_string(dir.join("launch.sh")).unwrap();
    let mcp = format!(
        "--strict-mcp-config' '--mcp-config' '{}'",
        dir.join("mcp.json").display()
    );
    assert!(script.contains(&mcp), "{script}");
}

#[test]
fn cleanup_without_card_leaves_no_marker() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let scratch = rig.scratch.join(&spawned.run.id);
    assert!(scratch.join("launch.sh").exists());
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    // The card is closed and gone from the board; cleanup asks nothing of it.
    rig.sql("UPDATE cards SET state = 'closed', outcome = 'cancelled', closed_at = created_at");
    let cleaned = rig.cleanup(&spawned.run.id).unwrap();
    assert!(cleaned.warnings.is_empty(), "{:?}", cleaned.warnings);
    // The run's launch files were its only files outside state.db.
    assert!(!scratch.exists());
    assert_eq!(std::fs::read_dir(&rig.scratch).unwrap().count(), 0);
    let worktree = PathBuf::from(&spawned.worktree);
    let stray: Vec<_> = std::fs::read_dir(&worktree)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".git" && name != "README.md")
        .collect();
    assert!(stray.is_empty(), "{stray:?}");
}

#[test]
fn spawn_without_a_brief_file_briefs_with_the_card() {
    let mut rig = Rig::new();
    let card = rig
        .fleet
        .store
        .card_create(
            &owner(),
            None,
            NewCard {
                project_id: "alpha".into(),
                title: "Ship the login page".into(),
                body: Some("Use the existing form.".into()),
                acceptance: Some("The page loads.".into()),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let mut request = Rig::request(&card, RunRole::Builder);
    request.brief = None;
    let ctx = rig.orchestrator();
    let Outcome::Spawned(spawned) = rig.try_spawn(&ctx, request).unwrap() else {
        panic!("expected a spawn");
    };
    let prompt =
        std::fs::read_to_string(rig.scratch.join(&spawned.run.id).join("prompt.md")).unwrap();
    assert!(prompt.contains(&format!("Card {}: Ship the login page", card.display_id)));
    assert!(prompt.contains("Use the existing form."));
    assert!(prompt.contains("## Acceptance\n\nThe page loads."));
}

#[test]
fn spawn_that_fails_before_its_base_is_read_returns_the_worktree() {
    let mut rig = Rig::new();
    // An origin that cannot be fetched fails the spawn right after the take.
    rig.pool.origin = rig.repo.root.path().join("gone.git");
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(matches!(error, SpawnError::Git(_)), "{error}");

    let run: Run = find(rig.fleet.store.conn(), "card_id = ?1", &[&card.id])
        .unwrap()
        .unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Failed));
    let taken = rig.pool.taken();
    assert_eq!(run.worktree_path.as_deref(), taken[0].0.to_str());
    assert_eq!(
        rig.pool.returned(),
        vec![(taken[0].0.clone(), taken[0].3.clone())]
    );
    assert_eq!(rig.events("run.cleaned", &run.id), 1);
    assert!(!rig.scratch.join(&run.id).exists());
}

#[test]
fn spawn_whose_placement_fails_returns_the_unrecorded_worktree() {
    let mut rig = Rig::new();
    // `run.place` fails after the pool leased the worktree, so no record
    // names the lease for cleanup to find.
    rig.sql(
        "CREATE TEMP TRIGGER refuse_placement BEFORE UPDATE OF worktree_path ON runs
         WHEN NEW.worktree_path IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'placement refused'); END",
    );
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(error.to_string().contains("placement refused"), "{error}");
    assert!(!matches!(error, SpawnError::Undone { .. }), "{error}");

    let run: Run = find(rig.fleet.store.conn(), "card_id = ?1", &[&card.id])
        .unwrap()
        .unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Failed));
    assert_eq!(run.worktree_path, None);
    let taken = rig.pool.taken();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].3, format!("tiller:{}", run.id));
    assert_eq!(
        rig.pool.returned(),
        vec![(taken[0].0.clone(), taken[0].3.clone())]
    );
}

#[test]
fn replaying_a_failed_spawn_refuses() {
    let mut rig = Rig::new();
    rig.herdr.refuse("create_workspace");
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let world = world!(rig);
    let request = || Rig::request(&card, RunRole::Builder);
    let first = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("spawn-1"),
        &world,
        request(),
    );
    assert!(matches!(first, Err(SpawnError::Herdr(_))), "{first:?}");
    let again = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("spawn-1"),
        &world,
        request(),
    )
    .map(|a| a.value)
    .unwrap_err();
    let text = refusal(&again);
    assert!(matches!(again, SpawnError::Refused(_)), "{text}");
    assert!(text.contains("which failed: spawn failed:"), "{text}");
    assert!(text.contains("new key"), "{text}");
    assert_eq!(rig.pool.taken().len(), 1);

    // A spawn that stopped before its pane opened refuses its replay too.
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let world = world!(rig);
    let started = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("spawn-2"),
        &world,
        Rig::request(&card, RunRole::Builder),
    )
    .unwrap();
    let Outcome::Spawned(spawned) = started.value else {
        panic!("expected a spawn");
    };
    rig.sql(&format!(
        "UPDATE sessions SET pane_id = NULL WHERE id = '{}'",
        spawned.session.id
    ));
    let error = spawn(
        &mut rig.fleet.store,
        &ctx,
        Some("spawn-2"),
        &world,
        Rig::request(&card, RunRole::Builder),
    )
    .map(|a| a.value)
    .unwrap_err();
    assert!(refusal(&error).contains("before a pane opened"), "{error}");
}

/// An unrecorded lease that goes back is not reported as kept, even when
/// the cleanup before it fails.
#[test]
fn a_failed_spawn_does_not_call_a_returned_worktree_kept() {
    let mut rig = Rig::new();
    // `run.place` fails after the lease, and the run cannot be ended, so
    // cleanup refuses an active run.
    rig.sql(
        "CREATE TEMP TRIGGER refuse_placement BEFORE UPDATE OF worktree_path ON runs
         WHEN NEW.worktree_path IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'placement refused'); END",
    );
    rig.sql(
        "CREATE TEMP TRIGGER refuse_end BEFORE UPDATE OF ended_at ON runs
         WHEN NEW.ended_at IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'end refused'); END",
    );
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    let SpawnError::Undone { notes, .. } = &error else {
        panic!("expected an undo note: {error}");
    };
    assert!(notes.contains("cleanup failed"), "{notes}");
    assert!(!notes.contains("was kept"), "{notes}");
    assert_eq!(rig.pool.returned().len(), 1);
}

#[test]
fn a_failed_spawn_keeps_its_own_error_under_the_undo_notes() {
    let mut rig = Rig::new();
    // The pane opens, the launch line fails, and cleanup cannot read herdr.
    rig.herdr.refuse("send_text");
    rig.herdr.refuse("pane");
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    let SpawnError::Undone { error, notes } = &error else {
        panic!("expected an undo note: {error}");
    };
    assert!(matches!(**error, SpawnError::Herdr(_)), "{error}");
    assert!(notes.contains("the worktree was kept"), "{notes}");
    assert!(rig.pool.returned().is_empty());
}

#[test]
fn cleaning_again_after_the_path_is_leased_again_changes_nothing() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let run = spawned.run.id.clone();
    rig.done(&run, RunOutcome::Abandoned).unwrap();
    rig.herdr.close_pane(&spawned.pane_id).unwrap();
    rig.cleanup(&run).unwrap();

    // treehouse hands the same path to a new run; its scan rewrites the row.
    rig.sql(&format!(
        "UPDATE worktrees SET gone_at = NULL, run_id = NULL WHERE path = '{}'",
        spawned.worktree
    ));
    let again = rig.cleanup(&run).unwrap();
    assert_eq!(again.returned, None);
    assert_eq!(rig.pool.returned().len(), 1);
    assert_eq!(rig.events("run.cleaned", &run), 1);
    let row: Worktree = find(rig.fleet.store.conn(), "path = ?1", &[&spawned.worktree])
        .unwrap()
        .unwrap();
    assert_eq!((row.gone_at, row.run_id), (None, None));
}

#[test]
fn cleanup_never_closes_a_pane_another_session_holds() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let run = spawned.run.id.clone();
    rig.done(&run, RunOutcome::Abandoned).unwrap();
    // herdr gave the run's pane id to another session's workspace, which
    // runs an agent there.
    rig.herdr
        .report_tokens(
            &spawned.pane_id,
            &std::collections::BTreeMap::from([("session".into(), "01OTHERSESSION".into())]),
        )
        .unwrap();
    rig.herdr.set_screen(&spawned.pane_id, Screen::Idle);

    let cleaned = rig.cleanup(&run).unwrap();
    assert!(cleaned.closed_panes.is_empty());
    assert!(rig.workspace_ids().contains(&spawned.workspace_id));
    assert!(
        !rig.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("close_pane"))
    );
    assert_eq!(cleaned.returned.as_deref(), Some(spawned.worktree.as_str()));
}

/// A rig whose config names the roots an orchestrator start needs, with the
/// fleet's orchestrator ended so the project has none.
fn orchestrator_rig() -> (Rig, PathBuf) {
    let mut rig = Rig::new();
    let root = rig.repo.root.path().to_owned();
    rig.config = config(
        &rig.repo.checkout,
        &format!(
            "\n[spawn]\npool_root = {:?}\ncheckouts_root = {:?}\n",
            root.join("pool").to_string_lossy(),
            root.to_string_lossy(),
        ),
    );
    let id = rig.fleet.orchestrator_session.clone();
    rig.fleet.store.session_end(&owner(), &id, "x").unwrap();
    let homes = rig.fleet.dir.path().join("homes");
    (rig, homes)
}

fn start_orchestrator(
    rig: &mut Rig,
    homes: &Path,
    ctx: &Ctx,
) -> Result<Applied<orchestrator::Orchestrator>> {
    let world = world!(rig);
    orchestrator::start(&mut rig.fleet.store, ctx, &world, homes, "alpha", None)
}

/// A verified peer board is the owner on this host (`authority.md` G41): its
/// forwarded Start orchestrator launches one, recorded as `board@<label>`.
/// A fleet session, a hook, an import or the curator never starts one.
#[test]
fn orchestrator_start_takes_a_peer_board_and_refuses_the_fleet() {
    let (mut rig, homes) = orchestrator_rig();
    let workspaces = rig.workspace_ids();
    for actor in [
        Actor::Session(rig.fleet.orchestrator_session.clone()),
        Actor::Hook,
        Actor::Import,
        Actor::Curator("01JPRETRO0000000000000000A".into()),
    ] {
        let error = start_orchestrator(&mut rig, &homes, &Ctx::new(actor.clone()))
            .map(|_| ())
            .unwrap_err();
        assert!(
            error.to_string().contains("may not start"),
            "{actor}: {error}"
        );
    }
    assert_eq!(
        rig.workspace_ids(),
        workspaces,
        "a refused start reached herdr"
    );

    let started = start_orchestrator(
        &mut rig,
        &homes,
        &Ctx::new(Actor::Peer("board-host".into())),
    )
    .unwrap();
    assert!(!started.replayed);
    let actor: String = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT actor FROM events WHERE kind = 'session.started' AND entity_id = ?1",
            [&started.value.session.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(actor, "board@board-host");
}

#[test]
fn orchestrator_restart_replaces_the_live_one_with_fresh_launch_files() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let old_pane = first.pane_id.clone().unwrap();
    rig.herdr.set_screen(&old_pane, Screen::Idle);
    rig.fleet.bind(&first.session.id, &old_pane);

    // Without a restart a live orchestrator is returned as it is.
    let again = start_orchestrator(&mut rig, &homes, &owner()).unwrap();
    assert!(again.replayed);
    assert_eq!(again.value.session.id, first.session.id);

    // tiller is upgraded: its binary now lives somewhere else.
    rig.tiller = PathBuf::from("/home/user/upgraded/bin/tiller");
    let waits = crate::control::Waits {
        interrupt: std::time::Duration::from_secs(2),
        exit: std::time::Duration::from_secs(2),
        bind: std::time::Duration::from_secs(2),
        poll: std::time::Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    };
    let world = world!(rig);
    let restarted = orchestrator::start(
        &mut rig.fleet.store,
        &owner(),
        &world,
        &homes,
        "alpha",
        Some(&waits),
    )
    .unwrap();
    assert!(!restarted.replayed);
    let value = restarted.value;
    assert_ne!(value.session.id, first.session.id);
    let old: Session = get(rig.fleet.store.conn(), "session", &first.session.id).unwrap();
    assert_eq!(old.end_reason.as_deref(), Some(orchestrator::RESTARTED));
    assert!(
        rig.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("close_pane") && c.contains(&old_pane)),
        "the stopped pane was not closed"
    );

    let settings =
        std::fs::read_to_string(rig.scratch.join(&value.session.id).join("settings.json")).unwrap();
    assert!(
        settings.contains("/home/user/upgraded/bin/tiller hook"),
        "{settings}"
    );
}

#[test]
fn orchestrator_restart_of_a_retired_project_keeps_the_live_one() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let pane = first.pane_id.clone().unwrap();
    rig.herdr.set_screen(&pane, Screen::Idle);
    rig.fleet.bind(&first.session.id, &pane);
    rig.fleet.store.project_retire(&owner(), "alpha").unwrap();

    let waits = crate::control::Waits {
        interrupt: std::time::Duration::from_secs(2),
        exit: std::time::Duration::from_secs(2),
        bind: std::time::Duration::from_secs(2),
        poll: std::time::Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    };
    let world = world!(rig);
    let error = orchestrator::start(
        &mut rig.fleet.store,
        &owner(),
        &world,
        &homes,
        "alpha",
        Some(&waits),
    )
    .unwrap_err();
    assert!(error.to_string().contains("project is retired"), "{error}");
    let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
        .unwrap()
        .unwrap();
    assert_eq!(live.id, first.session.id);
    assert!(
        !rig.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("close_pane") && c.contains(&pane)),
        "the live pane was closed"
    );
}

#[test]
fn agent_stop_ends_a_live_orchestrator_and_closes_its_pane() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let pane = first.pane_id.clone().unwrap();
    rig.herdr.set_screen(&pane, Screen::Idle);
    rig.fleet.bind(&first.session.id, &pane);
    let waits = crate::control::Waits {
        interrupt: std::time::Duration::from_secs(2),
        exit: std::time::Duration::from_secs(2),
        bind: std::time::Duration::from_secs(2),
        poll: std::time::Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    };

    // Only the owner stops an orchestrator.
    let world = world!(rig);
    let error = crate::control::stop_agent(
        &mut rig.fleet.store,
        &Ctx::new(Actor::Daemon),
        &world,
        &waits,
        &first.session,
    )
    .unwrap_err();
    assert!(error.to_string().contains("may not stop"), "{error}");
    assert!(
        !rig.herdr
            .calls()
            .contains(&format!("send_text {pane} /exit"))
    );

    let world = world!(rig);
    let stopped = crate::control::stop_agent(
        &mut rig.fleet.store,
        &owner(),
        &world,
        &waits,
        &first.session,
    )
    .unwrap()
    .value;
    assert_eq!(
        stopped.session.end_reason.as_deref(),
        Some(crate::control::STOPPED)
    );
    assert_eq!(stopped.closed_pane.as_deref(), Some(pane.as_str()));
    assert_eq!(stopped.cleaned, None);
    assert_eq!(stopped.warning, None);
    assert!(
        tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
            .unwrap()
            .is_none()
    );
}

#[test]
fn agent_stop_never_closes_a_pane_claude_still_holds() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let pane = first.pane_id.clone().unwrap();
    rig.herdr.set_screen(&pane, Screen::Idle);
    rig.fleet.bind(&first.session.id, &pane);
    // Its `SessionEnd` hook fired, but Claude is still in the pane.
    rig.fleet
        .store
        .session_end(
            &Ctx::new(Actor::Hook),
            &first.session.id,
            "prompt_input_exit",
        )
        .unwrap();
    rig.herdr.ignore_exit(&pane);
    let waits = crate::control::Waits {
        interrupt: std::time::Duration::from_secs(2),
        exit: std::time::Duration::from_millis(300),
        bind: std::time::Duration::from_secs(2),
        poll: std::time::Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    };

    let world = world!(rig);
    let stopped = crate::control::stop_agent(
        &mut rig.fleet.store,
        &owner(),
        &world,
        &waits,
        &first.session,
    )
    .unwrap()
    .value;
    assert!(
        stopped.stop_proof.contains("SessionEnd"),
        "{}",
        stopped.stop_proof
    );
    assert_eq!(stopped.closed_pane, None);
    let warning = stopped.warning.unwrap_or_default();
    assert!(
        warning.contains(&format!("left pane {pane} open")),
        "{warning}"
    );
    assert!(!rig.herdr.calls().contains(&format!("close_pane {pane}")));
}

#[test]
fn agent_stop_leaves_a_pane_another_session_now_holds() {
    let waits = crate::control::Waits {
        interrupt: std::time::Duration::from_secs(2),
        exit: std::time::Duration::from_millis(300),
        bind: std::time::Duration::from_secs(2),
        poll: std::time::Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    };
    // herdr gave the orchestrator's pane id to another workspace: once to an
    // idle agent, after the orchestrator's hook ended it, and once to a bare
    // shell while its row was still live.
    for (ended, screen) in [(true, Screen::Idle), (false, Screen::Shell)] {
        let (mut rig, homes) = orchestrator_rig();
        let first = start_orchestrator(&mut rig, &homes, &owner())
            .unwrap()
            .value;
        let pane = first.pane_id.clone().unwrap();
        rig.herdr.set_screen(&pane, Screen::Idle);
        rig.fleet.bind(&first.session.id, &pane);
        if ended {
            rig.fleet
                .store
                .session_end(
                    &Ctx::new(Actor::Hook),
                    &first.session.id,
                    "prompt_input_exit",
                )
                .unwrap();
        }
        rig.herdr
            .report_tokens(
                &pane,
                &std::collections::BTreeMap::from([("session".into(), "01OTHERSESSION".into())]),
            )
            .unwrap();
        rig.herdr.set_screen(&pane, screen);

        let world = world!(rig);
        let stopped = crate::control::stop_agent(
            &mut rig.fleet.store,
            &owner(),
            &world,
            &waits,
            &first.session,
        )
        .unwrap()
        .value;
        assert!(
            stopped.stop_proof.contains("01OTHERSESSION"),
            "{}",
            stopped.stop_proof
        );
        assert_eq!(stopped.closed_pane, None);
        assert_eq!(stopped.warning, None);
        let calls = rig.herdr.calls();
        assert!(!calls.contains(&format!("close_pane {pane}")));
        assert!(!calls.contains(&format!("send_text {pane} /exit")));
    }
}

#[test]
fn orchestrator_start_launches_in_a_new_workspace() {
    let (mut rig, homes) = orchestrator_rig();
    let started = start_orchestrator(&mut rig, &homes, &owner()).unwrap();
    assert!(!started.replayed);
    let value = &started.value;
    let pane = value.pane_id.clone().unwrap();
    assert_eq!(value.session.pane_id.as_deref(), Some(pane.as_str()));
    assert_eq!(value.session.role, tiller_core::domain::Role::Orchestrator);
    assert!(value.session.run_id.is_none());
    assert!(
        value
            .session
            .config_hash
            .as_deref()
            .is_some_and(|h| h.len() == 64)
    );
    assert!(value.warning.is_none(), "{:?}", value.warning);

    // A new workspace in the checkout, named for the project, with the
    // tokens that name the session.
    let workspace = value.workspace_id.clone().unwrap();
    assert!(rig.workspace_ids().contains(&workspace));
    let tokens = rig.herdr.fake_pane(&pane).tokens;
    assert_eq!(tokens.get("role").map(String::as_str), Some("orchestrator"));
    assert_eq!(tokens.get("project").map(String::as_str), Some("alpha"));

    // The home exists, private, and the launch files are the worker form
    // with no run and no brief.
    let home = homes.join("alpha");
    assert_eq!(value.home.as_deref(), Some(home.to_string_lossy().as_ref()));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&home).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    let dir = rig.scratch.join(&value.session.id);
    let script = std::fs::read_to_string(dir.join("launch.sh")).unwrap();
    assert!(script.contains("TILLER_RUN_ID=''"), "{script}");
    assert!(script.contains(&format!("TILLER_SESSION_ID='{}'", value.session.id)));
    assert!(script.contains("TILLER_ROLE='orchestrator'"));
    assert!(script.contains("'--permission-mode' 'auto'"), "{script}");
    assert!(!script.contains(launch::FIRST_PROMPT), "{script}");
    assert!(script.contains(&format!("cd '{}'", rig.repo.checkout.display())));
    assert!(rig.trusted(&rig.repo.checkout));
    let prompt = std::fs::read_to_string(dir.join("prompt.md")).unwrap();
    assert!(!prompt.contains("# Your brief"));
    let settings = std::fs::read_to_string(dir.join("settings.json")).unwrap();
    for placeholder in ["<home>", "<pool>", "<checkouts>"] {
        assert!(
            !settings.contains(placeholder),
            "{placeholder} in {settings}"
        );
    }
    assert!(settings.contains(&home.to_string_lossy().into_owned()));

    // The tokens go out before the launch line, which runs the script.
    let calls = rig.herdr.calls();
    let at = |prefix: &str| {
        calls
            .iter()
            .position(|c| c.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {calls:?}"))
    };
    let line = format!("send_text {pane} sh '{}'", dir.join("launch.sh").display());
    assert!(at("report_tokens") < at(&line));
    assert!(at(&line) < at("send_enter"));

    // Spawn now finds a live orchestrator.
    let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
        .unwrap()
        .unwrap();
    assert_eq!(live.id, value.session.id);
}

#[test]
fn a_second_orchestrator_start_returns_the_first() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner()).unwrap();
    let workspaces = rig.workspace_ids().len();
    let daemon = Ctx::new(Actor::Daemon);
    let second = start_orchestrator(&mut rig, &homes, &daemon).unwrap();
    assert!(second.replayed);
    assert!(second.events.is_empty());
    assert_eq!(second.value.session.id, first.value.session.id);
    assert_eq!(second.value.workspace_id, first.value.workspace_id);
    assert!(second.value.warning.is_none(), "{:?}", second.value.warning);
    assert_eq!(rig.workspace_ids().len(), workspaces, "no second workspace");
}

#[test]
fn a_keyed_orchestrator_start_retried_replays_the_first() {
    let (mut rig, homes) = orchestrator_rig();
    let keyed = || Ctx::new(Actor::Board).with_key("start-1");
    let first = start_orchestrator(&mut rig, &homes, &keyed()).unwrap();
    assert!(!first.replayed);
    // The retry answers with the first, and launches nothing.
    let id = first.value.session.id.clone();
    let workspaces = rig.workspace_ids().len();
    let retry = start_orchestrator(&mut rig, &homes, &keyed()).unwrap();
    assert!(retry.replayed);
    assert_eq!(retry.value.session.id, id);
    assert!(!retry.events.is_empty() && first.events.ends_with(&retry.events));
    assert_eq!(rig.workspace_ids().len(), workspaces, "no second workspace");

    // Once it has ended, as a failed launch ends it, a retry starts afresh.
    rig.fleet.store.session_end(&owner(), &id, "x").unwrap();
    let again = start_orchestrator(&mut rig, &homes, &keyed()).unwrap();
    assert!(!again.replayed);
    assert_ne!(again.value.session.id, id);
    let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha");
    assert_eq!(
        live.unwrap().map(|s| s.id),
        Some(again.value.session.id.clone())
    );
}

/// The fake herdr, calling `hook` after each `create_workspace` and
/// `send_text` it passes on, with the call and its text: another request's
/// write lands mid-flight.
pub(crate) struct Hooked<'a> {
    pub(crate) herdr: &'a FakeHerdr,
    pub(crate) hook: &'a dyn Fn(&str, &str),
}

impl tiller_herdr::client::Herdr for Hooked<'_> {
    fn snapshot(&self) -> tiller_herdr::client::Result<tiller_herdr::client::Snapshot> {
        self.herdr.snapshot()
    }
    fn pane(
        &self,
        pane_id: &str,
    ) -> tiller_herdr::client::Result<Option<tiller_herdr::client::PaneInfo>> {
        self.herdr.pane(pane_id)
    }
    fn read_visible(&self, pane_id: &str) -> tiller_herdr::client::Result<String> {
        self.herdr.read_visible(pane_id)
    }
    fn send_text(&self, pane_id: &str, text: &str) -> tiller_herdr::client::Result<()> {
        self.herdr.send_text(pane_id, text)?;
        (self.hook)("send_text", text);
        Ok(())
    }
    fn send_enter(&self, pane_id: &str) -> tiller_herdr::client::Result<()> {
        self.herdr.send_enter(pane_id)
    }
    fn send_escape(&self, pane_id: &str) -> tiller_herdr::client::Result<()> {
        self.herdr.send_escape(pane_id)
    }
    fn report_tokens(
        &self,
        pane_id: &str,
        tokens: &std::collections::BTreeMap<String, String>,
    ) -> tiller_herdr::client::Result<()> {
        self.herdr.report_tokens(pane_id, tokens)
    }
    fn create_workspace(
        &self,
        label: &str,
        cwd: Option<&Path>,
    ) -> tiller_herdr::client::Result<tiller_herdr::client::Created> {
        let created = self.herdr.create_workspace(label, cwd)?;
        (self.hook)("create_workspace", label);
        Ok(created)
    }
    fn rename_workspace(
        &self,
        workspace_id: &str,
        label: &str,
    ) -> tiller_herdr::client::Result<()> {
        self.herdr.rename_workspace(workspace_id, label)
    }
    fn move_workspace(&self, workspace_id: &str, index: usize) -> tiller_herdr::client::Result<()> {
        self.herdr.move_workspace(workspace_id, index)
    }
    fn close_pane(&self, pane_id: &str) -> tiller_herdr::client::Result<()> {
        self.herdr.close_pane(pane_id)
    }
    fn notify(&self, title: &str, body: &str) -> tiller_herdr::client::Result<()> {
        self.herdr.notify(title, body)
    }
}

#[test]
fn a_keyed_orchestrator_start_that_loses_its_race_closes_its_pane() {
    let (mut rig, homes) = orchestrator_rig();
    let db = rig.fleet.db.clone();
    // Another request's keyed start lands while this one opens its workspace.
    let rival = |call: &str, _: &str| {
        if call != "create_workspace" {
            return;
        }
        let mut store = tiller_core::Store::open(&db).unwrap();
        store
            .session_start(
                &Ctx::new(Actor::Board).with_key("start-1"),
                tiller_core::state::runs::NewOrchestrator {
                    project_id: "alpha".into(),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    let racing = Hooked {
        herdr: &rig.herdr,
        hook: &rival,
    };
    let mut world = world!(rig);
    world.herdr = &racing;
    let keyed = Ctx::new(Actor::Board).with_key("start-1");
    let started =
        orchestrator::start(&mut rig.fleet.store, &keyed, &world, &homes, "alpha", None).unwrap();
    assert!(started.replayed);
    let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
        .unwrap()
        .unwrap();
    assert_eq!(started.value.session.id, live.id, "the rival's start");
    assert!(live.pane_id.is_none());
    // The pane this start opened is closed and nothing was typed into it.
    let calls = rig.herdr.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("close_pane")),
        "{calls:?}"
    );
    assert!(
        !calls.iter().any(|c| c.starts_with("send_text")),
        "{calls:?}"
    );
}

#[test]
fn orchestrator_start_never_types_into_an_old_orchestrator_pane() {
    let (mut rig, homes) = orchestrator_rig();
    // The ended orchestrator's pane shows no agent, its tokens intact: it
    // may hold a shell or any other program, so it is left alone.
    let pane = rig.fleet.orchestrator_pane.clone();
    rig.herdr.set_screen(&pane, Screen::Shell);
    rig.herdr.set_tokens(
        &pane,
        [("project", "alpha"), ("role", "orchestrator")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
    );
    let started = start_orchestrator(&mut rig, &homes, &owner()).unwrap();
    assert_ne!(started.value.pane_id.as_deref(), Some(pane.as_str()));
    assert_ne!(started.value.workspace_id.as_deref(), Some("w1"));
    let typed = format!("send_text {pane} ");
    assert!(!rig.herdr.calls().iter().any(|c| c.starts_with(&typed)));
}

#[test]
fn orchestrator_start_is_the_owner_s_or_the_daemon_s() {
    let (mut rig, homes) = orchestrator_rig();
    let worker = Ctx::new(Actor::Session(rig.fleet.orchestrator_session.clone()));
    let error = start_orchestrator(&mut rig, &homes, &worker).unwrap_err();
    assert!(
        matches!(error, SpawnError::State(tiller_core::Error::Forbidden(_))),
        "{error:?}"
    );
    assert!(rig.herdr.calls().is_empty(), "{:?}", rig.herdr.calls());
}

#[test]
fn orchestrator_start_needs_its_roots_and_a_home_apart() {
    let (mut rig, homes) = orchestrator_rig();
    let checkout = rig.repo.checkout.clone();
    rig.config = config(&checkout, "");
    let error = start_orchestrator(&mut rig, &homes, &owner()).unwrap_err();
    assert!(refusal(&error).contains("spawn.pool_root"), "{error}");

    // A home inside the checkouts root would lose to its deny rule.
    let root = rig.repo.root.path().to_owned();
    rig.config = config(
        &checkout,
        &format!(
            "\n[spawn]\npool_root = {:?}\ncheckouts_root = {:?}\n",
            root.join("pool").to_string_lossy(),
            root.to_string_lossy(),
        ),
    );
    let inside = root.join("homes");
    let error = start_orchestrator(&mut rig, &inside, &owner()).unwrap_err();
    assert!(
        refusal(&error).contains("overlaps spawn.checkouts_root"),
        "{error}"
    );
    assert!(!inside.exists());

    // `..` could hide a home inside a root from `starts_with`.
    let sneaky = root.join("pool").join("..").join("homes");
    let error = start_orchestrator(&mut rig, &sneaky, &owner()).unwrap_err();
    assert!(refusal(&error).contains("without `..`"), "{error}");

    // The orchestrator's scratch would lose to the same deny rules.
    rig.scratch = root.join("pool").join("runs");
    let error = start_orchestrator(&mut rig, &homes, &owner()).unwrap_err();
    assert!(
        refusal(&error).contains("spawn.scratch_root") && refusal(&error).contains("overlaps"),
        "{error}"
    );
    assert!(
        rig.herdr
            .calls()
            .iter()
            .all(|c| !c.starts_with("create_workspace"))
    );
    assert!(
        tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_orchestrator_start_that_cannot_open_a_pane_leaves_no_new_home() {
    let (mut rig, homes) = orchestrator_rig();
    rig.herdr.refuse("create_workspace");
    let error = start_orchestrator(&mut rig, &homes, &owner()).unwrap_err();
    assert!(matches!(error, SpawnError::Herdr(_)), "{error:?}");
    assert!(!homes.join("alpha").exists());

    // A home an earlier orchestrator wrote to is kept.
    std::fs::create_dir_all(homes.join("alpha")).unwrap();
    std::fs::write(homes.join("alpha").join("notes.md"), "keep\n").unwrap();
    start_orchestrator(&mut rig, &homes, &owner()).unwrap_err();
    assert!(homes.join("alpha").join("notes.md").exists());
}

#[test]
fn an_orchestrator_launch_that_fails_ends_its_session_and_closes_its_pane() {
    let (mut rig, homes) = orchestrator_rig();
    rig.herdr.refuse("send_text");
    let before = rig.workspace_ids();
    let error = start_orchestrator(&mut rig, &homes, &owner()).unwrap_err();
    assert!(matches!(error, SpawnError::Herdr(_)), "{error:?}");
    let calls = rig.herdr.calls();
    assert!(
        calls.iter().any(|c| c.starts_with("close_pane")),
        "{calls:?}"
    );
    assert!(
        tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
            .unwrap()
            .is_none()
    );
    let reason: String = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT end_reason FROM sessions WHERE role = 'orchestrator' ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reason, orchestrator::LAUNCH_FAILED);
    assert_eq!(rig.workspace_ids(), before, "the new workspace is gone");
    assert!(!rig.scratch.exists() || std::fs::read_dir(&rig.scratch).unwrap().next().is_none());
}

fn delivery_project(delivery: &str, stage: &str) -> Project {
    serde_json::from_value(serde_json::json!({
        "id": "alpha",
        "name": "Alpha",
        "repo": "example/alpha",
        "remote": null,
        "default_branch": "main",
        "delivery": delivery,
        "checks": [],
        "capacity": 1,
        "deploy_workflow": null,
        "deploy_environment": null,
        "deploy_timeout_minutes": 30,
        "merge_gate": "detect",
        "owner_host_id": "host",
        "added_at": "2026-11-02T10:00:00Z",
        "next_card_seq": 1,
        "retired_at": null,
        "rev": 1,
        "stage": stage,
        "reviewed_through_sha": null,
        "stage_changed_at": null,
        "edited_at": "2026-11-02T10:00:00Z",
    }))
    .unwrap()
}

#[test]
fn a_direct_pr_brief_tells_the_worker_to_commit_and_never_push() {
    let commit_only = "Commit only, never push: describe the PR with `tiller pr describe`";
    for (delivery, stage) in [("direct-pr", "launched"), ("no-mistakes", "pre-launch")] {
        let line = delivery_line(&delivery_project(delivery, stage));
        assert!(line.contains("Delivery: direct-pr."), "{line}");
        assert!(line.contains(commit_only), "{line}");
        assert!(
            line.contains("the daemon pushes the branch and opens the PR"),
            "{line}"
        );
    }
    for (delivery, stage) in [("no-mistakes", "launched"), ("local-only", "pre-launch")] {
        let line = delivery_line(&delivery_project(delivery, stage));
        assert!(!line.contains(commit_only), "{line}");
    }
}

#[test]
fn respawn_storm_trips_rate_limit_not_host() {
    let mut rig = Rig::new();
    let ctx = rig.orchestrator();
    let cards: Vec<Card> = (0..6).map(|_| rig.card(CardKind::Ship, None)).collect();
    // Each run ends at once, so lanes are free and only the rate can refuse.
    for card in &cards[..5] {
        let spawned = rig.spawn(card, RunRole::Builder);
        rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    }
    let taken = rig.pool.taken().len();
    let error = rig
        .try_spawn(&ctx, Rig::request(&cards[5], RunRole::Builder))
        .unwrap_err();
    let why = refusal(&error);
    assert!(why.contains("spawn rate limit"), "{error}");
    assert!(why.contains("5 spawns in the last 10 minutes"), "{error}");
    // Refused before anything was leased, and the host itself is fine.
    assert_eq!(rig.pool.taken().len(), taken);
    let degraded: i64 = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM events WHERE kind = 'host.degraded'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(degraded, 0);

    // Launches before the window no longer count.
    rig.sql("UPDATE sessions SET started_at = '2026-01-01T00:00:00Z'");
    rig.try_spawn(&ctx, Rig::request(&cards[5], RunRole::Builder))
        .unwrap();
}

/// A review page the run published, recorded as the worker would.
fn publish_page(rig: &mut Rig, card: &Card, run: &str, path: &str) -> Artifact {
    rig.fleet
        .store
        .artifact_add(
            &owner(),
            NewArtifact {
                card: card.display_id.clone(),
                run_id: Some(run.to_owned()),
                kind: ArtifactKind::Page,
                title: "Review".into(),
                url: None,
                path: Some(path.to_owned()),
            },
        )
        .unwrap()
        .value
}

fn artifact(rig: &Rig, id: &str) -> Artifact {
    get(rig.fleet.store.conn(), "artifact", id).unwrap()
}

#[test]
fn cleanup_retires_task_review_listeners() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let other = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let neighbour = rig.spawn(&other, RunRole::Builder);
    let run = spawned.run.id.clone();
    let page = publish_page(&mut rig, &card, &run, "/home/user/.lavish/plan.html");
    let second = publish_page(&mut rig, &card, &run, "/home/user/.lavish/diff.html");
    let theirs = publish_page(
        &mut rig,
        &other,
        &neighbour.run.id,
        "/home/user/.lavish/theirs.html",
    );
    rig.done(&run, RunOutcome::Abandoned).unwrap();
    rig.herdr.close_pane(&spawned.pane_id).unwrap();

    // The review server is down: the pages stay live, and say so.
    rig.listeners.refuse(true);
    let cleaned = rig.cleanup(&run).unwrap();
    assert_eq!(cleaned.warnings.len(), 2, "{:?}", cleaned.warnings);
    assert!(
        cleaned.warnings[0].contains("plan.html"),
        "{:?}",
        cleaned.warnings
    );
    assert!(artifact(&rig, &page.id).retired_at.is_none());

    // Cleaning again once it answers retires them, and only the run's own.
    rig.listeners.refuse(false);
    let again = rig.cleanup(&run).unwrap();
    assert!(again.warnings.is_empty(), "{:?}", again.warnings);
    assert_eq!(
        rig.listeners.ended(),
        vec![
            PathBuf::from("/home/user/.lavish/plan.html"),
            PathBuf::from("/home/user/.lavish/diff.html"),
        ]
    );
    for id in [&page.id, &second.id] {
        assert!(artifact(&rig, id).retired_at.is_some());
        assert_eq!(rig.events("artifact.retired", id), 1);
    }
    assert!(artifact(&rig, &theirs.id).retired_at.is_none());

    // A third cleanup has nothing left to end.
    rig.cleanup(&run).unwrap();
    assert_eq!(rig.listeners.ended().len(), 2);
}

#[test]
fn worker_env_sets_review_allowed_hosts() {
    let mut rig = Rig::with_config(
        "\n[spawn]\nreview_allowed_hosts = [\"board.example.com\", \"192.0.2.10\"]\n",
    );
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let dir = rig.scratch.join(&spawned.run.id);
    let script = std::fs::read_to_string(dir.join("launch.sh")).unwrap();
    assert!(
        script.contains("LAVISH_AXI_ALLOWED_HOSTS='board.example.com 192.0.2.10'"),
        "{script}"
    );

    // A relaunch and a resume start the review server the same way.
    let run = rig.run(&spawned.run.id);
    let again = launch::AgainSpec {
        role: roles::Role::Builder,
        session_id: "01J0000000000000000000RELA",
        run_id: &run.id,
        worktree: Path::new(&spawned.worktree),
        scratch: &dir,
        tiller_home: &rig.tiller_home,
        model: &run.model,
        effort: run.effort,
        claude: &rig.config.spawn.claude,
        claude_config: &rig.claude,
        review_hosts: &rig.config.spawn.review_allowed_hosts,
    };
    launch::write_again(&again, launch::Start::Fresh { note: "again" }).unwrap();
    let relaunch =
        std::fs::read_to_string(dir.join("relaunch-01J0000000000000000000RELA.sh")).unwrap();
    assert!(
        relaunch.contains("LAVISH_AXI_ALLOWED_HOSTS='board.example.com 192.0.2.10'"),
        "{relaunch}"
    );

    // With none configured, the launch sets nothing and the server keeps its
    // loopback default.
    let mut plain = Rig::new();
    let card = plain.card(CardKind::Ship, None);
    let spawned = plain.spawn(&card, RunRole::Builder);
    let script =
        std::fs::read_to_string(plain.scratch.join(&spawned.run.id).join("launch.sh")).unwrap();
    assert!(!script.contains("LAVISH_AXI_ALLOWED_HOSTS"), "{script}");
}

#[test]
fn review_allowed_hosts_names_hostnames() {
    for bad in ["\"\"", "\"a.example.com b.example.com\""] {
        let table: toml::Table = format!("[spawn]\nreview_allowed_hosts = [{bad}]\n")
            .parse()
            .unwrap();
        let error = SpawnConfig::from_table(&table).unwrap_err();
        assert!(
            error.to_string().contains("review_allowed_hosts"),
            "{error}"
        );
    }
}

#[test]
fn a_direct_push_brief_lists_the_checks_and_the_lighter_set() {
    let mut project = delivery_project("direct-push", "launched");
    project.checks = serde_json::json!({
        "full": ["cargo test", "cargo clippy"],
        "no_logic": ["cargo fmt --check"],
    });
    let line = delivery_line(&project);
    assert!(line.contains("Delivery: direct-push."), "{line}");
    assert!(line.contains("Commit only, never push"), "{line}");
    assert!(line.contains("`cargo test`, `cargo clippy`"), "{line}");
    assert!(
        line.contains(
            "touches no logic (docs, comments, copy, formatting) runs only: `cargo fmt --check`"
        ),
        "{line}"
    );

    project.checks = serde_json::json!([]);
    let line = delivery_line(&project);
    assert!(line.contains("names no checks"), "{line}");

    let line = delivery_line(&delivery_project("direct-pr", "launched"));
    assert!(!line.contains("watches the deploy"), "{line}");
}

// -- stale sessions ------------------------------------------------------------

fn quick_waits() -> crate::control::Waits {
    crate::control::Waits {
        interrupt: std::time::Duration::from_secs(2),
        exit: std::time::Duration::from_secs(2),
        bind: std::time::Duration::from_secs(2),
        poll: std::time::Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    }
}

/// Starts the orchestrator, then makes it look launched by an older tiller
/// that is `activity` now.
fn stale_orchestrator(rig: &mut Rig, homes: &Path, activity: &str) -> String {
    let started = start_orchestrator(rig, homes, &owner()).unwrap().value;
    let pane = started.pane_id.clone().unwrap();
    rig.herdr.set_screen(&pane, Screen::Idle);
    rig.fleet.bind(&started.session.id, &pane);
    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE sessions SET tiller_version = '0.0.1', activity = ?2,
                    activity_observed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?1",
            rusqlite::params![started.session.id, activity],
        )
        .unwrap();
    started.session.id
}

#[test]
fn a_launch_records_the_version_and_hash_it_wrote() {
    let (mut rig, homes) = orchestrator_rig();
    let started = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let session: Session = get(rig.fleet.store.conn(), "session", &started.session.id).unwrap();
    let now = crate::stale::current(crate::roles::Role::Orchestrator);
    assert_eq!(session.tiller_version.as_deref(), Some(now.tiller_version));
    assert_eq!(
        session.launch_hash.as_deref(),
        Some(now.launch_hash.as_str())
    );
    assert_eq!(crate::stale::staleness(&session), None);
}

#[test]
fn a_session_is_stale_when_its_version_or_hash_differs_or_it_has_no_record() {
    let (mut rig, homes) = orchestrator_rig();
    let id = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value
        .session
        .id;
    let stale = |rig: &Rig| {
        let session: Session = get(rig.fleet.store.conn(), "session", &id).unwrap();
        crate::stale::staleness(&session)
    };
    let set = |rig: &Rig, sql: &str| rig.fleet.store.conn().execute(sql, []).unwrap();

    set(&rig, "UPDATE sessions SET tiller_version = '0.0.1'");
    let old = stale(&rig).unwrap();
    assert_eq!((old.since.as_str(), old.prompt_changed), ("0.0.1", false));

    // The same version with another role prompt or hook settings.
    let current = crate::stale::current_version();
    set(
        &rig,
        &format!("UPDATE sessions SET tiller_version = '{current}', launch_hash = 'old'"),
    );
    let changed = stale(&rig).unwrap();
    assert_eq!(
        (changed.since.as_str(), changed.prompt_changed),
        (current, true)
    );

    // A launch from before the record existed.
    set(
        &rig,
        "UPDATE sessions SET tiller_version = NULL, launch_hash = NULL",
    );
    let unknown = stale(&rig).unwrap();
    assert_eq!(unknown.since, crate::stale::UNKNOWN);
}

#[test]
fn an_idle_stale_orchestrator_restarts_now_on_the_current_launch() {
    let (mut rig, homes) = orchestrator_rig();
    let old = stale_orchestrator(&mut rig, &homes, "idle");

    let listed = crate::stale::stale_sessions(&rig.fleet.store).unwrap();
    let item = listed.iter().find(|item| item.session.id == old).unwrap();
    assert!(item.idle);

    let waits = quick_waits();
    let world = world!(rig);
    let reports = crate::stale::restart_stale(
        &mut rig.fleet.store,
        &Ctx::new(Actor::Board),
        Some(std::slice::from_ref(&old)),
        &mut |store, ctx, session| {
            orchestrator::start(
                store,
                ctx,
                &world,
                &homes,
                &session.project_id,
                Some(&waits),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        },
    )
    .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].outcome, crate::stale::Outcome::Restarted);

    let old: Session = get(rig.fleet.store.conn(), "session", &old).unwrap();
    assert_eq!(old.end_reason.as_deref(), Some(orchestrator::RESTARTED));
    let live = crate::stale::stale_sessions(&rig.fleet.store).unwrap();
    assert!(
        live.iter()
            .all(|item| item.session.role != tiller_core::domain::Role::Orchestrator),
        "the new orchestrator is on the current version"
    );
}

#[test]
fn a_working_stale_orchestrator_is_queued_and_restarts_once_idle() {
    let (mut rig, homes) = orchestrator_rig();
    let id = stale_orchestrator(&mut rig, &homes, "working");
    let waits = quick_waits();
    let ctx = Ctx::new(Actor::Board);

    let world = world!(rig);
    let restarts = std::cell::Cell::new(0);
    let mut restart = |store: &mut Store, ctx: &Ctx, session: &Session| {
        restarts.set(restarts.get() + 1);
        orchestrator::start(
            store,
            ctx,
            &world,
            &homes,
            &session.project_id,
            Some(&waits),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    };

    let reports =
        crate::stale::restart_stale(&mut rig.fleet.store, &ctx, None, &mut restart).unwrap();
    let report = reports
        .iter()
        .find(|report| report.session.id == id)
        .unwrap();
    assert_eq!(report.outcome, crate::stale::Outcome::Queued);
    assert_eq!(restarts.get(), 0, "a working session is not restarted");

    // The queue is in state.db: a reopened store still holds it.
    let reopened = Store::open_current(&rig.fleet.db).unwrap();
    let queued = reopened.restart_queued().unwrap();
    assert!(queued.iter().any(|session| session.id == id));
    drop(reopened);

    // Still working: a pass restarts nothing.
    assert!(
        crate::stale::drain_queue(&mut rig.fleet.store, &ctx, &mut restart)
            .unwrap()
            .is_empty()
    );

    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE sessions SET activity = 'idle' WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
    let drained = crate::stale::drain_queue(&mut rig.fleet.store, &ctx, &mut restart).unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].outcome, crate::stale::Outcome::Restarted);
    assert_eq!(restarts.get(), 1);
    assert!(rig.fleet.store.restart_queued().unwrap().is_empty());
}

#[test]
fn a_queued_restart_that_fails_is_dropped_and_shows_as_stale_again() {
    let (mut rig, homes) = orchestrator_rig();
    let id = stale_orchestrator(&mut rig, &homes, "working");
    let ctx = Ctx::new(Actor::Board);
    let mut fail = |_: &mut Store, _: &Ctx, _: &Session| Err("herdr is not running".to_owned());

    crate::stale::restart_stale(&mut rig.fleet.store, &ctx, None, &mut fail).unwrap();
    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE sessions SET activity = 'idle' WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();
    let drained = crate::stale::drain_queue(&mut rig.fleet.store, &ctx, &mut fail).unwrap();
    assert_eq!(
        drained[0].outcome,
        crate::stale::Outcome::Failed("herdr is not running".to_owned())
    );
    assert!(rig.fleet.store.restart_queued().unwrap().is_empty());
    let session: Session = get(rig.fleet.store.conn(), "session", &id).unwrap();
    assert!(session.restart_queued_at.is_none());
    assert!(crate::stale::staleness(&session).is_some());
}

/// A worker launch renders the approved memory that loads for its role and
/// project, fleet document first, and records what it rendered: a document
/// of the other boundary, one scoped to another role, an unapproved
/// revision and the working tree never reach the prompt (`memory.md`
/// sections 2, 7 and 8).
#[test]
fn spawn_renders_the_approved_memory_and_records_the_load() {
    let mut rig = Rig::new();
    let home = rig.fleet.dir.path().join("tiller-home");
    let repo = home.join("memory");
    std::fs::create_dir_all(repo.join("projects")).unwrap();
    git(&repo, &["init", "-q"]);
    // Writes `text` at `path` in the memory repo and returns its blob.
    let blob = |path: &str, text: &str| {
        std::fs::write(repo.join(path), text).unwrap();
        git(&repo, &["hash-object", "-w", path])
    };
    let fleet = blob("fleet.md", "## How we work here\n\n- Fleet line.\n");
    let alpha = blob(
        "projects/alpha.md",
        "## How we work here\n\n- Alpha line.\n",
    );
    let later = blob(
        "projects/alpha.md",
        "## How we work here\n\n- Unapproved line.\n",
    );
    let work = blob("projects/alpha-work.md", "- Work line.\n");
    let scoped = blob("projects/alpha-orch.md", "- Orchestrator line.\n");
    // The working tree moved on after every blob was written.
    std::fs::write(repo.join("projects/alpha.md"), "- Worktree line.\n").unwrap();
    rig.tiller_home = home;

    let source = |id: &str, project: &str, boundary: &str, path: &str, roles: &str| {
        rig.sql(&format!(
            "INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo,
                 canonical_path, apply_method, load_mode, roles, added_at)
             VALUES ('{id}', '{project}', '{boundary}', 'tiller_doc', '~/.tiller/memory/{path}',
                 '~/.tiller/memory', '{path}', 'tiller_commit', 'rendered', '{roles}',
                 '2026-11-02T09:00:00.000Z')"
        ));
    };
    let revision = |id: &str, source: &str, blob: &str, review: &str, at: &str| {
        rig.sql(&format!(
            "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub,
                 recorded_at, recorded_by)
             VALUES ('{id}', '{source}', '{blob}', '{review}', 'baseline', 'passed', '{at}', 'daemon')"
        ));
    };
    let all = r#"["orchestrator","scout","builder","reviewer","verifier"]"#;
    source("S-fleet", "fleet", "personal", "fleet.md", all);
    source("S-alpha", "alpha", "personal", "projects/alpha.md", all);
    source("S-work", "alpha", "work", "projects/alpha-work.md", all);
    source(
        "S-orch",
        "alpha",
        "personal",
        "projects/alpha-orch.md",
        r#"["orchestrator"]"#,
    );
    revision(
        "R-fleet",
        "S-fleet",
        &fleet,
        "approved",
        "2026-11-02T09:00:00.000Z",
    );
    revision(
        "R-alpha",
        "S-alpha",
        &alpha,
        "approved",
        "2026-11-02T09:00:00.000Z",
    );
    revision(
        "R-later",
        "S-alpha",
        &later,
        "unreviewed",
        "2026-11-02T10:00:00.000Z",
    );
    revision(
        "R-work",
        "S-work",
        &work,
        "approved",
        "2026-11-02T09:00:00.000Z",
    );
    revision(
        "R-orch",
        "S-orch",
        &scoped,
        "approved",
        "2026-11-02T09:00:00.000Z",
    );

    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let prompt =
        std::fs::read_to_string(rig.scratch.join(&spawned.run.id).join("prompt.md")).unwrap();
    let memory = prompt.find("\n# Memory\n\n## fleet.md\n\n## How we work here\n\n- Fleet line.\n");
    let project = prompt.find("\n## projects/alpha.md\n\n## How we work here\n\n- Alpha line.\n");
    let brief = prompt.find("\n# Your brief\n\n");
    assert!(
        matches!((memory, project, brief), (Some(m), Some(p), Some(b)) if m < p && p < b),
        "{prompt}"
    );
    for absent in [
        "Unapproved line",
        "Worktree line",
        "Work line",
        "Orchestrator line",
    ] {
        assert!(!prompt.contains(absent), "{absent}: {prompt}");
    }
    // The brief still reads back for a relaunch.
    assert_eq!(
        launch::brief_in(&rig.scratch.join(&spawned.run.id))
            .map(|b| b.starts_with("Do the work on")),
        Some(true)
    );

    let loads: Vec<(String, String)> = rig
        .fleet
        .store
        .conn()
        .prepare(
            "SELECT revision_id, trigger FROM session_loads WHERE session_id = ?1
             ORDER BY revision_id",
        )
        .unwrap()
        .query_map([&spawned.session.id], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        loads,
        [
            ("R-alpha".to_owned(), "startup".to_owned()),
            ("R-fleet".to_owned(), "startup".to_owned())
        ]
    );
    assert_eq!(rig.events("session.memory_loaded", &spawned.session.id), 1);

    // The manifest's memory part is the rendered pairs.
    let hash = rig.run(&spawned.run.id).config_hash.unwrap();
    let manifest: tiller_core::domain::ConfigManifest =
        tiller_core::state::find(rig.fleet.store.conn(), "config_hash = ?1", &[&hash])
            .unwrap()
            .unwrap();
    let rendered = launch::manifest(
        crate::roles::Role::Builder,
        b"{}",
        "m",
        Effort::High,
        None,
        &[("S-fleet".into(), fleet), ("S-alpha".into(), alpha)],
    );
    assert_eq!(manifest.parts["memory"], rendered["memory"]);

    // On a work host the personal documents never load; the work one does.
    rig.sql("UPDATE meta SET value = 'work' WHERE key = 'boundary'");
    let second = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&second, RunRole::Builder);
    let prompt =
        std::fs::read_to_string(rig.scratch.join(&spawned.run.id).join("prompt.md")).unwrap();
    assert!(
        prompt.contains("\n# Memory\n\n## projects/alpha-work.md\n\n- Work line.\n"),
        "{prompt}"
    );
    for absent in ["Fleet line", "Alpha line"] {
        assert!(!prompt.contains(absent), "{absent}: {prompt}");
    }
}

/// The payload of the session's `session.memory_loaded` event.
fn memory_loaded(rig: &Rig, session: &str) -> serde_json::Value {
    let payload: String = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'session.memory_loaded' AND entity_id = ?1",
            [session],
            |row| row.get(0),
        )
        .unwrap();
    serde_json::from_str(&payload).unwrap()
}

/// A document with no approved revision does not refuse the launch: the
/// session starts without it, and `session.memory_loaded` names the
/// document it skipped and why (`memory.md` sections 2 and 9). An emptied
/// baseline skips nothing, since it has no text to load.
#[test]
fn spawn_skips_a_document_with_no_approved_revision_and_records_the_skip() {
    let mut rig = Rig::new();
    let home = rig.fleet.dir.path().join("tiller-home");
    let repo = home.join("memory");
    std::fs::create_dir_all(repo.join("projects")).unwrap();
    git(&repo, &["init", "-q"]);
    let blob = |path: &str, text: &str| {
        std::fs::write(repo.join(path), text).unwrap();
        git(&repo, &["hash-object", "-w", path])
    };
    let fleet = blob("fleet.md", "## How we work here\n\n- Fleet line.\n");
    let alpha = blob(
        "projects/alpha.md",
        "## How we work here\n\n- Unreviewed line.\n",
    );
    rig.tiller_home = home;
    let all = r#"["orchestrator","scout","builder","reviewer","verifier"]"#;
    for (id, project, path) in [
        ("S-fleet", "fleet", "fleet.md"),
        ("S-alpha", "alpha", "projects/alpha.md"),
        ("S-empty", "alpha", "projects/alpha-empty.md"),
    ] {
        rig.sql(&format!(
            "INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo,
                 canonical_path, apply_method, load_mode, roles, added_at)
             VALUES ('{id}', '{project}', 'personal', 'tiller_doc', '~/.tiller/memory/{path}',
                 '~/.tiller/memory', '{path}', 'tiller_commit', 'rendered', '{all}',
                 '2026-11-02T09:00:00.000Z')"
        ));
    }
    for (id, source, blob, review, provenance) in [
        ("R-fleet", "S-fleet", fleet.as_str(), "approved", "baseline"),
        (
            "R-alpha",
            "S-alpha",
            alpha.as_str(),
            "unreviewed",
            "external",
        ),
        (
            "R-empty",
            "S-empty",
            tiller_core::import::EMPTY_BLOB,
            "unreviewed",
            "baseline",
        ),
    ] {
        rig.sql(&format!(
            "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub,
                 recorded_at, recorded_by)
             VALUES ('{id}', '{source}', '{blob}', '{review}', '{provenance}', 'passed',
                 '2026-11-02T09:00:00.000Z', 'daemon')"
        ));
    }

    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let prompt =
        std::fs::read_to_string(rig.scratch.join(&spawned.run.id).join("prompt.md")).unwrap();
    assert!(prompt.contains("- Fleet line."), "{prompt}");
    assert!(!prompt.contains("Unreviewed line"), "{prompt}");
    let payload = memory_loaded(&rig, &spawned.session.id);
    assert_eq!(payload["revision_ids"], serde_json::json!(["R-fleet"]));
    assert_eq!(
        payload["skipped"],
        serde_json::json!([{
            "source_id": "S-alpha",
            "project_id": "alpha",
            "path": "projects/alpha.md",
            "reason": "it has no approved revision",
        }])
    );

    // With nothing approved to load, the skip is still recorded.
    rig.sql("UPDATE memory_revisions SET review = 'unreviewed' WHERE id = 'R-fleet'");
    let second = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&second, RunRole::Builder);
    let payload = memory_loaded(&rig, &spawned.session.id);
    assert_eq!(payload["revision_ids"], serde_json::json!([]));
    let skipped: Vec<&str> = payload["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    assert_eq!(skipped, ["fleet.md", "projects/alpha.md"]);
    let loads: i64 = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT count(*) FROM session_loads WHERE session_id = ?1",
            [&spawned.session.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(loads, 0);
}

/// A blob the memory repo cannot give refuses the launch rather than start
/// a session without text the record would say it got.
#[test]
fn spawn_refuses_an_approved_blob_it_cannot_read() {
    let mut rig = Rig::new();
    let home = rig.fleet.dir.path().join("tiller-home");
    std::fs::create_dir_all(home.join("memory")).unwrap();
    git(&home.join("memory"), &["init", "-q"]);
    rig.tiller_home = home;
    rig.sql(
        "INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo,
             canonical_path, apply_method, load_mode, added_at)
         VALUES ('S-alpha', 'alpha', 'personal', 'tiller_doc', '~/.tiller/memory/projects/alpha.md',
             '~/.tiller/memory', 'projects/alpha.md', 'tiller_commit', 'rendered',
             '2026-11-02T09:00:00.000Z')",
    );
    rig.sql(
        "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub,
             recorded_at, recorded_by)
         VALUES ('R-alpha', 'S-alpha', '0123456789abcdef0123456789abcdef01234567', 'approved',
             'baseline', 'passed', '2026-11-02T09:00:00.000Z', 'daemon')",
    );
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(
        refusal(&error).contains("cannot be read"),
        "{}",
        refusal(&error)
    );
    let loads: i64 = rig
        .fleet
        .store
        .conn()
        .query_row("SELECT count(*) FROM session_loads", [], |row| row.get(0))
        .unwrap();
    assert_eq!(loads, 0);
}

/// T6: one board action has one actor. The board's stale restart queues a
/// working orchestrator as the board, and the restart that drains it ends
/// the old session and starts the new one as the board too.
#[test]
fn a_board_stale_restart_records_the_board_from_queue_to_restart() {
    let (mut rig, homes) = orchestrator_rig();
    let old = stale_orchestrator(&mut rig, &homes, "working");
    let waits = quick_waits();
    let board = Ctx::new(Actor::Board);
    let world = world!(rig);
    let mut restart = |store: &mut Store, ctx: &Ctx, session: &Session| {
        orchestrator::start(
            store,
            ctx,
            &world,
            &homes,
            &session.project_id,
            Some(&waits),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    };
    crate::stale::restart_stale(&mut rig.fleet.store, &board, None, &mut restart).unwrap();
    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE sessions SET activity = 'idle' WHERE id = ?1",
            rusqlite::params![old],
        )
        .unwrap();
    let drained = crate::stale::drain_queue(&mut rig.fleet.store, &board, &mut restart).unwrap();
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].outcome,
        crate::stale::Outcome::Restarted,
        "{:?}",
        drained[0].outcome
    );

    let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
        .unwrap()
        .unwrap();
    assert_ne!(live.id, old);
    let actors: Vec<(String, String)> = {
        let mut statement = rig
            .fleet
            .store
            .conn()
            .prepare(
                "SELECT kind, actor FROM events
                 WHERE kind IN ('session.restart_queued', 'session.ended', 'session.started')
                   AND entity_id IN (?1, ?2)
                   AND seq >= (SELECT min(seq) FROM events WHERE kind = 'session.restart_queued')
                 ORDER BY seq",
            )
            .unwrap();
        statement
            .query_map(rusqlite::params![old, live.id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let kinds: Vec<&str> = actors.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        kinds,
        ["session.restart_queued", "session.ended", "session.started"],
        "{actors:?}"
    );
    assert!(
        actors.iter().all(|(_, actor)| actor == "board"),
        "one action, one actor: {actors:?}"
    );
}

/// The board restarts an orchestrator as the owner does; a peer's board,
/// the daemon and a session may not, and their refusal stops nothing.
#[test]
fn orchestrator_restart_is_the_owners_or_this_boards() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let pane = first.pane_id.clone().unwrap();
    rig.herdr.set_screen(&pane, Screen::Idle);
    rig.fleet.bind(&first.session.id, &pane);
    let waits = quick_waits();
    let world = world!(rig);
    for actor in [
        Actor::Peer("board-host".into()),
        Actor::Daemon,
        Actor::Session(first.session.id.clone()),
    ] {
        let error = orchestrator::start(
            &mut rig.fleet.store,
            &Ctx::new(actor.clone()),
            &world,
            &homes,
            "alpha",
            Some(&waits),
        )
        .unwrap_err();
        assert!(error.to_string().contains("may not"), "{actor}: {error}");
        // Refused before the stop: the live orchestrator keeps running.
        let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
            .unwrap()
            .unwrap();
        assert_eq!(live.id, first.session.id);
    }
    let restarted = orchestrator::start(
        &mut rig.fleet.store,
        &Ctx::new(Actor::Board),
        &world,
        &homes,
        "alpha",
        Some(&waits),
    )
    .unwrap();
    assert_ne!(restarted.value.session.id, first.session.id);
    let old: Session = get(rig.fleet.store.conn(), "session", &first.session.id).unwrap();
    assert_eq!(old.end_reason.as_deref(), Some(orchestrator::RESTARTED));
}

/// The daemon drains every queue, but a restart the owner queued from a
/// terminal is still the owner's: the drain restarts as whoever queued it.
#[test]
fn a_drained_restart_records_whoever_queued_it() {
    let (mut rig, homes) = orchestrator_rig();
    let old = stale_orchestrator(&mut rig, &homes, "working");
    let waits = quick_waits();
    let world = world!(rig);
    let asked = std::cell::RefCell::new(Vec::new());
    let mut restart = |store: &mut Store, ctx: &Ctx, session: &Session| {
        asked.borrow_mut().push(ctx.actor.to_string());
        orchestrator::start(
            store,
            ctx,
            &world,
            &homes,
            &session.project_id,
            Some(&waits),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    };
    crate::stale::restart_stale(&mut rig.fleet.store, &owner(), None, &mut restart).unwrap();
    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE sessions SET activity = 'idle' WHERE id = ?1",
            rusqlite::params![old],
        )
        .unwrap();
    let drained =
        crate::stale::drain_queue(&mut rig.fleet.store, &Ctx::new(Actor::Board), &mut restart)
            .unwrap();
    assert_eq!(drained[0].outcome, crate::stale::Outcome::Restarted);
    assert_eq!(*asked.borrow(), ["owner"]);
    let ended: String = rig
        .fleet
        .store
        .conn()
        .query_row(
            "SELECT actor FROM events WHERE kind = 'session.ended' AND entity_id = ?1",
            [&old],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ended, "owner");
}

/// Only the board's own queue restarts as the board. Any other queue,
/// the daemon's among them, restarts as the owner, as every drained
/// restart did before T6: the drain never runs as an actor that may not
/// restart an orchestrator.
#[test]
fn a_restart_the_daemon_queued_drains_as_the_owner() {
    let (mut rig, homes) = orchestrator_rig();
    let old = stale_orchestrator(&mut rig, &homes, "working");
    let waits = quick_waits();
    let world = world!(rig);
    let asked = std::cell::RefCell::new(Vec::new());
    let mut restart = |store: &mut Store, ctx: &Ctx, session: &Session| {
        asked.borrow_mut().push(ctx.actor.to_string());
        orchestrator::start(
            store,
            ctx,
            &world,
            &homes,
            &session.project_id,
            Some(&waits),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    };
    let daemon = Ctx::new(Actor::Daemon);
    crate::stale::restart_stale(&mut rig.fleet.store, &daemon, None, &mut restart).unwrap();
    rig.fleet
        .store
        .conn()
        .execute(
            "UPDATE sessions SET activity = 'idle' WHERE id = ?1",
            rusqlite::params![old],
        )
        .unwrap();
    let drained = crate::stale::drain_queue(&mut rig.fleet.store, &daemon, &mut restart).unwrap();
    assert_eq!(drained[0].outcome, crate::stale::Outcome::Restarted);
    assert_eq!(*asked.borrow(), ["owner"]);
}

// -- memory a launch renders ---------------------------------------------------

impl Rig {
    /// Gives the rig a memory repo under its own Tiller home with one
    /// approved alpha document for every role, its text `text`. Returns the
    /// revision.
    pub(crate) fn approved_memory(&mut self, text: &str) -> String {
        let repo = self.memory_repo();
        std::fs::write(repo.join("projects/alpha.md"), text).unwrap();
        let blob = git(&repo, &["hash-object", "-w", "projects/alpha.md"]);
        self.memory_revision(&blob)
    }

    /// Records an approved alpha revision whose blob the memory repo does
    /// not have, as after a gc or a re-clone.
    pub(crate) fn unreadable_memory(&mut self) -> String {
        self.memory_repo();
        self.memory_revision("0123456789abcdef0123456789abcdef01234567")
    }

    fn memory_repo(&mut self) -> PathBuf {
        let home = self.fleet.dir.path().join("tiller-home");
        let repo = home.join("memory");
        if !repo.join(".git").exists() {
            std::fs::create_dir_all(repo.join("projects")).unwrap();
            git(&repo, &["init", "-q"]);
        }
        self.tiller_home = home;
        self.sql(
            r#"INSERT OR IGNORE INTO memory_sources (id, project_id, boundary, kind, loaded_path,
                 canonical_repo, canonical_path, apply_method, load_mode, roles, added_at)
             VALUES ('S-alpha', 'alpha', 'personal', 'tiller_doc',
                 '~/.tiller/memory/projects/alpha.md', '~/.tiller/memory', 'projects/alpha.md',
                 'tiller_commit', 'rendered',
                 '["orchestrator","scout","builder","reviewer","verifier"]',
                 '2026-11-02T09:00:00.000Z')"#,
        );
        repo
    }

    /// Records the newest approved revision of alpha's document, at `blob`.
    fn memory_revision(&self, blob: &str) -> String {
        let count: i64 = self
            .fleet
            .store
            .conn()
            .query_row("SELECT count(*) FROM memory_revisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        let id = format!("R-{:04}", count + 1);
        self.sql(&format!(
            "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub,
                 recorded_at, recorded_by)
             VALUES ('{id}', 'S-alpha', '{blob}', 'approved', 'baseline', 'passed',
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+{count} seconds'), 'daemon')"
        ));
        id
    }

    /// Every `session_loads` row in the store.
    pub(crate) fn all_loads(&self) -> i64 {
        self.fleet
            .store
            .conn()
            .query_row("SELECT count(*) FROM session_loads", [], |row| row.get(0))
            .unwrap()
    }
}

/// A spawn whose launch line never went in records no load: "Loaded by"
/// lists only sessions that got the prompt (`memory.md` section 7).
#[test]
fn a_spawn_that_fails_after_the_render_records_no_load() {
    let mut rig = Rig::new();
    rig.approved_memory("- Alpha line.\n");
    rig.herdr.refuse("send_text");
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(error, SpawnError::Herdr(_)), "{error}");
    assert_eq!(rig.all_loads(), 0);
}

/// The same for an orchestrator whose launch line never went in.
#[test]
fn an_orchestrator_start_that_fails_after_the_render_records_no_load() {
    let (mut rig, homes) = orchestrator_rig();
    rig.approved_memory("- Alpha line.\n");
    rig.herdr.refuse("send_text");
    start_orchestrator(&mut rig, &homes, &owner())
        .map(|_| ())
        .unwrap_err();
    assert_eq!(rig.all_loads(), 0);
}

/// A restart whose memory cannot be rendered refuses before the stop, so
/// the live orchestrator keeps running.
#[test]
fn orchestrator_restart_refused_by_its_memory_keeps_the_live_one() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let pane = first.pane_id.clone().unwrap();
    rig.herdr.set_screen(&pane, Screen::Idle);
    rig.fleet.bind(&first.session.id, &pane);
    rig.unreadable_memory();

    let waits = quick_waits();
    let world = world!(rig);
    let error = orchestrator::start(
        &mut rig.fleet.store,
        &owner(),
        &world,
        &homes,
        "alpha",
        Some(&waits),
    )
    .unwrap_err();
    assert!(refusal(&error).contains("cannot be read"), "{error}");
    let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
        .unwrap()
        .unwrap();
    assert_eq!(live.id, first.session.id);
    assert!(
        !rig.herdr.calls().iter().any(|c| c.contains("/exit")),
        "the live orchestrator was sent /exit: {:?}",
        rig.herdr.calls()
    );
}

/// A first start whose memory cannot be rendered leaves no home behind.
#[test]
fn orchestrator_start_refused_by_its_memory_makes_no_home() {
    let (mut rig, homes) = orchestrator_rig();
    rig.unreadable_memory();
    let error = start_orchestrator(&mut rig, &homes, &owner())
        .map(|_| ())
        .unwrap_err();
    assert!(refusal(&error).contains("cannot be read"), "{error}");
    let left: Vec<PathBuf> = std::fs::read_dir(&homes)
        .map(|dir| dir.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

/// A rig whose world has the terminal holder in it, as a daemon or a CLI
/// with `run/holder.sock` has.
fn holder_rig(mut rig: Rig) -> (Rig, tiller_panes::FakePanes) {
    let panes = tiller_panes::FakePanes::new();
    rig.panes = Some(panes.clone());
    (rig, panes)
}

/// The herdr calls that would make a pane or type into one.
fn herdr_launches(rig: &Rig) -> Vec<String> {
    rig.herdr
        .calls()
        .into_iter()
        .filter(|c| {
            ["create_workspace", "send_text", "send_enter"]
                .iter()
                .any(|verb| c.starts_with(verb))
        })
        .collect()
}

#[test]
fn a_spawn_in_the_terminal_holder_records_its_pane_and_types_nothing() {
    let (mut rig, panes) = holder_rig(Rig::new());
    let before = rig.workspace_ids();
    let card = rig.card(CardKind::Ship, Some(Size::M));
    let spawned = rig.spawn(&card, RunRole::Builder);

    // The record keeps the holder's pane id, on the session and the run.
    assert!(
        tiller_core::ids::is_holder_pane(&spawned.pane_id),
        "{}",
        spawned.pane_id
    );
    assert_eq!(
        spawned.session.pane_id.as_deref(),
        Some(spawned.pane_id.as_str())
    );
    let run = rig.run(&spawned.run.id);
    assert_eq!(
        run.worktree_path.as_deref(),
        Some(spawned.worktree.as_str())
    );
    let session: tiller_core::domain::Session =
        get(rig.fleet.store.conn(), "session", &spawned.session.id).unwrap();
    assert_eq!(session.pane_id.as_deref(), Some(spawned.pane_id.as_str()));

    // No herdr workspace, no tokens and no typed launch line.
    assert!(spawned.workspace_id.is_empty());
    assert_eq!(rig.workspace_ids(), before);
    assert!(herdr_launches(&rig).is_empty(), "{:?}", rig.herdr.calls());

    // The holder runs the run's launch script, keyed by the session, in
    // the worktree, and names whose the pane is.
    let script = rig.scratch.join(&run.id).join("launch.sh");
    assert_eq!(
        panes.calls(),
        [format!(
            "spawn {} /bin/sh {}",
            spawned.session.id,
            script.display()
        )]
    );
    let listing = tiller_panes::Panes::list(&panes).unwrap();
    let pane = &listing.panes[0];
    assert_eq!(pane.pane.to_string(), spawned.pane_id);
    assert_eq!(pane.cwd, spawned.worktree);
    assert_eq!(pane.meta["session"], spawned.session.id);
    assert_eq!(pane.meta["run"], run.id);
    assert_eq!(pane.meta["role"], "builder");
}

#[test]
fn a_spawn_with_a_holder_that_does_not_answer_launches_nothing() {
    let (mut rig, panes) = holder_rig(Rig::new());
    panes.set_unreachable(true);
    let card = rig.card(CardKind::Ship, None);
    let ctx = rig.orchestrator();
    let error = rig
        .try_spawn(&ctx, Rig::request(&card, RunRole::Builder))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("the terminal holder is unreachable"),
        "{error}"
    );
    // The launch is undone: no live run, no herdr pane in its place.
    let live: Vec<Run> = list(
        rig.fleet.store.conn(),
        "card_id = ?1 AND ended_at IS NULL",
        &[&card.id],
    )
    .unwrap();
    assert!(live.is_empty(), "{live:?}");
    assert!(herdr_launches(&rig).is_empty(), "{:?}", rig.herdr.calls());
}

#[test]
fn cleanup_never_ends_a_run_whose_holder_pane_runs() {
    let (mut rig, panes) = holder_rig(Rig::new());
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let run = spawned.run.id.clone();
    rig.done(&run, RunOutcome::Abandoned).unwrap();

    let error = rig.cleanup(&run).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("still runs in the terminal holder"),
        "{error}"
    );

    // Once the pane's agent exits, cleanup goes ahead: the holder forgets
    // the pane, and herdr closes nothing.
    assert!(panes.exit(
        &tiller_panes::PaneId::from(spawned.pane_id.as_str()),
        Some(0),
        None
    ));
    let cleaned = rig.cleanup(&run).unwrap();
    assert_eq!(cleaned.closed_panes, std::slice::from_ref(&spawned.pane_id));
    assert!(
        panes
            .calls()
            .contains(&format!("forget {}", spawned.pane_id)),
        "{:?}",
        panes.calls()
    );
    assert!(tiller_panes::Panes::list(&panes).unwrap().panes.is_empty());
    assert!(
        !rig.herdr
            .calls()
            .iter()
            .any(|c| c.starts_with("close_pane")),
        "{:?}",
        rig.herdr.calls()
    );
    assert_eq!(cleaned.returned.as_deref(), Some(spawned.worktree.as_str()));
}

#[test]
fn an_orchestrator_starts_in_the_terminal_holder_with_no_workspace() {
    let (rig, homes) = orchestrator_rig();
    let (mut rig, panes) = holder_rig(rig);
    let before = rig.workspace_ids();
    let started = start_orchestrator(&mut rig, &homes, &owner()).unwrap();
    let value = &started.value;
    let pane = value.pane_id.clone().unwrap();
    assert!(tiller_core::ids::is_holder_pane(&pane), "{pane}");
    assert_eq!(value.session.pane_id.as_deref(), Some(pane.as_str()));
    assert!(value.workspace_id.is_none());
    assert_eq!(rig.workspace_ids(), before);
    assert!(herdr_launches(&rig).is_empty(), "{:?}", rig.herdr.calls());

    let script = rig.scratch.join(&value.session.id).join("launch.sh");
    assert_eq!(
        panes.calls(),
        [format!(
            "spawn {} /bin/sh {}",
            value.session.id,
            script.display()
        )]
    );
    let listing = tiller_panes::Panes::list(&panes).unwrap();
    assert_eq!(listing.panes[0].meta["role"], "orchestrator");
    assert_eq!(listing.panes[0].cwd, rig.repo.checkout.to_string_lossy());

    // Its agent exits with no session end: a second start returns the live
    // session, and warns that the holder no longer runs its pane.
    assert!(panes.exit(&tiller_panes::PaneId::from(pane.as_str()), Some(1), None));
    let again = start_orchestrator(&mut rig, &homes, &owner()).unwrap();
    assert_eq!(again.value.session.id, value.session.id);
    let warning = again.value.warning.unwrap_or_default();
    assert!(
        warning.contains("the terminal holder no longer runs pane"),
        "{warning}"
    );
}

#[test]
fn a_stop_of_a_holder_pane_reads_the_holder_and_never_takes_it_for_gone() {
    let (mut rig, panes) = holder_rig(Rig::new());
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    rig.done(&spawned.run.id, RunOutcome::Abandoned).unwrap();
    let waits = crate::control::Waits {
        interrupt: std::time::Duration::from_millis(50),
        exit: std::time::Duration::from_millis(50),
        bind: std::time::Duration::from_millis(50),
        poll: std::time::Duration::from_millis(10),
        pacing: tiller_herdr::submit::Pacing::INSTANT,
    };
    let pane = tiller_panes::PaneId::from(spawned.pane_id.as_str());

    // herdr has no such pane, which a stop would read as proof the agent
    // is gone: the stop asks the holder, which runs it at no composer, so
    // the stop refuses and the session stays live.
    let world = world!(rig);
    let error = crate::control::stop_agent(
        &mut rig.fleet.store,
        &owner(),
        &world,
        &waits,
        &spawned.session,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("did not come to an empty composer"),
        "{error}"
    );
    let session: tiller_core::domain::Session =
        get(rig.fleet.store.conn(), "session", &spawned.session.id).unwrap();
    assert!(session.ended_at.is_none());
    assert!(
        panes
            .calls()
            .iter()
            .any(|c| c.starts_with(&format!("screen {pane} ")))
    );

    // Once its agent exits, the stop ends the session and the holder
    // forgets the pane.
    assert!(panes.exit(&pane, Some(0), None));
    let world = world!(rig);
    crate::control::stop_agent(
        &mut rig.fleet.store,
        &owner(),
        &world,
        &waits,
        &spawned.session,
    )
    .unwrap();
    let session: tiller_core::domain::Session =
        get(rig.fleet.store.conn(), "session", &spawned.session.id).unwrap();
    assert!(session.ended_at.is_some());
    assert!(herdr_launches(&rig).is_empty(), "{:?}", rig.herdr.calls());
}

/// A launch script up to the value of its last flag, `--effort`: what a
/// later start of the same launch must repeat.
fn up_to_effort(script: &str) -> &str {
    let at = script.find(" '--effort' '").unwrap() + " '--effort' '".len();
    let end = at + script[at..].find('\'').unwrap() + 1;
    &script[..end]
}

/// The daemon's resume writes the script `tiller agent resume` writes:
/// every flag of the first launch, with `--resume <id>` (or nothing, for a
/// plain start) in place of the first prompt. A worker's comes from its run.
#[test]
fn a_worker_resume_script_repeats_its_first_launch() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let dir = rig.scratch.join(&spawned.run.id);
    let first = std::fs::read_to_string(dir.join("launch.sh")).unwrap();
    let world = world!(rig);
    let worktree = PathBuf::from(&spawned.worktree);
    let resumed = resume::write(
        rig.fleet.store.conn(),
        &world.resume_host(),
        &spawned.session,
        &worktree,
        launch::Start::Resume {
            claude_session_id: "claude-1",
        },
    )
    .unwrap();
    let text = std::fs::read_to_string(&resumed.script).unwrap();
    assert_eq!(
        resumed.script,
        dir.join(format!("resume-{}.sh", spawned.session.id))
    );
    assert_eq!(up_to_effort(&text), up_to_effort(&first), "{text}");
    assert!(
        text.trim_end().ends_with(" '--resume' 'claude-1'"),
        "{text}"
    );
    for flag in [
        "--append-system-prompt-file",
        "--settings",
        "--permission-mode",
        "--strict-mcp-config",
        "--mcp-config",
        "--model",
    ] {
        assert!(
            up_to_effort(&text).contains(&format!(" '{flag}'")),
            "{flag}: {text}"
        );
    }

    let plain = resume::write(
        rig.fleet.store.conn(),
        &world.resume_host(),
        &spawned.session,
        &worktree,
        launch::Start::Plain,
    )
    .unwrap();
    let text = std::fs::read_to_string(&plain.script).unwrap();
    assert_eq!(text.trim_end(), up_to_effort(&first), "{text}");
}

/// An orchestrator's resume takes its model and effort from the manifest
/// its start recorded, and its launch files from its own scratch.
#[test]
fn an_orchestrator_resume_script_repeats_its_first_launch() {
    let (mut rig, homes) = orchestrator_rig();
    let started = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let session: Session = get(rig.fleet.store.conn(), "session", &started.session.id).unwrap();
    let dir = rig.scratch.join(&session.id);
    let first = std::fs::read_to_string(dir.join("launch.sh")).unwrap();
    let world = world!(rig);
    let resumed = resume::write(
        rig.fleet.store.conn(),
        &world.resume_host(),
        &session,
        &rig.repo.checkout,
        launch::Start::Resume {
            claude_session_id: "claude-1",
        },
    )
    .unwrap();
    let text = std::fs::read_to_string(&resumed.script).unwrap();
    assert_eq!(up_to_effort(&text), up_to_effort(&first), "{text}");
    assert!(
        text.trim_end().ends_with(" '--resume' 'claude-1'"),
        "{text}"
    );

    // A session that recorded no manifest cannot say how it started.
    let unrecorded = Session {
        config_hash: None,
        ..session
    };
    let error = resume::write(
        rig.fleet.store.conn(),
        &world.resume_host(),
        &unrecorded,
        &rig.repo.checkout,
        launch::Start::Plain,
    )
    .unwrap_err();
    assert!(
        matches!(&error, SpawnError::Refused(why) if why.contains("recorded no launch manifest")),
        "{error}"
    );
}

/// A session whose launch files are gone is refused, and nothing is
/// rebuilt: no script is written.
#[test]
fn a_resume_of_a_session_whose_launch_files_are_gone_is_refused() {
    let mut rig = Rig::new();
    let card = rig.card(CardKind::Ship, None);
    let spawned = rig.spawn(&card, RunRole::Builder);
    let dir = rig.scratch.join(&spawned.run.id);
    std::fs::remove_file(dir.join("settings.json")).unwrap();
    let world = world!(rig);
    let error = resume::write(
        rig.fleet.store.conn(),
        &world.resume_host(),
        &spawned.session,
        Path::new(&spawned.worktree),
        launch::Start::Resume {
            claude_session_id: "claude-1",
        },
    )
    .unwrap_err();
    assert!(
        matches!(&error, SpawnError::Refused(why) if why.contains("settings.json is missing")),
        "{error}"
    );
    assert!(!dir.join("settings.json").exists());
    assert!(
        !dir.join(format!("resume-{}.sh", spawned.session.id))
            .exists(),
        "a refused resume wrote a script"
    );
}
