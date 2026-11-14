//! A project's local checkout, as Add project sets it: `checkouts.<id>` in
//! `config.toml`, the folder treehouse cuts the project's worktrees from.
//!
//! The board names a folder that is already a checkout of the project, or a
//! folder to clone the project into. [`plan`] checks the folder against
//! that choice and says what adding the project will do; [`clone`] runs the
//! clone a plan asks for.

use std::collections::BTreeSet;
use std::fs::{File, TryLockError};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tiller_core::domain::forge::{self, Forge};
use tiller_panes::process::{self, Ended};

/// How long a clone may take before it is stopped.
pub const CLONE_LIMIT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CheckoutError {
    /// The folder does not suit the choice; the message says why.
    #[error("{0}")]
    Unsuitable(String),
    /// The clone ran and failed.
    #[error("{0}")]
    Clone(String),
}

fn unsuitable(message: impl Into<String>) -> CheckoutError {
    CheckoutError::Unsuitable(message.into())
}

/// What the board asked for.
#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub project: &'a str,
    /// `owner/name` on GitHub, when the project has one.
    pub repo: Option<&'a str>,
    /// The folder as typed; empty means `<checkouts_root>/<project>`.
    pub path: &'a str,
    /// Clone the project into the folder, instead of using a checkout that
    /// is there already.
    pub clone: bool,
    /// `spawn.checkouts_root`: the orchestrator starts only for a checkout
    /// under it.
    pub root: Option<&'a Path>,
    /// The home folder a leading `~` stands for.
    pub home: Option<&'a Path>,
    /// The folder of the locks a clone holds on its folder ([`Claim`]):
    /// `clone-locks` in the tiller home, shared by the daemon and every
    /// `peer serve`.
    pub locks: &'a Path,
}

/// What adding the project does with the folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// The folder is a checkout of the project already.
    Use { path: PathBuf },
    /// Clone `remote` into the folder, which is missing or empty.
    Clone { path: PathBuf, remote: String },
}

impl Plan {
    pub fn path(&self) -> &Path {
        match self {
            Self::Use { path } | Self::Clone { path, .. } => path,
        }
    }
}

/// Checks the folder against the request. Reads the disk and runs local git
/// only, so it is quick enough to answer as the folder is typed.
pub fn plan(request: &Request<'_>) -> Result<Plan, CheckoutError> {
    let path = folder(request)?;
    let shown = path.display();
    // A clone still running has made `.git` and its remote, so its folder
    // would pass for a finished clone below: an add sent again while it
    // runs, from this process or another, is refused instead.
    if path.exists() && Claim::running(request.locks, &path)? {
        return Err(unsuitable(format!(
            "a clone into {shown} is running already"
        )));
    }
    let repo = request.repo.map(str::trim).filter(|repo| !repo.is_empty());
    if let Some(repo) = repo {
        check_repo(repo)?;
    }
    if !request.clone {
        if !path.exists() {
            return Err(unsuitable(format!(
                "nothing is at {shown}; choose Clone to make it"
            )));
        }
        if !path.is_dir() {
            return Err(unsuitable(format!("{shown} is a file, not a folder")));
        }
        is_checkout_of(&path, repo)?;
        return Ok(Plan::Use { path });
    }
    let Some(repo) = repo else {
        return Err(unsuitable(
            "a clone needs the project's repo; fill in Repo as owner/name",
        ));
    };
    if path.is_dir() {
        // A clone that already ran, from an add sent again, is used as is.
        if top_level(&path).is_some_and(|top| same(&top, &path)) {
            is_checkout_of(&path, Some(repo))?;
            return Ok(Plan::Use { path });
        }
        let empty = std::fs::read_dir(&path)
            .map_err(|error| unsuitable(format!("{shown} cannot be read: {error}")))?
            .next()
            .is_none();
        if !empty {
            return Err(unsuitable(format!(
                "{shown} is not empty; give a new or empty folder to clone into"
            )));
        }
    } else if path.exists() {
        return Err(unsuitable(format!("{shown} is a file, not a folder")));
    } else if let Some(parent) = nearest(&path)
        && !parent.is_dir()
    {
        return Err(unsuitable(format!(
            "{} is a file, so {shown} cannot be made",
            parent.display()
        )));
    }
    Ok(Plan::Clone {
        path,
        remote: format!("https://github.com/{repo}.git"),
    })
}

/// The folder the request names: `~` expanded, the default filled in, and
/// checked to be absolute, without `..` and under the checkouts root.
fn folder(request: &Request<'_>) -> Result<PathBuf, CheckoutError> {
    let typed = request.path.trim();
    let path = if typed.is_empty() {
        let Some(root) = request.root else {
            return Err(unsuitable(
                "give a folder; spawn.checkouts_root is not set, so there is no default",
            ));
        };
        root.join(request.project)
    } else if typed == "~" || typed.starts_with("~/") {
        let Some(home) = request.home else {
            return Err(unsuitable("~ has no home folder to stand for here"));
        };
        home.join(typed.trim_start_matches('~').trim_start_matches('/'))
    } else {
        PathBuf::from(typed)
    };
    if !path.is_absolute() {
        return Err(unsuitable(format!(
            "{} is not a full path; start it with / or ~/",
            path.display()
        )));
    }
    if path.components().any(|part| part == Component::ParentDir) {
        return Err(unsuitable(format!(
            "{} has a `..` in it; give the folder without one",
            path.display()
        )));
    }
    // No `.` parts or trailing slash, as `checkouts.<id>` should read.
    let path: PathBuf = path.components().collect();
    if let Some(root) = request.root {
        let root: PathBuf = root.components().collect();
        if !path.starts_with(&root) || path == root {
            return Err(unsuitable(format!(
                "{} is not under spawn.checkouts_root ({}), where the orchestrator looks for checkouts",
                path.display(),
                root.display()
            )));
        }
    }
    Ok(path)
}

/// `owner/name`, in the characters GitHub allows.
fn check_repo(repo: &str) -> Result<(), CheckoutError> {
    let part = |text: &str| {
        !text.is_empty()
            && text != "."
            && text != ".."
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if part(owner) && part(name) => Ok(()),
        _ => Err(unsuitable(format!(
            "repo {repo} is not owner/name, like acme/web"
        ))),
    }
}

/// `path` is the top of a git checkout, and of `repo` when one is given.
fn is_checkout_of(path: &Path, repo: Option<&str>) -> Result<(), CheckoutError> {
    let shown = path.display();
    let Some(top) = top_level(path) else {
        return Err(unsuitable(format!("{shown} is not a git checkout")));
    };
    if !same(&top, path) {
        return Err(unsuitable(format!(
            "{shown} is inside the checkout at {}; give that folder",
            top.display()
        )));
    }
    let Some(repo) = repo else {
        return Ok(());
    };
    let found: Vec<String> = git(Some(path), &["remote", "-v"])
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|url| match forge::detect(url) {
            Forge::Github { repo } => Some(repo),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if found.iter().any(|other| other.eq_ignore_ascii_case(repo)) {
        return Ok(());
    }
    Err(unsuitable(match found.as_slice() {
        [] => format!("{shown} has no remote on GitHub for {repo}"),
        others => format!("{shown} is a checkout of {}, not {repo}", others.join(", ")),
    }))
}

/// The top of the checkout `path` is in, if it is in one.
fn top_level(path: &Path) -> Option<PathBuf> {
    git(Some(path), &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|out| PathBuf::from(out.trim()))
}

/// The same folder, through any symlinks.
fn same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The closest folder above `path` that exists.
fn nearest(path: &Path) -> Option<&Path> {
    path.ancestors().skip(1).find(|parent| parent.exists())
}

/// Runs the clone a [`Plan::Clone`] asks for; a [`Plan::Use`] does nothing.
/// A clone that fails or runs out of time removes the folder it made, or
/// empties the empty one it was given; folders made above it stay, empty.
/// Returns how to take the clone back, when it ran one, with the claim on
/// its folder: the caller holds it until the project is added or the clone
/// taken back, so no other add takes the folder in between.
pub fn clone(plan: &Plan, limit: Duration, locks: &Path) -> Result<Option<Cloned>, CheckoutError> {
    let Plan::Clone { path, remote } = plan else {
        return Ok(None);
    };
    let claim = Claim::take(locks, path)?;
    let made = claim_folder(path)?;
    let undo = |error: CheckoutError| {
        made.run();
        error
    };
    let stderr = tempfile::tempfile()
        .map_err(|error| CheckoutError::Clone(format!("git clone: {error}")))?;
    let mut command = no_prompt(Command::new("git"));
    // Its own process group, so a stop reaches the transport helpers git
    // starts as well as git itself.
    process::own_group(&mut command);
    // git holds the claim as its stdin, which it never reads: a git that
    // outlives this process keeps the folder claimed until it ends.
    let held = claim
        .lock
        .try_clone()
        .map_err(|error| CheckoutError::Clone(format!("git clone: {error}")))?;
    let mut child = command
        .args(["clone", "--quiet", "--", remote])
        .arg(path)
        .stdin(held)
        .stdout(Stdio::null())
        .stderr(
            stderr
                .try_clone()
                .map_err(|error| CheckoutError::Clone(format!("git clone: {error}")))?,
        )
        .spawn()
        .map_err(|error| CheckoutError::Clone(format!("git clone did not start: {error}")))?;
    let status =
        finish(&mut child, limit, remote).map_err(|why| undo(CheckoutError::Clone(why)))?;
    if !status.success() {
        let why = read_all(stderr);
        return Err(undo(CheckoutError::Clone(format!(
            "git clone of {remote} failed: {}",
            why.trim()
        ))));
    }
    Ok(Some(Cloned {
        undo: made,
        _claim: claim,
    }))
}

/// A clone [`clone`] ran: how to take it back, and the claim on its folder.
#[derive(Debug)]
pub struct Cloned {
    pub undo: Undo,
    _claim: Claim,
}

/// Makes the folder a clone goes into, now that it is claimed, and says how
/// to take the clone back. The plan saw the folder missing or empty, but
/// that was before the claim: a folder that has filled since is refused,
/// since its contents are not the clone's to remove.
fn claim_folder(path: &Path) -> Result<Undo, CheckoutError> {
    let shown = path.display();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| unsuitable(format!("{} cannot be made: {error}", parent.display())))?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => return Ok(Undo::Remove(path.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(unsuitable(format!("{shown} cannot be made: {error}"))),
    }
    if !path.is_dir() {
        return Err(unsuitable(format!("{shown} is a file, not a folder")));
    }
    let empty = std::fs::read_dir(path)
        .map_err(|error| unsuitable(format!("{shown} cannot be read: {error}")))?
        .next()
        .is_none();
    if !empty {
        return Err(unsuitable(format!(
            "{shown} is not empty; give a new or empty folder to clone into"
        )));
    }
    Ok(Undo::Empty(path.to_path_buf()))
}

/// Waits for the clone of `remote`, polling every 100 ms. Past `limit`, or
/// when waiting fails, git and every helper in its process group are
/// stopped (`tiller_panes::process`), and the error says why.
fn finish(
    child: &mut std::process::Child,
    limit: Duration,
    remote: &str,
) -> Result<std::process::ExitStatus, String> {
    match process::wait(
        child,
        Instant::now() + limit,
        Duration::from_millis(100),
        None,
    ) {
        Ended::Exited(status) => Ok(status),
        Ended::TimedOut => Err(format!(
            "the clone of {remote} took over {} and was stopped",
            span(limit)
        )),
        Ended::Failed(error) => Err(format!("git clone: {error}")),
        // No interrupt is passed, so none is caught.
        Ended::Interrupted => Err("git clone: interrupted".to_owned()),
    }
}

/// `limit` in whole minutes, or in seconds (rounded up) under a minute.
fn span(limit: Duration) -> String {
    let seconds = limit.as_millis().div_ceil(1000);
    let (count, unit) = if seconds < 60 {
        (seconds, "second")
    } else {
        (seconds / 60, "minute")
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural}")
}

/// Removes what a clone put in a folder that was empty before it.
fn empty_out(path: &Path) {
    for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
        let entry = entry.path();
        let _ = if entry.is_dir() {
            std::fs::remove_dir_all(&entry)
        } else {
            std::fs::remove_file(&entry)
        };
    }
}

/// How to take back a clone that [`clone`] ran, when what came after it
/// failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Undo {
    /// Remove this folder: the clone made it, and nothing above it.
    Remove(PathBuf),
    /// Empty this folder: it was there, empty, before the clone.
    Empty(PathBuf),
}

impl Undo {
    pub fn run(&self) {
        match self {
            Self::Remove(path) => {
                let _ = std::fs::remove_dir_all(path);
            }
            Self::Empty(path) => empty_out(path),
        }
    }
}

fn read_all(mut file: std::fs::File) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut text = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.read_to_string(&mut text);
    text
}

/// A lock on the folder a clone runs into, held while it runs: a second
/// clone into the same folder, or a plan that would take a half-made clone
/// for a finished one, is refused, in this process or another. The lock
/// file is in `locks`, named by a hash of the folder with its links
/// resolved, and the lock goes with the last handle on it, so a process
/// that dies gives it back. Only a clone makes a lock file: a plan reads
/// the ones there are.
#[derive(Debug)]
struct Claim {
    lock: File,
}

/// How long a clone waits for a plan that is reading its lock.
const CLAIM_GRACE: Duration = Duration::from_millis(500);

impl Claim {
    fn take(locks: &Path, path: &Path) -> Result<Self, CheckoutError> {
        let shown = path.display();
        let cannot = |error: std::io::Error| {
            unsuitable(format!("a clone into {shown} cannot be claimed: {error}"))
        };
        std::fs::create_dir_all(locks).map_err(cannot)?;
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_file(locks, path))
            .map_err(cannot)?;
        // A plan holds the lock for a moment as it reads it, so a clone
        // tries again for a while before it calls the folder taken.
        let since = Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(Self { lock }),
                Err(TryLockError::WouldBlock) if since.elapsed() < CLAIM_GRACE => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(unsuitable(format!(
                        "a clone into {shown} is running already"
                    )));
                }
                Err(TryLockError::Error(error)) => return Err(cannot(error)),
            }
        }
    }

    /// True while a clone holds the claim on `path`.
    fn running(locks: &Path, path: &Path) -> Result<bool, CheckoutError> {
        let cannot = |error: std::io::Error| {
            unsuitable(format!(
                "a clone into {} cannot be checked: {error}",
                path.display()
            ))
        };
        let lock = match File::open(lock_file(locks, path)) {
            Ok(lock) => lock,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(cannot(error)),
        };
        match lock.try_lock_shared() {
            Ok(()) => Ok(false),
            Err(TryLockError::WouldBlock) => Ok(true),
            Err(TryLockError::Error(error)) => Err(cannot(error)),
        }
    }
}

/// The lock file of `path` in `locks`. The path is keyed with the links of
/// its nearest existing folder resolved, so two spellings of one folder
/// share a lock.
fn lock_file(locks: &Path, path: &Path) -> PathBuf {
    let mut base = path;
    let mut rest = Vec::new();
    let resolved = loop {
        if let Ok(resolved) = base.canonicalize() {
            break resolved;
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                base = parent;
            }
            _ => break base.to_path_buf(),
        }
    };
    let key = rest.iter().rev().fold(resolved, |key, name| key.join(name));
    let name: String = Sha256::digest(key.as_os_str().as_encoded_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    locks.join(format!("{name}.lock"))
}

/// git with no question it could wait on: no password prompt, no ssh
/// prompt, and a stalled transfer given up after 20 seconds.
fn no_prompt(mut command: Command) -> Command {
    command
        .args(["-c", "http.lowSpeedLimit=1", "-c", "http.lowSpeedTime=20"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=20",
        )
        .stdin(Stdio::null());
    command
}

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    let out = no_prompt(command)
        .args(args)
        .output()
        .map_err(|error| format!("git {}: {error}", args[0]))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
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

    /// A bare origin with one commit on main, under `root`.
    fn origin(root: &Path) -> PathBuf {
        let seed = root.join("seed");
        run(
            root,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        run(root, &["clone", "--quiet", "origin.git", "seed"]);
        run(
            &seed,
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
        run(&seed, &["push", "--quiet", "origin", "HEAD:main"]);
        root.join("origin.git")
    }

    /// A checkout at `root/<name>` whose origin is `acme/<repo>` on GitHub.
    fn checkout(root: &Path, name: &str, repo: &str) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        run(&path, &["init", "--quiet", "-b", "main"]);
        run(
            &path,
            &[
                "remote",
                "add",
                "origin",
                &format!("https://github.com/acme/{repo}.git"),
            ],
        );
        path
    }

    fn request<'a>(path: &'a str, clone: bool, root: Option<&'a Path>) -> Request<'a> {
        Request {
            project: "web",
            repo: Some("acme/web"),
            path,
            clone,
            root,
            home: Some(Path::new("/home/user")),
            locks: Path::new("/home/user/.tiller/clone-locks"),
        }
    }

    fn refused(request: &Request<'_>) -> String {
        match plan(request) {
            Err(CheckoutError::Unsuitable(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn text(path: &Path) -> String {
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn an_existing_checkout_of_the_repo_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkout(dir.path(), "web", "web");
        let typed = format!("{}/", text(&path));
        assert_eq!(
            plan(&request(&typed, false, None)),
            Ok(Plan::Use { path: path.clone() })
        );
        // The repo is matched without regard to case, and no repo takes any
        // checkout.
        let mut upper = request(&typed, false, None);
        upper.repo = Some("Acme/Web");
        assert_eq!(plan(&upper), Ok(Plan::Use { path: path.clone() }));
        upper.repo = None;
        assert_eq!(plan(&upper), Ok(Plan::Use { path }));
    }

    #[test]
    fn a_folder_to_use_must_be_a_checkout_of_this_repo() {
        let dir = tempfile::tempdir().unwrap();
        let other = checkout(dir.path(), "other", "other");
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let nested = other.join("src");
        std::fs::create_dir(&nested).unwrap();
        let missing = dir.path().join("missing");

        let why = refused(&request(&text(&other), false, None));
        assert!(
            why.ends_with("is a checkout of acme/other, not acme/web"),
            "{why}"
        );
        let why = refused(&request(&text(&plain), false, None));
        assert!(why.ends_with("is not a git checkout"), "{why}");
        let why = refused(&request(&text(&file), false, None));
        assert!(why.ends_with("is a file, not a folder"), "{why}");
        let why = refused(&request(&text(&nested), false, None));
        assert!(why.contains("is inside the checkout at"), "{why}");
        let why = refused(&request(&text(&missing), false, None));
        assert!(why.starts_with("nothing is at"), "{why}");

        run(&other, &["remote", "remove", "origin"]);
        let why = refused(&request(&text(&other), false, None));
        assert!(
            why.ends_with("has no remote on GitHub for acme/web"),
            "{why}"
        );
    }

    #[test]
    fn a_clone_goes_into_a_new_or_empty_folder() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = dir.path().join("a/b/web");
        let remote = "https://github.com/acme/web.git".to_owned();
        assert_eq!(
            plan(&request(&text(&fresh), true, None)),
            Ok(Plan::Clone {
                path: fresh,
                remote: remote.clone()
            })
        );
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(
            plan(&request(&text(&empty), true, None)),
            Ok(Plan::Clone {
                path: empty.clone(),
                remote
            })
        );

        std::fs::write(empty.join("notes.txt"), "x").unwrap();
        let why = refused(&request(&text(&empty), true, None));
        assert!(
            why.ends_with("is not empty; give a new or empty folder to clone into"),
            "{why}"
        );
        let below_file = empty.join("notes.txt/web");
        let why = refused(&request(&text(&below_file), true, None));
        assert!(why.contains("is a file, so"), "{why}");

        let new = text(&dir.path().join("new"));
        let mut no_repo = request(&new, true, None);
        no_repo.repo = None;
        let why = refused(&no_repo);
        assert!(why.starts_with("a clone needs the project's repo"), "{why}");
        no_repo.repo = Some("not a repo");
        let why = refused(&no_repo);
        assert!(why.ends_with("is not owner/name, like acme/web"), "{why}");
    }

    #[test]
    fn a_clone_that_already_ran_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = checkout(dir.path(), "web", "web");
        assert_eq!(
            plan(&request(&text(&path), true, None)),
            Ok(Plan::Use { path: path.clone() })
        );
        let other = checkout(dir.path(), "other", "other");
        let why = refused(&request(&text(&other), true, None));
        assert!(
            why.ends_with("is a checkout of acme/other, not acme/web"),
            "{why}"
        );
    }

    #[test]
    fn the_folder_is_full_has_no_dots_and_sits_under_the_root() {
        let root = Path::new("/home/user/code");
        let why = refused(&request("code/web", true, Some(root)));
        assert!(why.starts_with("code/web is not a full path"), "{why}");
        let why = refused(&request("/home/user/code/../web", true, Some(root)));
        assert!(why.contains("has a `..` in it"), "{why}");
        let why = refused(&request("/srv/web", true, Some(root)));
        assert_eq!(
            why,
            "/srv/web is not under spawn.checkouts_root (/home/user/code), where the orchestrator looks for checkouts"
        );
        let why = refused(&request("/home/user/code", true, Some(root)));
        assert!(why.contains("is not under spawn.checkouts_root"), "{why}");
        let why = refused(&request("", true, None));
        assert!(why.contains("there is no default"), "{why}");
    }

    #[test]
    fn an_empty_folder_means_the_root_and_a_tilde_means_home() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("code");
        let Ok(Plan::Clone { path, .. }) = plan(&request("", true, Some(&root))) else {
            panic!("expected a clone");
        };
        assert_eq!(path, root.join("web"));

        let home = root.join("web");
        let mut tilde = request("~", true, Some(&root));
        tilde.home = Some(&home);
        for typed in ["~", "~/"] {
            tilde.path = typed;
            let Ok(Plan::Clone { path, .. }) = plan(&tilde) else {
                panic!("expected a clone for {typed}");
            };
            assert_eq!(path, home);
        }
        tilde.home = None;
        let why = refused(&tilde);
        assert!(why.starts_with("~ has no home folder"), "{why}");
    }

    #[test]
    fn a_clone_runs_and_can_be_taken_back() {
        let dir = tempfile::tempdir().unwrap();
        let origin = origin(dir.path());
        let path = dir.path().join("made/deeper/web");
        let plan = Plan::Clone {
            path: path.clone(),
            remote: text(&origin),
        };
        let undo = clone(&plan, CLONE_LIMIT, &dir.path().join("locks"))
            .unwrap()
            .map(|cloned| cloned.undo);
        assert_eq!(undo, Some(Undo::Remove(path.clone())));
        assert!(path.join(".git").is_dir());
        assert_eq!(top_level(&path).map(|top| same(&top, &path)), Some(true));
        undo.unwrap().run();
        assert!(!path.exists());
        // The folders made above it stay, empty.
        assert_eq!(
            std::fs::read_dir(dir.path().join("made/deeper"))
                .unwrap()
                .count(),
            0
        );

        // A folder that was there stays, emptied.
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let plan = Plan::Clone {
            path: empty.clone(),
            remote: text(&origin),
        };
        let undo = clone(&plan, CLONE_LIMIT, &dir.path().join("locks"))
            .unwrap()
            .map(|cloned| cloned.undo);
        assert_eq!(undo, Some(Undo::Empty(empty.clone())));
        undo.unwrap().run();
        assert!(empty.is_dir());
        assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);

        assert!(matches!(
            clone(
                &Plan::Use { path: empty },
                CLONE_LIMIT,
                &dir.path().join("locks")
            ),
            Ok(None)
        ));
    }

    #[test]
    fn a_failed_clone_leaves_nothing_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("made/web");
        let plan = Plan::Clone {
            path: path.clone(),
            remote: text(&dir.path().join("no-such-origin.git")),
        };
        let Err(CheckoutError::Clone(why)) = clone(&plan, CLONE_LIMIT, &dir.path().join("locks"))
        else {
            panic!("expected the clone to fail");
        };
        assert!(why.contains("git clone of"), "{why}");
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_dir(dir.path().join("made")).unwrap().count(),
            0
        );
    }

    #[test]
    fn taking_a_clone_back_leaves_a_clone_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let origin = origin(dir.path());
        let into = |name: &str| Plan::Clone {
            path: dir.path().join("new").join(name),
            remote: text(&origin),
        };
        let web = clone(&into("web"), CLONE_LIMIT, &dir.path().join("locks"))
            .unwrap()
            .unwrap();
        let api = clone(&into("api"), CLONE_LIMIT, &dir.path().join("locks"))
            .unwrap()
            .unwrap();
        web.undo.run();
        assert!(!dir.path().join("new/web").exists());
        assert!(dir.path().join("new/api/.git").is_dir());
        api.undo.run();
        assert!(!dir.path().join("new/api").exists());
    }

    #[test]
    fn a_folder_that_filled_after_the_plan_is_refused_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let origin = origin(dir.path());
        let path = dir.path().join("web");
        std::fs::create_dir(&path).unwrap();
        let plan = plan(&request(&text(&path), true, None)).unwrap();
        assert!(matches!(plan, Plan::Clone { .. }), "{plan:?}");
        let Plan::Clone { path, .. } = plan else {
            unreachable!()
        };
        // Another clone landed in it after the plan was made.
        std::fs::write(path.join("README"), "theirs").unwrap();
        let plan = Plan::Clone {
            path: path.clone(),
            remote: text(&origin),
        };
        assert_eq!(
            clone(&plan, CLONE_LIMIT, &dir.path().join("locks")).map(|_| ()),
            Err(CheckoutError::Unsuitable(format!(
                "{} is not empty; give a new or empty folder to clone into",
                path.display()
            )))
        );
        assert_eq!(
            std::fs::read_to_string(path.join("README")).unwrap(),
            "theirs"
        );
    }

    #[test]
    fn a_clone_past_its_limit_is_stopped_with_its_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("helper");
        let mut command = Command::new("sh");
        process::own_group(&mut command);
        let mut child = command
            .arg("-c")
            .arg(format!("sleep 30 & echo $! > '{}'; wait", helper.display()))
            .spawn()
            .unwrap();
        let started = Instant::now();
        let pid = loop {
            if let Ok(pid) = std::fs::read_to_string(&helper)
                && let Ok(pid) = pid.trim().parse::<i32>()
            {
                break pid;
            }
            assert!(started.elapsed() < Duration::from_secs(5), "no helper");
            std::thread::sleep(Duration::from_millis(10));
        };
        let why = finish(
            &mut child,
            Duration::from_millis(200),
            "https://example.com/web.git",
        )
        .unwrap_err();
        assert_eq!(
            why,
            "the clone of https://example.com/web.git took over 1 second and was stopped"
        );
        // Gone, or a zombie waiting on whoever reaps orphans here.
        let alive = || {
            let out = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout);
            !stat.trim().is_empty() && !stat.trim().starts_with('Z')
        };
        let started = Instant::now();
        while alive() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the helper outlived the stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn one_clone_into_a_folder_runs_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let path = Path::new("/home/user/code/web");
        let first = Claim::take(&locks, path).unwrap();
        let Err(CheckoutError::Unsuitable(why)) = Claim::take(&locks, path) else {
            panic!("expected the second claim to be refused");
        };
        assert_eq!(why, "a clone into /home/user/code/web is running already");
        drop(first);
        assert!(Claim::take(&locks, path).is_ok());
    }

    /// A clone still running has made `.git` and its remote, so its folder
    /// looks like a finished clone: an add sent again then is refused, not
    /// set to a folder the clone may yet take back.
    #[test]
    fn a_folder_a_clone_is_running_into_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let path = dir.path().join("web");
        std::fs::create_dir(&path).unwrap();
        let typed = text(&path);
        let request = Request {
            locks: &locks,
            ..request(&typed, true, None)
        };
        let running = Claim::take(&locks, &path).unwrap();
        assert_eq!(
            refused(&request),
            format!("a clone into {} is running already", path.display())
        );
        drop(running);
        assert!(matches!(plan(&request), Ok(Plan::Clone { .. })));
    }

    /// A plan reads the locks there are and makes none, and a clone waits
    /// for a plan that is reading its lock.
    #[test]
    fn a_plan_neither_makes_a_lock_nor_keeps_a_clone_out() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let path = dir.path().join("web");
        std::fs::create_dir(&path).unwrap();
        let typed = text(&path);
        let request = Request {
            locks: &locks,
            ..request(&typed, true, None)
        };
        assert!(plan(&request).is_ok());
        assert!(!locks.exists());
        drop(Claim::take(&locks, &path).unwrap());
        let reading = File::open(lock_file(&locks, &path)).unwrap();
        reading.try_lock_shared().unwrap();
        let unlock = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            drop(reading);
        });
        assert!(Claim::take(&locks, &path).is_ok());
        unlock.join().unwrap();
    }

    /// Two spellings of one folder, through a link, share one claim.
    #[test]
    fn a_folder_reached_through_a_link_shares_its_claim() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let held = Claim::take(&locks, &real.join("web")).unwrap();
        assert!(Claim::take(&locks, &linked.join("web")).is_err());
        assert!(Claim::take(&locks, &linked.join("./web")).is_err());
        drop(held);
        assert!(Claim::take(&locks, &linked.join("web")).is_ok());
    }

    /// git holds the claim as its stdin, so a git that outlives the process
    /// that started it keeps the folder claimed until it ends.
    #[test]
    fn a_claim_lasts_while_a_process_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let locks = dir.path().join("locks");
        let path = dir.path().join("web");
        let claim = Claim::take(&locks, &path).unwrap();
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(claim.lock.try_clone().unwrap())
            .spawn()
            .unwrap();
        drop(claim);
        assert!(Claim::take(&locks, &path).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(Claim::take(&locks, &path).is_ok());
    }

    #[test]
    fn a_limit_is_said_in_minutes_or_seconds() {
        assert_eq!(span(Duration::from_secs(600)), "10 minutes");
        assert_eq!(span(Duration::from_secs(60)), "1 minute");
        assert_eq!(span(Duration::from_secs(90)), "1 minute");
        assert_eq!(span(Duration::from_secs(30)), "30 seconds");
        assert_eq!(span(Duration::from_millis(200)), "1 second");
    }
}
