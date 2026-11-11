//! The holdout eval of a change set (`docs/design/learning-loop.md`, section
//! 7): the candidate text its members leave, the plan of model runs the
//! board shows before it starts one, and the run of `evals/run.sh` that
//! `tiller learn eval` and the board's "Run holdout eval..." share.
//!
//! The holdout cases are only counted here: their frontmatter gives each
//! case's run count and each grader's type. Nothing reads their text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tiller_core::domain::{ChangeSet, ChangeSetMember};
use tiller_core::state::learning::EvalResult;
use tiller_core::state::list;
use tiller_core::state::memory::MEMORY_REPO;

/// Runs per case when a case's frontmatter names none (`evals/README.md`).
pub const DEFAULT_RUNS: u64 = 3;

#[derive(Debug, thiserror::Error)]
pub enum HoldoutError {
    /// The request or the config is wrong; nothing ran.
    #[error("{0}")]
    Usage(String),
    /// A precondition failed; nothing ran.
    #[error("{0}")]
    Refused(String),
    /// The runner could not run or left no clean result; nothing recorded.
    #[error("{0}")]
    Unavailable(String),
    #[error(transparent)]
    Core(#[from] tiller_core::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, HoldoutError>;

/// What one holdout run costs in model runs: every case at its own run
/// count, and the judge once per run for each of its `llm` graders.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub cases: u64,
    /// Case runs: the role's model, once per run of each case.
    pub case_runs: u64,
    /// Judge runs: one per run of each case for each `llm` grader.
    pub judge_runs: u64,
}

impl Plan {
    pub fn model_runs(&self) -> u64 {
        self.case_runs + self.judge_runs
    }
}

/// The checkout's `evals/` directory.
fn evals(evals_dir: &Path) -> PathBuf {
    evals_dir.join("evals")
}

/// The `key: value` lines of a markdown file's frontmatter.
fn frontmatter(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let mut out = BTreeMap::new();
    if lines.next().map(str::trim) != Some("---") {
        return Ok(out);
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            out.insert(
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            );
        }
    }
    Ok(out)
}

/// The holdout split's plan, from the case and grader frontmatter under
/// `<evals_dir>/evals/cases/holdout`.
pub fn plan(evals_dir: &Path) -> Result<Plan> {
    let root = evals(evals_dir).join("cases/holdout");
    let entries = std::fs::read_dir(&root)
        .map_err(|error| HoldoutError::Unavailable(format!("{}: {error}", root.display())))?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("prompt.md").is_file())
        .collect();
    dirs.sort();
    let mut plan = Plan::default();
    for dir in dirs {
        let runs = match frontmatter(&dir.join("prompt.md"))?.get("runs") {
            Some(runs) => runs.parse::<u64>().map_err(|_| {
                HoldoutError::Unavailable(format!(
                    "{}: runs is not a number: {runs}",
                    dir.join("prompt.md").display()
                ))
            })?,
            None => DEFAULT_RUNS,
        };
        let mut judged = 0;
        if let Ok(graders) = std::fs::read_dir(dir.join("graders")) {
            for grader in graders.filter_map(std::result::Result::ok) {
                let path = grader.path();
                if path.extension().is_some_and(|e| e == "md")
                    && frontmatter(&path)?.get("type").map(String::as_str) == Some("llm")
                {
                    judged += 1;
                }
            }
        }
        plan.cases += 1;
        plan.case_runs += runs;
        plan.judge_runs += runs * judged;
    }
    if plan.cases == 0 {
        return Err(HoldoutError::Unavailable(format!(
            "{} holds no holdout cases",
            root.display()
        )));
    }
    Ok(plan)
}

/// `pins.env` as `KEY=value` lines, for the versions a run depends on.
pub fn pins(evals_dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(evals(evals_dir).join("pins.env"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((
                key.trim().to_owned(),
                value.trim().trim_matches('"').to_owned(),
            ))
        })
        .collect()
}

/// What a `claude plugin eval --json` result says (`evals/run.sh`).
pub fn parse_result(path: &Path) -> Result<EvalResult> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| HoldoutError::Usage(format!("{}: {error}", path.display())))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| HoldoutError::Usage(format!("{} is not JSON: {error}", path.display())))?;
    let aggregates = &value["aggregates"];
    let cases_total = aggregates["casesTotal"].as_i64().ok_or_else(|| {
        HoldoutError::Usage(format!("{} has no aggregates.casesTotal", path.display()))
    })?;
    Ok(EvalResult {
        members_sha256: None,
        candidate_sha256: None,
        cases_total,
        cases_passed: aggregates["casesPassed"].as_i64().unwrap_or(0),
        overall_score: aggregates["overallScore"].as_f64(),
        cost_usd: value["costUsd"].as_f64(),
        partial: value["partial"].as_bool().unwrap_or(false),
        claude_version: None,
        judge_model: value["judgeModel"].as_str().map(str::to_owned),
        result_path: Some(path.to_string_lossy().into_owned()),
    })
}

/// The git repo a source names. Tiller's own documents are recorded under
/// the literal `~/.tiller/memory`; they live in `<home>/memory`.
fn repo_dir(repo: &str, home: &Path) -> PathBuf {
    if repo == MEMORY_REPO {
        home.join("memory")
    } else {
        PathBuf::from(repo)
    }
}

/// The members' target files with their diffs applied, under a heading per
/// source, the way the launcher renders approved memory (learning-loop.md,
/// section 7). `home` is Tiller's home, where its memory repo is.
pub fn render_candidate(
    conn: &rusqlite::Connection,
    set: &ChangeSet,
    home: &Path,
) -> Result<String> {
    let members: Vec<ChangeSetMember> =
        list(conn, "change_set_id = ?1 ORDER BY added_at", &[&set.id])?;
    if members.is_empty() {
        return Err(HoldoutError::Refused(format!(
            "change set {} has no members to render",
            set.name
        )));
    }
    // Per target: (repo, path, base blob, diffs in order).
    let mut targets: Vec<(String, String, String, Vec<String>)> = Vec::new();
    for member in &members {
        let (repo, path, base_blob, diff): (Option<String>, Option<String>, String, String) =
            conn.query_row(
                "SELECT s.canonical_repo, s.canonical_path, p.base_blob, p.diff
                 FROM proposals p JOIN memory_sources s ON s.id = p.target_source_id WHERE p.id = ?1",
                [&member.proposal_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(tiller_core::Error::from)?;
        let (Some(repo), Some(path)) = (repo, path) else {
            return Err(HoldoutError::Refused(format!(
                "proposal {} targets a foreign source; it cannot be rendered",
                member.proposal_id
            )));
        };
        match targets.iter_mut().find(|t| t.0 == repo && t.1 == path) {
            Some(target) => target.3.push(diff),
            None => targets.push((repo, path, base_blob, vec![diff])),
        }
    }
    let work = tempfile::tempdir()?;
    let mut out = String::new();
    for (repo, path, base_blob, diffs) in targets {
        let base = if base_blob == tiller_core::import::EMPTY_BLOB {
            String::new()
        } else {
            let dir = repo_dir(&repo, home);
            let output = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["cat-file", "-p", &base_blob])
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(HoldoutError::Refused(format!(
                    "git cat-file {base_blob} in {repo} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            String::from_utf8_lossy(&output.stdout).into_owned()
        };
        let file = work.path().join(&path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, base)?;
        for (n, diff) in diffs.iter().enumerate() {
            let patch = work.path().join(format!(".patch-{n}"));
            std::fs::write(&patch, diff)?;
            let output = Command::new("git")
                .args(["apply", "--whitespace=nowarn"])
                .arg(&patch)
                .current_dir(work.path())
                .stdin(Stdio::null())
                .output()?;
            if !output.status.success() {
                return Err(HoldoutError::Refused(format!(
                    "a member's diff does not apply to {path}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
        }
        let rendered = std::fs::read_to_string(&file)?;
        out.push_str(&format!("## {path}\n\n{}\n\n", rendered.trim_end()));
    }
    Ok(out)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Runs `<evals_dir>/evals/run.sh --split holdout` on `candidate`, its
/// results under `out`, and returns the result to record, bound to
/// `members` (the set's members hash, taken before the run) and to the
/// candidate's hash. A runner that cannot start, exits non-zero or leaves
/// no `sealed/result.json` is `Unavailable`, and nothing is to be recorded.
pub fn run(evals_dir: &Path, out: &Path, candidate: &str, members: String) -> Result<EvalResult> {
    std::fs::create_dir_all(out)?;
    // An earlier run's sealed result must not be read as this one's.
    let sealed = out.join("sealed");
    if sealed.exists() {
        std::fs::remove_dir_all(&sealed)?;
    }
    let candidate_path = out.join("candidate.md");
    std::fs::write(&candidate_path, candidate)?;
    let runner = evals(evals_dir).join("run.sh");
    let status = Command::new(&runner)
        .args(["--split", "holdout", "--candidate"])
        .arg(&candidate_path)
        .arg("--out")
        .arg(out)
        .current_dir(evals_dir)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| {
            HoldoutError::Unavailable(format!("cannot run {}: {error}", runner.display()))
        })?;
    let result_path = out.join("sealed/result.json");
    if !status.success() || !result_path.exists() {
        // run.sh exits non-zero when a case failed to load even when the
        // rest scored, so a result is recorded only from a clean run.
        return Err(HoldoutError::Unavailable(format!(
            "{} exited {}{}; nothing recorded",
            runner.display(),
            status.code().map_or_else(
                || "on a signal".to_owned(),
                |code| format!("with code {code}")
            ),
            if result_path.exists() {
                format!(" (its result is at {})", result_path.display())
            } else {
                " and left no sealed/result.json".to_owned()
            }
        )));
    }
    let mut result = parse_result(&result_path)?;
    result.members_sha256 = Some(members);
    result.candidate_sha256 = Some(sha256_hex(candidate.as_bytes()));
    let pins = pins(evals_dir);
    result.claude_version = pins.get("CLAUDE_CODE_VERSION").cloned();
    if result.judge_model.is_none() {
        result.judge_model = pins.get("JUDGE_MODEL").cloned();
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
