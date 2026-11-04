//! Spawn, `tiller done` and cleanup against real git repositories, a fake
//! treehouse pool, the fake herdr and a file-backed `state.db`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier, Mutex};

use tempfile::TempDir;
use tiller_core::domain::{CardKind, Size};
use tiller_core::state::caches::QuotaReading;
use tiller_core::state::cards::NewCard;
use tiller_core::{Actor, Ctx};
use tiller_herdr::fake::{FakeHerdr, Screen};
use tiller_herdr::testing::{Fleet, owner};
use tiller_reconcile::git::GitScanner;

use super::*;

fn git(dir: &Path, args: &[&str]) -> String {
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
struct Repo {
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
struct FakePool {
    slots: PathBuf,
    origin: PathBuf,
    log: Arc<Mutex<PoolLog>>,
}

impl FakePool {
    fn taken(&self) -> Vec<(PathBuf, String, String, String)> {
        self.log.lock().unwrap().taken.clone()
    }

    fn returned(&self) -> Vec<(PathBuf, String)> {
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
struct Rig {
    fleet: Fleet,
    repo: Repo,
    pool: FakePool,
    herdr: FakeHerdr,
    scanner: GitScanner,
    config: SpawnConfig,
    tiller: PathBuf,
    scratch: PathBuf,
    disk_free_mb: Option<i64>,
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
            ram_free_mb: Some(64_000),
            disk_free_mb: $rig.disk_free_mb,
        }
    };
}

impl Rig {
    fn new() -> Self {
        Self::with_config("")
    }

    fn with_config(extra: &str) -> Self {
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
            fleet,
            repo,
            pool,
            herdr,
            scanner: GitScanner::new(),
            disk_free_mb: Some(64_000),
        }
    }

    fn orchestrator(&self) -> Ctx {
        Ctx::new(Actor::Session(self.fleet.orchestrator_session.clone()))
    }

    fn card(&mut self, kind: CardKind, size: Option<Size>) -> Card {
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

    fn request(card: &Card, role: RunRole) -> Request {
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

    fn spawn(&mut self, card: &Card, role: RunRole) -> Spawned {
        let ctx = self.orchestrator();
        match self.try_spawn(&ctx, Self::request(card, role)).unwrap() {
            Outcome::Spawned(spawned) => *spawned,
            other => panic!("expected a new spawn, got {other:?}"),
        }
    }

    fn run(&self, id: &str) -> Run {
        get(self.fleet.store.conn(), "run", id).unwrap()
    }

    fn done(&mut self, run: &str, outcome: RunOutcome) -> Result<Ended> {
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

    fn cleanup(&mut self, run: &str) -> Result<Cleaned> {
        let run = self.run(run);
        let world = world!(self);
        cleanup(&mut self.fleet.store, &owner(), &world, &run).map(|a| a.value)
    }

    fn workspace_ids(&self) -> Vec<String> {
        self.herdr
            .snapshot()
            .unwrap()
            .workspaces
            .into_iter()
            .map(|w| w.workspace_id)
            .collect()
    }

    fn events(&self, kind: &str, entity: &str) -> i64 {
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

    fn sql(&self, statement: &str) {
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
        (Some(Size::M), "opus", Effort::Medium),
        (Some(Size::L), "opus", Effort::High),
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
    for asked in [("haiku", Effort::High), ("opus", Effort::Medium)] {
        request.asked = Some((asked.0.into(), asked.1));
        let error = rig.try_spawn(&ctx, request.clone()).unwrap_err();
        assert!(matches!(error, SpawnError::Downgrade(_)), "{error}");
        assert!(refusal(&error).contains("needs the owner"), "{error}");
        assert!(refusal(&error).contains("opus:high"), "{error}");
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
