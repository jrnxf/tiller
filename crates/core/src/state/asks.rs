//! Asks: `ask.record`, `ask.resolve`, `ask.link` and the board's add card
//! (`state.md`, Ask; `protocol.md` section 2).

use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::cards::{NewCard, create_card, link_ask};
use super::outbox::{NewMessage, enqueue, live_orchestrator};
use super::{find, get, refuse, required, resolve_card, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};

/// The warning an ask's response carries when nobody was told about it.
pub const NO_ORCHESTRATOR: &str = "no_orchestrator";

/// An ask to record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewAsk {
    pub project_id: String,
    /// The Tiller session the prompt was typed in; `None` for a script note.
    pub session_id: Option<String>,
    pub source: AskSource,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AskRecorded {
    pub ask: Ask,
    /// The note that tells an orchestrator, when one was enqueued.
    pub message: Option<OutboxMessage>,
    /// The card a prompt typed in a worker pane followed up.
    pub followup: Option<Card>,
    pub warning: Option<String>,
}

/// Where `tiller ask resolve` sends an ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub enum Resolution {
    /// A new card in the ask's project; `project_id` is ignored.
    New(NewCard),
    /// An open card, by display id, ULID or short form.
    Followup {
        card: String,
    },
    Chat {
        reason: Option<String>,
    },
    Cancelled {
        reason: Option<String>,
    },
}

impl Resolution {
    fn disposition(&self) -> Disposition {
        match self {
            Resolution::New(_) => Disposition::New,
            Resolution::Followup { .. } => Disposition::Followup,
            Resolution::Chat { .. } => Disposition::Chat,
            Resolution::Cancelled { .. } => Disposition::Cancelled,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resolved {
    pub ask: Ask,
    pub card: Option<Card>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardAdded {
    pub ask: Ask,
    pub card: Card,
    pub message: Option<OutboxMessage>,
    pub warning: Option<String>,
}

fn insert_ask(tx: &mut Tx<'_>, new: &NewAsk) -> Result<Ask> {
    required("text", &new.text)?;
    let project: Project = get(tx.conn(), "project", &new.project_id)?;
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO asks (id, project_id, session_id, source, text, received_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, project.id, new.session_id, new.source, new.text, tx.now()],
    )?;
    tx.event(NewEvent::new("ask", &id, "ask.recorded").payload(
        json!({"project_id": project.id, "session_id": new.session_id, "source": new.source}),
    ))?;
    get(tx.conn(), "ask", &id)
}

/// Moves an ask out of `unresolved`, linking `card` when it has one.
fn resolve(
    tx: &mut Tx<'_>,
    ask: &Ask,
    to: Disposition,
    card: Option<(&Card, LinkRelation)>,
    reason: Option<&str>,
) -> Result<Ask> {
    ask.disposition
        .resolve(to)
        .map_err(refuse("ask", &ask.id))?;
    let session = tx.actor().session_id().map(str::to_owned);
    tx.conn().execute(
        "UPDATE asks SET disposition = ?2, resolved_at = ?3, resolved_by_session_id = ?4 WHERE id = ?1",
        params![ask.id, to, tx.now(), session],
    )?;
    tx.event(
        NewEvent::new("ask", &ask.id, "ask.resolved")
            .card_opt(card.map(|(card, _)| card.id.clone()))
            .payload(json!({"disposition": to, "reason": reason})),
    )?;
    if let Some((card, LinkRelation::Followup)) = card {
        link_ask(tx, &ask.id, &card.id, LinkRelation::Followup)?;
    }
    get(tx.conn(), "ask", &ask.id)
}

fn followup_card(tx: &Tx<'_>, reference: &str) -> Result<Card> {
    let card = resolve_card(tx.conn(), reference)?;
    if card.state != CardState::Open {
        return Err(Error::refused(
            "card",
            &card.display_id,
            format!(
                "card is closed {}; reopen it or file a new card",
                card.outcome.map_or("", CardOutcome::as_str)
            ),
        ));
    }
    Ok(card)
}

fn new_card_for(ask: &Ask, new: &NewCard) -> NewCard {
    NewCard {
        project_id: ask.project_id.clone(),
        ..new.clone()
    }
}

/// The first card an ask created.
fn created_card(tx: &Tx<'_>, ask: &Ask) -> Result<Option<Card>> {
    find(
        tx.conn(),
        "created_by_ask_id = ?1 ORDER BY created_at, id",
        &[&ask.id],
    )
}

/// Tells the project's live orchestrator about an ask, or warns.
fn note_orchestrator(
    tx: &mut Tx<'_>,
    ask: &Ask,
    card: Option<&Card>,
) -> Result<(Option<OutboxMessage>, Option<String>)> {
    let Some(orchestrator) = live_orchestrator(tx.conn(), &ask.project_id)? else {
        return Ok((None, Some(NO_ORCHESTRATOR.to_owned())));
    };
    let body = match card {
        Some(card) => format!("[{}] added from the board: {}", card.display_id, card.title),
        None => format!("ask {} ({}): {}", ask.id, ids::short(&ask.id), ask.text),
    };
    let message = enqueue(
        tx,
        &NewMessage {
            target_session_id: orchestrator.id,
            kind: OutboxKind::Note,
            body,
            card_id: card.map(|card| card.id.clone()),
            reply_to: Some(ask.id.clone()),
        },
    )?;
    Ok((Some(message), None))
}

impl Store {
    /// `ask.record`. Keyed. A prompt typed in a worker pane is resolved as a
    /// follow-up on the run's card and noted to its supervisor; a script note
    /// or a note typed in the board's ask box goes to the live orchestrator,
    /// or carries `warning: no_orchestrator`.
    pub fn ask_record(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        new: NewAsk,
    ) -> Result<Applied<AskRecorded>> {
        // The board's own ask box has no session and notes the orchestrator
        // like a script note; its add card has its own path (`board_add`).
        let board_note = new.source == AskSource::Board && new.session_id.is_none();
        if (new.source == AskSource::Board && !board_note) || new.source == AskSource::Import {
            return Err(Error::Usage(format!(
                "{} asks are recorded by their own path",
                new.source
            )));
        }
        self.write(ctx, "ask.record", key, |tx| {
            let session = match &new.session_id {
                Some(id) => Some(get::<Session>(tx.conn(), "session", id)?),
                None => None,
            };
            if let Some(session) = &session
                && session.project_id != new.project_id
            {
                return Err(Error::refused(
                    "session",
                    &session.id,
                    format!("session belongs to {}", session.project_id),
                ));
            }
            let ask = insert_ask(tx, &new)?;
            match session {
                Some(session) if session.role != Role::Orchestrator => {
                    let Some(run_id) = &session.run_id else {
                        return Err(Error::Internal(format!(
                            "worker session {} has no run",
                            session.id
                        )));
                    };
                    let run: Run = get(tx.conn(), "run", run_id)?;
                    let card: Card = get(tx.conn(), "card", &run.card_id)?;
                    if card.state != CardState::Open {
                        // A closed card cannot take a follow-up; the ask stays
                        // unresolved and visible in Asked.
                        let (message, warning) = note_orchestrator(tx, &ask, None)?;
                        return Ok(AskRecorded {
                            ask,
                            message,
                            followup: None,
                            warning,
                        });
                    }
                    let ask = resolve(
                        tx,
                        &ask,
                        Disposition::Followup,
                        Some((&card, LinkRelation::Followup)),
                        None,
                    )?;
                    let supervisor = find::<Session>(
                        tx.conn(),
                        "id = ?1 AND ended_at IS NULL",
                        &[&run.supervisor_session_id],
                    )?;
                    let (message, warning) = match supervisor {
                        Some(supervisor) => {
                            let message = enqueue(
                                tx,
                                &NewMessage {
                                    target_session_id: supervisor.id,
                                    kind: OutboxKind::Note,
                                    body: format!(
                                        "[{}] the owner typed in the {} pane: {}",
                                        card.display_id,
                                        run.role.short_name(),
                                        ask.text
                                    ),
                                    card_id: Some(card.id.clone()),
                                    reply_to: Some(ask.id.clone()),
                                },
                            )?;
                            (Some(message), None)
                        }
                        None => (None, Some(NO_ORCHESTRATOR.to_owned())),
                    };
                    Ok(AskRecorded {
                        ask,
                        message,
                        followup: Some(card),
                        warning,
                    })
                }
                Some(_) => Ok(AskRecorded {
                    ask,
                    message: None,
                    followup: None,
                    warning: None,
                }),
                None => {
                    let (message, warning) = note_orchestrator(tx, &ask, None)?;
                    Ok(AskRecorded {
                        ask,
                        message,
                        followup: None,
                        warning,
                    })
                }
            }
        })
    }

    /// `ask.resolve`. Natural: resolving again with the same target returns
    /// the first response; a different target is refused with the current
    /// disposition.
    pub fn ask_resolve(
        &mut self,
        ctx: &Ctx,
        ask: &str,
        resolution: Resolution,
    ) -> Result<Applied<Resolved>> {
        self.write(ctx, "ask.resolve", None, |tx| {
            let id = resolve_id(tx.conn(), "asks", "ask", ask)?;
            let ask: Ask = get(tx.conn(), "ask", &id)?;
            if ask.disposition != Disposition::Unresolved {
                let same = ask.disposition == resolution.disposition();
                let card = match (&resolution, ask.disposition) {
                    (Resolution::New(_), Disposition::New) => created_card(tx, &ask)?,
                    (Resolution::Followup { card }, Disposition::Followup) => {
                        let card = resolve_card(tx.conn(), card)?;
                        let linked: bool = tx.conn().query_row(
                            "SELECT EXISTS (SELECT 1 FROM ask_cards WHERE ask_id = ?1 AND card_id = ?2)",
                            params![ask.id, card.id],
                            |row| row.get(0),
                        )?;
                        if !linked {
                            return Err(Error::refused("ask", &ask.id, "ask is already resolved followup to another card"));
                        }
                        Some(card)
                    }
                    _ if same => None,
                    _ => return Err(Error::refused("ask", &ask.id, format!("ask is already resolved {}", ask.disposition))),
                };
                return Ok(Resolved { ask, card });
            }
            match &resolution {
                Resolution::New(new) => {
                    let ask = resolve(tx, &ask, Disposition::New, None, None)?;
                    let card = create_card(tx, &new_card_for(&ask, new), Some(&ask.id))?;
                    Ok(Resolved { ask, card: Some(card) })
                }
                Resolution::Followup { card } => {
                    let card = followup_card(tx, card)?;
                    let ask = resolve(tx, &ask, Disposition::Followup, Some((&card, LinkRelation::Followup)), None)?;
                    Ok(Resolved { ask, card: Some(card) })
                }
                Resolution::Chat { reason } | Resolution::Cancelled { reason } => {
                    let ask = resolve(tx, &ask, resolution.disposition(), None, reason.as_deref())?;
                    Ok(Resolved { ask, card: None })
                }
            }
        })
    }

    /// `ask.link`: one more card for an ask resolved `new` or `followup`.
    /// Keyed. Natural for a follow-up already linked.
    pub fn ask_link(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        ask: &str,
        resolution: Resolution,
    ) -> Result<Applied<Resolved>> {
        self.write(ctx, "ask.link", key, |tx| {
            let id = resolve_id(tx.conn(), "asks", "ask", ask)?;
            let ask: Ask = get(tx.conn(), "ask", &id)?;
            if !matches!(ask.disposition, Disposition::New | Disposition::Followup) {
                return Err(Error::refused(
                    "ask",
                    &ask.id,
                    format!(
                        "ask is {}; only a new or followup ask links more cards",
                        ask.disposition
                    ),
                ));
            }
            match &resolution {
                Resolution::New(new) => {
                    let card = create_card(tx, &new_card_for(&ask, new), Some(&ask.id))?;
                    Ok(Resolved {
                        ask,
                        card: Some(card),
                    })
                }
                Resolution::Followup { card } => {
                    let card = followup_card(tx, card)?;
                    let linked = find::<AskCard>(
                        tx.conn(),
                        "ask_id = ?1 AND card_id = ?2",
                        &[&ask.id, &card.id],
                    )?;
                    if linked.is_none() {
                        link_ask(tx, &ask.id, &card.id, LinkRelation::Followup)?;
                    }
                    Ok(Resolved {
                        ask,
                        card: Some(card),
                    })
                }
                _ => Err(Error::Usage("ask link takes new or followup".into())),
            }
        })
    }

    /// The board's add card (`POST /api/v1/cards`): `ask.record`, `ask.resolve
    /// new`, `card.create` and the note, in one transaction. Keyed by the
    /// click, so a double click never creates two cards.
    pub fn board_add(
        &mut self,
        ctx: &Ctx,
        key: Option<&str>,
        new: NewCard,
    ) -> Result<Applied<BoardAdded>> {
        self.write_as(ctx, "ask.record", "board.add", key, |tx| {
            let text = match &new.body {
                Some(body) => format!("{}\n\n{body}", new.title),
                None => new.title.clone(),
            };
            let ask = insert_ask(
                tx,
                &NewAsk {
                    project_id: new.project_id.clone(),
                    session_id: None,
                    source: AskSource::Board,
                    text,
                },
            )?;
            let ask = resolve(tx, &ask, Disposition::New, None, None)?;
            let card = create_card(tx, &new, Some(&ask.id))?;
            let (message, warning) = note_orchestrator(tx, &ask, Some(&card))?;
            Ok(BoardAdded {
                ask,
                card,
                message,
                warning,
            })
        })
    }
}
