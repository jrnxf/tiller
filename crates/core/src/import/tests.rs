//! T2.4: the importer against a small synthetic `tiller-import/v1` bundle.

use std::path::Path;

use serde_json::{Value, json};

use super::bundle::sha256_hex;
use super::*;
use crate::domain::*;
use crate::state::projects::Init;
use crate::state::tests::{owner, snapshot};
use crate::store::{Actor, Ctx, Store};
use crate::time::Timestamp;

fn import_ctx() -> Ctx {
    Ctx::new(Actor::Import)
}

fn store() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .init(
            &owner(),
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
}

fn card(id: &str, project: &str, kind: &str, column: &str) -> Value {
    json!({
        "id": id, "project": project, "title": format!("Card {id}"), "column": column, "kind": kind,
        "priority": null, "since": "2026-10-01", "hold": null, "pr": null, "report": null,
        "runs": [], "depends_on": [], "category": null, "size": null, "done": null,
        "body": [format!("Brief for {id}."), "Second line."],
        "source": {"file": "data/backlog.md", "line": 1},
    })
}

fn done(mut card: Value, outcome: &str, at: &str) -> Value {
    card["done"] = json!({"outcome": outcome, "at": at});
    card
}

fn memory(id: &str, scope: &str, class: &str, text: &str) -> Value {
    json!({
        "id": id, "text": text, "scope": scope, "section": "Learnings", "class": class,
        "tier_marker": null, "secret_ref": false, "provenance": "legacy-import", "rationale": null,
        "generalized": null, "reason": null, "source": {"file": "data/learnings.md", "line": 3},
    })
}

/// The records the fixture bundle holds.
struct Records {
    projects: Vec<Value>,
    cards: Vec<Value>,
    memory: Vec<Value>,
    inflight: Vec<Value>,
}

fn records() -> Records {
    let mut a5 = card("a5", "alpha", "captain", "needs");
    a5["hold"] =
        json!({"kind": "captain", "until": null, "reason": "Which name should the app use?"});
    let mut a6 = card("a6", "alpha", "ship", "queued");
    a6["hold"] =
        json!({"kind": "captain", "until": "2099-01-05", "reason": "Revisit after the launch"});
    let mut a7 = card("a7", "alpha", "ship", "queued");
    a7["hold"] = json!({"kind": "blocked", "until": null, "reason": "after a3 and a1"});
    a7["depends_on"] = json!(["a3", "a1"]);
    let mut a4 = card("a4", "alpha", "ship", "landing");
    a4["pr"] = json!("https://github.com/example/alpha/pull/4");
    let mut a1 = done(card("a1", "alpha", "ship", "done"), "merged", "2026-10-03");
    a1["pr"] = json!("https://github.com/example/alpha/pull/1");
    a1["runs"] =
        json!([{"started": null, "ended": null, "model": "opus", "outcome": "landed", "pr": null}]);
    let mut a2 = done(
        card("a2", "alpha", "scout", "done"),
        "reported",
        "2026-10-02",
    );
    a2["since"] = json!("2026-09-01");
    let mut b1 = done(
        card("b1", "Beta-Site", "other", "done"),
        "dropped",
        "2026-10-04",
    );
    b1["since"] = json!(null);
    let mut m1 = memory("mem-1", "global", "rule", "Workers stay on Claude.");
    m1["rationale"] = json!("The owner said so.");
    let mut m4 = memory(
        "mem-4",
        "global",
        "stale",
        "The old pipeline needs a restart.",
    );
    m4["reason"] = json!("The pipeline was replaced.");
    Records {
        projects: vec![
            json!({"id": "alpha", "repo": "example/alpha", "remote": "https://github.com/example/alpha.git",
                   "default_branch": "main", "delivery": "direct-pr", "capacity": 3, "added": "2026-09-01",
                   "description": "The alpha app", "source": {"file": "data/projects.md", "line": 5}}),
            json!({"id": "Beta-Site", "repo": "example/beta", "remote": null, "default_branch": "main",
                   "delivery": "no-mistakes", "capacity": null, "added": null}),
        ],
        cards: vec![
            a1,
            a2,
            card("a3", "alpha", "ship", "working"),
            a4,
            a5,
            a6,
            a7,
            card("n1", "alpha", "ship", "needs"),
            b1,
            {
                let mut f1 = done(
                    card("f1", "fleet", "scout", "done"),
                    "answered",
                    "2026-10-05",
                );
                f1["pr"] = json!("https://github.com/example/alpha/pull/9");
                f1
            },
            done(card("g1", "gone", "ship", "done"), "merged", "2026-10-06"),
        ],
        memory: vec![
            m1,
            memory(
                "mem-2",
                "project:alpha",
                "note",
                "alpha builds slowly.\n\nUse the cache.",
            ),
            memory("mem-3", "global", "requirement", "Merge before teardown."),
            m4,
            memory(
                "mem-5",
                "project:Beta-Site",
                "rule",
                "Beta deploys on Fridays only.",
            ),
        ],
        inflight: vec![json!({"id": "a3", "kind": "ship", "recommendation": "drain"})],
    }
}

fn jsonl(rows: &[Value]) -> String {
    rows.iter().map(|row| format!("{row}\n")).collect()
}

/// Writes the bundle, manifest last, as the exporter does.
fn write_bundle(dir: &Path, records: &Records, mode: &str) {
    std::fs::create_dir_all(dir.join("inflight")).unwrap();
    let mut files = serde_json::Map::new();
    let mut put = |name: &str, text: String| {
        std::fs::write(dir.join(name), &text).unwrap();
        files.insert(name.to_owned(), json!(sha256_hex(text.as_bytes())));
    };
    put(
        "projects.json",
        Value::Array(records.projects.clone()).to_string(),
    );
    put("cards.jsonl", jsonl(&records.cards));
    put("memory.jsonl", jsonl(&records.memory));
    for item in &records.inflight {
        put(
            &format!("inflight/{}.json", item["id"].as_str().unwrap()),
            item.to_string(),
        );
    }
    put("README.md", "A synthetic bundle.\n".into());
    let manifest = json!({
        "schema": "tiller-import/v1", "mode": mode, "host_label": "host-a",
        "generated_at": "2026-11-01T21:05:00+00:00", "firstmate_head": "0000000",
        "counts": {"projects": records.projects.len(), "cards": records.cards.len(),
                   "inflight": records.inflight.len(), "memory": records.memory.len(),
                   "regressions": 0, "capabilities": 0, "open_questions": 0},
        "files": files,
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

fn bundle(dir: &Path) -> Bundle {
    write_bundle(dir, &records(), "dry");
    Bundle::read(dir).unwrap()
}

fn count(store: &Store, sql: &str) -> i64 {
    store.conn().query_row(sql, [], |row| row.get(0)).unwrap()
}

fn column_of(store: &Store, legacy_id: &str) -> String {
    store
        .conn()
        .query_row(
            "SELECT v.\"column\" FROM card_column v JOIN cards c ON c.id = v.id WHERE c.legacy_id = ?1",
            [legacy_id],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn dry_run_plans_every_record_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let store = store();
    let before = snapshot(store.conn(), &[]);

    let plan = store.import_plan(&bundle, Timestamp::now()).unwrap();

    assert_eq!(snapshot(store.conn(), &[]), before, "a dry run wrote");
    assert!(plan.ok(), "{:?}", plan.checks);
    assert!(!plan.already_imported);
    assert_eq!(plan.cards.len(), 11);
    assert_eq!(plan.memory.len(), 5);
    let slugs: Vec<(&str, &str, bool)> = plan
        .projects
        .iter()
        .map(|p| (p.legacy_id.as_str(), p.id.as_str(), p.retire))
        .collect();
    assert_eq!(
        slugs,
        [
            ("alpha", "alpha", false),
            ("Beta-Site", "betasite", false),
            ("fleet", "fleet", false),
            ("gone", "gone", true)
        ]
    );
    // Display ids follow `since`, so the September scout is alpha-1; the
    // fleet continues after the host health card.
    let display = |legacy: &str| {
        plan.cards
            .iter()
            .find(|c| c.legacy_id == legacy)
            .map(|c| c.display_id.clone())
            .unwrap()
    };
    assert_eq!(display("a2"), "alpha-1");
    assert_eq!(display("a1"), "alpha-2");
    assert_eq!(display("f1"), "fleet-2");
    assert_eq!(display("b1"), "betasite-1");
    assert_eq!(
        plan.inflight[0].display_id.as_deref(),
        Some(display("a3").as_str())
    );
}

#[test]
fn import_reproduces_every_card_count_in_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();

    let applied = store.import_apply(&import_ctx(), &bundle).unwrap();
    assert!(!applied.replayed);
    let plan = applied.value;
    assert!(plan.ok());

    let counts = &bundle.manifest.counts;
    let imported = count(
        &store,
        "SELECT count(*) FROM cards WHERE legacy_id IS NOT NULL",
    );
    assert_eq!(imported as usize, counts.cards);
    let bundle_done = bundle.cards.iter().filter(|c| c.column == "done").count();
    let closed = count(
        &store,
        "SELECT count(*) FROM cards WHERE legacy_id IS NOT NULL AND state = 'closed'",
    );
    assert_eq!(closed as usize, bundle_done);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM projects WHERE id IN ('alpha', 'betasite')"
        ),
        2
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM evidence WHERE kind = 'legacy'"
        ) as usize,
        counts.memory
    );

    // Outcomes, waits and PRs as Tiller records them.
    let outcome = |legacy: &str| -> Option<String> {
        store
            .conn()
            .query_row(
                "SELECT outcome FROM cards WHERE legacy_id = ?1",
                [legacy],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(outcome("a1").as_deref(), Some("landed"));
    assert_eq!(outcome("b1").as_deref(), Some("cancelled"));
    assert_eq!(outcome("f1").as_deref(), Some("answered"));
    assert_eq!(column_of(&store, "a5"), "needs_you");
    assert_eq!(column_of(&store, "n1"), "needs_you");
    assert_eq!(column_of(&store, "a6"), "queued");
    assert_eq!(column_of(&store, "a7"), "queued");
    assert_eq!(column_of(&store, "a3"), "queued");
    let question: String = store
        .conn()
        .query_row(
            "SELECT a.question FROM attention a JOIN cards c ON c.id = a.card_id WHERE c.legacy_id = 'a5'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(question, "Which name should the app use?");
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM holds WHERE until_at = '2099-01-05T09:00:00.000Z'"
        ),
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM card_deps WHERE released_at IS NULL"
        ),
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM card_deps WHERE release_reason = 'landed'"
        ),
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM prs WHERE number = 1 AND state = 'merged'"
        ),
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM prs WHERE number = 4 AND state = 'open'"
        ),
        1
    );
    // A fleet card's PR is filed under the project whose repo holds it.
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM prs p JOIN cards c ON c.id = p.card_id
             WHERE p.number = 9 AND p.project_id = 'alpha' AND c.legacy_id = 'f1'"
        ),
        1
    );
    assert_eq!(count(&store, "SELECT count(*) FROM runs"), 0);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM projects WHERE id = 'gone' AND retired_at IS NOT NULL"
        ),
        1
    );
    assert_eq!(
        count(
            &store,
            "SELECT next_card_seq FROM projects WHERE id = 'alpha'"
        ),
        9
    );
    assert_eq!(
        count(
            &store,
            "SELECT next_card_seq FROM projects WHERE id = 'fleet'"
        ),
        3
    );

    // Every row this import wrote has its event, all under actor = import.
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM events WHERE txn = 'import.apply' AND actor <> 'import'"
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM events WHERE kind = 'card.created' AND txn = 'import.apply'"
        ),
        11
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM events WHERE kind = 'import.applied'"
        ),
        1
    );
    let sha: String = store
        .conn()
        .query_row(
            "SELECT value FROM meta WHERE key = 'import_manifest_sha256'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(sha, bundle.manifest_sha256);
}

#[test]
fn memory_becomes_a_placement_plan_with_legacy_import_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();
    store.import_apply(&import_ctx(), &bundle).unwrap();

    // Nothing is placed yet: no memory rows and no approved revision.
    assert_eq!(count(&store, "SELECT count(*) FROM memories"), 0);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM memory_revisions WHERE review <> 'unreviewed'"
        ),
        0
    );
    assert_eq!(count(&store, "SELECT count(*) FROM approvals"), 0);

    // Rules and notes become proposals; the requirement and the stale entry do not.
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT json_extract(p.memory_fields, '$.legacy_id'), s.canonical_path, p.op, p.state,
                    json_extract(p.memory_fields, '$.provenance'), p.rationale, p.authored_by_session_id,
                    p.diff, p.base_blob, p.card_id
             FROM proposals p JOIN memory_sources s ON s.id = p.target_source_id
             ORDER BY 1",
        )
        .unwrap();
    type Row = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        Option<String>,
    );
    let rows: Vec<Row> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let placed: Vec<(&str, &str)> = rows.iter().map(|r| (r.0.as_str(), r.1.as_str())).collect();
    assert_eq!(
        placed,
        [
            ("mem-1", "fleet.md"),
            ("mem-2", "projects/alpha.md"),
            ("mem-5", "projects/betasite.md")
        ]
    );
    for row in &rows {
        assert_eq!(
            (row.2.as_str(), row.3.as_str(), row.4.as_str()),
            ("add", "proposed", "legacy-import")
        );
        assert_eq!(row.6, None, "an imported proposal has no authoring session");
        assert_eq!(row.9, None, "an imported proposal has no origin card");
        assert_eq!(row.8, plan::EMPTY_BLOB);
    }
    // A rationale only where the source stated one.
    assert_eq!(rows[0].5.as_deref(), Some("The owner said so."));
    assert_eq!(rows[1].5, None);
    assert_eq!(
        rows[1].7,
        "--- a/projects/alpha.md\n+++ b/projects/alpha.md\n@@ -0,0 +1,5 @@\n\
         +## Watch out for\n+\n+- alpha builds slowly.\n+\n+  Use the cache.\n"
    );
    // Every proposal cites its evidence row.
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM proposal_evidence pe JOIN evidence e ON e.id = pe.evidence_id WHERE e.kind = 'legacy'"
        ),
        3
    );
    // The requirement and the stale entry are listed in the plan, not placed.
    let plan = store.import_plan(&bundle, Timestamp::now()).unwrap();
    let listed: Vec<(&str, &Placement)> = plan
        .memory
        .iter()
        .filter(|m| !matches!(m.placement, Placement::Proposal { .. }))
        .map(|m| (m.legacy_id.as_str(), &m.placement))
        .collect();
    assert_eq!(
        listed,
        [
            ("mem-3", &Placement::Requirement),
            (
                "mem-4",
                &Placement::NotPlaced {
                    reason: "The pipeline was replaced.".into()
                }
            ),
        ]
    );
}

/// `recovery.md` R-idempotency: an import that runs twice.
#[test]
fn second_import_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();
    store.import_apply(&import_ctx(), &bundle).unwrap();
    let after_first = snapshot(store.conn(), &[]);

    let again = store
        .import_apply(&import_ctx(), &Bundle::read(dir.path()).unwrap())
        .unwrap();
    assert!(again.replayed);
    assert!(again.value.already_imported);
    assert!(again.events.is_empty());
    assert_eq!(
        snapshot(store.conn(), &[]),
        after_first,
        "the second import wrote"
    );

    // A re-export of the same records (a new manifest) adds no rows either:
    // every project, card and memory entry is found and kept.
    let other = tempfile::tempdir().unwrap();
    write_bundle(other.path(), &records(), "final");
    let reexport = Bundle::read(other.path()).unwrap();
    let applied = store.import_apply(&import_ctx(), &reexport).unwrap();
    assert!(
        applied
            .value
            .cards
            .iter()
            .all(|c| c.action == Action::Exists)
    );
    assert!(
        applied
            .value
            .memory
            .iter()
            .all(|m| m.action == Action::Exists)
    );
    let changed: Vec<String> = snapshot(store.conn(), &[])
        .into_iter()
        .zip(after_first)
        .filter(|(now, before)| now != before)
        .map(|(now, _)| now.0)
        .collect();
    assert_eq!(
        changed,
        ["events", "meta"],
        "only the import.applied event and the manifest hash change"
    );
}

#[test]
fn a_tampered_miscounted_or_foreign_bundle_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &records(), "dry");
    std::fs::write(dir.path().join("cards.jsonl"), jsonl(&records().cards[1..])).unwrap();
    let error = Bundle::read(dir.path()).unwrap_err().to_string();
    assert!(
        error.contains("cards.jsonl differs from the manifest's sha256"),
        "{error}"
    );

    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &records(), "dry");
    let path = dir.path().join("manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    manifest["counts"]["cards"] = json!(12);
    std::fs::write(&path, manifest.to_string()).unwrap();
    let error = Bundle::read(dir.path()).unwrap_err().to_string();
    assert!(
        error.contains("the manifest counts 12 cards, the bundle holds 11"),
        "{error}"
    );

    manifest["counts"]["cards"] = json!(11);
    manifest["schema"] = json!("tiller-import/v2");
    std::fs::write(&path, manifest.to_string()).unwrap();
    let error = Bundle::read(dir.path()).unwrap_err().to_string();
    assert!(
        error.contains("this tiller reads tiller-import/v1"),
        "{error}"
    );

    manifest["schema"] = json!("tiller-import/v1");
    manifest["boundary"] = json!("work");
    std::fs::write(&path, manifest.to_string()).unwrap();
    let foreign = Bundle::read(dir.path()).unwrap();
    let mut store = store();
    let before = snapshot(store.conn(), &[]);
    let error = store
        .import_apply(&import_ctx(), &foreign)
        .unwrap_err()
        .to_string();
    assert!(error.contains("boundary is work"), "{error}");
    assert_eq!(snapshot(store.conn(), &[]), before);
}

#[test]
fn a_placement_into_a_document_with_content_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();
    store.import_apply(&import_ctx(), &bundle).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub, recorded_at, recorded_by)
             SELECT '01JZZZZZZZZZZZZZZZZZZZZZZZ', id, 'abc123', 'unreviewed', 'external', 'passed',
                    '2099-01-01T00:00:00.000Z', 'daemon'
             FROM memory_sources WHERE canonical_path = 'fleet.md'",
            [],
        )
        .unwrap();
    let mut records = records();
    records
        .memory
        .push(memory("mem-6", "global", "rule", "A new rule."));
    let other = tempfile::tempdir().unwrap();
    write_bundle(other.path(), &records, "final");
    let error = store
        .import_apply(&import_ctx(), &Bundle::read(other.path()).unwrap())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("the document already has content"),
        "{error}"
    );
}
