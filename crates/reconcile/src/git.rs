//! The worktree scan over real git (`recovery.md` section 2, "What unlanded
//! means"): a commit is landed when it is on the project's default branch,
//! when a merged PR's head equals or descends from it, or when `git cherry`
//! finds its patch on the default branch. Everything reachable from the
//! worktree's HEAD or its stash entries and not landed is unlanded.
//!
//! Every git question that fails leaves its value `None`, which blocks
//! cleanup: the scan never guesses.
//!
//! The worktrees it lists are the linked worktrees of the project checkouts
//! the pool cuts from, as `git worktree list --porcelain` reports them: the
//! checkout's own working tree is never among them, and a checkout whose
//! list cannot be read fails the whole list, so R15 returns nothing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tiller_core::domain::Project;

use crate::sources::{Scanner, WorktreeScan};

/// The worktree pool a scanner lists from and returns to.
pub trait WorktreePool: Send {
    /// The project checkouts the pool cuts worktrees from, as configured
    /// now. `Err` when the configuration cannot be read.
    fn checkouts(&self) -> Result<Vec<PathBuf>, String>;
    /// Returns a worktree to the pool, only while `holder` still leases it.
    fn give_back(&self, path: &Path, holder: &str) -> Result<(), String>;
}

/// A scanner that shells out to `git`, listing and returning worktrees
/// through a [`WorktreePool`] when it has one.
pub struct GitScanner {
    pool: Option<Box<dyn WorktreePool>>,
}

impl Default for GitScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl GitScanner {
    /// Git only; `listed` reports that no pool is configured and nothing
    /// is ever returned.
    pub fn new() -> Self {
        Self { pool: None }
    }

    /// Git plus the pool whose checkouts' worktrees are listed, and which
    /// takes a worktree back.
    pub fn with_pool(pool: impl WorktreePool + 'static) -> Self {
        Self {
            pool: Some(Box::new(pool)),
        }
    }
}

/// The linked worktrees in `git worktree list --porcelain -z` output: every
/// record after the first, which is the main working tree, except a bare
/// one.
fn linked_worktrees(porcelain: &str) -> Vec<PathBuf> {
    let mut records: Vec<(String, bool)> = Vec::new();
    for field in porcelain.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            records.push((path.to_owned(), false));
        } else if field == "bare"
            && let Some(record) = records.last_mut()
        {
            record.1 = true;
        }
    }
    records
        .into_iter()
        .skip(1)
        .filter(|(_, bare)| !bare)
        .map(|(path, _)| PathBuf::from(path))
        .collect()
}

fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .stdin(Stdio::null())
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
        let Some(pool) = &self.pool else {
            return Err("no worktree pool is configured".into());
        };
        let mut listed = Vec::new();
        for checkout in pool.checkouts()? {
            let porcelain = git(&checkout, &["worktree", "list", "--porcelain", "-z"])
                .map_err(|error| format!("{}: {error}", checkout.display()))?;
            listed.extend(linked_worktrees(&porcelain));
        }
        Ok(listed)
    }

    fn release(&self, path: &Path, holder: &str) -> Result<(), String> {
        let Some(pool) = &self.pool else {
            return Err(format!(
                "no worktree pool is configured to take back {}",
                path.display()
            ));
        };
        pool.give_back(path, holder)
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
        // Untracked files and changed submodules count, whatever the
        // repository's own config hides from a plain `git status`.
        scan.dirty = git(
            path,
            &[
                "status",
                "--porcelain",
                "--untracked-files=normal",
                "--ignore-submodules=none",
            ],
        )
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
            stage: tiller_core::domain::Stage::Launched,
            reviewed_through_sha: None,
            stage_changed_at: None,
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

    /// A pool over fixed checkouts that records each return.
    struct TestPool {
        checkouts: Result<Vec<PathBuf>, String>,
        returned: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl WorktreePool for TestPool {
        fn checkouts(&self) -> Result<Vec<PathBuf>, String> {
            self.checkouts.clone()
        }

        fn give_back(&self, path: &Path, holder: &str) -> Result<(), String> {
            self.returned
                .lock()
                .unwrap()
                .push(format!("{} {holder}", path.display()));
            Ok(())
        }
    }

    fn pool(checkouts: Result<Vec<PathBuf>, String>) -> GitScanner {
        GitScanner::with_pool(TestPool {
            checkouts,
            returned: Default::default(),
        })
    }

    #[test]
    fn porcelain_lists_linked_worktrees_but_not_the_main_or_a_bare_one() {
        let porcelain = "worktree /home/user/alpha\0HEAD 1111\0branch refs/heads/main\0\0\
                         worktree /home/user/pool/1/alpha\0HEAD 2222\0detached\0\0\
                         worktree /home/user/pool/2/my alpha\0HEAD 3333\0branch refs/heads/tiller/a\0locked\0\0\
                         worktree /home/user/gone\0HEAD 4444\0detached\0prunable gitdir file points to non-existent location\0\0";
        assert_eq!(
            linked_worktrees(porcelain),
            [
                "/home/user/pool/1/alpha",
                "/home/user/pool/2/my alpha",
                "/home/user/gone"
            ]
            .map(PathBuf::from)
        );
        let bare = "worktree /home/user/alpha.git\0bare\0\0worktree /home/user/pool/1/alpha\0HEAD 1111\0detached\0\0";
        assert_eq!(
            linked_worktrees(bare),
            vec![PathBuf::from("/home/user/pool/1/alpha")]
        );
        assert!(linked_worktrees("").is_empty());
    }

    /// The list is every checkout's linked worktrees; one checkout that
    /// cannot be listed, or a pool that cannot name its checkouts, fails
    /// the whole list, and a scanner without a pool lists nothing.
    #[test]
    fn listed_reads_each_checkouts_worktrees_and_fails_whole() {
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
        let worktree = dir.path().join("pool").join("one");
        sh(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fm/one",
                worktree.to_str().unwrap(),
            ],
        );

        let listed = pool(Ok(vec![repo.clone()])).listed().unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(
            std::fs::canonicalize(&listed[0]).unwrap(),
            std::fs::canonicalize(&worktree).unwrap()
        );

        let missing = dir.path().join("missing");
        assert!(pool(Ok(vec![repo.clone(), missing])).listed().is_err());
        assert!(
            pool(Err("config.toml does not parse".into()))
                .listed()
                .is_err()
        );
        assert!(GitScanner::new().listed().is_err());
    }

    #[test]
    fn release_goes_to_the_pool_with_the_lease_holder() {
        let returned = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let scanner = GitScanner::with_pool(TestPool {
            checkouts: Ok(Vec::new()),
            returned: returned.clone(),
        });
        scanner
            .release(Path::new("/home/user/pool/1/alpha"), "tiller:R1")
            .unwrap();
        assert_eq!(
            *returned.lock().unwrap(),
            vec!["/home/user/pool/1/alpha tiller:R1".to_owned()]
        );
        assert!(
            GitScanner::new()
                .release(Path::new("/home/user/pool/1/alpha"), "tiller:R1")
                .is_err()
        );
    }

    /// Untracked files are dirty even where the repository's config hides
    /// them from a plain `git status`.
    #[test]
    fn untracked_files_are_dirty_whatever_the_status_config() {
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
                "fm/wt",
                worktree.to_str().unwrap(),
            ],
        );
        sh(&worktree, &["config", "status.showUntrackedFiles", "no"]);
        let scanner = GitScanner::new();
        assert_eq!(scanner.scan(&worktree, &project(), &[]).dirty, Some(false));
        std::fs::create_dir(worktree.join("notes")).unwrap();
        std::fs::write(worktree.join("notes").join("draft.md"), "x").unwrap();
        let scan = scanner.scan(&worktree, &project(), &[]);
        assert_eq!(scan.dirty, Some(true));
        assert!(!scan.provably_clean());
    }
}
