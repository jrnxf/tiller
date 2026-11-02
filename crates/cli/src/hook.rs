//! `tiller hook <event>` (`protocol.md` section 4): reads the hook input on
//! stdin and hands it to `tiller-observe`, which writes `state.db` directly,
//! with intake (`tiller-agents`) plugged in. A hook always exits 0, so a
//! failure never stops a session; it prints only what intake hands back (an
//! ask's context line, an inbox's messages, a `block-once` decision).

use std::io::Read;
use std::path::PathBuf;

use tiller_agents::handoff;
use tiller_agents::intake::Hooks;
use tiller_core::Timestamp;
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
    // Only the two hooks that carry intake read config.toml, so a tool
    // hook keeps its budget.
    let intake = matches!(args.event.as_str(), "UserPromptSubmit" | "Stop")
        .then(|| Hooks::load(&env.tiller_home));
    let intake = intake.as_ref().map(|hooks| hooks as &dyn hooks::Intake);
    let (outcome, output) = hooks::run_with(&args.event, &input, &env, intake);
    // A compaction is a breakpoint: the note asking for the handoff is
    // written now, not on the daemon's next pass. A failure here is dropped
    // like any hook failure; the daemon's pass sends the note.
    if args.event == "PreCompact" && matches!(outcome, hooks::Outcome::Recorded { .. }) {
        let _ = tiller_core::Store::open_current(env.tiller_home.join("state.db"))
            .and_then(|mut store| handoff::signal_compactions(&mut store, Timestamp::now()));
    }
    if let Some(output) = output {
        println!("{output}");
    }
}
