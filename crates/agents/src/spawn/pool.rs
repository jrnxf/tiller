//! The worktree a run works in: taken from the treehouse pool, brought to
//! the current head of the project's default branch on origin (P05, P06),
//! and given back only through cleanup.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The worktree pool. Treehouse owns every worktree; spawn takes one per run
/// and cleanup gives it back, both under the run's lease holder.
pub trait Pool {
    /// Leases a worktree of the checkout at `checkout`, on a new `branch` cut
    /// from `base`, and returns its absolute path.
    fn take(
        &self,
        checkout: &Path,
        branch: &str,
        base: &str,
        holder: &str,
    ) -> Result<PathBuf, String>;
    /// Returns a worktree to the pool, only while `holder` still leases it.
    fn give_back(&self, path: &Path, holder: &str) -> Result<(), String>;
}

/// The `treehouse` CLI.
pub struct Treehouse {
    program: Vec<OsString>,
}

impl Treehouse {
    /// `program` is the command and its leading arguments, `["treehouse"]`
    /// unless the config names another.
    pub fn new(program: Vec<OsString>) -> Self {
        Self { program }
    }

    fn command(&self) -> Result<Command, String> {
        let (program, leading) = self
            .program
            .split_first()
            .ok_or_else(|| "the treehouse command is empty".to_owned())?;
        let mut command = Command::new(program);
        command
            .args(leading)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command)
    }
}

fn output(mut command: Command, what: &str) -> Result<String, String> {
    let out = command.output().map_err(|e| format!("{what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl Pool for Treehouse {
    fn take(
        &self,
        checkout: &Path,
        branch: &str,
        base: &str,
        holder: &str,
    ) -> Result<PathBuf, String> {
        let mut command = self.command()?;
        // treehouse fetches origin before it cuts the worktree.
        command
            .current_dir(checkout)
            .args(["get", "--lease", "--lease-holder", holder])
            .args(["--branch", branch, "--base", base]);
        let path = output(command, "treehouse get")?;
        let path = PathBuf::from(path.trim());
        if !path.is_absolute() {
            return Err(format!(
                "treehouse get printed {path:?}, not an absolute path"
            ));
        }
        Ok(path)
    }

    fn give_back(&self, path: &Path, holder: &str) -> Result<(), String> {
        // Never `--force`: treehouse itself refuses a worktree with
        // uncommitted changes when nobody confirms, a second guard behind
        // cleanup's own checks.
        let mut command = self.command()?;
        command
            .arg("return")
            .arg(path)
            .args(["--if-lease-holder", holder]);
        output(command, "treehouse return").map(drop)
    }
}

fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args).stdin(Stdio::null());
    output(command, &format!("git {}", args.join(" "))).map(|s| s.trim().to_owned())
}

/// Brings a fresh worktree to the head of `origin/<default>` as a fetch made
/// now sees it (P05), and returns that head as the run's `base_sha`.
///
/// A worktree cut from a stale local ref is fast-forwarded first, never
/// worked on. One that holds anything not on the default branch is not a
/// fresh worktree, so spawn refuses it rather than build on it.
pub fn start_from_origin(worktree: &Path, default_branch: &str) -> Result<String, String> {
    git(worktree, &["fetch", "--quiet", "origin", default_branch])?;
    let base = git(
        worktree,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{default_branch}^{{commit}}"),
        ],
    )?;
    let head = git(worktree, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head == base {
        return Ok(base);
    }
    if git(worktree, &["merge-base", "--is-ancestor", "HEAD", &base]).is_err() {
        return Err(format!(
            "the new worktree's HEAD {head} is not on origin/{default_branch}; it is not a fresh worktree"
        ));
    }
    git(worktree, &["merge", "--ff-only", "--quiet", &base])?;
    Ok(base)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A stand-in `treehouse` that appends its working directory and
    /// arguments to `calls` and prints `worktree` for a `get`.
    fn stand_in(dir: &Path, worktree: &str) -> (Treehouse, PathBuf) {
        let calls = dir.join("calls");
        let script = dir.join("treehouse");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n{{ pwd; for a in \"$@\"; do echo \"$a\"; done; echo --; }} >> '{}'\n[ \"$1\" = get ] && echo '{worktree}'\nexit 0\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (Treehouse::new(vec![script.into_os_string()]), calls)
    }

    /// P06: a run's worktree is leased from the pool with the exact `get`
    /// arguments, in the project's checkout, and given back only while the
    /// holder still leases it, never with `--force`.
    #[test]
    fn treehouse_takes_and_returns_with_the_lease_holder() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let (pool, calls) = stand_in(dir.path(), "/home/user/worktrees/alpha-1");

        let path = pool.take(&checkout, "fm/alpha-1", "main", "run-1").unwrap();
        assert_eq!(path, Path::new("/home/user/worktrees/alpha-1"));
        pool.give_back(&path, "run-1").unwrap();

        let log = std::fs::read_to_string(calls).unwrap();
        let checkout = checkout.canonicalize().unwrap();
        let want = format!(
            "{}\nget\n--lease\n--lease-holder\nrun-1\n--branch\nfm/alpha-1\n--base\nmain\n--\n{}\nreturn\n/home/user/worktrees/alpha-1\n--if-lease-holder\nrun-1\n--\n",
            checkout.display(),
            std::env::current_dir().unwrap().display(),
        );
        assert_eq!(log, want);
        assert!(!log.contains("--force"));
    }

    /// P06: a pool that prints a relative path, or fails, is a refusal and
    /// never a worktree.
    #[test]
    fn treehouse_refuses_a_relative_path_and_a_failing_get() {
        let dir = tempfile::tempdir().unwrap();
        let (pool, _) = stand_in(dir.path(), "relative/path");
        let error = pool.take(dir.path(), "fm/alpha-1", "main", "run-1");
        assert!(error.unwrap_err().contains("not an absolute path"));

        let failing = Treehouse::new(vec!["false".into()]);
        assert!(failing.take(dir.path(), "b", "main", "h").is_err());
        assert!(Treehouse::new(vec![]).give_back(dir.path(), "h").is_err());
    }
}
