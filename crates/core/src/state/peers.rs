//! Pairing for the federated board (ADR 0003; `protocol.md` section 10.1;
//! `state.md` section 8): `peer.accept` and `peer.revoke` on a peer,
//! `peer.pair` and `peer.remove` on the board host, and the Ed25519 key file
//! the board host signs its frames with.

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{SigningKey, VerifyingKey};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::projects::{local_host_id, require_owner};
use super::{find, get, list, required};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// The peer link protocol this build speaks (`protocol.md` 10.2).
pub const PEER_PROTOCOL: &str = "tiller-peer/1";

/// What `tiller peer accept` records on the peer: the board host asking to
/// read it, and to act on it when `scope` is `act`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewPeerKey {
    pub viewer_label: String,
    pub viewer_host_id: String,
    /// Ed25519, base64.
    pub public_key: String,
    pub scope: PeerScope,
}

/// What `tiller peer add` records on the board host once the peer accepted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewPairing {
    pub label: String,
    /// The herdr saved machine; a new row takes the label, an existing row
    /// keeps its own unless this names one.
    pub herdr_machine: Option<String>,
    pub scope: PeerScope,
    /// The peer's `meta.host_id`, from its accept reply.
    pub peer_host_id: String,
}

impl Store {
    /// `peer.accept`, on the peer: records the board host's public key. An
    /// older live key from the same board host is revoked in the same
    /// transaction, so a second `tiller peer add` replaces the first. The
    /// same key, label and scope again is a natural no-op.
    ///
    /// `federation_accept` is config `federation.accept` as the caller read
    /// it; `false` refuses (ADR 0003, owner choice 2).
    pub fn peer_accept(
        &mut self,
        ctx: &Ctx,
        federation_accept: bool,
        new: NewPeerKey,
    ) -> Result<Applied<PeerKey>> {
        require_owner(&ctx.actor, "peer.accept")?;
        if !federation_accept {
            return Err(Error::refused(
                "peer key",
                &new.viewer_label,
                "this host refuses pairing: config federation.accept is false",
            ));
        }
        check_label(&new.viewer_label)?;
        check_host_id("viewer host id", &new.viewer_host_id)?;
        // The key is stored as it is encoded here, so the same key always
        // compares equal however the caller spelled it.
        let new = NewPeerKey {
            public_key: BASE64.encode(parse_public_key(&new.public_key)?.to_bytes()),
            ..new
        };
        self.write(ctx, "peer.accept", None, |tx| {
            if new.viewer_host_id == local_host_id(tx.conn())? {
                return Err(Error::refused(
                    "peer key",
                    &new.viewer_label,
                    "the viewer is this host; a host does not pair with itself",
                ));
            }
            let live = live_key(tx, &new.viewer_host_id)?;
            if let Some(key) = &live
                && key.public_key == new.public_key
                && key.scope == new.scope
                && key.viewer_label == new.viewer_label
            {
                return Ok(key.clone());
            }
            if let Some(key) = live {
                revoke(tx, &key, "replaced")?;
            }
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO peer_keys (id, viewer_label, viewer_host_id, public_key, scope, accepted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    new.viewer_label,
                    new.viewer_host_id,
                    new.public_key,
                    new.scope,
                    tx.now()
                ],
            )?;
            tx.event(
                NewEvent::new("peer_key", &id, "peer.accepted")
                    .rev(1)
                    .payload(json!({
                        "viewer_label": new.viewer_label,
                        "viewer_host_id": new.viewer_host_id,
                        "public_key": new.public_key,
                        "scope": new.scope,
                    })),
            )?;
            get(tx.conn(), "peer key", &id)
        })
    }

    /// `peer.revoke`, on the peer: `viewer` is the board host's label, or a
    /// key id. A key already revoked is a natural no-op (`protocol.md` 6.4).
    pub fn peer_revoke(&mut self, ctx: &Ctx, viewer: &str) -> Result<Applied<PeerKey>> {
        require_owner(&ctx.actor, "peer.revoke")?;
        required("viewer", viewer)?;
        self.write(ctx, "peer.revoke", None, |tx| {
            let key = revoke_target(tx, viewer)?;
            if key.revoked_at.is_some() {
                return Ok(key);
            }
            revoke(tx, &key, "owner")?;
            get(tx.conn(), "peer key", &key.id)
        })
    }

    /// `peer.pair`, on the board host, after the peer accepted and the key
    /// file was written: marks the host row paired, adding it when the label
    /// is new. A label paired to another peer is refused; `tiller peer
    /// remove` it first.
    pub fn peer_pair(&mut self, ctx: &Ctx, new: NewPairing) -> Result<Applied<Host>> {
        require_owner(&ctx.actor, "peer.pair")?;
        check_label(&new.label)?;
        check_host_id("peer host id", &new.peer_host_id)?;
        if let Some(machine) = &new.herdr_machine {
            required("herdr machine", machine)?;
        }
        self.write(ctx, "peer.pair", None, |tx| {
            if new.peer_host_id == local_host_id(tx.conn())? {
                return Err(Error::refused(
                    "host",
                    &new.label,
                    "the peer is this host; a host does not pair with itself",
                ));
            }
            match find::<Host>(tx.conn(), "label = ?1", &[&new.label])? {
                None => insert_pairing(tx, &new),
                Some(host) => update_pairing(tx, host, &new),
            }
        })
    }

    /// `peer.remove`, on the board host: the row stays as the herdr machine
    /// it was, with no scope and no peer host id. The caller deletes the key
    /// file after the commit. A host not paired is a natural no-op
    /// (`protocol.md` 6.4).
    pub fn peer_remove(&mut self, ctx: &Ctx, label: &str) -> Result<Applied<Host>> {
        require_owner(&ctx.actor, "peer.remove")?;
        required("label", label)?;
        self.write(ctx, "peer.remove", None, |tx| {
            let host = find::<Host>(tx.conn(), "label = ?1", &[&label])?
                .ok_or_else(|| Error::not_found("peer", label))?;
            if host.peer_scope.is_none() {
                return Ok(host);
            }
            let rev = host.rev + 1;
            tx.conn().execute(
                "UPDATE hosts SET peer_scope = NULL, peer_host_id = NULL, rev = ?2 WHERE id = ?1",
                params![host.id, rev],
            )?;
            tx.event(
                NewEvent::new("host", &host.id, "host.edited")
                    .rev(rev)
                    .payload(json!({
                        "changed": {"peer_scope": null, "peer_host_id": null},
                        "was": {"peer_scope": host.peer_scope, "peer_host_id": host.peer_host_id},
                    })),
            )?;
            get(tx.conn(), "host", &host.id)
        })
    }

    /// This host's own row.
    pub fn this_host(&self) -> Result<Host> {
        get(self.conn(), "host", &local_host_id(self.conn())?)
    }

    /// The host row with this label, if there is one.
    pub fn host_labelled(&self, label: &str) -> Result<Option<Host>> {
        find(self.conn(), "label = ?1", &[&label])
    }

    /// Every host this board host is paired with, by label.
    pub fn paired_hosts(&self) -> Result<Vec<Host>> {
        list(self.conn(), "peer_scope IS NOT NULL ORDER BY label", &[])
    }

    /// `cache.refresh` of a paired peer's link, on the board host
    /// (`protocol.md` 10.2): `seen` moves `last_seen_at` to the frame's
    /// arrival, and `stale_reason` is the link's (`peer_link_down`,
    /// `peer_refused`, `peer_version`, or none while live). Every call is a
    /// link state change, so every call writes `host.changed`.
    pub fn peer_link(
        &mut self,
        ctx: &Ctx,
        host_id: &str,
        link: &str,
        stale_reason: Option<&str>,
        seen: Option<Timestamp>,
    ) -> Result<Applied<Host>> {
        self.write(ctx, "cache.refresh", None, |tx| {
            tx.rebuildable();
            let host: Host = get(tx.conn(), "host", host_id)?;
            tx.conn().execute(
                "UPDATE hosts SET stale_reason = ?2,
                    last_seen_at = CASE WHEN ?3 IS NULL OR last_seen_at > ?3 THEN last_seen_at ELSE ?3 END
                 WHERE id = ?1",
                params![host.id, stale_reason, seen],
            )?;
            tx.event(
                NewEvent::new("host", &host.id, "host.changed").payload(json!({
                    "link": link,
                    "stale_reason": stale_reason,
                    "was": host.stale_reason,
                })),
            )?;
            get(tx.conn(), "host", &host.id)
        })
    }

    /// Every board host this peer accepted, live or revoked, oldest first.
    pub fn peer_keys(&self) -> Result<Vec<PeerKey>> {
        list(self.conn(), "1 ORDER BY accepted_at, id", &[])
    }
}

fn live_key(tx: &Tx<'_>, viewer_host_id: &str) -> Result<Option<PeerKey>> {
    find(
        tx.conn(),
        "viewer_host_id = ?1 AND revoked_at IS NULL",
        &[&viewer_host_id],
    )
}

/// The key `tiller peer revoke <viewer>` names: the one live key with that
/// label, else the key with that id, else the newest key with that label.
fn revoke_target(tx: &Tx<'_>, viewer: &str) -> Result<PeerKey> {
    let live: Vec<PeerKey> = list(
        tx.conn(),
        "viewer_label = ?1 AND revoked_at IS NULL ORDER BY accepted_at, id",
        &[&viewer],
    )?;
    if live.len() > 1 {
        return Err(Error::NotFound {
            entity: "peer key",
            id: viewer.to_owned(),
            candidates: live.into_iter().map(|key| key.id).collect(),
        });
    }
    if let Some(key) = live.into_iter().next() {
        return Ok(key);
    }
    if let Some(key) = find::<PeerKey>(tx.conn(), "id = ?1", &[&viewer])? {
        return Ok(key);
    }
    find(
        tx.conn(),
        "viewer_label = ?1 ORDER BY accepted_at DESC, id DESC",
        &[&viewer],
    )?
    .ok_or_else(|| Error::not_found("peer key", viewer))
}

fn revoke(tx: &mut Tx<'_>, key: &PeerKey, reason: &str) -> Result<()> {
    let rev = key.rev + 1;
    tx.conn().execute(
        "UPDATE peer_keys SET revoked_at = ?2, rev = ?3 WHERE id = ?1",
        params![key.id, tx.now(), rev],
    )?;
    tx.event(
        NewEvent::new("peer_key", &key.id, "peer.revoked")
            .rev(rev)
            .payload(json!({
                "viewer_label": key.viewer_label,
                "viewer_host_id": key.viewer_host_id,
                "reason": reason,
            })),
    )?;
    Ok(())
}

fn insert_pairing(tx: &mut Tx<'_>, new: &NewPairing) -> Result<Host> {
    let id = ids::new_ulid();
    let machine = new
        .herdr_machine
        .clone()
        .unwrap_or_else(|| new.label.clone());
    tx.conn().execute(
        "INSERT INTO hosts (id, label, kind, herdr_machine, peer_scope, peer_host_id)
         VALUES (?1, ?2, 'remote', ?3, ?4, ?5)",
        params![id, new.label, machine, new.scope, new.peer_host_id],
    )?;
    tx.event(
        NewEvent::new("host", &id, "host.added")
            .rev(1)
            .payload(json!({
                "label": new.label,
                "kind": "remote",
                "herdr_machine": machine,
                "peer_scope": new.scope,
                "peer_host_id": new.peer_host_id,
            })),
    )?;
    get(tx.conn(), "host", &id)
}

fn update_pairing(tx: &mut Tx<'_>, host: Host, new: &NewPairing) -> Result<Host> {
    if host.kind != HostKind::Remote {
        return Err(Error::refused(
            "host",
            &host.id,
            format!(
                "{} is this host; a host does not pair with itself",
                host.label
            ),
        ));
    }
    if host.peer_scope.is_some() && host.peer_host_id.as_deref() != Some(&new.peer_host_id) {
        return Err(Error::refused(
            "host",
            &host.id,
            format!(
                "{} is paired with another peer (host id {}); tiller peer remove {} first",
                host.label,
                host.peer_host_id.as_deref().unwrap_or("unknown"),
                host.label
            ),
        ));
    }
    let mut changed = serde_json::Map::new();
    if host.peer_scope != Some(new.scope) {
        changed.insert("peer_scope".into(), json!(new.scope));
    }
    if host.peer_host_id.as_deref() != Some(&new.peer_host_id) {
        changed.insert("peer_host_id".into(), json!(new.peer_host_id));
    }
    let machine = new
        .herdr_machine
        .clone()
        .or_else(|| host.herdr_machine.clone())
        .unwrap_or_else(|| host.label.clone());
    if host.herdr_machine.as_deref() != Some(&machine) {
        changed.insert("herdr_machine".into(), json!(machine));
    }
    if changed.is_empty() {
        return Ok(host);
    }
    let rev = host.rev + 1;
    tx.conn().execute(
        "UPDATE hosts SET peer_scope = ?2, peer_host_id = ?3, herdr_machine = ?4, rev = ?5
         WHERE id = ?1",
        params![host.id, new.scope, new.peer_host_id, machine, rev],
    )?;
    tx.event(
        NewEvent::new("host", &host.id, "host.edited")
            .rev(rev)
            .payload(json!({"changed": changed})),
    )?;
    get(tx.conn(), "host", &host.id)
}

/// A peer label names a key file, a config table (`peers.<label>`) and a
/// word on the peer's command line, so it is plain: a letter or digit, then
/// letters, digits, `_` or `-`, at most 64 in all. No `.`, which would split
/// the config key.
pub fn check_label(label: &str) -> Result<()> {
    let mut chars = label.chars();
    let plain = chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        && label.len() <= 64;
    if plain {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "a peer label is a letter or digit, then letters, digits, '_' or '-' (at most 64): {label:?}"
        )))
    }
}

/// A host id is the other host's `meta.host_id`, a ULID. It reaches the
/// owner's confirmation prompt, so nothing else gets through.
pub fn check_host_id(what: &str, id: &str) -> Result<()> {
    if ids::is_ulid(id) {
        Ok(())
    } else {
        Err(Error::Usage(format!("{what} is not a ULID: {id:?}")))
    }
}

// -- keys ------------------------------------------------------------------

/// A fresh Ed25519 signing key from the operating system's random source.
pub fn generate_key() -> Result<SigningKey> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed)
        .map_err(|error| Error::Internal(format!("no random source for a key: {error}")))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The public half as `peer_keys.public_key` and the accept command carry it.
pub fn public_key_text(key: &SigningKey) -> String {
    BASE64.encode(key.verifying_key().to_bytes())
}

/// Parses a base64 Ed25519 public key; anything else is a usage error.
pub fn parse_public_key(text: &str) -> Result<VerifyingKey> {
    let bad = |why: String| Error::Usage(format!("not an Ed25519 public key: {why}"));
    let bytes = BASE64
        .decode(text.trim())
        .map_err(|error| bad(error.to_string()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| bad(format!("{} bytes, not 32", bytes.len())))?;
    VerifyingKey::from_bytes(&bytes).map_err(|error| bad(error.to_string()))
}

/// `~/.tiller/peers/<label>.key`: the board host's private half for one
/// pairing, and what its frames need to name it. Never in `state.db` or a
/// log; `Debug` leaves the secret out. `tiller peer add` writes it before
/// the peer accepts, then again with the peer's answer.
#[derive(Clone, Serialize, Deserialize)]
pub struct KeyFile {
    pub v: u32,
    pub label: String,
    /// The peer's `meta.host_id`, once it accepted.
    pub peer_host_id: Option<String>,
    /// The peer's `peer_keys.id`, the `key_id` of every frame, once it
    /// accepted.
    pub key_id: Option<String>,
    pub scope: PeerScope,
    pub public_key: String,
    secret_key: String,
}

impl std::fmt::Debug for KeyFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyFile")
            .field("label", &self.label)
            .field("peer_host_id", &self.peer_host_id)
            .field("key_id", &self.key_id)
            .field("scope", &self.scope)
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

impl KeyFile {
    pub fn new(label: &str, scope: PeerScope, key: &SigningKey) -> Self {
        Self {
            v: 1,
            label: label.to_owned(),
            peer_host_id: None,
            key_id: None,
            scope,
            public_key: public_key_text(key),
            secret_key: BASE64.encode(key.to_bytes()),
        }
    }

    /// The same key, with what the peer's accept answered.
    pub fn accepted(self, peer_host_id: &str, key_id: &str) -> Self {
        Self {
            peer_host_id: Some(peer_host_id.to_owned()),
            key_id: Some(key_id.to_owned()),
            ..self
        }
    }

    /// The signing key, checked against the public half it was saved with.
    pub fn signing_key(&self) -> Result<SigningKey> {
        let bad = |why: &str| Error::Internal(format!("key file for {}: {why}", self.label));
        let bytes = BASE64
            .decode(&self.secret_key)
            .map_err(|_| bad("the secret is not base64"))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| bad("the secret is not 32 bytes"))?;
        let key = SigningKey::from_bytes(&seed);
        if public_key_text(&key) != self.public_key {
            return Err(bad("the secret does not match its public key"));
        }
        Ok(key)
    }
}

/// Where the key file for `label` lives under the Tiller home.
pub fn key_path(home: &Path, label: &str) -> Result<PathBuf> {
    check_label(label)?;
    Ok(home.join("peers").join(format!("{label}.key")))
}

fn io_error(path: &Path, error: std::io::Error) -> Error {
    Error::Internal(format!("key file {}: {error}", path.display()))
}

/// Writes the key file atomically at mode 0600, in a directory at mode 0700:
/// a temporary file created with that mode, synced, then renamed over the
/// path, so no reader sees half a key or a key another user can read.
pub fn write_key_file(path: &Path, key: &KeyFile) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let dir = path
        .parent()
        .ok_or_else(|| Error::Internal(format!("{} has no directory", path.display())))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| io_error(dir, error))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| io_error(dir, error))?;
    let tmp = dir.join(format!(".{}.tmp", ids::new_ulid()));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(serde_json::to_string(key)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        std::fs::File::open(dir)?.sync_all()
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error(path, error));
    }
    Ok(())
}

/// Reads a key file, refusing one that group or others may read.
pub fn read_key_file(path: &Path) -> Result<KeyFile> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|error| io_error(path, error))?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(Error::refused(
            "key file",
            path.display().to_string(),
            "group or others may read it; it must be mode 0600",
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|error| io_error(path, error))?;
    serde_json::from_str(&text)
        .map_err(|error| Error::Internal(format!("key file {}: {error}", path.display())))
}

/// Deletes a key file; one already gone is fine.
pub fn remove_key_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(path, error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::{owner, setup};
    use crate::store::Actor;

    const BOARD_HOST: &str = "01JC0000000000000000B0ARD1";
    const PEER_HOST: &str = "01JC00000000000000000PEER1";

    fn store() -> Store {
        let mut store = Store::open_in_memory().unwrap();
        setup(&mut store);
        store
    }

    fn session(store: &Store) -> Ctx {
        let id: String = store
            .conn()
            .query_row("SELECT id FROM sessions LIMIT 1", [], |row| row.get(0))
            .unwrap();
        Ctx::new(Actor::Session(id))
    }

    fn refused<T: std::fmt::Debug>(result: Result<T>) -> String {
        let error = result.unwrap_err();
        assert_eq!(error.code(), "refused", "{error}");
        error.to_string()
    }

    fn accept(viewer: &str, scope: PeerScope) -> NewPeerKey {
        NewPeerKey {
            viewer_label: viewer.into(),
            viewer_host_id: BOARD_HOST.into(),
            public_key: public_key_text(&generate_key().unwrap()),
            scope,
        }
    }

    fn pairing(label: &str, peer_host_id: &str) -> NewPairing {
        NewPairing {
            label: label.into(),
            herdr_machine: None,
            scope: PeerScope::Act,
            peer_host_id: peer_host_id.into(),
        }
    }

    fn kinds(store: &Store, events: &[i64]) -> Vec<String> {
        events
            .iter()
            .map(|seq| {
                store
                    .conn()
                    .query_row("SELECT kind FROM events WHERE seq = ?1", [seq], |row| {
                        row.get(0)
                    })
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn accept_records_the_key_and_a_second_accept_revokes_the_first() {
        let mut store = store();
        let first = store
            .peer_accept(&owner(), true, accept("board", PeerScope::Act))
            .unwrap();
        assert_eq!(kinds(&store, &first.events), ["peer.accepted"]);
        assert_eq!(first.value.scope, PeerScope::Act);
        assert!(first.value.revoked_at.is_none());

        let again = NewPeerKey {
            viewer_label: "board".into(),
            viewer_host_id: BOARD_HOST.into(),
            public_key: format!(" {}\n", first.value.public_key),
            scope: PeerScope::Act,
        };
        let replay = store.peer_accept(&owner(), true, again).unwrap();
        assert!(
            replay.replayed,
            "the same key again, however spaced, writes nothing"
        );
        assert_eq!(replay.value.id, first.value.id);

        let second = store
            .peer_accept(&owner(), true, accept("board", PeerScope::Read))
            .unwrap();
        assert_eq!(
            kinds(&store, &second.events),
            ["peer.revoked", "peer.accepted"]
        );
        let keys = store.peer_keys().unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].id, first.value.id);
        assert!(keys[0].revoked_at.is_some());
        assert_eq!(keys[0].rev, 2);
        assert!(keys[1].revoked_at.is_none());
        assert_eq!(keys[1].scope, PeerScope::Read);
    }

    #[test]
    fn accept_refuses_when_federation_accept_is_false() {
        let mut store = store();
        let error = store
            .peer_accept(&owner(), false, accept("board", PeerScope::Act))
            .unwrap_err();
        assert_eq!(error.code(), "refused");
        assert!(error.to_string().contains("federation.accept"), "{error}");
        assert!(store.peer_keys().unwrap().is_empty());
    }

    #[test]
    fn accept_refuses_a_bad_key_this_host_and_a_session() {
        let mut store = store();
        let mut bad = accept("board", PeerScope::Act);
        bad.public_key = "not-a-key".into();
        assert_eq!(
            store.peer_accept(&owner(), true, bad).unwrap_err().code(),
            "usage"
        );
        let mut short = accept("board", PeerScope::Act);
        short.public_key = BASE64.encode([1u8; 16]);
        assert_eq!(
            store.peer_accept(&owner(), true, short).unwrap_err().code(),
            "usage"
        );

        for id in ["", "board", "01JC0000000000000000B0ARD1\n\u{1b}[2J"] {
            let mut odd = accept("board", PeerScope::Act);
            odd.viewer_host_id = id.into();
            assert_eq!(
                store.peer_accept(&owner(), true, odd).unwrap_err().code(),
                "usage",
                "{id:?}"
            );
        }

        let mut me = accept("board", PeerScope::Act);
        me.viewer_host_id = local_host_id(store.conn()).unwrap();
        assert_eq!(
            store.peer_accept(&owner(), true, me).unwrap_err().code(),
            "refused"
        );

        let ctx = session(&store);
        let error = store
            .peer_accept(&ctx, true, accept("board", PeerScope::Act))
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
        assert!(store.peer_keys().unwrap().is_empty());
    }

    #[test]
    fn revoke_by_label_or_id_and_again_is_a_no_op() {
        let mut store = store();
        let key = store
            .peer_accept(&owner(), true, accept("board", PeerScope::Act))
            .unwrap()
            .value;
        let ctx = session(&store);
        assert_eq!(
            store.peer_revoke(&ctx, "board").unwrap_err().code(),
            "forbidden"
        );

        let revoked = store.peer_revoke(&owner(), "board").unwrap();
        assert_eq!(kinds(&store, &revoked.events), ["peer.revoked"]);
        assert!(revoked.value.revoked_at.is_some());
        assert!(store.peer_revoke(&owner(), "board").unwrap().replayed);
        assert!(store.peer_revoke(&owner(), &key.id).unwrap().replayed);

        let next = store
            .peer_accept(&owner(), true, accept("board", PeerScope::Act))
            .unwrap()
            .value;
        let by_id = store.peer_revoke(&owner(), &next.id).unwrap();
        assert_eq!(by_id.value.id, next.id);
        assert!(by_id.value.revoked_at.is_some());
        assert_eq!(
            store.peer_revoke(&owner(), "nobody").unwrap_err().code(),
            "not_found"
        );
    }

    #[test]
    fn pair_adds_or_marks_a_row_and_refuses_another_peer_under_the_label() {
        let mut store = store();
        let added = store
            .peer_pair(&owner(), pairing("host-b", PEER_HOST))
            .unwrap();
        assert_eq!(kinds(&store, &added.events), ["host.added"]);
        assert_eq!(added.value.kind, HostKind::Remote);
        assert_eq!(added.value.herdr_machine.as_deref(), Some("host-b"));
        assert_eq!(added.value.peer_scope, Some(PeerScope::Act));
        assert_eq!(added.value.peer_host_id.as_deref(), Some(PEER_HOST));

        // The same peer again (a second `peer add`) is a no-op, or an edit
        // when the scope changes.
        assert!(
            store
                .peer_pair(&owner(), pairing("host-b", PEER_HOST))
                .unwrap()
                .replayed
        );
        let mut read = pairing("host-b", PEER_HOST);
        read.scope = PeerScope::Read;
        let edited = store.peer_pair(&owner(), read).unwrap();
        assert_eq!(kinds(&store, &edited.events), ["host.edited"]);
        assert_eq!(edited.value.rev, 2);

        // Another peer under the label.
        let other =
            refused(store.peer_pair(&owner(), pairing("host-b", "01JC00000000000000000PEER2")));
        assert!(other.contains("tiller peer remove host-b"), "{other}");

        // The local host's label and its own host id.
        let local = store
            .peer_pair(&owner(), pairing("test-host", "01JC00000000000000000PEER3"))
            .unwrap_err();
        assert_eq!(local.code(), "refused");
        let me = local_host_id(store.conn()).unwrap();
        assert_eq!(
            store
                .peer_pair(&owner(), pairing("host-d", &me))
                .unwrap_err()
                .code(),
            "refused"
        );

        let ctx = session(&store);
        assert_eq!(
            store
                .peer_pair(&ctx, pairing("host-e", "01JC00000000000000000PEER4"))
                .unwrap_err()
                .code(),
            "forbidden"
        );
        assert_eq!(store.paired_hosts().unwrap().len(), 1);
    }

    #[test]
    fn pair_marks_a_herdr_machine_and_remove_leaves_it() {
        let mut store = store();
        store
            .host_add(
                &owner(),
                super::super::projects::NewRemoteHost {
                    label: "host-b".into(),
                    herdr_machine: "machine-b".into(),
                },
            )
            .unwrap();
        let paired = store
            .peer_pair(&owner(), pairing("host-b", PEER_HOST))
            .unwrap();
        assert_eq!(kinds(&store, &paired.events), ["host.edited"]);
        assert_eq!(
            paired.value.herdr_machine.as_deref(),
            Some("machine-b"),
            "a herdr machine keeps its saved machine id"
        );

        let ctx = session(&store);
        assert_eq!(
            store.peer_remove(&ctx, "host-b").unwrap_err().code(),
            "forbidden"
        );
        let removed = store.peer_remove(&owner(), "host-b").unwrap();
        assert_eq!(kinds(&store, &removed.events), ["host.edited"]);
        assert_eq!(removed.value.peer_scope, None);
        assert_eq!(removed.value.peer_host_id, None);
        assert_eq!(removed.value.herdr_machine.as_deref(), Some("machine-b"));
        assert!(store.paired_hosts().unwrap().is_empty());
        assert!(store.peer_remove(&owner(), "host-b").unwrap().replayed);
        assert_eq!(
            store.peer_remove(&owner(), "host-z").unwrap_err().code(),
            "not_found"
        );

        // Removed, the label pairs again, with another peer too.
        store
            .peer_pair(&owner(), pairing("host-b", "01JC00000000000000000PEER2"))
            .unwrap();
    }

    #[test]
    fn labels_are_plain_file_names() {
        for good in ["host-b", "b", "host_b2", "B1"] {
            check_label(good).unwrap();
        }
        for bad in [
            "",
            "-b",
            ".b",
            "a.b",
            "x.ssh",
            "../b",
            "a/b",
            "a b",
            "a;b",
            &"x".repeat(65),
        ] {
            assert!(check_label(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn key_file_is_private_atomic_and_round_trips() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(dir.path(), "host-b").unwrap();
        assert_eq!(path, dir.path().join("peers/host-b.key"));
        let key = generate_key().unwrap();
        let file = KeyFile::new("host-b", PeerScope::Act, &key);
        write_key_file(&path, &file).unwrap();
        assert_eq!(read_key_file(&path).unwrap().key_id, None);
        write_key_file(
            &path,
            &file.accepted(PEER_HOST, "01JC00000000000000000KEY1"),
        )
        .unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["host-b.key"], "no temporary file is left");

        let read = read_key_file(&path).unwrap();
        assert_eq!(read.key_id.as_deref(), Some("01JC00000000000000000KEY1"));
        assert_eq!(read.peer_host_id.as_deref(), Some(PEER_HOST));
        assert_eq!(read.signing_key().unwrap().to_bytes(), key.to_bytes());
        assert!(!format!("{read:?}").contains(&read.secret_key));
        parse_public_key(&read.public_key).unwrap();

        // A second write replaces the first.
        let other = generate_key().unwrap();
        write_key_file(
            &path,
            &KeyFile::new("host-b", PeerScope::Read, &other)
                .accepted(PEER_HOST, "01JC00000000000000000KEY2"),
        )
        .unwrap();
        assert_eq!(
            read_key_file(&path).unwrap().key_id.as_deref(),
            Some("01JC00000000000000000KEY2")
        );
        assert_eq!(mode(&path), 0o600);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read_key_file(&path).unwrap_err().code(), "refused");

        remove_key_file(&path).unwrap();
        assert!(!path.exists());
        remove_key_file(&path).unwrap();
        assert!(key_path(dir.path(), "../x").is_err());
    }
}
