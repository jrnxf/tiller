//! The land sequence against a real `state.db` and an in-memory forge, and
//! `land_direct` against real git repositories (`authority.md` sections 6
//! and 12: G01, G02, G05, G10, G30).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use tiller_core::domain::{
    AnsweredVia, Approval, ApprovalAction, ApprovalKind, ApprovalState, Attention, Boundary, Card,
    CardKind, Checks, Coverage, Delivery, Effort, GrantKind, MergeGate, Mergeable, PipelineState,
    PrState, ReviewDecision, Reviewer, RunRole, Stage, Verdict,
};
use tiller_core::state::approvals::{Decision, NewApproval, NewGrant, NewPr, PrFetch};
use tiller_core::state::cards::NewCard;
use tiller_core::state::projects::{Init, NewProject};
use tiller_core::state::records::NewReview;
use tiller_core::state::runs::{NewOrchestrator, NewRun, SpawnChecks};
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Ctx, Store};

use super::forge::{
    AppToken, AppTokens, Forge, ForgeError, Identity, MergeOutcome, MergeRequest, NoApp, PullView,
};
use super::scrub::ScrubList;
use super::*;

const HEAD: &str = "1111111111111111111111111111111111111111";
const MOVED: &str = "2222222222222222222222222222222222222222";
const MERGED: &str = "3333333333333333333333333333333333333333";

/// The forge as a test sets it: one PR, its checks, and what a merge does.
struct FakeForge {
    view: RefCell<PullView>,
    outcome: RefCell<MergeOutcome>,
    merges: RefCell<Vec<(MergeRequest, Identity)>>,
    reads: RefCell<usize>,
    /// The head a push between the re-read and the merge leaves on the PR.
    push_before_merge: RefCell<Option<String>>,
}

impl FakeForge {
    fn new() -> Self {
        Self {
            view: RefCell::new(PullView {
                state: PrState::Open,
                head_sha: HEAD.into(),
                head_ref: "fm/alpha-1".into(),
                base_ref: "main".into(),
                checks: Checks::Success,
                mergeable: Mergeable::Clean,
                body: "Adds the login page.".into(),
                files: vec!["src/login.rs".into()],
            }),
            outcome: RefCell::new(MergeOutcome::Merged { sha: MERGED.into() }),
            merges: RefCell::new(Vec::new()),
            reads: RefCell::new(0),
            push_before_merge: RefCell::new(None),
        }
    }

    fn set(&self, edit: impl FnOnce(&mut PullView)) {
        edit(&mut self.view.borrow_mut());
    }

    fn merges(&self) -> Vec<(MergeRequest, Identity)> {
        self.merges.borrow().clone()
    }
}

impl Forge for FakeForge {
    fn pull(&self, _repo: &str, _number: i64) -> std::result::Result<PullView, ForgeError> {
        *self.reads.borrow_mut() += 1;
        Ok(self.view.borrow().clone())
    }

    fn checks(&self, _repo: &str, _sha: &str) -> std::result::Result<Checks, ForgeError> {
        Ok(self.view.borrow().checks)
    }

    /// GitHub's rule: the merge carries `sha`, and a PR head that differs
    /// is refused with a conflict.
    fn merge(&self, request: &MergeRequest, identity: &Identity) -> MergeOutcome {
        if let Some(head) = self.push_before_merge.borrow_mut().take() {
            self.view.borrow_mut().head_sha = head;
        }
        self.merges
            .borrow_mut()
            .push((request.clone(), identity.clone()));
        if self.view.borrow().head_sha != request.sha {
            return MergeOutcome::Refused(
                "Head branch was modified. Review and try the merge again. (HTTP 409)".into(),
            );
        }
        self.outcome.borrow().clone()
    }
}

struct FakeApp;

impl AppTokens for FakeApp {
    fn mint(&self, _repo: &str) -> std::result::Result<AppToken, ForgeError> {
        Ok(AppToken::new("ghs_test"))
    }
}

fn owner() -> Ctx {
    Ctx::new(Actor::Owner)
}

struct World {
    home: tempfile::TempDir,
    store: Store,
    orchestrator: String,
}

impl World {
    fn new(delivery: Delivery) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(home.path().join("state.db")).unwrap();
        store
            .init(
                &owner(),
                Init {
                    host_label: "test-host".into(),
                    lane_capacity: 4,
                    ram_floor_mb: None,
                    disk_floor_mb: None,
                    boundary: Boundary::Personal,
                },
            )
            .unwrap();
        store
            .project_add(
                &owner(),
                NewProject {
                    id: "alpha".into(),
                    name: "Alpha".into(),
                    repo: Some("example/alpha".into()),
                    remote: None,
                    default_branch: Some("main".into()),
                    delivery,
                    checks: None,
                    capacity: Some(4),
                    deploy_workflow: None,
                    deploy_environment: None,
                    deploy_timeout_minutes: None,
                },
            )
            .unwrap();
        let orchestrator = store
            .session_start(
                &owner(),
                NewOrchestrator {
                    project_id: "alpha".into(),
                    claude_session_id: Some("claude-orch".into()),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap()
            .value
            .session
            .id;
        Self {
            home,
            store,
            orchestrator,
        }
    }

    fn lock(&self) -> DaemonLock {
        DaemonLock::acquire(self.home.path()).unwrap()
    }

    fn card(&mut self, title: &str) -> Card {
        self.store
            .card_create(
                &owner(),
                None,
                NewCard {
                    project_id: "alpha".into(),
                    kind: CardKind::Ship,
                    title: title.into(),
                    ..NewCard::default()
                },
                None,
            )
            .unwrap()
            .value
    }

    /// A builder run on `card`, working in `worktree` on `branch`.
    fn builder(&mut self, card: &Card, worktree: Option<&Path>, branch: &str) -> String {
        self.store
            .run_start(
                &Ctx::new(Actor::Session(self.orchestrator.clone())),
                None,
                NewRun {
                    card: card.id.clone(),
                    role: RunRole::Builder,
                    model: "test-model".into(),
                    effort: Effort::Medium,
                    host_id: None,
                    worktree_path: worktree.map(|path| path.display().to_string()),
                    branch: Some(branch.into()),
                    base_branch: Some("main".into()),
                    base_sha: None,
                    config_hash: None,
                    checks: SpawnChecks::default(),
                },
            )
            .unwrap()
            .value
            .run
            .id
    }

    /// A ship card with a builder run and its open PR #7 at [`HEAD`].
    fn pr(&mut self) -> (Card, String) {
        self.pr_numbered(7)
    }

    fn pr_numbered(&mut self, number: i64) -> (Card, String) {
        let card = self.card("the login page");
        let run = self.builder(&card, None, &format!("fm/alpha-{number}"));
        let pr = self
            .store
            .pr_link(
                &owner(),
                NewPr {
                    card: card.id.clone(),
                    run_id: Some(run),
                    number,
                    url: format!("https://example.com/example/alpha/pull/{number}"),
                    head_sha: Some(HEAD.into()),
                    base_ref: Some("main".into()),
                },
            )
            .unwrap()
            .value;
        (card, pr.id)
    }

    fn request(&mut self, card: &Card, pr: &str) -> Approval {
        self.store
            .approval_request(
                &owner(),
                NewApproval {
                    kind: ApprovalKind::Merge,
                    card: card.id.clone(),
                    run_id: None,
                    pr_id: Some(pr.into()),
                    branch: None,
                    head_sha: HEAD.into(),
                    action: ApprovalAction::Squash,
                    memory_paths: None,
                    grant_id: None,
                },
            )
            .unwrap()
            .value
    }

    fn accept(&mut self, approval: &Approval) {
        self.store
            .approval_decide(
                &owner(),
                None,
                &approval.id,
                Decision {
                    accept: true,
                    via: AnsweredVia::Board,
                    memory: Vec::new(),
                    ask_id: None,
                },
            )
            .unwrap();
    }

    /// PR #7 with an accepted squash approval at [`HEAD`].
    fn accepted(&mut self) -> (Card, String, Approval) {
        let (card, pr) = self.pr();
        let approval = self.request(&card, &pr);
        self.accept(&approval);
        (card, pr, approval)
    }

    fn approval(&self, id: &str) -> Approval {
        get(self.store.conn(), "approval", id).unwrap()
    }

    fn attention(&self, card: &Card) -> Vec<Attention> {
        list(
            self.store.conn(),
            "card_id = ?1 AND answered_at IS NULL AND withdrawn_at IS NULL ORDER BY id",
            &[&card.id],
        )
        .unwrap()
    }

    fn set_gate(&mut self, gate: MergeGate) {
        self.store
            .project_set_merge_gate(&Ctx::new(Actor::Daemon), "alpha", gate, "test")
            .unwrap();
    }
}

fn lander(forge: &FakeForge) -> Lander<'_> {
    Lander {
        forge,
        app: &NoApp,
        scrub: ScrubList::default(),
    }
}

fn fetch(head: &str) -> PrFetch {
    PrFetch {
        state: PrState::Open,
        head_sha: Some(head.into()),
        base_ref: Some("main".into()),
        checks: Checks::Success,
        review_decision: ReviewDecision::None,
        mergeable: Mergeable::Clean,
        merged_at: None,
        merge_sha: None,
        pipeline_state: PipelineState::None,
        fetch_error: None,
    }
}

// -- G01 ----------------------------------------------------------------------

#[test]
fn execute_requires_daemon_lock() {
    let world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    assert!(lock.path().ends_with(LOCK_FILE));
    // A second daemon, or any other process, cannot take it while it is held.
    assert!(matches!(
        DaemonLock::acquire(world.home.path()),
        Err(LandError::Held(_))
    ));
    assert!(DaemonLock::held(world.home.path()));
    drop(lock);
    assert!(!DaemonLock::held(world.home.path()));
    // The lock is taken again once the daemon is gone.
    let again = world.lock();
    // The operations the land functions use are daemon only in the state
    // core too: the owner, a session or a hook cannot execute or finish.
    let mut world = world;
    let (_, _, approval) = world.accepted();
    for actor in [
        Actor::Owner,
        Actor::Hook,
        Actor::Session(world.orchestrator.clone()),
    ] {
        let error = world
            .store
            .approval_execute(&Ctx::new(actor), &approval.id, HEAD)
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
    }
    assert_eq!(world.approval(&approval.id).state, ApprovalState::Accepted);
    drop(again);
}

#[test]
fn land_refuses_without_accepted_approval() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (card, pr) = world.pr();
    let requested = world.request(&card, &pr);
    let error = lander(&forge)
        .land(&lock, &mut world.store, &requested.id)
        .unwrap_err();
    assert!(matches!(error, LandError::Refused { .. }), "{error}");
    world
        .store
        .approval_decide(
            &owner(),
            None,
            &requested.id,
            Decision {
                accept: false,
                via: AnsweredVia::Board,
                memory: Vec::new(),
                ask_id: None,
            },
        )
        .unwrap();
    assert!(
        lander(&forge)
            .land(&lock, &mut world.store, &requested.id)
            .is_err()
    );
    assert!(
        lander(&forge)
            .land_ready(&lock, &mut world.store)
            .unwrap()
            .is_empty()
    );
    assert!(forge.merges().is_empty(), "nothing reached the forge");
}

// -- G02 ----------------------------------------------------------------------

#[test]
fn land_rechecks_head_sha_before_merge() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (card, _, approval) = world.accepted();
    // The poller has not seen the push yet; the land's own re-read does.
    forge.set(|view| view.head_sha = MOVED.into());
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Stale { .. }), "{landed:?}");
    assert_eq!(world.approval(&approval.id).state, ApprovalState::Stale);
    assert!(forge.merges().is_empty(), "no merge call at a moved head");
    // A new approval asks again at the new head.
    let again: Approval = find(
        world.store.conn(),
        "state = 'requested' AND head_sha = ?1",
        &[&MOVED],
    )
    .unwrap()
    .unwrap();
    assert_eq!(again.card_id.as_deref(), Some(card.id.as_str()));
}

#[test]
fn merge_passes_bound_sha_to_forge() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (card, _, approval) = world.accepted();
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    let Landed::Succeeded { sha, .. } = landed else {
        panic!("{landed:?}");
    };
    assert_eq!(sha, MERGED);
    let merges = forge.merges();
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0].0.sha, HEAD);
    assert_eq!(merges[0].0.number, 7);
    assert_eq!(merges[0].0.repo, "example/alpha");
    assert_eq!(merges[0].0.method, ApprovalAction::Squash);
    let done = world.approval(&approval.id);
    assert_eq!(done.state, ApprovalState::Succeeded);
    assert_eq!(done.result.as_ref().unwrap()["sha"], MERGED);
    assert_eq!(world.store.card(&card.id).unwrap().state.as_str(), "closed");

    // A push between the re-read and the merge call: the forge refuses the
    // bound SHA, and the approval fails instead of landing other code.
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (_, _, approval) = world.accepted();
    *forge.push_before_merge.borrow_mut() = Some(MOVED.into());
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Failed { .. }), "{landed:?}");
    assert_eq!(world.approval(&approval.id).state, ApprovalState::Failed);

    // An unreadable reply is `uncertain`, never retried blind.
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (_, _, approval) = world.accepted();
    *forge.outcome.borrow_mut() = MergeOutcome::Unknown("timed out".into());
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Uncertain { .. }), "{landed:?}");
    assert_eq!(world.approval(&approval.id).state, ApprovalState::Uncertain);
    assert!(
        lander(&forge)
            .land_ready(&lock, &mut world.store)
            .unwrap()
            .is_empty()
    );
    assert_eq!(forge.merges().len(), 1);
}

/// One approval that errors does not stop the pass: the next one lands.
#[test]
fn an_error_on_one_approval_does_not_stop_the_pass() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (_, _, broken) = world.accepted();
    let (card, pr) = world.pr_numbered(8);
    let fine = world.request(&card, &pr);
    world.accept(&fine);
    // The store refuses every write to the first approval.
    world
        .store
        .conn()
        .execute_batch(&format!(
            "CREATE TRIGGER refuse BEFORE UPDATE ON approvals WHEN OLD.id = '{}'
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            broken.id
        ))
        .unwrap();
    let landed = lander(&forge).land_ready(&lock, &mut world.store).unwrap();
    assert!(
        matches!(&landed[..], [Landed::Errored { approval, error }, Landed::Succeeded { .. }]
            if approval.id == broken.id && error.contains("disk full")),
        "{landed:?}"
    );
    assert_eq!(world.approval(&broken.id).state, ApprovalState::Accepted);
    assert_eq!(world.approval(&fine.id).state, ApprovalState::Succeeded);
}

/// The T4.3 acceptance check: a push after approval invalidates the
/// approval, by the poller's refresh and by the land's own re-read.
#[test]
fn push_after_approval_marks_it_stale() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (_, pr, approval) = world.accepted();
    world
        .store
        .pr_refresh(&Ctx::new(Actor::Daemon), &pr, fetch(MOVED))
        .unwrap();
    assert_eq!(world.approval(&approval.id).state, ApprovalState::Stale);
    // The daemon has nothing to land, and the stale approval cannot execute.
    forge.set(|view| view.head_sha = MOVED.into());
    assert!(
        lander(&forge)
            .land_ready(&lock, &mut world.store)
            .unwrap()
            .is_empty()
    );
    assert!(
        lander(&forge)
            .land(&lock, &mut world.store, &approval.id)
            .is_err()
    );
    let error = world
        .store
        .approval_execute(&lock.ctx(), &approval.id, MOVED)
        .unwrap_err();
    assert_eq!(error.code(), "refused");
    assert!(forge.merges().is_empty());
}

#[test]
fn pending_checks_wait_and_failed_checks_stale() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (card, _, approval) = world.accepted();
    forge.set(|view| view.checks = Checks::Pending);
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Waiting { .. }), "{landed:?}");
    assert_eq!(world.approval(&approval.id).state, ApprovalState::Accepted);
    forge.set(|view| view.checks = Checks::Failure);
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Stale { .. }), "{landed:?}");
    let items = world.attention(&card);
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(
        items[0].question.contains("checks failed"),
        "{}",
        items[0].question
    );
    assert!(forge.merges().is_empty());
}

#[test]
fn closed_pr_stales_the_approval() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (_, _, approval) = world.accepted();
    forge.set(|view| view.state = PrState::Closed);
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Stale { .. }), "{landed:?}");
    assert!(forge.merges().is_empty());
}

// -- G05 ----------------------------------------------------------------------

fn grant(world: &mut World, conditions: Value) -> String {
    world
        .store
        .grant_create(
            &owner(),
            NewGrant {
                project_id: "alpha".into(),
                kind: GrantKind::Merge,
                conditions,
            },
        )
        .unwrap()
        .value
        .id
}

fn standing() -> Value {
    json!({
        "v": 1,
        "actions": ["squash"],
        "base_ref": "main",
        "head_ref_pattern": "fm/*",
        "opened_by_fleet_run": true,
        "card_kinds": ["ship"],
        "checks": "success",
        "pipeline": null,
        "reviews": {"require": ["code-review"], "max_high": 0},
        "exclude_paths": [".github/workflows/**"],
        "expires_at": null,
        "away_approval_id": null
    })
}

fn review(world: &mut World, card: &Card, verdict: Verdict, high: i64) {
    let pr: tiller_core::domain::Pr = find(world.store.conn(), "card_id = ?1", &[&card.id])
        .unwrap()
        .unwrap();
    world
        .store
        .review_record(
            &Ctx::new(Actor::Session(world.orchestrator.clone())),
            NewReview {
                run_id: pr.run_id.unwrap(),
                reviewer: Reviewer::CodeReview,
                coverage: Coverage::Diff,
                findings_high: high,
                findings_medium: 0,
                findings_low: 0,
                verdict,
                source_url: None,
            },
        )
        .unwrap();
}

#[test]
fn standing_grant_merges_only_green_in_scope_prs() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let grant_id = grant(&mut world, standing());
    let (card, _) = world.pr();
    let lander = lander(&forge);

    // Not green yet: pending checks, then no review.
    forge.set(|view| view.checks = Checks::Pending);
    assert!(
        lander
            .match_grants(&lock, &mut world.store)
            .unwrap()
            .approvals
            .is_empty()
    );
    forge.set(|view| view.checks = Checks::Success);
    assert!(
        lander
            .match_grants(&lock, &mut world.store)
            .unwrap()
            .approvals
            .is_empty(),
        "no review yet"
    );
    review(&mut world, &card, Verdict::Pass, 1);
    assert!(
        lander
            .match_grants(&lock, &mut world.store)
            .unwrap()
            .approvals
            .is_empty(),
        "a high finding"
    );
    review(&mut world, &card, Verdict::Pass, 0);
    // Out of scope: another base, another head pattern.
    forge.set(|view| view.base_ref = "release".into());
    assert!(
        lander
            .match_grants(&lock, &mut world.store)
            .unwrap()
            .approvals
            .is_empty()
    );
    forge.set(|view| {
        view.base_ref = "main".into();
        view.head_ref = "feature/x".into();
    });
    assert!(
        lander
            .match_grants(&lock, &mut world.store)
            .unwrap()
            .approvals
            .is_empty()
    );
    forge.set(|view| view.head_ref = "fm/alpha-1".into());

    let matched = lander
        .match_grants(&lock, &mut world.store)
        .unwrap()
        .approvals;
    assert_eq!(matched.len(), 1);
    let approval = &matched[0];
    assert_eq!(approval.state, ApprovalState::Accepted);
    assert_eq!(
        approval.decided_by.as_deref(),
        Some(format!("grant:{grant_id}").as_str())
    );
    assert_eq!(approval.head_sha.as_deref(), Some(HEAD));
    // Matched once; the open approval stops a second match.
    assert!(
        lander
            .match_grants(&lock, &mut world.store)
            .unwrap()
            .approvals
            .is_empty()
    );
    let landed = lander.land_ready(&lock, &mut world.store).unwrap();
    assert!(
        matches!(landed[..], [Landed::Succeeded { .. }]),
        "{landed:?}"
    );
    assert_eq!(forge.merges()[0].0.sha, HEAD);

    // One PR that errors does not stop the grant pass for the others.
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    grant(&mut world, standing());
    let (first, broken) = world.pr();
    let (second, fine) = world.pr_numbered(8);
    review(&mut world, &first, Verdict::Pass, 0);
    review(&mut world, &second, Verdict::Pass, 0);
    world
        .store
        .conn()
        .execute_batch(&format!(
            "CREATE TRIGGER refuse BEFORE INSERT ON approvals WHEN NEW.pr_id = '{broken}'
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;"
        ))
        .unwrap();
    let matched = lander.match_grants(&lock, &mut world.store).unwrap();
    assert_eq!(matched.errors.len(), 1, "{:?}", matched.errors);
    assert_eq!(matched.errors[0].0, broken);
    assert_eq!(matched.approvals.len(), 1);
    assert_eq!(matched.approvals[0].pr_id.as_deref(), Some(fine.as_str()));

    // A PR no fleet run opened is out of scope.
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let mut conditions = standing();
    conditions["reviews"] = Value::Null;
    grant(&mut world, conditions);
    let card = world.card("by hand");
    world
        .store
        .pr_link(
            &owner(),
            NewPr {
                card: card.id.clone(),
                run_id: None,
                number: 9,
                url: "https://example.com/example/alpha/pull/9".into(),
                head_sha: Some(HEAD.into()),
                base_ref: Some("main".into()),
            },
        )
        .unwrap();
    assert!(
        super::Lander {
            forge: &forge,
            app: &NoApp,
            scrub: ScrubList::default()
        }
        .match_grants(&lock, &mut world.store)
        .unwrap()
        .approvals
        .is_empty()
    );
}

/// D4 of `docs/design/pre-launch-validation.md`: a `pipeline: passed` grant
/// matches a pre-launch PR that had no pipeline run, and refuses the same PR
/// once the project is launched.
#[test]
fn pipeline_condition_skipped_only_while_prelaunch() {
    let mut world = World::new(Delivery::NoMistakes);
    let lock = world.lock();
    let forge = FakeForge::new();
    let mut conditions = standing();
    conditions["pipeline"] = json!("passed");
    conditions["reviews"] = Value::Null;
    let grant_id = grant(&mut world, conditions);
    let grant_row: tiller_core::domain::Grant =
        get(world.store.conn(), "grant", &grant_id).unwrap();
    let (card, pr_id) = world.pr();
    let approval = world.request(&card, &pr_id);
    let pr: tiller_core::domain::Pr = get(world.store.conn(), "pr", &pr_id).unwrap();
    assert_eq!(pr.pipeline_state, PipelineState::None);
    let view = forge.view.borrow().clone();
    let evaluate = |store: &Store| {
        grants::evaluate(
            store.conn(),
            &grant_row,
            &approval,
            &pr,
            &view,
            tiller_core::Timestamp::now(),
        )
        .unwrap()
    };
    let stage = |store: &mut Store, stage: Stage| {
        let rev = get::<tiller_core::domain::Project>(store.conn(), "project", "alpha")
            .unwrap()
            .rev;
        store
            .project_stage(&owner(), "alpha", rev, stage, Some(HEAD))
            .unwrap();
    };

    let miss = evaluate(&world.store).unwrap_err();
    assert!(miss.contains("pipeline is none, not passed"), "{miss}");

    stage(&mut world.store, Stage::PreLaunch);
    assert_eq!(evaluate(&world.store), Ok(()));
    // A pipeline that ran and failed still blocks while pre-launch.
    let failed = tiller_core::domain::Pr {
        pipeline_state: PipelineState::Failed,
        ..pr.clone()
    };
    let miss = grants::evaluate(
        world.store.conn(),
        &grant_row,
        &approval,
        &failed,
        &view,
        tiller_core::Timestamp::now(),
    )
    .unwrap()
    .unwrap_err();
    assert!(miss.contains("pipeline is failed, not passed"), "{miss}");
    // Checks and paths still apply while pre-launch.
    let mut red = view.clone();
    red.checks = Checks::Failure;
    assert!(
        grants::evaluate(
            world.store.conn(),
            &grant_row,
            &approval,
            &pr,
            &red,
            tiller_core::Timestamp::now()
        )
        .unwrap()
        .is_err()
    );

    // The daemon's grant pass agrees: a PR with no approval yet matches.
    let (_, other) = world.pr_numbered(8);
    let matched = lander(&forge)
        .match_grants(&lock, &mut world.store)
        .unwrap()
        .approvals;
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].pr_id.as_deref(), Some(other.as_str()));

    stage(&mut world.store, Stage::Launched);
    let miss = evaluate(&world.store).unwrap_err();
    assert!(miss.contains("pipeline is none, not passed"), "{miss}");
    let (_, third) = world.pr_numbered(9);
    let matched = lander(&forge)
        .match_grants(&lock, &mut world.store)
        .unwrap()
        .approvals;
    assert!(
        matched
            .iter()
            .all(|a| a.pr_id.as_deref() != Some(third.as_str())),
        "{matched:?}"
    );
}

/// A PR the poller last saw with pending checks costs no forge read.
#[test]
fn grant_pass_skips_the_forge_while_the_cache_says_pending() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    grant(&mut world, standing());
    let (_, pr) = world.pr();
    let daemon = Ctx::new(Actor::Daemon);
    world
        .store
        .pr_refresh(
            &daemon,
            &pr,
            PrFetch {
                checks: Checks::Pending,
                ..fetch(HEAD)
            },
        )
        .unwrap();
    lander(&forge)
        .match_grants(&lock, &mut world.store)
        .unwrap();
    assert_eq!(*forge.reads.borrow(), 0);
    world.store.pr_refresh(&daemon, &pr, fetch(HEAD)).unwrap();
    lander(&forge)
        .match_grants(&lock, &mut world.store)
        .unwrap();
    assert_eq!(*forge.reads.borrow(), 1);
}

#[test]
fn revoked_or_expired_grant_matches_nothing() {
    for case in ["revoked", "expired"] {
        let mut world = World::new(Delivery::DirectPr);
        let lock = world.lock();
        let forge = FakeForge::new();
        let mut conditions = standing();
        conditions["reviews"] = Value::Null;
        if case == "expired" {
            conditions["expires_at"] = json!("2020-01-01T00:00:00.000Z");
        }
        let id = grant(&mut world, conditions);
        if case == "revoked" {
            world
                .store
                .grant_revoke(&owner(), &id, "changed my mind")
                .unwrap();
        }
        world.pr();
        assert!(
            lander(&forge)
                .match_grants(&lock, &mut world.store)
                .unwrap()
                .approvals
                .is_empty(),
            "{case}"
        );
    }

    // Accepted by a grant, then revoked before execution: `stale`, no merge.
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let mut conditions = standing();
    conditions["reviews"] = Value::Null;
    let id = grant(&mut world, conditions.clone());
    world.pr();
    let matched = lander(&forge)
        .match_grants(&lock, &mut world.store)
        .unwrap()
        .approvals;
    assert_eq!(matched.len(), 1);
    world
        .store
        .grant_revoke(&owner(), &id, "changed my mind")
        .unwrap();
    assert_eq!(world.approval(&matched[0].id).state, ApprovalState::Stale);
    assert!(
        lander(&forge)
            .land_ready(&lock, &mut world.store)
            .unwrap()
            .is_empty()
    );
    assert!(forge.merges().is_empty());

    // A condition that stops holding between the match and the land stales it
    // in the sequence's own re-evaluation.
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    grant(&mut world, conditions);
    world.pr();
    let matched = lander(&forge)
        .match_grants(&lock, &mut world.store)
        .unwrap()
        .approvals;
    forge.set(|view| view.files.push(".github/workflows/ci.yml".into()));
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &matched[0].id)
        .unwrap();
    assert!(matches!(landed, Landed::Stale { .. }), "{landed:?}");
    assert!(forge.merges().is_empty());
}

#[test]
fn excluded_path_never_matches_grant() {
    for file in [
        ".github/workflows/ci.yml",
        "AGENTS.md",
        "crates/core/CLAUDE.md",
        ".claude/rules/style.md",
    ] {
        let mut world = World::new(Delivery::DirectPr);
        let lock = world.lock();
        let forge = FakeForge::new();
        let mut conditions = standing();
        conditions["reviews"] = Value::Null;
        // A grant that lists no exclusions still never covers a loaded file.
        if file != ".github/workflows/ci.yml" {
            conditions["exclude_paths"] = json!([]);
        }
        grant(&mut world, conditions);
        world.pr();
        forge.set(|view| view.files = vec!["src/login.rs".into(), file.into()]);
        assert!(
            lander(&forge)
                .match_grants(&lock, &mut world.store)
                .unwrap()
                .approvals
                .is_empty(),
            "{file}"
        );
    }
}

// -- G10 ----------------------------------------------------------------------

#[test]
fn merge_uses_daemon_app_identity() {
    // `detect`: the labelled fallback lands as the owner's login.
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (_, _, approval) = world.accepted();
    lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert_eq!(forge.merges()[0].1, Identity::Owner);

    // `enforced`: only the App's installation token merges.
    let mut world = World::new(Delivery::NoMistakes);
    world.set_gate(MergeGate::Enforced);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (_, _, approval) = world.accepted();
    let app = Lander {
        forge: &forge,
        app: &FakeApp,
        scrub: ScrubList::default(),
    };
    let landed = app.land(&lock, &mut world.store, &approval.id).unwrap();
    assert!(matches!(landed, Landed::Succeeded { .. }), "{landed:?}");
    assert_eq!(
        forge.merges()[0].1,
        Identity::App(AppToken::new("ghs_test"))
    );

    // `enforced` with no App on the host: it waits, never falls back to the
    // owner's login.
    let mut world = World::new(Delivery::NoMistakes);
    world.set_gate(MergeGate::Enforced);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (card, _, approval) = world.accepted();
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Waiting { .. }), "{landed:?}");
    assert_eq!(world.approval(&approval.id).state, ApprovalState::Accepted);
    assert!(forge.merges().is_empty());
    // The owner hears about it once, however often the daemon retries.
    lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    let notices = world.attention(&card);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].question.contains("merge App"), "{notices:?}");
    assert_eq!(format!("{:?}", AppToken::new("ghs_secret")), "AppToken(..)");
}

// -- G30 ----------------------------------------------------------------------

#[test]
fn pr_body_with_quoted_ask_is_refused() {
    let mut world = World::new(Delivery::DirectPr);
    let lock = world.lock();
    let forge = FakeForge::new();
    let (card, _, approval) = world.accepted();
    let ask = "please make the login page remember my email for next time I visit";
    world
        .store
        .ask_record(
            &Ctx::new(Actor::Hook),
            None,
            tiller_core::state::asks::NewAsk {
                project_id: "alpha".into(),
                session_id: Some(world.orchestrator.clone()),
                source: tiller_core::domain::AskSource::Prompt,
                text: ask.into(),
            },
        )
        .unwrap();
    forge.set(|view| {
        view.body = "## What changed\n\nThe owner said: \"make the login page remember my\n  email for next time I visit\".".into();
    });
    for _ in 0..2 {
        let landed = lander(&forge)
            .land(&lock, &mut world.store, &approval.id)
            .unwrap();
        let Landed::Waiting { why, .. } = &landed else {
            panic!("{landed:?}");
        };
        assert!(why.contains("quotes an ask"), "{why}");
        // The refusal never repeats the leaked words.
        assert!(!why.contains("remember"), "{why}");
    }
    assert_eq!(
        world.attention(&card).len(),
        1,
        "one item, however many passes"
    );
    assert!(forge.merges().is_empty());

    // A scrub-list term and a stale pipeline claim refuse it too.
    let list = ScrubList::parse(&format!("internal-box\n{}\n", sha("secretproj")));
    for body in [
        "Deployed to internal-box first.",
        "Matches the secretproj-v2 layout.",
        "no-mistakes passed on this head.",
    ] {
        forge.set(|view| view.body = body.into());
        let lander = Lander {
            forge: &forge,
            app: &NoApp,
            scrub: list.clone(),
        };
        let landed = lander.land(&lock, &mut world.store, &approval.id).unwrap();
        assert!(
            matches!(landed, Landed::Waiting { .. }),
            "{body}: {landed:?}"
        );
    }
    // A clean body lands.
    forge.set(|view| view.body = "Adds the login page. The pipeline is pending.".into());
    let lander = Lander {
        forge: &forge,
        app: &NoApp,
        scrub: list,
    };
    let landed = lander.land(&lock, &mut world.store, &approval.id).unwrap();
    assert!(matches!(landed, Landed::Succeeded { .. }), "{landed:?}");
}

fn sha(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// -- land_direct --------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A bare remote with `main`, and a worktree clone on `fm/alpha-1` one
/// commit ahead. Returns the dirs and the branch head.
fn repos() -> (tempfile::TempDir, tempfile::TempDir, String) {
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    let work = tempfile::tempdir().unwrap();
    git(work.path(), &["init", "-q", "-b", "main"]);
    git(
        work.path(),
        &[
            "remote",
            "add",
            "origin",
            &remote.path().display().to_string(),
        ],
    );
    commit(work.path(), "base");
    git(work.path(), &["push", "-q", "origin", "main"]);
    git(work.path(), &["switch", "-q", "-c", "fm/alpha-1"]);
    let head = commit(work.path(), "feature");
    (remote, work, head)
}

fn direct_approval(world: &mut World, work: &Path, head: &str) -> Approval {
    let card = world.card("push it");
    let run = world.builder(&card, Some(work), "fm/alpha-1");
    let approval = world
        .store
        .approval_request(
            &owner(),
            NewApproval {
                kind: ApprovalKind::LandDirect,
                card: card.id.clone(),
                run_id: Some(run),
                pr_id: None,
                branch: Some("fm/alpha-1".into()),
                head_sha: head.into(),
                action: ApprovalAction::Push,
                memory_paths: None,
                grant_id: None,
            },
        )
        .unwrap()
        .value;
    world.accept(&approval);
    approval
}

#[test]
fn direct_push_is_fast_forward_of_bound_sha() {
    let (remote, work, head) = repos();
    let mut world = World::new(Delivery::DirectPush);
    let lock = world.lock();
    let forge = FakeForge::new();
    let approval = direct_approval(&mut world, work.path(), &head);
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Succeeded { .. }), "{landed:?}");
    assert_eq!(git(remote.path(), &["rev-parse", "refs/heads/main"]), head);

    // The branch moved after approval: stale, nothing pushed.
    let (remote, work, head) = repos();
    let mut world = World::new(Delivery::DirectPush);
    let lock = world.lock();
    let approval = direct_approval(&mut world, work.path(), &head);
    let base = git(remote.path(), &["rev-parse", "refs/heads/main"]);
    commit(work.path(), "later");
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Stale { .. }), "{landed:?}");
    assert_eq!(git(remote.path(), &["rev-parse", "refs/heads/main"]), base);

    // The default branch moved past the bound head: not a descendant, so
    // the land stops before pushing and the remote keeps its commit.
    let (remote, work, head) = repos();
    let mut world = World::new(Delivery::DirectPush);
    let lock = world.lock();
    let approval = direct_approval(&mut world, work.path(), &head);
    let other = tempfile::tempdir().unwrap();
    git(
        other.path(),
        &["clone", "-q", &remote.path().display().to_string(), "."],
    );
    let moved = commit(other.path(), "elsewhere");
    git(other.path(), &["push", "-q", "origin", "main"]);
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Stale { .. }), "{landed:?}");
    assert_eq!(git(remote.path(), &["rev-parse", "refs/heads/main"]), moved);

    // And the remote's own fast-forward rule: a push of a non-descendant
    // that races past the check is refused, never forced.
    let (remote, work, head) = repos();
    let pushed = direct::push(
        work.path(),
        &remote.path().display().to_string(),
        &head,
        "main",
        &Identity::Owner,
    );
    assert_eq!(pushed, direct::Pushed::Done);
    let base = git(work.path(), &["rev-parse", "main"]);
    let pushed = direct::push(
        work.path(),
        &remote.path().display().to_string(),
        &base,
        "main",
        &Identity::Owner,
    );
    assert!(matches!(pushed, direct::Pushed::Refused(_)), "{pushed:?}");
    assert_eq!(git(remote.path(), &["rev-parse", "refs/heads/main"]), head);
}

/// A `land_direct` grant that expires after the approval it accepted: the
/// land stops before anything moves, on a remote and in a local checkout.
#[test]
fn expired_direct_grant_lands_nothing() {
    let forge = FakeForge::new();
    for delivery in [Delivery::DirectPush, Delivery::LocalOnly] {
        let (target, work, head) = if delivery == Delivery::LocalOnly {
            local_repos()
        } else {
            repos()
        };
        let mut world = World::new(delivery);
        let lock = world.lock();
        let grant_id = world
            .store
            .grant_create(
                &owner(),
                NewGrant {
                    project_id: "alpha".into(),
                    kind: GrantKind::LandDirect,
                    conditions: json!({
                        "v": 1,
                        "actions": ["push"],
                        "expires_at": "2999-01-01T00:00:00.000Z"
                    }),
                },
            )
            .unwrap()
            .value
            .id;
        let card = world.card("push it");
        let run = world.builder(&card, Some(work.path()), "fm/alpha-1");
        let approval = world
            .store
            .approval_request(
                &owner(),
                NewApproval {
                    kind: ApprovalKind::LandDirect,
                    card: card.id.clone(),
                    run_id: Some(run),
                    pr_id: None,
                    branch: Some("fm/alpha-1".into()),
                    head_sha: head.clone(),
                    action: ApprovalAction::Push,
                    memory_paths: None,
                    grant_id: Some(grant_id.clone()),
                },
            )
            .unwrap()
            .value;
        if approval.state == ApprovalState::Requested {
            world.accept(&approval);
        }
        let before = git(target.path(), &["rev-parse", "refs/heads/main"]);
        // Time passes: the grant's end is now behind us.
        world
            .store
            .conn()
            .execute(
                "UPDATE grants SET conditions = json_set(conditions, '$.expires_at', '2020-01-01T00:00:00.000Z') WHERE id = ?1",
                [&grant_id],
            )
            .unwrap();
        let landed = lander(&forge)
            .land(&lock, &mut world.store, &approval.id)
            .unwrap();
        assert!(
            matches!(&landed, Landed::Stale { why, .. } if why.contains("expired")),
            "{delivery:?}: {landed:?}"
        );
        assert_eq!(
            git(target.path(), &["rev-parse", "refs/heads/main"]),
            before
        );
    }
}

/// A `local-only` repository with `main` checked out and a linked worktree
/// on `fm/alpha-1` one commit ahead. Returns the dirs and the branch head.
fn local_repos() -> (tempfile::TempDir, tempfile::TempDir, String) {
    let main = tempfile::tempdir().unwrap();
    git(main.path(), &["init", "-q", "-b", "main"]);
    commit(main.path(), "base");
    let work = tempfile::tempdir().unwrap();
    git(
        main.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fm/alpha-1",
            &work.path().display().to_string(),
        ],
    );
    let head = commit(work.path(), "feature");
    (main, work, head)
}

#[test]
fn local_only_land_fast_forwards_the_checkout() {
    let forge = FakeForge::new();

    // `main` is checked out: it is merged there, so its files follow.
    let (main, work, head) = local_repos();
    let mut world = World::new(Delivery::LocalOnly);
    let lock = world.lock();
    let approval = direct_approval(&mut world, work.path(), &head);
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Succeeded { .. }), "{landed:?}");
    assert_eq!(git(main.path(), &["rev-parse", "refs/heads/main"]), head);
    assert!(main.path().join("feature").exists());
    assert_eq!(git(main.path(), &["status", "--porcelain"]), "");
    assert!(
        forge.merges().is_empty(),
        "no forge for a local-only project"
    );

    // No worktree has `main` checked out: only the ref moves.
    let (main, work, head) = local_repos();
    git(main.path(), &["switch", "-q", "--detach"]);
    let mut world = World::new(Delivery::LocalOnly);
    let lock = world.lock();
    let approval = direct_approval(&mut world, work.path(), &head);
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Succeeded { .. }), "{landed:?}");
    assert_eq!(git(main.path(), &["rev-parse", "refs/heads/main"]), head);

    // `main` moved past the bound head: stale, and `main` keeps its commit.
    let (main, work, head) = local_repos();
    let mut world = World::new(Delivery::LocalOnly);
    let lock = world.lock();
    let approval = direct_approval(&mut world, work.path(), &head);
    let moved = commit(main.path(), "elsewhere");
    let landed = lander(&forge)
        .land(&lock, &mut world.store, &approval.id)
        .unwrap();
    assert!(matches!(landed, Landed::Stale { .. }), "{landed:?}");
    assert_eq!(git(main.path(), &["rev-parse", "refs/heads/main"]), moved);

    // The ref moves only from the head the checks saw: a race that moves it
    // between the check and the update is refused, never overwritten.
    let (main, work, head) = local_repos();
    git(main.path(), &["switch", "-q", "--detach"]);
    let base = git(main.path(), &["rev-parse", "refs/heads/main"]);
    git(main.path(), &["switch", "-q", "main"]);
    let raced = commit(main.path(), "raced");
    git(main.path(), &["switch", "-q", "--detach"]);
    let advanced = direct::advance(work.path(), "main", &base, &head);
    assert!(
        matches!(advanced, direct::Pushed::Refused(_)),
        "{advanced:?}"
    );
    assert_eq!(git(main.path(), &["rev-parse", "refs/heads/main"]), raced);
}
