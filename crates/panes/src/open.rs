//! Open in terminal (`terminal-holder.md` 5.3): the terminal app that shows
//! a pane, and what an open did. The app goes through [`Terminal`] so tests
//! never open one.

use std::ffi::OsString;
use std::io;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Serialize;

use crate::{PaneId, Panes, PanesError};

/// What "Open in terminal" did (`terminal-holder.md` 5.3). A terminal app
/// that does not open is the caller's error, which the answer reports as
/// `failed` with the reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Opened {
    /// The pane is focused; no terminal app is configured to raise.
    Focused,
    /// The pane is focused and the terminal app, already showing it, came
    /// to the front.
    Raised,
    /// The terminal app opened a new client, which shows the pane.
    Launched,
    /// The pane is focused, but no client is open on this machine and no
    /// terminal app is configured to open one.
    Unseen,
}

/// The terminal app a pane is shown in.
pub trait Terminal: Send + Sync {
    /// The app's name, for answers and errors.
    fn name(&self) -> &str;
    /// Brings the running app to the front.
    fn raise(&self) -> io::Result<()>;
    /// Opens a new window of the app running `command`.
    fn open(&self, command: &[OsString]) -> io::Result<()>;
}

/// How long a launch waits for its client to attach, and how often it looks.
#[derive(Clone, Copy, Debug)]
pub struct Wait {
    pub every: Duration,
    pub within: Duration,
}

impl Default for Wait {
    fn default() -> Self {
        Self {
            every: Duration::from_millis(250),
            within: Duration::from_secs(10),
        }
    }
}

/// A macOS app, driven with `open`: `open -a <app>` raises it and
/// `open -na <app> --args <exec...> <command...>` opens a new instance
/// running the command. `exec` is what the app takes before a command to
/// run: `-e` for Ghostty and Alacritty, `start --` for WezTerm.
#[derive(Clone, Debug)]
pub struct MacApp {
    app: String,
    exec: Vec<OsString>,
}

impl MacApp {
    pub fn new(app: impl Into<String>) -> Self {
        Self {
            app: app.into(),
            exec: vec!["-e".into()],
        }
    }

    /// The arguments that come before the command, in place of `-e`.
    pub fn exec(mut self, exec: Vec<OsString>) -> Self {
        self.exec = exec;
        self
    }

    fn raise_command(&self) -> Command {
        let mut command = Command::new("open");
        command.arg("-a").arg(&self.app);
        command
    }

    fn open_command(&self, run: &[OsString]) -> Command {
        let mut command = Command::new("open");
        command
            .arg("-na")
            .arg(&self.app)
            .arg("--args")
            .args(&self.exec)
            .args(run);
        command
    }
}

/// Runs `open`, which returns once the app has the request.
fn launch(mut command: Command) -> io::Result<()> {
    let output = command.stdin(Stdio::null()).output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(io::Error::other(format!(
        "open exited with {}: {}",
        output.status,
        stderr.trim()
    )))
}

impl Terminal for MacApp {
    fn name(&self) -> &str {
        &self.app
    }

    fn raise(&self) -> io::Result<()> {
        launch(self.raise_command())
    }

    fn open(&self, command: &[OsString]) -> io::Result<()> {
        launch(self.open_command(command))
    }
}

/// Why "Open in terminal" failed.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Holder(#[from] PanesError),

    #[error("{app} did not open: {source}")]
    Terminal { app: String, source: io::Error },
}

/// Shows the holder's `pane` (`terminal-holder.md` 5.3): `switch` moves
/// every attach client to it, then a configured `terminal` comes to the
/// front, or, with no client attached, opens a new window running `attach`
/// (`tiller attach <session>`).
pub fn open_pane(
    panes: &dyn Panes,
    terminal: Option<&dyn Terminal>,
    pane: &PaneId,
    attach: &[OsString],
) -> Result<Opened, OpenError> {
    let attached = panes.switch(pane)? > 0;
    let Some(terminal) = terminal else {
        return Ok(if attached {
            Opened::Focused
        } else {
            Opened::Unseen
        });
    };
    let app = || terminal.name().to_owned();
    if attached {
        terminal
            .raise()
            .map_err(|source| OpenError::Terminal { app: app(), source })?;
        return Ok(Opened::Raised);
    }
    terminal
        .open(attach)
        .map_err(|source| OpenError::Terminal { app: app(), source })?;
    Ok(Opened::Launched)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_app_raises_with_open_a_and_launches_a_new_instance_with_its_command() {
        let app = MacApp::new("Ghostty");
        let raise = app.raise_command();
        assert_eq!(raise.get_program(), "open");
        assert_eq!(raise.get_args().collect::<Vec<_>>(), ["-a", "Ghostty"]);
        let command: Vec<OsString> = vec!["/home/user/bin/tiller".into()];
        let open = app.open_command(&command);
        assert_eq!(
            open.get_args().collect::<Vec<_>>(),
            ["-na", "Ghostty", "--args", "-e", "/home/user/bin/tiller"]
        );
        let open = MacApp::new("WezTerm")
            .exec(vec!["start".into(), "--".into()])
            .open_command(&command);
        assert_eq!(
            open.get_args().collect::<Vec<_>>(),
            [
                "-na",
                "WezTerm",
                "--args",
                "start",
                "--",
                "/home/user/bin/tiller"
            ]
        );
    }
}
