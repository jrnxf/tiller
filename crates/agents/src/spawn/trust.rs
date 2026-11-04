//! Claude Code's workspace trust for the directory a session launches in.
//!
//! Claude Code stops at an interactive "Is this a project you created or
//! one you trust?" dialog in any folder its user config has not seen, and
//! `--permission-mode` does not skip it in an interactive pane. Tiller never
//! answers a prompt in a pane (G29), so a session launched into a new
//! worktree or checkout would wait there for the owner. Before every launch
//! line is written, [`record`] sets `hasTrustDialogAccepted: true` on the
//! launch directory's entry under `projects` in
//! `${CLAUDE_CONFIG_DIR:-$HOME}/.claude.json`, and on the primary checkout's
//! entry when the directory is a linked worktree, since Claude Code reads
//! the project root's entry first.
//!
//! The file is the owner's live user config, which every running Claude
//! Code process also rewrites, so the write is careful:
//!
//! - only the trust flag is ever set; every other key, entry and value is
//!   kept byte for byte in its order, and the external-imports consent
//!   (`hasClaudeMdExternalIncludesApproved`) is never written;
//! - a file that is not a JSON object, or whose `projects` or entry is not
//!   an object, is refused and left alone;
//! - the write holds the lock Claude Code's own writers take
//!   (`.claude.json.lock`, a directory beside the file), so it never lands
//!   between another writer's read and rename; the new file is written
//!   beside the store and renamed over it, and the rename is skipped when
//!   the file changed since it was read; a later read checks the flag is
//!   still there.
//!
//! A flag that cannot be recorded refuses the launch.

use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::SpawnError;

/// The flag Claude Code writes when the owner picks "Yes, I trust this
/// folder".
pub const FLAG: &str = "hasTrustDialogAccepted";

/// How many read-modify-write rounds a store that keeps moving gets.
const ATTEMPTS: usize = 3;

/// How long a launch waits for another writer's lock.
const LOCK_WAIT: Duration = Duration::from_secs(10);

/// How old an untouched lock is before it counts as abandoned. A holder
/// refreshes its lock well inside this; it is the lock library's default,
/// which Claude Code keeps.
const STALE: Duration = Duration::from_secs(10);

/// Where Claude Code keeps its user config for the sessions tiller
/// launches: `CLAUDE_CONFIG_DIR` when tiller's environment sets it, which
/// the launch script then sets too, or else `$HOME`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeConfig {
    var: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl ClaudeConfig {
    /// Read from this process's `CLAUDE_CONFIG_DIR` and `HOME`.
    pub fn from_env() -> Self {
        let read = |key| {
            std::env::var_os(key)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            var: read("CLAUDE_CONFIG_DIR"),
            home: read("HOME"),
        }
    }

    /// As if `CLAUDE_CONFIG_DIR` were `dir`.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self {
            var: Some(dir.into()),
            home: None,
        }
    }

    /// The `CLAUDE_CONFIG_DIR` a launch script sets, when tiller's own
    /// environment set one.
    pub fn forwarded(&self) -> Option<&Path> {
        self.var.as_deref()
    }

    /// The file Claude Code reads its user config from: `.claude.json` in
    /// `CLAUDE_CONFIG_DIR` or `$HOME`, unless an older install's
    /// `.config.json` in the config home (`CLAUDE_CONFIG_DIR`, or else
    /// `~/.claude`) is still in use, as Claude Code decides it.
    pub fn store(&self) -> Result<PathBuf, String> {
        let current = self.current()?;
        let legacy = match &self.var {
            Some(var) => var.join(".config.json"),
            None => current.with_file_name(".claude").join(".config.json"),
        };
        Ok(match fs::metadata(&legacy) {
            Ok(meta)
                if meta.is_file()
                    && (meta.len() > 0
                        || (meta.permissions().mode() & 0o222 != 0
                            && fs::symlink_metadata(&current).is_err())) =>
            {
                legacy
            }
            _ => current,
        })
    }

    /// `.claude.json` in `CLAUDE_CONFIG_DIR` or `$HOME`.
    fn current(&self) -> Result<PathBuf, String> {
        let dir = match (&self.var, &self.home) {
            (Some(var), _) if var.is_absolute() => var,
            (Some(var), _) => {
                return Err(format!(
                    "CLAUDE_CONFIG_DIR {} is not an absolute path, so the session's cwd would decide which config it reads",
                    var.display()
                ));
            }
            (None, Some(home)) => home,
            (None, None) => return Err("neither CLAUDE_CONFIG_DIR nor HOME is set".into()),
        };
        Ok(dir.join(".claude.json"))
    }
}

/// Records workspace trust for `dir` (and its primary checkout when `dir`
/// is a linked worktree) before a launch into it. Returns the paths it
/// covers; refuses the launch when the flag cannot be recorded.
pub fn record(config: &ClaudeConfig, dir: &Path) -> Result<Vec<PathBuf>, SpawnError> {
    let refused = |store: &str, why: String| {
        SpawnError::Refused(format!(
            "Claude Code would stop at its workspace-trust dialog in {}: could not record {FLAG} in {store}: {why}; nothing was launched",
            dir.display()
        ))
    };
    let store = config.store().map_err(|why| refused(".claude.json", why))?;
    let shown = store.display().to_string();
    let dirs = launch_dirs(dir).map_err(|why| refused(&shown, why))?;
    write(&store, &dirs).map_err(|why| refused(&shown, why))?;
    Ok(dirs)
}

/// `dir` resolved, then the primary checkout of a linked worktree. Claude
/// Code keys `projects` by the physical path its cwd resolves to.
fn launch_dirs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let dir = fs::canonicalize(dir).map_err(|e| {
        format!(
            "the launch directory {} cannot be resolved: {e}",
            dir.display()
        )
    })?;
    let mut dirs = vec![dir.clone()];
    if let Some(primary) = primary_checkout(&dir)?.filter(|primary| *primary != dir) {
        dirs.push(primary);
    }
    Ok(dirs)
}

/// The primary checkout a linked worktree belongs to: the parent of the
/// common git dir, when that is its `.git`. `None` for anything else. A
/// linked worktree git cannot read refuses, since its primary checkout
/// would go unrecorded.
fn primary_checkout(dir: &Path) -> Result<Option<PathBuf>, String> {
    let linked = dir.join(".git").is_file();
    let failed = |why: String| {
        if linked {
            Err(format!(
                "{} is a linked worktree whose primary checkout git cannot name: {why}",
                dir.display()
            ))
        } else {
            Ok(None)
        }
    };
    let out = match Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .output()
    {
        Ok(out) if out.status.success() => out,
        Ok(out) => return failed(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
        Err(e) => return failed(format!("git did not run: {e}")),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().map(fs::canonicalize);
    let (Some(Ok(git_dir)), Some(Ok(common))) = (lines.next(), lines.next()) else {
        return failed(format!("git rev-parse printed {text:?}"));
    };
    if git_dir == common || common.file_name().is_none_or(|name| name != ".git") {
        return Ok(None);
    }
    let Some(primary) = common.parent().map(Path::to_path_buf) else {
        return Ok(None);
    };
    match fs::canonicalize(primary.join(".git")) {
        Ok(git) if git == common => Ok(Some(primary)),
        _ => Ok(None),
    }
}

/// The store's read-modify-write, under Claude Code's lock.
fn write(store: &Path, dirs: &[PathBuf]) -> Result<(), String> {
    let keys = dirs
        .iter()
        .map(|dir| {
            dir.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{} is not a UTF-8 path", dir.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parent = store
        .parent()
        .ok_or_else(|| "the store has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    // Claude Code names its lock after the path it opens, link or not.
    let _lock = lock(&store.with_file_name(format!(
        "{}.lock",
        store.file_name().map_or_default(|n| n.to_string_lossy())
    )))?;
    // A symlinked store (a dotfiles manager) is written through: the target
    // is replaced, the link stays.
    let store = match fs::symlink_metadata(store) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(store)
            .map_err(|e| format!("it is a symlink whose target cannot be resolved: {e}"))?,
        _ => store.to_path_buf(),
    };
    let dir = store
        .parent()
        .ok_or_else(|| "the store has no parent directory".to_owned())?;
    let name = store
        .file_name()
        .map_or_default(|n| n.to_string_lossy().into_owned());
    for round in 1..=ATTEMPTS {
        let original = read(&store)?;
        let marked = match &original {
            Some(bytes) => std::str::from_utf8(bytes)
                .map_err(|_| "it is not UTF-8 text".to_owned())
                .and_then(|text| mark(text, &keys)),
            None => mark("{}", &keys),
        };
        let next = match marked {
            Ok(Some(next)) => next,
            Ok(None) => return Ok(()),
            // A writer that ignores the lock may be part way through a
            // write; a file still unreadable on the last round is refused.
            Err(why) if round == ATTEMPTS => return Err(why),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let mode = match fs::metadata(&store) {
            Ok(meta) if !meta.is_file() => return Err("it is not a regular file".into()),
            Ok(meta) => meta.permissions().mode() & 0o777,
            Err(_) => 0o600,
        };
        let partial = dir.join(format!("{name}.tiller-{}", unique()));
        let staged = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&partial)?;
            file.write_all(next.as_bytes())?;
            file.sync_all()
        })();
        if let Err(e) = staged {
            let _ = fs::remove_file(&partial);
            return Err(format!("cannot write {}: {e}", partial.display()));
        }
        // A store that moved since the read holds a change this write would
        // lose: drop this round and read it again.
        if read(&store)? != original {
            let _ = fs::remove_file(&partial);
            continue;
        }
        if let Err(e) = fs::rename(&partial, &store) {
            let _ = fs::remove_file(&partial);
            return Err(format!("cannot replace it: {e}"));
        }
        // A Claude Code process that read the store before the rename can
        // write it back without the flag; the next round puts it back.
        let back = read(&store)?.unwrap_or_default();
        let back = std::str::from_utf8(&back).map_err(|_| "it is not UTF-8 text".to_owned())?;
        if mark(back, &keys)?.is_none() {
            return Ok(());
        }
    }
    Err(format!(
        "it kept changing under the write, so it was left as it was after {ATTEMPTS} attempts"
    ))
}

fn read(store: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::read(store) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read it: {e}")),
    }
}

/// A name no other writer picks: the process, the time and a counter.
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Claude Code's lock on its config: a directory that exists while a
/// writer holds it. Removed when dropped.
struct Lock(PathBuf);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Takes the lock at `path`, waiting out a live holder and clearing one
/// abandoned past [`STALE`], the way Claude Code's writers do.
fn lock(path: &Path) -> Result<Lock, String> {
    let started = Instant::now();
    loop {
        match fs::create_dir(path) {
            Ok(()) => return Ok(Lock(path.to_path_buf())),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let stale = fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| at.elapsed().ok())
                    .is_some_and(|age| age > STALE);
                if stale {
                    let _ = fs::remove_dir(path);
                    continue;
                }
                if started.elapsed() >= LOCK_WAIT {
                    return Err(format!(
                        "another writer held its lock {} for {}s",
                        path.display(),
                        LOCK_WAIT.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("cannot take its lock {}: {e}", path.display())),
        }
    }
}

/// `text` with the flag set on every key's entry, or `None` when every
/// entry already has it. Refuses anything that is not the shape Claude Code
/// writes rather than guess at it.
fn mark(text: &str, keys: &[String]) -> Result<Option<String>, String> {
    let mut root: Object =
        serde_json::from_str(text).map_err(|e| format!("it is not a JSON object ({e})"))?;
    let mut projects = match root.get("projects") {
        Some(raw) => Object::parse(raw).ok_or("its \"projects\" value is not an object")?,
        None => Object::default(),
    };
    let mut changed = false;
    for key in keys {
        let mut entry = match projects.get(key) {
            Some(raw) => Object::parse(raw)
                .ok_or_else(|| format!("its projects entry for {key} is not an object"))?,
            None => Object::default(),
        };
        if entry.get(FLAG).map(Node::text) == Some("true") {
            continue;
        }
        entry.set(FLAG, Node::Raw(raw("true")?));
        projects.set(key, Node::Object(entry));
        changed = true;
    }
    if !changed {
        return Ok(None);
    }
    root.set("projects", Node::Object(projects));
    // Two-space indent is what Claude Code writes, so the values kept raw
    // sit at the depth they were read at; the trailing whitespace is kept.
    let mut next = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    next.push_str(&text[text.trim_end().len()..]);
    Ok(Some(next))
}

fn raw(json: &str) -> Result<Box<RawValue>, String> {
    RawValue::from_string(json.to_owned()).map_err(|e| e.to_string())
}

/// A JSON object read with its keys in order and its values kept as the
/// exact text they were read from, so writing it back changes only what
/// was set. `serde_json::Value` sorts keys in this workspace (no
/// `preserve_order`), which would reorder the owner's whole file.
#[derive(Debug, Default)]
struct Object(Vec<(String, Node)>);

#[derive(Debug)]
enum Node {
    Raw(Box<RawValue>),
    Object(Object),
}

impl Object {
    fn parse(node: &Node) -> Option<Self> {
        match node {
            Node::Raw(raw) => serde_json::from_str(raw.get()).ok(),
            Node::Object(_) => None,
        }
    }

    /// The last value under `key`, the one `JSON.parse` keeps.
    fn get(&self, key: &str) -> Option<&Node> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    fn set(&mut self, key: &str, node: Node) {
        match self.0.iter_mut().rev().find(|(k, _)| k == key) {
            Some((_, slot)) => *slot = node,
            None => self.0.push((key.to_owned(), node)),
        }
    }
}

impl Node {
    /// The text of a value kept raw.
    fn text(&self) -> &str {
        match self {
            Node::Raw(raw) => raw.get(),
            Node::Object(_) => "",
        }
    }
}

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Pairs;
        impl<'de> Visitor<'de> for Pairs {
            type Value = Object;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Object, A::Error> {
                let mut pairs = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    pairs.push((key, Node::Raw(value)));
                }
                Ok(Object(pairs))
            }
        }
        deserializer.deserialize_map(Pairs)
    }
}

impl Serialize for Object {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, node) in &self.0 {
            match node {
                Node::Raw(raw) => map.serialize_entry(key, raw)?,
                Node::Object(object) => map.serialize_entry(key, object)?,
            }
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use serde_json::Value;
    use tempfile::TempDir;

    use super::*;

    /// A temp `CLAUDE_CONFIG_DIR` and a launch directory, never the real
    /// `~/.claude.json`.
    struct Lab {
        dir: TempDir,
    }

    impl Lab {
        fn new() -> Self {
            let lab = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            fs::create_dir_all(lab.config_dir()).unwrap();
            fs::create_dir_all(lab.work()).unwrap();
            lab
        }

        fn config_dir(&self) -> PathBuf {
            fs::canonicalize(self.dir.path()).unwrap().join("claude")
        }

        fn config(&self) -> ClaudeConfig {
            ClaudeConfig::at(self.config_dir())
        }

        fn store(&self) -> PathBuf {
            self.config_dir().join(".claude.json")
        }

        fn work(&self) -> PathBuf {
            fs::canonicalize(self.dir.path()).unwrap().join("work")
        }

        fn key(&self) -> String {
            self.work().to_string_lossy().into_owned()
        }

        fn put(&self, text: &str) {
            fs::write(self.store(), text).unwrap();
        }

        fn text(&self) -> String {
            fs::read_to_string(self.store()).unwrap()
        }

        fn record(&self) -> Result<Vec<PathBuf>, SpawnError> {
            record(&self.config(), &self.work())
        }

        /// What the config dir holds besides the store.
        fn strays(&self) -> Vec<String> {
            fs::read_dir(self.config_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != ".claude.json")
                .collect()
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn a_missing_store_is_created_with_the_flag() {
        let lab = Lab::new();
        assert_eq!(lab.record().unwrap(), vec![lab.work()]);
        let back: Value = serde_json::from_str(&lab.text()).unwrap();
        assert_eq!(back["projects"][lab.key()][FLAG], true);
        let mode = fs::metadata(lab.store()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(lab.strays().is_empty(), "{:?}", lab.strays());
    }

    #[test]
    fn every_other_key_is_kept_in_order_and_byte_for_byte() {
        let lab = Lab::new();
        let before = r#"{
  "numStartups": 12,
  "zeta": {
    "nested": [
      1,
      2.50,
      1e400
    ]
  },
  "projects": {
    "/home/user/other": {
      "allowedTools": [],
      "hasTrustDialogAccepted": false
    }
  },
  "alpha": "\u00e9",
  "oauthAccount": {
    "emailAddress": "user@example.com"
  }
}"#;
        lab.put(before);
        fs::set_permissions(lab.store(), fs::Permissions::from_mode(0o640)).unwrap();
        lab.record().unwrap();
        let expected = format!(
            r#"{{
  "numStartups": 12,
  "zeta": {{
    "nested": [
      1,
      2.50,
      1e400
    ]
  }},
  "projects": {{
    "/home/user/other": {{
      "allowedTools": [],
      "hasTrustDialogAccepted": false
    }},
    "{}": {{
      "hasTrustDialogAccepted": true
    }}
  }},
  "alpha": "\u00e9",
  "oauthAccount": {{
    "emailAddress": "user@example.com"
  }}
}}"#,
            lab.key()
        );
        assert_eq!(lab.text(), expected);
        let mode = fs::metadata(lab.store()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640, "the store keeps its mode");
        assert!(lab.strays().is_empty(), "{:?}", lab.strays());
    }

    #[test]
    fn an_existing_entry_is_merged_in_place() {
        let lab = Lab::new();
        let key = lab.key();
        lab.put(&format!(
            "{{\n  \"projects\": {{\n    \"{key}\": {{\n      \"allowedTools\": [\n        \"Bash\"\n      ],\n      \"hasTrustDialogAccepted\": false,\n      \"hasClaudeMdExternalIncludesApproved\": false,\n      \"lastCost\": 0.25\n    }}\n  }}\n}}\n"
        ));
        lab.record().unwrap();
        assert_eq!(
            lab.text(),
            format!(
                "{{\n  \"projects\": {{\n    \"{key}\": {{\n      \"allowedTools\": [\n        \"Bash\"\n      ],\n      \"hasTrustDialogAccepted\": true,\n      \"hasClaudeMdExternalIncludesApproved\": false,\n      \"lastCost\": 0.25\n    }}\n  }}\n}}\n"
            ),
            "only the trust flag moved; the external-imports consent was left as it was"
        );
    }

    #[test]
    fn external_imports_consent_is_never_written() {
        let lab = Lab::new();
        lab.record().unwrap();
        assert!(!lab.text().contains("hasClaudeMdExternalIncludes"));
    }

    #[test]
    fn an_entry_already_trusted_is_not_rewritten() {
        let lab = Lab::new();
        let before = format!(
            "{{\"projects\":{{\"{}\":{{\"hasTrustDialogAccepted\":true}}}}}}",
            lab.key()
        );
        lab.put(&before);
        let inode = |lab: &Lab| {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(lab.store()).unwrap().ino()
        };
        let was = inode(&lab);
        lab.record().unwrap();
        assert_eq!(lab.text(), before);
        assert_eq!(inode(&lab), was, "the store was not replaced");
    }

    #[test]
    fn a_corrupt_store_is_refused_and_left_alone() {
        let lab = Lab::new();
        let key = lab.key();
        for corrupt in [
            String::new(),
            "{\"projects\": {".into(),
            "[]".into(),
            "null".into(),
            "{\"projects\": []}".into(),
            format!("{{\"projects\": {{\"{key}\": \"yes\"}}}}"),
        ] {
            lab.put(&corrupt);
            let error = lab.record().unwrap_err().to_string();
            assert!(error.contains("workspace-trust dialog"), "{error}");
            assert!(error.contains("nothing was launched"), "{error}");
            assert_eq!(lab.text(), corrupt, "{corrupt:?} was rewritten");
            assert!(lab.strays().is_empty(), "{:?}", lab.strays());
        }
    }

    #[test]
    fn a_relative_config_dir_is_refused() {
        let lab = Lab::new();
        let error = record(&ClaudeConfig::at("claude"), &lab.work())
            .unwrap_err()
            .to_string();
        assert!(error.contains("not an absolute path"), "{error}");
    }

    #[test]
    fn home_is_used_without_a_config_dir() {
        let config = ClaudeConfig {
            var: None,
            home: Some("/home/user".into()),
        };
        assert_eq!(
            config.store().unwrap(),
            PathBuf::from("/home/user/.claude.json")
        );
        assert_eq!(config.forwarded(), None);
        assert!(ClaudeConfig::default().store().is_err());
    }

    #[test]
    fn a_symlinked_store_is_written_through_the_link() {
        let lab = Lab::new();
        let target = lab.dir.path().join("dotfiles.json");
        fs::write(&target, "{}").unwrap();
        symlink(&target, lab.store()).unwrap();
        lab.record().unwrap();
        assert!(
            fs::symlink_metadata(lab.store())
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let back: Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(back["projects"][lab.key()][FLAG], true);
    }

    #[test]
    fn a_linked_worktree_registers_its_primary_checkout_too() {
        let lab = Lab::new();
        let primary = lab.work();
        git(&primary, &["init", "-q", "-b", "main"]);
        git(&primary, &["commit", "-q", "--allow-empty", "-m", "start"]);
        let worktree = fs::canonicalize(lab.dir.path()).unwrap().join("wt");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "task",
                &worktree.to_string_lossy(),
            ],
        );
        let dirs = record(&lab.config(), &worktree).unwrap();
        assert_eq!(dirs, vec![worktree.clone(), primary.clone()]);
        let back: Value = serde_json::from_str(&lab.text()).unwrap();
        for dir in [&worktree, &primary] {
            assert_eq!(back["projects"][dir.to_string_lossy().as_ref()][FLAG], true);
        }
        // The primary checkout itself is one entry.
        assert_eq!(record(&lab.config(), &primary).unwrap(), vec![primary]);
    }

    #[test]
    fn concurrent_writers_all_land() {
        let lab = Lab::new();
        lab.put("{\n  \"keep\": 1\n}");
        let dirs: Vec<PathBuf> = (0..8)
            .map(|i| {
                let dir = lab.work().join(format!("d{i}"));
                fs::create_dir_all(&dir).unwrap();
                dir
            })
            .collect();
        std::thread::scope(|scope| {
            for dir in &dirs {
                let config = lab.config();
                scope.spawn(move || record(&config, dir).unwrap());
            }
        });
        let back: Value = serde_json::from_str(&lab.text()).unwrap();
        assert_eq!(back["keep"], 1);
        for dir in &dirs {
            assert_eq!(back["projects"][dir.to_string_lossy().as_ref()][FLAG], true);
        }
    }

    #[test]
    fn a_live_claude_lock_is_waited_out() {
        let lab = Lab::new();
        let held = lab.config_dir().join(".claude.json.lock");
        fs::create_dir(&held).unwrap();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| lab.record());
            std::thread::sleep(Duration::from_millis(200));
            // Nothing lands while Claude Code holds its lock.
            assert!(!lab.store().exists());
            fs::remove_dir(&held).unwrap();
            writer.join().unwrap().unwrap();
        });
        let back: Value = serde_json::from_str(&lab.text()).unwrap();
        assert_eq!(back["projects"][lab.key()][FLAG], true);
        assert!(lab.strays().is_empty(), "{:?}", lab.strays());
    }

    #[test]
    fn an_abandoned_claude_lock_is_cleared() {
        let lab = Lab::new();
        let held = lab.config_dir().join(".claude.json.lock");
        fs::create_dir(&held).unwrap();
        let old = SystemTime::now() - Duration::from_secs(60);
        File::open(&held).unwrap().set_modified(old).unwrap();
        lab.record().unwrap();
        assert!(lab.strays().is_empty(), "{:?}", lab.strays());
    }

    #[test]
    fn an_older_installs_config_file_is_the_one_written() {
        let lab = Lab::new();
        let legacy = lab.config_dir().join(".config.json");
        fs::write(&legacy, "{\"keep\": 1}").unwrap();
        assert_eq!(lab.config().store().unwrap(), legacy);
        lab.record().unwrap();
        let back: Value = serde_json::from_str(&fs::read_to_string(&legacy).unwrap()).unwrap();
        assert_eq!(back["keep"], 1);
        assert_eq!(back["projects"][lab.key()][FLAG], true);
        assert!(!lab.store().exists());

        // An empty one stands aside once the current file exists.
        fs::write(&legacy, "").unwrap();
        lab.put("{}");
        assert_eq!(lab.config().store().unwrap(), lab.store());
    }

    #[test]
    fn a_worktree_git_cannot_read_is_refused() {
        let lab = Lab::new();
        fs::write(lab.work().join(".git"), "gitdir: /home/user/gone\n").unwrap();
        let error = lab.record().unwrap_err().to_string();
        assert!(error.contains("linked worktree"), "{error}");
        assert!(!lab.store().exists());
    }
}
