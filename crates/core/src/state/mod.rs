//! The repository layer: every mutation in `state.md` section 8 as one method
//! on [`Store`](crate::Store). Each runs in one `BEGIN IMMEDIATE` transaction through
//! [`Store::write`](crate::Store::write), checks its preconditions inside it, and writes its event
//! rows before it commits. A mutation that composes others (a spawn, a cancel)
//! runs them in the same transaction and writes one event per sub-step.
//!
//! Field edits name the `rev` they were made against and are refused with a
//! readable conflict when it is stale. Transitions check the facts they need
//! instead, and a transition whose entity is already in the requested state
//! returns it and writes nothing (`protocol.md` section 6.3).

pub mod approvals;
pub mod asks;
pub mod attention;
pub mod caches;
pub mod cards;
pub mod deletion;
pub mod interventions;
pub mod learning;
pub mod memory;
pub mod notifications;
pub mod outbox;
pub mod peers;
pub mod projects;
pub mod publishes;
pub mod reads;
pub mod records;
pub mod review;
pub mod runs;
pub mod signals;
pub mod stamps;

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
pub(crate) mod tests;

use rusqlite::{Connection, OptionalExtension, ToSql};
use serde_json::Value;

use crate::domain::{Card, Record};
use crate::error::{Error, Result};
use crate::ids;

/// Loads one row by its `id` column.
pub fn get<T: Record>(conn: &Connection, entity: &'static str, id: &str) -> Result<T> {
    find(conn, "id = ?1", &[&id])?.ok_or_else(|| Error::not_found(entity, id))
}

/// Loads the first row matching `filter`, a `WHERE` clause.
pub fn find<T: Record>(
    conn: &Connection,
    filter: &str,
    params: &[&dyn ToSql],
) -> Result<Option<T>> {
    let sql = format!("{} WHERE {filter} LIMIT 1", T::select());
    Ok(conn
        .query_row(&sql, params, |row| T::from_row(row))
        .optional()?)
}

/// Loads every row matching `filter`, a `WHERE` clause with any `ORDER BY`.
pub fn list<T: Record>(conn: &Connection, filter: &str, params: &[&dyn ToSql]) -> Result<Vec<T>> {
    let sql = format!("{} WHERE {filter}", T::select());
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params, |row| T::from_row(row))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Resolves a ULID or its short form (the last 7 characters) to a full id of
/// `table`. An ambiguous short form fails with its candidates.
pub fn resolve_id(
    conn: &Connection,
    table: &str,
    entity: &'static str,
    input: &str,
) -> Result<String> {
    let input = input.trim();
    if ids::is_ulid(input) {
        let found: Option<String> = conn
            .query_row(
                &format!("SELECT id FROM {table} WHERE id = ?1"),
                [input],
                |row| row.get(0),
            )
            .optional()?;
        return found.ok_or_else(|| Error::not_found(entity, input));
    }
    if ids::is_short(input) {
        let upper = input.to_ascii_uppercase();
        let mut stmt = conn.prepare(&format!(
            "SELECT id FROM {table} WHERE substr(id, -{}) = ?1 ORDER BY id LIMIT 10",
            ids::SHORT_LEN
        ))?;
        let matches: Vec<String> = stmt
            .query_map([&upper], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        return match matches.as_slice() {
            [one] => Ok(one.clone()),
            [] => Err(Error::not_found(entity, input)),
            _ => Err(Error::NotFound {
                entity,
                id: input.to_owned(),
                candidates: matches,
            }),
        };
    }
    Err(Error::not_found(entity, input))
}

/// Resolves a card reference: a display id (`alpha-42`), a ULID or a short form.
pub fn resolve_card(conn: &Connection, input: &str) -> Result<Card> {
    let input = input.trim();
    if ids::parse_display_id(input).is_some() {
        return find(conn, "display_id = ?1", &[&input])?
            .ok_or_else(|| Error::not_found("card", input));
    }
    let id = resolve_id(conn, "cards", "card", input)?;
    get(conn, "card", &id)
}

/// Refuses an edit made against a stale `rev`, naming the first field the
/// caller meant to change and its current value.
pub(crate) fn check_rev(
    entity: &'static str,
    id: &str,
    current_rev: i64,
    expected_rev: i64,
    field: Option<(&str, Value)>,
) -> Result<()> {
    if current_rev == expected_rev {
        return Ok(());
    }
    Err(Error::Conflict {
        entity,
        id: id.to_owned(),
        current_rev,
        sent_rev: expected_rev,
        field: field.map(|(name, value)| (name.to_owned(), value)),
    })
}

/// Requires a non-empty text argument.
pub(crate) fn required(name: &str, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        Err(Error::Usage(format!("{name} must not be empty")))
    } else {
        Ok(())
    }
}

/// Maps a lifecycle refusal onto the entity it concerns.
pub(crate) fn refuse<'a>(
    entity: &'static str,
    id: &'a str,
) -> impl FnOnce(crate::domain::Illegal) -> Error + 'a {
    move |illegal| Error::refused(entity, id, illegal.0)
}

#[cfg(test)]
use tests::{DB_ENV, card, child, owner, setup, snapshot, wait_for};

/// T2.1 acceptance: a writer killed between its writes and `COMMIT` leaves
/// nothing behind: no ask, no card, no burned display id, no event, no key.
#[cfg(test)]
#[test]
fn killed_writer_leaves_no_half_state() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("state.db");
    let marker = dir.path().join("paused");
    {
        let mut store = crate::Store::open(&db).unwrap();
        setup(&mut store);
        store
            .card_create(
                &owner(),
                None,
                card(crate::domain::CardKind::Ship, "first"),
                None,
            )
            .unwrap();
    }
    let before = snapshot(crate::Store::open(&db).unwrap().conn(), &[]);

    let mut writer = child("state::killed_writer_child")
        .env(DB_ENV, &db)
        .env(testing::PAUSE_ENV, &marker)
        .spawn()
        .unwrap();
    assert!(
        wait_for(&marker, std::time::Duration::from_secs(60)),
        "the writer never reached COMMIT"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ask.record");
    writer.kill().unwrap();
    writer.wait().unwrap();

    let mut store = crate::Store::open(&db).unwrap();
    let integrity: String = store
        .conn()
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert_eq!(
        snapshot(store.conn(), &[]),
        before,
        "the killed transaction left rows behind"
    );

    // The same click again succeeds, and gets the display id the killed
    // writer would have used: the counter was never bumped.
    let added = store
        .board_add(
            &crate::Ctx::new(crate::Actor::Board),
            Some("click-1"),
            card(crate::domain::CardKind::Ship, "from the board"),
        )
        .unwrap();
    assert!(!added.replayed);
    assert_eq!(added.value.card.display_id, "alpha-2");
}

#[cfg(test)]
#[test]
#[ignore = "child process of killed_writer_leaves_no_half_state"]
fn killed_writer_child() {
    let Ok(db) = std::env::var(DB_ENV) else {
        return;
    };
    let mut store = crate::Store::open(db).unwrap();
    // A composite mutation: ask, ask resolve, counter bump, card, link and
    // the orchestrator's note, all in one transaction that never commits.
    store
        .board_add(
            &crate::Ctx::new(crate::Actor::Board),
            Some("click-1"),
            card(crate::domain::CardKind::Ship, "from the board"),
        )
        .unwrap();
    unreachable!("the writer must hang before COMMIT");
}
