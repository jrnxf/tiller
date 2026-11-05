//! Spawn, `tiller done` and cleanup against real git repositories, a fake
//! treehouse pool, the fake herdr and a file-backed `state.db`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier, Mutex};

use tempfile::TempDir;
use tiller_core::domain::{CardKind, Pr, PrState, Size};
use tiller_core::state::caches::QuotaReading;
use tiller_core::state::cards::NewCard;
use tiller_core::{Actor, Ctx};
use tiller_herdr::fake::{FakeHerdr, Screen};
use tiller_herdr::testing::{Fleet, owner};
use tiller_reconcile::git::GitScanner;

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
    pub(crate) scanner: GitScanner,
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
            scanner: &$rig.scanner,
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
            scanner: GitScanner::new(),
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
        self.fleet
            .store
            .cache_quota(
                &Ctx::new(Actor::Daemon),
                vec![QuotaReading {
                    label: "claude-weekly".into(),
                    percent_remaining: Some(percent),
                    resets_at: None,
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

fn refusal(error: &SpawnError) -> String {
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
                        scanner: &scanner,
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
fn orchestrator_restart_is_the_owners() {
    let (mut rig, homes) = orchestrator_rig();
    let first = start_orchestrator(&mut rig, &homes, &owner())
        .unwrap()
        .value;
    let world = world!(rig);
    let error = orchestrator::start(
        &mut rig.fleet.store,
        &Ctx::new(Actor::Daemon),
        &world,
        &homes,
        "alpha",
        Some(&crate::control::Waits::default()),
    )
    .unwrap_err();
    assert!(error.to_string().contains("may not restart"), "{error}");
    let live = tiller_core::state::outbox::live_orchestrator(rig.fleet.store.conn(), "alpha")
        .unwrap()
        .unwrap();
    assert_eq!(live.id, first.session.id);
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
