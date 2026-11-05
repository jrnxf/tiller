//! UserPromptSubmit intake, the wake-up line ack and the Stop hook's intake
//! check (`protocol.md` sections 2 and 3.2), plugged into `tiller hook` as
//! [`tiller_observe::hooks::Intake`].
//!
//! The hook carries only the mechanics: it records the ask and tells the
//! orchestrator its id; the orchestrator role carries the judgement and runs
//! `tiller ask resolve`. The Stop hook measures what blocking would have done
//! and, in `block-once` mode only, blocks a turn at most once.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tiller_core::domain::{Ask, AskSource, Card, OutboxMessage, OutboxState, Role, Session};
use tiller_core::state::asks::NewAsk;
use tiller_core::state::{find, get, list};
use tiller_core::{Actor, Ctx, Store, ids};
use tiller_observe::hooks::{HookInput, Intake, Reply};

/// The host config value `stop_hook_mode` (`protocol.md` 2.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StopHookMode {
    /// Record the check, never block. The default, and the only mode at
    /// cutover.
    #[default]
    Observe,
    /// Block a turn once when the check fails.
    BlockOnce,
    /// Record only the turn end.
    Off,
}

/// The host config value `intake_source` (`protocol.md` 2.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IntakeSource {
    #[default]
    Hook,
    /// Hooks are blocked on this host; the transcript tailer records asks.
    Transcript,
}

/// The two host config values intake reads from `config.toml`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostConfig {
    pub stop_hook_mode: StopHookMode,
    pub intake_source: IntakeSource,
}

impl HostConfig {
    /// Reads `<tiller home>/config.toml`. A missing or unreadable file, or
    /// an unknown value, gives the default: a hook fails open, and observing
    /// is the safe default.
    pub fn load(tiller_home: &Path) -> Self {
        let table = fs::read_to_string(tiller_home.join("config.toml"))
            .ok()
            .and_then(|text| text.parse::<toml::Table>().ok())
            .unwrap_or_default();
        let value = |key: &str| table.get(key).and_then(toml::Value::as_str);
        Self {
            stop_hook_mode: match value("stop_hook_mode") {
                Some("block-once") => StopHookMode::BlockOnce,
                Some("off") => StopHookMode::Off,
                _ => StopHookMode::Observe,
            },
            intake_source: match value("intake_source") {
                Some("transcript") => IntakeSource::Transcript,
                _ => IntakeSource::Hook,
            },
        }
    }
}

/// Intake as `tiller hook` runs it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hooks {
    pub config: HostConfig,
}

impl Hooks {
    pub fn load(tiller_home: &Path) -> Self {
        Self {
            config: HostConfig::load(tiller_home),
        }
    }
}

impl Intake for Hooks {
    fn prompt(
        &self,
        store: &mut Store,
        session: &Session,
        prompt: &str,
        input: &HookInput,
    ) -> tiller_core::Result<Reply> {
        // On a transcript host the tailer records the ask; a hook that fires
        // anyway must not make it two.
        if self.config.intake_source == IntakeSource::Transcript || prompt.trim().is_empty() {
            return Ok(Reply::default());
        }
        let recorded = store.ask_record(
            &Ctx::new(Actor::Hook),
            Some(&prompt_key(prompt, input)),
            NewAsk {
                project_id: session.project_id.clone(),
                session_id: Some(session.id.clone()),
                source: AskSource::Prompt,
                text: prompt.to_owned(),
            },
        )?;
        let ask = &recorded.value.ask;
        let context = match &recorded.value.followup {
            Some(card) => format!(
                "tiller: noted as a follow-up on [{}]; your orchestrator was told.",
                card.display_id
            ),
            None if session.role == Role::Orchestrator => {
                ask_context(ask, store.project_free_runs(&session.project_id)?)
            }
            // A worker prompt whose card is closed: the orchestrator decides.
            None => format!(
                "tiller: ask {} recorded for your orchestrator; the card is closed.",
                ids::short(&ask.id)
            ),
        };
        let mut payload = Map::new();
        payload.insert("ask_id".into(), json!(ask.id));
        Ok(Reply {
            payload,
            output: Some(additional_context("UserPromptSubmit", &context)),
        })
    }

    fn wake(
        &self,
        store: &mut Store,
        session: Option<&Session>,
        outbox_id: &str,
        mine: bool,
    ) -> tiller_core::Result<Reply> {
        let context = match session {
            Some(session) if mine => wake_context(store, session, outbox_id)?,
            _ => "tiller: that inbox line is not for this session; ignore it.".to_owned(),
        };
        Ok(Reply {
            payload: Map::new(),
            output: Some(additional_context("UserPromptSubmit", &context)),
        })
    }

    fn stop(
        &self,
        store: &mut Store,
        session: &Session,
        input: &HookInput,
    ) -> tiller_core::Result<Reply> {
        // Workers are never checked; `off` records only the turn end.
        if session.role != Role::Orchestrator || self.config.stop_hook_mode == StopHookMode::Off {
            return Ok(Reply::default());
        }
        let reply = input
            .extra
            .get("last_assistant_message")
            .and_then(Value::as_str)
            .unwrap_or("");
        let check = stop_check(store, session, reply)?;
        let active = input.extra.get("stop_hook_active").and_then(Value::as_bool) == Some(true);
        let output = (self.config.stop_hook_mode == StopHookMode::BlockOnce
            && check.would_block()
            && !active)
            .then(|| json!({"decision": "block", "reason": check.reason()}));
        Ok(Reply {
            payload: check.payload(),
            output,
        })
    }
}

/// The idempotency key of a typed prompt (`protocol.md` 2.2): the hook's
/// `prompt_id`, or the prompt's hash at the transcript's current length, so
/// a redelivered hook replays and the same words typed again are new.
pub fn prompt_key(prompt: &str, input: &HookInput) -> String {
    let claude = input.session_id.as_deref().unwrap_or("-");
    if let Some(prompt_id) = input.extra.get("prompt_id").and_then(Value::as_str) {
        return format!("prompt:{claude}:{prompt_id}");
    }
    let length = input
        .transcript_path
        .as_deref()
        .and_then(|path| fs::metadata(path).ok())
        .map_or(0, |meta| meta.len());
    format!("prompt:{claude}:{}:{length}", hex(&Sha256::digest(prompt)))
}

/// The one line the orchestrator gets for an ask (`protocol.md` 2.3), with
/// the project's free run slots so a spawn it cannot make is reported as
/// queued, not started.
pub fn ask_context(ask: &Ask, free_runs: i64) -> String {
    let short = ids::short(&ask.id);
    let slots = match free_runs {
        0 => "no free run slots: a spawn would be refused, so leave the card queued and say it is queued, not started".to_owned(),
        1 => "1 free run slot: spawn at most one run, leave other cards queued and say they are queued, not started".to_owned(),
        n => format!("{n} free run slots: spawn at most {n} runs, leave other cards queued and say they are queued, not started"),
    };
    format!(
        "tiller: ask {} (short {short}) recorded. Before this turn ends run: tiller ask resolve {short} new|followup:<card>|chat|cancelled. Start each line about a card with its tag, like [{}-42]. Project {}: {slots}.",
        ask.id, ask.project_id, ask.project_id
    )
}

/// The `additionalContext` output of a hook [D].
fn additional_context(event: &str, context: &str) -> Value {
    json!({"hookSpecificOutput": {"hookEventName": event, "additionalContext": context}})
}

/// Acks a wake-up line for this session and returns every message it
/// stands for (`protocol.md` 3.2). An acked row returns its body again and
/// writes nothing; a failed row returns its body with a note, and writes
/// nothing, since reconcile owns it.
fn wake_context(
    store: &mut Store,
    session: &Session,
    outbox_id: &str,
) -> tiller_core::Result<String> {
    let Some(named) = find::<OutboxMessage>(store.conn(), "id = ?1", &[&outbox_id])? else {
        return Ok("tiller: that inbox line names no message; ignore it.".to_owned());
    };
    let messages = match named.state {
        OutboxState::Acked => vec![named],
        OutboxState::Failed => {
            let line = message_line(store, &named)?;
            return Ok(format!(
                "{line}\n(tiller gave up delivering this message; it is shown for reference.)"
            ));
        }
        // One transaction acks the named row and every other message still
        // waiting for this session, oldest first.
        OutboxState::Pending | OutboxState::Delivered => {
            store
                .outbox_drain(&Ctx::new(Actor::Hook), &session.id)?
                .value
        }
    };
    let lines = messages
        .iter()
        .map(|message| message_line(store, message))
        .collect::<tiller_core::Result<Vec<_>>>()?;
    Ok(lines.join("\n"))
}

fn message_line(store: &Store, message: &OutboxMessage) -> tiller_core::Result<String> {
    let card = match &message.card_id {
        Some(id) => format!(" {}", get::<Card>(store.conn(), "card", id)?.display_id),
        None => String::new(),
    };
    Ok(format!(
        "tiller: message {} ({}{card}): {}",
        ids::short(&message.id),
        message.kind,
        message.body
    ))
}

/// What the Stop hook found in one orchestrator turn (`protocol.md` 2.5).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopCheck {
    /// Asks recorded in this session that are still unresolved, oldest first.
    pub unresolved_ask_ids: Vec<String>,
    /// Line-start tags that name no card on this host.
    pub unknown_tags: Vec<String>,
    /// Line-start tags that name a card: the cards the reply talked about.
    pub tags: Vec<String>,
}

impl StopCheck {
    pub fn would_block(&self) -> bool {
        !self.unresolved_ask_ids.is_empty() || !self.unknown_tags.is_empty()
    }

    /// The Stop observation's intake fields. Tags are stored as display ids.
    pub fn payload(&self) -> Map<String, Value> {
        let mut payload = Map::new();
        payload.insert("unresolved_ask_ids".into(), json!(self.unresolved_ask_ids));
        payload.insert("unknown_tags".into(), json!(self.unknown_tags));
        payload.insert("tags".into(), json!(self.tags));
        payload.insert("would_block".into(), json!(self.would_block()));
        payload
    }

    /// One line naming each unresolved ask and unknown tag.
    pub fn reason(&self) -> String {
        let mut parts = Vec::new();
        if !self.unresolved_ask_ids.is_empty() {
            let asks: Vec<&str> = self
                .unresolved_ask_ids
                .iter()
                .map(|id| ids::short(id))
                .collect();
            parts.push(format!(
                "resolve ask {} with tiller ask resolve <ask> new|followup:<card>|chat|cancelled",
                asks.join(", ")
            ));
        }
        if !self.unknown_tags.is_empty() {
            let tags: Vec<String> = self
                .unknown_tags
                .iter()
                .map(|tag| format!("[{tag}]"))
                .collect();
            parts.push(format!(
                "{} names no card; create it or fix the tag",
                tags.join(", ")
            ));
        }
        format!("tiller: before ending the turn, {}.", parts.join("; "))
    }
}

/// Checks one orchestrator turn: the session's unresolved asks, and the
/// card tags at the start of the reply's lines.
pub fn stop_check(store: &Store, session: &Session, reply: &str) -> tiller_core::Result<StopCheck> {
    let unresolved: Vec<Ask> = list(
        store.conn(),
        "session_id = ?1 AND disposition = 'unresolved' ORDER BY received_at, id",
        &[&session.id],
    )?;
    let mut check = StopCheck {
        unresolved_ask_ids: unresolved.into_iter().map(|ask| ask.id).collect(),
        ..StopCheck::default()
    };
    for tag in line_tags(reply) {
        let known = find::<Card>(store.conn(), "display_id = ?1", &[&tag])?.is_some();
        let list = if known {
            &mut check.tags
        } else {
            &mut check.unknown_tags
        };
        if !list.contains(&tag) {
            list.push(tag);
        }
    }
    Ok(check)
}

/// Every `[<project>-<n>]` token that starts a line (after indentation), as
/// a display id. `[chat]` is always valid and names no card, so it is not
/// returned; a bracket that is not a card tag is ordinary text. Lines inside
/// a fenced code block are examples, not talk about a card.
pub fn line_tags(reply: &str) -> Vec<String> {
    let mut fenced = false;
    let mut tags = Vec::new();
    for line in reply.lines() {
        let line = line.trim_start();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some((tag, _)) = line.strip_prefix('[').and_then(|rest| rest.split_once(']'))
            && ids::parse_display_id(tag).is_some()
        {
            tags.push(tag.to_owned());
        }
    }
    tags
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
