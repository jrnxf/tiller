//! `tiller hook <event>` (`protocol.md` section 4): reads the hook input on
//! stdin and hands it to `tiller-observe`, which writes `state.db` directly,
//! with intake (`tiller-agents`) plugged in. A hook always exits 0, so a
//! failure never stops a session; it prints only what intake hands back (an
//! ask's context line, an inbox's messages, a `block-once` decision).

use std::io::Read;
use std::path::PathBuf;

use rusqlite::config::DbConfig;
use tiller_agents::handoff;
use tiller_agents::intake::Hooks;
use tiller_core::Timestamp;
use tiller_observe::harness::{self, Harness};
use tiller_observe::hooks::{self, Env};

#[derive(clap::Args)]
pub struct Args {
    /// The harness whose hook fired; each has an adapter table (D04)
    #[arg(long, default_value = "claude", value_parser = Harness::NAMES)]
    harness: String,
    /// The hook event name, as the harness passes it
    event: String,
}

pub fn run(args: &Args) {
    let mut input = Vec::new();
    // An unreadable stdin is an empty input; the hook logs it and moves on.
    let _ = std::io::stdin().read_to_end(&mut input);
    // The parser admits only adapter names; the fallback is Claude Code.
    let harness = Harness::parse(&args.harness).unwrap_or(Harness::Claude);
    // A harness's own event name and input become Claude Code's, so the
    // pipeline below is the one table of `protocol.md` 4.2 for every harness.
    let Some(harness::Normalized { event, input }) =
        harness::normalize(harness, &args.event, &input)
    else {
        return;
    };
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
    // hook keeps its budget. Intake prints Claude Code's output shape, so
    // another harness's adapter observes and prints nothing.
    let intake = (harness == Harness::Claude
        && matches!(event.as_str(), "UserPromptSubmit" | "Stop"))
    .then(|| Hooks::load(&env.tiller_home));
    let intake = intake.as_ref().map(|hooks| hooks as &dyn hooks::Intake);
    let (outcome, output) = hooks::run_with(&event, &input, &env, intake);
    // A compaction is a breakpoint: the note asking for the handoff is
    // written here, after the observation. A failure is dropped like any
    // hook failure; the note is lost, and the orchestrator still writes a
    // handoff at its next natural breakpoint.
    if event == "PreCompact" && matches!(outcome, hooks::Outcome::Recorded { .. }) {
        let _ = tiller_core::Store::open_current(env.tiller_home.join("state.db")).and_then(
            |mut store| {
                store
                    .conn()
                    .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
                handoff::signal_compactions(&mut store, Timestamp::now())
            },
        );
    }
    if let Some(output) = output {
        println!("{output}");
    }
}
