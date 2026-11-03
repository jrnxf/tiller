use rusqlite::Connection;

use super::*;

const FIXTURE_V1: &str = include_str!("fixture_v1.sql");

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
