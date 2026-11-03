//! Read-only mode for a damaged live database (`recovery.md` R10).
//!
//! `PRAGMA quick_check` runs on `state.db` at daemon start and after any
//! SQLite error that names corruption. A database that fails it puts the
//! daemon here: reads and the board keep working, every mutation is refused
//! with the reason, the workers do not start and the daemon writes nothing,
//! and `<home>/read-only` tells hooks to spool. Restore is the owner's
//! command, run with the daemon stopped; the next start checks again.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::{Connection, OpenFlags};
use tiller_core::Error;
use tiller_core::error::{DAMAGED_PREFIX, READ_ONLY_MARKER};

/// Whether the live database is damaged, and why. Cheap to clone; every
/// clone sees the same answer.
#[derive(Clone, Default)]
pub struct Damage(Arc<Mutex<Option<String>>>);

impl Damage {
    /// Why the daemon is read-only, if it is.
    pub fn reason(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The start-up check: a database that passes clears a marker left by an
    /// earlier run (the owner restored since); one that fails sets it.
    pub fn check_at_start(&self, db: &Path) {
        if !db.exists() {
            return;
        }
        match quick_check(db) {
            Ok(()) => {
                *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
                if let Some(parent) = db.parent() {
                    let _ = std::fs::remove_file(parent.join(READ_ONLY_MARKER));
                }
            }
            Err(reason) => self.mark(db, reason),
        }
    }

    /// After a failed request: an error that names corruption runs the check
    /// again, and a database that fails it makes the daemon read-only.
    pub fn suspect(&self, db: &Path, error: &Error) {
        if matches!(error, Error::Unavailable(message) if message.starts_with(DAMAGED_PREFIX))
            && let Err(reason) = quick_check(db)
        {
            self.mark(db, reason);
        }
    }

    fn mark(&self, db: &Path, reason: String) {
        tracing::error!(
            reason,
            "state.db failed its integrity check; read-only mode"
        );
        if let Some(parent) = db.parent()
            && let Err(error) = std::fs::write(parent.join(READ_ONLY_MARKER), &reason)
        {
            tracing::warn!(%error, "could not write the read-only marker; hooks will not spool");
        }
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(reason);
    }
}

/// `PRAGMA quick_check` on a read-only connection. `Err` carries the first
/// findings when the database is damaged. A failure that is not damage (a
/// busy database, say) passes: it names no corruption, so it must not take
/// the daemon down.
fn quick_check(db: &Path) -> Result<(), String> {
    let found =
        Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY).and_then(|conn| {
            let mut statement = conn.prepare("PRAGMA quick_check")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        });
    match found {
        Ok(rows) if rows == ["ok"] => Ok(()),
        Ok(rows) => Err(format!(
            "quick_check: {}",
            rows.into_iter().take(3).collect::<Vec<_>>().join("; ")
        )),
        Err(error) if is_damage(&error) => Err(format!("quick_check: {error}")),
        Err(error) => {
            tracing::warn!(%error, "quick_check could not run; not treating it as damage");
            Ok(())
        }
    }
}

fn is_damage(error: &rusqlite::Error) -> bool {
    use rusqlite::ErrorCode;
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

#[cfg(test)]
mod fixtures {
    use std::io::{Seek, SeekFrom, Write};
    use std::path::Path;

    use rusqlite::Connection;

    /// Overwrites the first page of `table`'s b-tree with 0xFF, so
    /// `quick_check` fails while every other table still reads. The WAL is
    /// folded into the file first and no other connection may be open.
    pub fn damage_table(db: &Path, table: &str) {
        let conn = Connection::open(db).unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        let root: i64 = conn
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        let size: i64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        drop(conn);
        let mut file = std::fs::OpenOptions::new().write(true).open(db).unwrap();
        file.seek(SeekFrom::Start(u64::try_from((root - 1) * size).unwrap()))
            .unwrap();
        file.write_all(&vec![0xFF; usize::try_from(size).unwrap()])
            .unwrap();
        file.sync_all().unwrap();
    }

    /// Cards in `db`, read without opening it as the daemon does.
    pub fn cards_in(db: &Path) -> i64 {
        Connection::open(db)
            .unwrap()
            .query_row("SELECT count(*) FROM cards", [], |row| row.get(0))
            .unwrap()
    }
}

/// `recovery.md` R10: a live database that fails `quick_check` at start
/// leaves the daemon serving reads and refusing every mutation with the
/// reason, and a hook spools instead of writing.
#[cfg(test)]
#[tokio::test]
async fn damaged_database_refuses_mutations_and_serves_reads() {
    use axum::http::StatusCode;
    use serde_json::Value;
    use tiller_herdr::testing::Fleet;

    use crate::api::tests::{Fixture, app_with, cards, create, get, owner_cookie, send};
    use crate::assets::Assets;

    let fleet = Fleet::new();
    let home = fleet.dir.path();
    let cookie = owner_cookie(home);
    let before = cards(&fleet);
    // `handoffs` is read by no board endpoint, so reads stay whole.
    fixtures::damage_table(&home.join("state.db"), "handoffs");

    let damage = Damage::default();
    damage.check_at_start(&home.join("state.db"));
    let reason = damage.reason().expect("quick_check names the damage");
    assert!(reason.starts_with("quick_check:"), "{reason}");
    assert_eq!(
        std::fs::read_to_string(home.join(READ_ONLY_MARKER)).unwrap(),
        reason,
        "hooks find the reason in the marker"
    );

    let app = app_with(home, Assets::of::<Fixture>(), damage);
    for path in ["/api/v1/board", "/api/v1/health", "/api/v1/agents"] {
        let read = get(&app, path).await;
        assert_eq!(read.status, StatusCode::OK, "{path}: {}", read.body);
    }

    let refused = send(&app, create(&[("cookie", &cookie)])).await;
    assert_eq!(
        refused.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        refused.body
    );
    let body: Value = serde_json::from_str(&refused.body).unwrap();
    assert_eq!(body["error"]["code"], "unavailable");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("read only: state.db failed its integrity check"),
        "{message}"
    );
    assert!(
        message.contains(&reason),
        "the refusal names the reason: {message}"
    );
    assert!(
        body["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("tiller restore")
    );
    assert_eq!(
        fixtures::cards_in(&home.join("state.db")),
        before,
        "a refused mutation writes nothing"
    );
}

/// R10: "after any SQLite error that names corruption". The daemon started
/// whole; a read then meets a damaged page, the check runs again, and the
/// next mutation is refused.
#[cfg(test)]
#[tokio::test]
async fn corruption_found_while_serving_makes_the_daemon_read_only() {
    use axum::http::StatusCode;
    use tiller_herdr::testing::Fleet;

    use crate::api::tests::{Fixture, app_with, create, get, owner_cookie, send};
    use crate::assets::Assets;

    let fleet = Fleet::new();
    let home = fleet.dir.path();
    let cookie = owner_cookie(home);
    let damage = Damage::default();
    damage.check_at_start(&home.join("state.db"));
    assert_eq!(damage.reason(), None);
    let app = app_with(home, Assets::of::<Fixture>(), damage.clone());

    fixtures::damage_table(&home.join("state.db"), "cards");
    let read = get(&app, "/api/v1/board").await;
    assert_eq!(
        read.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        read.body
    );
    assert!(
        damage.reason().is_some(),
        "the corruption error ran quick_check"
    );

    let refused = send(&app, create(&[("cookie", &cookie)])).await;
    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(refused.body.contains("read only"), "{}", refused.body);
}

/// A restored database passes the next start's check, which lifts the mode.
#[cfg(test)]
#[test]
fn a_passing_check_at_start_clears_the_marker() {
    use tiller_herdr::testing::Fleet;

    let fleet = Fleet::new();
    let home = fleet.dir.path();
    std::fs::write(home.join(READ_ONLY_MARKER), "quick_check: old").unwrap();
    let damage = Damage::default();
    damage.check_at_start(&home.join("state.db"));
    assert_eq!(damage.reason(), None);
    assert!(!home.join(READ_ONLY_MARKER).exists());
}
