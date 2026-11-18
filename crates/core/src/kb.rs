//! The kb loader (`memory.md` section 3): reads the owner's kb store,
//! read-only, picks the notes a launch of a project loads, and renders them
//! into the role prompt.
//!
//! kb is the owner's note store: one file per note, `notes/<scope>/<id>.md`,
//! YAML front matter between `---` lines, then the body. This module reads
//! the few front matter fields the loader needs with a small parser of its
//! own, so a note kb writes in a form it does not know is skipped as
//! `unreadable`, never guessed at. `kb_store_fixture_parses` pins the
//! format.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The kinds a fleet role may load, in load order. `open-work` is a kb kind
/// too, and never loads.
pub const KINDS: [&str; 4] = ["convention", "decision", "gotcha", "how-to"];

/// The kb kind that never loads into a fleet role.
pub const OPEN_WORK: &str = "open-work";

/// The `memory.kb_scopes` entry for the project's own repo scope.
pub const REPO: &str = "repo";

/// The `memory.kb_scopes` entry, and kb scope, for notes of every repo.
pub const GLOBAL: &str = "global";

/// The reason a note that does not parse is skipped.
pub const UNREADABLE: &str = "unreadable";

/// The reason a note that does not fit the startup budget is skipped.
pub const OVER_BUDGET: &str = "over budget";

/// The text under the `# Memory` heading, before the first note.
pub const INTRO: &str = "These notes come from the owner's kb store. Follow them. To suggest a \
    new note, run `tiller mem note`; it waits for the owner and loads nowhere until kept.";

/// The refusal of a Keep with no kb command, and the Health warning.
pub const NO_COMMAND: &str = "tiller cannot find the kb command: set memory.kb_command";

/// Why the `[memory]` table of `config.toml` was not read.
#[derive(Debug, thiserror::Error)]
#[error("memory: {0}")]
pub struct ConfigError(String);

/// The `[memory]` table of `config.toml`. Keys of the old memory store
/// (`memory.remote`, `memory.review_days`) are ignored, and `tiller doctor`
/// names each one still set.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct KbConfig {
    /// The kb scopes this host loads: `global`, `repo` (the project's own
    /// repo scope) or an exact repo id.
    pub kb_scopes: Vec<String>,
    /// The kb command Keep runs: program, then arguments. When it is not
    /// set, the kb plugin in the Claude Code plugin list.
    pub kb_command: Option<Vec<String>>,
}

impl Default for KbConfig {
    fn default() -> Self {
        Self {
            kb_scopes: vec![REPO.to_owned(), GLOBAL.to_owned()],
            kb_command: None,
        }
    }
}

impl KbConfig {
    /// The `[memory]` table of a parsed `config.toml`; none is the default.
    pub fn from_table(table: &toml::Table) -> Result<Self, ConfigError> {
        let Some(memory) = table.get("memory") else {
            return Ok(Self::default());
        };
        let config: Self = memory
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError(e.message().to_owned()))?;
        if let Some(bad) = config
            .kb_scopes
            .iter()
            .find(|scope| *scope != REPO && !valid_scope(scope))
        {
            return Err(ConfigError(format!(
                "kb_scopes holds {bad:?}, which is neither \"repo\", \"global\" nor a repo id"
            )));
        }
        if config.kb_command.as_ref().is_some_and(|c| c.is_empty()) {
            return Err(ConfigError(
                "kb_command names a program, then its arguments".to_owned(),
            ));
        }
        Ok(config)
    }

    /// The kb scopes a launch in a project with `repo` loads, in load order:
    /// the repo scope, then exact repo ids in config order, then `global`.
    /// A project with no repo has no repo scope.
    pub fn scopes(&self, repo: Option<&str>) -> Vec<String> {
        let mut scopes = Vec::new();
        let mut push = |scope: String| {
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        };
        if self.kb_scopes.iter().any(|s| s == REPO)
            && let Some(scope) = repo.and_then(repo_scope)
        {
            push(scope);
        }
        for scope in self.kb_scopes.iter().filter(|s| *s != REPO && *s != GLOBAL) {
            push(scope.clone());
        }
        if self.kb_scopes.iter().any(|s| s == GLOBAL) {
            push(GLOBAL.to_owned());
        }
        scopes
    }

    /// The command Keep runs: `kb_command`, else `bun <installPath>/cli/main.ts`
    /// of the `kb@` entry in `plugins/installed_plugins.json` under the
    /// Claude config home. `None` with neither.
    pub fn command(&self, claude_home: Option<&Path>) -> Option<Vec<String>> {
        if let Some(command) = &self.kb_command {
            return Some(command.clone());
        }
        let file = claude_home?.join("plugins/installed_plugins.json");
        let list: serde_json::Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        let (_, entries) = list
            .get("plugins")?
            .as_object()?
            .iter()
            .find(|(key, _)| key.starts_with("kb@"))?;
        let path = entries.as_array()?.first()?.get("installPath")?.as_str()?;
        let main = Path::new(path).join("cli/main.ts");
        Some(vec!["bun".to_owned(), main.to_string_lossy().into_owned()])
    }
}

/// The kb store's home: `KB_HOME` in `env`, else `~/.kb`.
pub fn home(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(home) = var("KB_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home));
    }
    var("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".kb"))
}

/// The kb store's home from this process's environment.
pub fn home_from_env() -> Option<PathBuf> {
    home(|key| std::env::var(key).ok())
}

/// A repo URL as kb writes its scope: host and path, with no scheme, no
/// user, no port and no `.git`, lowercase, such as `github.com/example/app`.
/// `None` for a local path or a URL with fewer than two parts.
pub fn repo_scope(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.starts_with("file://") || url.starts_with('/') || url.starts_with('.')
    {
        return None;
    }
    let rest = match url.split_once("://") {
        Some((_, rest)) => {
            // `user@host:port/path`: drop the user, then the port.
            let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
            match rest.split_once('/') {
                Some((host, path)) => {
                    let host = host.split_once(':').map_or(host, |(h, _)| h);
                    format!("{host}/{path}")
                }
                None => rest.split_once(':').map_or(rest, |(h, _)| h).to_owned(),
            }
        }
        None => {
            // scp form, `user@host:path`.
            let rest = url.rsplit_once('@').map_or(url, |(_, r)| r);
            match rest.split_once(':') {
                Some((host, path)) => format!("{host}/{path}"),
                None => rest.to_owned(),
            }
        }
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let parts: Vec<String> = rest
        .split('/')
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.to_lowercase()
                .chars()
                .map(|c| {
                    if c.is_ascii_lowercase() || c.is_ascii_digit() || "._~-".contains(c) {
                        c
                    } else {
                        '-'
                    }
                })
                .collect()
        })
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let scope = parts.join("/");
    valid_scope(&scope).then_some(scope)
}

/// kb's scope rule: `global` or a repo id of lowercase parts.
fn valid_scope(scope: &str) -> bool {
    let mut parts = scope.split('/');
    let first_ok = parts.next().is_some_and(|first| {
        !first.is_empty()
            && first
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
    });
    first_ok
        && scope.split('/').skip(1).all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._~-".contains(c))
        })
        && scope.split('/').all(|part| part != "." && part != "..")
}

/// Whether the store could be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KbState {
    Ok,
    Missing,
    Unreadable,
}

impl KbState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
        }
    }
}

/// One note as the loader reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub scope: String,
    pub kind: String,
    pub indexed: bool,
    pub last_verified: String,
    pub body: String,
}

/// A file under `notes/` that did not parse: its path under `notes/`, and
/// the scope its directory names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unparsed {
    pub file: String,
    pub scope: String,
}

/// The store as one read found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub state: KbState,
    pub home: Option<PathBuf>,
    pub notes: Vec<Note>,
    pub unparsed: Vec<Unparsed>,
}

impl Read {
    /// A store that was not there to read.
    pub fn missing(home: Option<PathBuf>) -> Self {
        Self {
            state: KbState::Missing,
            home,
            notes: Vec::new(),
            unparsed: Vec::new(),
        }
    }
}

/// Reads every note under `<home>/notes`. `trash/` and `archive/` are
/// never read. A home with no `notes/` is missing; one whose `notes/`
/// cannot be listed is unreadable. A note that does not parse is kept in
/// [`Read::unparsed`] and the read goes on.
pub fn read(home: Option<&Path>) -> Read {
    let Some(home) = home else {
        return Read::missing(None);
    };
    let root = home.join("notes");
    let mut read = Read::missing(Some(home.to_path_buf()));
    match std::fs::metadata(&root) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            read.state = KbState::Unreadable;
            return read;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return read,
        Err(_) => {
            read.state = KbState::Unreadable;
            return read;
        }
    }
    let mut files = Vec::new();
    if walk(&root, &mut files).is_err() {
        read.state = KbState::Unreadable;
        return read;
    }
    read.state = KbState::Ok;
    files.sort();
    for path in files {
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| parse(&text));
        match parsed {
            Some(note) => read.notes.push(note),
            None => {
                let scope = Path::new(&relative)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                read.unparsed.push(Unparsed {
                    file: relative,
                    scope,
                });
            }
        }
    }
    read
}

/// Every `.md` file under `dir`, at any depth.
fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&path, files)?;
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "md") {
            files.push(path);
        }
    }
    Ok(())
}

/// One note file: front matter between `---` lines, then the body. Reads
/// `id`, `title`, `scope`, `kind`, `indexed` and `last_verified`, and
/// ignores every other field. `None` when a field it needs is missing or
/// not in a form it reads, or the body is empty.
pub fn parse(text: &str) -> Option<Note> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut id = None;
    let mut title = None;
    let mut scope = None;
    let mut kind = None;
    let mut indexed = None;
    let mut last_verified = None;
    let mut closed = false;
    for line in lines.by_ref() {
        let line = line.trim_end_matches(['\n', '\r']);
        if line == "---" {
            closed = true;
            break;
        }
        // A nested value (a block list, a continuation) is never one of the
        // fields read here, which are all plain top-level scalars.
        if line.is_empty() || line.starts_with([' ', '\t', '-', '#']) {
            continue;
        }
        let (key, value) = line.split_once(':')?;
        let slot = match key {
            "id" => &mut id,
            "title" => &mut title,
            "scope" => &mut scope,
            "kind" => &mut kind,
            "indexed" => &mut indexed,
            "last_verified" => &mut last_verified,
            _ => continue,
        };
        *slot = Some(scalar(value)?);
    }
    if !closed {
        return None;
    }
    let body: String = lines.collect();
    let body = body.trim().to_owned();
    let indexed = match indexed.as_deref() {
        None => true,
        Some("true" | "True" | "TRUE") => true,
        Some("false" | "False" | "FALSE") => false,
        Some(_) => return None,
    };
    let kind = kind?;
    let note = Note {
        id: id.filter(|v| !v.is_empty())?,
        title: title
            .map(|t| t.trim().to_owned())
            .filter(|v| !v.is_empty())?,
        scope: scope.filter(|s| valid_scope(s))?,
        indexed,
        last_verified: last_verified.filter(|v| !v.is_empty())?,
        body,
        kind,
    };
    let known = KINDS.contains(&note.kind.as_str()) || note.kind == OPEN_WORK;
    (known && !note.body.is_empty()).then_some(note)
}

/// One YAML scalar on one line: plain, double-quoted or single-quoted.
fn scalar(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    ' ' => out.push(' '),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    _ => return None,
                },
                c => out.push(c),
            }
        }
        let tail = chars.as_str().trim();
        return (tail.is_empty() || tail.starts_with('#')).then_some(out);
    }
    if let Some(rest) = value.strip_prefix('\'') {
        let mut out = String::new();
        let mut chars = rest.chars().peekable();
        loop {
            match chars.next()? {
                '\'' if chars.peek() == Some(&'\'') => {
                    chars.next();
                    out.push('\'');
                }
                '\'' => break,
                c => out.push(c),
            }
        }
        let tail: String = chars.collect();
        let tail = tail.trim();
        return (tail.is_empty() || tail.starts_with('#')).then_some(out);
    }
    // A flow collection, an anchor, a tag or a block scalar is not a form
    // the loader reads.
    if value.starts_with(['[', '{', '&', '*', '!', '|', '>']) {
        return None;
    }
    let plain = match value.find(" #") {
        Some(at) => &value[..at],
        None => value,
    };
    Some(plain.trim().to_owned())
}

/// The token count of a text, as the role file is counted: bytes divided
/// by four, rounded up (`memory.md` section 3).
pub fn tokens(text: &str) -> i64 {
    i64::try_from(text.len().div_ceil(4)).unwrap_or(i64::MAX)
}

/// The lowercase hex SHA-256 of a text.
pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// One note the loader considered, in load order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    /// `None` for a note that did not parse.
    pub kind: Option<String>,
    pub scope: String,
    /// The tokens of its rendered section.
    pub tokens: i64,
    pub loaded: bool,
    /// Why it did not load: `over budget` or `unreadable`.
    pub reason: Option<String>,
    /// The SHA-256 of its rendered section; empty for a note that did not
    /// parse.
    #[serde(skip)]
    pub sha256: String,
}

/// What a launch renders: the `# Memory` part of the prompt and the notes
/// it considered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub kb: KbState,
    /// Empty when no note loads; else the whole `# Memory` part.
    pub text: String,
    /// The SHA-256 of [`Self::text`].
    pub hash: String,
    /// Every note in the host's scopes that a launch would load or skip, in
    /// load order, then the files that did not parse.
    pub entries: Vec<Entry>,
    /// The tokens the role file and the loaded notes take.
    pub used: i64,
}

impl Rendered {
    /// The notes that loaded.
    pub fn loaded(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.loaded)
    }

    /// The notes that were skipped.
    pub fn skipped(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| !e.loaded)
    }
}

/// One note's rendered section.
pub fn section(note: &Note) -> String {
    format!(
        "## {}\n{} · {} · kb:{}\n\n{}\n",
        note.title, note.kind, note.scope, note.id, note.body
    )
}

/// The heading and intro before the first section.
fn heading() -> String {
    format!("# Memory\n\n{INTRO}\n\n")
}

/// Picks and renders the notes of `read` in `scopes` (load order), within
/// `budget` tokens of which the role file takes `role_tokens`. A note loads
/// when its scope is one of `scopes`, its kind is not `open-work` and it is
/// indexed. Notes are added in order while the total stays within the
/// budget; one that does not fit is skipped as `over budget`, and later
/// smaller ones may still fit.
pub fn render(read: &Read, scopes: &[String], role_tokens: i64, budget: i64) -> Rendered {
    let rank = |scope: &str| scopes.iter().position(|s| s == scope);
    let mut notes: Vec<(&Note, usize)> = read
        .notes
        .iter()
        .filter(|n| n.kind != OPEN_WORK && n.indexed)
        .filter_map(|n| rank(&n.scope).map(|r| (n, r)))
        .collect();
    let kind_rank = |kind: &str| KINDS.iter().position(|k| *k == kind).unwrap_or(KINDS.len());
    notes.sort_by(|(a, ra), (b, rb)| {
        ra.cmp(rb)
            .then_with(|| kind_rank(&a.kind).cmp(&kind_rank(&b.kind)))
            .then_with(|| b.last_verified.cmp(&a.last_verified))
            .then_with(|| a.id.cmp(&b.id))
    });
    let head = heading();
    let mut used = role_tokens;
    let mut sections = Vec::new();
    let mut entries = Vec::new();
    for (note, _) in notes {
        let text = section(note);
        let cost = tokens(&text);
        // The heading counts with the first note that loads.
        let extra = if sections.is_empty() {
            tokens(&head)
        } else {
            0
        };
        let fits = used + cost + extra <= budget;
        if fits {
            used += cost + extra;
        }
        entries.push(Entry {
            id: note.id.clone(),
            title: note.title.clone(),
            kind: Some(note.kind.clone()),
            scope: note.scope.clone(),
            tokens: cost,
            loaded: fits,
            reason: (!fits).then(|| OVER_BUDGET.to_owned()),
            sha256: sha256_hex(&text),
        });
        if fits {
            sections.push(text);
        }
    }
    for file in read.unparsed.iter().filter(|f| rank(&f.scope).is_some()) {
        entries.push(Entry {
            id: file.file.clone(),
            title: file.file.clone(),
            kind: None,
            scope: file.scope.clone(),
            tokens: 0,
            loaded: false,
            reason: Some(UNREADABLE.to_owned()),
            sha256: String::new(),
        });
    }
    let text = if sections.is_empty() {
        String::new()
    } else {
        format!("{head}{}", sections.join("\n"))
    };
    Rendered {
        kb: read.state,
        hash: sha256_hex(&text),
        text,
        entries,
        used,
    }
}

#[cfg(test)]
mod tests;
