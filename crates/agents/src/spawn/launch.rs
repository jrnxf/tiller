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
//! one (G35). Once the files are written, the launch directory's Claude
//! Code workspace trust is recorded ([`super::trust`]), so the session
//! reaches its composer instead of the trust dialog.

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
use super::trust::{self, ClaudeConfig};
use crate::roles::{self, MCP_CONFIG, Placeholders, Role};

/// What ends the role body in a prompt file and starts the brief.
const BRIEF_HEADING: &str = "# Your brief\n\n";

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
    /// The effective Tiller home, hidden from the sandbox and exported as
    /// `TILLER_HOME`.
    pub tiller_home: &'a Path,
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
    /// The hostnames the developer opens review links by, exported as
    /// `LAVISH_AXI_ALLOWED_HOSTS` (P49).
    pub review_hosts: &'a [String],
    pub no_mistakes_socket: Option<&'a str>,
    /// The worker may read the owner's GitHub login: a builder on a
    /// `no-mistakes` project ([`Placeholders::gh_login_readable`]).
    pub gh_login_readable: bool,
    /// The Claude Code command.
    pub claude: &'a str,
    /// Where Claude Code keeps the user config the trust is recorded in.
    pub claude_config: &'a ClaudeConfig,
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
    /// The effective Tiller home, exported as `TILLER_HOME`.
    pub tiller_home: &'a Path,
    pub model: &'a str,
    pub effort: Effort,
    pub claude: &'a str,
    pub claude_config: &'a ClaudeConfig,
    /// The hostnames the developer opens review links by (P49).
    pub review_hosts: &'a [String],
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
    /// Set when the role's declared permission mode is not the one launched
    /// (auto mode is unavailable); spawn shows it in its output.
    pub mode_note: Option<String>,
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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

/// Whether this Claude Code offers auto mode: its `--help` lists `auto` among
/// the `--permission-mode` choices. Given two seconds. `TILLER_CLAUDE_AUTO_MODE`
/// overrides it (`0`, `false`, `no`, `off` or empty for unavailable). When the
/// answer cannot be read the mode is assumed available: Claude Code itself then refuses a mode it
/// lacks, visibly, rather than tiller guessing a downgrade.
fn claude_offers_auto(command: &str) -> bool {
    if let Ok(pinned) = std::env::var("TILLER_CLAUDE_AUTO_MODE") {
        let pinned = pinned.trim().to_ascii_lowercase();
        return !["", "0", "false", "no", "off"].contains(&pinned.as_str());
    }
    read_help(command, Duration::from_secs(2)).is_none_or(|help| help_lists_auto(&help))
}

fn help_lists_auto(help: &str) -> bool {
    // A help without the option at all is a layout this does not know.
    if !help.contains("--permission-mode") {
        return true;
    }
    help.lines()
        .skip_while(|line| !line.contains("--permission-mode"))
        .enumerate()
        .take_while(|(i, line)| *i == 0 || !line.trim_start().starts_with('-'))
        .map(|(_, line)| line)
        .any(|line| line.contains("\"auto\""))
}

fn read_help(command: &str, timeout: Duration) -> Option<String> {
    let mut child = std::process::Command::new(command)
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut text).ok()?;
        Some(text)
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
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
    reader.join().ok()?
}

/// The permission mode a launch passes: the role's declared mode, except that
/// the orchestrator's `auto` falls back to `default` when this Claude Code
/// does not offer it (and says so). Never `bypassPermissions` or `dontAsk`.
fn effective_mode(claude: &str, declared: &str) -> (String, Option<String>) {
    if declared == "auto" && !claude_offers_auto(claude) {
        return (
            "default".into(),
            Some(
                "auto mode is not available in this Claude Code; the orchestrator launched in default mode, so routine actions will prompt"
                    .into(),
            ),
        );
    }
    (declared.to_owned(), None)
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
    tiller_home: &'a Path,
    claude: &'a str,
    claude_config: &'a ClaudeConfig,
    model: &'a str,
    effort: Effort,
    mode: &'a str,
    review_hosts: &'a [String],
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

/// The environment every fleet launch sets (`authority.md` section 7),
/// and tiller's `CLAUDE_CONFIG_DIR` when it has one, so the session reads
/// the config its trust was recorded in. Without one, the script unsets any
/// the pane inherited, for the same reason.
fn env(command: &Command<'_>) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".into()),
        ("TILLER_SESSION_ID", command.session_id.into()),
        ("TILLER_RUN_ID", command.run_id.into()),
        ("TILLER_ROLE", command.role.name().into()),
        // The session's `tiller` and its guard hook find the Tiller home by
        // this, and a herdr pane does not inherit the daemon's environment.
        (
            "TILLER_HOME",
            command.tiller_home.to_string_lossy().into_owned(),
        ),
    ];
    if let Some(dir) = command.claude_config.forwarded() {
        env.push(("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned()));
    }
    // The review server a session starts is shared and keeps the environment
    // of whoever started it first, so every launch carries the names the
    // developer reaches it by (P49).
    if !command.review_hosts.is_empty() {
        env.push((REVIEW_HOSTS_VAR, command.review_hosts.join(" ")));
    }
    env
}

/// The variable the review server reads its allowed hostnames from. It
/// splits on whitespace.
const REVIEW_HOSTS_VAR: &str = "LAVISH_AXI_ALLOWED_HOSTS";

/// Checks a launch script before anything runs it (`recovery.md` R17): the
/// environment a session needs is in the text, the MCP config is explicit and
/// on disk, and the role file and worktree are there. A failed check refuses
/// the launch and names itself.
fn preflight(text: &str, command: &Command<'_>, launch: &Launch) -> Result<(), SpawnError> {
    let fail = |check: String| Err(SpawnError::Refused(format!("launch preflight: {check}")));
    let mut wanted = vec![("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".to_owned())];
    if !command.review_hosts.is_empty() {
        wanted.push((REVIEW_HOSTS_VAR, command.review_hosts.join(" ")));
    }
    for (name, value) in wanted {
        if !text.contains(&format!("{name}={}", sh_quote(&value))) {
            return fail(format!("the launch environment does not set {name}"));
        }
    }
    if !text.contains("--strict-mcp-config") || !text.contains("--mcp-config") {
        return fail("the MCP config is not explicit".into());
    }
    for (what, file) in [("role file", &launch.prompt), ("MCP config", &launch.mcp)] {
        if !file.is_file() {
            return fail(format!("the {what} {} is missing", file.display()));
        }
    }
    if !command.worktree.is_dir() {
        return fail(format!(
            "the worktree {} is missing",
            command.worktree.display()
        ));
    }
    Ok(())
}

/// The launch script's text.
fn script(command: &Command<'_>, launch: &Launch) -> String {
    let mut env: Vec<String> = env(command)
        .into_iter()
        .map(|(k, v)| format!("{k}={}", sh_quote(&v)))
        .collect();
    if command.claude_config.forwarded().is_none() {
        env.insert(0, "-u CLAUDE_CONFIG_DIR".into());
    }
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

/// The launch files' text, before any of it is written.
struct Rendered {
    prompt: String,
    fragment: Vec<u8>,
    manifest: Value,
    config_hash: String,
    mode: String,
    mode_note: Option<String>,
}

/// Renders the prompt, the fragment and the manifest for `spec`.
fn render(spec: &LaunchSpec<'_>) -> Result<Rendered, SpawnError> {
    let worktree = absolute("the worktree", spec.worktree)?;
    let scratch = absolute("the scratch directory", spec.scratch)?;
    let tiller_home = absolute("the Tiller home", spec.tiller_home)?;
    let tiller = absolute("the tiller binary", spec.tiller)?;

    let front = spec.role.frontmatter()?;
    let body = spec.role.prompt_body()?;
    let prompt = match spec.brief {
        Some(brief) => format!("{}\n\n{BRIEF_HEADING}{}\n", body.trim_end(), brief.trim()),
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
    let mut fragment = roles::render_settings(
        spec.role,
        &Placeholders {
            tiller,
            tiller_home,
            worktree,
            scratch,
            home,
            pool,
            checkouts,
            default_branch: spec.default_branch.into(),
            network_allow: spec.network_allow.to_vec(),
            no_mistakes_socket: spec.no_mistakes_socket.map(str::to_owned),
            gh_login_readable: spec.gh_login_readable,
        },
    )?;
    let (mode, mode_note) = effective_mode(spec.claude, &front.permission_mode);
    if mode != front.permission_mode {
        fragment["permissions"]["defaultMode"] = Value::String(mode.clone());
    }
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
    Ok(Rendered {
        prompt,
        fragment,
        manifest,
        config_hash,
        mode,
        mode_note,
    })
}

/// The first prompt words of a launch.
fn first_prompt(spec: &LaunchSpec<'_>) -> Vec<String> {
    // The orchestrator has no brief, so no first prompt.
    match (spec.brief, spec.note) {
        (None, _) => Vec::new(),
        (Some(_), Some(note)) => vec![relaunch_prompt(note)],
        (Some(_), None) => vec![FIRST_PROMPT.to_owned()],
    }
}

/// Writes the prompt, the fragment, the MCP config and the launch script
/// into `spec.scratch`, which must not exist yet.
pub fn write(spec: &LaunchSpec<'_>) -> Result<Launch, SpawnError> {
    let rendered = render(spec)?;
    if let Some(parent) = spec.scratch.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(spec.scratch)?;
    fs::set_permissions(spec.scratch, fs::Permissions::from_mode(0o700))?;

    let dir = spec.scratch;
    let mut launch = Launch {
        prompt: dir.join("prompt.md"),
        settings: dir.join("settings.json"),
        mcp: dir.join("mcp.json"),
        script: dir.join("launch.sh"),
        config_hash: rendered.config_hash,
        manifest: rendered.manifest,
        line: String::new(),
        mode_note: rendered.mode_note,
    };
    write_new(&launch.prompt, rendered.prompt.as_bytes(), 0o600)?;
    write_new(&launch.settings, &rendered.fragment, 0o600)?;
    write_new(&launch.mcp, MCP_CONFIG.as_bytes(), 0o600)?;

    let command = Command {
        role: spec.role,
        session_id: spec.session_id,
        run_id: spec.run_id,
        worktree: spec.worktree,
        tiller_home: spec.tiller_home,
        claude: spec.claude,
        claude_config: spec.claude_config,
        model: spec.model,
        effort: spec.effort,
        mode: &rendered.mode,
        review_hosts: spec.review_hosts,
        start: first_prompt(spec),
    };
    let text = script(&command, &launch);
    preflight(&text, &command, &launch)?;
    write_new(&launch.script, text.as_bytes(), 0o700)?;
    // Last, so a launch refused for any other reason records nothing.
    trust::record(spec.claude_config, spec.worktree)?;
    launch.line = format!("sh {}", sh_quote(&launch.script.to_string_lossy()));
    Ok(launch)
}

/// The brief inside a prompt file [`write`] made, or `None` when the file
/// has none (an orchestrator's).
pub fn brief_of(prompt: &str) -> Option<&str> {
    let (_, brief) = prompt.split_once(BRIEF_HEADING)?;
    Some(brief.trim())
}

/// Replaces `path` with `bytes`, never leaving it half written.
fn replace(path: &Path, bytes: &[u8], mode: u32) -> Result<(), SpawnError> {
    let partial = path.with_extension("partial");
    match fs::remove_file(&partial) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    write_new(&partial, bytes, mode)?;
    fs::rename(&partial, path)?;
    Ok(())
}

/// A same-run relaunch: renders the prompt, the fragment and the MCP config
/// again from the current binary and config, over the run's own files in
/// `spec.scratch`, which must exist. `spec.brief` is the run's brief, read
/// back with [`brief_of`], so the session keeps its task but not the hook
/// and command paths, the role prompt or the settings of its first launch.
/// The script is `relaunch-<session>.sh`, as [`write_again`] names it.
pub fn write_refreshed(spec: &LaunchSpec<'_>) -> Result<Launch, SpawnError> {
    if !spec.scratch.is_dir() {
        return Err(SpawnError::Refused(format!(
            "{} is missing, so the run's launch cannot be repeated",
            spec.scratch.display()
        )));
    }
    let rendered = render(spec)?;
    let dir = spec.scratch;
    let mut launch = Launch {
        prompt: dir.join("prompt.md"),
        settings: dir.join("settings.json"),
        mcp: dir.join("mcp.json"),
        script: dir.join(format!("relaunch-{}.sh", spec.session_id)),
        config_hash: rendered.config_hash,
        manifest: rendered.manifest,
        line: String::new(),
        mode_note: rendered.mode_note,
    };
    replace(&launch.prompt, rendered.prompt.as_bytes(), 0o600)?;
    replace(&launch.settings, &rendered.fragment, 0o600)?;
    replace(&launch.mcp, MCP_CONFIG.as_bytes(), 0o600)?;
    let command = Command {
        role: spec.role,
        session_id: spec.session_id,
        run_id: spec.run_id,
        worktree: spec.worktree,
        tiller_home: spec.tiller_home,
        claude: spec.claude,
        claude_config: spec.claude_config,
        model: spec.model,
        effort: spec.effort,
        mode: &rendered.mode,
        review_hosts: spec.review_hosts,
        start: first_prompt(spec),
    };
    let text = script(&command, &launch);
    preflight(&text, &command, &launch)?;
    replace(&launch.script, text.as_bytes(), 0o700)?;
    trust::record(spec.claude_config, spec.worktree)?;
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
        mode_note: None,
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
    let (mode, _) = effective_mode(spec.claude, &front.permission_mode);
    // The fragment must name the mode the flag does.
    let mut fragment: Value = serde_json::from_slice(&fs::read(&launch.settings)?)?;
    if fragment["permissions"]["defaultMode"] != mode.as_str() {
        fragment["permissions"]["defaultMode"] = Value::String(mode.clone());
        fs::write(&launch.settings, serde_json::to_vec_pretty(&fragment)?)?;
    }
    let start = match start {
        Start::Fresh { note } => vec![relaunch_prompt(note)],
        Start::Resume { claude_session_id } => {
            vec!["--resume".to_owned(), claude_session_id.to_owned()]
        }
    };
    let command = Command {
        role: spec.role,
        session_id: spec.session_id,
        run_id: spec.run_id,
        worktree: spec.worktree,
        tiller_home: spec.tiller_home,
        claude: spec.claude,
        claude_config: spec.claude_config,
        model: spec.model,
        effort: spec.effort,
        mode: &mode,
        review_hosts: spec.review_hosts,
        start,
    };
    let text = script(&command, &launch);
    preflight(&text, &command, &launch)?;
    // Written beside, then renamed over, so a script is never half there.
    let partial = launch.script.with_extension("sh.partial");
    match fs::remove_file(&partial) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    write_new(&partial, text.as_bytes(), 0o700)?;
    fs::rename(&partial, &launch.script)?;
    trust::record(spec.claude_config, spec.worktree)?;
    Ok(format!("sh {}", sh_quote(&launch.script.to_string_lossy())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP_WITH_AUTO: &str = "  --permission-mode <mode>   Permission mode\n                             (choices: \"acceptEdits\", \"auto\",\n                             \"dontAsk\", \"plan\")\n";
    const HELP_WITHOUT_AUTO: &str = "  --permission-mode <mode>   Permission mode\n                             (choices: \"acceptEdits\", \"default\",\n                             \"dontAsk\", \"plan\")\n  --other   an \"auto\" thing\n";

    #[test]
    fn help_parser_reads_the_permission_mode_choices() {
        assert!(help_lists_auto(HELP_WITH_AUTO));
        assert!(!help_lists_auto(HELP_WITHOUT_AUTO));
    }

    #[test]
    fn auto_falls_back_to_default_when_claude_lacks_it() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        fs::write(
            &claude,
            format!("#!/bin/sh\ncat <<'EOF'\n{HELP_WITHOUT_AUTO}EOF\n"),
        )
        .unwrap();
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
        let (mode, note) = effective_mode(&claude.to_string_lossy(), "auto");
        assert_eq!(mode, "default");
        assert!(note.unwrap().contains("not available"));
    }

    #[test]
    fn workers_keep_their_declared_mode() {
        let (mode, note) = effective_mode("claude", "dontAsk");
        assert_eq!(mode, "dontAsk");
        assert!(note.is_none());
    }

    fn text(claude_config: &ClaudeConfig) -> String {
        let launch = Launch {
            prompt: "/home/user/run/prompt.md".into(),
            settings: "/home/user/run/settings.json".into(),
            mcp: "/home/user/run/mcp.json".into(),
            script: "/home/user/run/launch.sh".into(),
            config_hash: String::new(),
            manifest: Value::Null,
            line: String::new(),
            mode_note: None,
        };
        script(
            &Command {
                role: Role::Builder,
                session_id: "s1",
                run_id: "r1",
                worktree: Path::new("/home/user/work"),
                tiller_home: Path::new("/home/user/.tiller"),
                claude: "claude",
                claude_config,
                model: "sonnet",
                effort: Effort::Medium,
                mode: "dontAsk",
                review_hosts: &[],
                start: Vec::new(),
            },
            &launch,
        )
    }

    #[test]
    fn the_session_finds_the_tiller_home_by_its_environment() {
        let set = text(&ClaudeConfig::default());
        assert!(set.contains("TILLER_HOME='/home/user/.tiller'"), "{set}");
    }

    #[test]
    fn the_session_reads_the_config_its_trust_was_recorded_in() {
        let set = text(&ClaudeConfig::at("/home/user/claude"));
        assert!(
            set.contains("exec env CLAUDE_CODE_DISABLE_AUTO_MEMORY='1'"),
            "{set}"
        );
        assert!(
            set.contains("CLAUDE_CONFIG_DIR='/home/user/claude'"),
            "{set}"
        );
        // Without tiller's own, one the pane inherited is unset.
        let unset = text(&ClaudeConfig::default());
        assert!(
            unset.contains("exec env -u CLAUDE_CONFIG_DIR CLAUDE_CODE"),
            "{unset}"
        );
        assert!(!unset.contains("CLAUDE_CONFIG_DIR="), "{unset}");
    }

    /// A launch whose files and worktree are all there, in `dir`.
    fn staged(dir: &Path) -> Launch {
        fs::write(dir.join("prompt.md"), "role\n").unwrap();
        fs::write(dir.join("mcp.json"), MCP_CONFIG).unwrap();
        Launch {
            prompt: dir.join("prompt.md"),
            settings: dir.join("settings.json"),
            mcp: dir.join("mcp.json"),
            script: dir.join("launch.sh"),
            config_hash: String::new(),
            manifest: Value::Null,
            line: String::new(),
            mode_note: None,
        }
    }

    fn command<'a>(
        dir: &'a Path,
        claude_config: &'a ClaudeConfig,
        hosts: &'a [String],
    ) -> Command<'a> {
        Command {
            role: Role::Builder,
            session_id: "s1",
            run_id: "r1",
            worktree: dir,
            tiller_home: Path::new("/home/user/.tiller"),
            claude: "claude",
            claude_config,
            model: "sonnet",
            effort: Effort::Medium,
            mode: "dontAsk",
            review_hosts: hosts,
            start: Vec::new(),
        }
    }

    #[test]
    fn preflight_refuses_launch_with_missing_env() {
        let dir = tempfile::tempdir().unwrap();
        let launch = staged(dir.path());
        let config = ClaudeConfig::default();
        let hosts = vec!["board.example.com".to_owned()];
        let command = command(dir.path(), &config, &hosts);
        let good = script(&command, &launch);
        preflight(&good, &command, &launch).unwrap();

        // A script that lost the allowed hosts, or the auto-memory switch.
        for name in [
            "LAVISH_AXI_ALLOWED_HOSTS",
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY",
        ] {
            let broken = good.replace(name, "OTHER");
            let error = preflight(&broken, &command, &launch).unwrap_err();
            assert!(error.to_string().contains(name), "{error}");
        }
        // Hosts that differ from the configured ones are not the same launch.
        let wrong = good.replace("board.example.com", "other.example.com");
        assert!(preflight(&wrong, &command, &launch).is_err());
        // No hosts configured means none are required.
        let none = Vec::new();
        let plain = self::command(dir.path(), &config, &none);
        preflight(&script(&plain, &launch), &plain, &launch).unwrap();
    }

    #[test]
    fn preflight_refuses_a_launch_missing_its_files_or_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let launch = staged(dir.path());
        let config = ClaudeConfig::default();
        let command = command(dir.path(), &config, &[]);
        let text = script(&command, &launch);
        // The MCP config must be explicit on the command line.
        let loose = text.replace("--strict-mcp-config", "");
        assert!(
            preflight(&loose, &command, &launch)
                .unwrap_err()
                .to_string()
                .contains("MCP")
        );
        fs::remove_file(&launch.prompt).unwrap();
        let error = preflight(&text, &command, &launch).unwrap_err();
        assert!(error.to_string().contains("role file"), "{error}");
        let gone = command_in(&config);
        let error = preflight(&text, &gone, &staged(dir.path())).unwrap_err();
        assert!(error.to_string().contains("worktree"), "{error}");
    }

    fn command_in(claude_config: &ClaudeConfig) -> Command<'_> {
        command(Path::new("/home/user/missing-worktree"), claude_config, &[])
    }
}
