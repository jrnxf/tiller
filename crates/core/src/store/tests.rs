//! Idempotency keys (`state.md` Bookkeeping and invariant 10, `protocol.md`
//! 6.3): a true retry replays, and a reused key never returns another
//! request's result.

use serde_json::json;

use super::*;

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

const T0: &str = "2026-11-03T09:00:00.000Z";

/// One keyed append: writes one event for `name` and returns its `seq`.
fn append(store: &mut Store, ctx: &Ctx, name: &str) -> Result<Applied<String>> {
    let name = name.to_owned();
    store.write(ctx, "note.add", None, move |tx| {
        tx.event(NewEvent::new("note", name.clone(), "note.added"))?;
        Ok(name)
    })
}

fn events(store: &Store) -> i64 {
    store
        .conn()
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap()
}

fn key_row(store: &Store, key: &str) -> Option<(String, Option<String>, Option<String>)> {
    store
        .conn()
        .query_row(
            "SELECT created_at, actor, args_sha256 FROM idempotency_keys WHERE key = ?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .unwrap()
}

fn refused_fact(result: Result<Applied<String>>) -> String {
    match result {
        Err(Error::Refused { entity, id, fact }) => {
            assert_eq!((entity, id.as_str()), ("idempotency key", "k1"));
            fact
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The same key, actor and arguments: the stored response, nothing written.
#[test]
fn true_retry_returns_the_first_result_and_writes_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    let args = json!({"project": "alpha", "title": "Alpha"});
    let ctx = Ctx::at(Actor::Owner, at(T0))
        .with_key("k1")
        .with_args("k1", &args);
    let first = append(&mut store, &ctx, "alpha").unwrap();
    assert!(!first.replayed);
    let row = key_row(&store, "k1").unwrap();
    assert_eq!(row.1.as_deref(), Some("owner"));
    assert_eq!(row.2.as_deref().map(str::len), Some(64), "a hex sha256");

    let again = append(&mut store, &ctx, "alpha-again").unwrap();
    assert!(again.replayed);
    assert_eq!(again.value, "alpha");
    assert_eq!(again.events, first.events);
    assert_eq!(events(&store), 1);
}

/// B5: the same key with other arguments is refused, not answered with the
/// first request's result.
#[test]
fn reused_key_with_other_arguments_is_refused() {
    let mut store = Store::open_in_memory().unwrap();
    let alpha = Ctx::at(Actor::Owner, at(T0))
        .with_key("k1")
        .with_args("k1", &json!({"title": "Alpha"}));
    append(&mut store, &alpha, "alpha").unwrap();

    let beta = Ctx::at(Actor::Owner, at(T0))
        .with_key("k1")
        .with_args("k1", &json!({"title": "Beta"}));
    let fact = refused_fact(append(&mut store, &beta, "beta"));
    assert!(fact.contains("other arguments"), "{fact}");
    assert_eq!(events(&store), 1);
}

/// B5: the same key from another actor is refused, even with the same
/// arguments.
#[test]
fn reused_key_from_another_actor_is_refused() {
    let mut store = Store::open_in_memory().unwrap();
    let args = json!({"title": "Alpha"});
    let first = Ctx::at(Actor::Session("s1".into()), at(T0))
        .with_key("k1")
        .with_args("k1", &args);
    append(&mut store, &first, "alpha").unwrap();

    let other = Ctx::at(Actor::Session("s2".into()), at(T0))
        .with_key("k1")
        .with_args("k1", &args);
    let fact = refused_fact(append(&mut store, &other, "alpha"));
    assert!(
        fact.contains("session:s1") && fact.contains("session:s2"),
        "{fact}"
    );
    assert_eq!(events(&store), 1);
}

/// A row from before schema 15 (no actor, no hash) replays for anyone, and a
/// request that sends no arguments matches a stored hash: a missing side
/// matches.
#[test]
fn a_missing_actor_or_hash_matches() {
    let mut store = Store::open_in_memory().unwrap();
    let first = Ctx::at(Actor::Owner, at(T0))
        .with_key("k1")
        .with_args("k1", &json!({"title": "Alpha"}));
    append(&mut store, &first, "alpha").unwrap();
    let bare = Ctx::at(Actor::Owner, at(T0)).with_key("k1");
    assert!(append(&mut store, &bare, "x").unwrap().replayed);

    store
        .conn()
        .execute(
            "UPDATE idempotency_keys SET actor = NULL, args_sha256 = NULL WHERE key = 'k1'",
            [],
        )
        .unwrap();
    let anyone = Ctx::at(Actor::Script, at(T0))
        .with_key("k1")
        .with_args("k1", &json!({"title": "Beta"}));
    let replay = append(&mut store, &anyone, "beta").unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.value, "alpha");
    assert_eq!(events(&store), 1);
}

/// Arguments bound to one key are not checked against another: a mutation
/// that writes under a key of its own ignores the caller's arguments.
#[test]
fn arguments_bind_only_to_their_own_key() {
    let mut store = Store::open_in_memory().unwrap();
    let ctx = Ctx::at(Actor::Owner, at(T0)).with_args("k1", &json!({"title": "Alpha"}));
    store
        .write(&ctx, "note.add", Some("derived"), |tx| {
            tx.event(NewEvent::new("note", "n", "note.added"))?;
            Ok("n".to_owned())
        })
        .unwrap();
    assert_eq!(key_row(&store, "derived").unwrap().2, None);
}

/// B15: asking whether a key is recorded never deletes it, even when it has
/// expired.
#[test]
fn key_recorded_on_an_expired_key_deletes_nothing() {
    let mut store = Store::open_in_memory().unwrap();
    let ctx = Ctx::at(Actor::Owner, at(T0)).with_key("k1");
    append(&mut store, &ctx, "alpha").unwrap();
    assert!(store.key_recorded(&ctx).unwrap());

    let later = Ctx::at(Actor::Owner, at("2026-12-10T09:00:00.000Z")).with_key("k1");
    assert!(!store.key_recorded(&later).unwrap());
    assert!(key_row(&store, "k1").is_some(), "a read writes nothing");
}

/// B15: an expired key is forgotten in the transaction that stores it again,
/// so a mutation that fails keeps the old row, and one that commits replaces
/// it.
#[test]
fn an_expired_key_is_replaced_only_when_the_new_mutation_commits() {
    let mut store = Store::open_in_memory().unwrap();
    let ctx = Ctx::at(Actor::Owner, at(T0)).with_key("k1");
    append(&mut store, &ctx, "alpha").unwrap();

    let later = Ctx::at(Actor::Script, at("2026-12-10T09:00:00.000Z")).with_key("k1");
    let failed: Result<Applied<String>> = store.write(&later, "note.add", None, |tx| {
        tx.event(NewEvent::new("note", "beta", "note.added"))?;
        Err(Error::refused("note", "beta", "precondition failed"))
    });
    assert!(failed.is_err());
    assert_eq!(
        key_row(&store, "k1").unwrap(),
        (T0.to_owned(), Some("owner".to_owned()), None),
        "the failed mutation rolled the delete back"
    );
    assert_eq!(events(&store), 1);

    let stored = append(&mut store, &later, "beta").unwrap();
    assert!(!stored.replayed, "an expired key keys a new mutation");
    let row = key_row(&store, "k1").unwrap();
    assert_eq!(row.0, "2026-12-10T09:00:00.000Z");
    assert_eq!(row.1.as_deref(), Some("script"));
    assert_eq!(events(&store), 2);
}
