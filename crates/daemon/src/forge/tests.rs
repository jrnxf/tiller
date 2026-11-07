use std::cell::Cell;
use std::collections::HashMap;

use tiller_core::domain::{
    AttentionKind, Boundary, CardKind, CardOutcome, Delivery, ObligationState, TriggerKind,
};
use tiller_core::state::cards::NewCard;
use tiller_core::state::projects::{Init, NewProject};
use tiller_core::state::records::NewObligation;
use tiller_core::{Actor, Ctx};

use super::*;

const PUSHED: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const LATER: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
const OLDER: &str = "c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3";

/// A forge with the runs it is given. `descendants` are the SHAs that
/// descend from the pushed one; `environments` maps a run id to what it
/// deployed to.
#[derive(Default)]
struct Fake {
    runs: Vec<DeployRun>,
    descendants: Vec<&'static str>,
    environments: HashMap<u64, Vec<&'static str>>,
    reads: Cell<u32>,
}

impl DeployForge for Fake {
    fn runs(&self, _: &str, _: &str, _: &str) -> Result<Vec<DeployRun>, DeployError> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.runs.clone())
    }

    fn deployed_environments(&self, _: &str, run: &DeployRun) -> Result<Vec<String>, DeployError> {
        Ok(self
            .environments
            .get(&run.id)
            .map(|list| list.iter().map(|e| (*e).to_owned()).collect())
            .unwrap_or_default())
    }

    fn descends(&self, _: &str, base: &str, head: &str) -> Result<bool, DeployError> {
        assert_eq!(base, PUSHED);
        Ok(self.descendants.contains(&head))
    }
}

fn run(id: u64, sha: &str, status: &str, conclusion: Option<&str>, at: Timestamp) -> DeployRun {
    DeployRun {
        id,
        head_sha: sha.into(),
        status: status.into(),
        conclusion: conclusion.map(Into::into),
        url: format!("https://github.com/example/alpha/actions/runs/{id}"),
        created_at: Some(at),
    }
}

fn watch(environment: Option<&str>, pushed_at: Timestamp) -> Watch {
    Watch {
        obligation_id: "obl".into(),
        card_id: "card".into(),
        repo: "example/alpha".into(),
        workflow: "deploy.yml".into(),
        branch: "main".into(),
        environment: environment.map(Into::into),
        sha: PUSHED.into(),
        pushed_at,
        due_at: pushed_at.plus_minutes(30),
    }
}

fn judge(forge: &Fake, watch: &Watch, minutes: i64) -> Option<(DeployResult, Option<u64>)> {
    evaluate(forge, watch, watch.pushed_at.plus_minutes(minutes))
        .unwrap()
        .map(|verdict| (verdict.result, verdict.run.map(|run| run.id)))
}

#[test]
fn deploy_success_must_match_sha_and_environment() {
    let pushed = Timestamp::now();
    let mut forge = Fake {
        runs: vec![run(7, PUSHED, "completed", Some("success"), pushed)],
        ..Fake::default()
    };
    // No environment named: the run's own conclusion is the result.
    assert_eq!(
        judge(&forge, &watch(None, pushed), 1),
        Some((DeployResult::Success, Some(7)))
    );
    // An environment named and the run deployed to another one: it does not
    // count, so ten minutes on the watch is `missing`.
    forge.environments.insert(7, vec!["staging"]);
    assert_eq!(judge(&forge, &watch(Some("production"), pushed), 1), None);
    assert_eq!(
        judge(&forge, &watch(Some("production"), pushed), 11),
        Some((DeployResult::Missing, None))
    );
    // A run that skipped the environment altogether does not count either.
    forge.environments.insert(7, vec![]);
    assert_eq!(judge(&forge, &watch(Some("production"), pushed), 1), None);
    // The run deployed to the environment the project names.
    forge.environments.insert(7, vec!["staging", "production"]);
    assert_eq!(
        judge(&forge, &watch(Some("production"), pushed), 1),
        Some((DeployResult::Success, Some(7)))
    );
}

#[test]
fn deploy_for_older_commit_does_not_count() {
    let pushed = Timestamp::now();
    let forge = Fake {
        // A recent success for a commit that does not descend from the
        // pushed one, and one created before the push.
        runs: vec![
            run(
                9,
                OLDER,
                "completed",
                Some("success"),
                pushed.plus_minutes(1),
            ),
            run(
                8,
                LATER,
                "completed",
                Some("success"),
                pushed.plus_minutes(-5),
            ),
        ],
        descendants: vec![LATER],
        ..Fake::default()
    };
    assert_eq!(judge(&forge, &watch(None, pushed), 5), None);
    assert_eq!(
        judge(&forge, &watch(None, pushed), 11),
        Some((DeployResult::Missing, None))
    );
}

#[test]
fn superseded_deploy_closes_landed() {
    let pushed = Timestamp::now();
    let forge = Fake {
        runs: vec![
            run(
                12,
                LATER,
                "completed",
                Some("success"),
                pushed.plus_minutes(4),
            ),
            run(11, PUSHED, "completed", Some("cancelled"), pushed),
        ],
        descendants: vec![LATER],
        ..Fake::default()
    };
    assert_eq!(
        judge(&forge, &watch(None, pushed), 5),
        Some((DeployResult::Superseded, Some(12)))
    );

    // End to end: the card closes landed and the watch is met.
    let (mut store, card, obligation) = open_watch(pushed);
    let daemon = Ctx::new(Actor::Daemon);
    let recorded = poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(5)).unwrap();
    assert_eq!(recorded.len(), 1);
    let card = store.card(&card).unwrap();
    assert_eq!(card.outcome, Some(CardOutcome::Landed));
    let obligation: tiller_core::domain::Obligation =
        tiller_core::state::get(store.conn(), "obligation", &obligation).unwrap();
    assert_eq!(obligation.state, ObligationState::Met);
    assert!(open_watches(&store).unwrap().is_empty());
}

#[test]
fn missing_and_timed_out_deploys_open_recovery() {
    let pushed = Timestamp::now();
    let (mut store, card, obligation) = open_watch(pushed);
    let daemon = Ctx::new(Actor::Daemon);

    // Nothing yet at 9 minutes, then `missing` at 10, written once.
    let mut forge = Fake::default();
    assert!(
        poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(9))
            .unwrap()
            .is_empty()
    );
    let first = poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(10)).unwrap();
    assert_eq!(first[0].1.result, DeployResult::Missing);
    poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(11)).unwrap();
    let recovery = |store: &Store| -> Vec<tiller_core::domain::Attention> {
        tiller_core::state::list(
            store.conn(),
            "kind = 'recovery' AND withdrawn_at IS NULL AND answered_at IS NULL",
            &[],
        )
        .unwrap()
    };
    assert_eq!(recovery(&store).len(), 1, "a re-poll opens nothing new");
    assert_eq!(recovery(&store)[0].kind, AttentionKind::Recovery);

    // A run appears and is still going at `due_at`: `timed_out`, once.
    forge.runs = vec![run(
        21,
        PUSHED,
        "in_progress",
        None,
        pushed.plus_minutes(12),
    )];
    assert!(
        poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(29))
            .unwrap()
            .is_empty()
    );
    let timed_out = poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(30)).unwrap();
    assert_eq!(timed_out[0].1.result, DeployResult::TimedOut);
    poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(31)).unwrap();
    assert_eq!(recovery(&store).len(), 2);
    assert_eq!(
        store.card(&card).unwrap().outcome,
        None,
        "the card stays open"
    );

    // The run concludes after both: the watch resolves, the recovery items
    // it opened are withdrawn and the card closes landed.
    forge.runs = vec![run(
        21,
        PUSHED,
        "completed",
        Some("success"),
        pushed.plus_minutes(12),
    )];
    let resolved = poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(35)).unwrap();
    assert_eq!(resolved[0].1.result, DeployResult::Success);
    assert!(recovery(&store).is_empty());
    assert_eq!(
        store.card(&card).unwrap().outcome,
        Some(CardOutcome::Landed)
    );
    let met: tiller_core::domain::Obligation =
        tiller_core::state::get(store.conn(), "obligation", &obligation).unwrap();
    assert_eq!(met.state, ObligationState::Met);
}

#[test]
fn a_failed_run_opens_recovery_once_and_the_watch_stays_open() {
    let pushed = Timestamp::now();
    let (mut store, card, _) = open_watch(pushed);
    let daemon = Ctx::new(Actor::Daemon);
    let forge = Fake {
        runs: vec![run(
            31,
            PUSHED,
            "completed",
            Some("failure"),
            pushed.plus_minutes(1),
        )],
        ..Fake::default()
    };
    for minute in 2..5 {
        poll_watches(&mut store, &forge, &daemon, pushed.plus_minutes(minute)).unwrap();
    }
    let open: Vec<tiller_core::domain::Attention> = tiller_core::state::list(
        store.conn(),
        "kind = 'recovery' AND withdrawn_at IS NULL",
        &[],
    )
    .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open_watches(&store).unwrap().len(), 1);
    assert_eq!(store.card(&card).unwrap().outcome, None);
}

/// A store with a `direct-push` project that deploys through `deploy.yml`, a
/// card, and the deploy watch the land of `PUSHED` opens. Returns the card
/// and the obligation ids.
fn open_watch(pushed: Timestamp) -> (Store, String, String) {
    let owner = Ctx::new(Actor::Owner);
    let mut store = Store::open_in_memory().unwrap();
    store
        .init(
            &owner,
            Init {
                host_label: "host-a".into(),
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
                delivery: Delivery::DirectPush,
                checks: None,
                capacity: None,
                deploy_workflow: Some("deploy.yml".into()),
                deploy_environment: None,
                deploy_timeout_minutes: Some(30),
            },
        )
        .unwrap();
    let card = store
        .card_create(
            &owner,
            None,
            NewCard {
                project_id: "alpha".into(),
                kind: CardKind::Ship,
                title: "ship it".into(),
                ..NewCard::default()
            },
            None,
        )
        .unwrap()
        .value;
    let obligation = store
        .obligation_open(
            &owner,
            NewObligation {
                project_id: "alpha".into(),
                card_id: Some(card.id.clone()),
                what: format!("watch the deploy of {PUSHED} to success"),
                trigger_kind: TriggerKind::Event,
                trigger_event: Some(json!({
                    "kind": "deploy.finished",
                    "sha": PUSHED,
                    "environment": null,
                })),
                due_at: Some(pushed.plus_minutes(30)),
            },
        )
        .unwrap()
        .value;
    // The watch counts from the obligation's opening, which the store
    // stamps a moment after `pushed`: pin it, so the minute marks the
    // tests poll at are exact.
    store
        .conn()
        .execute(
            "UPDATE obligations SET opened_at = ?2 WHERE id = ?1",
            rusqlite::params![obligation.id, pushed],
        )
        .unwrap();
    (store, card.id, obligation.id)
}

/// The real reader, against a `gh` that answers like GitHub: runs come from
/// the workflow, a run's environments from the deployments whose status
/// links to it, and descent from the compare status.
#[test]
fn gh_reads_runs_environments_and_descent() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(
        &gh,
        format!(
            r#"#!/bin/sh
case "$*" in
  *repos/example/alpha/actions/workflows/deploy.yml/runs*branch=main*) echo '{{"workflow_runs":[
    {{"id":41,"head_sha":"{LATER}","status":"completed","conclusion":"success","html_url":"https://github.com/example/alpha/actions/runs/41","created_at":"2026-11-05T10:00:00Z"}},
    {{"id":4,"head_sha":"{PUSHED}","status":"in_progress","conclusion":null,"html_url":"https://github.com/example/alpha/actions/runs/4","created_at":"2026-11-05T09:59:00Z"}}]}}' ;;
  *deployments?sha={LATER}*) echo '[{{"id":900,"environment":"production"}},{{"id":901,"environment":"staging"}}]' ;;
  *deployments/900/statuses*) echo '[{{"log_url":"https://github.com/example/alpha/actions/runs/41","target_url":""}}]' ;;
  *deployments/901/statuses*) echo '[{{"log_url":"https://github.com/example/alpha/actions/runs/410","target_url":""}}]' ;;
  *compare/{PUSHED}...{LATER}*) echo '{{"status":"ahead"}}' ;;
  *compare/{PUSHED}...{OLDER}*) echo '{{"status":"diverged"}}' ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let forge = GhDeploys::new(gh.to_str().unwrap());

    let runs = forge.runs("example/alpha", "deploy.yml", "main").unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].id, 41);
    assert_eq!(runs[0].conclusion.as_deref(), Some("success"));
    assert_eq!(runs[1].conclusion, None);
    assert!(runs[0].created_at.is_some());

    assert_eq!(
        forge
            .deployed_environments("example/alpha", &runs[0])
            .unwrap(),
        ["production"],
        "run 410 is not run 41"
    );
    assert!(forge.descends("example/alpha", PUSHED, LATER).unwrap());
    assert!(!forge.descends("example/alpha", PUSHED, OLDER).unwrap());
    assert!(forge.runs("example/other", "deploy.yml", "main").is_err());
}
