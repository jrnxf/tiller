//! The review pages a run publishes. Each has a live session on the review
//! server and a listener waiting on it; cleanup ends both when the run goes
//! (P48), so a respawn storm never stacks listeners on the host.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

/// The review page tool, as far as cleanup needs it.
pub trait Listeners {
    /// Ends the review session of the page at `page`, and with it the
    /// listener polling it. Ending a page that has no session is not an
    /// error.
    fn end(&self, page: &Path) -> Result<(), String>;
}

/// The `lavish-axi` CLI.
pub struct Lavish {
    program: Vec<OsString>,
    /// The hostnames the review server answers to: ending a page starts the
    /// server when it is down, and the server reads these once, at start.
    hosts: Vec<String>,
}

impl Lavish {
    /// `program` is the command and its leading arguments, `["lavish-axi"]`
    /// unless the config names another.
    pub fn new(program: Vec<OsString>, hosts: Vec<String>) -> Self {
        Self { program, hosts }
    }
}

impl Listeners for Lavish {
    fn end(&self, page: &Path) -> Result<(), String> {
        let (program, leading) = self
            .program
            .split_first()
            .ok_or_else(|| "the lavish command is empty".to_owned())?;
        let mut command = Command::new(program);
        command
            .args(leading)
            .arg("end")
            .arg(page)
            .stdin(Stdio::null());
        if !self.hosts.is_empty() {
            command.env("LAVISH_AXI_ALLOWED_HOSTS", self.hosts.join(" "));
        }
        let out = command
            .output()
            .map_err(|e| format!("lavish-axi end: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        Err(format!(
            "lavish-axi end exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A host with no review server: nothing to end.
pub struct NoListeners;

impl Listeners for NoListeners {
    fn end(&self, _page: &Path) -> Result<(), String> {
        Ok(())
    }
}
