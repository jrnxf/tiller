//! Settings > Hosts, the pairings (`protocol.md` section 10.1; `state.md`
//! section 8): the hosts this board is paired with, the boards whose keys
//! this host accepted, and the two ends of a link the board may cut.
//!
//! Unpair is `tiller peer remove` started by the board, through the same
//! core path ([`peers::unpair`]): the host row stops being paired, then its
//! key file is deleted. A key file the delete missed is listed until an
//! unpair of the label deletes it. Revoke key is `tiller peer revoke`: the key ends at
//! once and the next frame signed with it is refused. Each side cuts the end
//! it owns. Both need the owner cookie and are never forwarded: a pairing is
//! the owner's, on the host that keeps it.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use tiller_core::domain::PeerKey;
use tiller_core::state::peers;
use utoipa::ToSchema;

use crate::api::{
    ApiResult, AppState, Done, body, detached, guard, key, mutate, not_forwarded, owner_only, read,
    writable,
};
use crate::dto::{ErrorBody, MutationResponse};

/// `DELETE /api/v1/peers/{label}` and `DELETE /api/v1/peer-keys/{id}`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PairingEnd {
    pub idempotency_key: String,
    /// Refused unless absent or this host: a pairing is never forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// A host this board is paired with: `tiller peer list`'s `peers`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PairedPeer {
    pub label: String,
    /// The peer's `meta.host_id`.
    pub peer_host_id: Option<String>,
    /// `read` or `act`.
    pub scope: String,
    /// `SHA256:` and the base64 SHA-256 of the public key this board signs
    /// with; `None` when the key file is missing or does not parse.
    pub fingerprint: Option<String>,
}

/// A board whose key this host accepted: `tiller peer list`'s `accepted`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AcceptedKey {
    pub id: String,
    pub viewer_label: String,
    /// The board host's `meta.host_id`.
    pub viewer_host_id: String,
    /// `read` or `act`.
    pub scope: String,
    pub fingerprint: String,
    pub accepted_at: String,
    /// Set once the key is revoked; a revoked key never comes back.
    pub revoked_at: Option<String>,
    pub rev: i64,
}

/// `GET /api/v1/peers`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PeerList {
    /// By label.
    pub paired: Vec<PairedPeer>,
    /// Oldest first, revoked keys too.
    pub accepted: Vec<AcceptedKey>,
    /// The labels of hosts no longer paired whose key file is still here,
    /// by label: an unpair whose delete failed. Unpair again deletes it.
    pub key_files_left: Vec<String>,
}

fn accepted(key: &PeerKey) -> AcceptedKey {
    AcceptedKey {
        id: key.id.clone(),
        viewer_label: key.viewer_label.clone(),
        viewer_host_id: key.viewer_host_id.clone(),
        scope: key.scope.to_string(),
        fingerprint: peers::fingerprint(&key.public_key),
        accepted_at: key.accepted_at.to_string(),
        revoked_at: key.revoked_at.map(|at| at.to_string()),
        rev: key.rev,
    }
}

#[utoipa::path(get, path = "/api/v1/peers", tag = "reads",
    responses((status = 200, body = PeerList), (status = 403, body = ErrorBody)))]
pub(crate) async fn list(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<PeerList> {
    owner_only(&state, &headers).await?;
    let home = state.0.home.clone();
    let listed = read(&state, move |store| {
        let paired = peers::paired(store, &home)?
            .into_iter()
            .map(|host| PairedPeer {
                label: host.label,
                peer_host_id: host.peer_host_id,
                scope: host
                    .scope
                    .map(|scope| scope.to_string())
                    .unwrap_or_default(),
                fingerprint: host.fingerprint,
            })
            .collect();
        let accepted = store.peer_keys()?.iter().map(accepted).collect();
        let key_files_left = peers::leftover_key_files(store, &home)?;
        Ok(PeerList {
            paired,
            accepted,
            key_files_left,
        })
    })
    .await?;
    Ok(Json(listed))
}

#[utoipa::path(delete, path = "/api/v1/peers/{label}", tag = "mutations", request_body = PairingEnd,
    params(("label" = String, Path, description = "The paired host's label")),
    responses((status = 200, body = MutationResponse, description = "`data` is the host row, no longer paired; a host not paired is a no-op that deletes a key file left behind. `warning` says when the row is unpaired but the key file was not deleted"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "`state.db` is read only")))]
pub(crate) async fn unpair(
    State(state): State<AppState>,
    Path(label): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PairingEnd = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "an unpair").await?;
    // Refused before the lock, so a bad key or a read-only `state.db` never
    // waits behind an ssh pairing.
    let key = key(&request.idempotency_key)?;
    writable(&state)?;
    // Its own task, as Add a host's: a browser that goes away does not drop
    // the lock while the commit and the key file delete still run.
    detached(unpair_alone(state, key, label)).await
}

/// One pairing change at a time, as Add a host holds it: an unpair never
/// races a pairing of the same label for the row or its key file.
async fn unpair_alone(state: AppState, key: String, label: String) -> ApiResult<MutationResponse> {
    let _alone = crate::peer::pair::PAIRING.lock().await;
    let home = state.0.home.clone();
    mutate(&state, key, move |store, ctx, _| {
        let unpaired = peers::unpair(store, ctx, &home, &label)?;
        Ok(Done::of(unpaired.applied)?.with_warning(unpaired.key_file_left))
    })
    .await
}

#[utoipa::path(delete, path = "/api/v1/peer-keys/{id}", tag = "mutations", request_body = PairingEnd,
    params(("id" = String, Path, description = "The accepted key's id")),
    responses((status = 200, body = MutationResponse, description = "`data` is the revoked key; revoking a revoked key returns it"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded")))]
pub(crate) async fn revoke(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: PairingEnd = body(&bytes)?;
    not_forwarded(&state, request.host.as_deref(), "a key revoke").await?;
    mutate(
        &state,
        key(&request.idempotency_key)?,
        move |store, ctx, _| Done::of(store.peer_revoke_key(ctx, &id)?),
    )
    .await
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use serde_json::{Value, json};
    use tiller_core::domain::PeerScope;
    use tiller_core::state::peers::{KeyFile, NewPairing, NewPeerKey};
    use tiller_core::{Actor, Ctx};
    use tiller_panes::testing::Fleet;

    use super::*;
    use crate::api::tests::{
        Answer, Fixture, HOST, app_with, error_of, get as get_at, mutation, owner_cookie, send,
    };
    use crate::assets::Assets;
    use crate::readonly::Damage;

    const PEER: &str = "01JC00000000000000000PEER2";
    const VIEWER: &str = "01JC00000000000000000BRD01";

    fn owner() -> Ctx {
        Ctx::new(Actor::Owner)
    }

    /// `host-b` paired from this board, its key file written; `host-c`'s
    /// board accepted here. Returns the accepted key's id.
    fn paired(fleet: &mut Fleet) -> String {
        paired_again(fleet);
        fleet
            .store
            .peer_accept(
                &owner(),
                true,
                NewPeerKey {
                    viewer_label: "host-c".into(),
                    viewer_host_id: VIEWER.into(),
                    public_key: peers::public_key_text(&peers::generate_key().unwrap()),
                    scope: PeerScope::Read,
                    unattended: false,
                },
            )
            .unwrap()
            .value
            .id
    }

    /// `host-b` paired and its key file written, as `tiller peer add` leaves
    /// them.
    fn paired_again(fleet: &mut Fleet) {
        fleet
            .store
            .peer_pair(
                &owner(),
                NewPairing {
                    label: "host-b".into(),
                    scope: PeerScope::Act,
                    peer_host_id: PEER.into(),
                },
            )
            .unwrap();
        let key = peers::generate_key().unwrap();
        let path = peers::key_path(fleet.dir.path(), "host-b").unwrap();
        peers::write_key_file(&path, &KeyFile::new("host-b", PeerScope::Act, &key)).unwrap();
    }

    async fn list(app: &Router, cookie: Option<&str>) -> Answer {
        let mut request = Request::get("/api/v1/peers")
            .header(header::HOST, HOST)
            .body(Body::empty())
            .unwrap();
        if let Some(cookie) = cookie {
            request
                .headers_mut()
                .insert(header::COOKIE, cookie.parse().unwrap());
        }
        send(app, request).await
    }

    /// How many events of this kind the board wrote.
    fn board_events(fleet: &Fleet, kind: &str) -> i64 {
        fleet
            .store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = ?1 AND actor = 'board'",
                [kind],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn end(method: &str, path: &str, key: &str, headers: &[(&str, &str)]) -> Request<Body> {
        mutation(method, path, json!({"idempotency_key": key}), headers)
    }

    /// The list is the owner's; it shows each end of a link with its key
    /// fingerprint and no secret.
    #[tokio::test]
    async fn the_owner_lists_both_ends_of_each_link() {
        let mut fleet = Fleet::new();
        let key_id = paired(&mut fleet);
        let app = app_with(fleet.dir.path(), Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(fleet.dir.path());

        assert_eq!(list(&app, None).await.status, StatusCode::FORBIDDEN);
        let answer = list(&app, Some(&cookie)).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        assert!(!answer.body.contains("secret"), "{}", answer.body);
        let listed: PeerList = serde_json::from_str(&answer.body).unwrap();
        assert_eq!(listed.paired.len(), 1);
        assert_eq!(listed.paired[0].label, "host-b");
        assert_eq!(listed.paired[0].scope, "act");
        assert_eq!(listed.paired[0].peer_host_id.as_deref(), Some(PEER));
        assert!(
            listed.paired[0]
                .fingerprint
                .as_deref()
                .is_some_and(|print| print.starts_with("SHA256:")),
            "{listed:?}"
        );
        assert_eq!(listed.accepted.len(), 1);
        assert_eq!(listed.accepted[0].id, key_id);
        assert_eq!(listed.accepted[0].viewer_label, "host-c");
        assert_eq!(listed.accepted[0].scope, "read");
        assert_eq!(listed.accepted[0].revoked_at, None);
    }

    /// Unpair runs `tiller peer remove`'s path: the row stops being paired,
    /// the key file goes, and a second click is a no-op. It needs the owner
    /// and is never forwarded.
    #[tokio::test]
    async fn the_board_unpairs_a_host_as_peer_remove_does() {
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];
        let key_file = peers::key_path(&home, "host-b").unwrap();
        let path = "/api/v1/peers/host-b";

        let stranger = send(&app, end("DELETE", path, "k-1", &[])).await;
        assert_eq!(stranger.status, StatusCode::FORBIDDEN, "{}", stranger.body);
        let forwarded = send(
            &app,
            mutation(
                "DELETE",
                path,
                json!({"idempotency_key": "k-2", "host": "host-b"}),
                &headers,
            ),
        )
        .await;
        assert_eq!(
            forwarded.status,
            StatusCode::BAD_GATEWAY,
            "{}",
            forwarded.body
        );
        assert_eq!(error_of(&forwarded)["reason"], "not_allowlisted");
        assert!(key_file.exists(), "a refused unpair keeps the key file");

        let unpaired = send(&app, end("DELETE", path, "k-3", &headers)).await;
        assert_eq!(unpaired.status, StatusCode::OK, "{}", unpaired.body);
        let body: Value = serde_json::from_str(&unpaired.body).unwrap();
        assert!(body["data"]["peer_scope"].is_null(), "{body}");
        assert!(!key_file.exists(), "the key file goes with the pairing");
        assert_eq!(board_events(&fleet, "host.edited"), 1);
        assert!(fleet.store.paired_hosts().unwrap().is_empty());

        let again = send(&app, end("DELETE", path, "k-4", &headers)).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        assert_eq!(
            board_events(&fleet, "host.edited"),
            1,
            "a host not paired is a no-op"
        );
        let unknown = send(&app, end("DELETE", "/api/v1/peers/host-z", "k-5", &headers)).await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
    }

    /// A bad idempotency key is refused at once, not after the pairing an
    /// Add a host holds the lock for.
    #[tokio::test]
    async fn an_unpair_is_checked_before_it_waits_for_a_pairing() {
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];

        let _pairing = crate::peer::pair::PAIRING.lock().await;
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            send(&app, end("DELETE", "/api/v1/peers/host-b", "", &headers)),
        )
        .await
        .expect("an unpair with no key waited for the pairing lock");
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{}", answer.body);
    }

    /// An unpair runs in a task of its own: a client that goes away while
    /// it waits for the lock does not stop it, and so never frees the lock
    /// while the commit and the key file delete still run.
    #[tokio::test]
    async fn an_unpair_outlives_the_request_that_sent_it() {
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];
        let key_file = peers::key_path(&home, "host-b").unwrap();

        let pairing = crate::peer::pair::PAIRING.lock().await;
        let gone = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            send(&app, end("DELETE", "/api/v1/peers/host-b", "k-1", &headers)),
        )
        .await;
        assert!(gone.is_err(), "the unpair did not wait for the pairing");
        drop(pairing);

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while key_file.exists() || !fleet.store.paired_hosts().unwrap().is_empty() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the unpair stopped with its request"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // The lock is free again once the work is done.
        let _again = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::peer::pair::PAIRING.lock(),
        )
        .await
        .expect("the unpair kept the pairing lock");
    }

    /// The row commits first. A key file the delete missed is a success with
    /// a warning, stays listed, and an Unpair of the label deletes it.
    #[tokio::test]
    async fn a_key_file_left_by_an_unpair_is_listed_and_unpair_deletes_it() {
        use std::os::unix::fs::PermissionsExt;
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];
        let key_file = peers::key_path(&home, "host-b").unwrap();
        let dir = key_file.parent().unwrap().to_owned();
        let path = "/api/v1/peers/host-b";

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let unpaired = send(&app, end("DELETE", path, "k-1", &headers)).await;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(unpaired.status, StatusCode::OK, "{}", unpaired.body);
        let body: Value = serde_json::from_str(&unpaired.body).unwrap();
        assert!(body["data"]["peer_scope"].is_null(), "{body}");
        let warning = body["warning"].as_str().unwrap_or_default();
        assert!(
            warning.starts_with("host-b is unpaired, but its key file was not deleted"),
            "{body}"
        );
        assert!(key_file.exists());
        let listed: PeerList = serde_json::from_str(&list(&app, Some(&cookie)).await.body).unwrap();
        assert!(listed.paired.is_empty());
        assert_eq!(listed.key_files_left, ["host-b"]);

        // The board's retry: a new key, a natural no-op on the row.
        let retried = send(&app, end("DELETE", path, "k-2", &headers)).await;
        assert_eq!(retried.status, StatusCode::OK, "{}", retried.body);
        let body: Value = serde_json::from_str(&retried.body).unwrap();
        assert!(body["warning"].is_null(), "{body}");
        assert!(!key_file.exists());
        let listed: PeerList = serde_json::from_str(&list(&app, Some(&cookie)).await.body).unwrap();
        assert!(listed.key_files_left.is_empty());
    }

    /// A retry with the stored key is a replay: it does not delete the key
    /// file a `tiller peer add` of the same label wrote since.
    #[tokio::test]
    async fn a_replayed_unpair_keeps_a_new_pairings_key_file() {
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];
        let key_file = peers::key_path(&home, "host-b").unwrap();
        let path = "/api/v1/peers/host-b";

        let first = send(&app, end("DELETE", path, "k-1", &headers)).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        assert!(!key_file.exists());
        paired_again(&mut fleet);
        let retry = send(&app, end("DELETE", path, "k-1", &headers)).await;
        assert_eq!(retry.status, StatusCode::OK, "{}", retry.body);
        let body: Value = serde_json::from_str(&retry.body).unwrap();
        assert_eq!(body["replayed"], true, "{body}");
        assert!(key_file.exists(), "the replay deleted the new key file");
        assert_eq!(fleet.store.paired_hosts().unwrap().len(), 1);
    }

    /// The unpair runs in its own task, and its key stays bound to the
    /// label: the same key on another label is refused, not replayed.
    #[tokio::test]
    async fn an_unpair_key_stays_bound_to_its_label() {
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];

        let first = send(&app, end("DELETE", "/api/v1/peers/host-b", "k-1", &headers)).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        let other = send(&app, end("DELETE", "/api/v1/peers/host-z", "k-1", &headers)).await;
        assert_eq!(
            other.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            other.body
        );
    }

    /// Revoke key runs `tiller peer revoke`'s path: `peer.revoked`, and a
    /// revoked key stays revoked.
    #[tokio::test]
    async fn the_board_revokes_an_accepted_key_as_peer_revoke_does() {
        let mut fleet = Fleet::new();
        let key_id = paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];
        let path = format!("/api/v1/peer-keys/{key_id}");

        let stranger = send(&app, end("DELETE", &path, "k-1", &[])).await;
        assert_eq!(stranger.status, StatusCode::FORBIDDEN, "{}", stranger.body);
        let forwarded = send(
            &app,
            mutation(
                "DELETE",
                &path,
                json!({"idempotency_key": "k-2", "host": "host-b"}),
                &headers,
            ),
        )
        .await;
        assert_eq!(
            forwarded.status,
            StatusCode::BAD_GATEWAY,
            "{}",
            forwarded.body
        );
        assert_eq!(board_events(&fleet, "peer.revoked"), 0);

        let revoked = send(&app, end("DELETE", &path, "k-3", &headers)).await;
        assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
        let body: Value = serde_json::from_str(&revoked.body).unwrap();
        assert!(body["data"]["revoked_at"].is_string(), "{body}");
        assert_eq!(board_events(&fleet, "peer.revoked"), 1);

        let again = send(&app, end("DELETE", &path, "k-4", &headers)).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        assert_eq!(
            board_events(&fleet, "peer.revoked"),
            1,
            "a revoked key is a no-op"
        );
        let listed: PeerList = serde_json::from_str(&list(&app, Some(&cookie)).await.body).unwrap();
        assert!(listed.accepted[0].revoked_at.is_some());
        // The pairing this board made is the other end, and stays.
        assert_eq!(listed.paired.len(), 1);
    }

    /// The path names a key by id only: a board label or an unknown id is
    /// 404 and revokes nothing, though `tiller peer revoke` takes a label.
    #[tokio::test]
    async fn a_revoke_names_its_key_by_id_only() {
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];

        for (n, path) in ["/api/v1/peer-keys/host-c", "/api/v1/peer-keys/01JKEYZ"]
            .into_iter()
            .enumerate()
        {
            let answer = send(&app, end("DELETE", path, &format!("k-{n}"), &headers)).await;
            assert_eq!(answer.status, StatusCode::NOT_FOUND, "{}", answer.body);
        }
        assert_eq!(board_events(&fleet, "peer.revoked"), 0);
        assert!(fleet.store.peer_keys().unwrap()[0].revoked_at.is_none());
    }

    /// The host label is refused while a peer is paired, since the peer
    /// knows this host by it; after Unpair the rename saves.
    #[tokio::test]
    async fn the_label_renames_only_after_unpair() {
        let mut fleet = Fleet::new();
        paired(&mut fleet);
        let home = fleet.dir.path().to_owned();
        let app = app_with(&home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(&home);
        let headers = [("cookie", cookie.as_str())];
        let rename = |rev: Value, key: &str| {
            mutation(
                "PATCH",
                "/api/v1/host",
                json!({"idempotency_key": key, "rev": rev, "label": "host-a"}),
                &headers,
            )
        };
        let rev = |app: Router| async move {
            let answer = get_at(&app, "/api/v1/host").await;
            serde_json::from_str::<Value>(&answer.body).unwrap()["rev"].clone()
        };

        let refused = send(&app, rename(rev(app.clone()).await, "k-1")).await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );
        let message = error_of(&refused)["message"].as_str().unwrap().to_owned();
        assert!(message.contains("unpair first: host-b knows"), "{message}");

        let unpaired = send(&app, end("DELETE", "/api/v1/peers/host-b", "k-2", &headers)).await;
        assert_eq!(unpaired.status, StatusCode::OK, "{}", unpaired.body);
        let renamed = send(&app, rename(rev(app.clone()).await, "k-3")).await;
        assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    }
}
