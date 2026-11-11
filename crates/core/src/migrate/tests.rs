use rusqlite::Connection;

use super::*;

const FIXTURE_V1: &str = include_str!("fixture_v1.sql");

/// The rows of migration 4's tables and columns, loaded on top of
/// [`FIXTURE_V1`]: a paired peer on the board host and a live and a revoked
/// accepted key.
const FIXTURE_V4: &str = "
    INSERT INTO hosts (id, label, kind, herdr_machine, peer_scope, peer_host_id)
    VALUES ('01JC00000000000000000HOST3', 'host-c', 'remote', 'machine-c', 'act',
            '01JC0000000000000000PEERC');
    INSERT INTO peer_keys (id, viewer_label, viewer_host_id, public_key, scope, accepted_at, revoked_at)
    VALUES ('01JC00000000000000000KEY1', 'host-d', '01JC0000000000000000PEERD',
            'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=', 'read',
            '2026-11-03T09:00:00.000Z', '2026-11-03T10:00:00.000Z'),
           ('01JC00000000000000000KEY2', 'host-d', '01JC0000000000000000PEERD',
            'AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=', 'act',
            '2026-11-03T10:00:00.000Z', NULL);
";

/// The rows of migration 6's tables, loaded on top of [`FIXTURE_V1`]: a
/// light retro on the closed scout card, a change set holding the fixture's
/// proposal with one holdout run, and the manifest behind the fixture's
/// config hash.
const FIXTURE_V6: &str = "
    INSERT INTO retros (id, card_id, project_id, closed_at, depth, reason, counts, config_hashes, state, outcome,
                        finished_at, finished_by, created_at)
    VALUES ('01JC0000000000000000000RT1', '01JC0000000000000000000CA2', 'alpha', '2026-11-02T10:00:00.000Z',
            'light', 'routine', '{\"elapsed_minutes\": 55}', '[\"cfg-1\"]', 'done', 'nothing',
            '2026-11-02T10:00:05.000Z', 'daemon', '2026-11-02T10:00:05.000Z');
    INSERT INTO change_sets (id, name, state, window_cards, opened_at, opened_by)
    VALUES ('01JC0000000000000000000CS1', 'tighter briefs', 'open', 10, '2026-11-02T11:00:00.000Z', 'owner');
    INSERT INTO change_set_members (change_set_id, proposal_id, added_at)
    VALUES ('01JC0000000000000000000CS1', '01JC0000000000000000000PP1', '2026-11-02T11:00:00.000Z');
    INSERT INTO eval_runs (id, change_set_id, split, members_sha256, cases_total, cases_passed, overall_score,
                           cost_usd, partial, passed, claude_version, judge_model, ran_at, recorded_by)
    VALUES ('01JC0000000000000000000ER1', '01JC0000000000000000000CS1', 'holdout', 'members-1', 4, 4, 0.95,
            1.25, 0, 1, '2.1.0', 'judge-model', '2026-11-02T11:30:00.000Z', 'owner');
    INSERT INTO config_manifests (config_hash, parts, first_seen_at)
    VALUES ('cfg-1', '{\"role\": \"r\", \"settings\": \"s\"}', '2026-11-02T09:10:00.000Z');
";

fn connection() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    conn
}

/// Every object in the schema, as SQLite stores its definition.
fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn row_counts(conn: &Connection) -> Vec<(String, i64)> {
    let tables: Vec<String> = schema(conn)
        .into_iter()
        .filter(|(kind, _, _)| kind == "table")
        .map(|(_, name, _)| name)
        .collect();
    tables
        .into_iter()
        .map(|table| {
            let n = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table, n)
        })
        .collect()
}

fn foreign_key_violations(conn: &Connection) -> usize {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check").unwrap();
    stmt.query_map([], |_| Ok(())).unwrap().count()
}

/// T2.1 acceptance, `recovery.md` R11: every migration runs up and down over a
/// populated database, one step at a time, and each version's schema is the
/// same on the way down as on the way up.
#[test]
fn migrations_run_both_ways_on_fixture() {
    let mut conn = connection();
    let mut schemas = vec![schema(&conn)];
    for version in 1..=LATEST {
        to_version(&mut conn, version).unwrap();
        assert_eq!(current_version(&conn).unwrap(), version);
        if version == 1 {
            conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
                .unwrap();
            assert_eq!(foreign_key_violations(&conn), 0);
            let empty: Vec<_> = row_counts(&conn)
                .into_iter()
                .filter(|(_, n)| *n == 0)
                .collect();
            assert_eq!(empty, vec![], "the fixture must put a row in every table");
        }
        if version == 4 {
            // migration 4 runs down over a paired host and accepted keys
            conn.execute_batch(&format!("BEGIN; {FIXTURE_V4} COMMIT;"))
                .unwrap();
            assert_eq!(foreign_key_violations(&conn), 0);
        }
        if version == 6 {
            // migration 6 runs down over a retro, a change set with its
            // run, and a manifest
            conn.execute_batch(&format!("BEGIN; {FIXTURE_V6} COMMIT;"))
                .unwrap();
            assert_eq!(foreign_key_violations(&conn), 0);
        }
        schemas.push(schema(&conn));
    }
    let schema_version: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(schema_version, LATEST.to_string());

    for version in (0..LATEST).rev() {
        to_version(&mut conn, version).unwrap();
        assert_eq!(current_version(&conn).unwrap(), version);
        assert_eq!(
            schema(&conn),
            schemas[version],
            "schema at {version} on the way down"
        );
        assert_eq!(foreign_key_violations(&conn), 0);
    }
    assert_eq!(schema(&conn), vec![], "down to 0 leaves nothing behind");

    up(&mut conn).unwrap();
    assert_eq!(schema(&conn), schemas[LATEST]);
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
}

/// Migration 15: a key stored before it keeps its row with no actor and no
/// arguments hash, a new row can carry both, and going back down keeps every
/// key and its response.
#[test]
fn idempotency_keys_keep_their_rows_across_0015() {
    let mut conn = connection();
    to_version(&mut conn, 14).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    to_version(&mut conn, 15).unwrap();
    let old: (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT actor, args_sha256 FROM idempotency_keys WHERE key = 'spawn-key-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(old, (None, None), "a key from before 15 has neither");
    conn.execute(
        "INSERT INTO idempotency_keys
           (key, command, entity, entity_id, response, created_at, actor, args_sha256)
         VALUES ('card-key-1', 'card.create', 'card', 'c1', '{\"value\":{},\"events\":[2]}',
                 '2026-11-03T09:00:00.000Z', 'owner', 'abc123')",
        [],
    )
    .unwrap();

    to_version(&mut conn, 14).unwrap();
    let keys: Vec<(String, String)> = conn
        .prepare("SELECT key, response FROM idempotency_keys ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        keys,
        vec![
            (
                "card-key-1".to_owned(),
                "{\"value\":{},\"events\":[2]}".to_owned()
            ),
            (
                "spawn-key-1".to_owned(),
                "{\"value\":{},\"events\":[1]}".to_owned()
            ),
        ]
    );
    assert!(
        conn.prepare("SELECT actor FROM idempotency_keys").is_err(),
        "down drops the columns"
    );
    to_version(&mut conn, 15).unwrap();
    assert_eq!(current_version(&conn).unwrap(), 15);
}

/// Migration 16: a review stored before it keeps its row with no commit, so
/// it never satisfies a grant; a new row carries a full SHA and the column
/// refuses a short one; going back down keeps every review.
#[test]
fn reviews_keep_their_rows_across_0016() {
    let mut conn = connection();
    to_version(&mut conn, 15).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    to_version(&mut conn, 16).unwrap();
    let old: Option<String> = conn
        .query_row(
            "SELECT head_sha FROM reviews WHERE id = '01JC0000000000000000000RV1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old, None, "a review from before 16 has no commit");
    let insert = |id: &str, sha: &str| {
        conn.execute(
            "INSERT INTO reviews (id, run_id, card_id, reviewer, coverage, verdict, recorded_at, head_sha)
             SELECT ?1, run_id, card_id, 'code-review', 'diff', 'pass', '2026-11-03T09:00:00.000Z', ?2
             FROM reviews WHERE id = '01JC0000000000000000000RV1'",
            [id, sha],
        )
    };
    insert("01JC0000000000000000000RV2", &"ab".repeat(20)).unwrap();
    insert("01JC0000000000000000000RV3", &"cd".repeat(32)).unwrap();
    for bad in ["abcdef1", &"AB".repeat(20), &"zz".repeat(20)] {
        assert!(
            insert("01JC0000000000000000000RV4", bad).is_err(),
            "{bad} is not a full lowercase SHA"
        );
    }

    to_version(&mut conn, 15).unwrap();
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM reviews ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        ids,
        [
            "01JC0000000000000000000RV1",
            "01JC0000000000000000000RV2",
            "01JC0000000000000000000RV3"
        ]
    );
    assert!(
        conn.prepare("SELECT head_sha FROM reviews").is_err(),
        "down drops the column"
    );
    to_version(&mut conn, 16).unwrap();
    assert_eq!(current_version(&conn).unwrap(), 16);
}

/// Migration 18: a project from before it reads the newest of when it was
/// added, its newest project event and its newest card; a row inserted
/// without the column takes `added_at`; going back down keeps every project.
#[test]
fn projects_learn_when_they_last_changed_across_0018() {
    let mut conn = connection();
    to_version(&mut conn, 17).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    conn.execute_batch(
        "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, rev_after, payload)
         VALUES ('01JC00000000000000000EVP17', '2026-11-04T10:00:00.000Z', 'project.edit', 'owner',
                 'project', 'fleet', 'project.edited', 2, '{}');",
    )
    .unwrap();
    to_version(&mut conn, 18).unwrap();
    let edited = |conn: &Connection| -> Vec<(String, String, String)> {
        conn.prepare("SELECT id, added_at, edited_at FROM projects ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let newest_card = |conn: &Connection, project: &str| -> Option<String> {
        conn.query_row(
            "SELECT max(created_at) FROM cards WHERE project_id = ?1",
            [project],
            |row| row.get(0),
        )
        .unwrap()
    };
    for (id, added, at) in edited(&conn) {
        let expected = match id.as_str() {
            "fleet" => "2026-11-04T10:00:00.000Z".to_owned(),
            _ => newest_card(&conn, &id).map_or(added.clone(), |card| card.max(added)),
        };
        assert_eq!(at, expected, "{id}");
    }

    conn.execute(
        "INSERT INTO projects (id, name, delivery, owner_host_id, added_at)
         SELECT 'gamma', 'Gamma', 'local-only', owner_host_id, '2026-11-05T09:00:00.000Z'
         FROM projects WHERE id = 'fleet'",
        [],
    )
    .unwrap();
    let gamma: String = conn
        .query_row(
            "SELECT edited_at FROM projects WHERE id = 'gamma'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(gamma, "2026-11-05T09:00:00.000Z");

    let before = edited(&conn).len();
    to_version(&mut conn, 17).unwrap();
    let count: i64 = conn
        .query_row("SELECT count(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count as usize, before);
    assert!(
        conn.prepare("SELECT edited_at FROM projects").is_err(),
        "down drops the column"
    );
    to_version(&mut conn, 18).unwrap();
    assert_eq!(current_version(&conn).unwrap(), 18);
}

/// Migration 19: load rows keep their order across the rebuild, a second
/// trigger in the same millisecond gets its own row, and going back down
/// keeps the row written first.
#[test]
fn load_rows_are_keyed_by_trigger_across_0019() {
    let mut conn = connection();
    to_version(&mut conn, 18).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    let loads = |conn: &Connection| -> Vec<(String, String)> {
        conn.prepare("SELECT trigger, loaded_at FROM session_loads ORDER BY rowid")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let same_ms = "INSERT INTO session_loads (session_id, revision_id, trigger, loaded_at)
                   VALUES ('01JC0000000000000000000SW1', '01JC0000000000000000000MR1', 'clear',
                           '2026-11-02T09:10:02.000Z')";
    assert!(
        conn.execute(same_ms, []).is_err(),
        "before 19 the key has no trigger"
    );
    let before = loads(&conn);
    to_version(&mut conn, 19).unwrap();
    assert_eq!(loads(&conn), before);
    conn.execute(same_ms, []).unwrap();
    let both = [
        ("startup".to_owned(), "2026-11-02T09:10:02.000Z".to_owned()),
        ("clear".to_owned(), "2026-11-02T09:10:02.000Z".to_owned()),
    ];
    assert_eq!(loads(&conn), both);

    to_version(&mut conn, 18).unwrap();
    assert_eq!(loads(&conn), both[..1]);
    to_version(&mut conn, 19).unwrap();
    assert_eq!(current_version(&conn).unwrap(), 19);
}

/// Migration 2 lowers no project's rigor: every project that existed before
/// it reads `launched`, with nothing reviewed yet.
#[test]
fn existing_projects_read_launched_after_0002() {
    let mut conn = connection();
    to_version(&mut conn, 1).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    let before: i64 = conn
        .query_row("SELECT count(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    assert!(before > 0);
    to_version(&mut conn, 2).unwrap();
    // Read through today's record, which needs every later column; no later
    // migration touches the stage.
    to_version(&mut conn, LATEST).unwrap();
    let projects: Vec<crate::domain::Project> =
        crate::state::list(&conn, "1 ORDER BY id", &[]).unwrap();
    assert_eq!(projects.len() as i64, before);
    for project in projects {
        assert_eq!(
            project.stage,
            crate::domain::Stage::Launched,
            "{}",
            project.id
        );
        assert_eq!(project.effective_delivery(), project.delivery);
        assert_eq!(project.reviewed_through_sha, None);
        assert_eq!(project.stage_changed_at, None);
    }
    let passes: i64 = conn
        .query_row("SELECT count(*) FROM review_passes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(passes, 0);
}

/// Migration 4 pairs nothing by itself, and its checks hold the rules of
/// `schema.sql`: only a remote host is paired, scope and host id go together,
/// and a board host has at most one live key on a peer.
#[test]
fn pairing_columns_and_keys_after_0004() {
    let mut conn = connection();
    to_version(&mut conn, 3).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    to_version(&mut conn, 4).unwrap();
    let paired: i64 = conn
        .query_row(
            "SELECT count(*) FROM hosts WHERE peer_scope IS NOT NULL OR peer_host_id IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(paired, 0);

    for bad in [
        "UPDATE hosts SET peer_scope = 'act', peer_host_id = 'x' WHERE label = 'host-a'",
        "UPDATE hosts SET peer_scope = 'act' WHERE label = 'host-b'",
        "UPDATE hosts SET peer_host_id = 'x' WHERE label = 'host-b'",
        "UPDATE hosts SET peer_scope = 'write', peer_host_id = 'x' WHERE label = 'host-b'",
    ] {
        assert!(conn.execute(bad, []).is_err(), "{bad}");
    }
    conn.execute(
        "UPDATE hosts SET peer_scope = 'read', peer_host_id = 'x' WHERE label = 'host-b'",
        [],
    )
    .unwrap();

    conn.execute_batch(FIXTURE_V4).unwrap();
    let second_live =
        "INSERT INTO peer_keys (id, viewer_label, viewer_host_id, public_key, scope, accepted_at)
        VALUES ('01JC00000000000000000KEY3', 'host-d', '01JC0000000000000000PEERD', 'k', 'act',
                '2026-11-03T11:00:00.000Z')";
    assert!(conn.execute(second_live, []).is_err());

    to_version(&mut conn, 3).unwrap();
    let hosts: i64 = conn
        .query_row("SELECT count(*) FROM hosts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(hosts, 3, "a paired host stays, as a herdr machine");
}

/// Migration 7: the `projects.id` check accepts exactly the ids
/// `validate_project_id` accepts, and going back down refuses while a project
/// needs the wider rule.
#[test]
fn hyphenated_project_ids_after_0007() {
    let mut conn = connection();
    to_version(&mut conn, 6).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    to_version(&mut conn, 7).unwrap();
    let insert = |conn: &Connection, id: &str| {
        conn.execute(
            "INSERT INTO projects (id, name, delivery, owner_host_id, added_at)
             VALUES (?1, ?1, 'local-only', '01JC00000000000000000HOST1', '2026-11-04T09:00:00.000Z')",
            [id],
        )
    };

    // every id of up to 6 characters over a small alphabet, and the length bounds
    let alphabet = ['a', '1', '-', 'A', '_'];
    let mut ids = vec![String::new()];
    for len in 1..=6 {
        let mut next = Vec::new();
        for id in ids.iter().filter(|id| id.len() == len - 1) {
            for c in alphabet {
                next.push(format!("{id}{c}"));
            }
        }
        ids.extend(next);
    }
    ids.extend(["a".repeat(31), "a".repeat(32), "a".repeat(33)]);
    ids.push(format!("{}-{}", "a".repeat(15), "b".repeat(16)));
    ids.push(format!("{}-{}", "a".repeat(16), "b".repeat(16)));
    conn.execute_batch("BEGIN").unwrap();
    for id in &ids {
        let valid = crate::ids::validate_project_id(id).is_ok();
        let stored = insert(&conn, id).is_ok();
        assert_eq!(stored, valid, "{id:?}");
    }
    conn.execute_batch("ROLLBACK").unwrap();

    for wide in ["web2-staging", "abcdefghijklmnopq"] {
        insert(&conn, wide).unwrap();
        let error = to_version(&mut conn, 6).unwrap_err().to_string();
        assert!(error.contains(wide), "{error}");
        assert_eq!(current_version(&conn).unwrap(), 7);
        conn.execute("DELETE FROM projects WHERE id = ?1", [wide])
            .unwrap();
    }
    to_version(&mut conn, 6).unwrap();
    // the old check, reloaded on this connection, again caps the length
    assert!(insert(&conn, "abcdefghijklmnopq").is_err());
    insert(&conn, "web2staging").unwrap();
}

/// The old check let some ids through that the new one refuses; migration 7
/// refuses rather than keep them, and names each.
#[test]
fn up_to_0007_refuses_a_stored_id_the_new_check_fails() {
    let mut conn = connection();
    to_version(&mut conn, 6).unwrap();
    conn.execute_batch(&format!("BEGIN; {FIXTURE_V1} COMMIT;"))
        .unwrap();
    conn.execute(
        "INSERT INTO projects (id, name, delivery, owner_host_id, added_at)
         VALUES ('ab_c', 'ab_c', 'local-only', '01JC00000000000000000HOST1', '2026-11-04T09:00:00.000Z')",
        [],
    )
    .unwrap();
    let error = to_version(&mut conn, 7).unwrap_err();
    assert!(matches!(error, Error::Refused { .. }), "{error:?}");
    assert!(error.to_string().contains("ab_c"), "{error}");
    assert_eq!(current_version(&conn).unwrap(), 6);
}

#[test]
fn latest_is_the_last_migration() {
    assert_eq!(list().len(), LATEST);
}

/// `recovery.md` R11: an older binary never writes to a newer database.
#[test]
fn older_binary_refuses_newer_schema_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    {
        let mut conn = Connection::open(&path).unwrap();
        up(&mut conn).unwrap();
        conn.execute_batch(&format!(
            "BEGIN; {FIXTURE_V1} COMMIT; PRAGMA user_version = {};",
            LATEST + 1
        ))
        .unwrap();
    }
    let before = {
        let conn = Connection::open(&path).unwrap();
        (schema(&conn), row_counts(&conn))
    };

    let error = crate::Store::open(&path).err().unwrap();
    assert!(matches!(error, crate::Error::SchemaTooNew { found, .. } if found == LATEST + 1));
    assert!(error.to_string().contains("tiller migrate down"));
    assert!(crate::Store::open_current(&path).is_err());

    let conn = Connection::open(&path).unwrap();
    assert_eq!((schema(&conn), row_counts(&conn)), before);
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, LATEST as i64 + 1);
}

#[test]
fn open_current_refuses_an_unmigrated_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    Connection::open(&path).unwrap();
    let error = crate::Store::open_current(&path).err().unwrap();
    assert_eq!(error.code(), "unavailable");
    crate::Store::open(&path).unwrap();
    crate::Store::open_current(&path).unwrap();
}
