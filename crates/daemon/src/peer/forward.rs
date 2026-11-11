//! Forwarded mutations (`protocol.md` 10.3): the board host signs a 7.4
//! mutation aimed at a paired peer's record as a `mutate` frame, and the
//! peer's `peer serve` verifies it in the one canonical order before the
//! same core function its own board runs applies it.
//!
//! The allowlist ([`Op`]) holds on both hosts: the board host has no route
//! that forwards an op off it, and the peer refuses one it does not know.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tiller_core::domain::{Approval, Attention, Card, PeerKey, PeerScope, Project};
use tiller_core::state::peers::parse_public_key;
use tiller_core::state::{get, resolve_card, resolve_id};
use tiller_core::{Actor, Ctx, Error, Store, Timestamp, ids};

use super::HelloKey;
use super::serve::{Denied, ServeConfig, current_key, refused, skew_ms};
use crate::api::{ApiError, Mutation};

// -- the allowlist ----------------------------------------------------------------

/// A mutation the board may forward to a peer: exactly the 7.4 list.
/// `stage`, `focus` and memory decisions are not on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    CreateCard,
    EditCard,
    Comment,
    Hold,
    ReleaseHold,
    Close,
    Cancel,
    Reopen,
    AddDep,
    RemoveDep,
    Answer,
    Decide,
    AskNote,
    StartOrchestrator,
    RestartStale,
}

impl Op {
    pub const ALL: [Op; 15] = [
        Op::CreateCard,
        Op::EditCard,
        Op::Comment,
        Op::Hold,
        Op::ReleaseHold,
        Op::Close,
        Op::Cancel,
        Op::Reopen,
        Op::AddDep,
        Op::RemoveDep,
        Op::Answer,
        Op::Decide,
        Op::AskNote,
        Op::StartOrchestrator,
        Op::RestartStale,
    ];

    /// The 7.4 endpoint, as a `mutate` frame's `op` names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Op::CreateCard => "POST /api/v1/cards",
            Op::EditCard => "PATCH /api/v1/cards/{id}",
            Op::Comment => "POST /api/v1/cards/{id}/comment",
            Op::Hold => "POST /api/v1/cards/{id}/hold",
            Op::ReleaseHold => "DELETE /api/v1/cards/{id}/hold",
            Op::Close => "POST /api/v1/cards/{id}/close",
            Op::Cancel => "POST /api/v1/cards/{id}/cancel",
            Op::Reopen => "POST /api/v1/cards/{id}/reopen",
            Op::AddDep => "POST /api/v1/cards/{id}/deps",
            Op::RemoveDep => "DELETE /api/v1/cards/{id}/deps",
            Op::Answer => "POST /api/v1/attention/{id}/answer",
            Op::Decide => "POST /api/v1/approvals/{id}/decide",
            Op::AskNote => "POST /api/v1/projects/{id}/asks",
            Op::StartOrchestrator => "POST /api/v1/projects/{id}/orchestrator",
            Op::RestartStale => "POST /api/v1/agents/restart-stale",
        }
    }

    /// The op an endpoint names; `None` for one off the allowlist.
    pub fn named(endpoint: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.as_str() == endpoint)
    }
}

// -- the frame --------------------------------------------------------------------

/// A forwarded mutation (`protocol.md` 10.3 step 2). `sig` is Ed25519 over
/// every other field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mutate {
    pub id: String,
    /// The 7.4 endpoint, as [`Op::as_str`] writes it. A string, so an op
    /// this build does not know is refused rather than unreadable.
    pub op: String,
    /// The card, attention item, approval or project the path names; null for
    /// `POST /api/v1/cards`, whose project is in the body.
    pub target: Option<String>,
    /// The request body, without `host`.
    pub body: Value,
    pub idempotency_key: String,
    pub ts: String,
    pub viewer_host_id: String,
    pub peer_host_id: String,
    pub key_id: String,
    pub sig: String,
}

/// What the board host forwards: a parsed mutation's op, target and body.
#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    pub op: Op,
    pub target: Option<String>,
    pub body: Value,
    pub idempotency_key: String,
}

/// The bytes a `mutate` signature covers: every field but `sig`.
fn mutate_message(frame: &Mutate) -> Vec<u8> {
    json!([
        "tiller-peer/1 mutate",
        frame.id,
        frame.op,
        frame.target,
        frame.body,
        frame.idempotency_key,
        frame.ts,
        frame.viewer_host_id,
        frame.peer_host_id,
        frame.key_id,
    ])
    .to_string()
    .into_bytes()
}

impl Mutate {
    /// A signed `mutate` for the peer whose `meta.host_id` is
    /// `peer_host_id`, at `now`, with a fresh frame id. A retry signs a new
    /// frame with the same `idempotency_key`.
    pub fn signed(signer: &HelloKey<'_>, peer_host_id: &str, order: Order, now: Timestamp) -> Self {
        let mut frame = Mutate {
            id: ids::new_ulid(),
            op: order.op.as_str().to_owned(),
            target: order.target,
            body: order.body,
            idempotency_key: order.idempotency_key,
            ts: now.to_string(),
            viewer_host_id: signer.viewer_host_id.to_owned(),
            peer_host_id: peer_host_id.to_owned(),
            key_id: signer.key_id.to_owned(),
            sig: String::new(),
        };
        frame.sign(signer.key);
        frame
    }

    /// Sets `sig` for `key` over every other field.
    pub(super) fn sign(&mut self, key: &SigningKey) {
        self.sig = BASE64.encode(key.sign(&mutate_message(self)).to_bytes());
    }

    /// True when `sig` is valid for `public_key` (base64, as `peer_keys`
    /// stores it).
    pub fn verifies(&self, public_key: &str) -> bool {
        let Ok(key) = parse_public_key(public_key) else {
            return false;
        };
        let Some(signature) = BASE64
            .decode(&self.sig)
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
        else {
            return false;
        };
        key.verify(&mutate_message(self), &signature).is_ok()
    }
}

// -- the peer's checks ------------------------------------------------------------

/// The `mutate` checks on the peer, in the one canonical order
/// (`protocol.md` 10.3 step 3, `authority.md` section 7): config
/// `federation.accept`, an unrevoked key row (this link's), the signature,
/// `ts` within the skew, this host's `host_id`, scope `act`, the allowlist,
/// the owner marker (G26) and a project this host owns (G25). The op on
/// success; a refusal writes nothing.
pub(super) fn verify(
    store: &Store,
    config: &ServeConfig,
    link: &PeerKey,
    frame: &Mutate,
    now: Timestamp,
) -> Result<Op, Denied> {
    let key = current_key(store, config, &frame.key_id)?;
    if key.id != link.id || key.viewer_host_id != frame.viewer_host_id {
        return refused(
            "unknown_key",
            format!("key {} is not this link's", frame.key_id),
        );
    }
    if !frame.verifies(&key.public_key) {
        return refused("bad_sig", "the mutate signature is not valid".to_owned());
    }
    let skew = skew_ms(&frame.ts, now);
    if u128::from(skew) > config.timing.skew.as_millis() {
        return refused(
            "clock_skew",
            format!("the mutate is {skew} ms from this host's clock"),
        );
    }
    let host = store.this_host()?;
    if frame.peer_host_id != host.id {
        return refused(
            "wrong_host",
            format!(
                "the mutate is for host {}, not {}",
                frame.peer_host_id, host.id
            ),
        );
    }
    if key.scope != PeerScope::Act {
        return refused(
            "read_scope",
            format!(
                "{} is paired with a read scope: view only pairing",
                host.label
            ),
        );
    }
    let Some(op) = Op::named(&frame.op) else {
        return refused(
            "not_allowlisted",
            format!("{} is not forwarded to {}", frame.op, host.label),
        );
    };
    if !owner_marker_names_tiller(&config.home) {
        return refused(
            "owner_marker",
            "the owner marker names another system; mutations are refused".to_owned(),
        );
    }
    if let Some(project) = target_project(store, op, frame)?
        && project.owner_host_id != host.id
    {
        return refused(
            "foreign_project",
            format!("project {} is not owned by {}", project.id, host.label),
        );
    }
    Ok(op)
}

/// The owner marker (`authority.md` G26): `<home>/owner` absent, empty or
/// naming `tiller`, read as the store reads it.
fn owner_marker_names_tiller(home: &Path) -> bool {
    tiller_core::owner::check(home).is_ok()
}

/// The project a frame's target is in, or `None` when the target does not
/// exist: the core function then answers its own 404.
fn target_project(store: &Store, op: Op, frame: &Mutate) -> tiller_core::Result<Option<Project>> {
    let conn = store.conn();
    let target = frame.target.as_deref().unwrap_or_default();
    let project_id = match op {
        Op::CreateCard => frame
            .body
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_owned),
        Op::Answer => match found(resolve_id(conn, "attention", "attention", target))? {
            Some(id) => {
                let item: Attention = get(conn, "attention", &id)?;
                let card: Card = get(conn, "card", &item.card_id)?;
                Some(card.project_id)
            }
            None => None,
        },
        Op::Decide => match found(resolve_id(conn, "approvals", "approval", target))? {
            Some(id) => {
                let approval: Approval = get(conn, "approval", &id)?;
                Some(approval.project_id)
            }
            None => None,
        },
        // The path names the project by its id, as the core calls read it.
        Op::AskNote | Op::StartOrchestrator => Some(target.to_owned()),
        // Every stale session on this host: no project to own.
        Op::RestartStale => None,
        Op::EditCard
        | Op::Comment
        | Op::Hold
        | Op::ReleaseHold
        | Op::Close
        | Op::Cancel
        | Op::Reopen
        | Op::AddDep
        | Op::RemoveDep => found(resolve_card(conn, target))?.map(|card| card.project_id),
    };
    match project_id {
        Some(id) => found(get(conn, "project", &id)),
        None => Ok(None),
    }
}

/// A lookup with "not found" as `None`.
fn found<T>(result: tiller_core::Result<T>) -> tiller_core::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(Error::NotFound { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

// -- applying ---------------------------------------------------------------------

/// Applies a verified `mutate` as this host's own board would, through the
/// same core function, with the forwarded owner's actor `board@<viewer
/// label>` and `via_peer` in every event's payload (`authority.md` section
/// 3). The `result` status and body; the `idempotency_key` table makes a
/// repeat return the stored response.
pub(super) fn apply(
    store: &mut Store,
    config: &ServeConfig,
    key: &PeerKey,
    frame: &Mutate,
    op: Op,
) -> (u16, Value) {
    match applied(store, config, key, frame, op) {
        Ok(response) => match serde_json::to_value(response) {
            Ok(body) => (200, body),
            Err(error) => ApiError::from(Error::Internal(format!(
                "the mutation applied; its response could not be encoded: {error}"
            )))
            .into_result(),
        },
        Err(error) => error.into_result(),
    }
}

fn applied(
    store: &mut Store,
    config: &ServeConfig,
    key: &PeerKey,
    frame: &Mutate,
    op: Op,
) -> Result<crate::dto::MutationResponse, ApiError> {
    let bytes = serde_json::to_vec(&frame.body).map_err(Error::from)?;
    let mutation = Mutation::parse(op, frame.target.clone(), &bytes)?;
    if mutation.host().is_some() {
        // A peer never forwards a frame on (G42).
        return Err(ApiError::from(Error::Usage(
            "a forwarded mutation names no host; a peer serves only its own records".into(),
        )));
    }
    if mutation.key() != frame.idempotency_key {
        return Err(ApiError::from(Error::Usage(
            "the body's idempotency_key is not the frame's".into(),
        )));
    }
    let ctx = Ctx::new(Actor::Peer(key.viewer_label.clone()))
        .with_key(frame.idempotency_key.clone())
        .with_args(frame.idempotency_key.clone(), &normalized_args(frame))
        .with_via_peer(json!({
            "key_id": key.id,
            "viewer_host_id": key.viewer_host_id,
            "viewer_label": key.viewer_label,
            "frame_id": frame.id,
        }));
    mutation.apply_here(store, &ctx, config.orchestrators.as_deref())
}

/// The arguments a forwarded key is bound to (`protocol.md` 6.3 and 10.3),
/// as the board binds a local key (`api::bind_args`): the op, its target and
/// the body. The body is a parsed JSON value, so its key order does not
/// count, and its top-level `rev` is left out: it is a precondition, not an
/// argument, and a retry after the write landed may carry the newer one.
fn normalized_args(frame: &Mutate) -> Value {
    let mut body = frame.body.clone();
    if let Some(fields) = body.as_object_mut() {
        fields.remove("rev");
    }
    json!({
        "op": frame.op,
        "target": frame.target,
        "body": body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_names_itself() {
        for op in Op::ALL {
            assert_eq!(Op::named(op.as_str()), Some(op));
        }
        for refused in [
            "POST /api/v1/projects/{id}/stage",
            "POST /api/v1/agents/{session}/focus",
            "POST /api/v1/memory/proposals/{id}/decide",
            "POST /api/v1/runs",
            "post /api/v1/cards",
        ] {
            assert_eq!(Op::named(refused), None, "{refused}");
        }
    }

    fn frame(target: &str, body: &str) -> Mutate {
        Mutate {
            id: "f-1".into(),
            op: Op::Hold.as_str().into(),
            target: Some(target.into()),
            body: serde_json::from_str(body).unwrap(),
            idempotency_key: "k-1".into(),
            ts: "2031-01-01T00:00:00Z".into(),
            viewer_host_id: "h-viewer".into(),
            peer_host_id: "h-peer".into(),
            key_id: "key-1".into(),
            sig: String::new(),
        }
    }

    /// A retry's `rev` and field order do not change its arguments; its
    /// target and its other fields do.
    #[test]
    fn a_forwarded_key_binds_the_op_target_and_body() {
        let base = normalized_args(&frame("c-1", r#"{"reason": "vendor", "rev": 3}"#));
        let retry = normalized_args(&frame("c-1", r#"{"rev": 4, "reason": "vendor"}"#));
        assert_eq!(base.to_string(), retry.to_string());
        let elsewhere = normalized_args(&frame("c-2", r#"{"reason": "vendor", "rev": 3}"#));
        assert_ne!(base, elsewhere);
        let other = normalized_args(&frame("c-1", r#"{"reason": "other", "rev": 3}"#));
        assert_ne!(base, other);
    }
}
