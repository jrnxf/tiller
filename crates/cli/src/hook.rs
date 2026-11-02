//! `tiller hook <event>` (`protocol.md` section 4): reads the hook input on
//! stdin and hands it to `tiller-observe`, which writes `state.db` directly.
//! A hook always exits 0 with no output, so a failure never stops a session.

use std::io::Read;
use std::path::PathBuf;

use tiller_observe::hooks::{self, Env};

#[derive(clap::Args)]
pub struct Args {
    /// The harness whose hook fired; Claude Code is the only adapter (C13)
    #[arg(long, default_value = "claude", value_parser = ["claude"])]
    harness: String,
    /// The hook event name, as Claude Code passes it
    event: String,
}

pub fn run(args: &Args) {
    let mut input = Vec::new();
    // An unreadable stdin is an empty input; the hook logs it and moves on.
    let _ = std::io::stdin().read_to_end(&mut input);
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    // No home at all: there is no state.db to find, and a relative path would
    // write into the session's checkout.
    let Some(tiller_home) = std::env::var_os("TILLER_HOME")
        .map(PathBuf::from)
        .or_else(|| user_home.as_ref().map(|home| home.join(".tiller")))
    else {
        return;
    };
    let env = Env {
        tiller_home,
        user_home,
        tiller_session_id: std::env::var("TILLER_SESSION_ID")
            .ok()
            .filter(|id| !id.is_empty()),
    };
    hooks::run(&args.event, &input, &env);
}
