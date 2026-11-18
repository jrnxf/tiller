//! The memory routes: the view, Edit, Keep through a stand-in kb command,
//! Discard, and who may use them (`memory.md` sections 4 and 7).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tiller_core::state::memory::NewNote;
use tiller_core::{Actor, Ctx};
use tiller_panes::testing::Fleet;

use crate::api::tests::{Answer, HOST, app_with_kb, get, mutation, owner_cookie, send};

const SCOPE: &str = "example.com/example/alpha";

struct Home {
    fleet: Fleet,
    app: Router,
    cookie: String,
    /// The stand-in kb's own folder: its store and what `add` was given.
    kb: tempfile::TempDir,
}

/// A fleet whose project has a repo scope, a kb store with one global note,
/// and a kb command that accepts every note unless `refuse` is set.
fn home(refuse: bool) -> Home {
    let fleet = Fleet::new();
    fleet
        .store
        .conn()
        .execute(
            "UPDATE projects SET remote = 'git@example.com:example/alpha.git' WHERE id = 'alpha'",
            [],
        )
        .unwrap();
    let kb = tempfile::tempdir().unwrap();
    let notes = kb.path().join("store/notes/global");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(
        notes.join("small-commits.md"),
        "---\nid: small-commits\ntitle: Small commits\nscope: global\nkind: convention\n\
         created: 2026-11-10T09:00:00Z\nlast_verified: 2026-11-10T09:00:00Z\n\
         confidence: medium\nindexed: true\n---\nCommit in small steps.\n",
    )
    .unwrap();
    let script = kb.path().join("kb");
    let out = kb.path().display();
    let body = if refuse {
        "cat > /dev/null\nprintf '{\"error\":\"a note with this title exists\"}'\nexit 1\n"
            .to_owned()
    } else {
        format!(
            "cat > {out}/input.json\nprintf '%s ' \"$@\" > {out}/args\n\
             printf '{{\"id\":\"never-rebase\",\"status\":\"added\"}}'\n"
        )
    };
    std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        fleet.dir.path().join("config.toml"),
        format!(
            "[memory]\nkb_command = [{:?}]\n",
            script.display().to_string()
        ),
    )
    .unwrap();
    let app = app_with_kb(fleet.dir.path(), &kb.path().join("store"));
    let cookie = owner_cookie(fleet.dir.path());
    Home {
        fleet,
        app,
        cookie,
        kb,
    }
}

impl Home {
    /// A note a builder suggests: its pending id.
    fn suggest(&mut self, title: &str, body: &str) -> String {
        let session = self.fleet.worker("w1:p2");
        self.fleet
            .store
            .mem_note(
                &Ctx::new(Actor::Session(session)),
                None,
                &NewNote {
                    project: None,
                    kind: "gotcha".into(),
                    title: title.into(),
                    body: body.into(),
                },
            )
            .unwrap()
            .value
            .id
    }

    async fn call(&self, method: &str, path: &str, body: Value) -> Answer {
        send(
            &self.app,
            mutation(method, path, body, &[("cookie", &self.cookie)]),
        )
        .await
    }

    async fn view(&self, query: &str) -> Value {
        let answer = get(&self.app, &format!("/api/v1/memory{query}")).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        serde_json::from_str(&answer.body).unwrap()
    }

    fn pending(&self) -> i64 {
        self.fleet
            .open()
            .conn()
            .query_row("SELECT count(*) FROM memory_pending", [], |row| row.get(0))
            .unwrap()
    }

    fn kb_file(&self, name: &str) -> PathBuf {
        self.kb.path().join(name)
    }
}

fn data(answer: &Answer) -> Value {
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    serde_json::from_str::<Value>(&answer.body).unwrap()["data"].clone()
}

#[tokio::test]
async fn the_view_shows_the_launch_and_the_queue() {
    let mut home = home(false);
    let id = home.suggest("Never rebase", "Rebasing a pushed branch broke CI.");
    let view = home.view("?project=alpha").await;
    assert_eq!(view["project_id"], "alpha");
    assert_eq!(view["role"], "builder");
    assert_eq!(view["kb"]["state"], "ok");
    assert_eq!(view["kb"]["scopes"], json!([SCOPE, "global"]));
    assert_eq!(view["budget"]["limit"], 7_500);
    assert!(view["budget"]["used"].as_i64().unwrap() > 0, "{view}");
    assert_eq!(
        view["live"],
        json!([{"id": "small-commits", "title": "Small commits", "kind": "convention",
                "scope": "global", "tokens": view["live"][0]["tokens"], "loaded": true,
                "reason": null}])
    );
    let pending = &view["pending"][0];
    assert_eq!(pending["id"], id.as_str());
    assert_eq!(pending["source"], "fleet");
    assert_eq!(pending["role"], "builder");
    assert_eq!(pending["rev"], 1);
    assert!(pending.get("session_id").is_none(), "{pending}");

    // Without a project, nothing loads and every project's queue shows.
    let all = home.view("").await;
    assert!(all["project_id"].is_null());
    assert_eq!(all["live"], json!([]));
    assert_eq!(all["pending"].as_array().unwrap().len(), 1);
    // Another role's launch, and a role that is not one.
    assert_eq!(
        home.view("?project=alpha&role=scout").await["role"],
        "scout"
    );
    let bad = get(&home.app, "/api/v1/memory?project=alpha&role=owner").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
}

#[tokio::test]
async fn edit_then_keep_writes_to_kb_and_drops_the_row() {
    let mut home = home(false);
    let id = home.suggest("Never rebase", "Rebasing a pushed branch broke CI.");
    let path = format!("/api/v1/memory/pending/{id}");
    let edit = json!({"idempotency_key": "e1", "rev": 1, "title": "Never rebase a pushed branch"});
    let row = data(&home.call("PATCH", &path, edit.clone()).await);
    assert_eq!(row["title"], "Never rebase a pushed branch");
    assert_eq!(row["rev"], 2);
    // The same key replays; a stale rev is a conflict.
    let replay: Value = serde_json::from_str(&home.call("PATCH", &path, edit).await.body).unwrap();
    assert_eq!(replay["replayed"], true);
    let stale = home
        .call(
            "POST",
            &format!("{path}/keep"),
            json!({"idempotency_key": "k0", "rev": 1}),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
    assert!(!home.kb_file("input.json").exists());

    let keep = json!({"idempotency_key": "k1", "rev": 2});
    let kept = data(
        &home
            .call("POST", &format!("{path}/keep"), keep.clone())
            .await,
    );
    assert_eq!(kept, json!({"note_id": "never-rebase", "scope": SCOPE}));
    assert_eq!(home.pending(), 0);
    let input: Value =
        serde_json::from_str(&std::fs::read_to_string(home.kb_file("input.json")).unwrap())
            .unwrap();
    assert_eq!(input["title"], "Never rebase a pushed branch");
    assert_eq!(input["scope"], SCOPE);
    let args = std::fs::read_to_string(home.kb_file("args")).unwrap();
    assert!(args.starts_with("add --json"), "{args}");

    // A replay of the Keep runs no second add.
    std::fs::remove_file(home.kb_file("input.json")).unwrap();
    let again: Value =
        serde_json::from_str(&home.call("POST", &format!("{path}/keep"), keep).await.body).unwrap();
    assert_eq!(again["replayed"], true, "{again}");
    assert!(!home.kb_file("input.json").exists());
}

/// Keep deletes the row only after kb accepted the note (`memory.md`
/// section 7).
#[tokio::test]
async fn a_failed_add_leaves_the_note_pending() {
    let mut home = home(true);
    let id = home.suggest("Never rebase", "Rebasing a pushed branch broke CI.");
    let answer = home
        .call(
            "POST",
            &format!("/api/v1/memory/pending/{id}/keep"),
            json!({"idempotency_key": "k1", "rev": 1}),
        )
        .await;
    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        answer.body
    );
    assert!(
        answer.body.contains("a note with this title exists"),
        "{}",
        answer.body
    );
    assert_eq!(home.pending(), 1);
}

#[tokio::test]
async fn discard_drops_the_row() {
    let mut home = home(false);
    let id = home.suggest("Never rebase", "Rebasing a pushed branch broke CI.");
    let done = data(
        &home
            .call(
                "POST",
                &format!("/api/v1/memory/pending/{id}/discard"),
                json!({"idempotency_key": "d1", "rev": 1}),
            )
            .await,
    );
    assert_eq!(done, json!({}));
    assert_eq!(home.pending(), 0);
    assert!(!home.kb_file("input.json").exists());
}

/// A fleet session cannot list, edit, keep or discard pending notes: every
/// route needs the owner's sign-in (`memory.md` section 7).
#[tokio::test]
async fn the_memory_routes_are_the_owners() {
    let mut home = home(false);
    let id = home.suggest("Never rebase", "Rebasing a pushed branch broke CI.");
    let view = send(
        &home.app,
        Request::get("/api/v1/memory?project=alpha")
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(view.status, StatusCode::FORBIDDEN, "{}", view.body);
    let decision = json!({"idempotency_key": "x", "rev": 1});
    for (method, path) in [
        ("PATCH", format!("/api/v1/memory/pending/{id}")),
        ("POST", format!("/api/v1/memory/pending/{id}/keep")),
        ("POST", format!("/api/v1/memory/pending/{id}/discard")),
    ] {
        let answer = send(&home.app, mutation(method, &path, decision.clone(), &[])).await;
        assert_eq!(
            answer.status,
            StatusCode::FORBIDDEN,
            "{path}: {}",
            answer.body
        );
    }
    assert_eq!(home.pending(), 1);
    assert!(!home.kb_file("input.json").exists());
}

/// Health warns while the newest launch found no kb store (`memory.md`
/// section 3).
#[tokio::test]
async fn health_warns_while_the_kb_store_is_missing() {
    let home = home(false);
    let warnings = async || {
        let health: Value =
            serde_json::from_str(&get(&home.app, "/api/v1/health").await.body).unwrap();
        health["warnings"].clone()
    };
    assert_eq!(warnings().await, json!([]));
    let missing = tiller_core::kb::render(&tiller_core::kb::Read::missing(None), &[], 0, 7_500);
    home.fleet
        .open()
        .mem_load(
            &Ctx::new(Actor::Daemon),
            &home.fleet.orchestrator_session,
            &missing,
        )
        .unwrap();
    let shown = warnings().await;
    assert!(
        shown[0].as_str().unwrap().contains("kb store is missing"),
        "{shown}"
    );
}
