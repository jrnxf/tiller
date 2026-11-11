use std::cell::RefCell;

use serde_json::json;
use tiller_agents::land::forge::AppToken;
use tiller_core::domain::{Attention, AttentionKind, Boundary, Event};
use tiller_core::state::list;
use tiller_core::state::projects::{Init, NewProject};
use tiller_core::{Actor, Ctx};

use super::*;

const APP_ID: u64 = 4242;

/// The forge as the fake gives it: the rules on each branch, and the
/// rulesets by id.
#[derive(Default)]
struct Fake {
    rules: Vec<Value>,
    rulesets: HashMap<u64, Value>,
    /// Set: every read fails, as on a network error.
    down: bool,
    reads: RefCell<Vec<String>>,
}

impl RulesetForge for Fake {
    fn branch_rules(&self, repo: &str, branch: &str) -> Result<Vec<Value>, GateError> {
        self.reads.borrow_mut().push(format!("{repo}@{branch}"));
        if self.down {
            return Err(GateError("connection refused".into()));
        }
        Ok(self.rules.clone())
    }

    fn ruleset(&self, _: &str, id: u64) -> Result<Value, GateError> {
        self.rulesets
            .get(&id)
            .cloned()
            .ok_or_else(|| GateError(format!("no ruleset {id}")))
    }
}

/// The merge App as the fake gives it: its id, or none, and whether it is
/// installed on the repo.
struct App {
    id: Option<u64>,
    installed: bool,
    /// Set: the App cannot be asked, as when GitHub is not reached or the
    /// key file is mid-replace.
    unreadable: bool,
}

const APP: App = App {
    id: Some(APP_ID),
    installed: true,
    unreadable: false,
};

impl AppTokens for App {
    fn mint(&self, repo: &str) -> Result<AppToken, ForgeError> {
        panic!("the ruleset check minted a token for {repo}")
    }

    fn app_id(&self) -> Result<u64, ForgeError> {
        if self.unreadable {
            return Err(ForgeError::Read("the key is not a usable RSA key".into()));
        }
        self.id.ok_or_else(|| ForgeError::NoApp {
            repo: "any repo".into(),
            why: "no merge App is installed on this host".into(),
        })
    }

    fn installed(&self, repo: &str) -> Result<(), ForgeError> {
        if self.installed {
            Ok(())
        } else {
            Err(ForgeError::NoApp {
                repo: repo.into(),
                why: "the App is not installed: GitHub answered 404".into(),
            })
        }
    }
}

fn rule(kind: &str, ruleset: u64) -> Value {
    json!({"type": kind, "ruleset_source_type": "Repository", "ruleset_source": "example/alpha", "ruleset_id": ruleset})
}

/// The rules of a gated default branch, all from ruleset 9.
fn gated_rules() -> Vec<Value> {
    let mut rules: Vec<Value> = ["pull_request", "update", "non_fast_forward", "deletion"]
        .iter()
        .map(|kind| rule(kind, 9))
        .collect();
    let mut checks = rule("required_status_checks", 9);
    checks["parameters"] = json!({
        "strict_required_status_checks_policy": false,
        "required_status_checks": [{"context": "rust"}],
    });
    rules.push(checks);
    rules
}

fn ruleset(id: u64, actors: Value) -> Value {
    json!({"id": id, "name": "merge gate", "target": "branch", "enforcement": "active", "bypass_actors": actors})
}

fn only_the_app() -> Value {
    json!([{"actor_id": APP_ID, "actor_type": "Integration", "bypass_mode": "always"}])
}

/// The forge of a gated repo: every rule, and the App as the only bypass
/// actor.
fn gated() -> Fake {
    Fake {
        rules: gated_rules(),
        rulesets: HashMap::from([(9, ruleset(9, only_the_app()))]),
        ..Fake::default()
    }
}

fn detect(forge: &Fake, app: &App) -> String {
    match judge(forge, app, "example/alpha", "main").unwrap() {
        Verdict::Detect(why) => why,
        Verdict::Enforced => panic!("expected detect"),
    }
}

/// A store with one `no-mistakes` project, `alpha`, on `example/alpha`.
fn store() -> Store {
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
            project("alpha", Some("example/alpha"), Delivery::NoMistakes),
        )
        .unwrap();
    store
}

fn project(id: &str, repo: Option<&str>, delivery: Delivery) -> NewProject {
    NewProject {
        id: id.into(),
        name: id.into(),
        repo: repo.map(Into::into),
        remote: None,
        default_branch: None,
        delivery,
        checks: None,
        capacity: None,
        deploy_workflow: None,
        deploy_environment: None,
        deploy_timeout_minutes: None,
    }
}

fn gate_of(store: &Store, id: &str) -> MergeGate {
    store
        .projects()
        .unwrap()
        .into_iter()
        .find(|project| project.id == id)
        .unwrap()
        .merge_gate
}

fn events(store: &Store, kind: &str) -> Vec<Event> {
    list(store.conn(), "kind = ?1 ORDER BY seq", &[&kind]).unwrap()
}

fn recovery(store: &Store) -> Vec<Attention> {
    list(
        store.conn(),
        "kind = 'recovery' AND withdrawn_at IS NULL AND answered_at IS NULL",
        &[],
    )
    .unwrap()
}

#[test]
fn a_gated_branch_with_only_the_app_as_bypass_is_enforced() {
    assert_eq!(
        judge(&gated(), &APP, "example/alpha", "main").unwrap(),
        Verdict::Enforced
    );
    // Rules split over two rulesets: each must bypass only the App.
    let mut forge = gated();
    forge.rules[0]["ruleset_id"] = json!(10);
    forge.rulesets.insert(10, ruleset(10, only_the_app()));
    assert_eq!(
        judge(&forge, &APP, "example/alpha", "main").unwrap(),
        Verdict::Enforced
    );
    forge.rulesets.insert(
        10,
        ruleset(
            10,
            json!([{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}]),
        ),
    );
    assert!(detect(&forge, &APP).contains("only bypass actor"));
}

#[test]
fn each_missing_rule_is_detect() {
    for missing in REQUIRED_RULES {
        let mut forge = gated();
        forge.rules.retain(|rule| rule["type"] != missing);
        let why = detect(&forge, &APP);
        assert!(why.contains(missing), "{missing}: {why}");
    }
    // Required checks that name no check gate nothing.
    let mut forge = gated();
    for rule in &mut forge.rules {
        if rule["type"] == "required_status_checks" {
            rule["parameters"]["required_status_checks"] = json!([]);
        }
    }
    assert!(detect(&forge, &APP).contains("no status check"));
}

#[test]
fn a_bypass_list_other_than_the_app_alone_is_detect() {
    let lists = [
        // No bypass actor: the App cannot merge either.
        json!([]),
        // Another App.
        json!([{"actor_id": 7, "actor_type": "Integration", "bypass_mode": "always"}]),
        // The App, but only through pull requests.
        json!([{"actor_id": APP_ID, "actor_type": "Integration", "bypass_mode": "pull_request"}]),
        // The App and the repo admins.
        json!([
            {"actor_id": APP_ID, "actor_type": "Integration", "bypass_mode": "always"},
            {"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"},
        ]),
    ];
    for actors in lists {
        let mut forge = gated();
        forge.rulesets.insert(9, ruleset(9, actors.clone()));
        let why = detect(&forge, &APP);
        assert!(why.contains("only bypass actor"), "{actors}: {why}");
        assert!(why.contains("merge gate"), "names the ruleset: {why}");
    }
    // A reply without the list (a login without admin rights) proves nothing.
    let mut forge = gated();
    forge
        .rulesets
        .insert(9, json!({"id": 9, "name": "merge gate"}));
    assert!(detect(&forge, &APP).contains("not readable"));
}

#[test]
fn no_app_or_an_uninstalled_app_is_detect() {
    let no_app = App {
        id: None,
        installed: false,
        ..APP
    };
    let forge = gated();
    assert!(detect(&forge, &no_app).contains("no merge App"));
    assert!(forge.reads.borrow().is_empty(), "no App, no forge read");

    let elsewhere = App {
        id: Some(APP_ID),
        installed: false,
        ..APP
    };
    assert!(detect(&gated(), &elsewhere).contains("not installed"));
}

#[test]
fn ruleset_check_sets_merge_gate() {
    let mut store = store();
    let owner = Ctx::new(Actor::Owner);
    for (id, repo, delivery) in [
        ("sandbox", Some("example/sandbox"), Delivery::DirectPush),
        ("offline", None, Delivery::DirectPr),
        ("local", Some("example/local"), Delivery::LocalOnly),
    ] {
        store
            .project_add(&owner, project(id, repo, delivery))
            .unwrap();
    }
    let daemon = Ctx::new(Actor::Daemon);
    let forge = gated();
    let mut last = HashMap::new();
    let start = Instant::now();

    let judged = check_due(&mut store, &forge, &APP, &daemon, &mut last, start).unwrap();
    // The gated project and the opted-in direct-push sandbox; never a
    // project with no repo, or a local-only one.
    let ids: Vec<&str> = judged.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, ["alpha", "sandbox"]);
    assert!(
        judged
            .iter()
            .all(|(_, verdict)| *verdict == Verdict::Enforced)
    );
    assert_eq!(gate_of(&store, "alpha"), MergeGate::Enforced);
    assert_eq!(gate_of(&store, "sandbox"), MergeGate::Enforced);
    assert_eq!(gate_of(&store, "offline"), MergeGate::Detect);
    assert_eq!(
        *forge.reads.borrow(),
        ["example/alpha@main", "example/sandbox@main"]
    );
    assert_eq!(events(&store, "project.merge_gate_changed").len(), 2);
    assert!(store.merge_gate_changed_at("alpha").unwrap().is_some());
    assert!(events(&store, "host.degraded").is_empty());
    assert!(recovery(&store).is_empty());

    // Within the hour nothing is read again; after it, a holding gate is
    // read and changes nothing.
    let soon = start + Duration::from_secs(59 * 60);
    assert!(
        check_due(&mut store, &forge, &APP, &daemon, &mut last, soon)
            .unwrap()
            .is_empty()
    );
    assert_eq!(forge.reads.borrow().len(), 2);
    let later = start + RECHECK;
    assert_eq!(
        check_due(&mut store, &forge, &APP, &daemon, &mut last, later)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(forge.reads.borrow().len(), 4);
    assert_eq!(events(&store, "project.merge_gate_changed").len(), 2);

    // A project never enforced stays detect without noise.
    let mut fresh = self::store();
    let open = Fake::default();
    let judged = check_due(&mut fresh, &open, &APP, &daemon, &mut HashMap::new(), start).unwrap();
    assert!(matches!(judged[0].1, Verdict::Detect(_)));
    assert_eq!(gate_of(&fresh, "alpha"), MergeGate::Detect);
    assert!(events(&fresh, "project.merge_gate_changed").is_empty());
    assert!(events(&fresh, "host.degraded").is_empty());
    assert!(recovery(&fresh).is_empty());
}

/// A verdict holds for the repo it judged: a project whose repo moves is
/// judged again on the next pass, not an hour later.
#[test]
fn a_moved_repo_is_checked_again_at_once() {
    let mut store = store();
    let daemon = Ctx::new(Actor::Daemon);
    let forge = gated();
    let mut last = HashMap::new();
    let start = Instant::now();
    check_due(&mut store, &forge, &APP, &daemon, &mut last, start).unwrap();
    assert_eq!(*forge.reads.borrow(), ["example/alpha@main"]);

    let rev = store.project("alpha").unwrap().rev;
    store
        .project_edit(
            &Ctx::new(Actor::Owner),
            "alpha",
            rev,
            tiller_core::state::projects::ProjectEdit {
                repo: Some(Some("example/moved".into())),
                ..Default::default()
            },
        )
        .unwrap();
    let soon = start + Duration::from_secs(60);
    let judged = check_due(&mut store, &forge, &APP, &daemon, &mut last, soon).unwrap();
    assert_eq!(judged.len(), 1);
    assert_eq!(
        *forge.reads.borrow(),
        ["example/alpha@main", "example/moved@main"]
    );
}

/// A project that moves to another repo and back within the hour is judged
/// again for its first repo, not left with the other repo's verdict. The
/// check keeps one entry per project, so the map does not grow with edits.
#[test]
fn a_repo_moved_back_is_checked_again_at_once() {
    let mut store = store();
    let daemon = Ctx::new(Actor::Daemon);
    let owner = Ctx::new(Actor::Owner);
    let forge = gated();
    let mut last = HashMap::new();
    let start = Instant::now();
    let move_to = |store: &mut Store, repo: &str| {
        let rev = store.project("alpha").unwrap().rev;
        store
            .project_edit(
                &owner,
                "alpha",
                rev,
                tiller_core::state::projects::ProjectEdit {
                    repo: Some(Some(repo.into())),
                    ..Default::default()
                },
            )
            .unwrap();
    };
    check_due(&mut store, &forge, &APP, &daemon, &mut last, start).unwrap();
    move_to(&mut store, "example/moved");
    let soon = start + Duration::from_secs(10 * 60);
    check_due(&mut store, &forge, &APP, &daemon, &mut last, soon).unwrap();
    move_to(&mut store, "example/alpha");
    let later = start + Duration::from_secs(20 * 60);
    let judged = check_due(&mut store, &forge, &APP, &daemon, &mut last, later).unwrap();
    assert_eq!(judged.len(), 1, "the repo moved back is judged again");
    assert_eq!(
        *forge.reads.borrow(),
        [
            "example/alpha@main",
            "example/moved@main",
            "example/alpha@main"
        ]
    );
    assert_eq!(last.len(), 1, "one entry per project");

    // A project the check no longer looks at keeps no entry.
    let rev = store.project("alpha").unwrap().rev;
    store
        .project_edit(
            &owner,
            "alpha",
            rev,
            tiller_core::state::projects::ProjectEdit {
                repo: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
    check_due(&mut store, &forge, &APP, &daemon, &mut last, later).unwrap();
    assert!(last.is_empty(), "{last:?}");
}

#[test]
fn edited_ruleset_drops_project_to_detect() {
    let mut store = store();
    let daemon = Ctx::new(Actor::Daemon);
    let mut last = HashMap::new();
    let start = Instant::now();
    check_due(&mut store, &gated(), &APP, &daemon, &mut last, start).unwrap();
    assert_eq!(gate_of(&store, "alpha"), MergeGate::Enforced);

    // The owner's account was added as a bypass actor.
    let mut edited = gated();
    edited.rulesets.insert(
        9,
        ruleset(
            9,
            json!([
                {"actor_id": APP_ID, "actor_type": "Integration", "bypass_mode": "always"},
                {"actor_id": 1, "actor_type": "User", "bypass_mode": "always"},
            ]),
        ),
    );
    // A forge or an App that cannot be read changes nothing, and is tried
    // again after RETRY, not an hour on.
    let down = Fake {
        down: true,
        ..Fake::default()
    };
    let unreadable = App {
        unreadable: true,
        ..APP
    };
    let mut later = start + RECHECK;
    for (forge, app) in [(&down, &APP), (&edited, &unreadable)] {
        assert!(
            check_due(&mut store, forge, app, &daemon, &mut last, later)
                .unwrap()
                .is_empty()
        );
        assert_eq!(gate_of(&store, "alpha"), MergeGate::Enforced);
        let soon = later + RETRY - Duration::from_secs(1);
        assert!(
            check_due(&mut store, &edited, &APP, &daemon, &mut last, soon)
                .unwrap()
                .is_empty(),
            "retried before RETRY"
        );
        later += RETRY;
    }
    assert!(events(&store, "host.degraded").is_empty());

    let judged = check_due(&mut store, &edited, &APP, &daemon, &mut last, later).unwrap();
    assert_eq!(judged.len(), 1);
    assert_eq!(gate_of(&store, "alpha"), MergeGate::Detect);
    let changed = events(&store, "project.merge_gate_changed");
    assert_eq!(changed.len(), 2);
    assert_eq!(changed[1].payload["from"], "enforced");
    assert_eq!(changed[1].payload["to"], "detect");
    assert!(
        changed[1].payload["reason"]
            .as_str()
            .unwrap()
            .contains("only bypass actor")
    );
    let degraded = events(&store, "host.degraded");
    assert_eq!(degraded.len(), 1);
    assert_eq!(degraded[0].payload["check"], "merge_gate");
    assert_eq!(degraded[0].payload["project"], "alpha");
    let items = recovery(&store);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind, AttentionKind::Recovery);
    assert!(items[0].question.contains("alpha"), "{}", items[0].question);

    // Removing the ruleset outright, an hour on, raises nothing new.
    let removed = Fake::default();
    let after = later + RECHECK;
    check_due(&mut store, &removed, &APP, &daemon, &mut last, after).unwrap();
    assert_eq!(events(&store, "host.degraded").len(), 1);
    assert_eq!(recovery(&store).len(), 1);

    // A deleted key downgrades the same way.
    let mut store = self::store();
    let mut last = HashMap::new();
    check_due(&mut store, &gated(), &APP, &daemon, &mut last, start).unwrap();
    let no_app = App {
        id: None,
        installed: false,
        ..APP
    };
    check_due(&mut store, &gated(), &no_app, &daemon, &mut last, later).unwrap();
    assert_eq!(gate_of(&store, "alpha"), MergeGate::Detect);
    assert_eq!(recovery(&store).len(), 1);
}

#[test]
fn gh_rulesets_reads_every_page_and_the_ruleset() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(
        &gh,
        r#"#!/bin/sh
case "$*" in
  "api --paginate --slurp repos/example/alpha/rules/branches/main?per_page=100")
    echo '[[{"type":"pull_request","ruleset_id":9}],[{"type":"deletion","ruleset_id":9}]]' ;;
  "api --paginate --slurp repos/example/alpha/rules/branches/release%2F1.0%23x?per_page=100")
    echo '[[{"type":"update","ruleset_id":9}]]' ;;
  "api repos/example/alpha/rulesets/9") echo '{"id":9,"bypass_actors":[]}' ;;
  *) echo "gh: Not Found (HTTP 404)" >&2; exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let forge = GhRulesets::new(gh.to_string_lossy());
    let rules = forge.branch_rules("example/alpha", "main").unwrap();
    let kinds: Vec<&str> = rules
        .iter()
        .filter_map(|rule| rule["type"].as_str())
        .collect();
    assert_eq!(kinds, ["pull_request", "deletion"]);
    // A branch name stays one path segment.
    let rules = forge
        .branch_rules("example/alpha", "release/1.0#x")
        .unwrap();
    assert_eq!(rules[0]["type"], "update");
    assert_eq!(forge.ruleset("example/alpha", 9).unwrap()["id"], 9);
    let error = forge.ruleset("example/alpha", 10).unwrap_err();
    assert!(error.to_string().contains("HTTP 404"), "{error}");
}

#[test]
fn placing_or_fixing_the_key_changes_the_app_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let none = app_files(dir.path());
    assert_eq!(none, [None, None]);
    let key = dir.path().join(KEY_FILE);
    std::fs::write(&key, "key").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let placed = app_files(dir.path());
    assert_ne!(placed, none);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let fixed = app_files(dir.path());
    assert_ne!(fixed, placed);
    std::fs::write(dir.path().join(APP_ID_FILE), "4242").unwrap();
    assert_ne!(app_files(dir.path()), fixed);
    assert_eq!(app_files(dir.path()), app_files(dir.path()));
}
