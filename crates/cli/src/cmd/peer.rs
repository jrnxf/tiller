//! `tiller peer`: pairing for the federated board (ADR 0003; `protocol.md`
//! sections 6.4 and 10.1). `add` runs on the board host and reaches the
//! peer's `accept` over `ssh -t`; both need the owner at a terminal. The
//! board's Add a host runs `accept --unattended` instead, which sshd must
//! start, as it starts `serve`, the peer's half of a link (10.2).

use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use clap::Subcommand;
use serde_json::{Value, json};
use tiller_core::Applied;
use tiller_core::domain::PeerScope;
use tiller_core::state::peers::{
    self, KeyFile, NewPairing, NewPeerKey, PEER_PROTOCOL, check_alias, check_label, fingerprint,
    parse_public_key,
};
use tiller_core::state::projects::meta;
use tiller_core::store::{Actor, Ctx};
use tiller_daemon::peer::Timing;
use tiller_daemon::peer::pair::{self, NotAccepted, Reply};
use tiller_daemon::peer::serve::{ServeConfig, ServeError};

use super::admin::{get_path, load_config, set_config};
use crate::app::{App, Done, Failure, Outcome};

#[derive(Subcommand, Debug)]
pub enum PeerCmd {
    /// Pair with another host running its own Tiller, over `ssh -t`
    Add {
        label: String,
        /// The ssh alias that reaches it
        #[arg(long)]
        ssh: String,
        /// Read the peer only; never forward a mutation to it
        #[arg(long)]
        read_only: bool,
        /// The herdr saved machine name; the label by default
        #[arg(long)]
        herdr_machine: Option<String>,
    },
    /// Accept a board host's key on this host; `tiller peer add` runs it
    Accept {
        /// The board host's label
        #[arg(long)]
        from: String,
        /// The board host's `meta.host_id`
        #[arg(long)]
        viewer_host_id: String,
        /// Its Ed25519 public key, base64
        #[arg(long)]
        pubkey: String,
        /// read, or act: read and forward the owner's mutations
        #[arg(long)]
        scope: PeerScope,
        /// Accept with no prompt: the board host's daemon asks over ssh
        /// when the owner adds this host on its board. sshd must start it.
        #[arg(long)]
        unattended: bool,
    },
    /// The hosts this one is paired with, and the board hosts it accepted
    List,
    /// Unpair a host here and delete its key file
    Remove { label: String },
    /// Revoke a board host's key on this host
    Revoke {
        /// The board host's label, or the key id
        viewer: String,
    },
    /// Serve this host's records to a paired board host over stdin and
    /// stdout; the board host's daemon runs it over ssh
    Serve {
        /// The board host's label
        #[arg(long)]
        from: String,
    },
}

pub fn run(app: &App, cmd: PeerCmd) -> Done {
    app.require_owner("tiller peer")?;
    match cmd {
        PeerCmd::Add {
            label,
            ssh,
            read_only,
            herdr_machine,
        } => {
            let scope = if read_only {
                PeerScope::Read
            } else {
                PeerScope::Act
            };
            add(app, &label, &ssh, scope, herdr_machine)
        }
        PeerCmd::Accept {
            from,
            viewer_host_id,
            pubkey,
            scope,
            unattended,
        } => accept(
            app,
            NewPeerKey {
                viewer_label: from,
                viewer_host_id,
                public_key: pubkey,
                scope,
                unattended,
            },
        ),
        PeerCmd::List => list(app),
        PeerCmd::Remove { label } => remove(app, &label),
        PeerCmd::Revoke { viewer } => {
            let mut store = app.write()?;
            let applied = store.peer_revoke(&app.ctx(), &viewer)?;
            Ok(Outcome::applied_as(applied, key_json))
        }
        PeerCmd::Serve { .. } => Err(Failure::new(
            "internal",
            1,
            "peer serve is run before dispatch",
        )),
    }
}

// -- serve, on the peer ----------------------------------------------------

/// `tiller peer serve --from <label>` (`protocol.md` 10.2). Once the start
/// check passes, stdout is the link: nothing else is printed there, and an
/// error goes to stderr.
pub fn serve(app: &App, from: String) -> Result<ExitCode, Failure> {
    require_sshd_owner("tiller peer serve")?;
    let config = ServeConfig {
        home: app.home.clone(),
        from,
        timing: Timing::default(),
        herdr: Some(super::spawn::configured_herdr(app)?),
        orchestrators: Some(std::sync::Arc::new(super::spawn::BoardStart {
            app: App {
                home: app.home.clone(),
                db: app.db.clone(),
                actor: app.actor.clone(),
            },
        })),
    };
    let input = std::io::BufReader::new(std::io::stdin());
    match tiller_daemon::peer::serve::serve(&config, input, std::io::stdout().lock()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error @ ServeError::Refused { .. }) => {
            eprintln!("tiller peer serve: {error}");
            Ok(ExitCode::from(6))
        }
        Err(error) => {
            eprintln!("tiller peer serve: {error}");
            Ok(ExitCode::from(1))
        }
    }
}

/// The start check of `peer serve` and `peer accept --unattended`, read
/// from this process.
fn require_sshd_owner(what: &str) -> Result<(), Failure> {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    serve_start_check(
        what,
        set("TILLER_SESSION_ID"),
        set("CLAUDECODE"),
        set("SSH_CONNECTION"),
        ancestors,
    )
}

/// `peer serve` and `peer accept --unattended` start only when sshd started
/// them for the owner (`authority.md` G44, G45): `SSH_CONNECTION` set, no
/// `TILLER_SESSION_ID`, no `CLAUDECODE` and no Claude Code process among
/// the ancestors.
fn serve_start_check(
    what: &str,
    session: bool,
    claudecode: bool,
    ssh_connection: bool,
    ancestors: impl FnOnce() -> std::io::Result<Vec<Process>>,
) -> Result<(), Failure> {
    let refuse = |why: String| {
        Failure::forbidden(format!("{what} runs only under sshd for the owner: {why}"))
            .hint("the board host's daemon starts it over ssh; never run it from an agent")
    };
    if session {
        return Err(refuse("TILLER_SESSION_ID is set".into()));
    }
    if claudecode {
        return Err(refuse("CLAUDECODE is set".into()));
    }
    if !ssh_connection {
        return Err(refuse("SSH_CONNECTION is not set".into()));
    }
    let chain = ancestors().map_err(|error| refuse(format!("the parent chain: {error}")))?;
    if let Some(claude) = chain.iter().find(|process| is_claude_code(process)) {
        return Err(refuse(format!(
            "Claude Code (pid {}) is among its ancestors",
            claude.pid
        )));
    }
    Ok(())
}

// -- add, on the board host ------------------------------------------------

fn add(
    app: &App,
    label: &str,
    alias: &str,
    scope: PeerScope,
    herdr_machine: Option<String>,
) -> Done {
    check_label(label)?;
    check_alias(alias)?;
    require_terminal_owner("tiller peer add")?;
    let mut store = app.write()?;
    let this = store.this_host()?;
    if this.label == label {
        return Err(Failure::refused(format!(
            "{label} is this host; a host does not pair with itself"
        )));
    }
    // This host's label is a word on the peer's command line.
    check_label(&this.label)?;
    let host_id = meta(store.conn(), "host_id")?
        .ok_or_else(|| Failure::refused("state.db has no host id; run tiller init"))?;
    // The peer an earlier pairing under this label reached. Only the same
    // peer may renew it, which the answer's host id shows (`peer.pair`).
    let paired_to = store
        .host_labelled(label)?
        .filter(|host| host.peer_scope.is_some())
        .and_then(|host| host.peer_host_id);

    // Step 2: the key, and its file before the peer sees the public half. The
    // file waits under a pending name until the peer answers, so an earlier
    // pairing's file stays in place when this add fails or is interrupted.
    let key = peers::generate_key()?;
    let public_key = peers::public_key_text(&key);
    let path = peers::key_path(&app.home, label)?;
    let pending = path.with_file_name(format!(".{label}.key.pending"));
    let file = KeyFile::new(label, scope, &key);
    peers::write_key_file(&pending, &file)?;
    eprintln!(
        "pairing {label} through ssh {alias}: key {}",
        fingerprint(&public_key)
    );
    // A failure from here deletes the new key. The earlier key file goes
    // too once the peer has revoked it, and otherwise stays as it was.
    let undo = |failure: Failure, replaced: bool| -> Failure {
        let _ = peers::remove_key_file(&pending);
        if replaced {
            let _ = peers::remove_key_file(&path);
        }
        failure
    };

    // Step 3: the peer's owner confirms at its terminal.
    let answer = ssh_accept(alias, &this.label, &host_id, &public_key, scope)
        .map_err(|failure| undo(failure, false))?;
    let replaced = peer_replaced(&answer, paired_to.as_deref());
    let reply = accepted_reply(label, scope, &answer).map_err(|failure| undo(failure, replaced))?;
    if let Some(old) = &paired_to
        && *old != reply.host_id
    {
        return Err(undo(
            Failure::refused(format!(
                "{label} is paired with host {old}, but {alias} answered as host {}: \
                 run tiller peer remove {label} first, and tiller peer revoke {} on {alias} \
                 to drop the key it just accepted",
                reply.host_id, this.label
            )),
            false,
        ));
    }

    // Step 4: the answer into the key file, then `peer.pair` and the alias.
    // The peer has replaced any earlier key from this host by now, so a
    // failure here deletes the key file: run `peer add` again.
    let file = file.accepted(&reply.host_id, &reply.key_id);
    peers::write_key_file(&path, &file).map_err(|error| undo(error.into(), true))?;
    let _ = peers::remove_key_file(&pending);
    let drop_all = |failure: Failure| undo(failure, true);
    let paired = store
        .peer_pair(
            &app.ctx(),
            NewPairing {
                label: label.to_owned(),
                herdr_machine,
                scope,
                peer_host_id: reply.host_id.clone(),
            },
        )
        .map_err(|error| drop_all(error.into()))?;
    let config = set_config(
        app,
        &mut store,
        &format!("peers.{label}.ssh"),
        toml::Value::String(alias.to_owned()),
    )
    .map_err(drop_all)?;

    let mut events = paired.events;
    events.extend(config.events);
    let applied = Applied {
        value: paired.value,
        events,
        // The peer always records a new key, so a second add is never a
        // replay even when nothing here changed.
        replayed: false,
    };
    Ok(Outcome::applied_as(applied, |host| {
        json!({
            "label": host.label,
            "kind": host.kind,
            "herdr_machine": host.herdr_machine,
            "peer_scope": host.peer_scope,
            "peer_host_id": host.peer_host_id,
            "ssh": alias,
            "key_id": reply.key_id,
            "fingerprint": fingerprint(&public_key),
            "peer": {
                "label": reply.label,
                "schema_version": reply.schema_version,
                "peer_protocol": reply.peer_protocol,
                "tiller_version": reply.tiller_version,
            },
        })
    }))
}

/// Runs the peer's accept with a terminal (`-t`), so its owner sees the
/// prompt and types the answer here. Everything the peer prints reaches
/// this terminal; only its JSON response is kept back.
fn ssh_accept(
    alias: &str,
    from: &str,
    host_id: &str,
    public_key: &str,
    scope: PeerScope,
) -> Result<Value, Failure> {
    let ssh = std::env::var("TILLER_SSH").unwrap_or_else(|_| "ssh".into());
    let mut child = Command::new(ssh)
        .args([
            "-t", "--", alias, "tiller", "peer", "accept", "--from", from,
        ])
        .args(["--viewer-host-id", host_id, "--pubkey", public_key])
        .args(["--scope", scope.as_str(), "--json"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| Failure::unavailable(format!("cannot run ssh to {alias}: {error}")))?;
    let mut relay = Relay::default();
    if let Some(mut out) = child.stdout.take() {
        let mut stderr = std::io::stderr();
        let mut buffer = [0u8; 4096];
        loop {
            let n = match out.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            let shown = relay.feed(&buffer[..n]);
            let _ = stderr.write_all(&shown);
            let _ = stderr.flush();
        }
    }
    let status = child.wait()?;
    let held = relay.finish();
    let reply = held
        .iter()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .ok_or_else(|| {
            Failure::unavailable(format!(
                "{alias} gave no answer to tiller peer accept (ssh exit {}); \
                 if its owner accepted, the earlier key from this host is revoked \
                 there: run tiller peer add again",
                status
                    .code()
                    .map_or("unknown".to_owned(), |c| c.to_string())
            ))
        })?;
    Ok(reply)
}

/// True when the peer committed `peer.accept` and is the peer `paired_to`
/// names: it revoked the key of the earlier pairing in the same transaction.
fn peer_replaced(answer: &Value, paired_to: Option<&str>) -> bool {
    answer.get("ok") == Some(&Value::Bool(true))
        && paired_to.is_some()
        && answer.pointer("/data/host_id").and_then(Value::as_str) == paired_to
}

/// Checks the peer's response: a failure comes back as one here, with the
/// peer's code where it is one this side knows.
fn accepted_reply(label: &str, scope: PeerScope, reply: &Value) -> Result<Reply, Failure> {
    pair::accepted_reply(label, scope, reply).map_err(|not| match not {
        NotAccepted::Answered { code, message } => {
            let text = format!("{label} did not accept the pairing: {message}");
            match code.as_str() {
                "refused" => Failure::refused(text),
                "forbidden" => Failure::forbidden(text),
                "usage" => Failure::usage(text),
                _ => Failure::unavailable(text),
            }
        }
        NotAccepted::Malformed(message) => Failure::unavailable(message),
    })
}

/// Splits what `ssh -t` prints into what the owner sees and the JSON lines
/// it carries. A terminal turns `\n` into `\r\n`, and a prompt has no line
/// end, so every byte that is not on a line starting `{` goes out at once.
#[derive(Default)]
struct Relay {
    line: Vec<u8>,
    held: bool,
    lines: Vec<String>,
}

impl Relay {
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut shown = Vec::new();
        for &byte in bytes {
            if self.line.is_empty() && !self.held {
                self.held = byte == b'{';
            }
            if self.held {
                if byte == b'\n' {
                    self.hold_line();
                } else {
                    self.line.push(byte);
                }
            } else {
                shown.push(byte);
                if byte == b'\n' {
                    self.line.clear();
                } else {
                    self.line.push(byte);
                }
            }
        }
        shown
    }

    fn hold_line(&mut self) {
        let text = String::from_utf8_lossy(&self.line);
        self.lines.push(text.trim_end_matches('\r').to_owned());
        self.line.clear();
        self.held = false;
    }

    fn finish(mut self) -> Vec<String> {
        if self.held {
            self.hold_line();
        }
        self.lines
    }
}

// -- accept, on the peer ---------------------------------------------------

fn accept(app: &App, new: NewPeerKey) -> Done {
    let unattended = new.unattended;
    check_label(&new.viewer_label)?;
    peers::check_host_id("viewer host id", &new.viewer_host_id)?;
    parse_public_key(&new.public_key)?;
    // An unattended accept is the board host's owner, through its board
    // (`authority.md` section 3): the event says `board@<label>`.
    let ctx = if unattended {
        Ctx::new(Actor::Peer(new.viewer_label.clone()))
    } else {
        app.ctx()
    };
    let mut store = app.write()?;
    match get_path(&load_config(app)?, "federation.accept") {
        None | Some(toml::Value::Boolean(true)) => {}
        Some(toml::Value::Boolean(false)) => {
            // The core refusal carries the reason.
            store.peer_accept(&ctx, false, new)?;
            return Err(Failure::refused("config federation.accept is false"));
        }
        Some(other) => {
            return Err(Failure::refused(format!(
                "this host refuses pairing: config federation.accept is {other}, not true or false"
            )));
        }
    }
    let this = store.this_host()?;
    if unattended {
        require_sshd_owner("tiller peer accept --unattended")?;
    } else {
        require_terminal_owner("tiller peer accept")?;
        confirm(&this.label, &new)?;
    }
    let host_id = meta(store.conn(), "host_id")?.unwrap_or_default();
    let applied = store.peer_accept(&ctx, true, new)?;
    Ok(Outcome::applied_as(applied, |key| {
        json!({
            "host_id": host_id,
            "label": this.label,
            "key_id": key.id,
            "scope": key.scope,
            "schema_version": tiller_core::migrate::LATEST,
            "peer_protocol": PEER_PROTOCOL,
            "tiller_version": env!("CARGO_PKG_VERSION"),
        })
    }))
}

/// The y/N the owner types at the board host's terminal, through `ssh -t`.
fn confirm(this: &str, new: &NewPeerKey) -> Result<(), Failure> {
    let what = match new.scope {
        PeerScope::Act => "read this host and act on its records for you",
        PeerScope::Read => "read this host",
    };
    eprint!(
        "{} ({}) asks to {what}.\nKey {}\nAccept it on {this}? [y/N] ",
        new.viewer_label,
        new.viewer_host_id,
        fingerprint(new.public_key.trim()),
    );
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Err(Failure::refused(format!(
            "the owner of {this} did not accept {}",
            new.viewer_label
        )));
    }
    Ok(())
}

/// Config `federation.accept`: pairing and the peer link are allowed when it
/// is `true` or absent (ADR 0003). Any other value, even one that is not a
/// boolean, keeps this host closed.
fn federation_accept(config: &toml::Table) -> bool {
    matches!(
        get_path(config, "federation.accept"),
        None | Some(toml::Value::Boolean(true))
    )
}

// -- list and remove -------------------------------------------------------

fn list(app: &App) -> Done {
    let store = app.read()?;
    let config = load_config(app)?;
    let paired = store
        .paired_hosts()?
        .into_iter()
        .map(|host| {
            let ssh = get_path(&config, &format!("peers.{}.ssh", host.label))
                .and_then(toml::Value::as_str)
                .map(str::to_owned);
            let key_file = peers::key_path(&app.home, &host.label).is_ok_and(|path| path.exists());
            json!({
                "label": host.label,
                "scope": host.peer_scope,
                "peer_host_id": host.peer_host_id,
                "ssh": ssh,
                "key_file": key_file,
                // The daemon's link state and `as_of` arrive with the peer
                // link (`protocol.md` 10.2).
                "link": null,
                "as_of": null,
            })
        })
        .collect::<Vec<_>>();
    let accepted = store.peer_keys()?.iter().map(key_json).collect::<Vec<_>>();
    Ok(Outcome::read(json!({
        "federation_accept": federation_accept(&config),
        "peers": paired,
        "accepted": accepted,
    })))
}

fn remove(app: &App, label: &str) -> Done {
    check_label(label)?;
    let mut store = app.write()?;
    let applied = store.peer_remove(&app.ctx(), label)?;
    // After the commit (`state.md` section 8): a crash before this leaves a
    // key file for an unpaired label, which the next `peer add` replaces.
    peers::remove_key_file(&peers::key_path(&app.home, label)?)?;
    Ok(Outcome::applied(applied))
}

fn key_json(key: &tiller_core::domain::PeerKey) -> Value {
    json!({
        "id": key.id,
        "viewer_label": key.viewer_label,
        "viewer_host_id": key.viewer_host_id,
        "scope": key.scope,
        "fingerprint": fingerprint(&key.public_key),
        "accepted_at": key.accepted_at,
        "revoked_at": key.revoked_at,
        "rev": key.rev,
    })
}

// -- the owner at a terminal -----------------------------------------------

/// The CLI owner principal for pairing (`authority.md` section 3, G22): no
/// `CLAUDECODE`, stdin a terminal, and no Claude Code process among the
/// caller's ancestors. `require_owner` has already refused a session id.
fn require_terminal_owner(what: &str) -> Result<(), Failure> {
    let claudecode = std::env::var_os("CLAUDECODE").is_some_and(|value| !value.is_empty());
    owner_principal(what, claudecode, std::io::stdin().is_terminal(), ancestors)
}

/// One process in the caller's parent chain.
#[derive(Clone, Debug)]
struct Process {
    pid: u32,
    comm: String,
    args: String,
}

fn owner_principal(
    what: &str,
    claudecode: bool,
    stdin_terminal: bool,
    ancestors: impl FnOnce() -> std::io::Result<Vec<Process>>,
) -> Result<(), Failure> {
    let refuse = |why: String| {
        Failure::forbidden(format!("{what} needs the owner at a terminal: {why}"))
            .hint("run it yourself in a terminal, not from an agent")
    };
    if claudecode {
        return Err(refuse("CLAUDECODE is set".into()));
    }
    if !stdin_terminal {
        return Err(refuse("stdin is not a terminal".into()));
    }
    let chain = ancestors().map_err(|error| refuse(format!("the parent chain: {error}")))?;
    if let Some(claude) = chain.iter().find(|process| is_claude_code(process)) {
        return Err(refuse(format!(
            "Claude Code (pid {}) is among its ancestors",
            claude.pid
        )));
    }
    Ok(())
}

/// A Claude Code process, however it was installed: the native binary
/// (`claude`, or `.claude-wrapped` behind a Nix wrapper) or the npm package.
fn is_claude_code(process: &Process) -> bool {
    let base = |path: &str| {
        Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let named = |name: &str| {
        let name = name.trim_start_matches('.');
        name.strip_suffix("-wrapped").unwrap_or(name) == "claude"
    };
    let argv0 = process.args.split_whitespace().next().unwrap_or_default();
    named(&base(&process.comm))
        || named(&base(argv0))
        || process.args.contains("@anthropic-ai/claude-code")
}

/// The caller's ancestors, nearest first, up to but not including pid 1.
fn ancestors() -> std::io::Result<Vec<Process>> {
    let mut chain = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && chain.len() < 128 {
        let (parent, process) = read_process(pid)?;
        chain.push(process);
        pid = parent;
    }
    Ok(chain)
}

#[cfg(target_os = "linux")]
fn read_process(pid: u32) -> std::io::Result<(u32, Process)> {
    let invalid = |what: &str| std::io::Error::other(format!("pid {pid}: {what}"));
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let open = stat.find('(').ok_or_else(|| invalid("no command name"))?;
    let close = stat.rfind(')').ok_or_else(|| invalid("no command name"))?;
    let comm = stat.get(open + 1..close).unwrap_or_default().to_owned();
    let parent = stat
        .get(close + 1..)
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|ppid| ppid.parse().ok())
        .ok_or_else(|| invalid("no parent pid"))?;
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    let args = String::from_utf8_lossy(&cmdline)
        .split('\0')
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Ok((parent, Process { pid, comm, args }))
}

#[cfg(not(target_os = "linux"))]
fn read_process(pid: u32) -> std::io::Result<(u32, Process)> {
    let ps = |field: &str| -> std::io::Result<String> {
        let output = Command::new("ps")
            .args(["-o", field, "-p", &pid.to_string()])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!("ps found no pid {pid}")));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let parent = ps("ppid=")?
        .parse()
        .map_err(|_| std::io::Error::other(format!("pid {pid}: no parent pid")))?;
    let comm = ps("comm=")?;
    let args = ps("args=")?;
    Ok((parent, Process { pid, comm, args }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(comm: &str, args: &str) -> Process {
        Process {
            pid: 42,
            comm: comm.into(),
            args: args.into(),
        }
    }

    fn chain(processes: Vec<Process>) -> impl FnOnce() -> std::io::Result<Vec<Process>> {
        move || Ok(processes)
    }

    #[test]
    fn the_owner_is_a_terminal_with_no_claude_code_behind_it() {
        let shell = vec![process("zsh", "-zsh"), process("sshd", "sshd: user@pts/0")];
        owner_principal("x", false, true, chain(shell.clone())).unwrap();

        let refused = |claudecode, terminal, processes| {
            let failure = owner_principal("x", claudecode, terminal, chain(processes)).unwrap_err();
            assert_eq!(failure.exit, 6);
            failure.message.clone()
        };
        assert!(refused(true, true, shell.clone()).contains("CLAUDECODE"));
        assert!(refused(false, false, shell.clone()).contains("not a terminal"));
        for claude in [
            process("claude", "claude"),
            process(
                ".claude-wrapped",
                "/home/user/.nix-profile/bin/claude --resume",
            ),
            process(
                "node",
                "node /home/user/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ),
            process("2.1.0", "/home/user/.local/bin/claude"),
        ] {
            let mut processes = shell.clone();
            processes.insert(1, claude.clone());
            assert!(
                refused(false, true, processes).contains("Claude Code"),
                "{claude:?}"
            );
        }
        let unreadable =
            owner_principal("x", false, true, || Err(std::io::Error::other("gone"))).unwrap_err();
        assert_eq!(unreadable.exit, 6, "an unreadable chain is not the owner");
    }

    #[test]
    fn serve_refuses_session_and_claude_ancestor() {
        let sshd = vec![
            process("sshd", "sshd: user@notty"),
            process("sshd", "/usr/sbin/sshd -D"),
        ];
        serve_start_check("serve", false, false, true, chain(sshd.clone())).unwrap();

        let refused = |session, claudecode, ssh, processes| {
            let failure =
                serve_start_check("serve", session, claudecode, ssh, chain(processes)).unwrap_err();
            assert_eq!(failure.exit, 6);
            failure.message.clone()
        };
        assert!(refused(true, false, true, sshd.clone()).contains("TILLER_SESSION_ID"));
        assert!(refused(false, true, true, sshd.clone()).contains("CLAUDECODE"));
        assert!(refused(false, false, false, sshd.clone()).contains("SSH_CONNECTION"));
        let mut under_claude = sshd.clone();
        under_claude.insert(0, process("claude", "claude"));
        assert!(refused(false, false, true, under_claude).contains("Claude Code"));
        let unreadable = serve_start_check("serve", false, false, true, || {
            Err(std::io::Error::other("gone"))
        })
        .unwrap_err();
        assert_eq!(unreadable.exit, 6, "an unreadable chain never serves");
    }

    #[test]
    fn other_programs_named_like_claude_are_not_claude_code() {
        for other in [
            process("claude-notes", "claude-notes"),
            process("vim", "vim claude.md"),
        ] {
            assert!(!is_claude_code(&other), "{other:?}");
        }
    }

    #[test]
    fn relay_shows_the_prompt_at_once_and_keeps_the_json() {
        let mut relay = Relay::default();
        let mut shown = relay.feed(b"board (01J) asks to read this host.\r\nAccept it? [y/N] ");
        assert_eq!(
            shown,
            b"board (01J) asks to read this host.\r\nAccept it? [y/N] "
        );
        shown = relay.feed(b"y\r\n{\"ok\":true,");
        assert_eq!(shown, b"y\r\n");
        shown = relay.feed(b"\"data\":{}}\r\nConnection to b closed.\r\n");
        assert_eq!(shown, b"Connection to b closed.\r\n");
        assert_eq!(relay.finish(), [r#"{"ok":true,"data":{}}"#]);

        let mut cut = Relay::default();
        cut.feed(b"{\"ok\":false");
        assert_eq!(cut.finish(), [r#"{"ok":false"#]);
    }

    fn accept_reply(scope: &str) -> Value {
        json!({"ok": true, "data": {
            "host_id": "01JC00000000000000000PEER1", "label": "host-b",
            "key_id": "01JC00000000000000000KEY1", "scope": scope,
            "schema_version": 4, "peer_protocol": PEER_PROTOCOL, "tiller_version": "0.1.0",
        }})
    }

    #[test]
    fn the_accept_reply_is_checked() {
        let reply = accepted_reply("host-b", PeerScope::Act, &accept_reply("act")).unwrap();
        assert_eq!(reply.host_id, "01JC00000000000000000PEER1");
        assert_eq!(reply.key_id, "01JC00000000000000000KEY1");
        assert_eq!(
            accepted_reply("host-b", PeerScope::Act, &accept_reply("read"))
                .unwrap_err()
                .exit,
            7
        );
        let mut no_key = accept_reply("act");
        no_key["data"]["key_id"] = Value::Null;
        assert_eq!(
            accepted_reply("host-b", PeerScope::Act, &no_key)
                .unwrap_err()
                .exit,
            7
        );
        let off = json!({"ok": false, "error": {"code": "refused", "message": "config federation.accept is false"}});
        let failure = accepted_reply("host-b", PeerScope::Act, &off).unwrap_err();
        assert_eq!(failure.exit, 5);
        assert!(failure.message.contains("federation.accept"));
        let forbidden =
            json!({"ok": false, "error": {"code": "forbidden", "message": "no terminal"}});
        assert_eq!(
            accepted_reply("host-b", PeerScope::Act, &forbidden)
                .unwrap_err()
                .exit,
            6
        );
    }

    #[test]
    fn federation_accept_is_on_only_when_true_or_absent() {
        let table = |text: &str| text.parse::<toml::Table>().unwrap();
        assert!(federation_accept(&table("")));
        assert!(federation_accept(&table("[federation]\naccept = true")));
        for closed in ["false", "\"false\"", "\"true\"", "0", "1", "\"no\""] {
            let config = table(&format!("[federation]\naccept = {closed}"));
            assert!(!federation_accept(&config), "{closed}");
        }
    }

    #[test]
    fn only_an_accept_from_the_same_peer_replaces_the_old_key() {
        let peer = "01JC00000000000000000PEER1";
        let ok = |host_id: &str| json!({"ok": true, "data": {"host_id": host_id}});
        assert!(peer_replaced(&ok(peer), Some(peer)));
        assert!(!peer_replaced(&ok(peer), None), "nothing paired before");
        assert!(
            !peer_replaced(&ok("01JC00000000000000000PEER2"), Some(peer)),
            "another peer"
        );
        let refused = json!({"ok": false, "error": {"code": "refused"}});
        assert!(!peer_replaced(&refused, Some(peer)), "the peer said no");
        assert!(
            !peer_replaced(&json!({"ok": true, "data": {}}), Some(peer)),
            "no host id in the answer"
        );
    }
}
