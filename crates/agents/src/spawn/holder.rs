//! A session's pane in the terminal holder (`terminal-holder.md` 4.4), the
//! first slice (S1c): one `spawn` in place of herdr's workspace, row
//! tokens, layout pass and typed launch line.
//!
//! The holder runs the launch script itself, which `exec`s the agent, so
//! nothing is typed into the pane. The pane's `key` is the session id and
//! its `meta` names the session, the run and the label, so the pane says
//! whose it is without herdr's tokens.

use std::collections::BTreeMap;
use std::path::Path;

use tiller_panes::{Meta, Pane, PaneId, PaneState, Panes, Spawn};

use super::Result;

/// The shell that runs a launch script: `argv[0]` must be absolute, since
/// the holder does no `PATH` search.
const SHELL: &str = "/bin/sh";

/// The environment names a pane gets from the process that launches it.
/// The holder passes on nothing of its own (spec 2.4), and the launch script
/// adds the session's own variables.
const PASSED: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "LANG",
    "LANGUAGE",
    "TZ",
    "TMPDIR",
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

/// Whose a pane is, for its `meta`.
pub(crate) struct Owner<'a> {
    pub session: &'a str,
    pub run: Option<&'a str>,
    pub project: &'a str,
    pub role: &'a str,
    pub label: &'a str,
}

/// The pane's environment: [`PASSED`] and every `LC_*` this process has.
fn env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, _)| PASSED.contains(&name.as_str()) || name.starts_with("LC_"))
        .collect()
}

/// The `spawn` that runs `script` in `cwd` for `owner`.
pub(crate) fn request(owner: &Owner<'_>, script: &Path, cwd: &Path) -> Spawn {
    let mut meta = Meta::from([
        ("session".to_owned(), owner.session.to_owned()),
        ("project".to_owned(), owner.project.to_owned()),
        ("role".to_owned(), owner.role.to_owned()),
        ("label".to_owned(), owner.label.to_owned()),
    ]);
    if let Some(run) = owner.run {
        meta.insert("run".to_owned(), run.to_owned());
    }
    Spawn {
        key: owner.session.to_owned(),
        argv: vec![SHELL.to_owned(), script.to_string_lossy().into_owned()],
        cwd: cwd.to_string_lossy().into_owned(),
        env: env(),
        size: None,
        meta,
    }
}

/// Starts the session's pane. A pane that is already running for the
/// session is returned as it is: the holder starts nothing for a key it
/// holds.
pub(crate) fn open(
    panes: &dyn Panes,
    owner: &Owner<'_>,
    script: &Path,
    cwd: &Path,
) -> Result<Pane> {
    Ok(panes.spawn(&request(owner, script, cwd))?)
}

/// Ends a pane a failed launch opened, by its process group, since the
/// slice's holder has no `close` (H09 brings it). A pane that is gone or
/// has exited already needs nothing.
pub(crate) fn close(panes: &dyn Panes, pane: &str) -> std::result::Result<(), String> {
    let listing = panes.list().map_err(|e| e.to_string())?;
    let id = PaneId::from(pane);
    let Some(found) = listing
        .panes
        .iter()
        .find(|p| p.pane == id && p.state == PaneState::Running)
    else {
        return Ok(());
    };
    let pid = i32::try_from(found.pid).map_err(|_| format!("pane {pane} has pid {}", found.pid))?;
    match nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    ) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(e) => Err(format!("killing pane {pane} (pid {pid}): {e}")),
    }
}

/// Whether one of `sessions`' panes still runs in the holder: a run's
/// cleanup never ends a session whose pane lives. A holder that does not
/// answer cannot prove the pane gone, so it refuses too.
pub(crate) fn running(panes: Option<&dyn Panes>, pane: &str) -> Result<bool> {
    let Some(panes) = panes else {
        return Ok(true);
    };
    let id = PaneId::from(pane);
    Ok(panes
        .list()?
        .panes
        .iter()
        .any(|p| p.pane == id && p.state == PaneState::Running))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_request_runs_the_script_keyed_by_the_session_with_whose_it_is() {
        let owner = Owner {
            session: "S1",
            run: Some("R1"),
            project: "alpha",
            role: "builder",
            label: "alpha-1 build",
        };
        let req = request(
            &owner,
            Path::new("/home/user/runs/R1/launch.sh"),
            Path::new("/home/user/pool/w1"),
        );
        assert_eq!(req.key, "S1");
        assert_eq!(req.argv, ["/bin/sh", "/home/user/runs/R1/launch.sh"]);
        assert_eq!(req.cwd, "/home/user/pool/w1");
        assert_eq!(req.meta["session"], "S1");
        assert_eq!(req.meta["run"], "R1");
        assert_eq!(req.meta["label"], "alpha-1 build");
        assert!(
            req.env
                .keys()
                .all(|name| PASSED.contains(&name.as_str()) || name.starts_with("LC_"))
        );
    }
}
