//! `tiller guard pretool --role <role>` (`authority.md` section 9): the
//! PreToolUse backstop. It reads the hook input on stdin and hands it to
//! `tiller-agents`; a deny exits 2 with the decision on stderr, an allow exits
//! 0 and prints nothing, so the normal permission flow still runs. An allowed
//! bare `tiller ...` exits 0 with the same call naming the packaged path,
//! the one command the sandbox exempts.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use tiller_agents::guards::{self, Env};
use tiller_agents::roles::Role;

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// Decide one PreToolUse call (reads the hook input on stdin)
    Pretool(PretoolArgs),
}

#[derive(clap::Args)]
pub struct PretoolArgs {
    /// The fleet role whose session made the call
    #[arg(long)]
    role: String,
    /// The project checkouts root, for the orchestrator's persistent-cd rule
    #[arg(long)]
    checkouts: Option<PathBuf>,
    /// The worktree pool root, for the orchestrator's persistent-cd rule
    #[arg(long)]
    pool: Option<PathBuf>,
}

pub fn run(cmd: &Cmd) -> ExitCode {
    let Cmd::Pretool(args) = cmd;
    // An unknown role is a broken fragment: allow, as for any broken hook;
    // the deny rules and the sandbox still hold, and spawn checks the
    // fragment (G35).
    let Some(role) = Role::parse(&args.role) else {
        return ExitCode::SUCCESS;
    };
    let mut input = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut input);
    let user_home = std::env::var_os("HOME").map(PathBuf::from);
    let Some(tiller_home) = std::env::var_os("TILLER_HOME")
        .map(PathBuf::from)
        .or_else(|| user_home.as_ref().map(|home| home.join(".tiller")))
    else {
        return ExitCode::SUCCESS;
    };
    let set = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let env = Env {
        tiller_home,
        user_home,
        tiller_session_id: set("TILLER_SESSION_ID"),
        tiller_run_id: set("TILLER_RUN_ID"),
        packaged_tiller: crate::stable::tiller_path().ok(),
        protected_roots: args.checkouts.iter().chain(&args.pool).cloned().collect(),
    };
    if let Some(denial) = guards::pretool(role, &input, &env) {
        eprintln!("{}", guards::render_deny(&denial));
        return ExitCode::from(2);
    }
    let payload: serde_json::Value = serde_json::from_slice(&input).unwrap_or_default();
    let updated = payload
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .zip(payload.get("tool_input"))
        .and_then(|(tool, tool_input)| {
            guards::packaged_form(tool, tool_input, env.packaged_tiller.as_deref())
        });
    if let Some(updated) = updated {
        println!("{}", guards::render_rewrite(&updated));
    }
    ExitCode::SUCCESS
}
