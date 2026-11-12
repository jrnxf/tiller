//! One [`Herdr`] for both pane holders (`terminal-holder.md` 4.4, 4.6): a
//! holder pane's calls go to the terminal holder and every other call goes
//! to herdr. The pane id tells the two apart
//! ([`tiller_core::ids::is_holder_pane`]).
//!
//! It lets delivery, stop, resume and interrupt drive a holder pane through
//! the code they already run on herdr panes, until H13 drops herdr.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::Path;
use std::time::Duration;

use tiller_core::ids::is_holder_pane;
use tiller_panes::{Format, Key, PaneId, PaneState, Panes, PanesError, Read};

use crate::client::{Created, Herdr, HerdrError, PaneInfo, Result, Snapshot};
use crate::composer::{Composer, classify};

/// How long a holder pane's child gets after `hup` before `kill`, when a
/// pane is closed (4.4 Stop).
pub const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// What a holder pane's agent reports as its `agent` (herdr's name for a
/// recognized Claude Code).
const AGENT: &str = "claude";

/// herdr for herdr panes, the terminal holder for holder panes.
pub struct Routed<H, P> {
    herdr: H,
    panes: Option<P>,
}

impl<'p, H, P> Routed<H, P>
where
    H: Herdr,
    P: Deref<Target = dyn Panes + 'p>,
{
    /// `panes` is the host's terminal holder, or None when it runs none: a
    /// holder pane is then unreachable.
    pub fn new(herdr: H, panes: Option<P>) -> Self {
        Self { herdr, panes }
    }

    fn holder(&self, pane_id: &str) -> Result<&(dyn Panes + 'p)> {
        match &self.panes {
            Some(panes) => Ok(panes.deref()),
            None => Err(HerdrError::Unreachable(format!(
                "{pane_id} is a terminal holder pane and this host runs no holder"
            ))),
        }
    }
}

/// A holder error in herdr's terms. A gone pane is `NotFound`, as herdr says
/// of a pane it no longer has.
fn herdr_error(call: &str, pane_id: &str, err: PanesError) -> HerdrError {
    match err {
        PanesError::NotFound(_) | PanesError::Exited(_) => HerdrError::NotFound {
            what: "pane",
            id: pane_id.to_owned(),
        },
        PanesError::Unreachable(detail) => HerdrError::Unreachable(detail),
        other => HerdrError::Api {
            call: call.to_owned(),
            code: "holder".to_owned(),
            message: other.to_string(),
        },
    }
}

/// herdr's lifecycle word for what the composer shows (4.6).
fn status(composer: &Composer) -> &'static str {
    match composer {
        Composer::Busy => "working",
        Composer::Empty | Composer::Holds(_) => "idle",
        Composer::Dialog => "blocked",
        Composer::Unknown => "unknown",
    }
}

impl<'p, H, P> Herdr for Routed<H, P>
where
    H: Herdr,
    P: Deref<Target = dyn Panes + 'p>,
{
    fn snapshot(&self) -> Result<Snapshot> {
        self.herdr.snapshot()
    }

    fn pane(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        if !is_holder_pane(pane_id) {
            return self.herdr.pane(pane_id);
        }
        let holder = self.holder(pane_id)?;
        let id = PaneId::from(pane_id);
        let pane = match holder.get(&id) {
            Ok(Some(pane)) => pane,
            Ok(None) | Err(PanesError::NotFound(_)) => return Ok(None),
            Err(err) => return Err(herdr_error("pane.get", pane_id, err)),
        };
        // An exited child is a pane with no agent in it, as herdr reports a
        // pane whose Claude has quit.
        let (agent, agent_status) = if pane.state == PaneState::Running {
            let screen = holder
                .screen(&id, Read::Visible, Format::Ansi)
                .map_err(|err| herdr_error("pane.screen", pane_id, err))?;
            let composer = classify(&screen.text());
            (Some(AGENT.to_owned()), Some(status(&composer).to_owned()))
        } else {
            (None, None)
        };
        Ok(Some(PaneInfo {
            pane_id: pane_id.to_owned(),
            workspace_id: String::new(),
            agent,
            agent_status,
            tokens: pane.meta.into_iter().collect(),
        }))
    }

    fn read_visible(&self, pane_id: &str) -> Result<String> {
        if !is_holder_pane(pane_id) {
            return self.herdr.read_visible(pane_id);
        }
        self.holder(pane_id)?
            .screen(&PaneId::from(pane_id), Read::Visible, Format::Ansi)
            .map(|screen| screen.text())
            .map_err(|err| herdr_error("pane.screen", pane_id, err))
    }

    fn send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        if !is_holder_pane(pane_id) {
            return self.herdr.send_text(pane_id, text);
        }
        self.holder(pane_id)?
            .input(&PaneId::from(pane_id), text)
            .map_err(|err| herdr_error("pane.input", pane_id, err))
    }

    fn send_enter(&self, pane_id: &str) -> Result<()> {
        self.key(pane_id, Key::ENTER)
    }

    fn send_escape(&self, pane_id: &str) -> Result<()> {
        self.key(pane_id, Key::ESC)
    }

    fn report_tokens(&self, pane_id: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        if !is_holder_pane(pane_id) {
            return self.herdr.report_tokens(pane_id, tokens);
        }
        let set = tokens.clone().into_iter().collect();
        self.holder(pane_id)?
            .set_meta(&PaneId::from(pane_id), &set, &[])
            .map(drop)
            .map_err(|err| herdr_error("pane.set_meta", pane_id, err))
    }

    fn create_workspace(&self, label: &str, cwd: Option<&Path>) -> Result<Created> {
        self.herdr.create_workspace(label, cwd)
    }

    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        self.herdr.rename_workspace(workspace_id, label)
    }

    fn move_workspace(&self, workspace_id: &str, index: usize) -> Result<()> {
        self.herdr.move_workspace(workspace_id, index)
    }

    /// A running child gets `hup`, then `kill` after [`CLOSE_GRACE`]; the
    /// exited pane is then forgotten (4.4 Stop).
    fn close_pane(&self, pane_id: &str) -> Result<()> {
        if !is_holder_pane(pane_id) {
            return self.herdr.close_pane(pane_id);
        }
        let holder = self.holder(pane_id)?;
        let id = PaneId::from(pane_id);
        let pane = match holder.get(&id) {
            Ok(Some(pane)) => pane,
            Ok(None) | Err(PanesError::NotFound(_)) => return Ok(()),
            Err(err) => return Err(herdr_error("pane.get", pane_id, err)),
        };
        if pane.state == PaneState::Running {
            match holder.close(&id, CLOSE_GRACE) {
                Ok(_) | Err(PanesError::NotFound(_) | PanesError::Exited(_)) => {}
                Err(err) => return Err(herdr_error("pane.close", pane_id, err)),
            }
        }
        match holder.forget(&id) {
            Ok(()) | Err(PanesError::NotFound(_)) => Ok(()),
            Err(err) => Err(herdr_error("pane.forget", pane_id, err)),
        }
    }

    fn notify(&self, title: &str, body: &str) -> Result<()> {
        self.herdr.notify(title, body)
    }

    fn quiet_for(&self, pane_id: &str) -> Result<Option<Duration>> {
        if !is_holder_pane(pane_id) {
            return self.herdr.quiet_for(pane_id);
        }
        match self.holder(pane_id)?.get(&PaneId::from(pane_id)) {
            Ok(pane) => Ok(pane.and_then(|pane| pane.quiet_for())),
            Err(PanesError::NotFound(_)) => Ok(None),
            Err(err) => Err(herdr_error("pane.get", pane_id, err)),
        }
    }
}

impl<'p, H, P> Routed<H, P>
where
    H: Herdr,
    P: Deref<Target = dyn Panes + 'p>,
{
    fn key(&self, pane_id: &str, key: Key) -> Result<()> {
        if !is_holder_pane(pane_id) {
            return if key == Key::ENTER {
                self.herdr.send_enter(pane_id)
            } else {
                self.herdr.send_escape(pane_id)
            };
        }
        self.holder(pane_id)?
            .keys(&PaneId::from(pane_id), &[key])
            .map_err(|err| herdr_error("pane.keys", pane_id, err))
    }
}

/// A borrowed herdr is a herdr, so a [`Routed`] can wrap `&dyn Herdr`.
impl<T: Herdr + ?Sized> Herdr for &T {
    fn snapshot(&self) -> Result<Snapshot> {
        (**self).snapshot()
    }
    fn pane(&self, pane_id: &str) -> Result<Option<PaneInfo>> {
        (**self).pane(pane_id)
    }
    fn read_visible(&self, pane_id: &str) -> Result<String> {
        (**self).read_visible(pane_id)
    }
    fn send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        (**self).send_text(pane_id, text)
    }
    fn send_enter(&self, pane_id: &str) -> Result<()> {
        (**self).send_enter(pane_id)
    }
    fn send_escape(&self, pane_id: &str) -> Result<()> {
        (**self).send_escape(pane_id)
    }
    fn report_tokens(&self, pane_id: &str, tokens: &BTreeMap<String, String>) -> Result<()> {
        (**self).report_tokens(pane_id, tokens)
    }
    fn create_workspace(&self, label: &str, cwd: Option<&Path>) -> Result<Created> {
        (**self).create_workspace(label, cwd)
    }
    fn rename_workspace(&self, workspace_id: &str, label: &str) -> Result<()> {
        (**self).rename_workspace(workspace_id, label)
    }
    fn move_workspace(&self, workspace_id: &str, index: usize) -> Result<()> {
        (**self).move_workspace(workspace_id, index)
    }
    fn close_pane(&self, pane_id: &str) -> Result<()> {
        (**self).close_pane(pane_id)
    }
    fn notify(&self, title: &str, body: &str) -> Result<()> {
        (**self).notify(title, body)
    }
    fn quiet_for(&self, pane_id: &str) -> Result<Option<Duration>> {
        (**self).quiet_for(pane_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeHerdr;
    use tiller_panes::{FakePanes, Spawn};

    fn herdr() -> (tempfile::TempDir, FakeHerdr) {
        let dir = tempfile::tempdir().unwrap();
        let herdr = FakeHerdr::new(dir.path().join("herdr.json"));
        (dir, herdr)
    }

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
    fn a_holder_pane_reads_and_types_through_the_holder() {
        let panes = FakePanes::new();
        let pane = spawn(&panes);
        let (_dir, fake) = herdr();
        let routed = Routed::new(fake, Some(&panes as &dyn Panes));
        let info = routed.pane(&pane).unwrap().unwrap();
        assert_eq!(info.agent.as_deref(), Some("claude"));
        assert_eq!(info.tokens.get("session").map(String::as_str), Some("s1"));
        routed.send_text(&pane, "hello").unwrap();
        routed.send_enter(&pane).unwrap();
        let calls = panes.calls();
        assert!(calls.contains(&format!("input {pane} hello")), "{calls:?}");
        assert!(calls.contains(&format!("keys {pane} enter")), "{calls:?}");
    }

    #[test]
    fn closing_a_holder_pane_forgets_it() {
        let panes = FakePanes::new();
        let pane = spawn(&panes);
        let (_dir, fake) = herdr();
        let routed = Routed::new(fake, Some(&panes as &dyn Panes));
        routed.close_pane(&pane).unwrap();
        assert_eq!(routed.pane(&pane).unwrap(), None);
        routed.close_pane(&pane).unwrap();
    }

    #[test]
    fn a_holder_pane_with_no_holder_is_unreachable() {
        let (_dir, fake) = herdr();
        let routed: Routed<_, &dyn Panes> = Routed::new(fake, None);
        let pane = format!("p{}", tiller_core::ids::new_ulid());
        assert!(matches!(
            routed.pane(&pane),
            Err(HerdrError::Unreachable(_))
        ));
    }
}
