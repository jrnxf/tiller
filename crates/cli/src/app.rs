//! What every command shares: who the caller is, where `state.db` lives, the
//! owner marker, idempotency keys, and the response and error shapes
//! (`protocol.md` sections 6.1 to 6.3).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tiller_core::authority::principal;
use tiller_core::domain::Session;
use tiller_core::state::{get, resolve_id};
use tiller_core::{Actor, Applied, Ctx, Error, Store};

/// A command's result before it is rendered.
#[derive(Debug)]
pub struct Outcome {
    pub data: Value,
    /// Present for a mutation: the `seq` of every event it wrote, and whether
    /// an idempotency key or a natural no-op made it a replay.
    pub mutation: Option<(Vec<i64>, bool)>,
    /// The key a shell caller can repeat the command with (6.3).
    pub key: Option<String>,
    pub warning: Option<String>,
    pub help: Vec<String>,
}

impl Outcome {
    pub fn read(data: impl Serialize) -> Self {
        Self {
            data: to_value(data),
            mutation: None,
            key: None,
            warning: None,
            help: Vec::new(),
        }
    }

    pub fn applied<T: Serialize>(applied: Applied<T>) -> Self {
        Self {
            data: to_value(applied.value),
            mutation: Some((applied.events, applied.replayed)),
            key: None,
            warning: None,
            help: Vec::new(),
        }
    }

    /// A mutation whose `data` is shaped by the command, not the core value.
    pub fn applied_as<T>(applied: Applied<T>, data: impl FnOnce(&T) -> Value) -> Self {
        Self {
            data: data(&applied.value),
            mutation: Some((applied.events, applied.replayed)),
            key: None,
            warning: None,
            help: Vec::new(),
        }
    }

    pub fn with_help(mut self, line: impl Into<String>) -> Self {
        self.help.push(line.into());
        self
    }

    pub fn with_warning(mut self, warning: Option<String>) -> Self {
        self.warning = warning;
        self
    }

    pub fn with_key(mut self, keyed: &Keyed) -> Self {
        if keyed.minted {
            self.key = Some(keyed.key.clone());
        }
        self
    }

    /// The response `protocol.md` section 6.1 describes.
    pub fn into_response(self) -> Value {
        let mut response = Map::new();
        response.insert("ok".into(), Value::Bool(true));
        response.insert("data".into(), self.data);
        if let Some((events, replayed)) = self.mutation {
            response.insert("events".into(), json!(events));
            response.insert("replayed".into(), Value::Bool(replayed));
        }
        if let Some(key) = self.key {
            response.insert("key".into(), Value::String(key));
        }
        if let Some(warning) = self.warning {
            response.insert("warning".into(), Value::String(warning));
        }
        if !self.help.is_empty() {
            response.insert("help".into(), json!(self.help));
        }
        let mut response = Value::Object(response);
        add_short_ids(&mut response);
        response
    }
}

pub fn to_value(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Prints the short form beside every ULID `id` (6.1).
fn add_short_ids(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(add_short_ids),
        Value::Object(map) => {
            map.values_mut().for_each(add_short_ids);
            let short = map
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| tiller_core::ids::is_ulid(id))
                .map(|id| tiller_core::ids::short(id).to_owned());
            if let Some(short) = short {
                map.insert("short".into(), Value::String(short));
            }
        }
        _ => {}
    }
}

/// A command that did not succeed, with the exit code and error body of
/// `protocol.md` sections 6.1 and 6.2.
#[derive(Debug)]
pub struct Failure(Box<Detail>);

impl std::ops::Deref for Failure {
    type Target = Detail;

    fn deref(&self) -> &Detail {
        &self.0
    }
}

impl std::ops::DerefMut for Failure {
    fn deref_mut(&mut self) -> &mut Detail {
        &mut self.0
    }
}

/// What a [`Failure`] carries, boxed so `Result<_, Failure>` stays small.
#[derive(Debug)]
pub struct Detail {
    pub code: &'static str,
    pub exit: u8,
    pub message: String,
    pub entity: Option<String>,
    pub id: Option<String>,
    pub current: Option<Value>,
    pub hint: Option<String>,
}

impl Failure {
    pub fn new(code: &'static str, exit: u8, message: impl Into<String>) -> Self {
        Self(Box::new(Detail {
            code,
            exit,
            message: message.into(),
            entity: None,
            id: None,
            current: None,
            hint: None,
        }))
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new("usage", 2, message)
    }

    pub fn refused(message: impl Into<String>) -> Self {
        Self::new("refused", 5, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new("forbidden", 6, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("unavailable", 7, message)
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn into_response(self) -> Value {
        let this = *self.0;
        let mut error = Map::new();
        error.insert("code".into(), Value::String(this.code.into()));
        error.insert("message".into(), Value::String(this.message));
        if let Some(entity) = this.entity {
            error.insert("entity".into(), Value::String(entity));
        }
        if let Some(id) = this.id {
            error.insert("id".into(), Value::String(id));
        }
        if let Some(current) = this.current {
            error.insert("current".into(), current);
        }
        if let Some(hint) = this.hint {
            error.insert("hint".into(), Value::String(hint));
        }
        json!({"ok": false, "error": Value::Object(error)})
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        let code = error.code();
        let exit = error.exit_code() as u8;
        let mut failure = Failure::new(code, exit, message);
        match error {
            Error::NotFound {
                entity,
                id,
                candidates,
            } => {
                failure.entity = Some(entity.to_owned());
                failure.id = Some(id);
                if !candidates.is_empty() {
                    failure.current = Some(json!({"candidates": candidates}));
                    failure.hint = Some("use the full id".into());
                }
            }
            Error::Conflict {
                entity,
                id,
                current_rev,
                sent_rev,
                field,
            } => {
                failure.entity = Some(entity.to_owned());
                failure.id = Some(id);
                failure.current = Some(json!({
                    "rev": current_rev,
                    "sent_rev": sent_rev,
                    "field": field.map(|(name, value)| json!({"name": name, "value": value})),
                }));
                failure.hint = Some(format!("read the {entity} again and retry with its rev"));
            }
            Error::Refused { entity, id, .. } => {
                failure.entity = Some(entity.to_owned());
                if !id.is_empty() {
                    failure.id = Some(id);
                }
            }
            Error::Unavailable(_) => failure.hint = Some("retry with the same key".into()),
            Error::SchemaTooNew { .. } => failure.hint = Some("upgrade tiller".into()),
            Error::OwnerMarker(_) => failure.hint = Some(tiller_core::owner::FLIP_BACK.into()),
            Error::Usage(_) | Error::Forbidden(_) | Error::Internal(_) => {}
        }
        failure
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Failure::new("internal", 1, format!("io: {error}"))
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Failure::usage(format!("not valid JSON: {error}"))
    }
}

pub type Done = Result<Outcome, Failure>;

/// A resolved idempotency key and whether the CLI minted it.
pub struct Keyed {
    pub key: String,
    pub minted: bool,
}

/// The command line's view of the host: where state lives and who is calling.
pub struct App {
    pub home: PathBuf,
    pub db: PathBuf,
    /// Who is calling, found on first use: a session id or retro id names it
    /// at once, but telling the owner from a script walks the parent chain,
    /// which a read-only command never needs.
    caller: OnceLock<Caller>,
    /// The key this run resolved and the normalized arguments it carries
    /// (`key`), so every [`App::ctx`] after it sends them with the key and the
    /// store refuses the same key with other arguments (`protocol.md` 6.3).
    keyed: Mutex<Option<(String, Value)>>,
}

/// The resolved caller.
#[derive(Clone)]
struct Caller {
    actor: Actor,
    /// Why a caller with no session id is a script and not the owner: the
    /// owner check it failed (`authority.md` section 3).
    not_owner: Option<String>,
    /// What the owner check saw of an owner who passed it: the terminal and
    /// the parent chain, which every event of their mutations carries
    /// (`authority.md` G23).
    terminal: Option<Value>,
}

impl App {
    pub fn from_env(db: Option<PathBuf>) -> Self {
        let home = std::env::var_os("TILLER_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".tiller")))
            .unwrap_or_else(|| PathBuf::from(".tiller"));
        let db = db.unwrap_or_else(|| home.join("state.db"));
        let caller = OnceLock::new();
        let named = |var: &str| std::env::var(var).ok().filter(|id| !id.is_empty());
        if let Some(id) = named("TILLER_SESSION_ID") {
            let _ = caller.set(Caller::of(Actor::Session(id)));
        } else if let Some(retro) = named("TILLER_RETRO_ID") {
            let _ = caller.set(Caller::of(Actor::Curator(retro)));
        }
        Self {
            home,
            db,
            caller,
            keyed: Mutex::new(None),
        }
    }

    /// Another `App` on the same state for the same caller, resolved now:
    /// what an owner's mutations record of their terminal comes along.
    pub fn same_caller(&self) -> Self {
        let caller = OnceLock::new();
        let _ = caller.set(self.caller().clone());
        Self {
            home: self.home.clone(),
            db: self.db.clone(),
            caller,
            keyed: Mutex::new(None),
        }
    }

    fn caller(&self) -> &Caller {
        self.caller.get_or_init(|| {
            // A call from an agent shell, cron or `ssh host tiller` is never
            // the owner, whatever its environment says.
            match principal::owner_at_terminal() {
                Ok(terminal) => Caller {
                    terminal: Some(terminal.record()),
                    ..Caller::of(Actor::Owner)
                },
                Err(why) => Caller {
                    not_owner: Some(why),
                    ..Caller::of(Actor::Script)
                },
            }
        })
    }

    /// The calling principal. The first call may walk the parent chain.
    pub fn actor(&self) -> &Actor {
        &self.caller().actor
    }

    /// Why a caller with no session id is a script, when it is one.
    pub fn not_owner(&self) -> Option<&str> {
        self.caller().not_owner.as_deref()
    }

    /// The context of a mutation. After [`App::key`] it also carries the
    /// arguments that key was resolved for. The owner's also carries what the
    /// owner check saw, so every owner decision made through the CLI records
    /// its terminal and parent chain (`authority.md` G23).
    pub fn ctx(&self) -> Ctx {
        let caller = self.caller();
        let mut ctx = Ctx::new(caller.actor.clone());
        if let Some(terminal) = &caller.terminal {
            ctx = ctx.with_via_terminal(terminal.clone());
        }
        let keyed = self
            .keyed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match keyed.as_ref() {
            Some((key, normalized)) => ctx.with_args(key, normalized),
            None => ctx,
        }
    }

    /// Binds `normalized` to `key` for the contexts this run builds next.
    pub fn bind_args(&self, key: &str, normalized: &Value) {
        *self
            .keyed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((key.to_owned(), normalized.clone()));
    }

    /// Opens `state.db` for a read. A missing database is not created by a
    /// read: `tiller init` makes it.
    pub fn read(&self) -> Result<Store, Failure> {
        if !self.db.exists() {
            // A session or the curator has a home by construction, so the
            // likely cause is the sandbox hiding it: this `tiller` ran inside
            // it (`authority.md` section 7, "Routes and their controls"). The
            // packaged path is the form that works with or without hooks.
            if matches!(self.actor(), Actor::Session(_) | Actor::Curator(_)) {
                let packaged = crate::stable::tiller_path()
                    .map_or_else(|_| "tiller".into(), |exe| exe.display().to_string());
                return Err(Failure::from(Error::Refused {
                    entity: "host",
                    id: String::new(),
                    fact: "state.db is not readable here: this tiller most likely ran inside the sandbox"
                        .into(),
                })
                .hint(format!(
                    "run tiller as one command on its own, typed as `{packaged} <command>`: not after cd, in a pipe, after && or with a redirect. If it already ran that way, the Tiller home has no state.db yet (tiller init)"
                )));
            }
            return Err(Failure::from(Error::Refused {
                entity: "host",
                id: String::new(),
                fact: "state.db is not initialized; run tiller owner set tiller, then tiller init"
                    .into(),
            })
            .hint(tiller_daemon::api::INIT_HINT));
        }
        Ok(Store::open(&self.db)?)
    }

    /// Opens `state.db` for a mutation: the owner marker must not name
    /// another system, and a session caller must be a live session.
    pub fn write(&self) -> Result<Store, Failure> {
        self.check_owner_marker()?;
        let store = self.read()?;
        self.check_session(&store)?;
        Ok(store)
    }

    /// Opens `state.db`, creating it, for `tiller init` and the commands that
    /// run before any host row exists.
    pub fn create(&self) -> Result<Store, Failure> {
        self.check_owner_marker()?;
        if let Some(dir) = self.db.parent() {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        Ok(Store::open(&self.db)?)
    }

    /// Refuses every mutation while `<home>/owner` names another system
    /// (`recovery.md` R20). A missing marker names none.
    pub fn check_owner_marker(&self) -> Result<(), Failure> {
        Ok(tiller_core::owner::check(&self.home)?)
    }

    pub fn owner_marker(&self) -> Option<String> {
        std::fs::read_to_string(self.home.join("owner"))
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    /// A session id that resolves to no row, or to an ended session, is
    /// refused for every mutation (`authority.md` section 3).
    fn check_session(&self, store: &Store) -> Result<(), Failure> {
        let Actor::Session(id) = self.actor() else {
            return Ok(());
        };
        let found = resolve_id(store.conn(), "sessions", "session", id)
            .and_then(|full| get::<Session>(store.conn(), "session", &full));
        match found {
            Ok(session) if session.ended_at.is_none() => Ok(()),
            _ => Err(Failure::forbidden(format!(
                "session {id} is not a live Tiller session"
            ))),
        }
    }

    /// The calling session's row, when the caller is a session.
    pub fn session(&self, store: &Store) -> Result<Option<Session>, Failure> {
        let Actor::Session(id) = self.actor() else {
            return Ok(None);
        };
        let full = resolve_id(store.conn(), "sessions", "session", id)?;
        Ok(Some(get(store.conn(), "session", &full)?))
    }

    /// Refuses every caller but the owner principal, for the owner-only
    /// commands (G22).
    pub fn require_owner(&self, what: &str) -> Result<(), Failure> {
        if self.actor().is_owner() {
            return Ok(());
        }
        if let Some(why) = self.not_owner() {
            return Err(
                Failure::forbidden(format!("{what} needs the owner at a terminal: {why}"))
                    .hint("run it yourself in a terminal, not from an agent"),
            );
        }
        Err(Failure::forbidden(format!(
            "{what} is owner only; {} may not",
            self.actor()
        )))
    }

    /// The key for a keyed command (6.3): the caller's `--key`, else for a
    /// Tiller session one derived from its arguments and turn, else a fresh
    /// one the response prints so a deliberate retry can repeat it. It also
    /// binds `normalized` to the key, so the same key with other arguments is
    /// refused, not replayed.
    pub fn key(
        &self,
        store: &Store,
        command: &str,
        normalized: &Value,
        explicit: &Option<String>,
    ) -> Result<Keyed, Failure> {
        if let Some(key) = explicit {
            self.bind_args(key, normalized);
            return Ok(Keyed {
                key: key.clone(),
                minted: false,
            });
        }
        if let Actor::Session(session) = self.actor() {
            let digest = Sha256::digest(normalized.to_string().as_bytes());
            let hash: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
            let anchor = turn_anchor(store, session)?;
            let key = format!("auto:{session}:{command}:{hash}:{anchor}");
            self.bind_args(&key, normalized);
            return Ok(Keyed { key, minted: false });
        }
        let key = format!("shell:{}", tiller_core::ids::new_ulid());
        self.bind_args(&key, normalized);
        Ok(Keyed { key, minted: true })
    }
}

/// The id of the session's newest turn-boundary observation, so a retry in one
/// turn replays and the same command after the next boundary is new (6.3).
fn turn_anchor(store: &Store, session: &str) -> Result<String, Failure> {
    let full = resolve_id(store.conn(), "sessions", "session", session)?;
    let anchor: Option<String> = store
        .conn()
        .query_row(
            "SELECT id FROM observations
             WHERE session_id = ?1
               AND (event IN ('UserPromptSubmit', 'wake', 'Stop', 'interrupt', 'transcript:turn_end')
                    OR event LIKE 'StopFailure%')
             ORDER BY observed_at DESC, id DESC LIMIT 1",
            [&full],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
        .map_err(Error::from)?;
    Ok(anchor.unwrap_or_else(|| "0".into()))
}

/// The text an argument carries: `--text <t>`, or `-` for standard input.
pub fn text_input(
    text: Option<String>,
    dash: Option<String>,
    what: &str,
) -> Result<String, Failure> {
    let from_stdin = dash.is_some() || text.as_deref() == Some("-");
    if from_stdin {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        return Ok(buffer.trim_end_matches('\n').to_owned());
    }
    text.ok_or_else(|| {
        Failure::usage(format!(
            "give {what} with --text <t>, or - to read it from standard input"
        ))
    })
}

/// A body: `--body <t>`, `--body-file <f>` (`-` for standard input), or `-`.
pub fn body_input(body: Option<String>, file: Option<&Path>) -> Result<Option<String>, Failure> {
    match (body, file) {
        (Some(_), Some(_)) => Err(Failure::usage("give --body or --body-file, not both")),
        (Some(text), None) if text == "-" => {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            Ok(Some(buffer.trim_end_matches('\n').to_owned()))
        }
        (Some(text), None) => Ok(Some(text)),
        (None, Some(path)) if path == Path::new("-") => {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            Ok(Some(buffer.trim_end_matches('\n').to_owned()))
        }
        (None, Some(path)) => Ok(Some(
            std::fs::read_to_string(path)
                .map_err(|error| {
                    Failure::usage(format!("cannot read {}: {error}", path.display()))
                })?
                .trim_end_matches('\n')
                .to_owned(),
        )),
        (None, None) => Ok(None),
    }
}

impl Caller {
    fn of(actor: Actor) -> Self {
        Self {
            actor,
            not_owner: None,
            terminal: None,
        }
    }
}
