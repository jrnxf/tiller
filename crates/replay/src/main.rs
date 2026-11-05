//! `tiller-replay`: replays a Firstmate export bundle's asks in a sandbox
//! and audits it (`run`), then compares the replay with Firstmate and has
//! the pinned judge classify each difference (`judge`). Prints counts
//! only; the records, the audit and the report go to `--out`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use tiller_core::Timestamp;
use tiller_core::import::Bundle;
use tiller_replay::Orchestrator;
use tiller_replay::baseline;
use tiller_replay::claude::{Claude, Session};
use tiller_replay::compare::{self, Cards, Replayed};
use tiller_replay::harness::{self, Options};
use tiller_replay::judge::{self, Case, JUDGE_EFFORT, JUDGE_MODEL, Logged, RUBRIC};
use tiller_replay::orchestrator::External;
use tiller_replay::report::{self, Header};

#[derive(Parser)]
#[command(about = "Replay a bundle's asks in a sandbox and judge the differences")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Replay every ask through intake and an orchestrator, and audit it.
    Run(RunArgs),
    /// Compare a replay with Firstmate, judge each difference and write
    /// the report.
    Judge(JudgeArgs),
}

#[derive(Args)]
struct RunArgs {
    /// The export bundle directory.
    bundle: PathBuf,
    /// Where decisions.jsonl, audit.json and turns.jsonl go.
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
    #[arg(long, conflicts_with = "claude")]
    orchestrator: Option<PathBuf>,
    /// Decide each turn with the packaged orchestrator role on `claude -p`.
    #[arg(long)]
    claude: bool,
    /// The Claude Code program `--claude` and `judge` run.
    #[arg(long, default_value = "claude")]
    claude_program: PathBuf,
    /// The orchestrator's model (default: the role's).
    #[arg(long, requires = "claude")]
    model: Option<String>,
    /// The orchestrator's effort (default: the role's).
    #[arg(long, requires = "claude")]
    effort: Option<String>,
}

#[derive(Args)]
struct JudgeArgs {
    /// The export bundle directory the replay ran.
    bundle: PathBuf,
    /// The replay's `--out`: judgments.jsonl and report.md go here too.
    #[arg(long)]
    out: PathBuf,
    /// The Claude Code program the judge runs on.
    #[arg(long, default_value = "claude")]
    claude_program: PathBuf,
    /// Write the report from the verdicts already judged, calling no model.
    #[arg(long)]
    no_judge: bool,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Run(args) => run(args),
        Command::Judge(args) => judge(args),
    }
}

fn run(args: RunArgs) -> anyhow::Result<()> {
    let dispatch = match &args.config {
        Some(path) => {
            let text =
                fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
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
    fs::create_dir_all(&args.out)?;
    // A new replay voids what the judge read from or wrote for an earlier
    // one: it reads this replay's turns only and resumes no stale verdict.
    for stale in [
        "turns.jsonl",
        "differences.json",
        "judgments.jsonl",
        "report.md",
    ] {
        match fs::remove_file(args.out.join(stale)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    // What decided, for the report to name.
    let decider = if args.claude {
        json!({"kind": "claude"})
    } else if let Some(program) = &args.orchestrator {
        let name = program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        json!({"kind": "external", "program": name})
    } else {
        json!({"kind": "recorded"})
    };
    fs::write(
        args.out.join("orchestrator.json"),
        serde_json::to_vec_pretty(&decider)?,
    )?;
    let turns = args.out.join("turns.jsonl");
    let mut orchestrator: Option<Box<dyn Orchestrator>> = if args.claude {
        Some(Box::new(Claude::new(
            args.claude_program.clone(),
            args.model.clone(),
            args.effort.clone(),
            temp.path().join("claude"),
            turns,
        )?))
    } else {
        args.orchestrator.clone().map(|program| {
            Box::new(External {
                program,
                args: Vec::new(),
            }) as Box<dyn Orchestrator>
        })
    };
    let options = Options {
        bundle: args.bundle,
        sandbox,
        out: args.out,
        dispatch,
        real_home: args.real_home,
    };
    let summary = harness::run(
        &options,
        orchestrator
            .as_mut()
            .map(|o| &mut **o as &mut dyn Orchestrator),
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

fn jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<Vec<T>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(n, l)| {
            serde_json::from_str(l).with_context(|| format!("{} line {}", path.display(), n + 1))
        })
        .collect()
}

fn judge(args: JudgeArgs) -> anyhow::Result<()> {
    let bundle = Bundle::read(&args.bundle)?;
    let asks = baseline::read(&args.bundle.join("baseline").join("replay.jsonl"))?;
    let records: Vec<Replayed> = jsonl(&args.out.join("decisions.jsonl"))?;
    let turns_path = args.out.join("turns.jsonl");
    let turns: Vec<Value> = if turns_path.exists() {
        jsonl(&turns_path)?
    } else {
        Vec::new()
    };
    let logged: HashMap<usize, Logged> = turns
        .iter()
        .filter_map(|t| serde_json::from_value::<Logged>(t.clone()).ok())
        .map(|t| (t.index, t))
        .collect();

    let differences = compare::compare(&asks, &records, &Cards::new(&bundle));
    fs::write(
        args.out.join("differences.json"),
        serde_json::to_vec_pretty(&differences)?,
    )?;
    let cards = judge::bundle_cards(&bundle);
    let cases: Vec<Case> = records
        .iter()
        .filter(|record| record.unanswered.is_none())
        .filter_map(|record| {
            let ask = asks.get(record.index)?;
            Some(Case {
                ask,
                project: &record.project,
                record,
                turn: logged.get(&record.index),
                differences: differences
                    .iter()
                    .filter(|d| d.index == record.index)
                    .collect(),
                cards: &cards,
            })
        })
        .collect();
    let log = args.out.join("judgments.jsonl");
    let judged = if args.no_judge {
        judge::read(&log)?
    } else {
        let empty = tempfile::Builder::new().prefix("tiller-judge-").tempdir()?;
        let session = Session {
            program: args.claude_program,
            model: JUDGE_MODEL.into(),
            effort: JUDGE_EFFORT.into(),
            system: RUBRIC.into(),
            workdir: empty.path().to_path_buf(),
        };
        judge::judge_all(&session, &cases, &log)?
    };

    let cost = |rows: &[Value]| {
        rows.iter()
            .filter_map(|r| r["cost_usd"].as_f64())
            .sum::<f64>()
    };
    let models = |rows: &[Value]| {
        let mut all: Vec<String> = rows
            .iter()
            .filter_map(|r| r["models"].as_array())
            .flatten()
            .filter_map(|m| m.as_str().map(str::to_owned))
            .collect();
        all.sort();
        all.dedup();
        all.join(", ")
    };
    let judged_rows: Vec<Value> = judged.values().map(|j| json!(j)).collect();
    let decider_path = args.out.join("orchestrator.json");
    let decider: Value = if decider_path.exists() {
        serde_json::from_slice(&fs::read(&decider_path)?)?
    } else if turns.is_empty() {
        json!({"kind": "recorded"})
    } else {
        json!({"kind": "claude"})
    };
    let model = decider["kind"] == "claude";
    let orchestrator = match decider["kind"].as_str() {
        Some("claude") => format!("orchestrator role on `claude -p` ({})", models(&turns)),
        Some("external") => format!(
            "the external program `{}`",
            decider["program"].as_str().unwrap_or("?")
        ),
        _ => "the recorded orchestrator (no model)".to_owned(),
    };
    let mut notes = Vec::new();
    for record in &records {
        if let Some(why) = &record.unanswered {
            notes.push(format!(
                "Ask {} went unanswered and is not compared: {why}",
                record.index
            ));
        }
    }
    if records.len() != asks.len() {
        notes.push(format!(
            "The replay recorded {} of {} asks.",
            records.len(),
            asks.len()
        ));
    }
    let header = Header {
        bundle: args
            .bundle
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        asks: asks.len(),
        orchestrator,
        model,
        judge: format!(
            "{JUDGE_MODEL} at {JUDGE_EFFORT} effort (reported: {})",
            models(&judged_rows)
        ),
        rubric_sha256: judge::rubric_sha256(),
        orchestrator_cost_usd: cost(&turns),
        judge_cost_usd: cost(&judged_rows),
        notes,
    };
    let text = report::render(&header, &differences, &judged, Timestamp::now());
    let path = args.out.join("report.md");
    fs::write(&path, text)?;
    let worse = judged
        .values()
        .flat_map(|j| &j.judgments)
        .filter(|j| j.verdict == judge::Verdict::Worse)
        .count();
    println!(
        "{}",
        json!({
            "differences": differences.len(),
            "judged_asks": judged.len(),
            "worse": worse,
            "report": path,
        })
    );
    Ok(())
}
