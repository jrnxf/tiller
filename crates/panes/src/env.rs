//! What a pane's child gets from the process that spawns it
//! (`terminal-holder.md` 2.4): the holder passes on nothing of its own.

use std::collections::BTreeMap;
use std::ffi::OsString;

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

#[cfg(test)]
mod tests {
    use super::*;

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
