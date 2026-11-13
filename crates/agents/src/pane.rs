//! A session's pane as stop, resume, relaunch and interrupt see it
//! (`terminal-holder.md` 4.4, 4.6): the terminal holder's record of it,
//! with the composer read off its screen.

use std::time::Duration;

use tiller_panes::composer::{Composer, classify};
use tiller_panes::{Format, Key, Meta, PaneId, PaneState, Panes, PanesError, Read};

/// How long a pane's child gets after `hup` before `kill`, when a pane is
/// closed (4.4 Stop).
pub const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// The agent a running pane holds.
const AGENT: &str = "claude";

type Result<T> = std::result::Result<T, PanesError>;

/// One pane, as the holder lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneInfo {
    pub pane_id: String,
    /// The agent in it; `None` once its child has exited.
    pub agent: Option<String>,
    /// What the composer shows: `working`, `idle`, `blocked` or `unknown`.
    pub agent_status: Option<String>,
    /// Whose the pane is: the `session`, `run` and `label` a spawn named.
    pub meta: Meta,
}

/// The host's terminal holder, or none when no holder runs in this home:
/// every pane is then unreachable.
#[derive(Clone, Copy)]
pub struct Holder<'a> {
    panes: Option<&'a dyn Panes>,
}

/// The lifecycle word for what the composer shows (4.6).
fn status(composer: &Composer) -> &'static str {
    match composer {
        Composer::Busy => "working",
        Composer::Empty | Composer::Holds(_) => "idle",
        Composer::Dialog => "blocked",
        Composer::Unknown => "unknown",
    }
}

impl<'a> Holder<'a> {
    pub fn new(panes: Option<&'a dyn Panes>) -> Self {
        Self { panes }
    }

    /// The holder's panes, or why there are none.
    pub fn require(&self) -> Result<&'a dyn Panes> {
        self.panes.ok_or_else(|| {
            PanesError::Unreachable(
                "no terminal holder runs in this home; tiller daemon starts one".to_owned(),
            )
        })
    }

    /// The pane, or `None` when the holder no longer has it.
    pub fn pane(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        let panes = self.require()?;
        let id = PaneId::from(pane_id);
        let pane = match panes.get(&id) {
            Ok(Some(pane)) => pane,
            Ok(None) | Err(PanesError::NotFound(_)) => return Ok(None),
            Err(err) => return Err(err),
        };
        // An exited child is a pane with no agent in it.
        let (agent, agent_status) = if pane.state == PaneState::Running {
            let screen = panes.screen(&id, Read::Visible, Format::Ansi)?;
            let composer = classify(&screen.text());
            (Some(AGENT.to_owned()), Some(status(&composer).to_owned()))
        } else {
            (None, None)
        };
        Ok(Some(PaneInfo {
            pane_id: pane_id.to_owned(),
            agent,
            agent_status,
            meta: pane.meta,
        }))
    }

    /// The visible screen, with its colours, for the composer to read.
    pub fn read_visible(&self, pane_id: &str) -> Result<String> {
        self.require()?
            .screen(&PaneId::from(pane_id), Read::Visible, Format::Ansi)
            .map(|screen| screen.text())
    }

    pub fn send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.require()?.input(&PaneId::from(pane_id), text)
    }

    pub fn send_enter(&self, pane_id: &str) -> Result<()> {
        self.require()?.keys(&PaneId::from(pane_id), &[Key::ENTER])
    }

    pub fn send_escape(&self, pane_id: &str) -> Result<()> {
        self.require()?.keys(&PaneId::from(pane_id), &[Key::ESC])
    }

    /// A running child gets `hup`, then `kill` after [`CLOSE_GRACE`]; the
    /// exited pane is then forgotten (4.4 Stop). A pane already gone is
    /// closed.
    pub fn close_pane(&self, pane_id: &str) -> Result<()> {
        let panes = self.require()?;
        let id = PaneId::from(pane_id);
        let pane = match panes.get(&id) {
            Ok(Some(pane)) => pane,
            Ok(None) | Err(PanesError::NotFound(_)) => return Ok(()),
            Err(err) => return Err(err),
        };
        if pane.state == PaneState::Running {
            match panes.close(&id, CLOSE_GRACE) {
                Ok(_) | Err(PanesError::NotFound(_) | PanesError::Exited(_)) => {}
                Err(err) => return Err(err),
            }
        }
        match panes.forget(&id) {
            Ok(()) | Err(PanesError::NotFound(_)) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// How long the pane has printed nothing, when the holder knows.
    pub fn quiet_for(&self, pane_id: &str) -> Result<Option<Duration>> {
        match self.require()?.get(&PaneId::from(pane_id)) {
            Ok(pane) => Ok(pane.and_then(|pane| pane.quiet_for())),
            Err(PanesError::NotFound(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// The holder as a submit target: Enter, then a look at the composer.
impl tiller_panes::submit::Target for Holder<'_> {
    type Error = PanesError;

    fn press_enter(&self, pane: &str) -> Result<()> {
        self.send_enter(pane)
    }

    fn read_visible(&self, pane: &str) -> Result<String> {
        Holder::read_visible(self, pane)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use tiller_panes::{FakePanes, Spawn};

    fn spawn(panes: &FakePanes) -> String {
        let pane = panes
            .spawn(&Spawn {
                key: "s1".into(),
                argv: vec!["/bin/sh".into()],
                cwd: "/home/user".into(),
                env: Default::default(),
                size: None,
                meta: [("session".to_owned(), "s1".to_owned())].into(),
            })
            .unwrap();
        pane.pane.to_string()
    }

    #[test]
    fn a_pane_reads_and_types_through_the_holder() {
        let panes = FakePanes::new();
        let pane = spawn(&panes);
        let holder = Holder::new(Some(&panes));
        let info = holder.pane(&pane).unwrap().unwrap();
        assert_eq!(info.agent.as_deref(), Some("claude"));
        assert_eq!(info.meta.get("session").map(String::as_str), Some("s1"));
        holder.send_text(&pane, "hello").unwrap();
        holder.send_enter(&pane).unwrap();
        let calls = panes.calls();
        assert!(calls.contains(&format!("input {pane} hello")), "{calls:?}");
        assert!(calls.contains(&format!("keys {pane} enter")), "{calls:?}");
    }

    #[test]
    fn closing_a_pane_forgets_it() {
        let panes = FakePanes::new();
        let pane = spawn(&panes);
        let holder = Holder::new(Some(&panes));
        holder.close_pane(&pane).unwrap();
        assert_eq!(holder.pane(&pane).unwrap(), None);
        holder.close_pane(&pane).unwrap();
    }

    #[test]
    fn a_pane_with_no_holder_is_unreachable() {
        let holder = Holder::new(None);
        assert!(matches!(holder.pane("p1"), Err(PanesError::Unreachable(_))));
    }
}
