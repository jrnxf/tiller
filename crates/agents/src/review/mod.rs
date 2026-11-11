//! The chunked pre-launch review (`docs/design/pre-launch-validation.md`
//! section 4): the plan that cuts a pass's commit range into bounded chunks,
//! and the briefs of the chunk runs and the summary run.
//!
//! [`plan`] is pure and deterministic: the same units in the same merge order
//! give the same chunks. [`collect`] reads the units from git and the `prs`
//! table, with the forge behind [`PullLookup`].

mod collect;

pub use collect::{
    GhLookup, HeadError, LookupError, NoLookup, PullInfo, PullLookup, checkout_head, collect,
    default_head,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use tiller_core::domain::{Project, ReviewPass};

/// A chunk aims at this many changed lines.
pub const TARGET_LINES: u64 = 1_500;
/// No chunk goes over this, except a lone file that is bigger on its own.
pub const HARD_LINES: u64 = 2_000;
/// At most this many PRs (units) in one chunk.
pub const MAX_UNITS: usize = 15;
/// A chunk session stops past about this much context and lists what it
/// did not cover.
const CONTEXT_STOP: &str = "150k";
/// A PR body longer than this is cut in the brief.
const BODY_CHARS: usize = 4_000;

/// One changed file of a unit, with its counted lines (added plus deleted).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub lines: u64,
}

/// One PR, or one direct commit, in merge order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    /// `#<number>` for a PR, the short SHA for a direct commit.
    pub key: String,
    pub number: Option<i64>,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    pub url: Option<String>,
    /// The Tiller card of the PR, when Tiller knows it.
    pub card_id: Option<String>,
    /// The first-parent commits of the unit, oldest first.
    pub commits: Vec<String>,
    /// Counted files, sorted by path.
    pub files: Vec<FileChange>,
}

impl Unit {
    pub fn lines(&self) -> u64 {
        self.files.iter().map(|file| file.lines).sum()
    }
}

/// A path left out of the line count, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub path: String,
    pub why: String,
}

/// One unit's share of a chunk: the whole unit, or one directory slice of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    pub unit: String,
    pub files: Vec<String>,
    pub lines: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub lines: u64,
    pub parts: Vec<Part>,
    /// A lone file over the hard cap; the report flags it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub oversized: bool,
}

/// A file more than one chunk changed: the summary run reads it at the head
/// for cross-chunk breakage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seam {
    pub path: String,
    /// 1-based chunk numbers.
    pub chunks: Vec<usize>,
}

/// The stored plan of a pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub base_sha: String,
    pub head_sha: String,
    pub units: Vec<Unit>,
    pub chunks: Vec<Chunk>,
    pub excluded: Vec<Excluded>,
    pub seams: Vec<Seam>,
}

impl Plan {
    /// The plan of `units` (merge order) over `base..head`.
    pub fn new(base_sha: &str, head_sha: &str, units: Vec<Unit>, excluded: Vec<Excluded>) -> Self {
        let chunks = plan(&units);
        let seams = seams(&chunks);
        Self {
            base_sha: base_sha.to_owned(),
            head_sha: head_sha.to_owned(),
            units,
            chunks,
            excluded,
            seams,
        }
    }

    fn unit(&self, key: &str) -> Option<&Unit> {
        self.units.iter().find(|unit| unit.key == key)
    }
}

/// A packable piece: a whole group, a whole unit, or a slice of one unit.
struct Item {
    parts: Vec<Part>,
    lines: u64,
}

impl Item {
    fn whole(unit: &Unit) -> Self {
        Self {
            lines: unit.lines(),
            parts: vec![Part {
                unit: unit.key.clone(),
                files: unit.files.iter().map(|file| file.path.clone()).collect(),
                lines: unit.lines(),
            }],
        }
    }

    fn slice(unit: &Unit, files: &[&FileChange]) -> Self {
        let lines = files.iter().map(|file| file.lines).sum();
        Self {
            lines,
            parts: vec![Part {
                unit: unit.key.clone(),
                files: files.iter().map(|file| file.path.clone()).collect(),
                lines,
            }],
        }
    }

    fn units(&self) -> usize {
        self.parts
            .iter()
            .map(|part| &part.unit)
            .collect::<BTreeSet<_>>()
            .len()
    }
}

/// Cuts units (in merge order) into chunks. Units that change the same file
/// form a group; a group within the caps stays whole. A group over the caps
/// packs its units whole, and a unit over the hard cap is sliced by
/// directory, then by file. Units with no counted lines are in no chunk.
pub fn plan(units: &[Unit]) -> Vec<Chunk> {
    let mut items = Vec::new();
    for group in groups(units) {
        let members: Vec<&Unit> = group.iter().map(|&i| &units[i]).collect();
        let lines: u64 = members.iter().map(|unit| unit.lines()).sum();
        if lines <= HARD_LINES && members.len() <= MAX_UNITS {
            items.push(Item {
                lines,
                parts: members
                    .iter()
                    .flat_map(|unit| Item::whole(unit).parts)
                    .collect(),
            });
            continue;
        }
        for unit in members {
            if unit.lines() <= HARD_LINES {
                items.push(Item::whole(unit));
            } else {
                items.extend(slices(unit));
            }
        }
    }
    pack(items)
}

/// Groups of unit indexes that share a changed file (connected components),
/// each in merge order, ordered by their first unit. Units with no counted
/// lines are left out.
fn groups(units: &[Unit]) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..units.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut owner: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, unit) in units.iter().enumerate() {
        for file in unit.files.iter().filter(|file| file.lines > 0) {
            if let Some(&first) = owner.get(file.path.as_str()) {
                let (a, b) = (root(&mut parent, first), root(&mut parent, i));
                // The earlier unit stays the root, so groups keep merge order.
                parent[a.max(b)] = a.min(b);
            } else {
                owner.insert(&file.path, i);
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, unit) in units.iter().enumerate() {
        if unit.lines() > 0 {
            let r = root(&mut parent, i);
            groups.entry(r).or_default().push(i);
        }
    }
    groups.into_values().collect()
}

/// A unit over the hard cap, cut by directory into slices of at most the
/// target; a directory over the target is cut by file, and a file over the
/// target is a slice of its own.
fn slices(unit: &Unit) -> Vec<Item> {
    let mut dirs: BTreeMap<&str, Vec<&FileChange>> = BTreeMap::new();
    for file in &unit.files {
        let dir = file.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        dirs.entry(dir).or_default().push(file);
    }
    let mut out = Vec::new();
    let mut current: Vec<&FileChange> = Vec::new();
    let mut lines = 0;
    let flush = |current: &mut Vec<&FileChange>, lines: &mut u64, out: &mut Vec<Item>| {
        if !current.is_empty() {
            out.push(Item::slice(unit, current));
            current.clear();
            *lines = 0;
        }
    };
    for files in dirs.into_values() {
        let dir_lines: u64 = files.iter().map(|file| file.lines).sum();
        if dir_lines <= TARGET_LINES {
            if lines + dir_lines > TARGET_LINES {
                flush(&mut current, &mut lines, &mut out);
            }
            current.extend(files);
            lines += dir_lines;
            continue;
        }
        for file in files {
            if file.lines > TARGET_LINES {
                flush(&mut current, &mut lines, &mut out);
                out.push(Item::slice(unit, &[file]));
                continue;
            }
            if lines + file.lines > TARGET_LINES {
                flush(&mut current, &mut lines, &mut out);
            }
            current.push(file);
            lines += file.lines;
        }
    }
    flush(&mut current, &mut lines, &mut out);
    out
}

/// Packs items in order: an item joins the open chunk while the chunk stays
/// within the target and the unit cap, else it opens a new one.
fn pack(items: Vec<Item>) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    for item in items {
        let fits = chunks.last().is_some_and(|chunk| {
            let mut keys: BTreeSet<&String> = chunk.parts.iter().map(|part| &part.unit).collect();
            keys.extend(item.parts.iter().map(|part| &part.unit));
            !chunk.oversized && chunk.lines + item.lines <= TARGET_LINES && keys.len() <= MAX_UNITS
        });
        match chunks.last_mut() {
            Some(chunk) if fits => {
                chunk.lines += item.lines;
                chunk.parts.extend(item.parts);
            }
            _ => {
                let oversized = item.lines > HARD_LINES;
                debug_assert!(!oversized || item.units() == 1);
                chunks.push(Chunk {
                    lines: item.lines,
                    parts: item.parts,
                    oversized,
                });
            }
        }
    }
    chunks
}

/// Files changed in more than one chunk.
fn seams(chunks: &[Chunk]) -> Vec<Seam> {
    let mut seen: BTreeMap<&str, BTreeSet<usize>> = BTreeMap::new();
    for (i, chunk) in chunks.iter().enumerate() {
        for part in &chunk.parts {
            for path in &part.files {
                seen.entry(path).or_default().insert(i + 1);
            }
        }
    }
    seen.into_iter()
        .filter(|(_, chunks)| chunks.len() > 1)
        .map(|(path, chunks)| Seam {
            path: path.to_owned(),
            chunks: chunks.into_iter().collect(),
        })
        .collect()
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn unit_label(unit: &Unit) -> String {
    match (&unit.number, &unit.url) {
        (Some(number), Some(url)) => format!("PR #{number} {:?} ({url})", unit.title),
        (Some(number), None) => format!("PR #{number} {:?}", unit.title),
        _ => format!("direct commit {} {:?}", unit.key, unit.title),
    }
}

/// The one-line index of every unit in the pass.
fn index(plan: &Plan) -> String {
    let mut out = String::new();
    for unit in &plan.units {
        let files: Vec<&str> = unit.files.iter().map(|file| file.path.as_str()).collect();
        let _ = writeln!(
            out,
            "- {}: {}",
            unit_label(unit),
            if files.is_empty() {
                "no counted files".to_owned()
            } else {
                files.join(", ")
            }
        );
    }
    out
}

/// The brief of one chunk run (1-based `chunk`). `only` narrows it to the
/// files a cut chunk session listed as not covered.
pub fn chunk_brief(
    project: &Project,
    pass: &ReviewPass,
    plan: &Plan,
    chunk: usize,
    only: &[String],
) -> Option<String> {
    let body = plan.chunks.get(chunk.checked_sub(1)?)?;
    let total = plan.chunks.len();
    let card = &pass.card_id;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Pre-launch review of {} ({} pass {}), chunk {chunk} of {total}: {}..{}.\n",
        project.id,
        pass.kind,
        pass.id,
        short(&plan.base_sha),
        short(&plan.head_sha),
    );
    let _ = writeln!(
        out,
        "The pass head is {head}. Your worktree is the whole repository at the default-branch head, which is at or after it; when a file differs (`git diff --stat {head} HEAD`), read it at the pass head with `git show {head}:<path>`. Judge only the diff below, but read anything you need: callers, tests, and the final state of every file, including code other chunks changed. The index at the end lists every PR in the pass, so open another PR's diff when it touches what you are reading.\n",
        head = plan.head_sha,
    );
    let _ = writeln!(
        out,
        "Scope: correctness bugs, security issues and regressions only. No style, no refactors, no nits. Every finding names file:line at {head}, the PR that introduced it, a tag ([confirmed] or [plausible]), a severity (high, medium or low), the failure scenario and the smallest fix.\n",
        head = short(&plan.head_sha),
    );
    if body.oversized {
        let _ = writeln!(
            out,
            "This chunk is one file over the {HARD_LINES}-line cap. Cover what you can and list the rest as not covered.\n"
        );
    }
    let _ = writeln!(out, "## The diff to judge ({} changed lines)\n", body.lines);
    for part in &body.parts {
        let Some(unit) = plan.unit(&part.unit) else {
            continue;
        };
        let files: Vec<&String> = part
            .files
            .iter()
            .filter(|path| only.is_empty() || only.contains(path))
            .collect();
        if files.is_empty() {
            continue;
        }
        let _ = writeln!(out, "### {}\n", unit_label(unit));
        let paths: Vec<&str> = files.iter().map(|path| path.as_str()).collect();
        for commit in &unit.commits {
            let _ = writeln!(
                out,
                "    git diff {commit}^1 {commit} -- {}",
                paths.join(" ")
            );
        }
        let _ = writeln!(out);
        for path in &files {
            let lines = unit
                .files
                .iter()
                .find(|file| &&file.path == path)
                .map_or(0, |file| file.lines);
            let _ = writeln!(out, "- {path} ({lines} lines)");
        }
        if !unit.body.trim().is_empty() {
            let text: String = unit.body.chars().take(BODY_CHARS).collect();
            let cut = if unit.body.chars().count() > BODY_CHARS {
                "\n[body cut]"
            } else {
                ""
            };
            let _ = writeln!(out, "\nPR body:\n\n{}{cut}", text.trim_end());
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(
        out,
        "## When you stop\n\nPast about {CONTEXT_STOP} tokens of context, stop reading. Write what you covered, then a \"Not covered\" section listing every file of this chunk you did not judge; the orchestrator reviews those in a new run. Never drop coverage silently.\n"
    );
    let _ = writeln!(
        out,
        "## Record\n\n1. Write review.md: the findings, then \"Covered\" and \"Not covered\".\n2. `tiller review record <your run> --reviewer code-review --coverage diff --verdict <v> --sha {head} --high <n> --medium <n> --low <n>`\n3. `tiller artifact add {card} --kind report --title \"chunk {chunk} of {total}\" --path review.md`\n",
        head = plan.head_sha,
    );
    let _ = writeln!(out, "## Every PR in the pass\n\n{}", index(plan));
    Some(out)
}

/// The brief of the summary run, which reads only the chunk reports.
pub fn summary_brief(project: &Project, pass: &ReviewPass, plan: &Plan) -> String {
    let card = &pass.card_id;
    let head = &plan.head_sha;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Summary of the pre-launch review of {} ({} pass {}): {}..{} in {} chunks.\n",
        project.id,
        pass.kind,
        pass.id,
        short(&plan.base_sha),
        short(head),
        plan.chunks.len(),
    );
    let _ = writeln!(
        out,
        "Read only the chunk reports (the `report` artifacts `tiller card show {card}` lists), not the diffs.\n\n1. Re-anchor every finding at {head} with `git blame`. A line already fixed later in the range is dropped as \"fixed in range\".\n2. Merge duplicates and rank by severity.\n3. Read each seam below at {head} for breakage across chunks.\n4. List every \"Not covered\" file the chunk reports name, so none is dropped silently.\n5. Write the report page and record it: `tiller artifact add {card} --kind page --title \"pre-launch review {}\" --path report.html`.\n",
        short(head),
    );
    if !plan.excluded.is_empty() {
        let _ = writeln!(out, "The report lists these paths as not counted:\n");
        for excluded in &plan.excluded {
            let _ = writeln!(out, "- {} ({})", excluded.path, excluded.why);
        }
        let _ = writeln!(out);
    }
    let oversized: Vec<String> = plan
        .chunks
        .iter()
        .enumerate()
        .filter(|(_, chunk)| chunk.oversized)
        .map(|(i, chunk)| {
            format!(
                "- chunk {}: {}",
                i + 1,
                chunk
                    .parts
                    .iter()
                    .flat_map(|part| &part.files)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect();
    if !oversized.is_empty() {
        let _ = writeln!(
            out,
            "Flag these oversized chunks in the report:\n\n{}\n",
            oversized.join("\n")
        );
    }
    let _ = writeln!(out, "## Seams\n");
    if plan.seams.is_empty() {
        let _ = writeln!(out, "None: no file was changed in more than one chunk.\n");
    }
    for seam in &plan.seams {
        let chunks: Vec<String> = seam.chunks.iter().map(usize::to_string).collect();
        let _ = writeln!(out, "- {} (chunks {})", seam.path, chunks.join(", "));
    }
    let _ = writeln!(out, "\n## Every PR in the pass\n\n{}", index(plan));
    out
}

#[cfg(test)]
mod tests;
