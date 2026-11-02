//! The worktree scan over real git (`recovery.md` section 2, "What unlanded
//! means"): a commit is landed when it is on the project's default branch,
//! when a merged PR's head equals or descends from it, or when `git cherry`
//! finds its patch on the default branch. Everything reachable from the
//! worktree's HEAD or its stash entries and not landed is unlanded.
//!
//! Every git question that fails leaves its value `None`, which blocks
//! cleanup: the scan never guesses.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use tiller_core::domain::Project;

use crate::sources::{Scanner, WorktreeScan};

/// A scanner that shells out to `git`, and to a lister command (such as
/// `treehouse list --paths`) that prints one worktree path per line, with a
/// matching return command that takes the path as its last argument.
pub struct GitScanner {
    lister: Option<Vec<OsString>>,
    releaser: Option<Vec<OsString>>,
}

impl Default for GitScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl GitScanner {
    /// Git only; `listed` reports that no lister is configured and nothing
    /// is ever returned.
    pub fn new() -> Self {
        Self {
            lister: None,
            releaser: None,
        }
    }

    /// Git plus a command whose stdout is one worktree path per line, and
    /// one that returns a worktree given its path as the last argument.
    pub fn with_pool(lister: Vec<OsString>, releaser: Vec<OsString>) -> Self {
        Self {
            lister: Some(lister),
            releaser: Some(releaser),
        }
    }
}

fn run_command(command: &[OsString], extra: Option<&Path>) -> Result<String, String> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "empty command".to_owned())?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(extra) = extra {
        cmd.arg(extra);
    }
    let out = cmd
        .output()
        .map_err(|error| format!("{}: {error}", program.to_string_lossy()))?;
    if !out.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            program.to_string_lossy(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} exited {}: {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The ref that stands for the project's default branch here: the remote
/// tracking ref when it exists, else the local branch.
fn default_ref(path: &Path, project: &Project) -> Option<String> {
    let branch = &project.default_branch;
    for candidate in [
        format!("refs/remotes/origin/{branch}"),
        format!("refs/heads/{branch}"),
    ] {
        if git(
            path,
            &[
                "rev-parse",
                "--verify",
                "-q",
                &format!("{candidate}^{{commit}}"),
            ],
        )
        .is_ok()
        {
            return Some(candidate);
        }
    }
    None
}

/// Commits reachable from HEAD and the stash entries, not reachable from
/// the default branch or any merged PR head known here, whose patch is not
/// on the default branch either.
fn unlanded(path: &Path, default_ref: &str, merged_heads: &[String]) -> Result<i64, String> {
    let stashes = git(path, &["stash", "list", "--format=%H"]).map(|s| lines(&s))?;
    let mut args: Vec<String> = vec!["rev-list".into(), "HEAD".into()];
    args.extend(stashes);
    args.push("--not".into());
    args.push(default_ref.to_owned());
    for head in merged_heads {
        if git(path, &["cat-file", "-e", &format!("{head}^{{commit}}")]).is_ok() {
            args.push(head.clone());
        }
    }
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let candidates = lines(&git(path, &borrowed)?);
    if candidates.is_empty() {
        return Ok(0);
    }
    // `git cherry` marks a commit `-` when a patch-equivalent commit is on
    // the upstream: a squash or rebase merge lands commits under new SHAs.
    let cherry = git(path, &["cherry", default_ref, "HEAD"])?;
    let landed_by_patch: Vec<String> = cherry
        .lines()
        .filter_map(|line| line.strip_prefix("- ").map(str::trim).map(str::to_owned))
        .collect();
    let count = candidates
        .iter()
        .filter(|sha| !landed_by_patch.contains(sha))
        .count();
    i64::try_from(count).map_err(|e| e.to_string())
}

impl Scanner for GitScanner {
    fn listed(&self) -> Result<Vec<PathBuf>, String> {
        let Some(command) = &self.lister else {
            return Err("no worktree lister is configured".into());
        };
        Ok(lines(&run_command(command, None)?)
            .into_iter()
            .map(PathBuf::from)
            .collect())
    }

    fn release(&self, path: &Path) -> Result<(), String> {
        let Some(command) = &self.releaser else {
            return Err(format!(
                "no worktree pool is configured to take back {}",
                path.display()
            ));
        };
        run_command(command, Some(path)).map(drop)
    }

    fn scan(&self, path: &Path, project: &Project, merged_heads: &[String]) -> WorktreeScan {
        let mut scan = WorktreeScan::missing(path);
        scan.exists = path.exists();
        if !scan.exists {
            return scan;
        }
        let toplevel = git(path, &["rev-parse", "--show-toplevel"])
            .ok()
            .map(|s| PathBuf::from(s.trim()));
        let canonical = std::fs::canonicalize(path).ok();
        scan.is_worktree = match (toplevel, canonical) {
            (Some(top), Some(here)) => std::fs::canonicalize(&top).ok().as_ref() == Some(&here),
            _ => false,
        };
        if !scan.is_worktree {
            return scan;
        }
        scan.branch = git(path, &["symbolic-ref", "--short", "-q", "HEAD"])
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        scan.head_sha = git(path, &["rev-parse", "HEAD"])
            .ok()
            .map(|s| s.trim().to_owned());
        scan.dirty = git(path, &["status", "--porcelain"])
            .ok()
            .map(|s| !s.trim().is_empty());
        scan.unlanded_commits = default_ref(path, project).and_then(|default| {
            unlanded(path, &default, merged_heads)
                .map_err(
                    |error| tracing::warn!(%error, path = %path.display(), "unlanded count failed"),
                )
                .ok()
        });
        scan
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use tiller_core::Timestamp;
    use tiller_core::domain::{Delivery, MergeGate};

    use super::*;

    fn project() -> Project {
        Project {
            id: "alpha".into(),
            name: "Alpha".into(),
            repo: Some("example/alpha".into()),
            remote: None,
            default_branch: "main".into(),
            delivery: Delivery::DirectPr,
            checks: serde_json::json!([]),
            capacity: 2,
            deploy_workflow: None,
            deploy_environment: None,
            deploy_timeout_minutes: 60,
            merge_gate: MergeGate::Detect,
            owner_host_id: "H".into(),
            added_at: Timestamp::now(),
            next_card_seq: 1,
            retired_at: None,
            rev: 1,
        }
    }

    fn sh(path: &Path, args: &[&str]) -> String {
        git(path, args).unwrap()
    }

    fn commit(path: &Path, file: &str, message: &str) -> String {
        std::fs::write(path.join(file), message).unwrap();
        sh(path, &["add", "."]);
        sh(path, &["commit", "-q", "-m", message]);
        sh(path, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// The unlanded count follows the spec's three ways to land: on the
    /// default branch, under a merged PR head, or by patch (`git cherry`).
    #[test]
    fn unlanded_counts_commits_not_on_main_not_under_a_merged_head_and_not_by_patch() {
        if !git_available() {
            eprintln!("git is not on PATH; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "user.email", "dev@example.com"]);
        sh(&repo, &["config", "user.name", "Dev"]);
        commit(&repo, "base.txt", "base");

        let worktree = dir.path().join("wt");
        sh(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fm/work",
                worktree.to_str().unwrap(),
            ],
        );
        sh(&worktree, &["config", "user.email", "dev@example.com"]);
        sh(&worktree, &["config", "user.name", "Dev"]);
        let first = commit(&worktree, "a.txt", "first");
        let second = commit(&worktree, "b.txt", "second");
        let scanner = GitScanner::new();

        let scan = scanner.scan(&worktree, &project(), &[]);
        assert!(scan.exists && scan.is_worktree);
        assert_eq!(scan.branch.as_deref(), Some("fm/work"));
        assert_eq!(scan.head_sha.as_deref(), Some(second.as_str()));
        assert_eq!(scan.dirty, Some(false));
        assert_eq!(scan.unlanded_commits, Some(2));

        // An untracked file is dirty; an ignored one is not.
        std::fs::write(worktree.join("notes.tmp"), "x").unwrap();
        assert_eq!(scanner.scan(&worktree, &project(), &[]).dirty, Some(true));
        std::fs::remove_file(worktree.join("notes.tmp")).unwrap();

        // Cherry-picking the first commit onto main lands it by patch.
        sh(&repo, &["cherry-pick", &first]);
        assert_eq!(
            scanner.scan(&worktree, &project(), &[]).unlanded_commits,
            Some(1)
        );

        // A merged PR whose head is the worktree head lands the rest.
        assert_eq!(
            scanner
                .scan(&worktree, &project(), std::slice::from_ref(&second))
                .unlanded_commits,
            Some(0)
        );
        assert!(
            scanner
                .scan(&worktree, &project(), &[second])
                .provably_clean()
        );

        // A stash entry holds unlanded work too.
        std::fs::write(worktree.join("a.txt"), "edited").unwrap();
        sh(&worktree, &["stash", "-q"]);
        assert!(
            scanner
                .scan(&worktree, &project(), &[])
                .unlanded_commits
                .unwrap()
                >= 2
        );

        // The repo root is not the worktree; a missing path is gone.
        assert!(
            !scanner
                .scan(&dir.path().join("nowhere"), &project(), &[])
                .exists
        );
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let plain_scan = scanner.scan(&plain, &project(), &[]);
        assert!(plain_scan.exists && !plain_scan.is_worktree);
        assert_eq!(
            plain_scan.unlanded_commits, None,
            "a non-worktree stays unknown"
        );
    }
}
