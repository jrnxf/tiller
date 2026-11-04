//! The PreToolUse backstop (`authority.md` section 9): `tiller guard pretool
//! --role <role>` sees every tool call of every fleet role and denies the
//! protected actions in every form section 10 lists.
//!
//! [`decide`] is the policy and touches nothing: it reads the tool call, the
//! role, and the [`Facts`] about the run, which it asks for only when a rule
//! needs them, so a call with no protected token never reads `state.db`.
//! [`pretool`] is the hook end to end: it parses the payload, loads the facts,
//! records a denial as an observation before it answers, and renders the
//! answer in the transport Claude Code reads.

mod bash;
pub mod shell;

#[cfg(test)]
mod live;
#[cfg(test)]
mod tests;

use std::cell::OnceCell;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiller_core::domain::{Card, ObservationSource, Project, Run, Session};
use tiller_core::state::get;
use tiller_core::state::runs::NewObservation;
use tiller_core::{Actor, Ctx, Store, Timestamp};

use crate::roles::Role;

/// The stable deny codes (`authority.md` section 9, "Output").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    Merge,
    DefaultPush,
    ForeignBranch,
    HttpClient,
    OwnerOnly,
    Identity,
    StateWrite,
    GuardTamper,
    WorktreeAdmin,
    Delegation,
    PersistentCd,
    /// A peer link: `tiller peer`, or SSH to another host's `tiller` (G43).
    Peer,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Merge => "merge",
            Code::DefaultPush => "default-push",
            Code::ForeignBranch => "foreign-branch",
            Code::HttpClient => "http-client",
            Code::OwnerOnly => "owner-only",
            Code::Identity => "identity",
            Code::StateWrite => "state-write",
            Code::GuardTamper => "guard-tamper",
            Code::WorktreeAdmin => "worktree-admin",
            Code::Delegation => "delegation",
            Code::PersistentCd => "persistent-cd",
            Code::Peer => "peer",
        }
    }

    /// The one sentence that names the allowed path.
    pub fn allowed_path(self) -> &'static str {
        match self {
            Code::Merge => {
                "merges go through the daemon: ask the orchestrator to request a merge approval"
            }
            Code::DefaultPush => {
                "push only your own branch, without force: git push origin HEAD:<your branch>"
            }
            Code::ForeignBranch => {
                "only a builder pushes, and only its own run branch; anything else goes through the orchestrator"
            }
            Code::HttpClient => {
                "no HTTP clients: read the forge with gh-axi and the web with WebFetch"
            }
            Code::OwnerOnly => {
                "this tiller command is the owner's: hand the owner the one command to run"
            }
            Code::Identity => {
                "run tiller from the Bash tool as yourself, with the launch environment"
            }
            Code::StateWrite => "~/.tiller is reached only through tiller commands",
            Code::GuardTamper => {
                "a fleet session cannot change its own guards or which tiller runs; ask the owner"
            }
            Code::WorktreeAdmin => "Tiller owns worktrees and panes: ask the orchestrator",
            Code::Delegation => {
                "start work through Tiller (a card the orchestrator spawns), not a nested agent or a scheduler"
            }
            Code::PersistentCd => {
                "do not move the orchestrator shell into a checkout: use git -C <dir> or a ( cd <dir> && ... ) subshell"
            }
            Code::Peer => {
                "another host's records change only from the owner's board: hand the owner the one command to run"
            }
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One deny: its code, why, and the classified command (never the payload).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denial {
    pub code: Code,
    pub why: String,
    pub command: String,
}

impl Denial {
    pub(crate) fn new(code: Code, why: impl Into<String>, command: impl Into<String>) -> Self {
        Denial {
            code,
            why: why.into(),
            command: command.into(),
        }
    }

    /// `[code] why; allowed path.`
    pub fn message(&self) -> String {
        format!(
            "[{}] {}; {}.",
            self.code,
            self.why,
            self.code.allowed_path()
        )
    }
}

/// What the backstop knows about the run, loaded from `state.db` only when a
/// rule asks for it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunFacts {
    /// `runs.branch`: the one branch a builder may push.
    pub branch: Option<String>,
    /// The project's default branch.
    pub default_branch: Option<String>,
}

/// Where the guard runs.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub tiller_home: PathBuf,
    pub user_home: Option<PathBuf>,
    pub tiller_session_id: Option<String>,
    pub tiller_run_id: Option<String>,
    /// The packaged `tiller`, the only one the sandbox exempts: the running
    /// binary, since the fragment starts the guard by that path.
    pub packaged_tiller: Option<PathBuf>,
    /// The orchestrator's protected roots: the project checkouts and the
    /// worktree pool (`--checkouts`, `--pool`).
    pub protected_roots: Vec<PathBuf>,
}

/// One call's context: the role, the environment, the payload's `cwd`, and
/// the run facts, loaded at most once.
pub struct Context<'a> {
    pub role: Role,
    pub env: &'a Env,
    pub cwd: PathBuf,
    facts: OnceCell<RunFacts>,
    load: &'a dyn Fn() -> RunFacts,
}

impl<'a> Context<'a> {
    pub fn new(role: Role, env: &'a Env, cwd: PathBuf, load: &'a dyn Fn() -> RunFacts) -> Self {
        Context {
            role,
            env,
            cwd,
            facts: OnceCell::new(),
            load,
        }
    }

    pub(crate) fn facts(&self) -> &RunFacts {
        self.facts.get_or_init(|| (self.load)())
    }

    /// True once a rule has asked for the run facts.
    pub fn read_facts(&self) -> bool {
        self.facts.get().is_some()
    }
}

/// The delegation stems (Firstmate's subagent-guard), matched against a tool
/// name lowercased with every non-alphanumeric character removed.
const DELEGATION_STEMS: &[&str] = &[
    "agent",
    "subagent",
    "task",
    "workflow",
    "cron",
    "schedul",
    "worktree",
    "delegate",
    "spawn",
    "dispatch",
    "handoff",
    "remote",
    "sendmessage",
    "monitor",
];

/// Tools that only read the session's own background work.
const OBSERVE_ONLY: &[&str] = &[
    "taskoutput",
    "taskstop",
    "taskget",
    "tasklist",
    "cronlist",
    "bashoutput",
    "killshell",
    "managesubagents",
    "managetask",
];

/// Tools that write only the session's own todo list.
const PLAN_ONLY: &[&str] = &["taskcreate", "taskupdate"];

/// `Agent` types a read-only role may start.
const READ_ONLY_AGENTS: &[&str] = &["Explore"];

fn normalized_tool(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The decision for one tool call; `None` allows it.
pub fn decide(tool: &str, input: &Value, ctx: &Context<'_>) -> Option<Denial> {
    let field = |name: &str| input.get(name).and_then(Value::as_str);
    if let Some(rest) = tool.strip_prefix("mcp__") {
        let lower = rest.to_ascii_lowercase();
        return [
            "merge",
            "push",
            "delete",
            "create_ref",
            "update_ref",
            "create_file",
            "update_file",
        ]
        .iter()
        .any(|word| lower.contains(word))
        .then(|| {
            Denial::new(
                Code::Merge,
                format!("{tool} is a forge write tool"),
                tool.to_owned(),
            )
        });
    }
    match tool {
        "Bash" => return field("command").and_then(|command| bash::classify(command, ctx)),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let path = field("file_path").or_else(|| field("notebook_path"))?;
            return path_denial(path, true, ctx)
                .map(|(code, why)| Denial::new(code, why, format!("{tool} {path}")));
        }
        "Read" | "Grep" | "Glob" | "LS" => {
            for name in ["file_path", "path", "pattern"] {
                if let Some(path) = field(name)
                    && let Some((code, why)) = path_denial(path, false, ctx)
                {
                    return Some(Denial::new(code, why, format!("{tool} {path}")));
                }
            }
            return None;
        }
        "EnterWorktree" | "ExitWorktree" => {
            return Some(Denial::new(
                Code::WorktreeAdmin,
                format!("{tool} changes worktrees"),
                tool,
            ));
        }
        "CronCreate" | "CronDelete" | "ScheduleWakeup" | "RemoteTrigger" | "Workflow"
        | "PushNotification" => {
            return Some(Denial::new(
                Code::Delegation,
                format!("{tool} starts or schedules work Tiller does not track"),
                tool,
            ));
        }
        "Agent" | "Task" => {
            if ctx.role == Role::Orchestrator {
                return Some(Denial::new(
                    Code::Delegation,
                    "the orchestrator does not start subagents",
                    tool,
                ));
            }
            if input
                .get("isolation")
                .is_some_and(|value| !value.is_null() && value != "")
            {
                return Some(Denial::new(
                    Code::WorktreeAdmin,
                    "a subagent never gets its own worktree",
                    tool,
                ));
            }
            if ctx.role != Role::Builder {
                let kind = field("subagent_type").unwrap_or("general-purpose");
                if !READ_ONLY_AGENTS.contains(&kind) {
                    return Some(Denial::new(
                        Code::Delegation,
                        format!(
                            "a {} starts only read-only subagents, not {kind}",
                            ctx.role.name()
                        ),
                        format!("{tool} {kind}"),
                    ));
                }
            }
            return None;
        }
        _ => {}
    }
    if ctx.role == Role::Orchestrator {
        let name = normalized_tool(tool);
        if !OBSERVE_ONLY.contains(&name.as_str())
            && !PLAN_ONLY.contains(&name.as_str())
            && DELEGATION_STEMS.iter().any(|stem| name.contains(stem))
        {
            return Some(Denial::new(
                Code::Delegation,
                format!("{tool} is delegation-shaped"),
                tool,
            ));
        }
    }
    None
}

/// A path's expansion: `~` and `$HOME` to the user's home, relative to the
/// call's directory, `.` and `..` folded without touching the disk.
pub(crate) fn resolve(text: &str, ctx: &Context<'_>) -> PathBuf {
    let home = ctx.env.user_home.clone().unwrap_or_default();
    let expanded = if text == "~" {
        home.clone()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home.join(rest)
    } else if let Some(rest) = text
        .strip_prefix("$HOME")
        .or_else(|| text.strip_prefix("${HOME}"))
    {
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() {
            home.clone()
        } else {
            home.join(rest)
        }
    } else {
        PathBuf::from(text)
    };
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        ctx.cwd.join(expanded)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn has_segment(text: &str, segment: &str) -> bool {
    text.split('/').any(|part| part == segment)
}

/// Whether `text` (a word or a tool path) names a protected path. Any access
/// to the Tiller home is `[state-write]`; a write into a guard file is
/// `[guard-tamper]`. A read matches only a path that resolves into the Tiller
/// home; a write also matches a `.tiller` segment or `TILLER_HOME` in the
/// text, since the shell may resolve it from a directory the guard does not
/// know.
pub(crate) fn path_denial(text: &str, writes: bool, ctx: &Context<'_>) -> Option<(Code, String)> {
    let path = resolve(text, ctx);
    let tiller_home = &ctx.env.tiller_home;
    let in_home = !tiller_home.as_os_str().is_empty() && path.starts_with(tiller_home);
    if in_home || (writes && (has_segment(text, ".tiller") || text.contains("TILLER_HOME"))) {
        return Some((Code::StateWrite, format!("{text} is in the Tiller home")));
    }
    if !writes {
        return None;
    }
    let shown = path.to_string_lossy();
    let guard_file = has_segment(&shown, ".claude")
        || has_segment(text, ".claude")
        || path
            .file_name()
            .is_some_and(|name| name == ".claude.json" || name == ".gitconfig")
        || shown.contains("/.git/hooks")
        || text.contains(".git/hooks")
        || shown.ends_with("/.git/config")
        || shown.contains("/.config/git/");
    guard_file.then(|| {
        (
            Code::GuardTamper,
            format!("{text} holds settings, hooks or git config"),
        )
    })
}

/// Whether a word names a secret file (`authority.md` G32): `tiller` reads
/// files outside the sandbox, so it must never be handed one.
pub(crate) fn is_secret_path(text: &str) -> bool {
    let name = text.rsplit('/').next().unwrap_or(text);
    name == ".env"
        || name.starts_with(".env.")
        || name == ".envrc"
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || has_segment(text, ".ssh")
}

/// The hook transport for Claude Code (pinned against the installed CLI at
/// T4.3, `docs/design/host-capabilities.md`): a deny exits 2 with this JSON
/// on stderr and nothing on stdout; an allow exits 0 with no output, so the
/// normal permission flow still runs.
pub fn render_deny(denial: &Denial) -> String {
    let message = denial.message();
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": message,
        },
        "systemMessage": message,
    })
    .to_string()
}

/// `tiller guard pretool` end to end. Returns the deny to print, or `None`
/// to allow. A payload that does not parse allows: a broken hook must never
/// deny every tool call, and the deny rules and the sandbox still hold.
pub fn pretool(role: Role, input: &[u8], env: &Env) -> Option<Denial> {
    let payload: Value = serde_json::from_slice(input).ok()?;
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    let tool_input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();
    let claude_session = payload.get("session_id").and_then(Value::as_str);
    let db = env.tiller_home.join("state.db");
    let store = OnceCell::new();
    let open = || -> Option<Store> { db.exists().then(|| Store::open_current(&db).ok()).flatten() };
    let load = || {
        store
            .get_or_init(open)
            .as_ref()
            .map(|store| facts(store, env, claude_session))
            .unwrap_or_default()
    };
    let ctx = Context::new(role, env, cwd, &load);
    let denial = decide(tool, &tool_input, &ctx)?;
    drop(ctx);
    let mut store = store.into_inner().unwrap_or_else(open);
    if let Some(store) = store.as_mut() {
        let key = match payload.get("tool_use_id").and_then(Value::as_str) {
            Some(call) => format!("pretool:{}:{call}", claude_session.unwrap_or_default()),
            None => format!("pretool:{}", hex(&Sha256::digest(input))),
        };
        record(store, env, role, tool, claude_session, &key, &denial);
    }
    Some(denial)
}

fn session(store: &Store, env: &Env, claude_session: Option<&str>) -> Option<Session> {
    if let Some(id) = &env.tiller_session_id
        && let Ok(session) = get::<Session>(store.conn(), "session", id)
    {
        return Some(session);
    }
    let claude = claude_session?;
    tiller_core::state::find(store.conn(), "claude_session_id = ?1", &[&claude])
        .ok()
        .flatten()
}

fn facts(store: &Store, env: &Env, claude_session: Option<&str>) -> RunFacts {
    let session = session(store, env, claude_session);
    let run_id = session
        .as_ref()
        .and_then(|session| session.run_id.clone())
        .or_else(|| env.tiller_run_id.clone());
    let run = run_id.and_then(|id| get::<Run>(store.conn(), "run", &id).ok());
    let project = match (&run, &session) {
        (Some(run), _) => get::<Card>(store.conn(), "card", &run.card_id)
            .ok()
            .map(|card| card.project_id),
        (None, Some(session)) => Some(session.project_id.clone()),
        (None, None) => None,
    }
    .and_then(|id| get::<Project>(store.conn(), "project", &id).ok());
    RunFacts {
        branch: run.and_then(|run| run.branch),
        default_branch: project.map(|project| project.default_branch),
    }
}

/// Writes the denial's observation (G38). Its key is the tool call's id,
/// or the payload's digest when the harness sends none, so a redelivered
/// hook records once.
fn record(
    store: &mut Store,
    env: &Env,
    role: Role,
    tool: &str,
    claude_session: Option<&str>,
    key: &str,
    denial: &Denial,
) {
    let found = session(store, env, claude_session);
    let payload = json!({
        "code": denial.code.as_str(),
        "role": role.name(),
        "tool": tool,
        "command": denial.command,
        "claude_session_id": claude_session,
    });
    let _ = store.observation_record_keyed(
        &Ctx::new(Actor::Hook),
        Some(key),
        NewObservation {
            session_id: found.as_ref().map(|session| session.id.clone()),
            run_id: found.and_then(|session| session.run_id),
            source: ObservationSource::Hook,
            event: "PreToolUse:denied".into(),
            verdict: None,
            observed_at: Timestamp::now(),
            payload,
        },
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Whether `path` is the packaged binary, comparing resolved paths.
pub(crate) fn is_packaged(path: &Path, packaged: Option<&Path>) -> bool {
    let Some(packaged) = packaged else {
        return false;
    };
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canonical(path) == canonical(packaged)
}
