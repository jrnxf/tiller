//! The curator (`docs/design/learning-loop.md`, section 4): the headless
//! session `tiller retro run` launches for one full retro. It is not a fleet
//! role, so it is not in [`crate::roles::Role`]: it has no run, no pane and
//! no hooks, and its settings fragment carries only the `<tiller>` and
//! `<scratch>` placeholders. The files are embedded like the roles', so a
//! launch reads nothing from disk.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::roles::MCP_CONFIG;
use crate::spawn::SpawnError;
use crate::spawn::launch::sh_quote;

const FILE: &str = include_str!("../../../roles/curator.md");
const SETTINGS: &str = include_str!("../../../roles/curator.settings.json");

/// The first prompt the curator gets; the brief is in its instructions.
pub const FIRST_PROMPT: &str =
    "Run the retro named at the end of your instructions, then finish it with tiller retro finish.";

/// The prompt body of `roles/curator.md`, without its frontmatter.
pub fn prompt_body() -> &'static str {
    FILE.strip_prefix("---\n")
        .and_then(|rest| {
            rest.find("\n---\n")
                .map(|end| &rest[end + "\n---\n".len()..])
        })
        .unwrap_or(FILE)
        .trim_start_matches('\n')
}

/// The `model:` of the frontmatter, the alias dispatch resolves.
pub fn model_alias() -> &'static str {
    FILE.lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix("model:"))
        .map_or("sonnet", str::trim)
}

/// The settings fragment with its two placeholders filled. Both must be
/// absolute paths without a trailing slash or a quote.
pub fn render_settings(tiller: &Path, scratch: &Path) -> Result<Value, SpawnError> {
    let mut text = SETTINGS.to_owned();
    for (placeholder, path) in [("<tiller>", tiller), ("<scratch>", scratch)] {
        let value = path.to_string_lossy();
        if !path.is_absolute() || value.ends_with('/') || value.contains(['\'', '"', '\\']) {
            return Err(SpawnError::Usage(format!(
                "{placeholder} for the curator must be an absolute path without a trailing slash or a quote, not {value:?}"
            )));
        }
        text = text.replace(placeholder, &value);
    }
    Ok(serde_json::from_str(&text)?)
}

/// What one curator launch needs.
pub struct LaunchSpec<'a> {
    pub retro_id: &'a str,
    pub scratch: &'a Path,
    pub tiller: &'a Path,
    pub claude: &'a str,
    pub model: &'a str,
    pub brief: &'a str,
}

/// The files written for a launch and the command to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub prompt: PathBuf,
    pub settings: PathBuf,
    pub mcp: PathBuf,
    /// The command, word by word: `claude -p ...`.
    pub argv: Vec<String>,
    /// The environment the command runs with.
    pub env: Vec<(String, String)>,
    /// The same, as one `sh` line, for the record.
    pub line: String,
}

fn write_new(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Writes the prompt, the fragment and the MCP config into `spec.scratch`,
/// which must not exist yet, and returns the launch.
pub fn write(spec: &LaunchSpec<'_>) -> Result<Launch, SpawnError> {
    if let Some(parent) = spec.scratch.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(spec.scratch)?;
    fs::set_permissions(spec.scratch, fs::Permissions::from_mode(0o700))?;
    let fragment = render_settings(spec.tiller, spec.scratch)?;
    let fragment = serde_json::to_vec_pretty(&fragment)?;
    let prompt = format!(
        "{}\n\n# Your brief\n\n{}\n",
        prompt_body().trim_end(),
        spec.brief.trim()
    );
    let launch_prompt = spec.scratch.join("prompt.md");
    let settings = spec.scratch.join("settings.json");
    let mcp = spec.scratch.join("mcp.json");
    write_new(&launch_prompt, prompt.as_bytes(), 0o600)?;
    write_new(&settings, &fragment, 0o600)?;
    write_new(&mcp, MCP_CONFIG.as_bytes(), 0o600)?;
    let argv: Vec<String> = [
        spec.claude,
        "-p",
        FIRST_PROMPT,
        "--append-system-prompt-file",
        &launch_prompt.to_string_lossy(),
        "--settings",
        &settings.to_string_lossy(),
        "--permission-mode",
        "dontAsk",
        "--strict-mcp-config",
        "--mcp-config",
        &mcp.to_string_lossy(),
        "--model",
        spec.model,
        "--output-format",
        "json",
    ]
    .map(str::to_owned)
    .into();
    let env: Vec<(String, String)> = vec![
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()),
        ("TILLER_ROLE".into(), "curator".into()),
        ("TILLER_RETRO_ID".into(), spec.retro_id.into()),
    ];
    let line = format!(
        "env {} {}",
        env.iter()
            .map(|(k, v)| format!("{k}={}", sh_quote(v)))
            .collect::<Vec<_>>()
            .join(" "),
        argv.iter()
            .map(|w| sh_quote(w))
            .collect::<Vec<_>>()
            .join(" ")
    );
    Ok(Launch {
        prompt: launch_prompt,
        settings,
        mcp,
        argv,
        env,
        line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curator_is_read_only_with_a_narrow_bash() {
        let fragment = render_settings(
            Path::new("/nix/store/x/bin/tiller"),
            Path::new("/home/user/s"),
        )
        .unwrap();
        let deny: Vec<&str> = fragment["permissions"]["deny"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let allow: Vec<&str> = fragment["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for tool in ["Edit", "Write", "NotebookEdit", "Agent", "WebFetch"] {
            assert!(deny.contains(&tool), "{tool} must be denied");
        }
        assert!(!allow.contains(&"Bash"), "no bare Bash allow");
        assert!(allow.contains(&"Bash(tiller retro *)"));
        assert!(allow.contains(&"Bash(tiller mem propose *)"));
        assert!(
            !allow
                .iter()
                .any(|rule| rule.starts_with("Bash(tiller mem *"))
        );
        assert!(deny.contains(&"Bash(tiller learn *)"));
        // The curator reads untrusted transcripts, so every dot directory
        // in the home (keys, tokens, the state directory) is closed to Read.
        for path in ["~/.tiller/**", "~/.claude/**", "**/.ssh/**", "**/id_*"] {
            assert!(deny.contains(&format!("Read({path})").as_str()), "{path}");
        }
        assert_eq!(fragment["env"]["TILLER_ROLE"], "curator");
        assert_eq!(fragment["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1");
        assert_eq!(fragment["autoMemoryEnabled"], false);
        assert_eq!(
            fragment["sandbox"]["filesystem"]["allowWrite"],
            serde_json::json!(["/home/user/s"])
        );
        assert_eq!(
            fragment["sandbox"]["excludedCommands"][0],
            "/nix/store/x/bin/tiller"
        );
        assert!(fragment.get("hooks").is_none(), "the curator has no hooks");
        assert!(
            fragment.to_string().find('<').is_none(),
            "no placeholder left"
        );
        assert_eq!(model_alias(), "sonnet");
        assert!(prompt_body().starts_with("# Curator"));
        assert!(prompt_body().contains("Nothing worth saving"));
    }

    #[test]
    fn a_launch_writes_its_files_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("retro");
        let launch = write(&LaunchSpec {
            retro_id: "01JC00000000000000000000R1",
            scratch: &scratch,
            tiller: Path::new("/nix/store/x/bin/tiller"),
            claude: "claude",
            model: "claude-sonnet-5-5",
            brief: "retro: 01JC00000000000000000000R1",
        })
        .unwrap();
        let prompt = fs::read_to_string(&launch.prompt).unwrap();
        assert!(prompt.starts_with("# Curator"));
        assert!(prompt.ends_with("# Your brief\n\nretro: 01JC00000000000000000000R1\n"));
        assert_eq!(launch.argv[0..3], ["claude", "-p", FIRST_PROMPT]);
        assert!(launch.argv.contains(&"--permission-mode".to_owned()));
        assert!(launch.argv.contains(&"dontAsk".to_owned()));
        assert!(launch.env.contains(&(
            "TILLER_RETRO_ID".into(),
            "01JC00000000000000000000R1".into()
        )));
        assert!(launch.line.contains("TILLER_ROLE='curator'"));
        assert_eq!(
            fs::metadata(&scratch).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(
            write(&LaunchSpec {
                retro_id: "x",
                scratch: &scratch,
                tiller: Path::new("/nix/store/x/bin/tiller"),
                claude: "claude",
                model: "m",
                brief: "b",
            })
            .is_err()
        );
    }
}
