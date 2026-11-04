//! `tiller-replay`: replays a Firstmate export bundle's asks in a sandbox
//! and audits it. Prints counts only; the records and the audit go to
//! `--out`.

use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::Parser;
use tiller_replay::Orchestrator;
use tiller_replay::harness::{self, Options};
use tiller_replay::orchestrator::External;

#[derive(Parser)]
#[command(about = "Replay a bundle's asks through intake and the orchestrator in a sandbox")]
struct Args {
    /// The export bundle directory.
    bundle: PathBuf,
    /// Where decisions.jsonl and audit.json go.
    #[arg(long)]
    out: PathBuf,
    /// An empty directory to build the sandbox in (default: a temporary one,
    /// removed afterwards).
    #[arg(long)]
    sandbox: Option<PathBuf>,
    /// A TOML file whose [dispatch] table spawn resolves models with.
    #[arg(long)]
    config: Option<PathBuf>,
    /// A real Tiller home to prove untouched, opened read-only.
    #[arg(long)]
    real_home: Option<PathBuf>,
    /// A program that decides each turn (turn JSON on stdin, a decision
    /// list on stdout) in place of the recorded orchestrator.
    #[arg(long)]
    orchestrator: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let dispatch = match &args.config {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let table: toml::Table = text
                .parse()
                .with_context(|| format!("parsing {}", path.display()))?;
            table.get("dispatch").cloned()
        }
        None => None,
    };
    let temp = tempfile::Builder::new()
        .prefix("tiller-replay-")
        .tempdir()?;
    let sandbox = args
        .sandbox
        .clone()
        .unwrap_or_else(|| temp.path().join("sandbox"));
    let options = Options {
        bundle: args.bundle,
        sandbox,
        out: args.out,
        dispatch,
        real_home: args.real_home,
    };
    let mut external = args.orchestrator.map(|program| External {
        program,
        args: Vec::new(),
    });
    let summary = harness::run(
        &options,
        external.as_mut().map(|e| e as &mut dyn Orchestrator),
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    if !summary.audit.clean() {
        bail!(
            "the sandbox audit found {} side effect(s)",
            summary.audit.findings.len()
        );
    }
    Ok(())
}
