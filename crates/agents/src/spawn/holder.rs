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
/// adds the session's own variables. The list is short on purpose: a spawn
/// can run inside an orchestrator's sandbox, whose proxy, temp directory and
/// session markers must not outlive it in the worker.
const PASSED: &[&str] = &[
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
    passed(std::env::vars_os())
}

fn passed(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> BTreeMap<String, String> {
    vars.filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
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
/// holds. A spawn that fails may still have started the pane, when only
/// its answer was lost, so a running pane with the session's key is ended
/// before the error is returned.
pub(crate) fn open(
    panes: &dyn Panes,
    owner: &Owner<'_>,
    script: &Path,
    cwd: &Path,
) -> Result<Pane> {
    match panes.spawn(&request(owner, script, cwd)) {
        Ok(pane) => Ok(pane),
        Err(error) => {
            // A holder that does not answer the list cannot show a pane to
            // end; the spawn's own error is the one to report.
            let _ = end(panes, |p| p.key == owner.session);
            Err(error.into())
        }
    }
}

/// Ends a pane a failed launch opened, by its process group, since the
/// slice's holder has no `close` (H09 brings it). A pane that is gone or
/// has exited already needs nothing.
pub(crate) fn close(panes: &dyn Panes, pane: &str) -> std::result::Result<(), String> {
    let id = PaneId::from(pane);
    end(panes, |p| p.pane == id)
}

/// Kills the process group of the running pane that `which` picks.
fn end(panes: &dyn Panes, which: impl Fn(&Pane) -> bool) -> std::result::Result<(), String> {
    let listing = panes.list().map_err(|e| e.to_string())?;
    let Some(found) = listing
        .panes
        .iter()
        .find(|p| p.state == PaneState::Running && which(p))
    else {
        return Ok(());
    };
    let pane = &found.pane;
    let pid = i32::try_from(found.pid).map_err(|_| format!("pane {pane} has pid {}", found.pid))?;
    match nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    ) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
        Err(e) => return Err(format!("killing pane {pane} (pid {pid}): {e}")),
    }
    // The holder reaps the child a moment later. Until it does, the pane
    // lists as running, and a cleanup right after would refuse it.
    let deadline = std::time::Instant::now() + REAPED;
    loop {
        let listing = panes.list().map_err(|e| e.to_string())?;
        if !listing
            .panes
            .iter()
            .any(|p| p.pane == *pane && p.state == PaneState::Running)
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "pane {pane} still runs {} s after its kill",
                REAPED.as_secs()
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// How long a killed pane may take to list as exited.
const REAPED: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether the holder's `pane` still runs: a run's cleanup never ends a
/// session whose pane lives. A holder that does not answer cannot prove
/// the pane gone, so it refuses. With no holder socket at all, no holder
/// runs, and a pane dies with its holder's PTY, so none of its panes does.
pub(crate) fn running(panes: Option<&dyn Panes>, pane: &str) -> Result<bool> {
    let Some(panes) = panes else {
        return Ok(false);
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

    /// A holder that starts the pane but whose answer is lost, and that
    /// lists the pane with the pid of a real process group.
    struct LostReply {
        inner: tiller_panes::FakePanes,
        pid: u32,
    }

    impl Panes for LostReply {
        fn spawn(&self, req: &Spawn) -> tiller_panes::Result<Pane> {
            self.inner.spawn(req)?;
            Err(tiller_panes::PanesError::Unreachable(
                "no reply in time".into(),
            ))
        }

        fn list(&self) -> tiller_panes::Result<tiller_panes::Listing> {
            let mut listing = self.inner.list()?;
            for pane in &mut listing.panes {
                pane.pid = self.pid;
            }
            Ok(listing)
        }
    }

    #[test]
    fn a_spawn_whose_answer_is_lost_ends_the_pane_it_started() {
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let panes = LostReply {
            inner: tiller_panes::FakePanes::new(),
            pid: child.id(),
        };
        let owner = Owner {
            session: "S1",
            run: Some("R1"),
            project: "alpha",
            role: "builder",
            label: "alpha-1 build",
        };
        let error = open(
            &panes,
            &owner,
            Path::new("/home/user/runs/R1/launch.sh"),
            Path::new("/home/user/pool/w1"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unreachable"), "{error}");
        // The stray pane's process group is ended, not left to run.
        let status = child.wait().unwrap();
        assert!(!status.success(), "{status:?}");
        use std::os::unix::process::ExitStatusExt as _;
        assert_eq!(status.signal(), Some(9), "{status:?}");
    }

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
