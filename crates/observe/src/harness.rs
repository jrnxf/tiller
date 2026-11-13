//! Harness adapters (D04; `protocol.md` section 10, `authority.md` G35).
//!
//! `sessions.harness` records which agent harness runs a session. Claude Code
//! is the reference: its hook events are the table in `protocol.md` 4.2, and
//! `hooks::EVENTS` is that table. Every other harness has an adapter table of
//! its own, like 4.2, that maps its native event names onto the same
//! vocabulary, so one `observations.event` and verdict set serves them all.
//! `tiller hook --harness <h> <event>` runs the adapter first and the shared
//! pipeline after, so a harness changes the mapping and nothing else.
//!
//! An adapter observes; it does not yet guard. G35 refuses a role on a
//! harness unless Tiller carries its tool calls to `tiller guard` and the
//! harness has an equivalent of the sandbox, and only Claude Code has both
//! today. A harness with no adapter is observed through the terminal holder
//! and liveness only, as a reduced host is.

use serde_json::{Map, Value};

/// An agent harness Tiller can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Codex,
    Grok,
    OpenCode,
    Pi,
    Omp,
    Cursor,
}

impl Harness {
    /// Every harness with an adapter, Claude Code first.
    pub const ALL: [Harness; 7] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Grok,
        Harness::OpenCode,
        Harness::Pi,
        Harness::Omp,
        Harness::Cursor,
    ];

    /// The names `--harness` and `sessions.harness` use.
    pub const NAMES: [&'static str; 7] =
        ["claude", "codex", "grok", "opencode", "pi", "omp", "cursor"];

    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Grok => "grok",
            Harness::OpenCode => "opencode",
            Harness::Pi => "pi",
            Harness::Omp => "omp",
            Harness::Cursor => "cursor",
        }
    }

    /// `None` for a harness with no adapter.
    pub fn parse(name: &str) -> Option<Harness> {
        Harness::ALL
            .into_iter()
            .find(|harness| harness.as_str() == name)
    }

    /// What Tiller provides for this harness today (G35).
    pub fn capabilities(self) -> Capabilities {
        match self {
            Harness::Claude => Capabilities {
                pretool_transport: true,
                sandbox: true,
            },
            // Codex has its own sandbox (`--sandbox`); Tiller ships no
            // `tiller guard` shim for it, or for any other harness below.
            Harness::Codex => Capabilities {
                pretool_transport: false,
                sandbox: true,
            },
            Harness::Grok | Harness::OpenCode | Harness::Pi | Harness::Omp | Harness::Cursor => {
                Capabilities {
                    pretool_transport: false,
                    sandbox: false,
                }
            }
        }
    }

    /// This harness's adapter table.
    pub fn adapter(self) -> &'static [Mapping] {
        match self {
            Harness::Claude => &[],
            Harness::Codex => CODEX,
            Harness::Grok => GROK,
            Harness::OpenCode => OPENCODE,
            Harness::Pi => PI,
            Harness::Omp => OMP,
            Harness::Cursor => CURSOR,
        }
    }
}

/// What a harness gives a Tiller role (G35). A role launches only where both
/// are true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Tiller carries the harness's tool calls to `tiller guard pretool`.
    pub pretool_transport: bool,
    /// The harness has an equivalent of the sandbox.
    pub sandbox: bool,
}

/// Why G35 refuses a role on `name`, or `None` when it may launch.
pub fn refusal(name: &str) -> Option<String> {
    let Some(harness) = Harness::parse(name) else {
        return Some(format!(
            "harness {name} has no adapter; Tiller observes it through the terminal holder and liveness only (G35)"
        ));
    };
    let caps = harness.capabilities();
    if !caps.pretool_transport {
        return Some(format!(
            "harness {name} has no pre-tool hook transport to tiller guard (G35)"
        ));
    }
    if !caps.sandbox {
        return Some(format!("harness {name} has no sandbox equivalent (G35)"));
    }
    None
}

/// What a native event becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The Claude Code event of `protocol.md` 4.2 whose handler it takes.
    Event(&'static str),
    /// Not registered: it implies no state, or its input is a reply.
    Ignore,
}

/// One row of an adapter table: the harness's own event name, the event it
/// maps to, and the input fields to supply when the harness omits them.
#[derive(Clone, Copy, Debug)]
pub struct Mapping {
    pub native: &'static str,
    pub target: Target,
    pub defaults: &'static [(&'static str, &'static str)],
}

const fn map(native: &'static str, event: &'static str) -> Mapping {
    Mapping {
        native,
        target: Target::Event(event),
        defaults: &[],
    }
}

const fn map_with(
    native: &'static str,
    event: &'static str,
    defaults: &'static [(&'static str, &'static str)],
) -> Mapping {
    Mapping {
        native,
        target: Target::Event(event),
        defaults,
    }
}

const fn ignore(native: &'static str) -> Mapping {
    Mapping {
        native,
        target: Target::Ignore,
        defaults: &[],
    }
}

const STARTUP: &[(&str, &str)] = &[("source", "startup")];
const FAILED: &[(&str, &str)] = &[("error", "unknown")];

/// Codex: Claude-shaped command hooks (`.codex/hooks.json`).
const CODEX: &[Mapping] = &[
    map("SessionStart", "SessionStart"),
    map("UserPromptSubmit", "UserPromptSubmit"),
    map("PreToolUse", "PreToolUse"),
    map("PermissionRequest", "PermissionRequest"),
    map("PostToolUse", "PostToolUse"),
    map("Stop", "Stop"),
];

/// Grok: Claude-compatible hook names, camelCase tool fields.
const GROK: &[Mapping] = &[
    map("SessionStart", "SessionStart"),
    map("PreToolUse", "PreToolUse"),
    map("Stop", "Stop"),
];

/// OpenCode: plugin events, posted by a plugin shim. 1.x ends a turn with
/// `session.idle`, 2.x with `session.execution.*`.
const OPENCODE: &[Mapping] = &[
    map_with("session.created", "SessionStart", STARTUP),
    map("tool.execute.before", "PreToolUse"),
    map("tool.execute.after", "PostToolUse"),
    map("session.idle", "Stop"),
    map("session.execution.succeeded", "Stop"),
    map("session.execution.interrupted", "Stop"),
    map_with("session.execution.failed", "StopFailure", FAILED),
    map_with("session.error", "StopFailure", FAILED),
];

/// Pi: extension events, posted by an extension shim.
const PI: &[Mapping] = &[
    map_with("session_start", "SessionStart", STARTUP),
    map("tool_call", "PreToolUse"),
    map("tool_result", "PostToolUse"),
    map("agent_settled", "Stop"),
    map("session_shutdown", "SessionEnd"),
];

/// omp: Pi's extension events, with a blocking `session_stop` for a turn end.
const OMP: &[Mapping] = &[
    map_with("session_start", "SessionStart", STARTUP),
    map("tool_call", "PreToolUse"),
    map("tool_result", "PostToolUse"),
    map("session_stop", "Stop"),
    map("session_shutdown", "SessionEnd"),
];

/// Cursor: camelCase command hooks (`.cursor/hooks.json`). Its payload has
/// no `source`, and `afterAgentResponse` carries the reply, so it is dropped.
const CURSOR: &[Mapping] = &[
    map_with("sessionStart", "SessionStart", STARTUP),
    map("sessionEnd", "SessionEnd"),
    map("beforeSubmitPrompt", "UserPromptSubmit"),
    map("preToolUse", "PreToolUse"),
    map("postToolUse", "PostToolUse"),
    map("postToolUseFailure", "PostToolUseFailure"),
    map("subagentStart", "SubagentStart"),
    map("subagentStop", "SubagentStop"),
    map("preCompact", "PreCompact"),
    map("stop", "Stop"),
    ignore("afterAgentResponse"),
    ignore("afterAgentThought"),
    ignore("beforeShellExecution"),
    ignore("afterShellExecution"),
    ignore("beforeMCPExecution"),
    ignore("afterMCPExecution"),
    ignore("beforeReadFile"),
    ignore("afterFileEdit"),
];

/// The adapter row for `native`, if the harness documents it.
pub fn mapping(harness: Harness, native: &str) -> Option<&'static Mapping> {
    harness.adapter().iter().find(|row| row.native == native)
}

/// A native hook call in Claude Code's shape, ready for `hooks::run_with`.
#[derive(Clone, Debug, PartialEq)]
pub struct Normalized {
    pub event: String,
    pub input: Vec<u8>,
}

/// Input names other harnesses use for the fields `HookInput` types, and the
/// ones `hooks::NEVER_STORED` names, in the order they are tried.
const ALIASES: &[(&str, &[&str])] = &[
    ("session_id", &["conversation_id", "sessionId", "sessionID"]),
    ("transcript_path", &["transcriptPath"]),
    ("cwd", &["workspaceRoot", "workspace_root"]),
    ("tool_name", &["toolName", "tool"]),
    (
        "tool_use_id",
        &["toolUseId", "tool_call_id", "toolCallId", "callId"],
    ),
    ("tool_input", &["toolInput"]),
    (
        "tool_response",
        &["toolResponse", "tool_output", "toolOutput"],
    ),
];

/// The fields an event this adapter does not know keeps. Everything else in
/// its input is dropped, because a foreign payload can hold replies or file
/// contents the allowlist never saw.
const IDENTITY: &[&str] = &["session_id", "transcript_path", "cwd", "hook_event_name"];

/// Maps one native hook call. `None` when the adapter ignores the event.
/// Claude Code's input passes through untouched.
pub fn normalize(harness: Harness, native: &str, input: &[u8]) -> Option<Normalized> {
    if harness == Harness::Claude {
        return Some(Normalized {
            event: native.to_owned(),
            input: input.to_vec(),
        });
    }
    let row = mapping(harness, native);
    let event = match row.map(|row| row.target) {
        Some(Target::Ignore) => return None,
        Some(Target::Event(event)) => event,
        None => native,
    };
    // Unreadable input goes on as it is; the hook logs it and fails open.
    let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(input) else {
        return Some(Normalized {
            event: event.to_owned(),
            input: input.to_vec(),
        });
    };
    for (name, aliases) in ALIASES {
        if fields.contains_key(*name) {
            continue;
        }
        if let Some(value) = aliases.iter().find_map(|alias| fields.remove(*alias)) {
            fields.insert((*name).to_owned(), value);
        }
    }
    if !fields.contains_key("cwd")
        && let Some(root) = fields
            .get("workspace_roots")
            .and_then(Value::as_array)
            .and_then(|roots| roots.first())
            .cloned()
    {
        fields.insert("cwd".to_owned(), root);
    }
    for (name, value) in row.map_or(&[][..], |row| row.defaults) {
        fields
            .entry((*name).to_owned())
            .or_insert_with(|| Value::from(*value));
    }
    fields.insert("hook_event_name".to_owned(), Value::from(event));
    if row.is_none() {
        fields.retain(|name, _| IDENTITY.contains(&name.as_str()));
    }
    let input = serde_json::to_vec(&Value::Object(Map::from_iter(fields))).unwrap_or_default();
    Some(Normalized {
        event: event.to_owned(),
        input,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;
    use crate::hooks::{EVENTS, Handler, handler};

    fn run(harness: Harness, native: &str, input: Value) -> Option<(String, Value)> {
        normalize(harness, native, input.to_string().as_bytes())
            .map(|n| (n.event, serde_json::from_slice(&n.input).unwrap()))
    }

    /// The totality test of `protocol.md` section 10: every adapter maps each
    /// native event it names to exactly one Claude Code event or to ignore,
    /// and gives the board the turn end and tool signals it reads.
    #[test]
    fn every_harness_event_has_exactly_one_mapping() {
        let canonical: HashSet<_> = EVENTS.iter().map(|spec| spec.name).collect();
        assert_eq!(Harness::ALL.len(), Harness::NAMES.len());
        for harness in Harness::ALL {
            assert_eq!(Harness::parse(harness.as_str()), Some(harness));
            assert_eq!(
                Harness::NAMES[Harness::ALL.iter().position(|h| *h == harness).unwrap()],
                harness.as_str()
            );
            let mut seen = HashSet::new();
            for row in harness.adapter() {
                assert!(
                    seen.insert(row.native),
                    "{harness:?} maps {} twice",
                    row.native
                );
                if let Target::Event(event) = row.target {
                    assert!(
                        canonical.contains(event),
                        "{harness:?} {} maps to {event}, which is not a documented event",
                        row.native
                    );
                    assert_ne!(
                        handler(event),
                        Handler::Ignore,
                        "{harness:?} {} maps to an ignored event; use Target::Ignore",
                        row.native
                    );
                }
            }
            if harness == Harness::Claude {
                assert!(harness.adapter().is_empty(), "Claude Code is 4.2 itself");
                continue;
            }
            let targets: HashSet<_> = harness
                .adapter()
                .iter()
                .filter_map(|row| match row.target {
                    Target::Event(event) => Some(event),
                    Target::Ignore => None,
                })
                .collect();
            for needed in ["Stop", "PreToolUse"] {
                assert!(targets.contains(needed), "{harness:?} never maps {needed}");
            }
        }
        assert_eq!(Harness::parse("gemini"), None);
    }

    #[test]
    fn claude_input_passes_through_untouched() {
        let raw = br#"{"session_id":"s","note":"not json at all"#;
        let out = normalize(Harness::Claude, "Stop", raw).unwrap();
        assert_eq!(out.event, "Stop");
        assert_eq!(out.input, raw);
    }

    #[test]
    fn native_names_take_the_claude_event_and_shape() {
        let (event, input) = run(
            Harness::Cursor,
            "preToolUse",
            json!({"conversation_id": "c1", "workspace_roots": ["/home/user/repo"], "tool_name": "Shell", "tool_input": {"command": "ls"}}),
        )
        .unwrap();
        assert_eq!(event, "PreToolUse");
        assert_eq!(input["session_id"], "c1");
        assert_eq!(input["cwd"], "/home/user/repo");
        assert_eq!(input["hook_event_name"], "PreToolUse");

        let (event, input) = run(
            Harness::Grok,
            "PreToolUse",
            json!({"sessionId": "g1", "toolName": "bash", "toolInput": {"command": "ls"}}),
        )
        .unwrap();
        assert_eq!(event, "PreToolUse");
        assert_eq!(input["session_id"], "g1");
        assert_eq!(input["tool_name"], "bash");
        assert_eq!(input["tool_input"]["command"], "ls");
        assert!(input.get("toolInput").is_none());
    }

    #[test]
    fn missing_fields_take_the_row_defaults() {
        let (event, input) =
            run(Harness::Cursor, "sessionStart", json!({"session_id": "c1"})).unwrap();
        assert_eq!(event, "SessionStart");
        assert_eq!(input["source"], "startup");
        // A field the harness does send is not overwritten.
        let (_, input) = run(
            Harness::OpenCode,
            "session.created",
            json!({"session_id": "o1", "source": "resume"}),
        )
        .unwrap();
        assert_eq!(input["source"], "resume");
        let (event, input) = run(
            Harness::OpenCode,
            "session.execution.failed",
            json!({"session_id": "o1"}),
        )
        .unwrap();
        assert_eq!(event, "StopFailure");
        assert_eq!(input["error"], "unknown");
    }

    #[test]
    fn ignored_events_are_not_registered() {
        assert!(
            run(
                Harness::Cursor,
                "afterAgentResponse",
                json!({"text": "a whole reply"})
            )
            .is_none()
        );
        assert!(normalize(Harness::Cursor, "beforeShellExecution", b"not json").is_none());
    }

    #[test]
    fn an_unmapped_native_event_keeps_only_identity_fields() {
        let (event, input) = run(
            Harness::Pi,
            "message_update",
            json!({"session_id": "p1", "cwd": "/home/user/repo", "text": "a whole reply", "tool_input": {"command": "secret"}}),
        )
        .unwrap();
        assert_eq!(event, "message_update");
        assert_eq!(
            input,
            json!({"session_id": "p1", "cwd": "/home/user/repo", "hook_event_name": "message_update"})
        );
    }

    #[test]
    fn only_claude_clears_g35() {
        for name in Harness::NAMES {
            let refused = refusal(name);
            assert_eq!(refused.is_none(), name == "claude", "{name}: {refused:?}");
        }
        assert!(refusal("gemini").unwrap().contains("no adapter"));
        assert!(
            refusal("cursor")
                .unwrap()
                .contains("pre-tool hook transport")
        );
        // Codex has a sandbox; it is the transport it lacks.
        assert!(Harness::Codex.capabilities().sandbox);
        assert!(
            refusal("codex")
                .unwrap()
                .contains("pre-tool hook transport")
        );
    }
}
