//! Replies: `ask.reply` and `reply.read` (`state.md`, Reply; `protocol.md`
//! 6.4, `tiller reply`). The orchestrator sends its answer to an ask, or to a
//! comment on a card, with `tiller reply`. The board lists the unread ones in
//! the Inbox and shows each on its card's timeline. No hook writes a reply:
//! the board stores only what the orchestrator chose to send.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{get, list, required, resolve_card, resolve_id};
use crate::authority::require_reply_scope;
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::limits::{COMMENT_MAX_CHARS, within};
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// A reply to send.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewReply {
    /// The ask (a ULID or its short form), or the message a comment or an
    /// ask note arrived as.
    pub to: String,
    pub text: String,
    /// The card the reply links to; by default the comment's card, or the
    /// first card the ask created or followed up.
    pub card: Option<String>,
}

/// What a reply answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyTo {
    Ask,
    Comment,
}

impl ReplyTo {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplyTo::Ask => "ask",
            ReplyTo::Comment => "comment",
        }
    }
}

/// A reply with the question it answers and the card it links to: what the
/// Inbox shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplyView {
    pub reply: Reply,
    pub to: ReplyTo,
    /// The owner's words: the ask's text, or the comment without its tag.
    pub question: String,
    pub asked_at: Timestamp,
    pub card: Option<Card>,
    /// How the ask touched the linked card (`created` or `followup`), when
    /// it did.
    pub card_relation: Option<LinkRelation>,
}

/// What `--to` names, resolved.
enum Target {
    Ask(Ask),
    Comment {
        message: OutboxMessage,
        card: Box<Card>,
    },
}

/// Whether a message was written by the owner: the CLI, the board, or a
/// paired board forwarding a comment.
fn by_owner(created_by: &str) -> bool {
    matches!(created_by, "owner" | "board") || created_by.starts_with("board@")
}

fn target(conn: &Connection, to: &str) -> Result<Target> {
    match resolve_id(conn, "asks", "ask", to) {
        Ok(id) => return Ok(Target::Ask(get(conn, "ask", &id)?)),
        Err(Error::NotFound { candidates, .. }) if candidates.is_empty() => {}
        Err(error) => return Err(error),
    }
    let id = match resolve_id(conn, "outbox", "message", to) {
        Ok(id) => id,
        Err(Error::NotFound { candidates, .. }) if candidates.is_empty() => {
            return Err(Error::not_found("ask or comment", to.trim()));
        }
        Err(error) => return Err(error),
    };
    let message: OutboxMessage = get(conn, "message", &id)?;
    if message.kind != OutboxKind::Note {
        return Err(Error::refused(
            "message",
            &message.id,
            format!("a {} message is not an ask or a comment", message.kind),
        ));
    }
    // An ask note (the board's ask box, a script note) answers its ask.
    if let Some(ask) = &message.reply_to {
        return Ok(Target::Ask(get(conn, "ask", ask)?));
    }
    match &message.card_id {
        Some(card) if by_owner(&message.created_by) => {
            let card: Card = get(conn, "card", card)?;
            Ok(Target::Comment {
                message,
                card: Box::new(card),
            })
        }
        _ => Err(Error::refused(
            "message",
            &message.id,
            "the message is not an ask or a comment on a card",
        )),
    }
}

/// The comment's words without the `[alpha-42] ` tag the outbox note adds.
fn comment_text(message: &OutboxMessage, card: Option<&Card>) -> String {
    let prefix = card.map(|card| format!("[{}] ", card.display_id));
    match prefix {
        Some(prefix) => message
            .body
            .strip_prefix(&prefix)
            .unwrap_or(&message.body)
            .to_owned(),
        None => message.body.clone(),
    }
}

fn ask_link(conn: &Connection, ask_id: &str, card_id: &str) -> Result<Option<LinkRelation>> {
    let links: Vec<AskCard> = list(
        conn,
        "ask_id = ?1 AND card_id = ?2 LIMIT 1",
        &[&ask_id, &card_id],
    )?;
    Ok(links.into_iter().next().map(|link| link.relation))
}

/// The card `--card` names, when the reply may link it: a card of the
/// reply's own project, or one the ask already links to (a follow-up in
/// another project). Any other card is refused, so a typo never puts the
/// answer on another project's timeline.
fn chosen_card(
    conn: &Connection,
    reference: &str,
    project_id: &str,
    ask_id: Option<&str>,
) -> Result<String> {
    let card = resolve_card(conn, reference)?;
    if card.project_id == project_id {
        return Ok(card.id);
    }
    if let Some(ask_id) = ask_id
        && ask_link(conn, ask_id, &card.id)?.is_some()
    {
        return Ok(card.id);
    }
    Err(Error::refused(
        "card",
        &card.display_id,
        format!(
            "the card belongs to {}, and the question was asked of {project_id}",
            card.project_id
        ),
    ))
}

/// One reply with its question and card.
pub fn view(conn: &Connection, reply: Reply) -> Result<ReplyView> {
    let card: Option<Card> = match &reply.card_id {
        Some(id) => Some(get(conn, "card", id)?),
        None => None,
    };
    let (to, question, asked_at, card_relation) = match (&reply.ask_id, &reply.message_id) {
        (Some(ask), _) => {
            let ask: Ask = get(conn, "ask", ask)?;
            let relation = match &card {
                Some(card) => ask_link(conn, &ask.id, &card.id)?,
                None => None,
            };
            (ReplyTo::Ask, ask.text, ask.received_at, relation)
        }
        (None, Some(message)) => {
            let message: OutboxMessage = get(conn, "message", message)?;
            let commented: Option<Card> = match &message.card_id {
                Some(id) => Some(get(conn, "card", id)?),
                None => None,
            };
            let text = comment_text(&message, commented.as_ref());
            (ReplyTo::Comment, text, message.created_at, None)
        }
        (None, None) => {
            return Err(Error::Internal(format!(
                "reply {} answers neither an ask nor a comment",
                reply.id
            )));
        }
    };
    Ok(ReplyView {
        reply,
        to,
        question,
        asked_at,
        card,
        card_relation,
    })
}

/// A project's unread replies, newest first.
pub fn unread(conn: &Connection, project_id: &str) -> Result<Vec<ReplyView>> {
    let replies: Vec<Reply> = list(
        conn,
        "project_id = ?1 AND read_at IS NULL ORDER BY replied_at DESC, id DESC",
        &[&project_id],
    )?;
    replies.into_iter().map(|reply| view(conn, reply)).collect()
}

/// The asks a session resolved since `since`, other than `cancelled`, that
/// have no reply yet, oldest first: what the Stop hook's reply check names
/// (`protocol.md` 2.5).
pub fn unreplied_asks(conn: &Connection, session_id: &str, since: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id FROM asks
         WHERE resolved_by_session_id = ?1 AND resolved_at >= ?2 AND disposition <> 'cancelled'
           AND NOT EXISTS (SELECT 1 FROM replies WHERE replies.ask_id = asks.id)
         ORDER BY received_at, id",
    )?;
    let ids = stmt
        .query_map(params![session_id, since], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

fn insert(tx: &mut Tx<'_>, reply: &Reply, to: ReplyTo) -> Result<()> {
    tx.conn().execute(
        "INSERT INTO replies (id, project_id, ask_id, message_id, card_id, session_id, body, replied_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            reply.id,
            reply.project_id,
            reply.ask_id,
            reply.message_id,
            reply.card_id,
            reply.session_id,
            reply.body,
            reply.replied_at,
        ],
    )?;
    tx.event(
        NewEvent::new("reply", &reply.id, "ask.replied")
            .rev(1)
            .card_opt(reply.card_id.as_deref())
            .payload(json!({
                "to": to.as_str(),
                "ask_id": reply.ask_id,
                "message_id": reply.message_id,
                "project_id": reply.project_id,
            })),
    )?;
    Ok(())
}

impl Store {
    /// `ask.reply`. Keyed. The project's live orchestrator answers an ask or
    /// a comment on a card. The reply links to `--card`, or to the comment's
    /// card, or to the first card the ask created or followed up.
    pub fn reply_send(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        new: NewReply,
    ) -> Result<Applied<ReplyView>> {
        required("reply", &new.text)?;
        within("a reply", &new.text, COMMENT_MAX_CHARS)?;
        self.write(ctx, "ask.reply", key, |tx| {
            let target = target(tx.conn(), &new.to)?;
            let (project_id, ask_id, message_id, default_card, to) = match &target {
                Target::Ask(ask) => {
                    let linked: Vec<AskCard> =
                        list(tx.conn(), "ask_id = ?1 ORDER BY linked_at", &[&ask.id])?;
                    let card = linked.into_iter().next().map(|link| link.card_id);
                    (
                        ask.project_id.clone(),
                        Some(ask.id.clone()),
                        None,
                        card,
                        ReplyTo::Ask,
                    )
                }
                Target::Comment { message, card } => (
                    card.project_id.clone(),
                    None,
                    Some(message.id.clone()),
                    Some(card.id.clone()),
                    ReplyTo::Comment,
                ),
            };
            let session = require_reply_scope(tx.conn(), tx.actor(), &project_id)?;
            let card_id = match &new.card {
                Some(card) => Some(chosen_card(
                    tx.conn(),
                    card,
                    &project_id,
                    ask_id.as_deref(),
                )?),
                None => default_card,
            };
            let reply = Reply {
                id: ids::new_ulid(),
                project_id,
                ask_id,
                message_id,
                card_id,
                session_id: session.id,
                body: new.text.clone(),
                replied_at: tx.now(),
                read_at: None,
                rev: 1,
            };
            insert(tx, &reply, to)?;
            view(tx.conn(), get(tx.conn(), "reply", &reply.id)?)
        })
    }

    /// `reply.read`. Natural: the owner marks a reply read, and marking it
    /// again returns it and writes nothing.
    pub fn reply_read(&mut self, ctx: &Ctx, reference: &str) -> Result<Applied<ReplyView>> {
        super::projects::require_owner(&ctx.actor, "reply.read")?;
        self.write(ctx, "reply.read", None, |tx| {
            let id = resolve_id(tx.conn(), "replies", "reply", reference)?;
            let reply: Reply = get(tx.conn(), "reply", &id)?;
            if reply.read_at.is_some() {
                return view(tx.conn(), reply);
            }
            let now: Timestamp = tx.now();
            tx.conn().execute(
                "UPDATE replies SET read_at = ?2, rev = rev + 1 WHERE id = ?1",
                params![reply.id, now],
            )?;
            tx.event(
                NewEvent::new("reply", &reply.id, "reply.read")
                    .rev(reply.rev + 1)
                    .payload(json!({"project_id": reply.project_id})),
            )?;
            view(tx.conn(), get(tx.conn(), "reply", &reply.id)?)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::state::asks::{NewAsk, Resolution};
    use crate::state::cards::NewCard;
    use crate::state::tests::{add_project, card, memory_store, owner};
    use crate::store::Actor;

    const LONG_AGO: &str = "2000-01-01T00:00:00Z";

    fn board_ask(store: &mut Store, text: &str) -> (Ask, OutboxMessage) {
        let recorded = store
            .ask_record(
                &Ctx::new(Actor::Board),
                Some(&format!("ask:{text}")),
                NewAsk {
                    project_id: "alpha".into(),
                    session_id: None,
                    source: AskSource::Board,
                    text: text.into(),
                },
            )
            .unwrap()
            .value;
        (recorded.ask, recorded.message.unwrap())
    }

    fn reply(to: &str, text: &str) -> NewReply {
        NewReply {
            to: to.into(),
            text: text.into(),
            card: None,
        }
    }

    #[test]
    fn the_orchestrator_replies_to_an_ask_and_the_owner_marks_it_read() {
        let (mut store, orchestrator) = memory_store();
        let me = Ctx::new(Actor::Session(orchestrator.clone()));
        let (ask, _) = board_ask(&mut store, "Can the export run nightly?");
        let resolved = store
            .ask_resolve(
                &me,
                &ask.id,
                Resolution::New(card(CardKind::Ship, "Run the export nightly")),
            )
            .unwrap()
            .value;
        let created = resolved.card.unwrap();
        assert_eq!(
            unreplied_asks(store.conn(), &orchestrator, LONG_AGO).unwrap(),
            vec![ask.id.clone()]
        );

        let sent = store
            .reply_send(
                &me,
                Some("k1"),
                reply(ids::short(&ask.id), "Yes, at 02:00."),
            )
            .unwrap();
        let view = &sent.value;
        assert_eq!(view.to, ReplyTo::Ask);
        assert_eq!(view.question, "Can the export run nightly?");
        assert_eq!(view.card.as_ref().unwrap().id, created.id);
        assert_eq!(view.card_relation, Some(LinkRelation::Created));
        assert!(
            unreplied_asks(store.conn(), &orchestrator, LONG_AGO)
                .unwrap()
                .is_empty()
        );

        // The same key replays and writes no second reply.
        let again = store
            .reply_send(
                &me,
                Some("k1"),
                reply(ids::short(&ask.id), "Yes, at 02:00."),
            )
            .unwrap();
        assert!(again.replayed);
        assert_eq!(unread(store.conn(), "alpha").unwrap().len(), 1);

        let read = store
            .reply_read(&Ctx::new(Actor::Board), &view.reply.id)
            .unwrap();
        assert!(read.value.reply.read_at.is_some());
        assert_eq!(read.events.len(), 1);
        assert!(unread(store.conn(), "alpha").unwrap().is_empty());
        let twice = store.reply_read(&owner(), &view.reply.id).unwrap();
        assert!(twice.events.is_empty(), "marking read again writes nothing");
    }

    #[test]
    fn a_cancelled_ask_needs_no_reply() {
        let (mut store, orchestrator) = memory_store();
        let me = Ctx::new(Actor::Session(orchestrator.clone()));
        let (ask, _) = board_ask(&mut store, "Never mind");
        store
            .ask_resolve(&me, &ask.id, Resolution::Cancelled { reason: None })
            .unwrap();
        assert!(
            unreplied_asks(store.conn(), &orchestrator, LONG_AGO)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_reply_to_an_ask_note_answers_its_ask() {
        let (mut store, orchestrator) = memory_store();
        let me = Ctx::new(Actor::Session(orchestrator));
        let (ask, message) = board_ask(&mut store, "What is left?");
        let sent = store
            .reply_send(&me, None, reply(&message.id, "Two cards."))
            .unwrap()
            .value;
        assert_eq!(sent.reply.ask_id.as_deref(), Some(ask.id.as_str()));
        assert_eq!(sent.card, None);
    }

    #[test]
    fn a_reply_to_a_comment_links_its_card() {
        let (mut store, orchestrator) = memory_store();
        let me = Ctx::new(Actor::Session(orchestrator));
        let made = store
            .card_create(
                &owner(),
                None,
                card(CardKind::Ship, "Retry the upload"),
                None,
            )
            .unwrap()
            .value;
        let comment = store
            .card_comment(
                &owner(),
                Some("c1"),
                &made.display_id,
                "Why 3 retries and not 5?",
            )
            .unwrap()
            .value;
        let sent = store
            .reply_send(
                &me,
                None,
                reply(ids::short(&comment.id), "The test is flaky."),
            )
            .unwrap();
        assert_eq!(sent.value.to, ReplyTo::Comment);
        assert_eq!(sent.value.question, "Why 3 retries and not 5?");
        assert_eq!(sent.value.card.as_ref().unwrap().id, made.id);
        let event: (String, Option<String>) = store
            .conn()
            .query_row(
                "SELECT kind, card_id FROM events WHERE seq = ?1",
                [sent.events[0]],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(event, ("ask.replied".to_owned(), Some(made.id)));
    }

    #[test]
    fn only_the_live_orchestrator_replies_and_only_the_owner_marks_read() {
        let (mut store, orchestrator) = memory_store();
        let (ask, _) = board_ask(&mut store, "Status?");
        let stranger = Actor::Session("01JZZZZZZZZZZZZZZZZZZZZZZZ".into());
        for actor in [
            Actor::Owner,
            Actor::Board,
            Actor::Hook,
            Actor::Script,
            stranger,
        ] {
            let refused = store
                .reply_send(&Ctx::new(actor.clone()), None, reply(&ask.id, "Fine."))
                .unwrap_err();
            assert!(matches!(refused, Error::Forbidden(_)), "{actor}: {refused}");
        }
        let me = Ctx::new(Actor::Session(orchestrator));
        let sent = store
            .reply_send(&me, None, reply(&ask.id, "Fine."))
            .unwrap()
            .value;
        let refused = store.reply_read(&me, &sent.reply.id).unwrap_err();
        assert!(matches!(refused, Error::Forbidden(_)), "{refused}");
    }

    #[test]
    fn a_reply_is_capped_like_a_comment_and_names_an_ask_or_a_comment() {
        let (mut store, orchestrator) = memory_store();
        let me = Ctx::new(Actor::Session(orchestrator));
        let (ask, _) = board_ask(&mut store, "Status?");
        let long = "a".repeat(COMMENT_MAX_CHARS + 1);
        assert!(matches!(
            store
                .reply_send(&me, None, reply(&ask.id, &long))
                .unwrap_err(),
            Error::Usage(_)
        ));
        assert!(matches!(
            store
                .reply_send(&me, None, reply(&ask.id, "  "))
                .unwrap_err(),
            Error::Usage(_)
        ));
        assert!(matches!(
            store
                .reply_send(&me, None, reply("ZZZZZZZ", "Fine."))
                .unwrap_err(),
            Error::NotFound { .. }
        ));
    }

    #[test]
    fn a_reply_links_only_a_card_of_its_project_or_of_the_ask() {
        let (mut store, orchestrator) = memory_store();
        add_project(&mut store, "beta");
        let me = Ctx::new(Actor::Session(orchestrator));
        let theirs = store
            .card_create(
                &owner(),
                None,
                NewCard {
                    project_id: "beta".into(),
                    ..card(CardKind::Ship, "Trim the cache")
                },
                None,
            )
            .unwrap()
            .value;
        let ours = store
            .card_create(
                &owner(),
                None,
                card(CardKind::Ship, "Pin the toolchain"),
                None,
            )
            .unwrap()
            .value;
        let (ask, _) = board_ask(&mut store, "Status?");

        let refused = store
            .reply_send(
                &me,
                None,
                NewReply {
                    card: Some(theirs.display_id.clone()),
                    ..reply(&ask.id, "Fine.")
                },
            )
            .unwrap_err();
        assert!(matches!(refused, Error::Refused { .. }), "{refused}");
        assert!(unread(store.conn(), "alpha").unwrap().is_empty());

        let sent = store
            .reply_send(
                &me,
                None,
                NewReply {
                    card: Some(ours.display_id.clone()),
                    ..reply(&ask.id, "Fine.")
                },
            )
            .unwrap()
            .value;
        assert_eq!(sent.card.unwrap().id, ours.id);
    }

    #[test]
    fn an_ambiguous_short_id_names_its_candidates() {
        let (mut store, orchestrator) = memory_store();
        let me = Ctx::new(Actor::Session(orchestrator));
        for id in ["01JAAAAAAAAAAAAAAAAAAAAAAA", "01JBBBBBBBBBBBBBBBBAAAAAAA"] {
            store
                .conn()
                .execute(
                    "INSERT INTO asks (id, project_id, source, text, received_at)
                     VALUES (?1, 'alpha', 'board', 'Status?', '2026-11-11T12:00:00Z')",
                    [id],
                )
                .unwrap();
        }
        let refused = store
            .reply_send(&me, None, reply("AAAAAAA", "Fine."))
            .unwrap_err();
        match refused {
            Error::NotFound { candidates, .. } => assert_eq!(candidates.len(), 2),
            other => panic!("{other}"),
        }
    }
}
