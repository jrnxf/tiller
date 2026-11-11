//! The command surface of `protocol.md` section 6.4, one module per group.
//! Each module owns its clap arguments and a `run` function from a parsed
//! command to an [`Outcome`](crate::app::Outcome); none writes SQL of its own.

pub mod admin;
pub mod approval;
pub mod ask;
pub mod attention;
pub mod backup;
pub mod bug;
pub mod card;
pub mod control;
pub mod doctor;
pub mod health;
pub mod import;
pub mod later;
pub mod learn;
pub mod mem;
pub mod peer;
pub mod read;
pub mod recap;
pub mod records;
pub mod reply;
pub mod review;
pub mod run;
pub mod spawn;
pub mod stale;

use clap::Args;
use serde_json::Value;
use tiller_core::Store;
use tiller_core::domain::{Card, CardKind, Effort, Run, RunRole, Session, Size};
use tiller_core::limits::{BODY_MAX_CHARS, TITLE_MAX_CHARS, within};
use tiller_core::state::{find, get, list, resolve_card, resolve_id};

use crate::app::{Failure, body_input};

/// `--key <k>` on every keyed command (`protocol.md` 6.3).
#[derive(Args, Debug, Clone, Default)]
pub struct KeyArg {
    /// Idempotency key. Without --key every owner run is a new request. Pass the same --key
    /// with the same arguments to retry safely: the retry replays the first result and
    /// changes nothing. The same key with other arguments is refused
    #[arg(long)]
    pub key: Option<String>,
}

/// The card fields `card create` and `ask resolve new` take.
#[derive(Args, Debug, Clone, Default)]
pub struct CardFields {
    /// ship, scout, question or ops
    #[arg(long)]
    pub kind: Option<CardKind>,
    /// The card's title
    #[arg(long)]
    pub title: Option<String>,
    /// 0 (highest) to 4; the default is 2
    #[arg(long)]
    pub priority: Option<i64>,
    /// The card's brief in markdown; - reads standard input
    #[arg(long)]
    pub body: Option<String>,
    /// A file holding the body, or - for standard input; not with --body
    #[arg(long)]
    pub body_file: Option<std::path::PathBuf>,
    /// S, M or L
    #[arg(long)]
    pub size: Option<Size>,
    /// What done looks like, in words
    #[arg(long)]
    pub acceptance: Option<String>,
    /// A free-text label, like tiller-bug
    #[arg(long)]
    pub category: Option<String>,
    /// The card this one fixes
    #[arg(long)]
    pub fixes: Option<String>,
}

impl CardFields {
    pub fn into_new(self, project_id: &str) -> Result<tiller_core::state::cards::NewCard, Failure> {
        let kind = self
            .kind
            .ok_or_else(|| Failure::usage("a new card needs --kind ship|scout|question|ops"))?;
        let title = self
            .title
            .ok_or_else(|| Failure::usage("a new card needs --title"))?;
        let body = body_input(self.body, self.body_file.as_deref())?;
        card_text(Some(&title), body.as_deref())?;
        Ok(tiller_core::state::cards::NewCard {
            project_id: project_id.to_owned(),
            kind,
            title,
            body,
            priority: self.priority,
            category: self.category,
            size: self.size,
            acceptance: self.acceptance,
            fixes_card: self.fixes,
            legacy_id: None,
        })
    }

    /// The arguments as the key derivation sees them.
    pub fn normalized(&self) -> Value {
        serde_json::json!({
            "kind": self.kind, "title": self.title, "priority": self.priority,
            "body": self.body,
            "body_file": self.body_file.as_ref().map(|path| std::fs::read_to_string(path).ok()),
            "size": self.size,
            "acceptance": self.acceptance, "category": self.category, "fixes": self.fixes,
        })
    }
}

/// A usage error when a card's title or body runs past the API's caps
/// (`protocol.md` 7.4, "Length caps"), so the CLI takes what the board takes.
pub fn card_text(title: Option<&str>, body: Option<&str>) -> Result<(), Failure> {
    within("title", title.unwrap_or_default(), TITLE_MAX_CHARS)?;
    within("body", body.unwrap_or_default(), BODY_MAX_CHARS)?;
    Ok(())
}

/// An edit flag as the core takes it: empty clears, anything else sets.
pub fn clearable(text: Option<String>) -> Option<Option<String>> {
    text.map(|text| if text.is_empty() { None } else { Some(text) })
}

/// `<model>:<effort>`, the dispatch override form.
pub fn parse_dispatch(text: &str) -> Result<(String, Effort), Failure> {
    let (model, effort) = text
        .split_once(':')
        .ok_or_else(|| Failure::usage("a dispatch override is <model>:<effort>, like opus:high"))?;
    if model.is_empty() {
        return Err(Failure::usage("a dispatch override names a model"));
    }
    Ok((model.to_owned(), effort.parse()?))
}

/// The tag an agent starts a line about a card with: `[alpha-42]`.
pub fn tag(display_id: &str) -> String {
    format!("[{display_id}]")
}

/// Resolves a run: a ULID or short form, `<display id> <role>` (`alpha-42
/// build`), or a herdr name (`alpha-42-build`, `alpha-42-build-2`).
///
/// A role without an attempt means the card's active run of that role, else
/// the newest.
pub fn resolve_run(store: &Store, input: &str) -> Result<Run, Failure> {
    let conn = store.conn();
    let input = input.trim();
    if let Some((display, role_word)) = input.split_once(char::is_whitespace) {
        return run_by_role(store, display.trim(), role_word.trim(), None);
    }
    if tiller_core::ids::is_ulid(input) || tiller_core::ids::is_short(input) {
        let id = resolve_id(conn, "runs", "run", input)?;
        return Ok(get(conn, "run", &id)?);
    }
    // alpha-42-build or alpha-42-build-2: the display id has its own dash.
    let parts: Vec<&str> = input.split('-').collect();
    for cut in (2..=parts.len().saturating_sub(1)).rev() {
        let display = parts[..cut].join("-");
        if tiller_core::ids::parse_display_id(&display).is_none() {
            continue;
        }
        let rest = &parts[cut..];
        let (role_word, attempt) = match rest {
            [role] => (*role, None),
            [role, attempt] => match attempt.parse::<i64>() {
                Ok(n) => (*role, Some(n)),
                Err(_) => continue,
            },
            _ => continue,
        };
        return run_by_role(store, &display, role_word, attempt);
    }
    Err(Failure::from(tiller_core::Error::NotFound {
        entity: "run",
        id: input.to_owned(),
        candidates: Vec::new(),
    }))
}

fn run_by_role(
    store: &Store,
    display: &str,
    role_word: &str,
    attempt: Option<i64>,
) -> Result<Run, Failure> {
    let role = [
        RunRole::Scout,
        RunRole::Builder,
        RunRole::Reviewer,
        RunRole::Verifier,
    ]
    .into_iter()
    .find(|role| role.short_name() == role_word || role.as_str() == role_word)
    .ok_or_else(|| {
        Failure::usage(format!(
            "not a run role: {role_word:?} (scout, build, review or verify)"
        ))
    })?;
    let card = resolve_card(store.conn(), display)?;
    let runs: Vec<Run> = list(
        store.conn(),
        "card_id = ?1 AND role = ?2 ORDER BY attempt",
        &[&card.id, &role],
    )?;
    let picked = match attempt {
        Some(n) => runs.into_iter().find(|run| run.attempt == n),
        None => {
            let active = runs.iter().position(|run| run.ended_at.is_none());
            match active {
                Some(index) => runs.into_iter().nth(index),
                None => runs.into_iter().last(),
            }
        }
    };
    picked.ok_or_else(|| {
        Failure::from(tiller_core::Error::NotFound {
            entity: "run",
            id: format!("{display} {role_word}"),
            candidates: Vec::new(),
        })
    })
}

/// A run or a session reference: a run resolves to its live session.
pub fn resolve_session(store: &Store, input: &str) -> Result<Session, Failure> {
    let conn = store.conn();
    if let Ok(id) = resolve_id(conn, "sessions", "session", input)
        && let Ok(session) = get::<Session>(conn, "session", &id)
    {
        return Ok(session);
    }
    let run = resolve_run(store, input)?;
    find::<Session>(conn, "run_id = ?1 AND ended_at IS NULL", &[&run.id])?.ok_or_else(|| {
        Failure::refused(format!("run {} has no live session", run.id))
            .hint("tiller agent relaunch")
    })
}

/// A card and its project, for the commands that take a card reference.
pub fn card_of(store: &Store, reference: &str) -> Result<Card, Failure> {
    Ok(resolve_card(store.conn(), reference)?)
}
