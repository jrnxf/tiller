//! The git side of `land_direct` (P11, `authority.md` section 6): fetch the
//! default branch, check the bound head descends from it, and push that one
//! SHA as a plain fast-forward. The remote refuses anything else, so a moved
//! default branch fails the land instead of being overwritten.
//!
//! The publish of a direct-pr run (`publish.rs`) pushes the run's branch with
//! the same [`push`].
//!
//! A `local-only` project (P12) has no remote: the same checks run against
//! the local default branch, which [`advance`] fast-forwards in the
//! repository the run's worktree shares.
//!
//! Every call here runs as the owner, outside the sandbox, in a repository a
//! worker can write. So no call runs a hook or an fsmonitor, and every call
//! that fetches, pushes, reads a worktree or writes one ([`fetch_default`],
//! [`descends`], [`push`], [`uncommitted`], [`advance`]) runs in a
//! [`CleanRoom`]: the repository's own config never applies to them, since a
//! filter driver, `core.sshCommand`, `remote.<name>.uploadpack` or a
//! credential helper written there would run as a command too. The calls
//! left in the repository only read refs.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use super::forge::{AppToken, Identity};

/// How long one push may run before the daemon stops it: a remote that
/// never answers must not hold the publish or the land pass.
pub const PUSH_TIMEOUT: Duration = Duration::from_secs(120);

/// How long one fetch may run, for the same reason.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Config every call carries, above the repository's own: no hook and no
/// fsmonitor runs.
const HARDENED: [(&str, &str); 2] = [("core.hooksPath", "/dev/null"), ("core.fsmonitor", "false")];

/// A `git` command with no repository chosen for it by the environment.
fn command() -> Command {
    let mut command = Command::new("git");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    command
}

/// What one `git` call in `repo` printed, or why it could not run.
fn git(repo: &Path, args: &[&str], identity: &Identity) -> Result<String, String> {
    let mut command = command();
    command.arg("-C").arg(repo).args(args);
    run(command, identity, None)
}

/// Runs `command` with the hardened config, and `identity`'s token when it
/// has one; `timeout` stops a call that runs over.
fn run(
    mut command: Command,
    identity: &Identity,
    timeout: Option<Duration>,
) -> Result<String, String> {
    // Never prompt; never pick up a credential helper's interactive login.
    command.env("GIT_TERMINAL_PROMPT", "0").stdin(Stdio::null());
    let mut config: Vec<(&str, String)> = HARDENED
        .iter()
        .map(|(key, value)| (*key, (*value).to_owned()))
        .collect();
    if let Identity::App(token) = identity {
        // The token goes in the environment, not on the command line, so the
        // process list never shows it.
        config.push(("http.https://github.com/.extraheader", basic_header(token)));
    }
    command.env("GIT_CONFIG_COUNT", config.len().to_string());
    for (i, (key, value)) in config.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{i}"), key)
            .env(format!("GIT_CONFIG_VALUE_{i}"), value);
    }
    let output = match timeout {
        None => command
            .output()
            .map_err(|error| format!("run git: {error}"))?,
        Some(limit) => within(command, limit)?,
    };
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

/// `command`'s output, or an error once it has run for `limit`; the child
/// is killed then. A call that could not start reads `run git: ...`.
fn within(mut command: Command, limit: Duration) -> Result<Output, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("run git: {error}"))?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git ran past {}s and was stopped", limit.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("wait for git: {error}")),
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// A bare git dir only the daemon writes, which borrows a repository's
/// objects through `objects/info/alternates`. Git in it reads the owner's
/// global and system config and its own, never the repository's config,
/// hooks or `info/attributes`. Removed on drop.
struct CleanRoom {
    dir: tempfile::TempDir,
    /// The repository's own git dir, which holds the worktree's index.
    git_dir: PathBuf,
    /// The repository's object store, where a fetch stores what it gets.
    objects: PathBuf,
}

impl CleanRoom {
    fn open(repo: &Path) -> Result<Self, String> {
        let dirs = git(
            repo,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
            ],
            &Identity::Owner,
        )?;
        let mut lines = dirs.lines();
        let (Some(git_dir), Some(common)) = (lines.next(), lines.next()) else {
            return Err(format!("git rev-parse printed {dirs:?}"));
        };
        let dir = tempfile::Builder::new()
            .prefix("tiller-git-")
            .tempdir()
            .map_err(|error| format!("create a clean git dir: {error}"))?;
        let mut init = command();
        init.args(["init", "--bare", "--quiet"]).arg(dir.path());
        run(init, &Identity::Owner, None)?;
        std::fs::write(
            dir.path().join("objects/info/alternates"),
            format!("{common}/objects\n"),
        )
        .map_err(|error| format!("write the clean git dir's alternates: {error}"))?;
        Ok(Self {
            dir,
            git_dir: PathBuf::from(git_dir),
            objects: Path::new(common).join("objects"),
        })
    }

    fn command(&self) -> Command {
        let mut command = command();
        command.arg("--git-dir").arg(self.dir.path());
        command
    }

    /// Like [`Self::command`], but new objects go to the repository's
    /// object store, so they outlive the room.
    fn storing(&self) -> Command {
        let mut command = self.command();
        command.env("GIT_OBJECT_DIRECTORY", &self.objects);
        command
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

/// Fetches the remote default branch from a [`CleanRoom`], into the
/// repository's object store, and returns its head. `remote` is a remote of
/// `repo` or a URL.
pub fn fetch_default(
    repo: &Path,
    remote: &str,
    default: &str,
    identity: &Identity,
) -> Result<String, String> {
    let url = remote_url(repo, remote, false)?;
    let room = CleanRoom::open(repo)?;
    let mut fetch = room.storing();
    fetch.args([
        "fetch",
        "--quiet",
        "--no-tags",
        "--",
        &url,
        &format!("refs/heads/{default}"),
    ]);
    run(fetch, identity, Some(FETCH_TIMEOUT))?;
    let mut head = room.storing();
    head.args(["rev-parse", "--verify", "FETCH_HEAD^{commit}"]);
    run(head, &Identity::Owner, None)
}

/// Whether `head` descends from `ancestor` (or is it), judged in a
/// [`CleanRoom`].
pub fn descends(repo: &Path, ancestor: &str, head: &str) -> bool {
    let Ok(room) = CleanRoom::open(repo) else {
        return false;
    };
    let mut check = room.command();
    check.args(["merge-base", "--is-ancestor", ancestor, head]);
    run(check, &Identity::Owner, None).is_ok()
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

/// `git push <remote> <sha>:refs/heads/<branch>`, never forced, from a
/// [`CleanRoom`]. `remote` is a remote of `repo` or a URL.
pub fn push(repo: &Path, remote: &str, sha: &str, branch: &str, identity: &Identity) -> Pushed {
    let url = match remote_url(repo, remote, true) {
        Ok(url) => url,
        Err(error) => return Pushed::Refused(error),
    };
    let room = match CleanRoom::open(repo) {
        Ok(room) => room,
        Err(error) => return Pushed::Refused(format!("nothing was sent: {error}")),
    };
    let refspec = format!("{sha}:refs/heads/{branch}");
    let mut command = room.command();
    command.args(["push", "--porcelain", "--", &url, &refspec]);
    match run(command, identity, Some(PUSH_TIMEOUT)) {
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

/// Where a push (or a fetch) to `remote` goes: its push (or fetch) URL when
/// `repo` names it, else `remote` itself. A URL that would read as an option
/// or names a transport helper (`ext::`, `fd::`), which runs a command, is
/// refused.
fn remote_url(repo: &Path, remote: &str, push: bool) -> Result<String, String> {
    let mut args = vec!["remote", "get-url"];
    if push {
        args.push("--push");
    }
    args.push(remote);
    let url = git(repo, &args, &Identity::Owner).unwrap_or_else(|_| remote.to_owned());
    let helper = url.split_once("::").is_some_and(|(transport, _)| {
        !transport.is_empty()
            && transport
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    });
    if url.is_empty() || url.starts_with('-') || helper {
        return Err(format!(
            "nothing was sent: the remote URL {url:?} is refused"
        ));
    }
    Ok(url)
}

/// The tracked files a worktree changed and did not commit against `head`,
/// as `git status --porcelain` lists them; empty when there are none. It
/// runs in a [`CleanRoom`] on the worktree's index, without taking its lock,
/// so no filter driver or fsmonitor the repository names runs. Untracked
/// files are left out: while a sandboxed command runs, the sandbox keeps
/// empty read-only placeholders (`.bashrc`, `.gitconfig`, `.mcp.json` and
/// the like) in its working directory, and the host sees them there.
pub fn uncommitted(repo: &Path, head: &str) -> Result<String, String> {
    let room = CleanRoom::open(repo)?;
    let mut point = room.command();
    point.args(["update-ref", "--no-deref", "HEAD", head]);
    run(point, &Identity::Owner, None)?;
    let mut status = room.command();
    status
        .arg("--work-tree")
        .arg(repo)
        .arg("--no-optional-locks")
        .args([
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--ignore-submodules=all",
        ])
        .env("GIT_INDEX_FILE", room.git_dir.join("index"));
    run(status, &Identity::Owner, None)
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
        Some(worktree) => fast_forward(Path::new(worktree), repo, default, old, sha),
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

/// Moves `default` from `old` to `sha` in the worktree that has it checked
/// out, and the files with it. The files change in a [`CleanRoom`] with the
/// worktree's own index, so no smudge filter or `core.fsmonitor` the
/// repository names runs; a worktree edit that the move would overwrite
/// refuses it. The ref moves last, and only from `old`.
fn fast_forward(
    worktree: &Path,
    repo: &Path,
    default: &str,
    old: &str,
    sha: &str,
) -> Result<String, String> {
    if branch_head(worktree, default).as_deref() != Ok(old) {
        return Err(format!("{default} moved from {old}"));
    }
    let room = CleanRoom::open(worktree)?;
    let mut ancestor = room.command();
    ancestor.args(["merge-base", "--is-ancestor", old, sha]);
    run(ancestor, &Identity::Owner, None)
        .map_err(|_| format!("{sha} is not a fast-forward of {default} at {old}"))?;
    let index = room.git_dir.join("index");
    let mut files = room.command();
    files
        .arg("--work-tree")
        .arg(worktree)
        .args(["read-tree", "-m", "-u", old, sha])
        .env("GIT_INDEX_FILE", &index);
    run(files, &Identity::Owner, None)?;
    let moved = git(
        repo,
        &["update-ref", &format!("refs/heads/{default}"), sha, old],
        &Identity::Owner,
    );
    if moved.is_err() {
        // Put the files back where the ref still is.
        let mut back = room.command();
        back.arg("--work-tree")
            .arg(worktree)
            .args(["read-tree", "-m", "-u", sha, old])
            .env("GIT_INDEX_FILE", &index);
        let _ = run(back, &Identity::Owner, None);
    }
    moved
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
