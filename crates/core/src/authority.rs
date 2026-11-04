//! Unauthorized-action detection (`authority.md` section 11): checks of the
//! event log that cost no tokens and run every reconcile cycle. Each finding
//! writes a `guard.breach` event, which Safety counts as an unauthorized
//! action (the cutover's rollback trigger), and opens a `recovery` item with
//! its evidence, in one transaction.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::domain::{AttentionKind, CardState, PeerKey, PeerScope};
use crate::error::Result;
use crate::ids::HOST_HEALTH_CARD;
use crate::state::attention::{NewAttention, open_attention};
use crate::state::{find, get};
use crate::store::{Applied, Ctx, NewEvent, Store};
use crate::time::Timestamp;

/// The guard a decision by the wrong principal breaches: only the owner
/// decides an approval, or a grant (G04).
pub const DECISION_GUARD: &str = "G04";

/// An `approval.decided` event written by a principal that may not decide.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WrongPrincipal {
    pub event_id: String,
    pub seq: i64,
    pub at: Timestamp,
    pub actor: String,
    pub approval_id: String,
    pub card_id: Option<String>,
    pub via_peer: Option<Value>,
    /// Why the actor may not decide, in words.
    pub why: String,
}

/// Every `approval.decided` event not yet raised whose `actor` is not
/// `owner`, `board`, `board@<label>` whose `payload.via_peer.key_id` names an
/// `act` key that was unrevoked when the event was written, or `daemon` with
/// a grant id. Oldest first.
pub fn decisions_by_wrong_principal(conn: &Connection) -> Result<Vec<WrongPrincipal>> {
    // `owner`, `board` and a grant's `daemon` decisions are settled here, so
    // the rows read are the forwarded ones and the suspects.
    let mut stmt = conn.prepare(
        "SELECT e.id, e.seq, e.at, e.actor, e.entity_id, e.card_id, json_extract(e.payload, '$.via_peer')
         FROM events e
         WHERE e.kind = 'approval.decided'
           AND e.actor NOT IN ('owner', 'board')
           AND NOT (e.actor = 'daemon'
                    AND substr(coalesce(json_extract(e.payload, '$.decided_by'), ''), 1, 6) = 'grant:'
                    AND length(coalesce(json_extract(e.payload, '$.decided_by'), '')) > 6)
           AND NOT EXISTS (
             SELECT 1 FROM events b
             WHERE b.entity = 'guard' AND b.entity_id = ?1 AND b.kind = 'guard.breached'
               AND json_extract(b.payload, '$.event_id') = e.id)
         ORDER BY e.seq",
    )?;
    let rows = stmt.query_map(params![DECISION_GUARD], |row| {
        let via_peer: Option<String> = row.get(6)?;
        Ok((
            WrongPrincipal {
                event_id: row.get(0)?,
                seq: row.get(1)?,
                at: row.get(2)?,
                actor: row.get(3)?,
                approval_id: row.get(4)?,
                card_id: row.get(5)?,
                via_peer: None,
                why: String::new(),
            },
            via_peer,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (mut event, via_peer) = row?;
        event.via_peer = via_peer
            .map(|text| serde_json::from_str(&text))
            .transpose()?;
        if let Some(why) = why_not(conn, &event)? {
            event.why = why;
            found.push(event);
        }
    }
    Ok(found)
}

/// Why the event's actor may not decide, or `None` when it may.
fn why_not(conn: &Connection, event: &WrongPrincipal) -> Result<Option<String>> {
    let actor = event.actor.as_str();
    if actor == "daemon" {
        return Ok(Some("daemon decided with no grant id".into()));
    }
    let Some(label) = actor
        .strip_prefix("board@")
        .filter(|label| !label.is_empty())
    else {
        return Ok(Some(format!("{actor} may not decide an approval")));
    };
    let Some(key_id) = event
        .via_peer
        .as_ref()
        .and_then(|via| via.get("key_id"))
        .and_then(Value::as_str)
    else {
        return Ok(Some(format!(
            "board@{label} decided with no via_peer.key_id"
        )));
    };
    let Some(key) = find::<PeerKey>(conn, "id = ?1", &[&key_id])? else {
        return Ok(Some(format!(
            "via_peer.key_id {key_id} names no key of this host"
        )));
    };
    if key.scope != PeerScope::Act {
        return Ok(Some(format!(
            "key {key_id} has {} scope, not act",
            key.scope
        )));
    }
    if key.accepted_at > event.at {
        return Ok(Some(format!(
            "key {key_id} was accepted at {}, after the decision",
            key.accepted_at
        )));
    }
    Ok(key
        .revoked_at
        .filter(|revoked| *revoked <= event.at)
        .map(|revoked| format!("key {key_id} was revoked at {revoked}, before the decision")))
}

impl Store {
    /// The detector for decisions by the wrong principal: for each event
    /// [`decisions_by_wrong_principal`] finds, a `guard.breach` of
    /// [`DECISION_GUARD`] and a `recovery` item on the approval's card, or on
    /// `fleet-1` when the card is gone or closed. An event is raised once.
    /// Writes nothing when nothing is found.
    pub fn raise_wrong_principal_decisions(
        &mut self,
        ctx: &Ctx,
    ) -> Result<Applied<Vec<WrongPrincipal>>> {
        if decisions_by_wrong_principal(self.conn())?.is_empty() {
            return Ok(Applied {
                value: Vec::new(),
                events: Vec::new(),
                replayed: false,
            });
        }
        self.write(ctx, "guard.breach", None, |tx| {
            let found = decisions_by_wrong_principal(tx.conn())?;
            for event in &found {
                tx.event(
                    NewEvent::new("guard", DECISION_GUARD, "guard.breached")
                        .card_opt(event.card_id.clone())
                        .payload(json!({
                            "rule": "decision_by_wrong_principal",
                            "event_id": event.event_id,
                            "seq": event.seq,
                            "at": event.at,
                            "actor": event.actor,
                            "approval_id": event.approval_id,
                            "via_peer": event.via_peer,
                            "why": event.why,
                        })),
                )?;
                let card = match &event.card_id {
                    Some(id) => find::<crate::domain::Card>(tx.conn(), "id = ?1", &[id])?
                        .filter(|card| card.state == CardState::Open)
                        .map(|card| card.id),
                    None => None,
                };
                let card = match card {
                    Some(card) => card,
                    None => {
                        find::<crate::domain::Card>(tx.conn(), "display_id = ?1", &[&HOST_HEALTH_CARD])?
                            .map(|card| card.id)
                            .unwrap_or_else(|| HOST_HEALTH_CARD.to_owned())
                    }
                };
                let approval = get::<crate::domain::Approval>(tx.conn(), "approval", &event.approval_id).ok();
                open_attention(
                    tx,
                    &NewAttention {
                        card,
                        kind: AttentionKind::Recovery,
                        question: format!(
                            "Approval {} ({}) was decided at {} by {}, which is not the owner or a grant: {}. This is an unauthorized action, and it raises the cutover's rollback trigger. Was it yours?",
                            event.approval_id,
                            approval.as_ref().map_or_else(|| "unknown kind".to_owned(), |a| a.kind.to_string()),
                            event.at,
                            event.actor,
                            event.why
                        ),
                        evidence: Some(
                            json!({
                                "event_id": event.event_id,
                                "seq": event.seq,
                                "actor": event.actor,
                                "approval_id": event.approval_id,
                                "approval_state": approval.as_ref().map(|a| a.state),
                                "via_peer": event.via_peer,
                            })
                            .to_string(),
                        ),
                        recommendation: Some(
                            "Read the event and the approval. If it was not yours, revoke the key or grant that made it and stop the fleet before the approval executes.".into(),
                        ),
                        ..NewAttention::default()
                    },
                )?;
            }
            Ok(found)
        })
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;
    use serde_json::{Value, json};

    use super::*;
    use crate::domain::{ApprovalAction, ApprovalKind, ApprovalState, CardKind, GrantKind};
    use crate::ids;
    use crate::state::approvals::{NewApproval, NewGrant, NewPr};
    use crate::state::peers::{NewPeerKey, generate_key, public_key_text};
    use crate::state::tests::{accept, card, owner, requested_merge, setup};
    use crate::store::Actor;

    const BOARD_HOST: &str = "01JC0000000000000000B0ARD1";
    const OTHER_BOARD_HOST: &str = "01JC0000000000000000B0ARD2";

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn accept_key(
        store: &mut Store,
        when: &str,
        label: &str,
        host: &str,
        scope: PeerScope,
    ) -> String {
        store
            .peer_accept(
                &Ctx::at(Actor::Owner, at(when)),
                true,
                NewPeerKey {
                    viewer_label: label.into(),
                    viewer_host_id: host.into(),
                    public_key: public_key_text(&generate_key().unwrap()),
                    scope,
                },
            )
            .unwrap()
            .value
            .id
    }

    /// An `approval.decided` event as a writer of `state.db` would leave it,
    /// whether or not a mutation allows that writer.
    fn decided(store: &Store, when: &str, actor: &str, payload: Value) -> String {
        let id = ids::new_ulid();
        store
            .conn()
            .execute(
                "INSERT INTO events (id, at, txn, actor, entity, entity_id, kind, payload)
                 VALUES (?1, ?2, 'approval.decide', ?3, 'approval', ?4, 'approval.decided', ?5)",
                params![id, at(when), actor, ids::new_ulid(), payload],
            )
            .unwrap();
        id
    }

    fn breaches(store: &Store) -> Vec<Value> {
        let mut stmt = store
            .conn()
            .prepare("SELECT payload FROM events WHERE kind = 'guard.breached' AND entity_id = 'G04' ORDER BY seq")
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|payload| serde_json::from_str(&payload.unwrap()).unwrap())
            .collect()
    }

    fn recovery_items(store: &Store) -> i64 {
        store
            .conn()
            .query_row(
                "SELECT count(*) FROM attention WHERE kind = 'recovery'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn decision_by_wrong_principal_raises_trigger() {
        let mut store = Store::open_in_memory().unwrap();
        let session = setup(&mut store);
        let act = accept_key(
            &mut store,
            "2026-11-02T10:00:00.000Z",
            "board-a",
            BOARD_HOST,
            PeerScope::Act,
        );
        let read = accept_key(
            &mut store,
            "2026-11-02T10:00:00.000Z",
            "board-b",
            OTHER_BOARD_HOST,
            PeerScope::Read,
        );
        let via = |key: &str| json!({"via_peer": {"key_id": key, "viewer_host_id": BOARD_HOST, "viewer_label": "board-a"}});

        // The principals that may decide.
        decided(
            &store,
            "2026-11-02T11:00:00.000Z",
            "owner",
            json!({"accepted": true, "decided_by": "owner"}),
        );
        decided(
            &store,
            "2026-11-02T11:00:00.000Z",
            "board",
            json!({"accepted": true, "decided_by": "owner"}),
        );
        decided(
            &store,
            "2026-11-02T11:00:00.000Z",
            "daemon",
            json!({"accepted": true, "decided_by": "grant:01JC00000000000000000GRANT"}),
        );
        decided(
            &store,
            "2026-11-02T11:00:00.000Z",
            "board@board-a",
            via(&act),
        );
        assert_eq!(
            decisions_by_wrong_principal(store.conn()).unwrap(),
            Vec::new()
        );

        // Those that may not.
        let wrong = [
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                &format!("session:{session}"),
                json!({"accepted": true}),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "hook",
                json!({"accepted": true}),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "mallory",
                json!({"accepted": true}),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "daemon",
                json!({"accepted": true}),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "daemon",
                json!({"accepted": true, "decided_by": "grant:"}),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "board@board-a",
                json!({"accepted": true}),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "board@board-a",
                via("01JC000000000000000000NONE"),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "board@board-b",
                via(&read),
            ),
            decided(
                &store,
                "2026-11-02T09:00:00.000Z",
                "board@board-a",
                via(&act),
            ),
        ];

        // A second key from the same board host revokes the first: a
        // decision under the first key before then stands, one after does
        // not.
        let next = accept_key(
            &mut store,
            "2026-11-02T12:00:00.000Z",
            "board-a",
            BOARD_HOST,
            PeerScope::Act,
        );
        decided(
            &store,
            "2026-11-02T11:59:59.999Z",
            "board@board-a",
            via(&act),
        );
        decided(
            &store,
            "2026-11-02T12:00:01.000Z",
            "board@board-a",
            via(&next),
        );
        let revoked = decided(
            &store,
            "2026-11-02T12:00:00.000Z",
            "board@board-a",
            via(&act),
        );
        let wrong: Vec<String> = wrong.into_iter().chain([revoked]).collect();

        let found = decisions_by_wrong_principal(store.conn()).unwrap();
        assert_eq!(
            found.iter().map(|f| f.event_id.clone()).collect::<Vec<_>>(),
            wrong
        );
        let whys: Vec<&str> = found.iter().map(|f| f.why.as_str()).collect();
        assert_eq!(
            whys,
            [
                format!("session:{session} may not decide an approval").as_str(),
                "hook may not decide an approval",
                "mallory may not decide an approval",
                "daemon decided with no grant id",
                "daemon decided with no grant id",
                "board@board-a decided with no via_peer.key_id",
                "via_peer.key_id 01JC000000000000000000NONE names no key of this host",
                format!("key {read} has read scope, not act").as_str(),
                format!("key {act} was accepted at 2026-11-02T10:00:00.000Z, after the decision")
                    .as_str(),
                format!("key {act} was revoked at 2026-11-02T12:00:00.000Z, before the decision")
                    .as_str(),
            ]
        );

        // Raising writes one breach and one recovery item per event, once.
        let ctx = Ctx::at(Actor::Daemon, at("2026-11-02T13:00:00.000Z"));
        let raised = store.raise_wrong_principal_decisions(&ctx).unwrap();
        assert_eq!(raised.value, found);
        let written = breaches(&store);
        assert_eq!(written.len(), wrong.len());
        for (breach, event) in written.iter().zip(&wrong) {
            assert_eq!(breach["event_id"], Value::from(event.clone()));
            assert_eq!(breach["rule"], "decision_by_wrong_principal");
        }
        assert_eq!(recovery_items(&store), wrong.len() as i64);
        // Safety counts every breach but G19 as an unauthorized action.
        let unauthorized: i64 = store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'guard.breached' AND entity_id <> 'G19'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unauthorized, wrong.len() as i64);

        let again = store.raise_wrong_principal_decisions(&ctx).unwrap();
        assert!(again.value.is_empty() && again.events.is_empty());
        assert_eq!(breaches(&store).len(), wrong.len());
        assert_eq!(recovery_items(&store), wrong.len() as i64);
    }

    /// Every way the mutations decide an approval passes the detector: the
    /// owner, the board, a forwarded decision and a grant.
    #[test]
    fn mutation_decisions_are_not_raised() {
        let mut store = Store::open_in_memory().unwrap();
        setup(&mut store);
        let (_, by_owner) = requested_merge(&mut store, 1, "a1b2c3d4e5f6a7b8");
        store
            .approval_decide(&owner(), None, &by_owner.id, accept())
            .unwrap();
        let (_, by_board) = requested_merge(&mut store, 2, "0123456789abcdef");
        store
            .approval_decide(&Ctx::new(Actor::Board), None, &by_board.id, accept())
            .unwrap();
        let key = accept_key(
            &mut store,
            "2026-11-02T10:00:00.000Z",
            "board-a",
            BOARD_HOST,
            PeerScope::Act,
        );
        let (_, by_peer) = requested_merge(&mut store, 3, "fedcba9876543210");
        let forwarded = Ctx::new(Actor::Peer("board-a".into())).with_via_peer(json!({
            "key_id": key,
            "viewer_host_id": BOARD_HOST,
            "viewer_label": "board-a",
            "frame_id": ids::new_ulid(),
        }));
        store
            .approval_decide(&forwarded, None, &by_peer.id, accept())
            .unwrap();

        let grant = store
            .grant_create(
                &owner(),
                NewGrant {
                    project_id: "alpha".into(),
                    kind: GrantKind::Merge,
                    conditions: json!({"v": 1, "actions": ["squash"]}),
                },
            )
            .unwrap()
            .value;
        let card = store
            .card_create(&owner(), None, card(CardKind::Ship, "granted"), None)
            .unwrap()
            .value;
        let pr = store
            .pr_link(
                &owner(),
                NewPr {
                    card: card.id.clone(),
                    run_id: None,
                    number: 4,
                    url: "https://example.com/example/alpha/pull/4".into(),
                    head_sha: Some("1111222233334444".into()),
                    base_ref: Some("main".into()),
                },
            )
            .unwrap()
            .value;
        let by_grant = store
            .approval_request(
                &Ctx::new(Actor::Daemon),
                NewApproval {
                    kind: ApprovalKind::Merge,
                    card: card.id.clone(),
                    run_id: None,
                    pr_id: Some(pr.id),
                    branch: None,
                    head_sha: "1111222233334444".into(),
                    action: ApprovalAction::Squash,
                    memory_paths: None,
                    grant_id: Some(grant.id),
                },
            )
            .unwrap()
            .value;
        assert_eq!(by_grant.state, ApprovalState::Accepted);

        let decided: i64 = store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'approval.decided'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(decided, 4);
        assert_eq!(
            decisions_by_wrong_principal(store.conn()).unwrap(),
            Vec::new()
        );
        let raised = store
            .raise_wrong_principal_decisions(&Ctx::new(Actor::Daemon))
            .unwrap();
        assert!(raised.value.is_empty() && raised.events.is_empty());
    }
}
