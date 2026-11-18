//! Keeps a project's checkout current: the folder `[checkouts]` names, which
//! the orchestrator reads to answer what is on the default branch but may
//! never write (G15). Workers start from a fresh fetch (`spawn::pool`), so
//! nothing else moves the checkout, and it fell behind every merge made on
//! the forge.
//!
//! [`sync`] fetches the default branch and fast-forwards the checkout's local
//! default branch to it, as `git pull --ff-only` would, but only when the
//! checkout has that branch checked out, has no uncommitted change to a
//! tracked file, and the move is a fast-forward. Anything else is the
//! owner's work in progress, so the checkout is left as it is and the
//! answer says why. The daemon runs it on a timer and after each landing,
//! and `tiller project sync` runs it on demand.
//!
//! Every git call goes through [`direct`], so no hook, fsmonitor, filter or
//! config command the repository names runs: the caller may be the
//! orchestrator, through the one `tiller` binary the sandbox exempts.

use std::path::Path;

use serde::Serialize;

use crate::land::direct::{self, Pushed};
use crate::land::forge::Identity;

/// What [`sync`] did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Synced {
    /// The checkout's default branch already was the remote's.
    Current { head: String },
    /// The checkout's default branch moved, and its files with it.
    Advanced { from: String, to: String },
    /// Fetched, but the checkout was left as it is: `why` says what holds it.
    Held {
        head: String,
        remote: String,
        why: String,
    },
}

impl Synced {
    /// One line for a person.
    pub fn line(&self, project: &str, default: &str) -> String {
        match self {
            Self::Current { head } => {
                format!("{project}: {default} is current at {}.", short(head))
            }
            Self::Advanced { from, to } => format!(
                "{project}: {default} moved from {} to {}.",
                short(from),
                short(to)
            ),
            Self::Held { head, remote, why } => format!(
                "{project}: {default} stays at {} ({why}); the remote is at {}.",
                short(head),
                short(remote)
            ),
        }
    }
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// Fetches `default` from `remote` (a remote of `checkout` or a URL) and
/// fast-forwards the checkout to it when that is safe. An error means the
/// fetch or the move failed; a checkout that is not safe to move is
/// [`Synced::Held`], not an error.
pub fn sync(checkout: &Path, remote: &str, default: &str) -> Result<Synced, String> {
    let fetched = direct::fetch_default(checkout, remote, default, &Identity::Owner)
        .map_err(|error| format!("fetch {default}: {error}"))?;
    direct::track(checkout, remote, default, &fetched)
        .map_err(|error| format!("record {remote}/{default}: {error}"))?;
    let head = direct::local_default(checkout, default)
        .map_err(|error| format!("read {default}: {error}"))?;
    let held = |why: String| {
        Ok(Synced::Held {
            head: head.clone(),
            remote: fetched.clone(),
            why,
        })
    };
    if head == fetched {
        return Ok(Synced::Current { head });
    }
    if !direct::descends(checkout, &head, &fetched) {
        return if direct::descends(checkout, &fetched, &head) {
            held(format!("{default} has commits the remote does not"))
        } else {
            held(format!("{default} and the remote have diverged"))
        };
    }
    match direct::checked_out(checkout)? {
        Some(branch) if branch == default => {}
        Some(branch) => return held(format!("the checkout is on {branch}")),
        None => return held("the checkout has a detached HEAD".to_owned()),
    }
    let changed = direct::uncommitted(checkout, &head)?;
    if !changed.is_empty() {
        return held("the checkout has uncommitted changes".to_owned());
    }
    match direct::advance(checkout, default, &head, &fetched) {
        Pushed::Done => Ok(Synced::Advanced {
            from: head,
            to: fetched,
        }),
        Pushed::Refused(why) => held(why),
        Pushed::Unknown(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::{Synced, sync};

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn commit(dir: &Path, file: &str) -> String {
        std::fs::write(dir.join(file), file).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", file]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A bare remote, the owner's checkout of it on `main`, and a second
    /// clone that pushes `main` one commit ahead (a merge on the forge).
    /// Returns the dirs, the checkout's head and the remote's new head.
    struct World {
        _remote: tempfile::TempDir,
        checkout: tempfile::TempDir,
        _other: tempfile::TempDir,
        old: String,
        new: String,
    }

    fn world() -> World {
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
        let url = remote.path().display().to_string();
        let checkout = tempfile::tempdir().unwrap();
        git(checkout.path(), &["init", "-q", "-b", "main"]);
        git(checkout.path(), &["remote", "add", "origin", &url]);
        let old = commit(checkout.path(), "base");
        git(checkout.path(), &["push", "-q", "origin", "main"]);
        let other = tempfile::tempdir().unwrap();
        git(other.path(), &["clone", "-q", &url, "."]);
        let new = commit(other.path(), "merged");
        git(other.path(), &["push", "-q", "origin", "main"]);
        World {
            _remote: remote,
            checkout,
            _other: other,
            old,
            new,
        }
    }

    #[test]
    fn a_clean_checkout_on_main_moves_with_its_files() {
        let w = world();
        let synced = sync(w.checkout.path(), "origin", "main").unwrap();
        assert_eq!(
            synced,
            Synced::Advanced {
                from: w.old.clone(),
                to: w.new.clone()
            }
        );
        assert_eq!(git(w.checkout.path(), &["rev-parse", "main"]), w.new);
        assert_eq!(git(w.checkout.path(), &["rev-parse", "origin/main"]), w.new);
        assert!(w.checkout.path().join("merged").exists());
        assert_eq!(git(w.checkout.path(), &["status", "--porcelain"]), "");
        // A second sync has nothing to do.
        assert_eq!(
            sync(w.checkout.path(), "origin", "main").unwrap(),
            Synced::Current { head: w.new }
        );
    }

    #[test]
    fn uncommitted_changes_hold_the_checkout() {
        let w = world();
        std::fs::write(w.checkout.path().join("base"), "edited").unwrap();
        let synced = sync(w.checkout.path(), "origin", "main").unwrap();
        assert!(
            matches!(&synced, Synced::Held { why, .. } if why.contains("uncommitted")),
            "{synced:?}"
        );
        assert_eq!(git(w.checkout.path(), &["rev-parse", "main"]), w.old);
        assert_eq!(
            std::fs::read_to_string(w.checkout.path().join("base")).unwrap(),
            "edited"
        );
        // The fetch still lands, so the remote's head can be read.
        assert_eq!(git(w.checkout.path(), &["rev-parse", "origin/main"]), w.new);
    }

    #[test]
    fn another_branch_holds_the_checkout() {
        let w = world();
        git(w.checkout.path(), &["switch", "-q", "-c", "work"]);
        let synced = sync(w.checkout.path(), "origin", "main").unwrap();
        assert!(
            matches!(&synced, Synced::Held { why, .. } if why.contains("on work")),
            "{synced:?}"
        );
        assert_eq!(git(w.checkout.path(), &["rev-parse", "main"]), w.old);
    }

    #[test]
    fn local_commits_hold_the_checkout() {
        let w = world();
        commit(w.checkout.path(), "mine");
        let synced = sync(w.checkout.path(), "origin", "main").unwrap();
        assert!(
            matches!(&synced, Synced::Held { why, .. } if why.contains("diverged")),
            "{synced:?}"
        );
        assert_ne!(git(w.checkout.path(), &["rev-parse", "main"]), w.new);
    }

    #[test]
    fn a_checkout_ahead_of_the_remote_is_held() {
        let w = world();
        git(
            w.checkout.path(),
            &["pull", "-q", "--ff-only", "origin", "main"],
        );
        commit(w.checkout.path(), "ahead");
        let synced = sync(w.checkout.path(), "origin", "main").unwrap();
        assert!(
            matches!(&synced, Synced::Held { why, .. } if why.contains("commits the remote does not")),
            "{synced:?}"
        );
    }

    /// The sync runs outside the sandbox for the orchestrator, so nothing
    /// the repository's config names may run: not a hook, not a filter.
    #[test]
    fn the_sync_runs_nothing_the_repository_names() {
        let w = world();
        let marks = tempfile::tempdir().unwrap();
        let script = |name: &str| {
            let path = marks.path().join(format!("{name}.sh"));
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\ntouch '{}'\ncat\n",
                    marks.path().join(name).display()
                ),
            )
            .unwrap();
            let mut mode = std::fs::metadata(&path).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
            std::fs::set_permissions(&path, mode).unwrap();
            path.display().to_string()
        };
        let hooks = w.checkout.path().join(".git/hooks");
        for hook in ["post-merge", "post-checkout", "reference-transaction"] {
            let path = script(hook);
            std::fs::copy(&path, hooks.join(hook)).unwrap();
        }
        std::fs::write(
            w.checkout.path().join(".git/info/attributes"),
            "merged filter=x\n",
        )
        .unwrap();
        let smudge = script("smudge");
        git(w.checkout.path(), &["config", "filter.x.smudge", &smudge]);
        let synced = sync(w.checkout.path(), "origin", "main").unwrap();
        assert!(matches!(synced, Synced::Advanced { .. }), "{synced:?}");
        for mark in [
            "post-merge",
            "post-checkout",
            "reference-transaction",
            "smudge",
        ] {
            assert!(!marks.path().join(mark).exists(), "{mark} ran");
        }
        // Plain git in the same checkout runs them, so the setup is live.
        git(w.checkout.path(), &["switch", "-q", "-c", "probe"]);
        std::fs::remove_file(w.checkout.path().join("merged")).unwrap();
        git(w.checkout.path(), &["checkout", "--", "merged"]);
        for mark in ["post-checkout", "reference-transaction", "smudge"] {
            assert!(marks.path().join(mark).exists(), "{mark} is not live");
        }
    }
}
