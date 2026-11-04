//! The memory repo's commit path (`memory.md` section 7): a per-repo lock,
//! a temporary index built from `HEAD` so a commit carries only its one file,
//! the scrub before `HEAD` moves, and the one working-tree file written last.
//!
//! Nothing here writes outside `<repo>/.git` and the one canonical file the
//! approval names.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use tiller_agents::land::scrub::ScrubList;
use tiller_core::memory::EMPTY_BLOB;
use tiller_core::state::memory::FileState;
use tiller_core::{Error, Result, ids};

use super::scrub;

/// The name and address Tiller commits to the memory repo as.
const AUTHOR: (&str, &str) = ("Tiller", "tiller@localhost");

/// The host's memory repo, `~/.tiller/memory`.
#[derive(Clone, Debug)]
pub struct Repo {
    root: PathBuf,
}

/// The per-repo lock, held from the base check to the last write.
#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] File);

/// A commit that moved `HEAD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Committed {
    pub blob: String,
    pub commit: String,
}

fn internal(what: &str, error: impl std::fmt::Display) -> Error {
    Error::Internal(format!("memory repo: {what}: {error}"))
}

/// A canonical path: relative, with plain components only.
fn checked(path: &str) -> Result<&str> {
    let plain = !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
    if plain {
        Ok(path)
    } else {
        Err(Error::Internal(format!(
            "memory repo: {path:?} is not a canonical path"
        )))
    }
}

impl Repo {
    pub fn at(home: &Path) -> Self {
        Self {
            root: home.join("memory"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn git_dir(&self) -> PathBuf {
        self.root.join(".git")
    }

    fn command(&self, index: Option<&Path>) -> Command {
        let mut git = Command::new("git");
        git.arg("-c")
            .arg("core.hooksPath=/dev/null")
            .env("GIT_DIR", self.git_dir())
            .env("GIT_WORK_TREE", &self.root)
            .env("GIT_AUTHOR_NAME", AUTHOR.0)
            .env("GIT_AUTHOR_EMAIL", AUTHOR.1)
            .env("GIT_COMMITTER_NAME", AUTHOR.0)
            .env("GIT_COMMITTER_EMAIL", AUTHOR.1)
            .env_remove("GIT_INDEX_FILE")
            .current_dir(&self.root);
        if let Some(index) = index {
            git.env("GIT_INDEX_FILE", index);
        }
        git
    }

    /// Runs git; `Ok(None)` when it exits non-zero.
    fn try_git(
        &self,
        index: Option<&Path>,
        args: &[&str],
        input: Option<&str>,
    ) -> Result<Option<String>> {
        Ok(self
            .try_git_bytes(index, args, input)?
            .map(|out| String::from_utf8_lossy(&out).into_owned()))
    }

    /// `try_git` with its output as bytes.
    fn try_git_bytes(
        &self,
        index: Option<&Path>,
        args: &[&str],
        input: Option<&str>,
    ) -> Result<Option<Vec<u8>>> {
        let mut git = self.command(index);
        git.args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = git.spawn().map_err(|e| internal("git did not start", e))?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| internal("git stdin", e))?;
        }
        let out = child.wait_with_output().map_err(|e| internal("git", e))?;
        Ok(out.status.success().then_some(out.stdout))
    }

    fn git(&self, index: Option<&Path>, args: &[&str], input: Option<&str>) -> Result<String> {
        self.try_git(index, args, input)?
            .ok_or_else(|| internal("git failed", args.join(" ")))
    }

    fn git_bytes(&self, args: &[&str], input: &str) -> Result<Vec<u8>> {
        self.try_git_bytes(None, args, Some(input))?
            .ok_or_else(|| internal("git failed", args.join(" ")))
    }

    /// Takes the lock, making the repo first when the host has none yet.
    pub fn lock(&self) -> Result<Lock> {
        if !self.git_dir().is_dir() {
            fs::create_dir_all(&self.root).map_err(|e| internal("create", e))?;
            let root = self.root.to_string_lossy();
            let made = Command::new("git")
                .args(["init", "--quiet", "--initial-branch=main", &root])
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|e| internal("git did not start", e))?;
            if !made.success() {
                return Err(internal("git init failed", root));
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.git_dir().join("tiller.lock"))
            .map_err(|e| internal("lock file", e))?;
        file.lock().map_err(|e| internal("lock", e))?;
        Ok(Lock(file))
    }

    fn head(&self) -> Result<Option<String>> {
        if !self.git_dir().is_dir() {
            return Ok(None);
        }
        Ok(self
            .try_git(
                None,
                &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
                None,
            )?
            .map(|oid| oid.trim().to_owned()))
    }

    /// The text of `path` at `rev`; empty when it is not there.
    fn text_at(&self, rev: &str, path: &str) -> Result<String> {
        Ok(self
            .try_git(None, &["cat-file", "blob", &format!("{rev}:{path}")], None)?
            .unwrap_or_default())
    }

    /// The text of each path at `HEAD`, empty when it is not there, from one
    /// git process (the document list reads every file).
    pub fn head_texts(&self, paths: &[String]) -> Result<Vec<String>> {
        if paths.is_empty() || self.head()?.is_none() {
            return Ok(vec![String::new(); paths.len()]);
        }
        let mut input = String::new();
        for path in paths {
            input.push_str("HEAD:");
            input.push_str(checked(path)?);
            input.push('\n');
        }
        let out = self.git_bytes(&["cat-file", "--batch"], &input)?;
        let mut rest = out.as_slice();
        let mut texts = Vec::with_capacity(paths.len());
        for _ in paths {
            let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            let header = String::from_utf8_lossy(&rest[..end]).into_owned();
            rest = rest.get(end + 1..).unwrap_or_default();
            // `<oid> blob <size>`, then the bytes; `<name> missing` otherwise.
            let mut parts = header.rsplit(' ');
            let size = parts.next().and_then(|s| s.parse::<usize>().ok());
            match (size, parts.next()) {
                (Some(size), Some("blob")) if size <= rest.len() => {
                    texts.push(String::from_utf8_lossy(&rest[..size]).into_owned());
                    rest = rest.get(size + 1..).unwrap_or_default();
                }
                _ => texts.push(String::new()),
            }
        }
        Ok(texts)
    }

    /// The file as Tiller checks it before every change.
    pub fn state(&self, path: &str, list: &ScrubList) -> Result<FileState> {
        let path = checked(path)?;
        let head_blob = match self.head()? {
            Some(_) => self
                .try_git(
                    None,
                    &["rev-parse", "--verify", "--quiet", &format!("HEAD:{path}")],
                    None,
                )?
                .map_or_else(|| EMPTY_BLOB.to_owned(), |b| b.trim().to_owned()),
            None => EMPTY_BLOB.to_owned(),
        };
        let file = self.root.join(path);
        let work_blob = if file.is_file() {
            self.git(None, &["hash-object", "--", path], None)?
                .trim()
                .to_owned()
        } else {
            EMPTY_BLOB.to_owned()
        };
        let text = if head_blob == EMPTY_BLOB {
            String::new()
        } else {
            self.git(None, &["cat-file", "blob", &head_blob], None)?
        };
        let clean = scrub::check(&text, list).is_none();
        Ok(FileState {
            head_blob,
            work_blob,
            text,
            clean,
        })
    }

    /// The text of a blob, for the revision detail.
    pub fn blob(&self, blob: &str) -> Result<Option<String>> {
        if blob == EMPTY_BLOB {
            return Ok(Some(String::new()));
        }
        if !self.git_dir().is_dir() {
            return Ok(None);
        }
        self.try_git(None, &["cat-file", "blob", blob], None)
    }

    /// Commits `diff` to `path` alone, under the lock the caller holds.
    /// `Err(Error::Refused)` when the diff does not apply, touches another
    /// path, or the scrub flags it; `HEAD` and the working tree are then
    /// as they were.
    pub fn commit(
        &self,
        _lock: &Lock,
        path: &str,
        diff: &str,
        message: &str,
        list: &ScrubList,
    ) -> Result<Committed> {
        let path = checked(path)?;
        let index = self
            .git_dir()
            .join(format!("tiller-index-{}", ids::new_ulid()));
        let done = self.commit_with(&index, path, diff, message, list);
        let _ = fs::remove_file(&index);
        done
    }

    fn commit_with(
        &self,
        index: &Path,
        path: &str,
        diff: &str,
        message: &str,
        list: &ScrubList,
    ) -> Result<Committed> {
        let refuse = |fact: String| super::refused("memory document", path, fact);
        let old = self.head()?;
        match &old {
            Some(head) => self.git(Some(index), &["read-tree", head], None)?,
            None => self.git(Some(index), &["read-tree", "--empty"], None)?,
        };
        let listed = self.git(Some(index), &["ls-files", "--stage", "--", path], None)?;
        if listed.is_empty() {
            let empty = self.git(None, &["hash-object", "-w", "--stdin"], Some(""))?;
            let entry = format!("100644,{},{path}", empty.trim());
            self.git(
                Some(index),
                &["update-index", "--add", "--cacheinfo", &entry],
                None,
            )?;
        }
        let applied = self.try_git(
            Some(index),
            &["apply", "--cached", "--whitespace=nowarn", "-"],
            Some(diff),
        )?;
        if applied.is_none() {
            return Err(refuse(
                "the change does not apply to the file at HEAD".into(),
            ));
        }
        let changed = match &old {
            Some(head) => self.git(
                Some(index),
                &["diff-index", "--cached", "--name-only", head],
                None,
            )?,
            None => self.git(Some(index), &["ls-files"], None)?,
        };
        if changed.lines().collect::<Vec<_>>() != [path] {
            return Err(refuse(
                "the change touches another file than its own".into(),
            ));
        }
        let mode = self.git(Some(index), &["ls-files", "--stage", "--", path], None)?;
        if !mode.starts_with("100644 ") {
            return Err(refuse("the change is not a plain file edit".into()));
        }
        let finding = scrub::check_diff(diff, list)
            .or_else(|| scrub::check(message, list).map(|f| format!("{f} of the commit message")));
        if let Some(finding) = finding {
            return Err(refuse(format!("the scrub flagged it: {finding}")));
        }
        let tree = self.git(Some(index), &["write-tree"], None)?;
        let blob = self
            .git(Some(index), &["rev-parse", &format!(":{path}")], None)?
            .trim()
            .to_owned();
        let mut args = vec!["commit-tree", "--no-gpg-sign", tree.trim()];
        if let Some(head) = &old {
            args.extend(["-p", head]);
        }
        args.extend(["-F", "-"]);
        let commit = self.git(None, &args, Some(message))?.trim().to_owned();
        let zero = "0".repeat(commit.len());
        let expected = old.as_deref().unwrap_or(&zero);
        let moved = self.try_git(
            None,
            &[
                "update-ref",
                "-m",
                "tiller: memory apply",
                "HEAD",
                &commit,
                expected,
            ],
            None,
        )?;
        if moved.is_none() {
            return Err(refuse("HEAD moved while the change was committed".into()));
        }
        if let Err(error) = self.check_out(path, &blob) {
            // HEAD goes back, so HEAD, the index and the file agree again.
            let back = match &old {
                Some(head) => vec![
                    "update-ref",
                    "-m",
                    "tiller: memory apply undone",
                    "HEAD",
                    head,
                    &commit,
                ],
                None => vec!["update-ref", "-d", "HEAD", &commit],
            };
            self.try_git(None, &back, None)?;
            return Err(error);
        }
        Ok(Committed { blob, commit })
    }

    /// Writes the one file from its new blob, then its entry in the real
    /// index; every other staged change stays as it was. On a failure the
    /// file is put back as it was.
    fn check_out(&self, path: &str, blob: &str) -> Result<()> {
        let text = self.git(None, &["cat-file", "blob", blob], None)?;
        let file = self.root.join(path);
        let dir = file.parent().unwrap_or(&self.root);
        fs::create_dir_all(dir).map_err(|e| internal("create", e))?;
        // Only a plain file is kept to restore; a link is never followed.
        let before = fs::symlink_metadata(&file)
            .is_ok_and(|m| m.is_file())
            .then(|| fs::read(&file).ok())
            .flatten();
        let replace = |bytes: &[u8]| {
            let temp = dir.join(format!(".tiller-{}", ids::new_ulid()));
            let done = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, &file));
            if done.is_err() {
                let _ = fs::remove_file(&temp);
            }
            done
        };
        let written = replace(text.as_bytes())
            .map_err(|e| internal("write", e))
            .and_then(|()| {
                self.git(None, &["update-index", "--add", "--", path], None)
                    .map(drop)
            });
        if written.is_err() {
            let _ = match &before {
                Some(bytes) => replace(bytes),
                None => fs::remove_file(&file),
            };
        }
        written
    }

    /// `commit`'s change to `path` reverted onto the text at `HEAD`: the
    /// same three-way merge `git revert` makes. `Ok(None)` on a conflict.
    pub fn revert(&self, path: &str, commit: &str, current: &str) -> Result<Option<String>> {
        let path = checked(path)?;
        let after = self.text_at(commit, path)?;
        let before = self.text_at(&format!("{commit}^"), path)?;
        let dir = self
            .git_dir()
            .join(format!("tiller-revert-{}", ids::new_ulid()));
        fs::create_dir_all(&dir).map_err(|e| internal("create", e))?;
        let write = |name: &str, text: &str| -> Result<String> {
            let file = dir.join(name);
            fs::write(&file, text).map_err(|e| internal("write", e))?;
            Ok(file.to_string_lossy().into_owned())
        };
        let merged = (|| {
            let ours = write("ours", current)?;
            let base = write("base", &after)?;
            let theirs = write("theirs", &before)?;
            self.try_git(
                None,
                &["merge-file", "-p", "--quiet", &ours, &base, &theirs],
                None,
            )
        })();
        let _ = fs::remove_dir_all(&dir);
        merged
    }
}
