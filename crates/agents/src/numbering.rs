//! Where a re-added project's cards start (`state.md`, Deleting a project).
//!
//! A delete keeps the project's checkout, branches and PRs. A run branch is
//! named after its card, `tiller/<project>-<n>-<role>`, so the cards of a
//! project added under a deleted id start at 1 again only when none of those
//! branches is left in the checkout or on its remote. Otherwise they number
//! on from the tombstone, and the add says why.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

use tiller_core::Store;
use tiller_core::state::projects::{NewProject, Numbering};

/// How `project add` numbers `new`, with `checkout` its configured local
/// checkout, and the line that says why when the cards number on.
pub fn numbering(
    store: &Store,
    new: &NewProject,
    checkout: Option<&Path>,
) -> tiller_core::Result<(Numbering, Option<String>)> {
    let Some(seq) = store.deleted_card_seq(&new.id)? else {
        return Ok((Numbering::Continue, None));
    };
    let remote = new.remote.clone().or_else(|| {
        new.repo
            .as_ref()
            .map(|repo| format!("https://github.com/{repo}.git"))
    });
    let id = &new.id;
    Ok(match left_run_branches(id, checkout, remote.as_deref()) {
        Ok(left) if left.is_empty() => (Numbering::Restart, None),
        Ok(left) => (
            Numbering::Continue,
            Some(format!(
                "cards continue at {id}-{seq}: the deleted project's run branches are left ({}); delete them, then delete and add the project again to start at {id}-1",
                left.into_iter().collect::<Vec<_>>().join(", ")
            )),
        ),
        Err(error) => (
            Numbering::Continue,
            Some(format!(
                "cards continue at {id}-{seq}: the deleted project's run branches could not be looked for ({error})"
            )),
        ),
    })
}

/// The run branches of `project_id` in `checkout` and on its origin, or on
/// `remote` when no checkout is configured. Remote-tracking refs are not
/// counted: they outlive a branch deleted on the remote until a prune, and
/// `ls-remote` asks the origin itself.
pub fn left_run_branches(
    project_id: &str,
    checkout: Option<&Path>,
    remote: Option<&str>,
) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let heads = match (checkout, remote) {
        (Some(checkout), _) => {
            let refs = git(
                Some(checkout),
                &["for-each-ref", "--format=%(refname)", "refs/heads/tiller/"],
            )?;
            names.extend(
                refs.lines()
                    .filter_map(|line| line.strip_prefix("refs/heads/").map(str::to_owned)),
            );
            git(Some(checkout), &["ls-remote", "--heads", "origin"])?
        }
        (None, Some(remote)) => git(None, &["ls-remote", "--heads", remote])?,
        (None, None) => {
            return Err("no checkout is configured and the project has no remote".into());
        }
    };
    names.extend(heads.lines().filter_map(|line| {
        line.split_once('\t')
            .and_then(|(_, name)| name.strip_prefix("refs/heads/"))
            .map(str::to_owned)
    }));
    names.retain(|name| is_run_branch(name, project_id));
    Ok(names)
}

/// `tiller/<project>-<n>-...`: a run branch of one of the project's cards,
/// not of a project whose id only starts with this one.
fn is_run_branch(name: &str, project_id: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("tiller/")
        .and_then(|rest| rest.strip_prefix(project_id))
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('-')
}

/// Runs git without a prompt: a remote that asks for credentials, or does not
/// answer within the timeouts, is an error, not a hang.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    command
        .args(["-c", "http.lowSpeedLimit=1", "-c", "http.lowSpeedTime=20"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=20",
        )
        .stdin(Stdio::null());
    let out = command
        .output()
        .map_err(|error| format!("git {}: {error}", args[0]))?;
    if !out.status.success() {
        return Err(format!(
            "git {} exited {}: {}",
            args[0],
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    }

    /// A bare origin and a clone of it with one commit on main.
    fn repos() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let origin = root.path().join("origin.git");
        let clone = root.path().join("clone");
        run(
            root.path(),
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        run(root.path(), &["clone", "--quiet", "origin.git", "clone"]);
        run(
            &clone,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "start",
            ],
        );
        run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
        (root, origin, clone)
    }

    #[test]
    fn a_run_branch_names_a_card_of_this_project() {
        assert!(is_run_branch("tiller/alpha-2-build", "alpha"));
        assert!(is_run_branch("tiller/alpha-12-review-3", "alpha"));
        assert!(!is_run_branch("tiller/alpha-beta-1-build", "alpha"));
        assert!(!is_run_branch("tiller/alpha-2", "alpha"));
        assert!(!is_run_branch("tiller/alphabet-2-build", "alpha"));
        assert!(!is_run_branch("feature/alpha-2-build", "alpha"));
    }

    #[test]
    fn branches_are_found_in_the_checkout_and_on_its_origin() {
        let (_root, origin, clone) = repos();
        assert!(
            left_run_branches("alpha", Some(&clone), None)
                .unwrap()
                .is_empty()
        );

        run(&clone, &["branch", "tiller/alpha-1-build"]);
        run(&clone, &["branch", "tiller/alpha-beta-1-build"]);
        run(
            &clone,
            &[
                "push",
                "--quiet",
                "origin",
                "HEAD:refs/heads/tiller/alpha-3-build",
            ],
        );
        assert_eq!(
            left_run_branches("alpha", Some(&clone), None).unwrap(),
            BTreeSet::from(["tiller/alpha-1-build".into(), "tiller/alpha-3-build".into()])
        );
        // A remote-tracking ref left after the remote branch is gone is stale.
        run(&clone, &["fetch", "--quiet", "origin"]);
        run(&origin, &["branch", "-D", "tiller/alpha-3-build"]);
        run(&clone, &["branch", "-D", "tiller/alpha-1-build"]);
        let stale = git(
            Some(&clone),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/tiller/alpha-3-build",
            ],
        );
        assert!(stale.is_ok(), "the clone still holds the stale ref");
        assert!(
            left_run_branches("alpha", Some(&clone), None)
                .unwrap()
                .is_empty()
        );
        run(
            &clone,
            &[
                "push",
                "--quiet",
                "origin",
                "HEAD:refs/heads/tiller/alpha-3-build",
            ],
        );
        let url = origin.to_str().unwrap();
        assert_eq!(
            left_run_branches("alpha", None, Some(url)).unwrap(),
            BTreeSet::from(["tiller/alpha-3-build".into()])
        );
        assert!(left_run_branches("alpha", None, None).is_err());
    }
}
