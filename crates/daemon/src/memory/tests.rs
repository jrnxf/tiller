//! The memory view against a real git repo: every write path changes the one
//! document it names and nothing else under `~/.tiller`, the scrub refuses
//! before `HEAD` moves, and concurrent writers never bundle their changes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use axum::Router;
use axum::http::StatusCode;
use serde_json::{Value, json};
use tiller_agents::land::scrub::ScrubList;
use tiller_core::domain::PeerScope;
use tiller_core::memory::{self as doc, NOTE_SECTION, RULE_SECTION, document_path};
use tiller_core::state::learning::EvalResult;
use tiller_core::state::peers::NewPairing;
use tiller_core::{Actor, Ctx, Error, NewEvent, ids};
use tiller_herdr::testing::{Fleet, owner};

use super::apply::Repo;
use crate::api::tests::{Answer, app_with_peers, get, mutation, owner_cookie, send};
use crate::dto::{ErrorBody, ErrorDetail, MutationResponse};
use crate::peer::link::Peers;

const ALPHA: &str = "projects/alpha.md";

struct Home {
    fleet: Fleet,
    app: Router,
    cookie: String,
}

fn home() -> Home {
    let fleet = Fleet::new();
    let app = app_with_peers(fleet.dir.path(), Peers::idle());
    let cookie = owner_cookie(fleet.dir.path());
    Home { fleet, app, cookie }
}

impl Home {
    fn path(&self) -> &Path {
        self.fleet.dir.path()
    }

    fn repo(&self) -> Repo {
        Repo::at(self.path())
    }

    async fn post(&self, path: &str, body: Value) -> Answer {
        send(
            &self.app,
            mutation("POST", path, body, &[("cookie", &self.cookie)]),
        )
        .await
    }

    async fn view(&self, project: &str) -> Value {
        let answer = get(&self.app, &format!("/api/v1/memory?scope={project}")).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        serde_json::from_str(&answer.body).unwrap()
    }

    async fn blob(&self, project: &str) -> String {
        self.view(project).await["doc"]["blob"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn edit(&self, key: &str, body: Value) -> Answer {
        let mut body = body;
        body["idempotency_key"] = json!(key);
        if body.get("base_blob").is_none() {
            let project = body["project"].as_str().unwrap().to_owned();
            body["base_blob"] = json!(self.blob(&project).await);
        }
        self.post("/api/v1/memory/edits", body).await
    }

    async fn add(&self, key: &str, project: &str, section: &str, text: &str) -> MutationResponse {
        let answer = self
            .edit(
                key,
                json!({"project": project, "op": "add", "section": section, "text": text}),
            )
            .await;
        ok(&answer)
    }

    fn git(&self, args: &[&str]) -> String {
        git(self.repo().root(), args)
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn ok(answer: &Answer) -> MutationResponse {
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    serde_json::from_str(&answer.body).unwrap()
}

fn refusal(answer: &Answer, status: StatusCode) -> ErrorDetail {
    assert_eq!(answer.status, status, "{}", answer.body);
    serde_json::from_str::<ErrorBody>(&answer.body)
        .unwrap()
        .error
}

/// Every file under the home, by path, outside the database and the repo's
/// own `.git`: what a memory write may change is checked against this.
fn files(home: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if name.starts_with("state.db") || name == "memory/.git" {
                continue;
            }
            if path.is_dir() {
                walk(&path, base, out);
            } else {
                out.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(home, home, &mut out);
    out
}

/// The paths whose bytes differ between two snapshots.
fn changed(before: &BTreeMap<String, Vec<u8>>, after: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut names: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|name| before.get(*name) != after.get(*name))
        .cloned()
        .collect();
    names.dedup();
    names
}

/// A repo with someone else's work in it: a committed file, a staged
/// change and an untracked file, none of which a memory write may touch.
fn others(home: &Home) {
    home.repo().lock().unwrap();
    let root = home.repo().root().to_owned();
    std::fs::write(root.join("notes.md"), "kept by hand\n").unwrap();
    git(&root, &["add", "notes.md"]);
    git(
        &root,
        &[
            "-c",
            "user.name=Owner",
            "-c",
            "user.email=owner@example.com",
            "commit",
            "--quiet",
            "-m",
            "notes",
        ],
    );
    std::fs::write(
        root.join("notes.md"),
        "kept by hand\nstaged, not committed\n",
    )
    .unwrap();
    git(&root, &["add", "notes.md"]);
    std::fs::write(root.join("scratch.txt"), "untracked\n").unwrap();
}

/// What a write left behind: only `path` changed on disk, the last commit
/// changed only `path`, and the staged change to `notes.md` is still staged.
fn only(home: &Home, before: &BTreeMap<String, Vec<u8>>, path: &str) {
    let after = files(home.path());
    assert_eq!(changed(before, &after), [format!("memory/{path}")]);
    assert_eq!(
        home.git(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
            .trim(),
        path
    );
    assert_eq!(
        home.git(&["diff", "--cached", "--name-only"]).trim(),
        "notes.md"
    );
    assert_eq!(home.git(&["status", "--porcelain", "--", path]), "");
}

/// An agent's proposal to add `text` to `project`'s notes, made against the
/// file at `HEAD`, as the curator records one.
fn propose(home: &Home, project: &str, text: &str) -> String {
    let path = document_path(project);
    let file = home.repo().state(&path, &ScrubList::default()).unwrap();
    let mut store = home.fleet.open();
    let (source, revision): (String, String) = store
        .conn()
        .query_row(
            "SELECT s.id, r.id FROM memory_sources s JOIN memory_revisions r ON r.source_id = s.id
              WHERE s.project_id = ?1 AND r.blob_sha = ?2",
            [project, file.head_blob.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let diff = doc::unified_diff(&path, &file.text, &doc::add(&file.text, NOTE_SECTION, text));
    store
        .write(&Ctx::new(Actor::Daemon), "test.propose", None, |tx| {
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO proposals (id, project_id, boundary, target_source_id, op, base_revision_id, base_blob,
                                        diff, diff_sha256, memory_fields, rationale, expected_effect, created_at, created_by)
                 VALUES (?1, ?2, 'personal', ?3, 'add', ?4, ?5, ?6, ?7, ?8, 'Seen twice this week', 'fewer retries', ?9,
                         'session:curator')",
                rusqlite::params![
                    id,
                    project,
                    source,
                    revision,
                    file.head_blob,
                    diff,
                    doc::sha256_hex(diff.as_bytes()),
                    json!({"section": NOTE_SECTION, "class": "note"}),
                    tx.now(),
                ],
            )?;
            tx.event(NewEvent::new("proposal", &id, "proposal.created").rev(1))?;
            Ok(id)
        })
        .unwrap()
        .value
}

/// Bundles `proposals` as one change set with a passing holdout run, so
/// the gate lets their Keep through.
fn measured(home: &Home, name: &str, proposals: &[&str]) {
    let members: Vec<String> = proposals.iter().map(|p| (*p).to_owned()).collect();
    let mut store = home.fleet.open();
    store
        .change_set_create(&owner(), name, &members, 10)
        .unwrap();
    let passing = EvalResult {
        cases_total: 4,
        cases_passed: 4,
        ..EvalResult::default()
    };
    store.eval_run_record(&owner(), name, passing).unwrap();
}

fn text(home: &Home, path: &str) -> String {
    std::fs::read_to_string(home.repo().root().join(path)).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_commits_its_one_file_and_nothing_else() {
    let home = home();
    others(&home);
    let before = files(home.path());
    let first = json!({"project": "alpha", "op": "add", "section": RULE_SECTION,
                       "text": "Run the slow suite before landing",
                       "base_blob": home.blob("alpha").await});
    let done = ok(&home.edit("k-1", first.clone()).await);
    assert!(!done.replayed);
    only(&home, &before, ALPHA);
    assert_eq!(
        text(&home, ALPHA),
        "## How we work here\n\n- Run the slow suite before landing\n"
    );
    let message = home.git(&["log", "-1", "--format=%an <%ae>%n%B"]);
    assert!(
        message.starts_with(
            "Tiller <tiller@localhost>\nmemory(alpha): add a line to How we work here\n"
        ),
        "{message}"
    );
    assert!(message.contains(&format!(
        "Tiller-Proposal: {}",
        done.data["proposal_id"].as_str().unwrap()
    )));
    assert_eq!(
        done.data["blob"],
        home.git(&["rev-parse", &format!("HEAD:{ALPHA}")]).trim()
    );

    // The same key with the same request again changes nothing.
    let head = home.git(&["rev-parse", "HEAD"]);
    let again = ok(&home.edit("k-1", first).await);
    assert!(again.replayed);
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);

    // A rewrite and a removal touch the same one file.
    let before = files(home.path());
    let blob = home.blob("alpha").await;
    let rewrite = json!({"project": "alpha", "op": "rewrite", "section": RULE_SECTION, "index": 0,
                         "text": "Run the slow suite before every landing", "base_blob": blob});
    ok(&home.edit("k-2", rewrite).await);
    only(&home, &before, ALPHA);
    let before = files(home.path());
    let remove = json!({"project": "alpha", "op": "remove", "section": RULE_SECTION, "index": 0});
    ok(&home.edit("k-3", remove).await);
    only(&home, &before, ALPHA);
    assert_eq!(text(&home, ALPHA), "## How we work here\n");

    // The fleet document is its own file.
    let before = files(home.path());
    home.add("k-4", "fleet", NOTE_SECTION, "Prefer small commits")
        .await;
    only(&home, &before, "fleet.md");
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_and_discard_commit_only_their_document() {
    let home = home();
    home.add(
        "k-1",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    others(&home);
    let keep = propose(&home, "alpha", "The flaky test is in the parser");
    let discard = propose(&home, "alpha", "Nothing to see");
    let view = home.view("alpha").await;
    let notes = &view["doc"]["sections"][1];
    assert_eq!(notes["title"], NOTE_SECTION);
    assert_eq!(notes["suggestions"].as_array().unwrap().len(), 2);
    assert_eq!(view["docs"][0]["project_id"], "fleet");
    let gate = &notes["suggestions"][0]["gate"];
    assert_eq!(gate["ok"], false);
    assert_eq!(gate["reason"], "the proposal is in no change set");

    // Keep waits for the holdout gate, and a refusal commits nothing.
    let head = home.git(&["rev-parse", "HEAD"]);
    let blob = home.blob("alpha").await;
    let answer = home
        .post(
            "/api/v1/memory/proposals/decide",
            json!({"idempotency_key": "k-gate", "decisions": [
                {"proposal": keep, "decision": "keep", "base_blob": blob},
            ]}),
        )
        .await;
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    let hint = format!("the proposal is in no change set; tiller learn eval --proposal {keep}");
    assert!(error.message.contains(&hint), "{}", error.message);
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);
    measured(&home, "parser notes", &[&keep]);
    let view = home.view("alpha").await;
    assert_eq!(
        view["doc"]["sections"][1]["suggestions"][0]["gate"]["ok"],
        true
    );

    let before = files(home.path());
    let blob = home.blob("alpha").await;
    let answer = home
        .post(
            "/api/v1/memory/proposals/decide",
            json!({"idempotency_key": "k-2", "decisions": [
                {"proposal": discard, "decision": "discard", "base_blob": blob},
                {"proposal": keep, "decision": "keep", "base_blob": blob},
            ]}),
        )
        .await;
    let done = ok(&answer);
    assert_eq!(done.data["decisions"][0]["approval_id"], Value::Null);
    only(&home, &before, ALPHA);
    assert!(text(&home, ALPHA).contains("- The flaky test is in the parser\n"));
    assert!(!text(&home, ALPHA).contains("Nothing to see"));
    let view = home.view("alpha").await;
    let kinds: Vec<&str> = view["doc"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds[..2], ["kept", "discard"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_keeps_proposals_made_against_the_same_text() {
    let home = home();
    home.add(
        "k-1",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    let first = propose(&home, "alpha", "The parser test flakes");
    let second = propose(&home, "alpha", "The cache needs a warm start");
    measured(&home, "test notes", &[&first, &second]);
    let blob = home.blob("alpha").await;
    let answer = home
        .post(
            "/api/v1/memory/proposals/decide",
            json!({"idempotency_key": "k-2", "decisions": [
                {"proposal": first, "decision": "keep", "base_blob": blob},
                {"proposal": second, "decision": "keep", "base_blob": blob},
            ]}),
        )
        .await;
    ok(&answer);
    let text = text(&home, ALPHA);
    assert!(
        text.contains("- The parser test flakes\n")
            && text.contains("- The cache needs a warm start\n"),
        "{text}"
    );
    assert_eq!(home.git(&["rev-list", "--count", "HEAD"]).trim(), "3");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_keep_after_another_set_moved_the_text_is_stale() {
    let home = home();
    home.add(
        "k-0",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    let first = propose(&home, "alpha", "The parser test flakes");
    let second = propose(&home, "alpha", "The cache needs a warm start");
    measured(&home, "parser notes", &[&first]);
    measured(&home, "cache notes", &[&second]);
    let blob = home.blob("alpha").await;
    let answer = home
        .post(
            "/api/v1/memory/proposals/decide",
            json!({"idempotency_key": "k-1", "decisions": [
                {"proposal": first, "decision": "keep", "base_blob": blob},
                {"proposal": second, "decision": "keep", "base_blob": blob},
            ]}),
        )
        .await;
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error.message.contains(
            "the text moved; re-propose it on the current text and run the holdout again"
        ),
        "{}",
        error.message
    );
    let text = text(&home, ALPHA);
    assert!(
        text.contains("- The parser test flakes\n") && !text.contains("warm start"),
        "{text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_over_two_documents_with_the_same_text_keeps_each() {
    let home = home();
    // The same first line makes both documents the same blob.
    home.add(
        "k-1",
        "fleet",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    home.add(
        "k-2",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    let blob = home.blob("alpha").await;
    assert_eq!(home.blob("fleet").await, blob);
    let shared = propose(&home, "fleet", "Quota resets on Monday");
    let local = propose(&home, "alpha", "The parser test flakes");
    measured(&home, "quota notes", &[&shared, &local]);
    let answer = home
        .post(
            "/api/v1/memory/proposals/decide",
            json!({"idempotency_key": "k-3", "decisions": [
                {"proposal": shared, "decision": "keep", "base_blob": blob},
                {"proposal": local, "decision": "keep", "base_blob": blob},
            ]}),
        )
        .await;
    ok(&answer);
    let fleet = text(&home, "fleet.md");
    let alpha = text(&home, ALPHA);
    assert!(
        fleet.contains("- Quota resets on Monday\n") && !fleet.contains("parser"),
        "{fleet}"
    );
    assert!(
        alpha.contains("- The parser test flakes\n") && !alpha.contains("Quota"),
        "{alpha}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_document_list_counts_the_lines_at_head() {
    let home = home();
    home.add(
        "k-1",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    home.add("k-2", "alpha", NOTE_SECTION, "The parser test flakes")
        .await;
    home.add("k-3", "fleet", RULE_SECTION, "Ask before a sizeable merge")
        .await;
    let view = home.view("fleet").await;
    let lines: BTreeMap<&str, u64> = view["docs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["project_id"].as_str().unwrap(),
                d["lines"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(lines.get("fleet"), Some(&1));
    assert_eq!(lines.get("alpha"), Some(&2));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_cannot_be_written_puts_head_back() {
    use std::os::unix::fs::PermissionsExt;

    let home = home();
    home.add(
        "k-1",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    let dir = home.repo().root().join("projects");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // A user that writes through the mode (root) cannot see this failure.
    let probe = dir.join(".probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let head = home.git(&["rev-parse", "HEAD"]);
    let before = text(&home, ALPHA);
    let answer = home
        .edit(
            "k-2",
            json!({"project": "alpha", "op": "add", "section": NOTE_SECTION, "text": "Not written"}),
        )
        .await;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(text(&home, ALPHA), before);
    assert_eq!(home.git(&["status", "--porcelain", "--", ALPHA]), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_reverts_its_commit_and_keeps_the_rest() {
    let home = home();
    let first = home
        .add(
            "k-1",
            "alpha",
            RULE_SECTION,
            "Run the slow suite before landing",
        )
        .await;
    home.add("k-2", "alpha", NOTE_SECTION, "The parser test flakes")
        .await;
    others(&home);
    let before = files(home.path());
    let first_id = first.data["proposal_id"].as_str().unwrap().to_owned();
    let blob = home.blob("alpha").await;
    let answer = home
        .post(
            &format!("/api/v1/memory/changes/{first_id}/undo"),
            json!({"idempotency_key": "k-3", "base_blob": blob}),
        )
        .await;
    ok(&answer);
    only(&home, &before, ALPHA);
    assert_eq!(
        text(&home, ALPHA),
        "## How we work here\n\n## Watch out for\n\n- The parser test flakes\n"
    );
    // History keeps the line: the commit that added it is still there.
    let log = home.git(&["log", "--format=%s"]);
    assert_eq!(log.lines().count(), 4, "{log}");

    // Undoing it again is refused.
    let blob = home.blob("alpha").await;
    let again = home
        .post(
            &format!("/api/v1/memory/changes/{first_id}/undo"),
            json!({"idempotency_key": "k-4", "base_blob": blob}),
        )
        .await;
    refusal(&again, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test(flavor = "multi_thread")]
async fn undo_that_conflicts_with_a_later_edit_is_refused() {
    let home = home();
    let first = home
        .add(
            "k-1",
            "alpha",
            RULE_SECTION,
            "Run the slow suite before landing",
        )
        .await;
    let rewrite = json!({"project": "alpha", "op": "rewrite", "section": RULE_SECTION, "index": 0,
                         "text": "Run the slow suite before every landing"});
    ok(&home.edit("k-2", rewrite).await);
    let head = home.git(&["rev-parse", "HEAD"]);
    let blob = home.blob("alpha").await;
    let id = first.data["proposal_id"].as_str().unwrap();
    let answer = home
        .post(
            &format!("/api/v1/memory/changes/{id}/undo"),
            json!({"idempotency_key": "k-3", "base_blob": blob}),
        )
        .await;
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error.message.contains("conflicts with a later edit"),
        "{}",
        error.message
    );
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);
}

#[tokio::test(flavor = "multi_thread")]
async fn moved_base_goes_stale() {
    let home = home();
    home.add(
        "k-1",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    let shown = home.blob("alpha").await;
    home.add("k-2", "alpha", RULE_SECTION, "Keep commits small")
        .await;
    let head = home.git(&["rev-parse", "HEAD"]);
    let answer = home
        .edit("k-3", json!({"project": "alpha", "op": "add", "section": NOTE_SECTION, "text": "Late", "base_blob": shown}))
        .await;
    refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);

    // An edit made outside Tiller and not committed blocks changes too.
    std::fs::write(
        home.repo().root().join(ALPHA),
        "## How we work here\n\n- hand edit\n",
    )
    .unwrap();
    let answer = home
        .edit(
            "k-4",
            json!({"project": "alpha", "op": "add", "section": NOTE_SECTION, "text": "Late"}),
        )
        .await;
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error.message.contains("uncommitted edits"),
        "{}",
        error.message
    );
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(text(&home, ALPHA), "## How we work here\n\n- hand edit\n");
    assert_eq!(home.view("alpha").await["doc"]["dirty"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn flagged_commit_is_undone() {
    let home = home();
    others(&home);
    home.add(
        "k-1",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    let before = files(home.path());
    let head = home.git(&["rev-parse", "HEAD"]);
    let answer = home
        .edit("k-2", json!({"project": "alpha", "op": "add", "section": NOTE_SECTION, "text": concat!("Ask sam@", "mail.test.io first")}))
        .await;
    let error = refusal(&answer, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        error
            .message
            .contains("the scrub flagged it: email address on line 7"),
        "{}",
        error.message
    );
    assert!(!error.message.contains("sam@"), "{}", error.message);
    // HEAD, the working tree and the index are as they were.
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(changed(&before, &files(home.path())), Vec::<String>::new());
    assert_eq!(
        home.git(&["diff", "--cached", "--name-only"]).trim(),
        "notes.md"
    );
    // The approval and its proposal are failed, not left open.
    let store = home.fleet.open();
    let states: (String, String) = store
        .conn()
        .query_row(
            "SELECT a.state, p.state FROM proposals p JOIN approvals a ON a.id = p.approval_id
              ORDER BY p.created_at DESC, p.id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(states, ("failed".to_owned(), "failed".to_owned()));
    drop(store);
    // A retry with the same key is refused again, never answered as landed.
    let again = home
        .edit("k-2", json!({"project": "alpha", "op": "add", "section": NOTE_SECTION, "text": concat!("Ask sam@", "mail.test.io first")}))
        .await;
    refusal(&again, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(home.git(&["rev-parse", "HEAD"]), head);
}

#[test]
fn a_diff_naming_another_path_is_refused() {
    let fleet = Fleet::new();
    let repo = Repo::at(fleet.dir.path());
    let lock = repo.lock().unwrap();
    let list = ScrubList::default();
    let ours = doc::unified_diff(ALPHA, "", "## How we work here\n\n- one\n");
    repo.commit(&lock, ALPHA, &ours, "memory(alpha): add\n", &list)
        .unwrap();
    let head = git(repo.root(), &["rev-parse", "HEAD"]);
    let before = files(fleet.dir.path());

    let theirs = doc::unified_diff("fleet.md", "", "## How we work here\n\n- two\n");
    let both = format!(
        "{}{theirs}",
        doc::unified_diff(
            ALPHA,
            "## How we work here\n\n- one\n",
            "## How we work here\n\n- three\n"
        )
    );
    let escape = theirs.replace("fleet.md", "../outside.md");
    for diff in [theirs.as_str(), both.as_str(), escape.as_str()] {
        let error = repo
            .commit(&lock, ALPHA, diff, "memory(alpha): edit\n", &list)
            .unwrap_err();
        assert!(matches!(error, Error::Refused { .. }), "{error}");
    }
    assert!(
        repo.commit(&lock, "../outside.md", &escape, "x\n", &list)
            .is_err()
    );
    assert_eq!(git(repo.root(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        changed(&before, &files(fleet.dir.path())),
        Vec::<String>::new()
    );
    assert!(!fleet.dir.path().join("outside.md").exists());
}

#[test]
fn concurrent_writers_do_not_bundle() {
    let fleet = Fleet::new();
    let home = fleet.dir.path().to_owned();
    Repo::at(&home).lock().unwrap();
    let root = Repo::at(&home).root().to_owned();
    std::fs::write(root.join("notes.md"), "staged by hand\n").unwrap();
    git(&root, &["add", "notes.md"]);
    let writers: Vec<_> = [ALPHA, "fleet.md"]
        .into_iter()
        .map(|path| {
            let home = home.clone();
            std::thread::spawn(move || {
                let repo = Repo::at(&home);
                let list = ScrubList::default();
                for n in 0..5 {
                    let lock = repo.lock().unwrap();
                    let now = repo.state(path, &list).unwrap().text;
                    let next = doc::add(&now, RULE_SECTION, &format!("line {n}"));
                    let diff = doc::unified_diff(path, &now, &next);
                    repo.commit(&lock, path, &diff, &format!("{path} {n}\n"), &list)
                        .unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let log = git(&root, &["log", "--format=%H"]);
    assert_eq!(log.lines().count(), 10);
    for commit in log.lines() {
        let names = git(
            &root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--root",
                commit,
            ],
        );
        assert_eq!(names.lines().count(), 1, "{commit}: {names}");
    }
    assert_eq!(
        git(&root, &["ls-tree", "--name-only", "-r", "HEAD"]),
        "fleet.md\nprojects/alpha.md\n"
    );
    assert_eq!(
        git(&root, &["diff", "--cached", "--name-only"]).trim(),
        "notes.md"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_changes_need_the_owner_and_stay_on_this_host() {
    let home = home();
    let blob = home.blob("alpha").await;
    let body = json!({"idempotency_key": "k-1", "project": "alpha", "op": "add", "section": NOTE_SECTION,
                      "text": "x", "base_blob": blob});
    let answer = send(
        &home.app,
        mutation("POST", "/api/v1/memory/edits", body.clone(), &[]),
    )
    .await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN, "{}", answer.body);

    let peer = Fleet::new();
    home.fleet
        .open()
        .peer_pair(
            &owner(),
            NewPairing {
                label: "peer".into(),
                herdr_machine: None,
                scope: PeerScope::Act,
                peer_host_id: peer.host.clone(),
            },
        )
        .unwrap();
    let mut forwarded = body;
    forwarded["host"] = json!("peer");
    let error = refusal(
        &home.post("/api/v1/memory/edits", forwarded).await,
        StatusCode::BAD_GATEWAY,
    );
    assert_eq!(
        (error.code.as_str(), error.reason.as_deref()),
        ("peer_refused", Some("not_allowlisted"))
    );
    assert!(!home.repo().root().join(ALPHA).exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revision_names_its_approval_and_text() {
    let home = home();
    home.add(
        "k-1",
        "alpha",
        RULE_SECTION,
        "Run the slow suite before landing",
    )
    .await;
    let view = home.view("alpha").await;
    let line = &view["doc"]["sections"][0]["lines"][0];
    let revision = line["provenance"]["revision_id"].as_str().unwrap();
    let answer = get(&home.app, &format!("/api/v1/memory/revisions/{revision}")).await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let detail: Value = serde_json::from_str(&answer.body).unwrap();
    assert_eq!(detail["review"], "approved");
    assert_eq!(detail["approval"]["state"], "succeeded");
    assert_eq!(
        detail["text"],
        "## How we work here\n\n- Run the slow suite before landing\n"
    );
}

/// The suggestion `proposal` in the view of `project`.
fn suggestion(view: &Value, proposal: &str) -> Value {
    view["doc"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|section| {
            let lines = section["lines"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|line| line.get("suggestion").filter(|s| !s.is_null()).cloned());
            section["suggestions"]
                .as_array()
                .unwrap()
                .clone()
                .into_iter()
                .chain(lines.collect::<Vec<_>>())
        })
        .find(|s| s["proposal_id"] == proposal)
        .unwrap_or(Value::Null)
}

/// An invented evals checkout: one holdout case at two runs with one judged
/// grader, and a fake `run.sh` running `body`. Nothing in it calls a model.
fn evals_checkout(body: &str) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let case = dir.path().join("evals/cases/holdout/invented_case");
    std::fs::create_dir_all(case.join("graders")).unwrap();
    std::fs::write(
        case.join("prompt.md"),
        "---\nrole: builder\nruns: 2\n---\nDo it.\n",
    )
    .unwrap();
    std::fs::write(
        case.join("graders/judge.md"),
        "---\ntype: llm\n---\nDone?\n",
    )
    .unwrap();
    let runner = dir.path().join("evals/run.sh");
    std::fs::write(&runner, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

const PASSING_RUN: &str = r#"mkdir -p "$6/sealed"
printf '{"aggregates":{"casesTotal":1,"casesPassed":1,"overallScore":1.0}}' > "$6/sealed/result.json""#;

fn point_at(home: &Home, checkout: &Path) {
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[learning]\nevals_dir = \"{}\"\neval_percent_per_run = 0.5\n",
            checkout.display()
        ),
    )
    .unwrap();
}

fn weekly(home: &Home, left: f64) {
    let mut store = home.fleet.open();
    store
        .cache_quota(
            &Ctx::new(Actor::Daemon),
            vec![tiller_core::state::caches::QuotaReading {
                label: "claude:seven_day".into(),
                percent_remaining: Some(left),
                resets_at: Some(tiller_core::Timestamp::now().plus_minutes(3 * 24 * 60)),
                pace: None,
                reserve: None,
                burn_multiple: None,
                elapsed_percent: None,
                projected_exhaustion_at: None,
                title: Some("Weekly".into()),
                kind: Some("weekly".into()),
                fetch_error: None,
            }],
        )
        .unwrap();
}

async fn plan(home: &Home, proposal: &str) -> Value {
    let answer = get(
        &home.app,
        &format!("/api/v1/memory/proposals/{proposal}/eval"),
    )
    .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    serde_json::from_str(&answer.body).unwrap()
}

/// The view once no eval is running, or after ten seconds.
async fn settled(home: &Home, project: &str) -> Value {
    for _ in 0..100 {
        let view = home.view(project).await;
        let running = view["evals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["finished_at"].is_null());
        if !running {
            return view;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the eval is still running");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_board_runs_the_holdout_eval_that_unlocks_keep() {
    let home = home();
    home.add("k-1", "alpha", RULE_SECTION, "Run the slow suite")
        .await;
    let proposal = propose(&home, "alpha", "The parser test flakes");
    let view = home.view("alpha").await;
    assert_eq!(suggestion(&view, &proposal)["eval_clears"], true);
    assert_eq!(view["evals"], json!([]));

    // With no evals checkout set, the plan says why it will not start.
    let first = plan(&home, &proposal).await;
    assert!(
        first["refusal"]
            .as_str()
            .unwrap()
            .contains("learning.evals_dir is not set"),
        "{first}"
    );
    let start = |key: &'static str| {
        let proposal = proposal.clone();
        let home = &home;
        async move {
            home.post(
                &format!("/api/v1/memory/proposals/{proposal}/eval"),
                json!({"idempotency_key": key}),
            )
            .await
        }
    };
    refusal(&start("e-1").await, StatusCode::UNPROCESSABLE_ENTITY);

    let checkout = evals_checkout(PASSING_RUN);
    point_at(&home, checkout.path());
    // The quota floor fails closed with no reading, and refuses a run that
    // would leave the week at or under it.
    let unread = plan(&home, &proposal).await;
    assert!(
        unread["refusal"]
            .as_str()
            .unwrap()
            .contains("no weekly quota reading"),
        "{unread}"
    );
    weekly(&home, 6.0);
    let low = plan(&home, &proposal).await;
    assert_eq!(low["quota"]["floor"], 5.0);
    assert_eq!(low["quota"]["left_after"], 4.0);
    assert!(
        low["refusal"]
            .as_str()
            .unwrap()
            .contains("at or under the 5 percent floor"),
        "{low}"
    );
    let error = refusal(&start("e-2").await, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(error.message.contains("floor"), "{}", error.message);

    weekly(&home, 60.0);
    let ready = plan(&home, &proposal).await;
    assert_eq!(ready["refusal"], Value::Null, "{ready}");
    assert_eq!(
        (
            &ready["cases"],
            &ready["case_runs"],
            &ready["judge_runs"],
            &ready["model_runs"]
        ),
        (&json!(1), &json!(2), &json!(2), &json!(4))
    );
    assert_eq!(ready["percent"], 2.0);
    assert_eq!(ready["quota"]["left_after"], 58.0);
    assert_eq!(ready["change_set"], ids::short(&proposal));

    let started = ok(&start("e-3").await);
    assert_eq!(started.data["change_set"], ids::short(&proposal));
    let view = settled(&home, "alpha").await;
    assert_eq!(view["evals"], json!([]), "a recorded run leaves no job");
    let shown = suggestion(&view, &proposal);
    assert_eq!(shown["holdout"]["passed"], true);
    assert_eq!(shown["holdout"]["current"], true);
    assert_eq!(shown["gate"]["ok"], true, "{shown}");
    assert_eq!(shown["eval_clears"], false);

    // The run unlocked Keep.
    let blob = home.blob("alpha").await;
    let kept = home
        .post(
            "/api/v1/memory/proposals/decide",
            json!({"idempotency_key": "d-1", "decisions": [
                {"proposal": proposal, "decision": "keep", "base_blob": blob}]}),
        )
        .await;
    ok(&kept);
    assert!(text(&home, ALPHA).contains("The parser test flakes"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_holdout_eval_that_records_nothing_keeps_its_reason() {
    let home = home();
    home.add("k-1", "alpha", RULE_SECTION, "Run the slow suite")
        .await;
    let proposal = propose(&home, "alpha", "The parser test flakes");
    let checkout = evals_checkout("exit 3");
    point_at(&home, checkout.path());
    weekly(&home, 60.0);
    let started = home
        .post(
            &format!("/api/v1/memory/proposals/{proposal}/eval"),
            json!({"idempotency_key": "e-1"}),
        )
        .await;
    ok(&started);
    let view = settled(&home, "alpha").await;
    let job = &view["evals"][0];
    assert_eq!(job["proposal_id"], proposal.as_str());
    assert!(
        job["error"]
            .as_str()
            .unwrap()
            .contains("exited with code 3"),
        "{job}"
    );
    let shown = suggestion(&view, &proposal);
    assert_eq!(shown["gate"]["ok"], false);
    assert_eq!(shown["eval_clears"], true, "it can run again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_moved_proposal_is_reproposed_on_the_current_text() {
    let home = home();
    home.add("k-1", "alpha", RULE_SECTION, "Run the slow suite")
        .await;
    let proposal = propose(&home, "alpha", "The parser test flakes");
    home.add("k-2", "alpha", RULE_SECTION, "Keep commits small")
        .await;
    let view = home.view("alpha").await;
    let moved = suggestion(&view, &proposal);
    assert_eq!(moved["moved"], true);
    assert_eq!(moved["eval_clears"], false);
    assert!(
        moved["gate"]["hint"]
            .as_str()
            .unwrap()
            .contains("Re-propose on current text"),
        "{moved}"
    );
    // The eval will not start on a moved proposal.
    let checkout = evals_checkout(PASSING_RUN);
    point_at(&home, checkout.path());
    weekly(&home, 60.0);
    let refused = plan(&home, &proposal).await;
    assert!(
        refused["refusal"].as_str().unwrap().contains("re-propose"),
        "{refused}"
    );

    let blob = home.blob("alpha").await;
    let path = format!("/api/v1/memory/proposals/{proposal}/repropose");
    let answer = home
        .post(&path, json!({"idempotency_key": "r-1", "base_blob": blob}))
        .await;
    let again = ok(&answer);
    let new = again.data["proposal_id"].as_str().unwrap().to_owned();
    assert_ne!(new, proposal);
    let view = home.view("alpha").await;
    assert_eq!(suggestion(&view, &proposal), Value::Null);
    let fresh = suggestion(&view, &new);
    assert_eq!(fresh["moved"], false);
    assert_eq!(fresh["eval_clears"], true);
    assert_eq!(fresh["text"], "The parser test flakes");
    assert!(
        view["doc"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|change| change["proposal_id"] != proposal.as_str()),
        "the closed proposal is not in the history"
    );
    // A second re-proposal of the closed one is refused.
    let twice = home
        .post(&path, json!({"idempotency_key": "r-2", "base_blob": blob}))
        .await;
    refusal(&twice, StatusCode::UNPROCESSABLE_ENTITY);
}
