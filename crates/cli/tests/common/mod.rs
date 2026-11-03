//! A throwaway Tiller home and a way to run the real `tiller` binary against
//! it, for the command snapshots and the idempotency tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;
use tiller_core::domain::{Effort, RunRole};
use tiller_core::state::runs::{NewOrchestrator, NewRun, SpawnChecks};
use tiller_core::{Actor, Ctx, Store};

/// What one command printed and how it exited.
pub struct Out {
    pub code: i32,
    pub json: Value,
}

impl Out {
    pub fn ok(&self) -> bool {
        self.json["ok"] == true
    }

    pub fn data(&self) -> &Value {
        &self.json["data"]
    }

    pub fn error_code(&self) -> &str {
        self.json["error"]["code"].as_str().unwrap_or("")
    }

    pub fn replayed(&self) -> bool {
        self.json["replayed"] == true
    }

    pub fn events(&self) -> Vec<i64> {
        self.json["events"]
            .as_array()
            .map(|events| events.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default()
    }
}

pub struct World {
    pub dir: TempDir,
    names: RefCell<Names>,
    /// Extra environment for every command, like a fake forge client.
    env: RefCell<Vec<(String, String)>>,
}

#[derive(Default)]
struct Names {
    ulids: HashMap<String, String>,
}

impl World {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            names: RefCell::default(),
            env: RefCell::default(),
        }
    }

    /// A world that has had `tiller init` and `project add alpha`.
    pub fn initialized() -> Self {
        let world = Self::new();
        world.must(&["init", "--host-label", "host-a", "--lane-capacity", "2"]);
        world.must(&[
            "project",
            "add",
            "alpha",
            "--name",
            "Alpha",
            "--repo",
            "example/alpha",
            "--delivery",
            "direct-pr",
            "--capacity",
            "3",
        ]);
        world
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn db(&self) -> PathBuf {
        self.home().join("state.db")
    }

    fn command(&self, session: Option<&str>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tiller"));
        command
            .env("TILLER_HOME", self.home())
            .env_remove("TILLER_DB")
            .env_remove("TILLER_SESSION_ID")
            .env_remove("TILLER_RUN_ID")
            .env_remove("TILLER_SSH")
            .env_remove("TILLER_GH");
        if let Some(session) = session {
            command.env("TILLER_SESSION_ID", session);
        }
        command.envs(self.env.borrow().iter().cloned());
        command
    }

    /// Sets `key` for every later command.
    pub fn set_env(&self, key: &str, value: &str) {
        self.env.borrow_mut().push((key.into(), value.into()));
    }

    /// Runs `tiller --json <args>` as the owner.
    pub fn run(&self, args: &[&str]) -> Out {
        self.exec(None, args, None)
    }

    /// Runs as a Tiller session (`TILLER_SESSION_ID` set).
    pub fn run_as(&self, session: &str, args: &[&str]) -> Out {
        self.exec(Some(session), args, None)
    }

    pub fn run_with_stdin(&self, args: &[&str], stdin: &str) -> Out {
        self.exec(None, args, Some(stdin))
    }

    fn exec(&self, session: Option<&str>, args: &[&str], stdin: Option<&str>) -> Out {
        use std::io::Write;
        use std::process::Stdio;
        let mut command = self.command(session);
        command
            .arg("--json")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        let mut child = command.spawn().unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        let output = child.wait_with_output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let json = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{args:?} printed no JSON ({error}): {text:?}"));
        Out {
            code: output.status.code().unwrap(),
            json,
        }
    }

    /// Runs and requires success, returning the response.
    pub fn must(&self, args: &[&str]) -> Out {
        let out = self.run(args);
        assert!(out.ok() && out.code == 0, "{args:?} failed: {}", out.json);
        out
    }

    pub fn must_as(&self, session: &str, args: &[&str]) -> Out {
        let out = self.run_as(session, args);
        assert!(out.ok() && out.code == 0, "{args:?} failed: {}", out.json);
        out
    }

    pub fn store(&self) -> Store {
        Store::open(self.db()).unwrap()
    }

    pub fn owner_ctx(&self) -> Ctx {
        Ctx::new(Actor::Owner)
    }

    /// A live orchestrator for `alpha`; returns its session id.
    pub fn orchestrator(&self) -> String {
        self.store()
            .session_start(
                &self.owner_ctx(),
                NewOrchestrator {
                    project_id: "alpha".into(),
                    claude_session_id: Some("claude-orchestrator".into()),
                    ..NewOrchestrator::default()
                },
            )
            .unwrap()
            .value
            .session
            .id
    }

    /// Starts a builder run on `card` (a display id); returns the run id and
    /// the worker session id.
    pub fn builder_run(&self, card: &str) -> (String, String) {
        let started = self
            .store()
            .run_start(
                &self.owner_ctx(),
                None,
                NewRun {
                    card: card.into(),
                    role: RunRole::Builder,
                    model: "test-model".into(),
                    effort: Effort::Medium,
                    host_id: None,
                    worktree_path: Some("/home/user/work/alpha-1".into()),
                    branch: Some("alpha-1-build".into()),
                    base_branch: Some("main".into()),
                    base_sha: None,
                    config_hash: None,
                    checks: SpawnChecks::default(),
                },
            )
            .unwrap()
            .value;
        (started.run.id, started.session.id)
    }

    // -- normalizing for snapshots --------------------------------------------

    /// The response as a snapshot: ULIDs, their short forms and times replaced
    /// by stable names in order of first appearance, with the exit code.
    pub fn snapshot(&self, out: &Out) -> String {
        let mut json = out.json.clone();
        self.normalize(&mut json);
        format!(
            "exit: {}\n{}",
            out.code,
            serde_json::to_string_pretty(&json).unwrap()
        )
    }

    fn normalize(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.normalize_text(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.normalize(item)),
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if matches!(key.as_str(), "age_seconds" | "age") {
                        *item = Value::String("<age>".into());
                    } else if matches!(key.as_str(), "seq" | "last_seq" | "last_event_seq") {
                        *item = Value::String("<seq>".into());
                    } else if key == "events"
                        && item
                            .as_array()
                            .is_some_and(|a| a.iter().all(Value::is_number))
                    {
                        let count = item.as_array().map_or(0, Vec::len);
                        *item = Value::Array(vec![Value::String("<seq>".into()); count]);
                    } else {
                        self.normalize(item);
                    }
                }
            }
            _ => {}
        }
    }

    fn normalize_text(&self, text: &str) -> String {
        if is_time(text) {
            return "<time>".into();
        }
        let mut names = self.names.borrow_mut();
        let mut out = String::new();
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if i + 26 <= bytes.len()
                && bytes[i..i + 26].iter().all(|b| is_crockford(*b))
                && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
                && (i + 26 == bytes.len() || !bytes[i + 26].is_ascii_alphanumeric())
            {
                let ulid = &text[i..i + 26];
                let next = names.ulids.len() + 1;
                let name = names
                    .ulids
                    .entry(ulid.to_owned())
                    .or_insert_with(|| format!("<id{next}>"))
                    .clone();
                // The short form is the ULID's last seven characters.
                names
                    .ulids
                    .entry(ulid[19..].to_owned())
                    .or_insert_with(|| format!("{}.short", name.trim_end_matches('>')) + ">");
                out.push_str(&name);
                i += 26;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        // Short forms (the last seven characters of a ULID seen above).
        let mut result = String::new();
        let mut token = String::new();
        for c in out.chars().chain(std::iter::once('\0')) {
            if c.is_ascii_alphanumeric() {
                token.push(c);
                continue;
            }
            match names.ulids.get(&token) {
                Some(known) if token.len() == 7 => result.push_str(known),
                _ => result.push_str(&token),
            }
            token.clear();
            if c != '\0' {
                result.push(c);
            }
        }
        result
    }
}

fn is_crockford(byte: u8) -> bool {
    byte.is_ascii_digit()
        || (byte.is_ascii_uppercase() && !matches!(byte, b'I' | b'L' | b'O' | b'U'))
}

fn is_time(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 24
        && bytes[4] == b'-'
        && bytes[10] == b'T'
        && bytes[19] == b'.'
        && bytes[23] == b'Z'
}

/// Asserts one named snapshot of a command's response and exit code.
pub fn snap(world: &World, name: &str, out: &Out) {
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.set_snapshot_path("../snapshots");
    settings.set_omit_expression(true);
    settings.bind(|| {
        insta::assert_snapshot!(name, world.snapshot(out));
    });
}
