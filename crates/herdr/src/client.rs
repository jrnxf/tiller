//! herdr socket and CLI calls (T2.5).
//!
//! Every call goes through the `herdr` CLI, herdr's documented surface, which
//! also forwards to a saved machine with `--machine`. The one exception is
//! `workspace.move`, which herdr exposes only on the session's API socket; the
//! adapter sends that single method there (`docs/design/herdr-layout.md`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The pane metadata source Tiller reports its row tokens under.
pub const TOKEN_SOURCE: &str = "tiller";

#[derive(Debug, thiserror::Error)]
pub enum HerdrError {
    /// The server did not answer: not running, a dead socket, a saved machine
    /// that cannot be reached, or a call past its timeout.
    #[error("herdr is unreachable: {0}")]
    Unreachable(String),

    #[error("herdr has no {what} {id}")]
    NotFound { what: &'static str, id: String },

    /// The server answered with an error.
    #[error("herdr refused {call}: {code}: {message}")]
    Api {
        call: String,
        code: String,
        message: String,
    },

    /// The server answered with something the adapter cannot read.
    #[error("herdr answered {call} with something unreadable: {detail}")]
    Parse { call: String, detail: String },

    /// The call needs the session's API socket, which this client lacks.
    #[error("{0} needs the herdr API socket, which this client does not have")]
    NoSocket(&'static str),
}

pub type Result<T, E = HerdrError> = std::result::Result<T, E>;

/// One workspace, in sidebar order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
}

/// One pane as herdr reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub workspace_id: String,
    /// The recognized agent in the pane (`claude`), or none.
    #[serde(default)]
    pub agent: Option<String>,
    /// herdr's lifecycle reading: `idle`, `working`, `blocked`, `done` or `unknown`.
    #[serde(default)]
    pub agent_status: Option<String>,
    /// Display-only metadata tokens, Tiller's row tokens among them.
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

/// One consistent read of a herdr server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// herdr's version, when it reports one.
    #[serde(default)]
    pub version: Option<String>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub panes: Vec<PaneInfo>,
}

impl Snapshot {
    pub fn pane(&self, pane_id: &str) -> Option<&PaneInfo> {
        self.panes.iter().find(|p| p.pane_id == pane_id)
    }
}

/// A workspace herdr just made, with the pane it opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub workspace_id: String,
    pub pane_id: String,
}

/// Everything Tiller asks of herdr. The CLI client is the real one; tests
/// drive the same code through a fake.
pub trait Herdr {
    fn snapshot(&self) -> Result<Snapshot>;
    /// One pane, or `None` when herdr no longer has it.
    fn pane(&self, pane_id: &str) -> Result<Option<PaneInfo>>;
    /// The pane's visible viewport with its ANSI styling.
    fn read_visible(&self, pane_id: &str) -> Result<String>;
    /// Types literal text into the pane, with no Enter.
    fn send_text(&self, pane_id: &str, text: &str) -> Result<()>;
    fn send_enter(&self, pane_id: &str) -> Result<()>;
    /// Escape: cancels a working turn (`tiller agent interrupt`).
    fn send_escape(&self, pane_id: &str) -> Result<()>;
    /// Reports Tiller's row tokens on the pane; herdr merges them with the
    /// tokens already reported under the same source.
    fn report_tokens(&self, pane_id: &str, tokens: &BTreeMap<String, String>) -> Result<()>;
    fn create_workspace(&self, label: &str, cwd: Option<&Path>) -> Result<Created>;
    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()>;
    /// Moves a workspace to `index` in the sidebar order (0 is the top).
    fn move_workspace(&self, workspace_id: &str, index: usize) -> Result<()>;
    /// Closes a pane; herdr closes the workspace it empties. A pane herdr no
    /// longer has is already closed.
    fn close_pane(&self, pane_id: &str) -> Result<()>;
    fn notify(&self, title: &str, body: &str) -> Result<()>;
}

/// The `herdr` CLI, aimed at one server: the local default session, a named
/// session, or a saved machine.
#[derive(Clone, Debug)]
pub struct Cli {
    program: Vec<OsString>,
    session: Option<String>,
    machine: Option<String>,
    socket: Option<PathBuf>,
    timeout: Duration,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            program: vec!["herdr".into()],
            session: None,
            machine: None,
            socket: None,
            timeout: Duration::from_secs(10),
        }
    }
}

impl Cli {
    /// The local server's default session.
    pub fn local() -> Self {
        Self::default()
    }

    /// A saved machine, addressed by its id (the display name failed in
    /// testing; `herdr-layout.md`). Remote hosts are read-only in v1.
    pub fn machine(id: impl Into<String>) -> Self {
        Self {
            machine: Some(id.into()),
            ..Self::default()
        }
    }

    /// The same CLI aimed at a saved machine, keeping its program.
    pub fn on_machine(mut self, id: impl Into<String>) -> Self {
        self.machine = Some(id.into());
        self
    }

    /// A named local session; every call carries `--session <name>`.
    pub fn session(mut self, name: impl Into<String>) -> Self {
        self.session = Some(name.into());
        self
    }

    /// Runs `herdr` through another program, for example a wrapper that adds
    /// its own session flag. The arguments follow `program` verbatim.
    pub fn program(mut self, program: Vec<OsString>) -> Self {
        self.program = program;
        self
    }

    /// The session's API socket, needed only for `workspace.move`.
    pub fn socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket = Some(path.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Focuses an agent's pane in the herdr client (`herdr agent focus`),
    /// on the saved machine first for a remote agent. The board's "Open in
    /// herdr" button calls this through the daemon.
    pub fn focus_agent(&self, pane_id: &str) -> Result<()> {
        self.run(&["agent", "focus", pane_id]).map(|_| ())
    }

    fn command(&self, args: &[&str]) -> Command {
        let (program, leading) = self
            .program
            .split_first()
            .map_or((OsString::from("herdr"), &[][..]), |(p, rest)| {
                (p.clone(), rest)
            });
        let mut command = Command::new(program);
        command.args(leading);
        if let Some(machine) = &self.machine {
            command.arg("--machine").arg(machine);
        }
        command.args(args);
        if let Some(session) = &self.session {
            command.arg("--session").arg(session);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Runs one CLI call and returns its stdout.
    fn run(&self, args: &[&str]) -> Result<String> {
        let call = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        let mut child = self
            .command(args)
            .spawn()
            .map_err(|e| HerdrError::Unreachable(format!("cannot run herdr: {e}")))?;
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        // Drain both pipes on their own threads so a large read never blocks
        // the child on a full pipe while this thread waits for it to exit.
        let (out_tx, out) = mpsc::channel();
        let (err_tx, err) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(pipe) = stdout.as_mut() {
                let _ = pipe.read_to_string(&mut text);
            }
            let _ = out_tx.send(text);
        });
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(pipe) = stderr.as_mut() {
                let _ = pipe.read_to_string(&mut text);
            }
            let _ = err_tx.send(text);
        });
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(HerdrError::Unreachable(format!(
                        "{call} did not answer within {:?}",
                        self.timeout
                    )));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(HerdrError::Unreachable(format!("{call}: {e}"))),
            }
        };
        // A process the child left behind can hold the pipes open past its
        // exit, so the reads share the call's deadline too.
        let drained = |pipe: mpsc::Receiver<String>| {
            pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|_| {
                    HerdrError::Unreachable(format!(
                        "{call} kept its output open past {:?}",
                        self.timeout
                    ))
                })
        };
        let stdout = drained(out)?;
        let stderr = drained(err)?;
        if status.success() {
            return Ok(stdout);
        }
        Err(cli_error(&call, &stderr, &stdout))
    }

    /// Runs one CLI call that answers with a JSON `result`.
    fn result(&self, args: &[&str]) -> Result<Value> {
        let call = args.iter().take(2).copied().collect::<Vec<_>>().join(" ");
        let stdout = self.run(args)?;
        let value: Value = serde_json::from_str(stdout.trim()).map_err(|e| HerdrError::Parse {
            call: call.clone(),
            detail: e.to_string(),
        })?;
        value
            .get("result")
            .cloned()
            .ok_or_else(|| HerdrError::Parse {
                call,
                detail: "no result".into(),
            })
    }

    /// Sends one request to the API socket.
    fn socket_call(&self, method: &'static str, params: Value) -> Result<Value> {
        let path = self.socket.as_ref().ok_or(HerdrError::NoSocket(method))?;
        let unreachable = |e: std::io::Error| HerdrError::Unreachable(format!("{method}: {e}"));
        let mut stream = UnixStream::connect(path).map_err(unreachable)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(unreachable)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(unreachable)?;
        let request = json!({"id": format!("tiller:{method}"), "method": method, "params": params});
        stream
            .write_all(format!("{request}\n").as_bytes())
            .map_err(unreachable)?;
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(unreachable)?;
        let value: Value = serde_json::from_str(line.trim()).map_err(|e| HerdrError::Parse {
            call: method.into(),
            detail: e.to_string(),
        })?;
        if let Some(error) = value.get("error") {
            return Err(api_error(method, error));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| HerdrError::Parse {
                call: method.into(),
                detail: "no result".into(),
            })
    }
}

fn api_error(call: &str, error: &Value) -> HerdrError {
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    HerdrError::Api {
        call: call.into(),
        code,
        message,
    }
}

/// herdr prints a server error as JSON on stderr; anything else (a syntax
/// error, a server that never answered) is unreachable.
fn cli_error(call: &str, stderr: &str, stdout: &str) -> HerdrError {
    let parsed = serde_json::from_str::<Value>(stderr.trim())
        .or_else(|_| serde_json::from_str::<Value>(stdout.trim()));
    match parsed.ok().as_ref().and_then(|v| v.get("error")) {
        Some(error) => api_error(call, error),
        None => HerdrError::Unreachable(format!("{call}: {}", stderr.trim())),
    }
}

fn parse<T: serde::de::DeserializeOwned>(call: &str, value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| HerdrError::Parse {
        call: call.into(),
        detail: e.to_string(),
    })
}

fn field(value: &Value, path: &[&str]) -> Option<String> {
    path.iter()
        .try_fold(value, |v, key| v.get(key))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// herdr reports a token whose value it dropped as `null`; such a token is
/// simply absent.
#[derive(Deserialize)]
struct RawPane {
    pane_id: String,
    workspace_id: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    agent_status: Option<String>,
    #[serde(default)]
    tokens: Option<BTreeMap<String, Option<String>>>,
}

impl From<RawPane> for PaneInfo {
    fn from(raw: RawPane) -> Self {
        PaneInfo {
            pane_id: raw.pane_id,
            workspace_id: raw.workspace_id,
            agent: raw.agent,
            agent_status: raw.agent_status,
            tokens: raw
                .tokens
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v)))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct RawSnapshot {
    #[serde(default)]
    version: Option<String>,
    workspaces: Vec<WorkspaceInfo>,
    panes: Vec<RawPane>,
}

/// Reads a `session.snapshot` result.
pub fn parse_snapshot(result: Value) -> Result<Snapshot> {
    let snapshot = result.get("snapshot").cloned().unwrap_or(result);
    let raw: RawSnapshot = parse("api snapshot", snapshot)?;
    Ok(Snapshot {
        version: raw.version,
        workspaces: raw.workspaces,
        panes: raw.panes.into_iter().map(PaneInfo::from).collect(),
    })
}

impl Herdr for Cli {
    fn snapshot(&self) -> Result<Snapshot> {
        parse_snapshot(self.result(&["api", "snapshot"])?)
    }

    fn pane(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        match self.result(&["pane", "get", pane_id]) {
            Ok(result) => {
                let pane = result.get("pane").cloned().unwrap_or(result);
                Ok(Some(parse::<RawPane>("pane get", pane)?.into()))
            }
            Err(HerdrError::Api { code, .. }) if code == "pane_not_found" => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read_visible(&self, pane_id: &str) -> Result<String> {
        match self.run(&[
            "pane", "read", pane_id, "--source", "visible", "--format", "ansi",
        ]) {
            Err(HerdrError::Api { code, .. }) if code == "pane_not_found" => {
                Err(HerdrError::NotFound {
                    what: "pane",
                    id: pane_id.into(),
                })
            }
            other => other,
        }
    }

    // herdr 0.9.5 prints nothing for `send-text` and `send-keys`: a write
    // succeeds on exit 0, and a refusal is an error on stderr.
    fn send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.run(&["pane", "send-text", pane_id, text]).map(drop)
    }

    fn send_enter(&self, pane_id: &str) -> Result<()> {
        self.run(&["pane", "send-keys", pane_id, "enter"]).map(drop)
    }

    fn send_escape(&self, pane_id: &str) -> Result<()> {
        self.run(&["pane", "send-keys", pane_id, "esc"]).map(drop)
    }

    fn report_tokens(&self, pane_id: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        let pairs: Vec<String> = tokens.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut args = vec!["pane", "report-metadata", pane_id, "--source", TOKEN_SOURCE];
        for pair in &pairs {
            args.push("--token");
            args.push(pair);
        }
        self.run(&args).map(drop)
    }

    fn create_workspace(&self, label: &str, cwd: Option<&Path>) -> Result<Created> {
        let cwd = cwd.map(|p| p.to_string_lossy().into_owned());
        let mut args = vec!["workspace", "create", "--label", label, "--no-focus"];
        if let Some(cwd) = &cwd {
            args.push("--cwd");
            args.push(cwd);
        }
        let result = self.result(&args)?;
        match (
            field(&result, &["workspace", "workspace_id"]),
            field(&result, &["root_pane", "pane_id"]),
        ) {
            (Some(workspace_id), Some(pane_id)) => Ok(Created {
                workspace_id,
                pane_id,
            }),
            _ => Err(HerdrError::Parse {
                call: "workspace create".into(),
                detail: "no workspace or root pane id".into(),
            }),
        }
    }

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        self.run(&["workspace", "rename", workspace_id, label])
            .map(drop)
    }

    fn move_workspace(&self, workspace_id: &str, index: usize) -> Result<()> {
        self.socket_call(
            "workspace.move",
            json!({"workspace_id": workspace_id, "insert_index": index}),
        )
        .map(drop)
    }

    fn close_pane(&self, pane_id: &str) -> Result<()> {
        match self.run(&["pane", "close", pane_id]) {
            Err(HerdrError::Api { code, .. }) if code == "pane_not_found" => Ok(()),
            other => other.map(drop),
        }
    }

    fn notify(&self, title: &str, body: &str) -> Result<()> {
        self.run(&["notification", "show", title, "--body", body])
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_keeps_sidebar_order_and_drops_null_tokens() {
        let result = json!({
            "type": "session_snapshot",
            "snapshot": {
                "version": "0.9.5",
                "workspaces": [
                    {"workspace_id": "w1", "label": "alpha", "number": 1},
                    {"workspace_id": "w3", "label": "├ alpha-1 build", "number": 2}
                ],
                "panes": [
                    {"pane_id": "w1:p1", "workspace_id": "w1", "agent_status": "idle", "agent": "claude",
                     "tokens": {"project": "alpha", "card": null}, "revision": 3},
                    {"pane_id": "w3:p1", "workspace_id": "w3", "agent_status": "unknown"}
                ],
                "agents": []
            }
        });
        let snapshot = parse_snapshot(result).unwrap();
        let order: Vec<&str> = snapshot
            .workspaces
            .iter()
            .map(|w| w.workspace_id.as_str())
            .collect();
        assert_eq!(order, ["w1", "w3"]);
        let pane = snapshot.pane("w1:p1").unwrap();
        assert_eq!(pane.agent.as_deref(), Some("claude"));
        assert_eq!(
            pane.tokens,
            BTreeMap::from([("project".into(), "alpha".into())])
        );
        assert!(snapshot.pane("w3:p1").unwrap().tokens.is_empty());
    }

    #[test]
    fn server_error_on_stderr_is_an_api_error_and_garbage_is_unreachable() {
        let error = cli_error(
            "pane get",
            r#"{"error":{"code":"pane_not_found","message":"pane w9:p9 not found"},"id":"cli:pane:get"}"#,
            "",
        );
        assert!(matches!(error, HerdrError::Api { ref code, .. } if code == "pane_not_found"));
        let error = cli_error("pane get", "error: could not connect to server", "");
        assert!(matches!(error, HerdrError::Unreachable(_)));
    }

    #[test]
    fn a_write_that_prints_nothing_succeeds_and_a_failed_one_does_not() {
        let quiet = Cli::local().program(vec!["true".into()]);
        assert!(quiet.send_text("w1:p1", "tiller inbox 01").is_ok());
        assert!(quiet.send_enter("w1:p1").is_ok());
        let refused = Cli::local().program(vec!["false".into()]);
        assert!(refused.send_enter("w1:p1").is_err());
    }

    #[test]
    fn a_leftover_process_holding_the_pipes_cannot_outlast_the_timeout() {
        // `sh` exits at once; the `sleep` it leaves keeps stdout open.
        let cli = Cli::local()
            .program(vec![
                "sh".into(),
                "-c".into(),
                "sleep 5 & exit 0".into(),
                "--".into(),
            ])
            .timeout(Duration::from_millis(300));
        let started = Instant::now();
        let error = cli.send_enter("w1:p1").unwrap_err();
        assert!(matches!(error, HerdrError::Unreachable(_)), "{error:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn session_flag_trails_and_machine_flag_leads() {
        let cli = Cli::local().session("lab");
        let command = cli.command(&["pane", "get", "w1:p1"]);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["pane", "get", "w1:p1", "--session", "lab"]);

        let cli = Cli::machine("m-1");
        let command = cli.command(&["api", "snapshot"]);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--machine", "m-1", "api", "snapshot"]);
    }
}
