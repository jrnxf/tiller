//! Tool versions and the `no-mistakes` daemon against its CLI (P43, R01).
//!
//! The daemon's host check and `tiller doctor` read versions the same way:
//! `<tool> --version`, the first line of stdout, within [`PROBE_TIMEOUT`]. A
//! tool that cannot be run reads as missing (`null` in Health), never as an
//! error.
//!
//! The `no-mistakes` CLI does not report its daemon's version. The daemon
//! writes its pid to `<no-mistakes home>/daemon.pid`; the process list gives
//! the binary that pid runs, and that binary's `--version` is the daemon's.
//! After an upgrade the CLI on `PATH` is new while the running daemon still
//! executes the old store path, which is the skew this reads.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A tool that answers `--version` within this long is alive.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the `no-mistakes` daemon keeps its pid file: this variable, else
/// `~/.no-mistakes`.
pub const NO_MISTAKES_HOME: &str = "TILLER_NO_MISTAKES_HOME";

/// Runs `first rest... arg...`; `Ok(Some(line))` is the first non-empty
/// stdout line, `Ok(None)` a clean exit that printed nothing.
pub fn run_first_line(
    first: &OsStr,
    rest: &[OsString],
    arg: &str,
) -> Result<Option<String>, String> {
    let mut child = Command::new(first)
        .args(rest)
        .args(arg.split_whitespace())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot run {}: {error}", first.to_string_lossy()))?;
    let mut stdout = child.stdout.take();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_string(&mut text);
        }
        let _ = tx.send(text);
    });
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("did not answer within {PROBE_TIMEOUT:?}"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(error.to_string()),
        }
    };
    if !status.success() {
        return Err(format!("exited with {status}"));
    }
    let text = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "kept its output open".to_owned())?;
    Ok(text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned))
}

/// `program --version`: the first line of stdout.
pub fn read_version(program: &[OsString]) -> Result<String, String> {
    let (first, rest) = program.split_first().ok_or("no command is configured")?;
    run_first_line(first, rest, "--version").and_then(|line| line.ok_or("printed nothing".into()))
}

/// The numeric parts of the first dotted version in `text`, so `v1.84.0` in
/// `no-mistakes version v1.84.0 (4822244) 2026-09-26` reads as `[1, 84, 0]`.
pub fn version_parts(text: &str) -> Vec<u64> {
    text.split_whitespace()
        .map(|word| {
            word.trim_start_matches('v')
                .split('.')
                .map_while(|part| part.parse().ok())
                .collect::<Vec<u64>>()
        })
        .find(|parts| parts.len() > 1)
        .unwrap_or_default()
}

/// The `no-mistakes` daemon's version from its pid file, or `None` when no
/// daemon is running or its binary cannot be asked. `process_args` is the
/// command line of a pid, as `ps -o args= -p <pid>` prints it.
pub fn daemon_binary(
    pid_file: &str,
    process_args: impl Fn(u32) -> Option<String>,
) -> Option<PathBuf> {
    let pid = serde_json::from_str::<Value>(pid_file)
        .ok()?
        .get("pid")?
        .as_u64()?;
    let args = process_args(u32::try_from(pid).ok()?)?;
    // `<binary> daemon run --root <home>`: the binary path may hold spaces.
    let (binary, _) = args.split_once(" daemon run")?;
    let binary = binary.trim();
    (!binary.is_empty()).then(|| PathBuf::from(binary))
}

/// Where the `no-mistakes` daemon keeps its pid file: [`NO_MISTAKES_HOME`],
/// else `~/.no-mistakes`.
pub fn no_mistakes_home() -> Option<PathBuf> {
    std::env::var_os(NO_MISTAKES_HOME)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".no-mistakes")))
}

/// The version line of the running `no-mistakes` daemon's own binary, from
/// the pid file in `home`.
pub fn no_mistakes_daemon_version(
    home: &Path,
    process_args: impl Fn(u32) -> Option<String>,
) -> Option<String> {
    let pid_file = std::fs::read_to_string(home.join("daemon.pid")).ok()?;
    let binary = daemon_binary(&pid_file, process_args)?;
    read_version(&[binary.into_os_string()]).ok()
}

/// [`no_mistakes_daemon_version`] for this host's daemon.
pub fn running_daemon_version() -> Option<String> {
    no_mistakes_daemon_version(&no_mistakes_home()?, ps_args)
}

/// One line when the `no-mistakes` daemon runs a different version than the
/// CLI that would talk to it, else `None`. A version that is not known on
/// either side is not a skew: the host check never degrades on a failed read.
pub fn skew(cli: Option<&str>, daemon: Option<&str>) -> Option<String> {
    let (cli, daemon) = (cli?, daemon?);
    let (cli_parts, daemon_parts) = (version_parts(cli), version_parts(daemon));
    if cli_parts.is_empty() || daemon_parts.is_empty() || cli_parts == daemon_parts {
        return None;
    }
    let shown = |line: &str| {
        line.split_whitespace()
            .find(|word| !version_parts(word).is_empty())
            .unwrap_or(line)
            .to_owned()
    };
    let (cli, daemon_shown) = (shown(cli), shown(daemon));
    let relation = if daemon_parts < cli_parts {
        "older than"
    } else {
        "newer than"
    };
    Some(format!(
        "the no-mistakes daemon is {daemon_shown}, {relation} its CLI {cli}; \
         restart it with: no-mistakes daemon restart"
    ))
}

/// What one read of the host's tools found.
#[derive(Clone, Debug, PartialEq)]
pub struct Versions {
    /// By tool name; `null` for a tool that could not be run.
    pub tools: Value,
    /// See [`skew`].
    pub skew: Option<String>,
}

/// The programs to ask, each with any leading arguments, and where the
/// `no-mistakes` daemon keeps its pid file.
pub struct Programs {
    pub quota_axi: Vec<OsString>,
    pub gh_axi: Vec<OsString>,
    pub git: Vec<OsString>,
    pub no_mistakes: Vec<OsString>,
    /// `None` reads no daemon version.
    pub no_mistakes_home: Option<PathBuf>,
}

impl Programs {
    /// The bare program names and this host's `no-mistakes` home.
    pub fn standard(quota_axi: Vec<OsString>) -> Self {
        Self {
            quota_axi,
            gh_axi: vec!["gh-axi".into()],
            git: vec!["git".into()],
            no_mistakes: vec!["no-mistakes".into()],
            no_mistakes_home: no_mistakes_home(),
        }
    }
}

/// Reads the tools Health lists (P43) that answer `--version`: `gh-axi`,
/// `quota-axi`, git, the `no-mistakes` CLI and its daemon. herdr reports its
/// version in its snapshot, so the caller adds it. `process_args` reads a
/// pid's command line, as `ps -o args= -p <pid>` prints it.
pub fn read_all(programs: &Programs, process_args: impl Fn(u32) -> Option<String>) -> Versions {
    let version = |program: &[OsString]| read_version(program).ok();
    let cli = version(&programs.no_mistakes);
    let daemon = programs
        .no_mistakes_home
        .as_deref()
        .and_then(|home| no_mistakes_daemon_version(home, process_args));
    let tools = json!({
        "tiller": env!("CARGO_PKG_VERSION"),
        "gh-axi": version(&programs.gh_axi),
        "quota-axi": version(&programs.quota_axi),
        "git": version(&programs.git),
        "no-mistakes": cli,
        "no-mistakes-daemon": daemon,
    });
    let skew = skew(cli.as_deref(), daemon.as_deref());
    Versions { tools, skew }
}

/// The command line of `pid` from the process list.
pub fn ps_args(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|args| !args.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI: &str = "no-mistakes version v1.106.1 (a1b2c3d) 2026-11-01T08:30:42Z";
    const OLD: &str = "no-mistakes version v1.84.0 (4822244) 2026-09-26T08:30:42Z";

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(version_parts(OLD), vec![1, 84, 0]);
        assert!(version_parts(CLI) > version_parts(OLD));
        assert_eq!(version_parts("herdr 0.9.5"), vec![0, 9, 5]);
        assert!(version_parts("no digits here").is_empty());
    }

    #[test]
    fn a_daemon_older_than_its_cli_is_a_skew() {
        let line = skew(Some(CLI), Some(OLD)).unwrap();
        assert!(
            line.contains("v1.84.0, older than its CLI v1.106.1"),
            "{line}"
        );
        assert!(line.contains("no-mistakes daemon restart"));
        assert!(
            skew(Some(OLD), Some(CLI))
                .unwrap()
                .contains("newer than its CLI")
        );
    }

    #[test]
    fn matching_or_unknown_versions_are_no_skew() {
        // The build id and date differ run to run; the version decides.
        assert_eq!(
            skew(
                Some(CLI),
                Some("no-mistakes version v1.106.1 (ffff) 2026-11-02")
            ),
            None
        );
        assert_eq!(skew(Some(CLI), None), None);
        assert_eq!(skew(None, Some(OLD)), None);
        assert_eq!(skew(Some("garbled"), Some(OLD)), None);
    }

    #[test]
    fn the_daemon_binary_comes_from_its_pid_file_and_process_line() {
        let pid = r#"{"pid":271,"started_at":"2026-11-04T15:03:40Z"}"#;
        let args = |pid: u32| {
            assert_eq!(pid, 271);
            Some("/opt/nm 1.84/bin/no-mistakes daemon run --root /home/user/.no-mistakes".into())
        };
        assert_eq!(
            daemon_binary(pid, args),
            Some(PathBuf::from("/opt/nm 1.84/bin/no-mistakes"))
        );
        assert_eq!(daemon_binary(pid, |_| None), None);
        assert_eq!(daemon_binary("not json", |_| Some("x".into())), None);
        // Some other process reused the pid.
        assert_eq!(daemon_binary(pid, |_| Some("vim notes".into())), None);
    }
}
