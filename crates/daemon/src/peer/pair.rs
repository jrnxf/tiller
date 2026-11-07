//! Pairing with a host (`protocol.md` 10.1). `tiller peer add` and the
//! board's Add a host both ask the peer's `tiller peer accept --json` and
//! check its answer here. The board's run is unattended: `ssh -T` in batch
//! mode, and the peer's accept runs under sshd with no prompt, as `tiller
//! peer serve` does (`authority.md` G45).

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tiller_core::config_file::{self as config_toml, ConfigError};
use tiller_core::domain::PeerScope;
use tokio::process::Command;
use tokio::sync::Mutex;

/// Every pairing from the board runs alone: two at once for one label would
/// share its pending key file.
pub static PAIRING: Mutex<()> = Mutex::const_new(());

/// How long one unattended accept may take, ssh's connect included.
pub const ACCEPT_LIMIT: Duration = Duration::from_secs(30);

/// What the peer's `tiller peer accept --json` answered, checked.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub host_id: String,
    pub label: String,
    pub key_id: String,
    pub scope: Value,
    pub schema_version: Value,
    pub peer_protocol: Value,
    pub tiller_version: Value,
}

/// Why an answer is not an accept.
#[derive(Debug, PartialEq)]
pub enum NotAccepted {
    /// The peer answered with an error: its code and message.
    Answered { code: String, message: String },
    /// The answer is not an accept of the scope asked for.
    Malformed(String),
}

/// Checks the peer's response for an accept of `scope`.
pub fn accepted_reply(label: &str, scope: PeerScope, reply: &Value) -> Result<Reply, NotAccepted> {
    if reply.get("ok") != Some(&Value::Bool(true)) {
        let error = reply.get("error");
        let field = |name: &str, default: &str| {
            error
                .and_then(|e| e.get(name))
                .and_then(Value::as_str)
                .unwrap_or(default)
                .to_owned()
        };
        return Err(NotAccepted::Answered {
            code: field("code", "unknown"),
            message: field("message", "no message"),
        });
    }
    let data = reply.get("data").cloned().unwrap_or(Value::Null);
    let text = |field: &str| {
        data.get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                NotAccepted::Malformed(format!("{label} answered the accept without {field}"))
            })
    };
    let reply = Reply {
        host_id: text("host_id")?,
        label: text("label")?,
        key_id: text("key_id")?,
        scope: data.get("scope").cloned().unwrap_or(Value::Null),
        schema_version: data.get("schema_version").cloned().unwrap_or(Value::Null),
        peer_protocol: data.get("peer_protocol").cloned().unwrap_or(Value::Null),
        tiller_version: data.get("tiller_version").cloned().unwrap_or(Value::Null),
    };
    if reply.scope != json!(scope) {
        return Err(NotAccepted::Malformed(format!(
            "{label} accepted scope {}, not {scope}",
            reply.scope
        )));
    }
    Ok(reply)
}

/// One unattended `tiller peer accept` on the host `alias` reaches.
pub struct Accept<'a> {
    pub alias: &'a str,
    /// This host's label.
    pub from: &'a str,
    /// This host's `meta.host_id`.
    pub host_id: &'a str,
    pub public_key: &'a str,
    pub scope: PeerScope,
}

impl Accept<'_> {
    /// `ssh -T -o BatchMode=yes -o ConnectTimeout=10 -- <alias> tiller peer
    /// accept ... --unattended --json`. Batch mode and ssh's own host key
    /// check, as the link dials: an unknown host key fails, and never asks.
    pub fn args(&self) -> Vec<String> {
        [
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "--",
            self.alias,
            "tiller",
            "peer",
            "accept",
            "--from",
            self.from,
            "--viewer-host-id",
            self.host_id,
            "--pubkey",
            self.public_key,
            "--scope",
            self.scope.as_str(),
            "--unattended",
            "--json",
        ]
        .map(str::to_owned)
        .to_vec()
    }
}

/// An accept that gave no JSON answer.
#[derive(Debug, PartialEq)]
pub struct NoAnswer {
    pub cause: Cause,
    /// Why, in a few words: ssh's last line of stderr, or the time limit.
    pub why: String,
}

/// What kept the accept from answering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cause {
    /// ssh did not run, connect or log in.
    Unreached,
    /// ssh logged in, and its shell found no `tiller` (exit 127).
    NoTiller,
    /// No answer within the limit. The new host may have accepted.
    TimedOut,
}

/// Runs the accept with `program` (`ssh`, or `TILLER_SSH` split on
/// whitespace) and returns the last JSON line it printed. `limit` covers the
/// whole run; ssh is killed when it passes.
pub async fn run(
    program: &[OsString],
    accept: &Accept<'_>,
    limit: Duration,
) -> Result<Value, NoAnswer> {
    let Some((ssh, leading)) = program.split_first() else {
        return Err(NoAnswer {
            cause: Cause::Unreached,
            why: "no ssh program".into(),
        });
    };
    let child = Command::new(ssh)
        .args(leading)
        .args(accept.args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| NoAnswer {
            cause: Cause::Unreached,
            why: format!("cannot run ssh: {error}"),
        })?;
    let output = match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(NoAnswer {
                cause: Cause::Unreached,
                why: format!("ssh failed: {error}"),
            });
        }
        Err(_) => {
            return Err(NoAnswer {
                cause: Cause::TimedOut,
                why: format!("no answer in {} seconds", limit.as_secs()),
            });
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The last object line: a shell's rc file may print after it.
    if let Some(reply) = stdout.lines().rev().find_map(|line| {
        serde_json::from_str::<Value>(line.trim())
            .ok()
            .filter(Value::is_object)
    }) {
        return Ok(reply);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let last = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| plain(line.strip_prefix("ssh: ").unwrap_or(line)));
    let code = output.status.code();
    let exit = code.map_or("a signal".to_owned(), |code| format!("exit {code}"));
    Err(NoAnswer {
        cause: if code == Some(127) {
            Cause::NoTiller
        } else {
            Cause::Unreached
        },
        why: match last {
            Some(line) => format!("{line} (ssh {exit})"),
            None => format!("no answer (ssh {exit})"),
        },
    })
}

/// Sets config `peers.<label>.ssh` to `alias` in `<home>/config.toml`, as
/// `tiller config set` writes it: through a temporary file and a rename, so
/// a reader never sees half a file. False when it already held `alias`.
pub fn write_alias(home: &Path, label: &str, alias: &str) -> Result<bool, ConfigError> {
    let path = home.join("config.toml");
    let mut config = config_toml::load(&path)?;
    let key = format!("peers.{label}.ssh");
    if config_toml::get(&config, &key).and_then(toml::Value::as_str) == Some(alias) {
        return Ok(false);
    }
    config_toml::set(&mut config, &key, toml::Value::String(alias.to_owned()))?;
    config_toml::store(&path, &config)?;
    Ok(true)
}

/// The labels whose config `peers.<label>.ssh` is `alias`.
pub fn labels_for(config: &toml::Table, alias: &str) -> Vec<String> {
    let peers = config_toml::get(config, "peers").and_then(toml::Value::as_table);
    peers
        .into_iter()
        .flatten()
        .filter(|(_, peer)| peer.get("ssh").and_then(toml::Value::as_str) == Some(alias))
        .map(|(label, _)| label.clone())
        .collect()
}

/// A line from the other host, safe to show: printable characters only, at
/// most 200 of them.
fn plain(line: &str) -> String {
    let mut text: String = line.chars().filter(|c| !c.is_control()).take(200).collect();
    if line.chars().count() > 200 {
        text.push_str("...");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept_reply(scope: &str) -> Value {
        json!({"ok": true, "data": {
            "host_id": "01JC00000000000000000PEER1", "label": "host-b",
            "key_id": "01JC00000000000000000KEY1", "scope": scope,
            "schema_version": 4, "peer_protocol": "tiller-peer/1", "tiller_version": "0.1.0",
        }})
    }

    #[test]
    fn the_reply_must_accept_the_scope_asked_for() {
        let reply = accepted_reply("host-b", PeerScope::Act, &accept_reply("act")).unwrap();
        assert_eq!(reply.host_id, "01JC00000000000000000PEER1");
        assert!(matches!(
            accepted_reply("host-b", PeerScope::Act, &accept_reply("read")),
            Err(NotAccepted::Malformed(_))
        ));
        let mut no_key = accept_reply("act");
        no_key["data"]["key_id"] = Value::Null;
        assert!(matches!(
            accepted_reply("host-b", PeerScope::Act, &no_key),
            Err(NotAccepted::Malformed(_))
        ));
        let off = json!({"ok": false, "error": {"code": "refused", "message": "off"}});
        assert_eq!(
            accepted_reply("host-b", PeerScope::Act, &off),
            Err(NotAccepted::Answered {
                code: "refused".into(),
                message: "off".into()
            })
        );
    }

    #[test]
    fn the_alias_follows_the_end_of_options() {
        let accept = Accept {
            alias: "host-b",
            from: "board",
            host_id: "01JC0000000000000000B0ARD1",
            public_key: "a2V5",
            scope: PeerScope::Read,
        };
        let args = accept.args();
        let at = args.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(args[at + 1], "host-b");
        assert!(args.contains(&"BatchMode=yes".to_owned()));
        assert!(!args.iter().any(|arg| arg.contains("StrictHostKeyChecking")));
        assert_eq!(&args[args.len() - 2..], ["--unattended", "--json"]);
    }

    #[test]
    fn the_alias_joins_the_config_and_keeps_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        assert!(write_alias(home.path(), "host-b", "b.example.com").unwrap());
        assert!(!write_alias(home.path(), "host-b", "b.example.com").unwrap());
        std::fs::write(
            &path,
            "[federation]\naccept = true\n[peers.host-b]\nssh = \"b.example.com\"\n",
        )
        .unwrap();
        assert!(write_alias(home.path(), "host-c", "c.example.com").unwrap());
        let config: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(config["federation"]["accept"].as_bool(), Some(true));
        assert_eq!(
            config["peers"]["host-b"]["ssh"].as_str(),
            Some("b.example.com")
        );
        assert_eq!(
            config["peers"]["host-c"]["ssh"].as_str(),
            Some("c.example.com")
        );
        std::fs::write(&path, "[peers\n").unwrap();
        assert!(write_alias(home.path(), "host-b", "b").is_err());
    }

    fn script(body: &str) -> (tempfile::TempDir, Vec<OsString>) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, vec![path.into_os_string()])
    }

    fn accept() -> Accept<'static> {
        Accept {
            alias: "host-b",
            from: "board",
            host_id: "01JC0000000000000000B0ARD1",
            public_key: "a2V5",
            scope: PeerScope::Act,
        }
    }

    #[tokio::test]
    async fn the_answer_is_the_last_object_line() {
        let (_dir, ssh) = script(r#"echo hello; echo '{"ok":true}'; echo 0; echo true"#);
        let reply = run(&ssh, &accept(), Duration::from_secs(10)).await;
        assert_eq!(reply, Ok(json!({"ok": true})));
    }

    #[tokio::test]
    async fn no_answer_says_why() {
        let (_dir, ssh) = script("echo 'ssh: connect to host b: Connection refused' >&2; exit 255");
        let no = run(&ssh, &accept(), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(no.cause, Cause::Unreached);
        assert_eq!(
            no.why,
            "connect to host b: Connection refused (ssh exit 255)"
        );
        let (_dir, ssh) = script("echo 'sh: tiller: command not found' >&2; exit 127");
        let no = run(&ssh, &accept(), Duration::from_secs(10))
            .await
            .unwrap_err();
        assert_eq!(no.cause, Cause::NoTiller);
        let (_dir, ssh) = script("sleep 5");
        let no = run(&ssh, &accept(), Duration::from_millis(200))
            .await
            .unwrap_err();
        assert_eq!(no.cause, Cause::TimedOut);
    }

    #[test]
    fn an_alias_names_its_labels() {
        let config: toml::Table =
            "[peers.host-b]\nssh = \"b.example.com\"\n[peers.host-c]\nssh = \"c.example.com\"\n"
                .parse()
                .unwrap();
        assert_eq!(labels_for(&config, "b.example.com"), ["host-b"]);
        assert!(labels_for(&config, "d.example.com").is_empty());
        assert!(labels_for(&toml::Table::new(), "b.example.com").is_empty());
    }

    #[test]
    fn a_line_from_the_peer_is_plain_and_short() {
        assert_eq!(
            plain("Permission denied\u{1b}[31m"),
            "Permission denied[31m"
        );
        assert_eq!(plain(&"x".repeat(300)).len(), 203);
    }
}
