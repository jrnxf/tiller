//! The git side of `land_direct` (P11, `authority.md` section 6): fetch the
//! default branch, check the bound head descends from it, and push that one
//! SHA as a plain fast-forward. The remote refuses anything else, so a moved
//! default branch fails the land instead of being overwritten.
//!
//! A `local-only` project (P12) has no remote: the same checks run against
//! the local default branch, which [`advance`] fast-forwards in the
//! repository the run's worktree shares.

use std::path::Path;
use std::process::Command;

use super::forge::{AppToken, Identity};

/// What one `git` call printed, or why it could not run.
fn git(repo: &Path, args: &[&str], identity: &Identity) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(repo).args(args);
    // Never prompt; never pick up a credential helper's interactive login.
    command.env("GIT_TERMINAL_PROMPT", "0");
    if let Identity::App(token) = identity {
        // The token goes in the environment, not on the command line, so the
        // process list never shows it.
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader")
            .env("GIT_CONFIG_VALUE_0", basic_header(token));
    }
    let output = command
        .output()
        .map_err(|error| format!("run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        // `--porcelain` reports a rejected ref on stdout, the summary on stderr.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("{}\n{}", stdout.trim(), stderr.trim())
            .trim()
            .to_owned())
    }
}

fn basic_header(token: &AppToken) -> String {
    format!(
        "AUTHORIZATION: basic {}",
        base64(&format!("x-access-token:{}", token.secret()))
    )
}

fn base64(text: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The local branch's head in the run's worktree.
pub fn branch_head(repo: &Path, branch: &str) -> Result<String, String> {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
        &Identity::Owner,
    )
}

/// Fetches the remote default branch and returns its head.
pub fn fetch_default(
    repo: &Path,
    remote: &str,
    default: &str,
    identity: &Identity,
) -> Result<String, String> {
    git(
        repo,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            remote,
            &format!("refs/heads/{default}"),
        ],
        identity,
    )?;
    git(
        repo,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
        &Identity::Owner,
    )
}

pub fn descends(repo: &Path, ancestor: &str, head: &str) -> bool {
    git(
        repo,
        &["merge-base", "--is-ancestor", ancestor, head],
        &Identity::Owner,
    )
    .is_ok()
}

/// What the push did.
#[derive(Debug, PartialEq, Eq)]
pub enum Pushed {
    Done,
    /// The remote refused it (a non-fast-forward, a rule) or git never sent it.
    Refused(String),
    /// git failed in a way that leaves the remote's state unknown.
    Unknown(String),
}

/// `git push <remote> <sha>:refs/heads/<default>`, never forced.
pub fn push(repo: &Path, remote: &str, sha: &str, default: &str, identity: &Identity) -> Pushed {
    let refspec = format!("{sha}:refs/heads/{default}");
    match git(repo, &["push", "--porcelain", remote, &refspec], identity) {
        Ok(_) => Pushed::Done,
        Err(error) if error.starts_with("run git:") => Pushed::Refused(error),
        Err(error)
            if error.contains("[rejected]")
                || error.contains("[remote rejected]")
                || error.contains("non-fast-forward")
                || error.contains("fetch first") =>
        {
            Pushed::Refused(error)
        }
        Err(error) => Pushed::Unknown(error),
    }
}

/// The local default branch's head.
pub fn local_default(repo: &Path, default: &str) -> Result<String, String> {
    branch_head(repo, default)
}

/// Fast-forwards the local `default` from `old` to `sha` (P12). A worktree
/// that has `default` checked out merges it there with `--ff-only`, so its
/// files follow the ref; otherwise the ref moves only if it still holds
/// `old`. Either way a default branch that moved since `old` is never
/// overwritten.
pub fn advance(repo: &Path, default: &str, old: &str, sha: &str) -> Pushed {
    let owner = Identity::Owner;
    let listed = match git(repo, &["worktree", "list", "--porcelain"], &owner) {
        Ok(listed) => listed,
        Err(error) => return Pushed::Refused(error),
    };
    let wanted = format!("branch refs/heads/{default}");
    let mut path = None;
    let mut checked_out = None;
    for line in listed.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_owned());
        } else if line == wanted {
            checked_out = path.clone();
        }
    }
    let moved = match &checked_out {
        Some(worktree) => {
            if branch_head(Path::new(worktree), default).as_deref() != Ok(old) {
                return Pushed::Refused(format!("{default} moved from {old}"));
            }
            git(
                Path::new(worktree),
                &["merge", "--ff-only", "--quiet", sha],
                &owner,
            )
        }
        None => git(
            repo,
            &["update-ref", &format!("refs/heads/{default}"), sha, old],
            &owner,
        ),
    };
    match moved {
        Ok(_) => Pushed::Done,
        Err(error) => match local_default(repo, default) {
            // The ref is readable: it either reached `sha` or was left alone.
            Ok(head) if head == sha => Pushed::Done,
            Ok(_) => Pushed::Refused(error),
            Err(_) => Pushed::Unknown(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(""), "");
        assert_eq!(base64("f"), "Zg==");
        assert_eq!(base64("fo"), "Zm8=");
        assert_eq!(base64("foo"), "Zm9v");
        assert_eq!(base64("x-access-token:abc"), "eC1hY2Nlc3MtdG9rZW46YWJj");
    }
}
