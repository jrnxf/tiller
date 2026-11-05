//! A model-driven orchestrator: each turn goes to `claude -p` with the
//! packaged orchestrator role appended to the system prompt, as `tiller
//! spawn` launches it, plus the replay addendum, which asks for the turn's
//! `tiller` commands as [`Decision`]s instead of running them.
//!
//! The session loads no settings, hooks, MCP servers or tools, so nothing
//! the model answers can leave the sandbox. Each turn's full answer goes to
//! a log beside the records: unlike the records, it quotes the bundle.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use tiller_agents::roles::Role;

use crate::Error;
use crate::orchestrator::{Decision, Orchestrator, Turn};

/// The replay addendum appended after the role body.
pub const REPLAY_PROMPT: &str = include_str!("../prompts/orchestrator-replay.md");

/// One `claude -p` session: a model, an effort and a system prompt.
#[derive(Clone, Debug)]
pub struct Session {
    pub program: PathBuf,
    pub model: String,
    pub effort: String,
    /// Appended to Claude Code's own system prompt.
    pub system: String,
    /// An empty directory the session runs in.
    pub workdir: PathBuf,
}

/// What one call answered.
#[derive(Clone, Debug)]
pub struct Answer {
    pub structured: Value,
    /// The models the CLI reports it used.
    pub models: Vec<String>,
    pub cost_usd: f64,
}

/// How long to wait before each retry of a call that failed: a long replay
/// should outlast a dropped connection or a brief overload.
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(15), Duration::from_secs(60)];

impl Session {
    /// Asks and returns the answer that matches `schema`, retrying a call
    /// that failed after each of [`RETRY_DELAYS`].
    pub fn ask(&self, schema: &Value, prompt: &str) -> Result<Answer, Error> {
        self.ask_with(schema, prompt, &RETRY_DELAYS)
    }

    fn ask_with(&self, schema: &Value, prompt: &str, delays: &[Duration]) -> Result<Answer, Error> {
        let mut delays = delays.iter();
        loop {
            match self.ask_once(schema, prompt) {
                Err(e) => match delays.next() {
                    Some(delay) => {
                        eprintln!("{e}; retrying in {}s", delay.as_secs());
                        thread::sleep(*delay);
                    }
                    None => return Err(e),
                },
                answer => return answer,
            }
        }
    }

    /// Asks once.
    fn ask_once(&self, schema: &Value, prompt: &str) -> Result<Answer, Error> {
        fs::create_dir_all(&self.workdir)?;
        let out = Command::new(&self.program)
            .current_dir(&self.workdir)
            .args(["-p", "--model", &self.model, "--effort", &self.effort])
            .args([
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--tools",
                "",
            ])
            .args(["--no-session-persistence", "--output-format", "json"])
            .arg("--append-system-prompt")
            .arg(&self.system)
            .arg("--json-schema")
            .arg(schema.to_string())
            .arg(prompt)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;
        let reply: Value = serde_json::from_slice(&out.stdout).map_err(|e| {
            Error::Orchestrator(format!(
                "{} exited {} with no JSON answer ({e}): {}",
                self.program.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        })?;
        if !out.status.success() || reply["is_error"].as_bool() == Some(true) {
            return Err(Error::Orchestrator(format!(
                "{} exited {}: {}",
                self.program.display(),
                out.status,
                reply["result"].as_str().unwrap_or_default()
            )));
        }
        let structured = reply
            .get("structured_output")
            .cloned()
            .filter(|v| !v.is_null())
            .ok_or_else(|| Error::Orchestrator("the answer has no structured output".into()))?;
        let models = reply["modelUsage"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        Ok(Answer {
            structured,
            models,
            cost_usd: reply["total_cost_usd"].as_f64().unwrap_or(0.0),
        })
    }
}

/// The orchestrator a replay runs against a model.
pub struct Claude {
    pub session: Session,
    /// Where each turn's full answer is appended.
    pub log: PathBuf,
}

/// One turn as the log keeps it.
#[derive(Serialize)]
struct Logged<'a> {
    index: usize,
    ask_id: &'a str,
    decisions: &'a Value,
    board: &'a [crate::board::BoardCard],
    models: &'a [String],
    cost_usd: f64,
    attempts: usize,
    seconds: f64,
}

impl Claude {
    /// The packaged orchestrator role's body plus the replay addendum, with
    /// its frontmatter's model and effort unless `model` or `effort`
    /// overrides them.
    pub fn new(
        program: PathBuf,
        model: Option<String>,
        effort: Option<String>,
        workdir: PathBuf,
        log: PathBuf,
    ) -> Result<Self, Error> {
        let role = Role::Orchestrator;
        let refused = |e: tiller_agents::roles::RoleError| Error::Input(e.to_string());
        let front = role.frontmatter().map_err(refused)?;
        let body = role.prompt_body().map_err(refused)?;
        let system = format!("{}\n\n{REPLAY_PROMPT}", body.trim_end());
        Ok(Self {
            session: Session {
                program,
                model: model.unwrap_or(front.model),
                effort: effort.unwrap_or(front.effort),
                system,
                workdir,
            },
            log,
        })
    }
}

/// The turn as the model reads it.
pub fn prompt(turn: &Turn) -> String {
    let shown = json!({
        "now": turn.now,
        "ask_id": turn.ask_id,
        "project": turn.project,
        "projects": turn.projects,
        "hook_context": turn.context,
        "board": turn.board,
    });
    format!(
        "Replay turn.\n\n```json\n{}\n```\n\nThe owner's message:\n\n{}\n",
        serde_json::to_string_pretty(&shown).unwrap_or_default(),
        turn.text
    )
}

/// The shape every answer is held to: a list of decision objects.
pub fn schema() -> Value {
    let text = json!({"type": "string"});
    json!({
        "type": "object",
        "properties": {
            "decisions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "do": {"enum": ["new", "followup", "reopen", "chat", "cancelled", "spawn", "hold", "attention", "dep", "reply"]},
                        "label": text, "project": text, "title": text, "acceptance": text,
                        "kind": {"enum": ["ship", "scout", "question", "ops"]},
                        "size": {"enum": ["S", "M", "L"]},
                        "card": text, "depends_on": text, "reason": text, "question": text, "text": text,
                        "role": {"enum": ["scout", "builder", "reviewer", "verifier"]},
                        "model": text,
                        "effort": {"enum": ["low", "medium", "high", "xhigh", "max"]},
                        "until": text,
                    },
                    "required": ["do"],
                },
            },
        },
        "required": ["decisions"],
    })
}

impl Orchestrator for Claude {
    fn decide(&mut self, turn: &Turn) -> Result<Vec<Decision>, Error> {
        let started = Instant::now();
        let schema = schema();
        let mut prompt = prompt(turn);
        let mut cost = 0.0;
        for attempt in 1..=2 {
            let answer = self.session.ask(&schema, &prompt)?;
            cost += answer.cost_usd;
            let raw = answer
                .structured
                .get("decisions")
                .cloned()
                .unwrap_or_default();
            match serde_json::from_value::<Vec<Decision>>(raw.clone()) {
                Ok(decisions) => {
                    let logged = Logged {
                        index: turn.index,
                        ask_id: &turn.ask_id,
                        decisions: &raw,
                        board: &turn.board,
                        models: &answer.models,
                        cost_usd: cost,
                        attempts: attempt,
                        seconds: started.elapsed().as_secs_f64(),
                    };
                    let mut log = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&self.log)?;
                    writeln!(log, "{}", serde_json::to_string(&logged)?)?;
                    return Ok(decisions);
                }
                // One retry, told what did not parse.
                Err(e) if attempt == 1 => {
                    prompt = format!(
                        "{prompt}\nYour last answer was not a valid decision list ({e}). Answer again.\n"
                    );
                }
                Err(e) => {
                    return Err(Error::Orchestrator(format!(
                        "ask {}: no valid decision list after two answers: {e}",
                        turn.index
                    )));
                }
            }
        }
        Err(Error::Orchestrator(format!(
            "ask {}: no answer",
            turn.index
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn a_failed_call_is_retried_until_the_delays_run_out() {
        let dir = tempfile::tempdir().unwrap();
        // Fails until its work directory's `pass_at` call, then answers.
        let program = dir.path().join("claude");
        fs::write(
            &program,
            "#!/bin/sh\necho x >> calls\n[ \"$(wc -l < calls)\" -ge \"$(cat pass_at)\" ] || { echo overloaded >&2; exit 1; }\nprintf '{\"is_error\":false,\"total_cost_usd\":0.1,\"structured_output\":{\"ok\":true}}\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let session = |work: &str, pass_at: &str| {
            let workdir = dir.path().join(work);
            fs::create_dir_all(&workdir).unwrap();
            fs::write(workdir.join("pass_at"), pass_at).unwrap();
            Session {
                program: program.clone(),
                model: "m".into(),
                effort: "low".into(),
                system: String::new(),
                workdir,
            }
        };
        let calls = |work: &str| {
            fs::read_to_string(dir.path().join(work).join("calls"))
                .unwrap()
                .lines()
                .count()
        };
        let none = [Duration::ZERO; 2];
        let answer = session("pass", "3")
            .ask_with(&json!({}), "q", &none)
            .unwrap();
        assert_eq!(answer.structured, json!({"ok": true}));
        assert_eq!(calls("pass"), 3);
        let failed = session("fail", "3").ask_with(&json!({}), "q", &none[..1]);
        assert!(failed.is_err());
        assert_eq!(calls("fail"), 2);
    }
}
