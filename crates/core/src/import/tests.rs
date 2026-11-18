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

fn needs_you_of(store: &Store, legacy_id: &str) -> bool {
    store
        .conn()
        .query_row(
            "SELECT v.needs_you FROM card_column v JOIN cards c ON c.id = v.id WHERE c.legacy_id = ?1",
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
    // Every entry is placed as a pending note or listed by the import.
    let listed: Value = store
        .conn()
        .query_row(
            "SELECT payload FROM events WHERE kind = 'import.applied'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|text| serde_json::from_str(&text).unwrap())
        .unwrap();
    assert_eq!(
        pending(&store).len()
            + listed["requirements"].as_array().unwrap().len()
            + listed["not_placed"].as_array().unwrap().len(),
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
    // A scout cannot close answered: the delivered outcome becomes its own.
    assert_eq!(outcome("f1").as_deref(), Some("reported"));
    // A question keeps a card in its column and flags it for the owner.
    assert_eq!(column_of(&store, "a5"), "queued");
    assert_eq!(column_of(&store, "n1"), "queued");
    assert!(needs_you_of(&store, "a5") && needs_you_of(&store, "n1"));
    assert!(!needs_you_of(&store, "a6"));
    assert_eq!(column_of(&store, "a6"), "queued");
    assert_eq!(column_of(&store, "a7"), "queued");
    assert_eq!(column_of(&store, "a3"), "queued");
    assert_eq!(column_of(&store, "a4"), "landing");
    // The plan prints the column each card derives once written.
    for card in plan.cards.iter().filter(|c| c.state == CardState::Open) {
        assert_eq!(
            card.column,
            column_of(&store, &card.legacy_id),
            "{}",
            card.legacy_id
        );
    }
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

/// The pending notes an import wrote: (legacy id, project, kind, title, body).
fn pending(store: &Store) -> Vec<(String, String, String, String, String)> {
    store
        .conn()
        .prepare(
            "SELECT json_extract(e.payload, '$.legacy_id'), p.project_id, p.kind, p.title, p.body
             FROM memory_pending p JOIN events e ON e.entity_id = p.id AND e.kind = 'memory.noted'
             WHERE p.source = 'migration' ORDER BY 1",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn memory_becomes_pending_notes_for_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();
    store.import_apply(&import_ctx(), &bundle).unwrap();

    // Rules become conventions and notes gotchas, each a pending note of
    // its project; nothing reaches kb and nothing asks for an approval.
    let s = |text: &str| text.to_owned();
    assert_eq!(
        pending(&store),
        [
            (
                s("mem-1"),
                s("fleet"),
                s("convention"),
                s("Workers stay on Claude."),
                s("Workers stay on Claude.")
            ),
            (
                s("mem-2"),
                s("alpha"),
                s("gotcha"),
                s("alpha builds slowly."),
                s("alpha builds slowly.\n\nUse the cache.")
            ),
            (
                s("mem-5"),
                s("betasite"),
                s("convention"),
                s("Beta deploys on Fridays only."),
                s("Beta deploys on Fridays only.")
            ),
        ]
    );
    assert_eq!(count(&store, "SELECT count(*) FROM approvals"), 0);

    // The requirement and the stale entry are listed in the plan, not placed.
    let plan = store.import_plan(&bundle, Timestamp::now()).unwrap();
    let listed: Vec<(&str, &Placement)> = plan
        .memory
        .iter()
        .filter(|m| !matches!(m.placement, Placement::Pending { .. }))
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
    assert!(plan.memory.iter().all(|m| m.action == Action::Exists));
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

    // A re-export of the same records (a new manifest) writes nothing either:
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
    assert!(applied.events.is_empty());
    assert_eq!(
        snapshot(store.conn(), &[]),
        after_first,
        "the re-export wrote"
    );
}

/// What `card.create`, `card.close` and `dep.add` refuse, the importer
/// refuses or maps the same way.
#[test]
fn cards_keep_the_lifecycle_rules() {
    let refused = |extra: Vec<Value>| -> String {
        let mut records = self::records();
        records.cards.extend(extra);
        let dir = tempfile::tempdir().unwrap();
        write_bundle(dir.path(), &records, "dry");
        store()
            .import_plan(&Bundle::read(dir.path()).unwrap(), Timestamp::now())
            .unwrap_err()
            .to_string()
    };
    let mut blank = card("x1", "alpha", "ship", "queued");
    blank["title"] = json!("  ");
    assert!(refused(vec![blank]).contains("the card has no title"));
    let mut c1 = card("c1", "alpha", "ship", "queued");
    c1["depends_on"] = json!(["c2"]);
    let mut c2 = card("c2", "alpha", "ship", "queued");
    c2["depends_on"] = json!(["c1"]);
    let error = refused(vec![c1, c2]);
    assert!(
        error.contains("form a cycle: c1 -> c2 -> c1")
            || error.contains("form a cycle: c2 -> c1 -> c2"),
        "{error}"
    );

    // A dependency on a card that closed undelivered stays and asks the owner.
    let mut records = self::records();
    let mut d1 = card("d1", "alpha", "ship", "queued");
    d1["depends_on"] = json!(["b1"]);
    records.cards.push(d1);
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &records, "dry");
    let plan = store()
        .import_plan(&Bundle::read(dir.path()).unwrap(), Timestamp::now())
        .unwrap();
    let d1 = plan.cards.iter().find(|c| c.legacy_id == "d1").unwrap();
    assert_eq!(d1.column, "queued");
    assert!(
        matches!(
            d1.waits.as_slice(),
            [Wait::Attention { kind: AttentionKind::Decision, question }, Wait::Dependency { released: None, .. }]
                if question.contains("which closed cancelled")
        ),
        "{:?}",
        d1.waits
    );
    let f1 = plan.cards.iter().find(|c| c.legacy_id == "f1").unwrap();
    assert_eq!(
        (f1.outcome, f1.bundle_outcome.as_deref()),
        (Some(CardOutcome::Reported), Some("answered"))
    );
}

/// A hyphenated legacy id is kept as its own slug, unless an import from
/// before project ids could hold a hyphen already gave it a hyphen-free one.
#[test]
fn long_or_hyphenated_legacy_ids_keep_their_slug_or_their_earlier_one() {
    let dir = tempfile::tempdir().unwrap();
    let store = store();
    let host = crate::state::projects::local_host_id(store.conn()).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO projects (id, name, delivery, owner_host_id, added_at)
             VALUES ('oldsite', 'old-site', 'local-only', ?1, '2026-10-10T09:00:00.000Z'),
                    ('abcdefghijklmnop', 'abcdefghijklmnopq', 'local-only', ?1, '2026-10-10T09:00:00.000Z')",
            [&host],
        )
        .unwrap();
    let mut records = self::records();
    records
        .cards
        .push(card("hy1", "new-site", "ship", "queued"));
    records
        .cards
        .push(card("hy2", "old-site", "ship", "queued"));
    // a 17-character id an earlier import cut to 16
    records
        .cards
        .push(card("hy3", "abcdefghijklmnopq", "ship", "queued"));
    // a plain id naming the slug old-site already holds gives way
    records.cards.push(card("hy4", "oldsite", "ship", "queued"));
    write_bundle(dir.path(), &records, "dry");
    let plan = store
        .import_plan(&Bundle::read(dir.path()).unwrap(), Timestamp::now())
        .unwrap();
    let slug = |legacy: &str| {
        let p = plan
            .projects
            .iter()
            .find(|p| p.legacy_id == legacy)
            .unwrap();
        (p.id.clone(), p.action)
    };
    assert_eq!(slug("new-site"), ("new-site".to_owned(), Action::Create));
    assert_eq!(slug("old-site"), ("oldsite".to_owned(), Action::Exists));
    assert_eq!(
        slug("abcdefghijklmnopq"),
        ("abcdefghijklmnop".to_owned(), Action::Exists)
    );
    assert_eq!(slug("oldsite"), ("oldsite2".to_owned(), Action::Create));
    let display = |legacy: &str| {
        plan.cards
            .iter()
            .find(|c| c.legacy_id == legacy)
            .map(|c| c.display_id.clone())
            .unwrap()
    };
    assert_eq!(display("hy1"), "new-site-1");
    assert_eq!(display("hy2"), "oldsite-1");
}

/// A later bundle meets what the first import and the owner left in `state.db`.
#[test]
fn a_later_bundle_reads_state_db_as_it_now_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = store();
    store
        .import_apply(&import_ctx(), &bundle(dir.path()))
        .unwrap();
    // The owner closes a3 after the first import, and adds a project that
    // a legacy id would otherwise normalize onto.
    store
        .conn()
        .execute(
            "UPDATE cards SET state = 'closed', outcome = 'landed', closed_at = '2026-10-10T09:00:00.000Z',
                              closed_by = 'owner' WHERE legacy_id = 'a3'",
            [],
        )
        .unwrap();
    let host = crate::state::projects::local_host_id(store.conn()).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO projects (id, name, delivery, owner_host_id, added_at)
             VALUES ('gammaapp', 'gammaapp', 'local-only', ?1, '2026-10-10T09:00:00.000Z')",
            [&host],
        )
        .unwrap();

    let mut records = self::records();
    let mut d2 = card("d2", "alpha", "ship", "queued");
    d2["depends_on"] = json!(["a3"]);
    records.cards.push(d2);
    records
        .cards
        .push(card("e1", "Gamma.App", "ship", "queued"));
    records.cards.push(done(
        card("g2", "gone", "ship", "done"),
        "merged",
        "2026-10-07",
    ));
    let other = tempfile::tempdir().unwrap();
    write_bundle(other.path(), &records, "final");
    let plan = store
        .import_plan(&Bundle::read(other.path()).unwrap(), Timestamp::now())
        .unwrap();
    let d2 = plan.cards.iter().find(|c| c.legacy_id == "d2").unwrap();
    assert!(
        matches!(
            d2.waits.as_slice(),
            [Wait::Dependency {
                released: Some(DepReleaseReason::Landed),
                ..
            }]
        ),
        "{:?}",
        d2.waits
    );
    let gamma = plan
        .projects
        .iter()
        .find(|p| p.legacy_id == "Gamma.App")
        .unwrap();
    assert_eq!(
        (gamma.id.as_str(), gamma.action),
        ("gammaapp2", Action::Create)
    );
    // Closed history may join the retired project; open work may not.
    assert_eq!(
        plan.cards
            .iter()
            .find(|c| c.legacy_id == "g2")
            .unwrap()
            .action,
        Action::Create
    );
    records.cards.push(card("g3", "gone", "ship", "queued"));
    let third = tempfile::tempdir().unwrap();
    write_bundle(third.path(), &records, "final");
    let error = store
        .import_plan(&Bundle::read(third.path()).unwrap(), Timestamp::now())
        .unwrap_err()
        .to_string();
    assert!(error.contains("project gone is retired"), "{error}");
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

/// A later bundle adds only its new entries, also after the owner kept or
/// discarded the first ones: the `memory.noted` event outlives the row.
#[test]
fn a_later_bundle_adds_only_new_memory() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();
    store.import_apply(&import_ctx(), &bundle).unwrap();
    for (id, rev) in store
        .pending_notes(None)
        .unwrap()
        .into_iter()
        .map(|row| (row.id, row.rev))
    {
        store.mem_discard(&owner(), None, &id, rev).unwrap();
    }
    let mut records = self::records();
    records.memory.push(memory(
        "mem-6",
        "global",
        "rule",
        &"A long rule. ".repeat(200),
    ));
    records
        .memory
        .push(memory("mem-7", "global", "note", "A new note."));
    let other = tempfile::tempdir().unwrap();
    write_bundle(other.path(), &records, "final");
    let later = Bundle::read(other.path()).unwrap();
    let plan = store.import_apply(&import_ctx(), &later).unwrap().value;
    let created: Vec<(&str, &Placement)> = plan
        .memory
        .iter()
        .filter(|m| m.action == Action::Create)
        .map(|m| (m.legacy_id.as_str(), &m.placement))
        .collect();
    assert_eq!(
        created,
        [
            (
                "mem-6",
                &Placement::NotPlaced {
                    reason: "longer than 2000 characters, the most a pending note holds".into()
                }
            ),
            (
                "mem-7",
                &Placement::Pending {
                    kind: "gotcha".into()
                }
            ),
        ]
    );
    let rows = store.pending_notes(None).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
        ["A new note."]
    );
    // A third run writes nothing more.
    let again = store.import_apply(&import_ctx(), &later).unwrap();
    assert!(again.events.is_empty());
}

#[test]
fn a_deleted_projects_id_numbers_on_from_its_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();
    // As `project.delete` leaves it: alpha's rows are gone, its tombstone stays.
    store
        .conn()
        .execute(
            "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, payload)
             VALUES ('ev-1', '2026-10-01T00:00:00.000Z', 'project.delete', 'owner', 'project',
                     'alpha', 'project.deleted', '{\"next_card_seq\": 40}')",
            [],
        )
        .unwrap();

    store.import_apply(&import_ctx(), &bundle).unwrap();
    assert_eq!(
        count(
            &store,
            "SELECT next_card_seq FROM projects WHERE id = 'alpha'"
        ),
        48
    );
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM cards WHERE project_id = 'alpha'
             AND CAST(substr(display_id, 7) AS INTEGER) < 40"
        ),
        0,
        "no display id of the deleted project is reused"
    );
}

/// P10: the import stores each project's delivery mode and registry fields
/// on the project row. The exporter writes no `checks`, so a project comes in
/// with none unless a hand-edited bundle names them; the owner sets them with
/// `project.edit`.
#[test]
fn p10_import_stores_each_projects_delivery_and_registry_fields() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = bundle(dir.path());
    let mut store = store();
    store.import_apply(&import_ctx(), &bundle).unwrap();

    let alpha: Project = crate::state::get(store.conn(), "project", "alpha").unwrap();
    assert_eq!(alpha.delivery, Delivery::DirectPr);
    assert_eq!(alpha.repo.as_deref(), Some("example/alpha"));
    assert_eq!(
        alpha.remote.as_deref(),
        Some("https://github.com/example/alpha.git")
    );
    assert_eq!(alpha.default_branch, "main");
    assert_eq!(alpha.capacity, 3);
    assert_eq!(alpha.checks, json!([]));

    let beta: Project = crate::state::get(store.conn(), "project", "betasite").unwrap();
    assert_eq!(beta.delivery, Delivery::NoMistakes);
    assert_eq!(beta.repo.as_deref(), Some("example/beta"));
    assert_eq!(beta.remote, None);
    assert_eq!(beta.checks, json!([]));
}

/// P10: every delivery mode survives export, import and a read back through
/// the project registry.
#[test]
fn delivery_mode_round_trips_from_import() {
    const MODES: [&str; 5] = [
        "no-mistakes",
        "no-mistakes-prod-only",
        "direct-pr",
        "direct-push",
        "local-only",
    ];
    let dir = tempfile::tempdir().unwrap();
    let mut records = self::records();
    records.projects = MODES
        .iter()
        .map(|mode| json!({"id": format!("p-{mode}"), "delivery": mode, "default_branch": "main"}))
        .collect();
    records.cards.clear();
    records.memory.clear();
    records.inflight.clear();
    write_bundle(dir.path(), &records, "dry");
    let mut store = store();
    store
        .import_apply(&import_ctx(), &Bundle::read(dir.path()).unwrap())
        .unwrap();

    for mode in MODES {
        let project = store.project(&format!("p-{mode}")).unwrap();
        assert_eq!(project.delivery.to_string(), mode);
    }
}

/// P10: a bundle that names `checks` imports them in either shape, and a
/// shape the registry would refuse stops the plan.
#[test]
fn import_carries_a_projects_checks() {
    let dir = tempfile::tempdir().unwrap();
    let mut records = self::records();
    records.projects[0]["checks"] =
        json!({"full": ["cargo test"], "no_logic": ["cargo fmt --check"]});
    records.projects[1]["checks"] = json!(["make test"]);
    write_bundle(dir.path(), &records, "dry");
    let mut store = store();
    store
        .import_apply(&import_ctx(), &Bundle::read(dir.path()).unwrap())
        .unwrap();
    let alpha: Project = crate::state::get(store.conn(), "project", "alpha").unwrap();
    assert_eq!(
        alpha.checks,
        json!({"full": ["cargo test"], "no_logic": ["cargo fmt --check"]})
    );
    let beta: Project = crate::state::get(store.conn(), "project", "betasite").unwrap();
    assert_eq!(beta.checks, json!(["make test"]));

    let dir = tempfile::tempdir().unwrap();
    let mut bad = self::records();
    bad.projects[0]["checks"] = json!("cargo test");
    write_bundle(dir.path(), &bad, "dry");
    let error = self::store()
        .import_plan(&Bundle::read(dir.path()).unwrap(), Timestamp::now())
        .unwrap_err();
    assert!(error.to_string().contains("checks"), "{error}");
}

/// A bundle project gets the forge check of `project.add`: a Gerrit remote
/// with a delivery mode Tiller cannot publish with stops the plan, naming the
/// project, so the dry run shows it before anything is stored.
#[test]
fn a_gerrit_remote_that_would_publish_stops_the_plan() {
    let dir = tempfile::tempdir().unwrap();
    let mut records = self::records();
    records.projects[1]["remote"] = json!("ssh://review.example.com:29418/beta");
    write_bundle(dir.path(), &records, "dry");
    let mut store = store();
    let bundle = Bundle::read(dir.path()).unwrap();
    let error = store.import_plan(&bundle, Timestamp::now()).unwrap_err();
    assert!(error.to_string().contains("Beta-Site"), "{error}");
    assert!(error.to_string().contains("Gerrit"), "{error}");
    assert!(store.import_apply(&import_ctx(), &bundle).is_err());
    assert_eq!(
        count(&store, "SELECT count(*) FROM projects WHERE id != 'fleet'"),
        0
    );

    // local-only is a delivery mode Gerrit can run.
    let dir = tempfile::tempdir().unwrap();
    records.projects[1]["delivery"] = json!("local-only");
    write_bundle(dir.path(), &records, "dry");
    store
        .import_apply(&import_ctx(), &Bundle::read(dir.path()).unwrap())
        .unwrap();
    let beta: Project = crate::state::get(store.conn(), "project", "betasite").unwrap();
    assert_eq!(beta.delivery, Delivery::LocalOnly);
}

/// A bundle's default branch is stored trimmed, and one that is only space
/// is absent, as an empty one is: the project gets `main`.
#[test]
fn a_bundle_default_branch_is_stored_trimmed() {
    let dir = tempfile::tempdir().unwrap();
    let mut records = self::records();
    records.projects[0]["default_branch"] = json!(" trunk ");
    records.projects[1]["default_branch"] = json!("   ");
    write_bundle(dir.path(), &records, "dry");
    let mut store = store();
    store
        .import_apply(&import_ctx(), &Bundle::read(dir.path()).unwrap())
        .unwrap();
    let alpha: Project = crate::state::get(store.conn(), "project", "alpha").unwrap();
    assert_eq!(alpha.default_branch, "trunk");
    let beta: Project = crate::state::get(store.conn(), "project", "betasite").unwrap();
    assert_eq!(beta.default_branch, "main");
}
