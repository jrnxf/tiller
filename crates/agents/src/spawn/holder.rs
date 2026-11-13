//! A session's pane in the terminal holder (`terminal-holder.md` 4.4): one
//! `spawn` that runs the launch script.
//!
//! The holder runs the launch script itself, which `exec`s the agent, so
//! nothing is typed into the pane. The pane's `key` is the session id and
//! its `meta` names the session, the run and the label, so the pane says
//! whose it is.

use std::path::Path;

use crate::pane::CLOSE_GRACE;
use tiller_panes::{Meta, Pane, PaneId, PaneState, Panes, PanesError, Spawn};

use super::Result;

/// The shell that runs a launch script: `argv[0]` must be absolute, since
/// the holder does no `PATH` search.
const SHELL: &str = tiller_panes::env::SHELL;

/// Whose a pane is, for its `meta`.
pub(crate) struct Owner<'a> {
    pub session: &'a str,
    pub run: Option<&'a str>,
    pub project: &'a str,
    pub role: &'a str,
    pub label: &'a str,
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
        env: tiller_panes::env::current(),
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
        Err(error) => match end(panes, |p| p.key == owner.session) {
            Ok(()) => Err(error.into()),
            // The holder may run a pane for the session that nothing ends.
            Err(note) => Err(super::SpawnError::Undone {
                error: Box::new(error.into()),
                notes: format!(
                    "a pane for session {} may have started in the terminal holder and could not be checked: {note}",
                    owner.session
                ),
            }),
        },
    }
}

/// Ends a pane a failed launch opened: `close`, which sends `hup` and then
/// `kill` after the grace, and `forget` (4.4 Stop). A pane that is gone or
/// has exited already is only forgotten.
pub(crate) fn close(panes: &dyn Panes, pane: &str) -> std::result::Result<(), String> {
    let id = PaneId::from(pane);
    end(panes, |p| p.pane == id)
}

/// Closes and forgets the pane that `which` picks.
fn end(panes: &dyn Panes, which: impl Fn(&Pane) -> bool) -> std::result::Result<(), String> {
    let listing = panes.list().map_err(|e| e.to_string())?;
    let Some(found) = listing.panes.iter().find(|p| which(p)) else {
        return Ok(());
    };
    let pane = &found.pane;
    if found.state == PaneState::Running {
        match panes.close(pane, CLOSE_GRACE) {
            Ok(_) | Err(PanesError::NotFound(_) | PanesError::Exited(_)) => {}
            Err(e) => return Err(format!("closing pane {pane}: {e}")),
        }
    }
    match panes.forget(pane) {
        Ok(()) | Err(PanesError::NotFound(_)) => Ok(()),
        Err(e) => Err(format!("forgetting pane {pane}: {e}")),
    }
}

/// Whether the holder's `pane` still runs: a run's cleanup never ends a
/// session whose pane lives. A holder that does not answer cannot prove
/// the pane gone, so it refuses. With no holder socket at all, no holder
/// runs, and a pane dies with its holder's PTY, so none of its panes does.
pub(crate) fn running(panes: Option<&dyn Panes>, pane: &str) -> Result<bool> {
    Ok(state(panes, pane)? == Some(PaneState::Running))
}

/// The state of the holder's `pane`, or None when the holder does not list
/// it (or no holder runs), on the terms of [`running`].
pub(crate) fn state(panes: Option<&dyn Panes>, pane: &str) -> Result<Option<PaneState>> {
    let Some(panes) = panes else {
        return Ok(None);
    };
    let id = PaneId::from(pane);
    Ok(panes
        .list()?
        .panes
        .iter()
        .find(|p| p.pane == id)
        .map(|p| p.state))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// A holder that starts the pane but whose answer is lost.
    struct LostReply(tiller_panes::FakePanes);

    impl Panes for LostReply {
        fn spawn(&self, req: &Spawn) -> tiller_panes::Result<Pane> {
            self.0.spawn(req)?;
            Err(tiller_panes::PanesError::Unreachable(
                "no reply in time".into(),
            ))
        }

        fn list(&self) -> tiller_panes::Result<tiller_panes::Listing> {
            self.0.list()
        }

        fn get(&self, pane: &PaneId) -> tiller_panes::Result<Option<Pane>> {
            self.0.get(pane)
        }

        fn screen(
            &self,
            pane: &PaneId,
            read: tiller_panes::message::Read,
            format: tiller_panes::message::Format,
        ) -> tiller_panes::Result<tiller_panes::message::Screen> {
            self.0.screen(pane, read, format)
        }

        fn input(&self, pane: &PaneId, text: &str) -> tiller_panes::Result<()> {
            self.0.input(pane, text)
        }

        fn paste(&self, pane: &PaneId, text: &str) -> tiller_panes::Result<()> {
            self.0.paste(pane, text)
        }

        fn keys(&self, pane: &PaneId, keys: &[tiller_panes::Key]) -> tiller_panes::Result<()> {
            self.0.keys(pane, keys)
        }

        fn set_meta(
            &self,
            pane: &PaneId,
            set: &tiller_panes::message::Meta,
            remove: &[&str],
        ) -> tiller_panes::Result<Pane> {
            self.0.set_meta(pane, set, remove)
        }

        fn signal(
            &self,
            pane: &PaneId,
            signal: tiller_panes::message::Signal,
        ) -> tiller_panes::Result<()> {
            self.0.signal(pane, signal)
        }

        fn close(
            &self,
            pane: &PaneId,
            grace: std::time::Duration,
        ) -> tiller_panes::Result<tiller_panes::message::Exit> {
            self.0.close(pane, grace)
        }

        fn forget(&self, pane: &PaneId) -> tiller_panes::Result<()> {
            self.0.forget(pane)
        }

        fn switch(&self, pane: &PaneId) -> tiller_panes::Result<usize> {
            self.0.switch(pane)
        }
    }

    #[test]
    fn a_spawn_whose_answer_is_lost_closes_and_forgets_the_pane_it_started() {
        let fake = tiller_panes::FakePanes::new();
        let panes = LostReply(fake.clone());
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
        // The stray pane is closed and forgotten, not left to run.
        let calls = fake.calls();
        assert!(calls.iter().any(|c| c.starts_with("close ")), "{calls:?}");
        assert!(calls.iter().any(|c| c.starts_with("forget ")), "{calls:?}");
        assert!(fake.list().unwrap().panes.is_empty());
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
                .all(|name| tiller_panes::env::PASSED.contains(&name.as_str())
                    || name.starts_with("LC_"))
        );
    }
}
