//! The role matrix's card scope (`authority.md` section 4), and
//! unauthorized-action detection (section 11): checks of the event log that
//! cost no tokens and run every reconcile cycle. Each finding writes a
//! `guard.breach` event, which Safety counts as an unauthorized action (the
//! cutover's rollback trigger), and opens a `recovery` item with its
//! evidence, in one transaction.

pub mod principal;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::domain::{
    AttentionKind, CardOutcome, CardState, PeerKey, PeerScope, Project, Role, Session,
};
use crate::error::{Error, Result};
use crate::ids::HOST_HEALTH_CARD;
use crate::state::attention::{NewAttention, open_attention};
use crate::state::outbox::live_orchestrator;
use crate::state::{find, get};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store};
use crate::time::Timestamp;

/// A card mutation the role matrix scopes by principal and project
/// (`authority.md` section 4, the rows "Resolve an ask, create or edit a
/// card", "Hold, depend, reprioritize" and "Close a card").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAction {
    /// `ask.resolve`, `ask.link`.
    ResolveAsk,
    /// `card.create`.
    Create,
    /// `card.edit` (a priority is a reprioritize) and `card.reopen`.
    Edit,
    /// `card.hold`, and `hold.release` for any reason but `due`.
    Hold,
    /// `hold.release` of a hold whose date passed.
    ReleaseDueHold,
    /// `dep.add`, `dep.release`.
    Depend,
    /// `card.close`, `card.complete`, `card.cancel`.
    Close(CardOutcome),
}

impl CardAction {
    fn describe(self) -> &'static str {
        match self {
            CardAction::ResolveAsk => "resolve an ask",
            CardAction::Create => "create a card",
            CardAction::Edit => "edit a card",
            CardAction::Hold => "hold a card or release its hold",
            CardAction::ReleaseDueHold => "release a due hold",
            CardAction::Depend => "add or remove a dependency",
            CardAction::Close(_) => "close a card",
        }
    }
}

/// Refuses `actor` the card mutation `action` in `project_id` unless the
/// role matrix allows it. The owner, the board, a peer board and the import
/// keep their full reach. The daemon releases due holds and closes cards
/// `landed`, and `superseded` only on the owner's answer to R07, which
/// `card.close` checks (`state.md` section 7: no other close comes from
/// reconcile). The
/// project's live orchestrator acts in its own project, closes anything but
/// `landed`, and may also create a card in another project of its host.
/// Workers, hooks, the curator and a script do none of these.
pub fn require_card_scope(
    conn: &Connection,
    actor: &Actor,
    project_id: &str,
    action: CardAction,
) -> Result<()> {
    let what = action.describe();
    match actor {
        // `Actor::is_owner`, spelled out so a new principal fails to compile
        // here until its scope is decided.
        Actor::Owner | Actor::Board | Actor::Peer(_) | Actor::Import => Ok(()),
        Actor::Daemon => match action {
            CardAction::ReleaseDueHold
            | CardAction::Close(CardOutcome::Landed | CardOutcome::Superseded) => Ok(()),
            _ => Err(Error::Forbidden(format!(
                "the daemon may not {what}; it releases due holds and closes cards landed, or superseded on the owner's answer"
            ))),
        },
        Actor::Session(id) => {
            let Some(session) = find::<Session>(conn, "id = ?1", &[id])?
                .filter(|session| session.ended_at.is_none())
            else {
                return Err(Error::Forbidden(format!(
                    "session {id} is not a live Tiller session"
                )));
            };
            if session.role != Role::Orchestrator {
                return Err(Error::Forbidden(format!(
                    "{actor} is a {} and may not {what}; report it to the orchestrator",
                    session.role
                )));
            }
            // The same orchestrator `run.start` accepts (G28).
            if live_orchestrator(conn, &session.project_id)?.map(|live| live.id)
                != Some(session.id.clone())
            {
                return Err(Error::Forbidden(format!(
                    "{actor} is not the live orchestrator of {}",
                    session.project_id
                )));
            }
            if action == CardAction::Close(CardOutcome::Landed) {
                return Err(Error::Forbidden(format!(
                    "{actor} may not close a card landed; the daemon does, after it lands the approved change"
                )));
            }
            if session.project_id == project_id {
                return Ok(());
            }
            if action == CardAction::Create {
                let project: Project = get(conn, "project", project_id)?;
                if project.owner_host_id == session.host_id {
                    return Ok(());
                }
                return Err(Error::Forbidden(format!(
                    "{actor} may not create a card in {project_id}: another host owns it"
                )));
            }
            Err(Error::Forbidden(format!(
                "{actor} is the orchestrator of {} and may not {what} in {project_id}; comment on the card for its orchestrator",
                session.project_id
            )))
        }
        Actor::Hook | Actor::Curator(_) | Actor::Script => {
            Err(Error::Forbidden(format!("{actor} may not {what}")))
        }
    }
}

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
    // the rows read are the forwarded ones and the suspects. A payload that
    // is not JSON reads as one with no fields, so it can neither carry a
    // grant id nor a key.
    let mut stmt = conn.prepare(
        "SELECT e.id, e.seq, e.at, e.actor, e.entity_id, e.card_id,
                CASE WHEN json_valid(e.payload) THEN e.payload -> '$.via_peer' END
         FROM events e
         WHERE e.kind = 'approval.decided'
           AND e.actor NOT IN ('owner', 'board')
           AND NOT (e.actor = 'daemon'
                    AND coalesce(CASE WHEN json_valid(e.payload) THEN json_extract(e.payload, '$.decided_by') END, '')
                        GLOB 'grant:?*')
           AND NOT EXISTS (
             SELECT 1 FROM events b
             WHERE b.entity = 'guard' AND b.entity_id = ?1 AND b.kind = 'guard.breached'
               AND CASE WHEN json_valid(b.payload) THEN json_extract(b.payload, '$.event_id') END = e.id)
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
        // `->` returns JSON text, so this parses whatever `via_peer` holds.
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
    // "When the event was written" is the log's order: the key's
    // `peer.accepted` and `peer.revoked` events against the decision's `seq`,
    // which a shared millisecond cannot blur. The columns stand in for an
    // event the log lacks.
    let (accepted_seq, revoked_seq) = key_events(conn, key_id)?;
    if key.accepted_at > event.at || accepted_seq.is_some_and(|seq| seq > event.seq) {
        return Ok(Some(format!(
            "key {key_id} was accepted at {}, after the decision",
            key.accepted_at
        )));
    }
    let revoked = match revoked_seq {
        Some(seq) => seq < event.seq,
        None => key.revoked_at.is_some_and(|revoked| revoked <= event.at),
    };
    Ok(revoked.then(|| {
        format!(
            "key {key_id} was revoked at {}, before the decision",
            key.revoked_at
                .map_or_else(|| "an unknown time".to_owned(), |at| at.to_string())
        )
    }))
}

/// The `seq` of a key's first `peer.accepted` and first `peer.revoked`
/// events, when the log has them.
fn key_events(conn: &Connection, key_id: &str) -> Result<(Option<i64>, Option<i64>)> {
    Ok(conn.query_row(
        "SELECT (SELECT min(seq) FROM events
                 WHERE entity = 'peer_key' AND entity_id = ?1 AND kind = 'peer.accepted'),
                (SELECT min(seq) FROM events
                 WHERE entity = 'peer_key' AND entity_id = ?1 AND kind = 'peer.revoked')",
        [key_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

impl Store {
    /// The detector for decisions by the wrong principal: for each event
    /// [`decisions_by_wrong_principal`] finds, a `guard.breach` of
    /// [`DECISION_GUARD`] and a `recovery` item on the approval's card, or on
    /// `fleet-1` when the card is gone or closed (the breach alone when
    /// neither is open). An event is raised once. Writes nothing when
    /// nothing is found.
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
                // The approval's card while it is open, else `fleet-1`. With
                // neither, the breach alone still counts.
                let open_card = |reference: &str, column: &str| -> Result<Option<String>> {
                    Ok(find::<crate::domain::Card>(
                        tx.conn(),
                        &format!("{column} = ?1"),
                        &[&reference],
                    )?
                    .filter(|card| card.state == CardState::Open)
                    .map(|card| card.id))
                };
                let card = match &event.card_id {
                    Some(id) => open_card(id, "id")?,
                    None => None,
                };
                let Some(card) = (match card {
                    Some(card) => Some(card),
                    None => open_card(HOST_HEALTH_CARD, "display_id")?,
                }) else {
                    continue;
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
    use rusqlite::{ToSql, params};
    use serde_json::{Value, json};

    use super::*;
    use crate::domain::{
        AnsweredVia, ApprovalAction, ApprovalKind, ApprovalState, AskSource, Boundary, Card,
        CardKind, Delivery, GrantKind, HoldReleaseReason, RunRole,
    };
    use crate::ids;
    use crate::state::approvals::{NewApproval, NewGrant, NewPr};
    use crate::state::asks::{NewAsk, Resolution};
    use crate::state::cards::{CardEdit, MARK_LANDED, NewCard};
    use crate::state::peers::{NewPeerKey, generate_key, public_key_text};
    use crate::state::projects::{Init, NewProject};
    use crate::state::runs::{NewOrchestrator, NewRun};
    use crate::state::tests::{accept, builder_run, card, owner, requested_merge, setup};
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
                    unattended: false,
                },
            )
            .unwrap()
            .value
            .id
    }

    /// An `approval.decided` event as a writer of `state.db` would leave it,
    /// whether or not a mutation allows that writer.
    fn decided(store: &Store, when: &str, actor: &str, payload: impl ToSql) -> String {
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
            // A payload that is not JSON, or a `via_peer` that is not an
            // object, carries no grant id and no key.
            decided(&store, "2026-11-02T11:00:00.000Z", "daemon", "grant:01JC"),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "board@board-a",
                json!({"via_peer": 42}),
            ),
            decided(
                &store,
                "2026-11-02T11:00:00.000Z",
                "board@board-a",
                json!({"via_peer": act}),
            ),
        ];
        decided(&store, "2026-11-02T11:00:00.000Z", "owner", "not json");

        // A second key from the same board host revokes the first. A
        // decision under the first key written before then stands, even in
        // the same millisecond; one written after does not.
        decided(
            &store,
            "2026-11-02T11:59:59.999Z",
            "board@board-a",
            via(&act),
        );
        decided(
            &store,
            "2026-11-02T12:00:00.000Z",
            "board@board-a",
            via(&act),
        );
        let next = accept_key(
            &mut store,
            "2026-11-02T12:00:00.000Z",
            "board-a",
            BOARD_HOST,
            PeerScope::Act,
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
                "daemon decided with no grant id",
                "board@board-a decided with no via_peer.key_id",
                "board@board-a decided with no via_peer.key_id",
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
    /// owner, the board, a forwarded decision, a grant and away mode.
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
        store
            .away_start(
                &owner(),
                json!({"until_at": "2099-01-01T00:00:00.000Z", "projects": ["alpha"]}),
            )
            .unwrap();

        let decided: i64 = store
            .conn()
            .query_row(
                "SELECT count(*) FROM events WHERE kind = 'approval.decided'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(decided, 5);
        assert_eq!(
            decisions_by_wrong_principal(store.conn()).unwrap(),
            Vec::new()
        );
        let raised = store
            .raise_wrong_principal_decisions(&Ctx::new(Actor::Daemon))
            .unwrap();
        assert!(raised.value.is_empty() && raised.events.is_empty());
    }

    /// With neither the approval's card nor `fleet-1` open, the breach is
    /// still written: the trigger never waits on a card.
    #[test]
    fn breach_stands_without_an_open_card() {
        let mut store = Store::open_in_memory().unwrap();
        setup(&mut store);
        store
            .conn()
            .execute(
                "UPDATE cards SET state = 'closed', outcome = 'completed', closed_at = ?1",
                params![at("2026-11-02T10:00:00.000Z")],
            )
            .unwrap();
        let forged = decided(
            &store,
            "2026-11-02T11:00:00.000Z",
            "hook",
            json!({"accepted": true}),
        );
        let raised = store
            .raise_wrong_principal_decisions(&Ctx::at(
                Actor::Daemon,
                at("2026-11-02T12:00:00.000Z"),
            ))
            .unwrap();
        assert_eq!(raised.value.len(), 1);
        assert_eq!(breaches(&store)[0]["event_id"], Value::from(forged));
        assert_eq!(recovery_items(&store), 0);
    }

    /// The role matrix as `authority.md` section 4 writes it.
    const SPEC: &str = include_str!("../../../docs/spec/authority.md");

    /// The matrix's principal columns, in order.
    const COLUMNS: [&str; 8] = [
        "Owner",
        "Daemon",
        "Orchestrator",
        "Scout",
        "Builder",
        "Reviewer",
        "Verifier",
        "Hook, script",
    ];

    /// The rows core scopes by principal and project, and the mutations
    /// each row covers.
    const ROWS: [(&str, &[Probe]); 4] = [
        (
            "Resolve an ask, create or edit a card",
            &[Probe::ResolveAsk, Probe::Create, Probe::Edit, Probe::Reopen],
        ),
        (
            "Hold, depend, reprioritize",
            &[
                Probe::Reprioritize,
                Probe::Hold,
                Probe::ReleaseHold,
                Probe::ReleaseDueHold,
                Probe::DepAdd,
                Probe::DepRelease,
            ],
        ),
        ("Start a run (the execute decision)", &[Probe::StartRun]),
        (
            "Close a card",
            &[Probe::CloseCompleted, Probe::CloseLanded, Probe::Cancel],
        ),
    ];

    /// A cell's scope. A cell this does not know fails the test, so a
    /// change to the table is a change here too.
    #[derive(Clone, Copy, Debug)]
    enum Scope {
        Yes,
        No,
        OwnProject,
        /// Its own project, and a create in another project of its host.
        OwnProjectCreateOnHost,
        OwnProjectNotLanded,
        DueHolds,
        /// Closes `landed`; `state.md` gives reconcile no other close.
        Landed,
    }

    fn scope(cell: &str) -> Scope {
        match cell {
            "yes" => Scope::Yes,
            "no" => Scope::No,
            "own project" | "own project, within capacity, quota and model rules" => {
                Scope::OwnProject
            }
            "own project; also create a card in another project on its host" => {
                Scope::OwnProjectCreateOnHost
            }
            "own project, not `landed`" => Scope::OwnProjectNotLanded,
            "release due holds" => Scope::DueHolds,
            "`landed`; reconcile per state.md" => Scope::Landed,
            other => panic!("authority.md has a cell matrix_matches_spec does not know: {other:?}"),
        }
    }

    /// One mutation, run against a fresh fixture.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Probe {
        ResolveAsk,
        Create,
        Edit,
        Reopen,
        Reprioritize,
        Hold,
        ReleaseHold,
        ReleaseDueHold,
        DepAdd,
        DepRelease,
        StartRun,
        CloseCompleted,
        CloseLanded,
        Cancel,
    }

    impl Probe {
        fn allowed(self, scope: Scope, own: bool) -> bool {
            match scope {
                Scope::Yes => true,
                Scope::No => false,
                Scope::OwnProject => own,
                Scope::OwnProjectCreateOnHost => own || self == Probe::Create,
                Scope::OwnProjectNotLanded => own && self != Probe::CloseLanded,
                Scope::DueHolds => self == Probe::ReleaseDueHold,
                Scope::Landed => self == Probe::CloseLanded,
            }
        }
    }

    /// The principals a column stands for.
    #[derive(Clone, Copy, Debug)]
    enum Who {
        Owner,
        Board,
        Peer,
        Daemon,
        Orchestrator,
        Worker(RunRole),
        Hook,
    }

    fn principals(column: &str, probe: Probe) -> Vec<Who> {
        match column {
            // A peer board has the board's row, but no spawn (section 3).
            "Owner" if probe == Probe::StartRun => vec![Who::Owner, Who::Board],
            "Owner" => vec![Who::Owner, Who::Board, Who::Peer],
            "Daemon" => vec![Who::Daemon],
            "Orchestrator" => vec![Who::Orchestrator],
            "Scout" => vec![Who::Worker(RunRole::Scout)],
            "Builder" => vec![Who::Worker(RunRole::Builder)],
            "Reviewer" => vec![Who::Worker(RunRole::Reviewer)],
            "Verifier" => vec![Who::Worker(RunRole::Verifier)],
            // Core has no script principal: a script reaches `tiller` as
            // the CLI's caller, which the ancestry check refuses first.
            "Hook, script" => vec![Who::Hook],
            other => panic!("no principals for column {other:?}"),
        }
    }

    /// Each row of the matrix, by its action cell, as its other cells.
    fn matrix() -> Vec<(String, Vec<String>)> {
        let cells = |line: &str| -> Vec<String> {
            line.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_owned())
                .collect()
        };
        let section = SPEC
            .split("\n## 4. Role matrix\n")
            .nth(1)
            .expect("authority.md has a section 4, Role matrix");
        let section = section.split("\n## ").next().unwrap();
        let mut rows = section.lines().filter(|line| line.starts_with('|'));
        let header = cells(rows.next().expect("the role matrix has a header"));
        assert_eq!(header[0], "Action");
        assert_eq!(header[1..], COLUMNS);
        rows.next();
        rows.map(cells)
            .map(|mut row| {
                let action = row.remove(0);
                assert_eq!(row.len(), COLUMNS.len(), "the row {action:?}");
                (action, row)
            })
            .collect()
    }

    /// The cards and asks a project's probes act on.
    struct Target {
        ask: String,
        open: Card,
        closed: String,
        held: String,
        waiting: String,
        ops: String,
        landed: String,
    }

    /// Projects `alpha` and `beta` on one host, each with a live
    /// orchestrator and a [`Target`], and one worker run of each role in
    /// `alpha`.
    struct Fixture {
        store: Store,
        orchestrator: String,
        workers: Vec<(RunRole, String)>,
        alpha: Target,
        beta: Target,
    }

    fn fixture(now: Timestamp) -> Fixture {
        let owner = Ctx::at(Actor::Owner, now);
        let mut store = Store::open_in_memory().unwrap();
        store
            .init(
                &owner,
                Init {
                    host_label: "test-host".into(),
                    lane_capacity: 8,
                    ram_floor_mb: None,
                    disk_floor_mb: None,
                    boundary: Boundary::Personal,
                },
            )
            .unwrap();
        let mut orchestrators = Vec::new();
        for id in ["alpha", "beta"] {
            store
                .project_add(
                    &owner,
                    NewProject {
                        id: id.into(),
                        name: id.into(),
                        repo: Some(format!("example/{id}")),
                        remote: None,
                        default_branch: None,
                        delivery: Delivery::DirectPr,
                        checks: None,
                        capacity: Some(8),
                        deploy_workflow: None,
                        deploy_environment: None,
                        deploy_timeout_minutes: None,
                    },
                )
                .unwrap();
            orchestrators.push(
                store
                    .session_start(
                        &owner,
                        NewOrchestrator {
                            project_id: id.into(),
                            claude_session_id: Some(format!("claude-{id}")),
                            ..NewOrchestrator::default()
                        },
                    )
                    .unwrap()
                    .value
                    .session
                    .id,
            );
        }
        let alpha = target(&mut store, &owner, "alpha");
        let beta = target(&mut store, &owner, "beta");
        let work = new_card(&mut store, &owner, "alpha", CardKind::Ship, "work");
        let workers = [
            RunRole::Scout,
            RunRole::Builder,
            RunRole::Reviewer,
            RunRole::Verifier,
        ]
        .into_iter()
        .map(|role| {
            let started = store
                .run_start(
                    &owner,
                    None,
                    NewRun {
                        role,
                        ..builder_run(&work.id)
                    },
                )
                .unwrap()
                .value;
            (role, started.session.id)
        })
        .collect();
        Fixture {
            store,
            orchestrator: orchestrators.remove(0),
            workers,
            alpha,
            beta,
        }
    }

    fn new_card(
        store: &mut Store,
        owner: &Ctx,
        project: &str,
        kind: CardKind,
        title: &str,
    ) -> Card {
        store
            .card_create(
                owner,
                None,
                NewCard {
                    project_id: project.into(),
                    ..card(kind, title)
                },
                None,
            )
            .unwrap()
            .value
    }

    fn target(store: &mut Store, owner: &Ctx, project: &str) -> Target {
        let ask = store
            .ask_record(
                owner,
                None,
                NewAsk {
                    project_id: project.into(),
                    session_id: None,
                    source: AskSource::Script,
                    text: "is the login page slow?".into(),
                },
            )
            .unwrap()
            .value
            .ask
            .id;
        let open = new_card(store, owner, project, CardKind::Ship, "open");
        let closed = new_card(store, owner, project, CardKind::Ship, "closed").id;
        store.card_cancel(owner, &closed, "not needed").unwrap();
        // Due a minute after the fixture; the probes run an hour after.
        let held = new_card(store, owner, project, CardKind::Ship, "held").id;
        store
            .card_hold(
                owner,
                &held,
                owner.now.plus_minutes(1),
                "wait for the release",
            )
            .unwrap();
        let ops = new_card(store, owner, project, CardKind::Ops, "ops").id;
        let waiting = new_card(store, owner, project, CardKind::Ship, "waiting").id;
        store.dep_add(owner, &waiting, &ops).unwrap();
        // A merge outside Tiller the owner marked landed: evidence for
        // `landed`.
        let landed = new_card(store, owner, project, CardKind::Ship, "landed").id;
        let item = store
            .attention_open(
                owner,
                None,
                NewAttention {
                    card: landed.clone(),
                    kind: AttentionKind::Recovery,
                    question: "PR merged outside Tiller. Mark landed or reopen work?".into(),
                    options: Some(json!([MARK_LANDED, "Reopen work"])),
                    ..NewAttention::default()
                },
            )
            .unwrap()
            .value;
        store
            .attention_answer(owner, None, &item.id, MARK_LANDED, AnsweredVia::Board)
            .unwrap();
        Target {
            ask,
            open,
            closed,
            held,
            waiting,
            ops,
            landed,
        }
    }

    fn run(probe: Probe, store: &mut Store, ctx: &Ctx, project: &str, t: &Target) -> Result<()> {
        match probe {
            Probe::ResolveAsk => store
                .ask_resolve(
                    ctx,
                    &t.ask,
                    Resolution::Chat {
                        reason: Some("answered in chat".into()),
                    },
                )
                .map(drop),
            Probe::Create => store
                .card_create(
                    ctx,
                    None,
                    NewCard {
                        project_id: project.into(),
                        ..card(CardKind::Ship, "new")
                    },
                    None,
                )
                .map(drop),
            Probe::Edit | Probe::Reprioritize => {
                let edit = if probe == Probe::Edit {
                    CardEdit {
                        title: Some("renamed".into()),
                        ..CardEdit::default()
                    }
                } else {
                    CardEdit {
                        priority: Some(1),
                        ..CardEdit::default()
                    }
                };
                store
                    .card_edit(ctx, None, &t.open.id, t.open.rev, edit)
                    .map(drop)
            }
            Probe::Reopen => store.card_reopen(ctx, &t.closed, "broke again").map(drop),
            Probe::Hold => store
                .card_hold(
                    ctx,
                    &t.open.id,
                    ctx.now.plus_minutes(60),
                    "after the freeze",
                )
                .map(drop),
            Probe::ReleaseHold => store
                .hold_release(ctx, &t.held, HoldReleaseReason::Removed)
                .map(drop),
            Probe::ReleaseDueHold => store
                .hold_release(ctx, &t.held, HoldReleaseReason::Due)
                .map(drop),
            Probe::DepAdd => store.dep_add(ctx, &t.open.id, &t.ops).map(drop),
            Probe::DepRelease => store.dep_release(ctx, &t.waiting, &t.ops).map(drop),
            Probe::StartRun => store
                .run_start(ctx, None, builder_run(&t.open.id))
                .map(drop),
            Probe::CloseCompleted => store
                .card_close(
                    ctx,
                    &t.ops,
                    CardOutcome::Completed,
                    None,
                    Some("rotated the key"),
                )
                .map(drop),
            Probe::CloseLanded => store
                .card_close(ctx, &t.landed, CardOutcome::Landed, None, None)
                .map(drop),
            Probe::Cancel => store.card_cancel(ctx, &t.open.id, "not needed").map(drop),
        }
    }

    /// `authority.md` section 4, the rows core scopes by project: each
    /// mutation of a row, by each principal of a column, in the
    /// principal's own project and in another project on the same host,
    /// is allowed when the cell allows it and refused `forbidden`
    /// otherwise.
    #[test]
    fn matrix_matches_spec() {
        let matrix = matrix();
        let now = Timestamp::now();
        let later = now.plus_minutes(60);
        // One fixture, copied for each case: building it each time is slow.
        let fx = fixture(now);
        for (action, probes) in ROWS {
            let (_, cells) = matrix
                .iter()
                .find(|(row, _)| row == action)
                .unwrap_or_else(|| panic!("authority.md has no row {action:?}"));
            for (column, cell) in COLUMNS.iter().zip(cells) {
                let scope = scope(cell);
                for &probe in probes {
                    for who in principals(column, probe) {
                        for project in ["alpha", "beta"] {
                            let mut store = fx.store.copy_in_memory().unwrap();
                            let (actor, own) = match who {
                                Who::Owner => (Actor::Owner, false),
                                Who::Board => (Actor::Board, false),
                                Who::Peer => (Actor::Peer("board-a".into()), false),
                                Who::Daemon => (Actor::Daemon, false),
                                Who::Orchestrator => {
                                    (Actor::Session(fx.orchestrator.clone()), project == "alpha")
                                }
                                Who::Worker(role) => {
                                    let (_, session) =
                                        fx.workers.iter().find(|(r, _)| *r == role).unwrap();
                                    (Actor::Session(session.clone()), project == "alpha")
                                }
                                Who::Hook => (Actor::Hook, false),
                            };
                            let ctx = Ctx::at(actor, later);
                            let t = if project == "alpha" {
                                &fx.alpha
                            } else {
                                &fx.beta
                            };
                            let result = run(probe, &mut store, &ctx, project, t);
                            let case =
                                format!("{probe:?} by {who:?} in {project} ({action:?}: {cell:?})");
                            if probe.allowed(scope, own) {
                                assert!(result.is_ok(), "{case} is allowed: {result:?}");
                            } else {
                                let error = result.expect_err(&format!("{case} is refused"));
                                assert_eq!(error.code(), "forbidden", "{case}: {error}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// The cross-project create stops at the host: an orchestrator may not
    /// create a card in a project another host owns.
    #[test]
    fn orchestrator_creates_only_on_its_host() {
        let now = Timestamp::now();
        let mut fx = fixture(now);
        fx.store
            .conn()
            .execute_batch(
                "INSERT INTO hosts (id, label, kind) VALUES ('01JC0000000000000000REM0TE', 'host-b', 'remote');
                 UPDATE projects SET owner_host_id = '01JC0000000000000000REM0TE' WHERE id = 'beta';",
            )
            .unwrap();
        let ctx = Ctx::at(Actor::Session(fx.orchestrator.clone()), now);
        let error = run(Probe::Create, &mut fx.store, &ctx, "beta", &fx.beta).unwrap_err();
        assert_eq!(error.code(), "forbidden");
        assert!(
            error.to_string().contains("another host owns it"),
            "{error}"
        );
    }

    /// A create in another project may not cite that project's ask: the
    /// citation links the ask, and only that project resolves its asks.
    #[test]
    fn cross_project_create_cites_no_ask() {
        let now = Timestamp::now();
        let mut fx = fixture(now);
        let ctx = Ctx::at(Actor::Session(fx.orchestrator.clone()), now);
        let new = || NewCard {
            project_id: "beta".into(),
            ..card(CardKind::Ship, "for beta")
        };
        let error = fx
            .store
            .card_create(&ctx, None, new(), Some(&fx.beta.ask))
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
        assert!(error.to_string().contains("resolve an ask"), "{error}");
        let created = fx.store.card_create(&ctx, None, new(), None).unwrap().value;
        assert_eq!(created.project_id, "beta");
    }

    /// An ask resolved as a follow-up adds to the card, so the card must be
    /// in the orchestrator's own project.
    #[test]
    fn follow_up_stays_in_the_project() {
        let now = Timestamp::now();
        let mut fx = fixture(now);
        let ctx = Ctx::at(Actor::Session(fx.orchestrator.clone()), now);
        let follow_up = |card: &Card| Resolution::Followup {
            card: card.id.clone(),
        };
        let error = fx
            .store
            .ask_resolve(&ctx, &fx.alpha.ask, follow_up(&fx.beta.open))
            .unwrap_err();
        assert_eq!(error.code(), "forbidden");
        assert!(error.to_string().contains("in beta"), "{error}");
        fx.store
            .ask_resolve(&ctx, &fx.alpha.ask, follow_up(&fx.alpha.open))
            .unwrap();
    }
}
