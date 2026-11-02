//! The launch form (`authority.md` section 7, layer 1): the files a run's
//! session starts from, written into its scratch directory, and the one
//! line spawn types into the new pane.
//!
//! The role body and the brief go into one file passed with
//! `--append-system-prompt-file`, so the brief survives compaction and a
//! resume passes it again with the same flags. The settings fragment is the
//! role's template rendered for this run (T4.1) and its SHA-256 is the run's
//! `config_hash`. The MCP config is the packaged one (G35).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tiller_core::domain::Effort;

use super::SpawnError;
use crate::roles::{self, MCP_CONFIG, Placeholders, Role};

/// The first prompt a worker gets; the brief itself is in its instructions.
pub const FIRST_PROMPT: &str = "Start on the brief at the end of your instructions.";

/// What one launch needs.
#[derive(Clone, Debug)]
pub struct LaunchSpec<'a> {
    pub role: Role,
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub worktree: &'a Path,
    pub scratch: &'a Path,
    pub model: &'a str,
    pub effort: Effort,
    pub brief: &'a str,
    /// The packaged `tiller` binary, by its absolute path.
    pub tiller: &'a Path,
    pub default_branch: &'a str,
    pub network_allow: &'a [String],
    pub no_mistakes_socket: Option<&'a str>,
    /// The Claude Code command.
    pub claude: &'a str,
}

/// The files written for a launch, and the line that starts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub prompt: PathBuf,
    pub settings: PathBuf,
    pub mcp: PathBuf,
    pub script: PathBuf,
    /// The SHA-256 of the settings fragment as written.
    pub config_hash: String,
    /// What spawn types into the pane, followed by Enter.
    pub line: String,
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

/// The command the script runs, word by word.
fn argv(spec: &LaunchSpec<'_>, launch: &Launch, mode: &str) -> Vec<String> {
    [
        spec.claude,
        "--append-system-prompt-file",
        &launch.prompt.to_string_lossy(),
        "--settings",
        &launch.settings.to_string_lossy(),
        "--permission-mode",
        mode,
        "--strict-mcp-config",
        "--mcp-config",
        &launch.mcp.to_string_lossy(),
        "--model",
        spec.model,
        "--effort",
        spec.effort.as_str(),
        FIRST_PROMPT,
    ]
    .map(str::to_owned)
    .into()
}

/// The environment every fleet launch sets (`authority.md` section 7).
fn env(spec: &LaunchSpec<'_>) -> Vec<(&'static str, String)> {
    vec![
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".into()),
        ("TILLER_SESSION_ID", spec.session_id.into()),
        ("TILLER_RUN_ID", spec.run_id.into()),
        ("TILLER_ROLE", spec.role.name().into()),
    ]
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
    let prompt = format!(
        "{}\n\n# Your brief\n\n{}\n",
        body.trim_end(),
        spec.brief.trim()
    );
    let fragment = roles::render_settings(
        spec.role,
        &Placeholders {
            tiller,
            worktree,
            scratch,
            default_branch: spec.default_branch.into(),
            network_allow: spec.network_allow.to_vec(),
            no_mistakes_socket: spec.no_mistakes_socket.map(str::to_owned),
            ..Placeholders::default()
        },
    )?;
    let fragment = serde_json::to_vec_pretty(&fragment)?;
    let digest = Sha256::digest(&fragment);
    let config_hash = digest.iter().map(|b| format!("{b:02x}")).collect();

    let dir = spec.scratch;
    let mut launch = Launch {
        prompt: dir.join("prompt.md"),
        settings: dir.join("settings.json"),
        mcp: dir.join("mcp.json"),
        script: dir.join("launch.sh"),
        config_hash,
        line: String::new(),
    };
    write_new(&launch.prompt, prompt.as_bytes(), 0o600)?;
    write_new(&launch.settings, &fragment, 0o600)?;
    write_new(&launch.mcp, MCP_CONFIG.as_bytes(), 0o600)?;

    let env: Vec<String> = env(spec)
        .into_iter()
        .map(|(k, v)| format!("{k}={}", sh_quote(&v)))
        .collect();
    let words: Vec<String> = argv(spec, &launch, &front.permission_mode)
        .iter()
        .map(|w| sh_quote(w))
        .collect();
    let script = format!(
        "#!/bin/sh\n# tiller spawn: run {run}, {role}. Regenerated on every launch; edits are lost.\ncd {cwd} || exit 1\nexec env {env} \\\n  {argv}\n",
        run = spec.run_id,
        role = spec.role.name(),
        cwd = sh_quote(&spec.worktree.to_string_lossy()),
        env = env.join(" "),
        argv = words.join(" "),
    );
    write_new(&launch.script, script.as_bytes(), 0o700)?;
    launch.line = format!("sh {}", sh_quote(&launch.script.to_string_lossy()));
    Ok(launch)
}
