//! The launch form (`authority.md` section 7, layer 1): the files a run's
//! session starts from, written into its scratch directory, and the one
//! line spawn types into the new pane.
//!
//! The role body and the brief go into one file passed with
//! `--append-system-prompt-file`, so the brief survives compaction and a
//! resume passes it again with the same flags. The orchestrator has no
//! brief and no first prompt: it starts at its composer, waiting for the
//! owner or its inbox. The settings fragment is the
//! role's template rendered for this run (T4.1). The run's `config_hash` is
//! the SHA-256 of a manifest over the role file, the fragment, the MCP
//! config, the model, the effort, the tiller and Claude Code versions and the
//! memory revisions rendered (`docs/design/learning-loop.md`, section 5),
//! so two runs can be compared part by part. The MCP config is the packaged
//! one (G35).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiller_core::domain::Effort;

use super::SpawnError;
use crate::roles::{self, MCP_CONFIG, Placeholders, Role};

/// The first prompt a worker gets; the brief itself is in its instructions.
pub const FIRST_PROMPT: &str = "Start on the brief at the end of your instructions.";

/// The directories only the orchestrator's fragment names (`<home>`,
/// `<pool>`, `<checkouts>`).
#[derive(Clone, Copy, Debug)]
pub struct Roots<'a> {
    pub home: &'a Path,
    pub pool: &'a Path,
    pub checkouts: &'a Path,
}

/// The first prompt of a relaunched session, which carries the relaunch
/// note: the progress the new session picks up from (P24).
pub fn relaunch_prompt(note: &str) -> String {
    format!(
        "{FIRST_PROMPT} You were relaunched in the same worktree, which keeps the earlier session's work. Note from the relaunch: {}",
        note.trim()
    )
}

/// What one launch needs.
#[derive(Clone, Debug)]
pub struct LaunchSpec<'a> {
    pub role: Role,
    pub session_id: &'a str,
    /// The run, or empty for the orchestrator, which has none.
    pub run_id: &'a str,
    pub worktree: &'a Path,
    pub scratch: &'a Path,
    pub model: &'a str,
    pub effort: Effort,
    /// A worker's brief; `None` for the orchestrator.
    pub brief: Option<&'a str>,
    /// The orchestrator's roots; `None` for a worker.
    pub roots: Option<Roots<'a>>,
    /// The packaged `tiller` binary, by its absolute path.
    pub tiller: &'a Path,
    pub default_branch: &'a str,
    pub network_allow: &'a [String],
    pub no_mistakes_socket: Option<&'a str>,
    /// The Claude Code command.
    pub claude: &'a str,
    /// The Claude Code version, for the manifest ([`claude_version`]).
    pub claude_version: Option<&'a str>,
    /// The memory revisions rendered into the prompt, as `(source id, blob
    /// sha)` pairs (`memory.md` section 7). Empty until the renderer passes
    /// them.
    pub memory: &'a [(String, String)],
    /// The relaunch note of a new attempt, which goes into its first prompt.
    pub note: Option<&'a str>,
}

/// A later session of a run, started from the files [`write`] left in the
/// run's scratch directory (`tiller agent relaunch` and `resume`).
#[derive(Clone, Debug)]
pub struct AgainSpec<'a> {
    pub role: Role,
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub worktree: &'a Path,
    pub scratch: &'a Path,
    pub model: &'a str,
    pub effort: Effort,
    pub claude: &'a str,
}

/// How that later session starts.
#[derive(Clone, Copy, Debug)]
pub enum Start<'a> {
    /// A new session on the same run, with the relaunch note.
    Fresh { note: &'a str },
    /// `claude --resume <claude session id>`.
    Resume { claude_session_id: &'a str },
}

/// The files written for a launch, and the line that starts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub prompt: PathBuf,
    pub settings: PathBuf,
    pub mcp: PathBuf,
    pub script: PathBuf,
    /// The SHA-256 of [`Launch::manifest`] in its canonical JSON form.
    pub config_hash: String,
    /// The parts behind the hash: `role`, `settings`, `mcp`, `model`,
    /// `effort`, `tiller`, `claude` and `memory`.
    pub manifest: Value,
    /// What spawn types into the pane, followed by Enter.
    pub line: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The manifest behind a run's `config_hash` (learning-loop.md, section 5).
pub fn manifest(
    role: Role,
    fragment: &[u8],
    model: &str,
    effort: Effort,
    claude_version: Option<&str>,
    memory: &[(String, String)],
) -> Value {
    let mut pairs: Vec<String> = memory
        .iter()
        .map(|(source, blob)| format!("{source} {blob}"))
        .collect();
    pairs.sort();
    pairs.dedup();
    json!({
        "role": sha256_hex(role.file().as_bytes()),
        "settings": sha256_hex(fragment),
        "mcp": sha256_hex(MCP_CONFIG.as_bytes()),
        "model": model,
        "effort": effort.as_str(),
        "tiller": env!("CARGO_PKG_VERSION"),
        "claude": claude_version.unwrap_or("unknown"),
        "memory": sha256_hex(pairs.join("\n").as_bytes()),
    })
}

/// The hash of a manifest: SHA-256 of its canonical JSON (sorted keys, no
/// spaces), which `serde_json` prints for a `Value` object.
pub fn hash_of(manifest: &Value) -> String {
    sha256_hex(manifest.to_string().as_bytes())
}

/// The Claude Code version `claude --version` prints, read once per
/// process and given two seconds; `None` when it does not answer. Spawns
/// run in a short-lived `tiller` process, so the cache never outlives an
/// upgrade. The variable `TILLER_CLAUDE_VERSION` overrides it, for tests and
/// for a host that pins the version elsewhere.
pub fn claude_version(command: &str) -> Option<String> {
    static VERSION: OnceLock<Option<String>> = OnceLock::new();
    VERSION
        .get_or_init(|| {
            if let Ok(pinned) = std::env::var("TILLER_CLAUDE_VERSION") {
                return (!pinned.trim().is_empty()).then(|| pinned.trim().to_owned());
            }
            read_version(command, Duration::from_secs(2))
        })
        .clone()
}

fn read_version(command: &str, timeout: Duration) -> Option<String> {
    let mut child = std::process::Command::new(command)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut text).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(64).collect())
}

/// Quotes one word for `sh`.
pub fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

fn absolute(name: &str, path: &Path) -> Result<String, SpawnError> {
    let text = path.to_string_lossy();
    if !path.is_absolute() || text.len() < 2 || text.ends_with('/') {
        return Err(SpawnError::Usage(format!(
            "{name} must be an absolute path without a trailing slash, not {text:?}"
        )));
    }
    Ok(text.into_owned())
}

/// Writes a file only the owner can read, refusing to follow or replace
/// anything already there: a run's scratch directory is new.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> Result<(), SpawnError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// What one launch script runs.
struct Command<'a> {
    role: Role,
    session_id: &'a str,
    run_id: &'a str,
    worktree: &'a Path,
    claude: &'a str,
    model: &'a str,
    effort: Effort,
    mode: &'a str,
    /// The words after the flags: the first prompt, or `--resume <id>`.
    start: Vec<String>,
}

/// The command the script runs, word by word.
fn argv(command: &Command<'_>, launch: &Launch) -> Vec<String> {
    let mut words: Vec<String> = [
        command.claude,
        "--append-system-prompt-file",
        &launch.prompt.to_string_lossy(),
        "--settings",
        &launch.settings.to_string_lossy(),
        "--permission-mode",
        command.mode,
        "--strict-mcp-config",
        "--mcp-config",
        &launch.mcp.to_string_lossy(),
        "--model",
        command.model,
        "--effort",
        command.effort.as_str(),
    ]
    .map(str::to_owned)
    .into();
    words.extend(command.start.iter().cloned());
    words
}

/// The environment every fleet launch sets (`authority.md` section 7).
fn env(command: &Command<'_>) -> Vec<(&'static str, String)> {
    vec![
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".into()),
        ("TILLER_SESSION_ID", command.session_id.into()),
        ("TILLER_RUN_ID", command.run_id.into()),
        ("TILLER_ROLE", command.role.name().into()),
    ]
}

/// The launch script's text.
fn script(command: &Command<'_>, launch: &Launch) -> String {
    let env: Vec<String> = env(command)
        .into_iter()
        .map(|(k, v)| format!("{k}={}", sh_quote(&v)))
        .collect();
    let words: Vec<String> = argv(command, launch).iter().map(|w| sh_quote(w)).collect();
    let what = match command.run_id {
        "" => format!("session {}", command.session_id),
        run => format!("run {run}"),
    };
    format!(
        "#!/bin/sh\n# tiller: {what}, {role}. Regenerated on every launch; edits are lost.\ncd {cwd} || exit 1\nexec env {env} \\\n  {argv}\n",
        role = command.role.name(),
        cwd = sh_quote(&command.worktree.to_string_lossy()),
        env = env.join(" "),
        argv = words.join(" "),
    )
}

/// Writes the prompt, the fragment, the MCP config and the launch script
/// into `spec.scratch`, which must not exist yet.
pub fn write(spec: &LaunchSpec<'_>) -> Result<Launch, SpawnError> {
    let worktree = absolute("the worktree", spec.worktree)?;
    let scratch = absolute("the scratch directory", spec.scratch)?;
    let tiller = absolute("the tiller binary", spec.tiller)?;
    if let Some(parent) = spec.scratch.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(spec.scratch)?;
    fs::set_permissions(spec.scratch, fs::Permissions::from_mode(0o700))?;

    let front = spec.role.frontmatter()?;
    let body = spec.role.prompt_body()?;
    let prompt = match spec.brief {
        Some(brief) => format!("{}\n\n# Your brief\n\n{}\n", body.trim_end(), brief.trim()),
        None => format!("{}\n", body.trim_end()),
    };
    let (home, pool, checkouts) = match spec.roots {
        Some(roots) => (
            absolute("the orchestrator home", roots.home)?,
            absolute("the pool root", roots.pool)?,
            absolute("the checkouts root", roots.checkouts)?,
        ),
        None => Default::default(),
    };
    let fragment = roles::render_settings(
        spec.role,
        &Placeholders {
            tiller,
            worktree,
            scratch,
            home,
            pool,
            checkouts,
            default_branch: spec.default_branch.into(),
            network_allow: spec.network_allow.to_vec(),
            no_mistakes_socket: spec.no_mistakes_socket.map(str::to_owned),
        },
    )?;
    let fragment = serde_json::to_vec_pretty(&fragment)?;
    let manifest = manifest(
        spec.role,
        &fragment,
        spec.model,
        spec.effort,
        spec.claude_version,
        spec.memory,
    );
    let config_hash = hash_of(&manifest);

    let dir = spec.scratch;
    let mut launch = Launch {
        prompt: dir.join("prompt.md"),
        settings: dir.join("settings.json"),
        mcp: dir.join("mcp.json"),
        script: dir.join("launch.sh"),
        config_hash,
        manifest,
        line: String::new(),
    };
    write_new(&launch.prompt, prompt.as_bytes(), 0o600)?;
    write_new(&launch.settings, &fragment, 0o600)?;
    write_new(&launch.mcp, MCP_CONFIG.as_bytes(), 0o600)?;

    // The orchestrator has no brief, so no first prompt.
    let start = match (spec.brief, spec.note) {
        (None, _) => Vec::new(),
        (Some(_), Some(note)) => vec![relaunch_prompt(note)],
        (Some(_), None) => vec![FIRST_PROMPT.to_owned()],
    };
    let text = script(
        &Command {
            role: spec.role,
            session_id: spec.session_id,
            run_id: spec.run_id,
            worktree: spec.worktree,
            claude: spec.claude,
            model: spec.model,
            effort: spec.effort,
            mode: &front.permission_mode,
            start,
        },
        &launch,
    );
    write_new(&launch.script, text.as_bytes(), 0o700)?;
    launch.line = format!("sh {}", sh_quote(&launch.script.to_string_lossy()));
    Ok(launch)
}

/// Writes the script of a later session into the run's scratch directory,
/// beside the prompt, fragment and MCP config [`write`] left there, which it
/// reuses unchanged: the run's brief and hash stay what they were. The
/// script is named for the session and how it starts
/// (`relaunch-<session>.sh`, `resume-<session>.sh`) and replaces only an
/// earlier script of that same name. Returns the line to type.
pub fn write_again(spec: &AgainSpec<'_>, start: Start<'_>) -> Result<String, SpawnError> {
    absolute("the worktree", spec.worktree)?;
    absolute("the scratch directory", spec.scratch)?;
    let launch = Launch {
        prompt: spec.scratch.join("prompt.md"),
        settings: spec.scratch.join("settings.json"),
        mcp: spec.scratch.join("mcp.json"),
        script: spec.scratch.join(match start {
            Start::Fresh { .. } => format!("relaunch-{}.sh", spec.session_id),
            Start::Resume { .. } => format!("resume-{}.sh", spec.session_id),
        }),
        config_hash: String::new(),
        manifest: Value::Null,
        line: String::new(),
    };
    for file in [&launch.prompt, &launch.settings, &launch.mcp] {
        if !file.is_file() {
            return Err(SpawnError::Refused(format!(
                "{} is missing, so the run's launch cannot be repeated",
                file.display()
            )));
        }
    }
    let front = spec.role.frontmatter()?;
    let start = match start {
        Start::Fresh { note } => vec![relaunch_prompt(note)],
        Start::Resume { claude_session_id } => {
            vec!["--resume".to_owned(), claude_session_id.to_owned()]
        }
    };
    let text = script(
        &Command {
            role: spec.role,
            session_id: spec.session_id,
            run_id: spec.run_id,
            worktree: spec.worktree,
            claude: spec.claude,
            model: spec.model,
            effort: spec.effort,
            mode: &front.permission_mode,
            start,
        },
        &launch,
    );
    // Written beside, then renamed over, so a script is never half there.
    let partial = launch.script.with_extension("sh.partial");
    match fs::remove_file(&partial) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    write_new(&partial, text.as_bytes(), 0o700)?;
    fs::rename(&partial, &launch.script)?;
    Ok(format!("sh {}", sh_quote(&launch.script.to_string_lossy())))
}
