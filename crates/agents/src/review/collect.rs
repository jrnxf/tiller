//! Reads a pass's units from git and the `prs` table (section 4 steps 1-3):
//! the first-parent commits of `base..head`, each mapped to its PR through
//! `prs.merge_sha`, else through the forge, else a direct commit; and each
//! unit's counted files, leaving out lockfiles, generated files and binaries.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use tiller_core::domain::{Delivery, Project};

use super::{Excluded, FileChange, Unit};
use crate::land::direct::fetch_default;
use crate::land::forge::Identity;
use crate::spawn::config::SpawnConfig;

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("git {args}: {why}")]
    Git { args: String, why: String },
    #[error("forge read failed: {0}")]
    Forge(String),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
}

/// One PR as the forge reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullInfo {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
}

/// The forge side of the PR mapping.
pub trait PullLookup {
    /// The PR merged into `base` that `sha` belongs to, if any.
    fn by_commit(&self, repo: &str, base: &str, sha: &str)
    -> Result<Option<PullInfo>, LookupError>;
    fn by_number(&self, repo: &str, number: i64) -> Result<Option<PullInfo>, LookupError>;
}

/// No forge: a project with no `repo`, or a test.
pub struct NoLookup;

impl PullLookup for NoLookup {
    fn by_commit(&self, _: &str, _: &str, _: &str) -> Result<Option<PullInfo>, LookupError> {
        Ok(None)
    }

    fn by_number(&self, _: &str, _: i64) -> Result<Option<PullInfo>, LookupError> {
        Ok(None)
    }
}

/// GitHub through `gh api`, read-only, like [`crate::land::forge::GhForge`].
pub struct GhLookup {
    program: String,
}

impl GhLookup {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn json(&self, path: &str) -> Result<Value, LookupError> {
        let output = Command::new(&self.program)
            .arg("api")
            .arg(path)
            .env("GH_PROMPT_DISABLED", "1")
            .output()
            .map_err(|error| LookupError::Forge(format!("run {}: {error}", self.program)))?;
        if !output.status.success() {
            return Err(LookupError::Forge(format!(
                "{path}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| LookupError::Forge(format!("{path}: {error}")))
    }
}

fn pull_info(pull: &Value) -> Option<PullInfo> {
    let text = |key: &str| {
        pull.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some(PullInfo {
        number: pull.get("number")?.as_i64()?,
        title: text("title"),
        body: text("body"),
        url: text("html_url"),
    })
}

impl PullLookup for GhLookup {
    fn by_commit(
        &self,
        repo: &str,
        base: &str,
        sha: &str,
    ) -> Result<Option<PullInfo>, LookupError> {
        let pulls = self.json(&format!("repos/{repo}/commits/{sha}/pulls"))?;
        Ok(pulls.as_array().into_iter().flatten().find_map(|pull| {
            let merged = pull.get("merged_at").is_some_and(|at| !at.is_null());
            let into_base = pull.pointer("/base/ref").and_then(Value::as_str) == Some(base);
            if merged && into_base {
                pull_info(pull)
            } else {
                None
            }
        }))
    }

    fn by_number(&self, repo: &str, number: i64) -> Result<Option<PullInfo>, LookupError> {
        Ok(pull_info(
            &self.json(&format!("repos/{repo}/pulls/{number}"))?,
        ))
    }
}

fn git(checkout: &Path, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, LookupError> {
    let failed = |why: String| LookupError::Git {
        args: args.join(" "),
        why,
    };
    let mut child = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| failed(error.to_string()))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input)
            .map_err(|error| failed(error.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| failed(error.to_string()))?;
    if !output.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(output.stdout)
}

/// Lockfiles and generated files by name: always left out.
fn excluded_by_name(path: &str) -> Option<&'static str> {
    const LOCKFILES: &[&str] = &[
        "Cargo.lock",
        "bun.lock",
        "bun.lockb",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "flake.lock",
        "Gemfile.lock",
        "poetry.lock",
        "uv.lock",
        "composer.lock",
        "go.sum",
    ];
    let name = path.rsplit('/').next().unwrap_or(path);
    if LOCKFILES.contains(&name) {
        Some("lockfile")
    } else if name == "bun.nix" || name.contains(".gen.") {
        Some("generated")
    } else {
        None
    }
}

/// The paths the repo marks `linguist-generated` at `head`.
fn linguist_generated(
    checkout: &Path,
    head: &str,
    paths: &BTreeSet<String>,
) -> Result<BTreeSet<String>, LookupError> {
    if paths.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut input = Vec::new();
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let source = format!("--source={head}");
    let out = git(
        checkout,
        &["check-attr", "-z", "--stdin", &source, "linguist-generated"],
        Some(&input),
    )?;
    let fields: Vec<&[u8]> = out.split(|&b| b == 0).collect();
    Ok(fields
        .chunks(3)
        .filter(|triple| triple.len() == 3 && matches!(triple[2], b"set" | b"true"))
        .map(|triple| String::from_utf8_lossy(triple[0]).into_owned())
        .collect())
}

/// One commit's changed files: `(path, lines)`, `None` lines for a binary.
fn numstat(checkout: &Path, sha: &str) -> Result<Vec<(String, Option<u64>)>, LookupError> {
    let parent = format!("{sha}^1");
    let out = git(
        checkout,
        &["diff", "--numstat", "-z", "--no-renames", &parent, sha],
        None,
    )?;
    let mut files = Vec::new();
    for record in out.split(|&b| b == 0).filter(|record| !record.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let lines = added
            .parse::<u64>()
            .ok()
            .zip(deleted.parse::<u64>().ok())
            .map(|(a, d)| a + d);
        files.push((path.to_owned(), lines));
    }
    Ok(files)
}

/// The Tiller PR whose merge commit is `sha`: number, URL, card and title.
fn tiller_pr(
    conn: &Connection,
    project: &str,
    sha: &str,
) -> Result<Option<(i64, String, String, String)>, LookupError> {
    Ok(conn
        .query_row(
            "SELECT p.number, p.url, p.card_id, c.title FROM prs p JOIN cards c ON c.id = p.card_id
             WHERE p.project_id = ?1 AND p.merge_sha = ?2",
            params![project, sha],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?)
}

fn card_of(conn: &Connection, project: &str, number: i64) -> Result<Option<String>, LookupError> {
    Ok(conn
        .query_row(
            "SELECT card_id FROM prs WHERE project_id = ?1 AND number = ?2",
            params![project, number],
            |row| row.get(0),
        )
        .optional()?)
}

/// Why a project's default-branch head could not be read.
#[derive(Debug, thiserror::Error)]
pub enum HeadError {
    #[error("no checkout of {0} is configured")]
    NoCheckout(String),
    #[error("read the head of {branch}: {why}")]
    Read { branch: String, why: String },
}

/// A project's configured checkout and its default-branch head, read just
/// now. `tiller project stage`, `tiller review` and the board's stage switch
/// all read it here.
pub fn checkout_head(
    config: &SpawnConfig,
    project: &Project,
) -> Result<(PathBuf, String), HeadError> {
    let checkout = config
        .checkouts
        .get(&project.id)
        .ok_or_else(|| HeadError::NoCheckout(project.id.clone()))?;
    let head = default_head(checkout, project).map_err(|why| HeadError::Read {
        branch: project.default_branch.clone(),
        why,
    })?;
    Ok((checkout.clone(), head))
}

/// The default-branch head, read just now: fetched from the remote, or for a
/// `local-only` project the local branch.
pub fn default_head(checkout: &Path, project: &Project) -> Result<String, String> {
    if project.delivery == Delivery::LocalOnly {
        let branch = format!("refs/heads/{}^{{commit}}", project.default_branch);
        let out = git(checkout, &["rev-parse", "--verify", &branch], None)
            .map_err(|error| error.to_string())?;
        return Ok(String::from_utf8_lossy(&out).trim().to_owned());
    }
    let remote = project.remote.as_deref().unwrap_or("origin");
    fetch_default(checkout, remote, &project.default_branch, &Identity::Owner)
}

/// The units of `base..head` in merge order, and the paths left out.
pub fn collect(
    conn: &Connection,
    project: &Project,
    checkout: &Path,
    base: &str,
    head: &str,
    lookup: &dyn PullLookup,
) -> Result<(Vec<Unit>, Vec<Excluded>), LookupError> {
    let range = format!("{base}..{head}");
    let out = git(
        checkout,
        &["rev-list", "--first-parent", "--reverse", &range],
        None,
    )?;
    let commits: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(str::to_owned)
        .collect();
    let repo = project.repo.as_deref();

    let mut units: Vec<Unit> = Vec::new();
    let mut by_key: BTreeMap<String, usize> = BTreeMap::new();
    let mut raw: Vec<Vec<(String, Option<u64>)>> = Vec::new();
    for sha in &commits {
        let subject =
            String::from_utf8_lossy(&git(checkout, &["log", "-1", "--format=%s", sha], None)?)
                .trim()
                .to_owned();
        let unit = match tiller_pr(conn, &project.id, sha)? {
            Some((number, url, card, title)) => {
                let pull = match repo {
                    Some(repo) => lookup.by_number(repo, number)?,
                    None => None,
                };
                Unit {
                    key: format!("#{number}"),
                    number: Some(number),
                    title: pull.as_ref().map_or(title, |pull| pull.title.clone()),
                    body: pull.map(|pull| pull.body).unwrap_or_default(),
                    url: Some(url),
                    card_id: Some(card),
                    commits: Vec::new(),
                    files: Vec::new(),
                }
            }
            None => match repo
                .map(|repo| lookup.by_commit(repo, &project.default_branch, sha))
                .transpose()?
                .flatten()
            {
                Some(pull) => Unit {
                    key: format!("#{}", pull.number),
                    number: Some(pull.number),
                    card_id: card_of(conn, &project.id, pull.number)?,
                    title: pull.title,
                    body: pull.body,
                    url: Some(pull.url),
                    commits: Vec::new(),
                    files: Vec::new(),
                },
                None => Unit {
                    key: sha.get(..12).unwrap_or(sha).to_owned(),
                    number: None,
                    title: subject,
                    body: String::new(),
                    url: None,
                    card_id: None,
                    commits: Vec::new(),
                    files: Vec::new(),
                },
            },
        };
        // A rebase merge puts several first-parent commits under one PR.
        let at = match by_key.get(&unit.key) {
            Some(&at) => at,
            None => {
                by_key.insert(unit.key.clone(), units.len());
                units.push(unit);
                raw.push(Vec::new());
                units.len() - 1
            }
        };
        units[at].commits.push(sha.clone());
        raw[at].extend(numstat(checkout, sha)?);
    }

    let paths: BTreeSet<String> = raw.iter().flatten().map(|(path, _)| path.clone()).collect();
    let generated = linguist_generated(checkout, head, &paths)?;
    let mut excluded: BTreeMap<String, &'static str> = BTreeMap::new();
    for (unit, files) in units.iter_mut().zip(raw) {
        let mut counted: BTreeMap<String, u64> = BTreeMap::new();
        for (path, lines) in files {
            let why = match lines {
                None => Some("binary"),
                Some(_) if generated.contains(&path) => Some("linguist-generated"),
                Some(_) => excluded_by_name(&path),
            };
            match (why, lines) {
                (Some(why), _) => {
                    excluded.entry(path).or_insert(why);
                }
                (None, Some(lines)) => *counted.entry(path).or_default() += lines,
                (None, None) => {}
            }
        }
        unit.files = counted
            .into_iter()
            .map(|(path, lines)| FileChange { path, lines })
            .collect();
    }
    let excluded = excluded
        .into_iter()
        .map(|(path, why)| Excluded {
            path,
            why: why.to_owned(),
        })
        .collect();
    Ok((units, excluded))
}
