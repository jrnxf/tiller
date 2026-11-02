//! A small fleet in a fresh `state.db` for the observation tests.

use std::path::Path;

use tiller_core::domain::{Boundary, CardKind, Delivery, Effort, RunRole, Session};
use tiller_core::state::cards::NewCard;
use tiller_core::state::projects::{Init, NewProject};
use tiller_core::state::runs::{NewOrchestrator, NewRun, SpawnChecks};
use tiller_core::{Actor, Ctx, Store};

/// Project `alpha` with a live orchestrator (Claude id `claude-orch`) and one
/// active builder run on `alpha-1` whose session is not bound yet.
pub(crate) struct Fleet {
    pub orchestrator: String,
    pub worker: String,
    pub run: String,
}

pub(crate) fn fleet(store: &mut Store) -> Fleet {
    let owner = Ctx::new(Actor::Owner);
    store
        .init(
            &owner,
            Init {
                host_label: "test-host".into(),
                lane_capacity: 2,
                ram_floor_mb: None,
                disk_floor_mb: None,
                boundary: Boundary::Personal,
            },
        )
        .unwrap();
    store
        .project_add(
            &owner,
            NewProject {
                id: "alpha".into(),
                name: "Alpha".into(),
                repo: Some("example/alpha".into()),
                remote: None,
                default_branch: None,
                delivery: Delivery::DirectPr,
                checks: None,
                capacity: Some(2),
                deploy_workflow: None,
                deploy_environment: None,
                deploy_timeout_minutes: None,
            },
        )
        .unwrap();
    let orchestrator = store
        .session_start(
            &owner,
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
    let card = store
        .card_create(
            &owner,
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: CardKind::Ship,
                title: "a card".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let started = store
        .run_start(
            &Ctx::new(Actor::Session(orchestrator.clone())),
            None,
            NewRun {
                card: card.id,
                role: RunRole::Builder,
                model: "test-model".into(),
                effort: Effort::Medium,
                host_id: None,
                worktree_path: None,
                branch: None,
                base_branch: None,
                base_sha: None,
                config_hash: None,
                checks: SpawnChecks::default(),
            },
        )
        .unwrap()
        .value;
    Fleet {
        orchestrator,
        worker: started.session.id,
        run: started.run.id,
    }
}

pub(crate) fn session(store: &Store, id: &str) -> Session {
    tiller_core::state::get(store.conn(), "session", id).unwrap()
}

pub(crate) fn count(store: &Store, sql: &str) -> i64 {
    store.conn().query_row(sql, [], |row| row.get(0)).unwrap()
}

/// A migrated `state.db` under `home`, as `tiller init` leaves it.
pub(crate) fn file_store(home: &Path) -> (Store, Fleet) {
    let mut store = Store::open(home.join("state.db")).unwrap();
    let fleet = fleet(&mut store);
    (store, fleet)
}
