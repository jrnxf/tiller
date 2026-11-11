//! `tiller hook <event>`: map every hook event to an observation and write it
//! straight to `state.db`, with no daemon round trip (`protocol.md` section 4).
//!
//! A hook fails open: whatever goes wrong, it exits 0 and the session goes on
//! (`protocol.md` section 1). A database it cannot write because of its schema
//! gets the observation in `~/.tiller/hook-spool/` instead, and the daemon
//! imports the spool on its next pass (`recovery.md` R11).

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;
use rusqlite::config::DbConfig;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tiller_core::domain::{Activity, ObservationSource, Session};
use tiller_core::error::READ_ONLY_MARKER;
use tiller_core::state::runs::{Bind, NewObservation};
use tiller_core::state::{find, get};
use tiller_core::{Actor, Ctx, Error, Store, Timestamp};

use crate::session::end_reason;
use crate::wake;

/// Every handler in `protocol.md` section 4.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Handler {
    SessionBind,
    Intake,
    Guard,
    PermissionRequest,
    PermissionDenied,
    ToolDone,
    Notify,
    Subagent,
    TurnEnd,
    TurnFailed,
    Instructions,
    Config,
    Cwd,
    Worktree,
    Compact,
    Model,
    Elicitation,
    SessionEnd,
    /// Not registered, so it never runs.
    Ignore,
    /// An event this binary does not map (settings newer than the binary).
    UnknownEvent,
}

impl Handler {
    pub fn as_str(self) -> &'static str {
        match self {
            Handler::SessionBind => "session_bind",
            Handler::Intake => "intake",
            Handler::Guard => "guard",
            Handler::PermissionRequest => "permission_request",
            Handler::PermissionDenied => "permission_denied",
            Handler::ToolDone => "tool_done",
            Handler::Notify => "notify",
            Handler::Subagent => "subagent",
            Handler::TurnEnd => "turn_end",
            Handler::TurnFailed => "turn_failed",
            Handler::Instructions => "instructions",
            Handler::Config => "config",
            Handler::Cwd => "cwd",
            Handler::Worktree => "worktree",
            Handler::Compact => "compact",
            Handler::Model => "model",
            Handler::Elicitation => "elicitation",
            Handler::SessionEnd => "session_end",
            Handler::Ignore => "ignore",
            Handler::UnknownEvent => "unknown_event",
        }
    }
}

/// One row of the event table: the hook event, its handler and the input
/// fields its observation payload keeps.
pub struct EventSpec {
    pub name: &'static str,
    pub handler: Handler,
    pub keep: &'static [&'static str],
}

const fn spec(name: &'static str, handler: Handler, keep: &'static [&'static str]) -> EventSpec {
    EventSpec {
        name,
        handler,
        keep,
    }
}

const TOOL: &[&str] = &["tool_name", "tool_use_id"];

/// The 33 documented hook events, each with exactly one handler
/// (`protocol.md` section 4.2). The `keep` lists are the payload allowlists.
pub const EVENTS: &[EventSpec] = &[
    spec(
        "SessionStart",
        Handler::SessionBind,
        &["source", "model", "transcript_path"],
    ),
    spec("Setup", Handler::Ignore, &[]),
    spec("UserPromptSubmit", Handler::Intake, &["prompt_id"]),
    spec("UserPromptExpansion", Handler::Ignore, &[]),
    spec("PreToolUse", Handler::Guard, TOOL),
    spec(
        "PermissionRequest",
        Handler::PermissionRequest,
        &["tool_name"],
    ),
    spec(
        "PermissionDenied",
        Handler::PermissionDenied,
        &["tool_name", "reason"],
    ),
    spec("PostToolUse", Handler::ToolDone, TOOL),
    spec(
        "PostToolUseFailure",
        Handler::ToolDone,
        &["tool_name", "tool_use_id", "is_interrupt"],
    ),
    spec("PostToolBatch", Handler::Ignore, &[]),
    spec("Notification", Handler::Notify, &["notification_type"]),
    spec("MessageDisplay", Handler::Ignore, &[]),
    spec(
        "SubagentStart",
        Handler::Subagent,
        &["agent_id", "agent_type"],
    ),
    spec(
        "SubagentStop",
        Handler::Subagent,
        &["agent_id", "agent_type"],
    ),
    spec("TaskCreated", Handler::Ignore, &[]),
    spec("TaskCompleted", Handler::Ignore, &[]),
    spec("Stop", Handler::TurnEnd, &["stop_hook_active"]),
    spec("StopFailure", Handler::TurnFailed, &["error"]),
    spec("TeammateIdle", Handler::Ignore, &[]),
    spec(
        "InstructionsLoaded",
        Handler::Instructions,
        &["memory_type", "load_reason"],
    ),
    spec("ConfigChange", Handler::Config, &["source", "file_path"]),
    spec("CwdChanged", Handler::Cwd, &["old_cwd", "new_cwd"]),
    spec("DirectoryAdded", Handler::Cwd, &["directory"]),
    spec("FileChanged", Handler::Ignore, &[]),
    spec(
        "WorktreeCreate",
        Handler::Worktree,
        &["name", "worktree_path"],
    ),
    spec("WorktreeRemove", Handler::Worktree, &["worktree_path"]),
    spec("PreCompact", Handler::Compact, &["trigger"]),
    spec("PostCompact", Handler::Compact, &["trigger"]),
    spec("PreModelSwitch", Handler::Ignore, &[]),
    spec(
        "PostModelSwitch",
        Handler::Model,
        &["model", "previous_model"],
    ),
    spec(
        "Elicitation",
        Handler::Elicitation,
        &["mcp_server_name", "mode"],
    ),
    spec(
        "ElicitationResult",
        Handler::Elicitation,
        &["mcp_server_name", "action"],
    ),
    spec("SessionEnd", Handler::SessionEnd, &["reason"]),
];

/// Input fields no observation ever stores: they can hold whole replies, file
/// contents or secrets (`protocol.md` section 4.1).
pub const NEVER_STORED: &[&str] = &[
    "prompt",
    "last_assistant_message",
    "tool_input",
    "tool_response",
    "error_details",
];

/// The 13 documented `notification_type` values and the verdict each implies
/// (`protocol.md` section 4.4). Any other value implies none.
pub const NOTIFICATION_TYPES: &[(&str, Option<Activity>)] = &[
    ("permission_prompt", Some(Activity::WaitingPermission)),
    ("idle_prompt", Some(Activity::Idle)),
    ("auth_success", None),
    ("elicitation_dialog", Some(Activity::WaitingInput)),
    ("elicitation_url_dialog", Some(Activity::WaitingInput)),
    ("elicitation_complete", Some(Activity::Working)),
    ("elicitation_response", Some(Activity::Working)),
    ("agent_needs_input", Some(Activity::WaitingInput)),
    ("agent_completed", None),
    ("quota_auto_resume_fired", Some(Activity::Working)),
    ("quota_auto_resume_stale", Some(Activity::WaitingInput)),
    ("quota_auto_resume_disabled", Some(Activity::Errored)),
    ("plugin_notification", None),
];

/// How long a tool event's `working` stays fresh before the next one writes
/// a row again (`protocol.md` section 4.3).
pub const TOOL_COALESCE_MS: i64 = 60_000;

/// `~/.tiller/log/hook.log` rotates when a write finds it at this size or
/// more: it is renamed `hook.log.1`, replacing the one before, so the two
/// hold about twice this at most (`protocol.md` section 4.1).
pub const HOOK_LOG_MAX_BYTES: u64 = 1024 * 1024;

/// How many `*.json.bad` spool files [`import_spool`] keeps: the newest by
/// name, which is the spool's ULID, so by age (`protocol.md` section 4.1).
pub const SPOOL_BAD_KEEP: usize = 100;

/// The table row for `event`, if it is a documented one.
pub fn event_spec(event: &str) -> Option<&'static EventSpec> {
    EVENTS.iter().find(|spec| spec.name == event)
}

/// The handler for `event`; an undocumented name gets `unknown_event`.
pub fn handler(event: &str) -> Handler {
    event_spec(event).map_or(Handler::UnknownEvent, |spec| spec.handler)
}

pub fn notification_verdict(notification_type: &str) -> Option<Activity> {
    NOTIFICATION_TYPES
        .iter()
        .find(|(name, _)| *name == notification_type)
        .and_then(|(_, verdict)| *verdict)
}

/// The hook input. The common fields are typed; every other field, including
/// ones a newer Claude Code adds, lands in `extra`, so no new field ever
/// breaks a hook.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct HookInput {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub hook_event_name: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Where a hook runs: the Tiller home (`~/.tiller`), the user's home for
/// relative instruction paths, and the session id Tiller set at launch.
#[derive(Clone, Debug)]
pub struct Env {
    pub tiller_home: PathBuf,
    pub user_home: Option<PathBuf>,
    pub tiller_session_id: Option<String>,
}

/// One hook call, mapped and with its payload already cut to the allowlist,
/// so neither the database nor the spool ever sees a field it must not keep.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observed {
    pub hook_event: String,
    pub key: String,
    pub claude_session_id: Option<String>,
    pub tiller_session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub event: String,
    pub verdict: Option<Activity>,
    pub observed_at: Timestamp,
    pub payload: Value,
    /// The outbox id when the prompt was a wake-up line (the prompt itself is
    /// never kept).
    pub wake_id: Option<String>,
}

/// What a hook call did.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// An observation row was written.
    Recorded { observation_id: String },
    /// The same input was already recorded (its idempotency key matched).
    Replayed,
    /// A tool event while the session is freshly `working` (section 4.3).
    Coalesced,
    /// An `ignore` event: nothing to do.
    Ignored,
    /// The owner marker names another system: its hooks own the session.
    NotOwner,
    /// No `state.db` yet (`tiller init` never ran): nothing is written.
    NoDatabase,
    /// The database could not take the write; the observation is spooled.
    Spooled(PathBuf),
    /// Something failed; it is logged and the session goes on.
    Failed(String),
}

/// What intake adds to one hook call: fields for the observation's payload
/// and the JSON the hook prints for Claude Code (`protocol.md` 2.3, 2.5, 3.2).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    pub payload: Map<String, Value>,
    pub output: Option<Value>,
}

/// The intake protocol (`protocol.md` sections 2 and 3.2). It lives in
/// `tiller-agents` (T4.4) and plugs into the two hooks that carry it. Each
/// call gets the raw input, which holds the fields no observation keeps.
pub trait Intake {
    /// A `UserPromptSubmit` whose prompt is not a wake-up line.
    fn prompt(
        &self,
        store: &mut Store,
        session: &Session,
        prompt: &str,
        input: &HookInput,
    ) -> tiller_core::Result<Reply>;

    /// A `UserPromptSubmit` whose prompt is a wake-up line; `mine` is true
    /// when the outbox row targets this session.
    fn wake(
        &self,
        store: &mut Store,
        session: Option<&Session>,
        outbox_id: &str,
        mine: bool,
    ) -> tiller_core::Result<Reply>;

    /// A `Stop`.
    fn stop(
        &self,
        store: &mut Store,
        session: &Session,
        input: &HookInput,
    ) -> tiller_core::Result<Reply>;
}

/// Runs one hook call end to end. Never fails: the caller exits 0 whatever
/// the outcome.
pub fn run(event: &str, input: &[u8], env: &Env) -> Outcome {
    run_with(event, input, env, None).0
}

/// [`run`], with intake plugged in. Also returns what the hook prints.
pub fn run_with(
    event: &str,
    input: &[u8],
    env: &Env,
    intake: Option<&dyn Intake>,
) -> (Outcome, Option<Value>) {
    if !owner_is_tiller(&env.tiller_home) {
        return (Outcome::NotOwner, None);
    }
    let observed = match observe(event, input, env, Timestamp::now()) {
        Ok(Some(observed)) => observed,
        Ok(None) => return (Outcome::Ignored, None),
        Err(error) => return (fail(env, event, &error), None),
    };
    let db = env.tiller_home.join("state.db");
    if !db.exists() {
        // Not initialized: nothing to record into, and nothing to replay
        // later. Spooling is for a database this binary cannot write (R11).
        return (Outcome::NoDatabase, None);
    }
    // A daemon whose database failed its check wrote this marker: nothing is
    // written until the owner restores (`recovery.md` R10).
    if let Ok(reason) = fs::read_to_string(env.tiller_home.join(READ_ONLY_MARKER)) {
        return (spool(env, &observed, reason.trim()), None);
    }
    // Only the two hooks that carry intake need the raw input again; a tool
    // event's input can be a whole file. `observe` already parsed it, so
    // this cannot fail.
    let intake = intake.filter(|_| matches!(handler(event), Handler::Intake | Handler::TurnEnd));
    let raw: HookInput = match intake {
        Some(_) => serde_json::from_slice(input).unwrap_or_default(),
        None => HookInput::default(),
    };
    let mut output = None;
    let applied = Store::open_current(&db).and_then(|mut store| {
        // With the daemon down the hook holds the last connection, and SQLite
        // would checkpoint the WAL and fsync on close: most of the budget.
        // The daemon's connection and SQLite's auto-checkpoint do that work.
        store
            .conn()
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        let intake = intake.map(|intake| (&raw, intake, &mut output));
        apply_with(&mut store, env, &observed, intake)
    });
    let outcome = match applied {
        Ok(outcome) => outcome,
        Err(error @ (Error::Unavailable(_) | Error::SchemaTooNew { .. })) => {
            spool(env, &observed, &error.to_string())
        }
        Err(error) => fail(env, event, &error.to_string()),
    };
    (outcome, output)
}

/// Maps a hook call to its observation. `None` for an `ignore` event.
pub fn observe(
    event: &str,
    input: &[u8],
    env: &Env,
    now: Timestamp,
) -> Result<Option<Observed>, String> {
    let handler = handler(event);
    if handler == Handler::Ignore {
        return Ok(None);
    }
    let input: HookInput =
        serde_json::from_slice(input).map_err(|error| format!("unreadable hook input: {error}"))?;
    let field = |name: &str| input.extra.get(name).and_then(Value::as_str);

    let mut payload = Map::new();
    if handler == Handler::UnknownEvent {
        for (name, value) in &input.extra {
            if !NEVER_STORED.contains(&name.as_str()) {
                payload.insert(name.clone(), value.clone());
            }
        }
    } else if let Some(spec) = event_spec(event) {
        for name in spec.keep {
            let value = match *name {
                "transcript_path" => input.transcript_path.clone().map(Value::from),
                _ => input.extra.get(*name).cloned(),
            };
            if let Some(value) = value {
                payload.insert((*name).to_owned(), value);
            }
        }
    }
    if let Some(id) = &input.session_id {
        payload.insert("claude_session_id".into(), Value::from(id.as_str()));
    }

    let mut wake_id = None;
    let (name, verdict) = match handler {
        Handler::SessionBind => (
            format!("SessionStart:{}", field("source").unwrap_or("unknown")),
            None,
        ),
        Handler::Intake => {
            wake_id = field("prompt").and_then(wake::outbox_id).map(str::to_owned);
            (event.to_owned(), Some(Activity::Working))
        }
        Handler::Guard | Handler::ToolDone => (event.to_owned(), Some(Activity::Working)),
        Handler::Notify => {
            let kind = field("notification_type").unwrap_or("unknown");
            (format!("Notification:{kind}"), notification_verdict(kind))
        }
        Handler::Subagent => {
            let verdict = (event == "SubagentStart").then_some(Activity::Working);
            (event.to_owned(), verdict)
        }
        Handler::TurnEnd => {
            let background = count(input.extra.get("background_tasks"));
            let crons = count(input.extra.get("session_crons"));
            payload.insert("background_tasks".into(), json!(background));
            payload.insert("session_crons".into(), json!(crons));
            payload.insert("waiting_on_own_work".into(), json!(background + crons > 0));
            (event.to_owned(), Some(Activity::Idle))
        }
        Handler::TurnFailed => (
            format!("StopFailure:{}", field("error").unwrap_or("unknown")),
            Some(Activity::Errored),
        ),
        Handler::Instructions => {
            if let Some(path) = field("file_path") {
                let (shown, sha256) = instructions_file(path, input.cwd.as_deref(), env);
                payload.insert("file_path".into(), json!(shown));
                payload.insert("sha256".into(), json!(sha256));
            }
            (event.to_owned(), None)
        }
        Handler::Elicitation => {
            let verdict = (event == "ElicitationResult").then_some(Activity::Working);
            (event.to_owned(), verdict)
        }
        Handler::SessionEnd => {
            let reason = field("reason").unwrap_or("other");
            let verdict = match reason {
                "clear" | "resume" => None,
                _ => Some(Activity::Ended),
            };
            (format!("SessionEnd:{reason}"), verdict)
        }
        Handler::PermissionRequest
        | Handler::PermissionDenied
        | Handler::Config
        | Handler::Cwd
        | Handler::Worktree
        | Handler::Compact
        | Handler::Model
        | Handler::UnknownEvent => (event.to_owned(), None),
        Handler::Ignore => return Ok(None),
    };

    let mut hashed = input_bytes_for_key(&input);
    hashed.extend_from_slice(position(&input, now).as_bytes());
    let digest = hex(&Sha256::digest(hashed));
    let key = format!(
        "hook:{}:{event}:{digest}",
        input.session_id.as_deref().unwrap_or("-")
    );
    Ok(Some(Observed {
        hook_event: event.to_owned(),
        key,
        claude_session_id: input.session_id.clone(),
        tiller_session_id: env.tiller_session_id.clone(),
        transcript_path: input.transcript_path.clone(),
        event: name,
        verdict,
        observed_at: now,
        payload: Value::Object(payload),
        wake_id,
    }))
}

/// Writes one mapped hook call: binds or ends the session where the event
/// says so, and appends the observation under its idempotency key.
pub fn apply(store: &mut Store, env: &Env, observed: &Observed) -> tiller_core::Result<Outcome> {
    apply_with(store, env, observed, None)
}

/// The raw input, the intake to run and where its output goes.
type IntakeCall<'a> = (&'a HookInput, &'a dyn Intake, &'a mut Option<Value>);

/// [`apply`], running intake on the hooks that carry it. A spooled
/// observation is applied without intake: its raw input is gone.
fn apply_with(
    store: &mut Store,
    env: &Env,
    observed: &Observed,
    mut intake: Option<IntakeCall<'_>>,
) -> tiller_core::Result<Outcome> {
    let handler = handler(&observed.hook_event);
    let ctx = Ctx::new(Actor::Hook);
    let mut session = resolve_session(store, observed)?;
    let mut event = observed.event.clone();
    let mut payload = observed.payload.clone();

    match handler {
        Handler::SessionBind => {
            if let (Some(found), Some(claude_id)) = (&session, &observed.claude_session_id) {
                let source = observed
                    .event
                    .trim_start_matches("SessionStart:")
                    .to_owned();
                let bound = store.run_bind_session(
                    &ctx,
                    Bind {
                        session_id: found.id.clone(),
                        claude_session_id: claude_id.clone(),
                        pane_id: None,
                        transcript_path: observed.transcript_path.clone(),
                        source: source.clone(),
                    },
                );
                match bound {
                    Ok(applied) => {
                        // The appended prompt survives a resume, `/clear`
                        // and `/compact`, so the session keeps the memory
                        // its launch rendered: copy that set under the new
                        // trigger (`memory.md` section 7). The launch wrote
                        // the `startup` rows itself.
                        if matches!(source.as_str(), "resume" | "clear" | "compact")
                            && let Err(error) = store.mem_load_again(
                                &ctx,
                                &applied.value.id,
                                &source,
                                observed.observed_at,
                            )
                        {
                            insert(
                                &mut payload,
                                "memory_load_refused",
                                json!(error.to_string()),
                            );
                        }
                        session = Some(applied.value);
                    }
                    Err(error) => {
                        insert(&mut payload, "bind_refused", json!(error.to_string()));
                    }
                }
            }
        }
        Handler::Intake => {
            if let Some(id) = &observed.wake_id {
                let target: Option<String> = store
                    .conn()
                    .query_row(
                        "SELECT target_session_id FROM outbox WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .optional()?;
                let mine = matches!((&target, &session), (Some(t), Some(s)) if *t == s.id);
                event = if mine { "wake" } else { "wake_mismatch" }.to_owned();
                insert(&mut payload, "outbox_id", json!(id));
                if let Some((_, intake, output)) = &mut intake {
                    let reply = intake.wake(store, session.as_ref(), id, mine);
                    merge_reply(env, &observed.hook_event, reply, &mut payload, output);
                }
            } else if let (Some((raw, intake, output)), Some(found)) = (&mut intake, &session)
                && let Some(prompt) = raw.extra.get("prompt").and_then(Value::as_str)
            {
                let reply = intake.prompt(store, found, prompt, raw);
                merge_reply(env, &observed.hook_event, reply, &mut payload, output);
            }
        }
        Handler::TurnEnd => {
            if let (Some((raw, intake, output)), Some(found)) = (&mut intake, &session) {
                let reply = intake.stop(store, found, raw);
                merge_reply(env, &observed.hook_event, reply, &mut payload, output);
            }
        }
        Handler::Guard | Handler::ToolDone => {
            if let Some(found) = &session
                && fresh_working(found, observed.observed_at)
            {
                return Ok(Outcome::Coalesced);
            }
        }
        Handler::UnknownEvent => {
            let seen: Option<i64> = store
                .conn()
                .query_row(
                    "SELECT 1 FROM observations WHERE source = 'hook' AND event = ?1 LIMIT 1",
                    [&event],
                    |row| row.get(0),
                )
                .optional()?;
            if seen.is_none() {
                log(
                    env,
                    &format!("unknown hook event {event:?}; extend the table in protocol.md 4.2"),
                );
            }
        }
        _ => {}
    }

    let recorded = store.observation_record_keyed(
        &ctx,
        Some(&observed.key),
        NewObservation {
            session_id: session.as_ref().map(|s| s.id.clone()),
            run_id: session.as_ref().and_then(|s| s.run_id.clone()),
            source: ObservationSource::Hook,
            event,
            verdict: observed.verdict,
            observed_at: observed.observed_at,
            payload,
        },
    )?;
    // A redelivered SessionEnd still ends the session: the first delivery
    // may have recorded its observation and then failed before the end.
    if handler == Handler::SessionEnd
        && observed.verdict == Some(Activity::Ended)
        && let Some(found) = &session
        && found.ended_at.is_none()
    {
        let reason = observed.event.trim_start_matches("SessionEnd:");
        let reason = end_reason(store.conn(), found, reason)?;
        store.session_end(&ctx, &found.id, &reason)?;
    }
    if recorded.replayed {
        return Ok(Outcome::Replayed);
    }
    Ok(Outcome::Recorded {
        observation_id: recorded.value.observation.id,
    })
}

/// Imports every spooled observation into `store`, oldest first, and removes
/// each file once it is written. The daemon calls this on each pass. A busy
/// database stops the pass, to retry on the next; a file that does not parse
/// or that fails to apply is moved aside with a `.bad` suffix and logged, so
/// one bad file never blocks the rest. Each pass that gets through the spool
/// then keeps only the newest [`SPOOL_BAD_KEEP`] bad files.
pub fn import_spool(store: &mut Store, env: &Env) -> tiller_core::Result<usize> {
    let dir = env.tiller_home.join("hook-spool");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(0);
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let mut imported = 0;
    for path in files {
        let parsed = fs::read(&path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<Observed>(&bytes).map_err(|error| error.to_string())
            });
        match parsed {
            Ok(observed) => match apply(store, env, &observed) {
                Ok(_) => {
                    fs::remove_file(&path).map_err(|error| Error::Internal(error.to_string()))?;
                    imported += 1;
                }
                Err(error @ Error::Unavailable(_)) => return Err(error),
                Err(error) => {
                    log(
                        env,
                        &format!("spool file {} failed: {error}", path.display()),
                    );
                    let _ = fs::rename(&path, path.with_extension("json.bad"));
                }
            },
            Err(error) => {
                log(
                    env,
                    &format!("unreadable spool file {}: {error}", path.display()),
                );
                let _ = fs::rename(&path, path.with_extension("json.bad"));
            }
        }
    }
    prune_bad_spool(env, &dir);
    Ok(imported)
}

/// The Tiller session: the launch's `TILLER_SESSION_ID`, then the session
/// bound to the Claude id, then none (`protocol.md` section 4.1).
fn resolve_session(store: &Store, observed: &Observed) -> tiller_core::Result<Option<Session>> {
    if let Some(id) = &observed.tiller_session_id
        && let Ok(session) = get::<Session>(store.conn(), "session", id)
    {
        return Ok(Some(session));
    }
    match &observed.claude_session_id {
        Some(claude_id) => find(store.conn(), "claude_session_id = ?1", &[claude_id]),
        None => Ok(None),
    }
}

fn fresh_working(session: &Session, now: Timestamp) -> bool {
    session.activity == Activity::Working
        && session
            .activity_observed_at
            .is_some_and(|at| now.as_millis() - at.as_millis() < TOOL_COALESCE_MS)
}

/// `~/.tiller/owner` absent or saying `tiller`. Any other name means another
/// system's hooks own the session (`recovery.md` R20).
fn owner_is_tiller(home: &Path) -> bool {
    match fs::read_to_string(home.join("owner")) {
        Ok(text) => text.trim() == "tiller",
        Err(_) => true,
    }
}

fn spool(env: &Env, observed: &Observed, why: &str) -> Outcome {
    let dir = env.tiller_home.join("hook-spool");
    let path = dir.join(format!("{}.json", tiller_core::ids::new_ulid()));
    let written = fs::create_dir_all(&dir)
        .and_then(|()| serde_json::to_vec(observed).map_err(std::io::Error::other))
        .and_then(|bytes| fs::write(&path, bytes));
    match written {
        Ok(()) => {
            log(env, &format!("spooled {} ({why})", observed.hook_event));
            Outcome::Spooled(path)
        }
        Err(error) => fail(
            env,
            &observed.hook_event,
            &format!("{why}; spool failed: {error}"),
        ),
    }
}

fn fail(env: &Env, event: &str, error: &str) -> Outcome {
    log(env, &format!("{event}: {error}"));
    Outcome::Failed(error.to_owned())
}

/// Appends one line to `~/.tiller/log/hook.log`. Best effort.
///
/// A log at [`HOOK_LOG_MAX_BYTES`] or more is first rotated to
/// `hook.log.1`. The size check is one `fstat` on the file already open,
/// and the rename happens once per `HOOK_LOG_MAX_BYTES` of log. Measured in
/// a release build on Linux: the `fstat` takes about 0.5 µs and a whole
/// call about 5 µs, and a call that rotates about 50 µs (0.5 ms at worst
/// in 50 tries), all far inside the hook's 10 ms budget.
fn log(env: &Env, line: &str) {
    use std::os::unix::fs::MetadataExt as _;
    let dir = env.tiller_home.join("log");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("hook.log");
    let open = || fs::OpenOptions::new().create(true).append(true).open(&path);
    let Ok(mut file) = open() else {
        return;
    };
    if let Ok(opened) = file.metadata()
        && opened.len() >= HOOK_LOG_MAX_BYTES
    {
        // Rename only the file this call opened: a hook that rotated it a
        // moment ago has put a fresh log in its place, which stays.
        let same = fs::metadata(&path)
            .is_ok_and(|now| (now.dev(), now.ino()) == (opened.dev(), opened.ino()));
        if (!same || fs::rename(&path, dir.join("hook.log.1")).is_ok())
            && let Ok(fresh) = open()
        {
            file = fresh;
        }
    }
    let _ = writeln!(file, "{} {line}", Timestamp::now());
}

/// Deletes all but the newest [`SPOOL_BAD_KEEP`] `*.json.bad` files in the
/// spool `dir`, logging how many went. Best effort; the daemon's pass only.
fn prune_bad_spool(env: &Env, dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut bad: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".json.bad"))
        })
        .collect();
    if bad.len() <= SPOOL_BAD_KEEP {
        return;
    }
    bad.sort();
    let old = bad.len() - SPOOL_BAD_KEEP;
    let removed = bad[..old]
        .iter()
        .filter(|path| fs::remove_file(path).is_ok())
        .count();
    log(
        env,
        &format!("deleted {removed} old bad spool files; the newest {SPOOL_BAD_KEEP} are kept"),
    );
}

/// Folds an intake reply into the observation. Intake fails open like the
/// hook: an error is logged and noted in the payload, and the observation is
/// still written.
fn merge_reply(
    env: &Env,
    event: &str,
    reply: tiller_core::Result<Reply>,
    payload: &mut Value,
    output: &mut Option<Value>,
) {
    match reply {
        Ok(reply) => {
            for (name, value) in reply.payload {
                insert(payload, &name, value);
            }
            *output = reply.output;
        }
        Err(error) => {
            log(env, &format!("{event} intake: {error}"));
            insert(payload, "intake_error", json!(error.to_string()));
        }
    }
}

fn insert(payload: &mut Value, name: &str, value: Value) {
    if let Value::Object(map) = payload {
        map.insert(name.to_owned(), value);
    }
}

/// Where in the session this hook fired, so that two real events with the
/// same input (a second identical permission prompt, a repeated "continue")
/// are not taken for one redelivered event. It is the transcript's length,
/// which grows between turns and tool calls, the same marker `ask.record`
/// keys on (`protocol.md` section 2.2); without a readable transcript, the
/// 2-second window the hook fired in.
fn position(input: &HookInput, now: Timestamp) -> String {
    input
        .transcript_path
        .as_deref()
        .and_then(|path| fs::metadata(path).ok())
        .map_or_else(
            || format!(":t{}", now.as_millis().div_euclid(2000)),
            |meta| format!(":b{}", meta.len()),
        )
}

/// The input's bytes for the idempotency key: the parsed input re-serialized,
/// so whitespace differences in a redelivery do not make a second row.
fn input_bytes_for_key(input: &HookInput) -> Vec<u8> {
    serde_json::to_vec(input).unwrap_or_default()
}

fn count(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(items)) => items.len(),
        Some(Value::Object(map)) => map.len(),
        Some(Value::Null) | None => 0,
        Some(_) => 1,
    }
}

/// An instructions file as the payload names it: relative to the project or
/// to `~`, with the sha256 of its bytes (null when unreadable).
fn instructions_file(path: &str, cwd: Option<&str>, env: &Env) -> (String, Option<String>) {
    let sha256 = fs::read(path).ok().map(|bytes| hex(&Sha256::digest(bytes)));
    let file = Path::new(path);
    let shown = cwd
        .and_then(|cwd| file.strip_prefix(cwd).ok())
        .map(|rel| rel.display().to_string())
        .or_else(|| {
            env.user_home
                .as_deref()
                .and_then(|home| file.strip_prefix(home).ok())
                .map(|rel| format!("~/{}", rel.display()))
        })
        .unwrap_or_else(|| path.to_owned());
    (shown, sha256)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
