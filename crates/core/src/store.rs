//! Opening `state.db` and running a mutation (`state.md` sections 1 and 8).
//!
//! Every mutation goes through [`Store::write`]: one `BEGIN IMMEDIATE`
//! transaction that checks the idempotency key, runs the mutation's checks and
//! writes, and commits only together with the mutation's event rows. A
//! mutation that changes a row without writing an event is rolled back as a
//! bug (invariant 3).

use std::fmt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::ids;
use crate::migrate;
use crate::time::Timestamp;

/// How long a writer waits for the database lock before giving up with
/// `unavailable` (`protocol.md` section 1).
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an idempotency key is remembered (`state.md`, Bookkeeping).
pub const KEY_TTL_DAYS: i64 = 30;

/// Who performs a mutation (`state.md`, Event; `authority.md` section 3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    Session(String),
    Board,
    Daemon,
    Hook,
    Import,
    Owner,
    /// The owner on a paired board host, through a forwarded mutation that
    /// `tiller peer serve` verified (`authority.md` section 3): the board's
    /// row of the matrix, limited to the forwarding allowlist. Holds the
    /// board host's label; written as `board@<label>`.
    Peer(String),
    /// The curator finishing one retro (`docs/design/learning-loop.md`,
    /// section 4): not a fleet role and not the owner. Holds the retro id;
    /// written as `curator:<retro>`.
    Curator(String),
}

impl Actor {
    /// The owner, acting through the CLI, the board or a peer board.
    pub fn is_owner(&self) -> bool {
        matches!(self, Actor::Owner | Actor::Board | Actor::Peer(_))
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Actor::Session(id) => Some(id),
            _ => None,
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Actor::Session(id) => write!(f, "session:{id}"),
            Actor::Board => f.write_str("board"),
            Actor::Daemon => f.write_str("daemon"),
            Actor::Hook => f.write_str("hook"),
            Actor::Import => f.write_str("import"),
            Actor::Owner => f.write_str("owner"),
            Actor::Peer(label) => write!(f, "board@{label}"),
            Actor::Curator(retro) => write!(f, "curator:{retro}"),
        }
    }
}

impl FromStr for Actor {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "board" => Ok(Actor::Board),
            "daemon" => Ok(Actor::Daemon),
            "hook" => Ok(Actor::Hook),
            "import" => Ok(Actor::Import),
            "owner" => Ok(Actor::Owner),
            _ => match (
                s.strip_prefix("session:"),
                s.strip_prefix("board@"),
                s.strip_prefix("curator:"),
            ) {
                (Some(id), _, _) if !id.is_empty() => Ok(Actor::Session(id.to_owned())),
                (_, Some(label), _) if !label.is_empty() => Ok(Actor::Peer(label.to_owned())),
                (_, _, Some(retro)) if !retro.is_empty() => Ok(Actor::Curator(retro.to_owned())),
                _ => Err(Error::Usage(format!("not an actor: {s:?}"))),
            },
        }
    }
}

impl Serialize for Actor {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Actor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// The caller of a mutation: who acts, and the time the mutation records.
#[derive(Clone, Debug)]
pub struct Ctx {
    pub actor: Actor,
    pub now: Timestamp,
    /// The caller's idempotency key, for a mutation whose function takes no
    /// key argument of its own (`protocol.md` section 6.3). An explicit key
    /// argument wins.
    pub key: Option<String>,
    /// For a forwarded mutation ([`Actor::Peer`]): which board host and key
    /// sent it. Every event the mutation writes carries it as
    /// `payload.via_peer` (`authority.md` section 3).
    pub via_peer: Option<Value>,
}

impl Ctx {
    pub fn new(actor: Actor) -> Self {
        Self::at(actor, Timestamp::now())
    }

    pub fn at(actor: Actor, now: Timestamp) -> Self {
        Self {
            actor,
            now,
            key: None,
            via_peer: None,
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn with_via_peer(mut self, via_peer: Value) -> Self {
        self.via_peer = Some(via_peer);
        self
    }
}

/// What a mutation returns: its value, the `seq` of every event it wrote, and
/// whether it was a replay (an idempotency key matched, or a natural
/// transition found the entity already in the requested state).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Applied<T> {
    pub value: T,
    pub events: Vec<i64>,
    pub replayed: bool,
}

/// An event row to write inside a mutation.
#[derive(Clone, Debug)]
pub struct NewEvent {
    pub entity: &'static str,
    pub entity_id: String,
    pub kind: &'static str,
    pub rev_after: Option<i64>,
    pub card_id: Option<String>,
    pub payload: Value,
}

impl NewEvent {
    pub fn new(entity: &'static str, entity_id: impl Into<String>, kind: &'static str) -> Self {
        Self {
            entity,
            entity_id: entity_id.into(),
            kind,
            rev_after: None,
            card_id: None,
            payload: Value::Object(Default::default()),
        }
    }

    pub fn rev(mut self, rev: i64) -> Self {
        self.rev_after = Some(rev);
        self
    }

    pub fn card(mut self, card_id: impl Into<String>) -> Self {
        self.card_id = Some(card_id.into());
        self
    }

    pub fn card_opt(mut self, card_id: Option<impl Into<String>>) -> Self {
        self.card_id = card_id.map(Into::into);
        self
    }

    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
}

/// An open `state.db`.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) and migrates `state.db` at `path`. Only the
    /// daemon and the CLI migrate (`recovery.md` R11); hooks use
    /// [`Store::open_current`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut conn = Connection::open(&path)?;
        configure(&conn)?;
        crate::backup::before_migration(&conn, path.as_ref())?;
        migrate::up(&mut conn)?;
        Ok(Self { conn })
    }

    /// Opens `state.db` without migrating. Refuses a database whose schema is
    /// not exactly this binary's, so a hook never writes across versions.
    pub fn open_current(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        configure(&conn)?;
        let version = migrate::current_version(&conn)?;
        if version != migrate::LATEST {
            return Err(Error::Unavailable(format!(
                "schema {version} is not this tiller's schema {}; the daemon or the CLI migrates it",
                migrate::LATEST
            )));
        }
        Ok(Self { conn })
    }

    /// A migrated in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrate::up(&mut conn)?;
        Ok(Self { conn })
    }

    /// The connection, for reads. Writes go through [`Store::write`].
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Whether the caller's idempotency key is already recorded, so a write
    /// with it would replay. Lets a mutation skip slow work it only needs
    /// when it applies.
    pub fn key_recorded(&self, ctx: &Ctx) -> Result<bool> {
        match ctx.key.as_deref() {
            Some(key) => Ok(lookup_key(&self.conn, key, ctx.now)?.is_some()),
            None => Ok(false),
        }
    }

    /// Runs one mutation named `txn` in one `BEGIN IMMEDIATE` transaction.
    ///
    /// With `key`, a key already stored for the same mutation returns the
    /// stored response and writes nothing; a key stored for another mutation
    /// is refused. A mutation that writes nothing at all (a natural transition
    /// already in its target state) returns `replayed: true`.
    pub fn write<T, F>(
        &mut self,
        ctx: &Ctx,
        txn: &'static str,
        key: Option<&str>,
        f: F,
    ) -> Result<Applied<T>>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(&mut Tx<'_>) -> Result<T>,
    {
        self.write_as(ctx, txn, txn, key, f)
    }

    /// [`Store::write`] for a command whose response differs from that of the
    /// mutation it opens with: `command` names it in `idempotency_keys`, so
    /// its key never replays as a plain `txn` (or the other way round).
    pub fn write_as<T, F>(
        &mut self,
        ctx: &Ctx,
        txn: &'static str,
        command: &'static str,
        key: Option<&str>,
        f: F,
    ) -> Result<Applied<T>>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce(&mut Tx<'_>) -> Result<T>,
    {
        let key = key.or(ctx.key.as_deref());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(key) = key
            && let Some(stored) = lookup_key(&tx, key, ctx.now)?
        {
            if stored.command != command {
                return Err(Error::refused(
                    "idempotency key",
                    key,
                    format!("key was used for {}, not {command}", stored.command),
                ));
            }
            let response: StoredResponse<T> = serde_json::from_value(stored.response)?;
            return Ok(Applied {
                value: response.value,
                events: response.events,
                replayed: true,
            });
        }

        let before = total_changes(&tx)?;
        let mut work = Tx {
            tx,
            ctx,
            txn,
            key,
            events: Vec::new(),
            first_entity: None,
            rebuildable: false,
        };
        let value = f(&mut work)?;
        let Tx {
            tx,
            events,
            first_entity,
            rebuildable,
            ..
        } = work;
        let changed = total_changes(&tx)? - before;

        if events.is_empty() && !(rebuildable && changed > 0) {
            if changed > 0 {
                return Err(Error::Internal(format!(
                    "mutation {txn} changed {changed} rows without writing its event"
                )));
            }
            // A natural no-op: nothing to commit, nothing to remember.
            return Ok(Applied {
                value,
                events,
                replayed: true,
            });
        }

        if let Some(key) = key {
            let response = serde_json::json!({ "value": &value, "events": &events }).to_string();
            let (entity, entity_id) = first_entity.unwrap_or_default();
            tx.execute(
                "INSERT INTO idempotency_keys (key, command, entity, entity_id, response, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![key, command, entity, entity_id, response, ctx.now],
            )?;
        }

        #[cfg(test)]
        crate::state::testing::pause_before_commit(txn);

        tx.commit()?;
        Ok(Applied {
            value,
            events,
            replayed: false,
        })
    }
}

/// A mutation's open transaction.
pub struct Tx<'c> {
    tx: rusqlite::Transaction<'c>,
    ctx: &'c Ctx,
    txn: &'static str,
    key: Option<&'c str>,
    events: Vec<i64>,
    first_entity: Option<(String, String)>,
    rebuildable: bool,
}

impl Tx<'_> {
    pub fn conn(&self) -> &Connection {
        &self.tx
    }

    pub fn now(&self) -> Timestamp {
        self.ctx.now
    }

    pub fn actor(&self) -> &Actor {
        &self.ctx.actor
    }

    pub fn txn(&self) -> &'static str {
        self.txn
    }

    pub fn key(&self) -> Option<&str> {
        self.key
    }

    /// The `seq` values written so far in this mutation.
    pub fn events(&self) -> &[i64] {
        &self.events
    }

    /// Declares that this mutation writes rebuildable rows (observations,
    /// caches, `sessions.activity*`) that are not events (`state.md` section
    /// 10), so it may commit without one. Authoritative writes still need
    /// their event.
    pub(crate) fn rebuildable(&mut self) {
        self.rebuildable = true;
    }

    /// Writes one event row in this transaction and returns its `seq`.
    pub fn event(&mut self, mut event: NewEvent) -> Result<i64> {
        if let Some(via_peer) = &self.ctx.via_peer {
            // Payloads are objects; a bare value is kept under `value`.
            if !event.payload.is_object() {
                let value = event.payload.take();
                event.payload = Value::Object(Default::default());
                if !value.is_null() {
                    event.payload["value"] = value;
                }
            }
            event.payload["via_peer"] = via_peer.clone();
        }
        let id = ids::new_ulid();
        self.tx.execute(
            "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, rev_after, card_id, payload, idempotency_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                id,
                self.ctx.now,
                self.txn,
                self.ctx.actor.to_string(),
                event.entity,
                event.entity_id,
                event.kind,
                event.rev_after,
                event.card_id,
                event.payload,
                self.key,
            ],
        )?;
        let seq = self.tx.last_insert_rowid();
        if self.first_entity.is_none() {
            self.first_entity = Some((event.entity.to_owned(), event.entity_id));
        }
        self.events.push(seq);
        Ok(seq)
    }
}

#[derive(Serialize, Deserialize)]
struct StoredResponse<T> {
    value: T,
    events: Vec<i64>,
}

struct StoredKey {
    command: String,
    response: Value,
}

fn lookup_key(conn: &Connection, key: &str, now: Timestamp) -> Result<Option<StoredKey>> {
    let row = conn
        .query_row(
            "SELECT command, response, created_at FROM idempotency_keys WHERE key = ?1",
            [key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Timestamp>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((command, response, created_at)) = row else {
        return Ok(None);
    };
    if created_at.plus_minutes(KEY_TTL_DAYS * 24 * 60) < now {
        // Expired: forget it so the key can be stored again.
        conn.execute("DELETE FROM idempotency_keys WHERE key = ?1", [key])?;
        return Ok(None);
    }
    Ok(Some(StoredKey {
        command,
        response: serde_json::from_str(&response)?,
    }))
}

fn total_changes(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT total_changes()", [], |row| row.get(0))?)
}

/// The connection settings every writer uses (`state.md` section 10).
fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if mode != "wal" && mode != "memory" {
        return Err(Error::Internal(format!(
            "state.db could not switch to WAL mode (journal_mode is {mode})"
        )));
    }
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
    Ok(())
}
