//! What a pane's child gets from the process that spawns it
//! (`terminal-holder.md` 2.4): the holder passes on nothing of its own.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

/// The shell that runs a launch line or script: `argv[0]` must be absolute,
/// since the holder does no `PATH` search.
pub const SHELL: &str = "/bin/sh";

/// The environment names a pane gets from the process that launches it.
/// The launch script adds the session's own variables. The list is short on
/// purpose: a spawn can run inside an orchestrator's sandbox, whose proxy,
/// temp directory and session markers must not outlive it in the worker.
pub const PASSED: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "LANG",
    "LANGUAGE",
    "TZ",
    "SSH_AUTH_SOCK",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "NIX_PROFILES",
    "NIX_SSL_CERT_FILE",
    "SSL_CERT_FILE",
];

/// A pane's environment: [`PASSED`] and every `LC_*` this process has.
pub fn current() -> BTreeMap<String, String> {
    passed(std::env::vars_os())
}

/// The names of `vars` a pane gets, as [`current`] picks them.
pub fn passed(vars: impl Iterator<Item = (OsString, OsString)>) -> BTreeMap<String, String> {
    vars.filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| PASSED.contains(&name.as_str()) || name.starts_with("LC_"))
        .collect()
}

/// Whose agent a pane runs, for [`agent`].
#[derive(Clone, Copy, Debug)]
pub struct Agent<'a> {
    /// The effective Tiller home.
    pub tiller_home: &'a Path,
    pub session: &'a str,
    /// The run, or empty for the orchestrator, which has none.
    pub run: &'a str,
    pub role: &'a str,
    /// Tiller's `CLAUDE_CONFIG_DIR`, when it has one.
    pub claude_config: Option<&'a Path>,
}

/// The Tiller environment of an agent's pane (`authority.md` section 7):
/// what a first launch's script exports and what every resume of the
/// session gives its pane, so the agent's `tiller` and hooks find the same
/// home and session whichever path started it. A pane gets none of it
/// from [`current`].
pub fn agent(agent: &Agent<'_>) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1".to_owned()),
        ("TILLER_SESSION_ID", agent.session.to_owned()),
        ("TILLER_RUN_ID", agent.run.to_owned()),
        ("TILLER_ROLE", agent.role.to_owned()),
        // The session's `tiller` and its guard hook find the Tiller home by
        // this; without it they fall back to `$HOME/.tiller`.
        (
            "TILLER_HOME",
            agent.tiller_home.to_string_lossy().into_owned(),
        ),
    ];
    if let Some(dir) = agent.claude_config {
        env.push(("CLAUDE_CONFIG_DIR", dir.to_string_lossy().into_owned()));
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_pane_names_its_home_session_run_and_role() {
        let env: BTreeMap<_, _> = agent(&Agent {
            tiller_home: Path::new("/home/user/.tiller-b"),
            session: "S1",
            run: "",
            role: "orchestrator",
            claude_config: None,
        })
        .into_iter()
        .collect();
        assert_eq!(env["TILLER_HOME"], "/home/user/.tiller-b");
        assert_eq!(env["TILLER_SESSION_ID"], "S1");
        assert_eq!(env["TILLER_RUN_ID"], "");
        assert_eq!(env["TILLER_ROLE"], "orchestrator");
        assert_eq!(env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1");
        assert!(!env.contains_key("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn a_pane_gets_the_users_basics_but_not_the_launchers_sandbox_or_session() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("LC_ALL", "C.UTF-8"),
            ("HTTPS_PROXY", "http://192.0.2.1:3128"),
            ("TMPDIR", "/home/user/sandbox/tmp"),
            ("TILLER_SESSION_ID", "S0"),
            ("CLAUDECODE", "1"),
            ("TERM", "dumb"),
        ]
        .map(|(name, value)| (name.into(), value.into()));
        let env = passed(vars.into_iter());
        assert_eq!(
            env.keys().map(String::as_str).collect::<Vec<_>>(),
            ["LC_ALL", "PATH"]
        );
    }
}
