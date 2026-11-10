//! Bash tool calls: every command position the shell parser finds, through
//! `sh -c`, `eval`, wrappers and substitutions, against the rules of
//! `authority.md` sections 8 to 10.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::shell::{self, Redirect, Script, Simple, Word};
use super::{Code, Context, Denial, is_packaged, is_secret_path, path_denial, resolve};
use crate::roles::Role;

/// A word as the classifier sees it: its text, with every variable and
/// lookup it could resolve statically filled in.
#[derive(Clone, Debug)]
struct Arg {
    text: String,
    /// The text is exactly what the shell will pass.
    fixed: bool,
}

impl Arg {
    fn fixed(text: &str) -> Self {
        Arg {
            text: text.to_owned(),
            fixed: true,
        }
    }

    fn is(&self, text: &str) -> bool {
        self.fixed && self.text == text
    }
}

fn texts(args: &[Arg]) -> String {
    args.iter()
        .map(|arg| arg.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `gh pr review` may approve: `--approve`, `--approve=...`, `-a`
/// alone or in a cluster of short flags such as `-ab lgtm`, or a word the
/// guard cannot read that could be one of them.
fn review_approves(args: &[Arg]) -> bool {
    const WITH_VALUE: &[&str] = &["-b", "--body", "-F", "--body-file", "-R", "--repo"];
    let mut j = 0;
    while let Some(arg) = args.get(j) {
        let text = arg.text.as_str();
        if WITH_VALUE.contains(&text) {
            j += 2;
            continue;
        }
        if !arg.fixed || text == "--approve" || text.starts_with("--approve=") {
            return true;
        }
        if let Some(cluster) = text.strip_prefix('-').filter(|c| !c.starts_with('-'))
            // `-b`, `-F` and `-R` take the rest of the cluster as their value.
            && cluster
                .chars()
                .take_while(|c| !['b', 'F', 'R'].contains(c))
                .any(|c| c == 'a')
        {
            return true;
        }
        j += 1;
    }
    false
}

/// The base `gh pr edit` sets: `--base <b>`, `--base=<b>`, `-B <b>` or
/// `-B<b>`. A flag with no value after it is a base the guard cannot read.
fn pr_base(args: &[Arg]) -> Option<Arg> {
    let mut j = 0;
    while let Some(arg) = args.get(j) {
        let text = arg.text.as_str();
        if text == "--base" || text == "-B" {
            return Some(args.get(j + 1).cloned().unwrap_or(Arg {
                text: String::new(),
                fixed: false,
            }));
        }
        if let Some(value) = text
            .strip_prefix("--base=")
            .or_else(|| text.strip_prefix("-B").filter(|v| !v.is_empty()))
        {
            return Some(Arg {
                text: value.to_owned(),
                fixed: arg.fixed,
            });
        }
        if [
            "-R",
            "--repo",
            "-b",
            "--body",
            "-F",
            "--body-file",
            "-t",
            "--title",
        ]
        .contains(&text)
        {
            j += 2;
            continue;
        }
        j += 1;
    }
    None
}

fn basename(text: &str) -> &str {
    text.rsplit('/').next().unwrap_or(text)
}

/// `xargs` options that take the next word as their value.
const XARGS_WITH_ARG: &[&str] = &[
    "-L",
    "-n",
    "-P",
    "-s",
    "-d",
    "-E",
    "-a",
    "--arg-file",
    "--delimiter",
    "--max-args",
    "--max-procs",
    "--max-lines",
];

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish"];

const HTTP_CLIENTS: &[&str] = &[
    "curl", "wget", "http", "https", "xh", "httpie", "aria2c", "nc", "ncat", "netcat", "telnet",
    "lynx", "w3m",
];

const PTY_ALLOCATORS: &[&str] = &[
    "script", "expect", "unbuffer", "tmux", "screen", "dtach", "abduco", "zellij",
];

const NESTED_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "aider",
    "opencode",
    "cursor-agent",
    "amp",
    "goose",
    "crush",
];

const SCHEDULERS: &[&str] = &["crontab", "at", "batch", "systemd-run", "launchctl"];

/// Programs that only read their arguments, so a path among them is never a
/// write (a redirect still is).
const READ_ONLY: &[&str] = &[
    "cat",
    "tac",
    "less",
    "more",
    "head",
    "tail",
    "ls",
    "tree",
    "stat",
    "file",
    "wc",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "ack",
    "fd",
    "du",
    "realpath",
    "readlink",
    "basename",
    "dirname",
    "diff",
    "cmp",
    "comm",
    "jq",
    "yq",
    "bat",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "sha512sum",
    "shasum",
    "b2sum",
    "cksum",
    "od",
    "xxd",
    "hexdump",
    "strings",
    "column",
    "uniq",
    "cut",
    "tr",
    "nl",
    "fold",
    "which",
    "type",
    "whereis",
    "git",
    "pwd",
    "cd",
    "pushd",
    "popd",
];

/// Programs that only print their arguments: nothing in them is a path.
const PRINTERS: &[&str] = &[
    "echo", "printf", "true", "false", ":", "test", "[", "[[", "]]",
];

/// The words that make a command worth a closer look when it cannot be
/// classified: fail closed on these (`authority.md` section 9, "Fail mode").
const PROTECTED_WORDS: &[&str] = &[
    "git",
    "gh",
    "gh-axi",
    "curl",
    "wget",
    "xh",
    "aria2c",
    "nc",
    "ncat",
    "netcat",
    "socat",
    "telnet",
    "tiller",
    "treehouse",
    "herdr",
    "claude",
    "codex",
    "npx",
    "bunx",
    "crontab",
    "systemd-run",
    "systemctl",
    "launchctl",
    "pkill",
    "killall",
    "expect",
    "unbuffer",
    "tmux",
    "screen",
    "sqlite3",
    "push",
    "merge",
    "graphql",
    "worktree",
    "approve",
    "grant",
    "land",
];

const PROTECTED_MARKS: &[&str] = &[
    ".tiller",
    ".claude",
    ".git/hooks",
    "hookspath",
    "tiller_",
    "claudecode",
    "ld_preload",
    "path=",
];

/// Whether `text` holds a protected token after quote removal.
pub(super) fn protected(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if PROTECTED_MARKS.iter().any(|mark| lower.contains(mark)) {
        return true;
    }
    lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || "-_./".contains(c)))
        .any(|token| PROTECTED_WORDS.contains(&basename(token)))
}

/// The code a command most likely deserves when it cannot be read.
fn guessed_code(text: &str) -> Code {
    let lower = text.to_ascii_lowercase();
    let has = |word: &str| {
        lower
            .split(|c: char| !(c.is_ascii_alphanumeric() || "-_".contains(c)))
            .any(|token| token == word)
    };
    if has("merge") || has("graphql") {
        Code::Merge
    } else if has("push") {
        Code::DefaultPush
    } else if lower.contains(".tiller") || has("sqlite3") {
        Code::StateWrite
    } else if has("tiller") {
        Code::OwnerOnly
    } else if HTTP_CLIENTS.iter().any(|name| has(name)) {
        Code::HttpClient
    } else if has("worktree") || has("treehouse") || has("herdr") {
        Code::WorktreeAdmin
    } else if has("claude") || has("crontab") || has("codex") {
        Code::Delegation
    } else {
        Code::GuardTamper
    }
}

pub(super) fn classify(command: &str, ctx: &Context<'_>) -> Option<Denial> {
    let mut walker = Walker {
        ctx,
        vars: HashMap::new(),
        background: HashSet::new(),
        depth: 0,
        remote: false,
    };
    walker.text(command, true)
}

struct Walker<'a, 'b> {
    ctx: &'a Context<'b>,
    /// Variables a script set to a static value earlier in the same call.
    vars: HashMap<String, Option<String>>,
    /// Variables holding `$!`, the last job this script put in the
    /// background: the one process id `kill` may take from a variable.
    background: HashSet<String>,
    depth: usize,
    /// The script runs on another host through `ssh`, `mosh` or `et`.
    remote: bool,
}

impl Walker<'_, '_> {
    fn deny(
        &self,
        code: Code,
        why: impl Into<String>,
        command: impl Into<String>,
    ) -> Option<Denial> {
        Some(Denial::new(code, why, command))
    }

    /// Classifies a script given as text; `persists` is whether its top-level
    /// commands run in the session's own shell.
    fn text(&mut self, text: &str, persists: bool) -> Option<Denial> {
        if self.depth > 16 {
            return protected(text).then(|| {
                Denial::new(
                    Code::GuardTamper,
                    "the command nests too deep to read",
                    text,
                )
            });
        }
        match shell::parse(text) {
            Ok(script) => {
                self.depth += 1;
                let found = self.script(&script, persists);
                self.depth -= 1;
                found
            }
            Err(error) => protected(text).then(|| {
                Denial::new(
                    guessed_code(text),
                    format!(
                        "the command could not be read ({error}) and names a protected command"
                    ),
                    text,
                )
            }),
        }
    }

    fn script(&mut self, script: &Script, persists: bool) -> Option<Denial> {
        // Bash and zsh both call a function named by a path, so a function
        // `/nix/store/...-tiller/bin/tiller` shadows the packaged path too.
        if script
            .functions
            .iter()
            .any(|name| basename(name) == "tiller")
        {
            return self.deny(
                Code::GuardTamper,
                "a function named tiller shadows the packaged binary",
                "tiller() { ... }",
            );
        }
        for command in &script.commands {
            if let Some(found) = self.simple(command, script, persists && command.persists) {
                return Some(found);
            }
        }
        None
    }

    fn expand(&self, word: &Word) -> Arg {
        if !word.dynamic {
            return Arg::fixed(&word.text);
        }
        let text = word.text.trim();
        for (open, close) in [("$(", ")"), ("`", "`")] {
            if let Some(inner) = text.strip_prefix(open).and_then(|t| t.strip_suffix(close)) {
                let parts: Vec<&str> = inner.split_whitespace().collect();
                if let ["which", program]
                | ["command", "-v", program]
                | ["type", "-P" | "-p", program] = parts.as_slice()
                {
                    return Arg::fixed(program);
                }
            }
        }
        let chars: Vec<char> = word.text.chars().collect();
        let mut out = String::new();
        let mut fixed = true;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '$' => {
                    let (name, next) = if chars.get(i + 1) == Some(&'{') {
                        let end = chars[i + 2..].iter().position(|c| *c == '}');
                        match end {
                            Some(end) => (
                                chars[i + 2..i + 2 + end].iter().collect::<String>(),
                                i + 3 + end,
                            ),
                            None => (String::new(), chars.len()),
                        }
                    } else {
                        let mut end = i + 1;
                        while chars
                            .get(end)
                            .is_some_and(|c| *c == '_' || c.is_ascii_alphanumeric())
                        {
                            end += 1;
                        }
                        (chars[i + 1..end].iter().collect::<String>(), end)
                    };
                    match self.var(&name) {
                        Some(value) => out.push_str(&value),
                        None => {
                            return Arg {
                                text: word.text.clone(),
                                fixed: false,
                            };
                        }
                    }
                    i = next.max(i + 1);
                    continue;
                }
                '`' => {
                    return Arg {
                        text: word.text.clone(),
                        fixed: false,
                    };
                }
                '*' | '?' | '[' => fixed = false,
                _ => {}
            }
            out.push(chars[i]);
            i += 1;
        }
        Arg { text: out, fixed }
    }

    fn var(&self, name: &str) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        if let Some(value) = self.vars.get(name) {
            return value.clone();
        }
        match name {
            "HOME" => self
                .ctx
                .env
                .user_home
                .as_ref()
                .map(|home| home.to_string_lossy().into_owned()),
            "PWD" => Some(self.ctx.cwd.to_string_lossy().into_owned()),
            _ => None,
        }
    }

    fn simple(&mut self, command: &Simple, script: &Script, persists: bool) -> Option<Denial> {
        let words: Vec<Arg> = command.words.iter().map(|word| self.expand(word)).collect();
        let shown = texts(&words);
        for (name, value) in &command.assigns {
            if let Some(found) = self.assign(name, &self.expand(value), &shown) {
                return Some(found);
            }
        }
        if words.is_empty() {
            for (name, value) in &command.assigns {
                if value.text == "$!" || value.text == "${!}" {
                    self.background.insert(name.clone());
                } else {
                    self.background.remove(name);
                }
                let value = self.expand(value);
                self.vars
                    .insert(name.clone(), value.fixed.then_some(value.text));
            }
        }
        for redirect in &command.redirects {
            if redirect.stdin_text {
                continue;
            }
            let target = self.expand(&redirect.target);
            if let Some((code, why)) = path_denial(&target.text, redirect.writes, self.ctx) {
                return self.deny(
                    code,
                    why,
                    format!("{shown} {} {}", redirect.op, target.text),
                );
            }
        }
        if words.is_empty() {
            return None;
        }
        self.exec(&words, command, script, persists, false)
    }

    /// A leading assignment or an `export`: who the command runs as and
    /// which programs it finds.
    fn assign(&self, name: &str, value: &Arg, shown: &str) -> Option<Denial> {
        let command = format!("{name}={} {shown}", value.text);
        if name.starts_with("TILLER_") || name == "CLAUDECODE" {
            return self.deny(Code::Identity, format!("the command sets {name}"), command);
        }
        if name == "PATH" {
            return self.deny(
                Code::GuardTamper,
                "the command sets PATH, which changes which tiller runs",
                command,
            );
        }
        if name.starts_with("LD_") || name.starts_with("DYLD_") {
            return self.deny(
                Code::GuardTamper,
                format!("{name} loads code into the programs the command runs"),
                command,
            );
        }
        if name == "GIT_CONFIG_PARAMETERS"
            || name == "GIT_CONFIG_COUNT"
            || name.starts_with("GIT_CONFIG_KEY_")
            || name.starts_with("GIT_CONFIG_VALUE_")
        {
            return self.deny(
                Code::GuardTamper,
                "the command passes git config through the environment",
                command,
            );
        }
        if let Some((code, why)) = path_denial(&value.text, false, self.ctx) {
            return self.deny(code, why, command);
        }
        None
    }

    /// One command position after assignments: strips wrappers, then runs
    /// the program's rules. `appended` is true when `xargs` adds arguments
    /// the guard cannot see.
    fn exec(
        &mut self,
        words: &[Arg],
        command: &Simple,
        script: &Script,
        persists: bool,
        appended: bool,
    ) -> Option<Denial> {
        let shown = texts(words);
        let mut i = 0;
        loop {
            let program = words.get(i)?;
            if !program.fixed {
                return self.unknown_program(&words[i..]);
            }
            let rest = &words[i + 1..];
            let skip = |start: usize, with_arg: &[&str]| -> usize {
                let mut j = start;
                while let Some(word) = words.get(j) {
                    let text = word.text.as_str();
                    if text == "--" {
                        return j + 1;
                    }
                    if !text.starts_with('-') || text == "-" {
                        break;
                    }
                    j += if with_arg.contains(&text) { 2 } else { 1 };
                }
                j
            };
            match basename(&program.text) {
                "env" => {
                    let mut j = i + 1;
                    while let Some(word) = words.get(j) {
                        let text = word.text.as_str();
                        let unset = text
                            .strip_prefix("--unset=")
                            .or_else(|| text.strip_prefix("-u").filter(|v| !v.is_empty()));
                        if text == "-i" || text == "--ignore-environment" || text == "-" {
                            return self.deny(
                                Code::Identity,
                                "env -i strips the session's identity",
                                shown,
                            );
                        } else if text == "-u" || text == "--unset" || unset.is_some() {
                            let name = match unset {
                                Some(name) => name.to_owned(),
                                None => {
                                    j += 1;
                                    words.get(j).map(|w| w.text.clone()).unwrap_or_default()
                                }
                            };
                            if name.starts_with("TILLER_") || name == "CLAUDECODE" {
                                return self.deny(
                                    Code::Identity,
                                    format!("env -u {name} strips the session's identity"),
                                    shown,
                                );
                            }
                        } else if text == "-S"
                            || text == "--split-string"
                            || text.starts_with("-S")
                            || text.starts_with("--split-string=")
                        {
                            let inline = text
                                .strip_prefix("--split-string=")
                                .or_else(|| text.strip_prefix("-S").filter(|v| !v.is_empty()));
                            let (split, after) = match inline {
                                Some(split) => (split.to_owned(), j + 1),
                                None => (
                                    words.get(j + 1).map(|w| w.text.clone()).unwrap_or_default(),
                                    j + 2,
                                ),
                            };
                            let mut joined = split;
                            for word in words.iter().skip(after) {
                                joined.push(' ');
                                joined.push_str(&shell_quote(&word.text));
                            }
                            return self.text(&joined, false);
                        } else if text == "-C" || text == "--chdir" {
                            j += 1;
                        } else if text == "--" {
                            j += 1;
                            break;
                        } else if let Some((name, value)) = text.split_once('=')
                            && !text.starts_with('-')
                        {
                            if let Some(found) = self.assign(name, &Arg::fixed(value), &shown) {
                                return Some(found);
                            }
                        } else if !text.starts_with('-') {
                            break;
                        }
                        j += 1;
                    }
                    i = j;
                }
                "command" => {
                    if rest.first().is_some_and(|w| w.is("-v") || w.is("-V")) {
                        return None;
                    }
                    i = skip(i + 1, &[]);
                }
                "builtin" | "nohup" | "busybox" => i += 1,
                "exec" => i = skip(i + 1, &["-a"]),
                "nice" => i = skip(i + 1, &["-n", "--adjustment"]),
                "time" => i = skip(i + 1, &["-f", "-o", "--format", "--output"]),
                "setsid" => i = skip(i + 1, &[]),
                "stdbuf" => i = skip(i + 1, &["-i", "-o", "-e"]),
                "ionice" => i = skip(i + 1, &["-c", "-n", "-p", "-P", "-u"]),
                "taskset" => i = skip(i + 1, &[]) + 1,
                "timeout" => i = skip(i + 1, &["-s", "-k", "--signal", "--kill-after"]) + 1,
                "sudo" | "doas" => {
                    i = skip(
                        i + 1,
                        &[
                            "-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-T", "--user",
                            "--group",
                        ],
                    );
                    while let Some(word) = words.get(i)
                        && let Some((name, value)) = word.text.split_once('=')
                    {
                        if let Some(found) = self.assign(name, &Arg::fixed(value), &shown) {
                            return Some(found);
                        }
                        i += 1;
                    }
                }
                "xargs" => {
                    // `-I R` runs the command once per input line with R
                    // replaced, and appends nothing; any other form appends.
                    let mut replace: Option<String> = None;
                    let mut j = i + 1;
                    while let Some(word) = words.get(j) {
                        let text = word.text.as_str();
                        if text == "--" {
                            j += 1;
                            break;
                        }
                        if !text.starts_with('-') || text == "-" {
                            break;
                        }
                        if text == "-I" {
                            replace = words.get(j + 1).map(|w| w.text.clone());
                            j += 2;
                            continue;
                        }
                        if text == "--replace" || text == "-i" {
                            replace = Some("{}".into());
                        } else if let Some(given) = text
                            .strip_prefix("--replace=")
                            .or_else(|| text.strip_prefix("-I"))
                            .or_else(|| text.strip_prefix("-i").filter(|_| !text.starts_with("--")))
                        {
                            replace = Some(given.to_owned());
                        }
                        j += if XARGS_WITH_ARG.contains(&text) { 2 } else { 1 };
                    }
                    let tail = words.get(j..).filter(|tail| !tail.is_empty())?;
                    // A word that holds the replace string is only known
                    // once xargs reads its input.
                    let tail: Vec<Arg> = tail
                        .iter()
                        .map(|word| match replace.as_deref() {
                            Some(r) if !r.is_empty() && word.text.contains(r) => Arg {
                                text: word.text.clone(),
                                fixed: false,
                            },
                            _ => word.clone(),
                        })
                        .collect();
                    let appended = appended || replace.is_none();
                    return self.exec(&tail, command, script, false, appended);
                }
                "nix"
                    if rest
                        .first()
                        .is_some_and(|w| w.is("develop") || w.is("shell")) =>
                {
                    let at = rest.iter().position(|w| w.is("-c") || w.is("--command"))?;
                    i = i + 1 + at + 1;
                }
                "nix-shell" => {
                    let at = rest
                        .iter()
                        .position(|w| w.is("--run") || w.is("--command"))?;
                    let payload = rest.get(at + 1)?;
                    return self.payload(payload, false);
                }
                "direnv" if rest.first().is_some_and(|w| w.is("exec")) => i += 3,
                "flock" => {
                    let mut j = skip(i + 1, &["-w", "-E", "--timeout", "--conflict-exit-code"]) + 1;
                    if words
                        .get(j)
                        .is_some_and(|w| w.is("-c") || w.is("--command"))
                    {
                        let payload = words.get(j + 1)?;
                        return self.payload(payload, false);
                    }
                    if words.get(j).is_some_and(|w| w.is("--")) {
                        j += 1;
                    }
                    i = j;
                }
                "su" => {
                    let at = rest.iter().position(|w| w.is("-c") || w.is("--command"))?;
                    let payload = rest.get(at + 1)?;
                    return self.payload(payload, false);
                }
                "watch" => {
                    let exec = rest.iter().any(|w| w.is("-x") || w.is("--exec"));
                    let j = skip(i + 1, &["-n", "--interval", "-q", "--equexit"]);
                    if exec {
                        i = j;
                    } else {
                        let joined = texts(&words[j..]);
                        return self.text(&joined, false);
                    }
                }
                _ => break,
            }
        }
        let program = &words[i];
        let name = basename(&program.text).to_owned();
        let args = &words[i..];
        if let Some(found) = self.program(
            &name,
            program,
            &args[1..],
            command,
            script,
            persists,
            appended,
        ) {
            return Some(found);
        }
        self.arg_paths(&name, &args[1..], &shown)
    }

    fn payload(&mut self, payload: &Arg, persists: bool) -> Option<Denial> {
        if payload.fixed {
            self.text(&payload.text, persists)
        } else {
            protected(&payload.text).then(|| {
                Denial::new(
                    guessed_code(&payload.text),
                    "the command string is not known before it runs and names a protected command",
                    payload.text.clone(),
                )
            })
        }
    }

    fn unknown_program(&self, words: &[Arg]) -> Option<Denial> {
        let shown = texts(words);
        if self.remote {
            return self.deny(
                Code::Peer,
                "the remote program is not known before it runs and could be tiller",
                shown,
            );
        }
        protected(&shown).then(|| {
            Denial::new(
                guessed_code(&shown),
                "the program is not known before it runs and the command names a protected one",
                shown.clone(),
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn program(
        &mut self,
        name: &str,
        program: &Arg,
        args: &[Arg],
        command: &Simple,
        script: &Script,
        persists: bool,
        appended: bool,
    ) -> Option<Denial> {
        let mut all = vec![program.clone()];
        all.extend_from_slice(args);
        let shown = texts(&all);
        let ctx = self.ctx;
        if self.remote && name == "tiller" {
            return self.deny(
                Code::Peer,
                "the remote command runs tiller on another host",
                shown,
            );
        }
        if SHELLS.contains(&name) {
            return self.shell(args, command, script, appended);
        }
        if HTTP_CLIENTS.contains(&name) {
            return self.deny(Code::HttpClient, format!("{name} is an HTTP client"), shown);
        }
        if PTY_ALLOCATORS.contains(&name) {
            return self.deny(
                Code::Identity,
                format!("{name} fakes a terminal, which strips the session's identity"),
                shown,
            );
        }
        if NESTED_AGENTS.contains(&name) {
            return self.deny(
                Code::Delegation,
                format!("{name} starts a nested agent"),
                shown,
            );
        }
        if SCHEDULERS.contains(&name) {
            return self.deny(
                Code::Delegation,
                format!("{name} schedules work outside Tiller"),
                shown,
            );
        }
        match name {
            "eval" => {
                if args.iter().all(|arg| arg.fixed) {
                    return self.text(&texts(args), persists);
                }
                return self.unknown_program(args).or_else(|| {
                    protected(&shown).then(|| {
                        Denial::new(
                            guessed_code(&shown),
                            "eval of text not known before it runs",
                            shown.clone(),
                        )
                    })
                });
            }
            "alias" => {
                for arg in args {
                    if let Some((alias, value)) = arg.text.split_once('=') {
                        if basename(alias) == "tiller" {
                            return self.deny(
                                Code::GuardTamper,
                                "an alias named tiller shadows the packaged binary",
                                shown,
                            );
                        }
                        if let Some(found) = self.text(value, false) {
                            return Some(found);
                        }
                    }
                }
            }
            "export" | "declare" | "typeset" | "local" | "readonly" => {
                let unexport = args.iter().any(|arg| arg.is("-n"));
                for arg in args.iter().filter(|arg| !arg.text.starts_with('-')) {
                    let (var, value) = match arg.text.split_once('=') {
                        Some((var, value)) => (var, Some(value)),
                        None => (arg.text.as_str(), None),
                    };
                    if unexport && (var.starts_with("TILLER_") || var == "CLAUDECODE") {
                        return self.deny(
                            Code::Identity,
                            format!("export -n {var} strips the session's identity"),
                            shown,
                        );
                    }
                    if let Some(value) = value {
                        let value = Arg {
                            text: value.to_owned(),
                            fixed: arg.fixed,
                        };
                        if let Some(found) = self.assign(var, &value, &shown) {
                            return Some(found);
                        }
                        self.vars
                            .insert(var.to_owned(), value.fixed.then_some(value.text));
                    }
                }
            }
            "unset" => {
                for arg in args.iter().filter(|arg| !arg.text.starts_with('-')) {
                    if arg.text.starts_with("TILLER_") || arg.text == "CLAUDECODE" {
                        return self.deny(
                            Code::Identity,
                            format!("unset {} strips the session's identity", arg.text),
                            shown,
                        );
                    }
                    if arg.text == "PATH" {
                        return self.deny(
                            Code::GuardTamper,
                            "unset PATH changes which tiller runs",
                            shown,
                        );
                    }
                    self.vars.insert(arg.text.clone(), None);
                }
            }
            "socat" => {
                let pty = args
                    .iter()
                    .any(|arg| arg.text.to_ascii_lowercase().contains("pty"));
                return if pty {
                    self.deny(Code::Identity, "socat with a pty fakes a terminal", shown)
                } else {
                    self.deny(Code::HttpClient, "socat is a network client", shown)
                };
            }
            "openssl" if args.first().is_some_and(|arg| arg.is("s_client")) => {
                return self.deny(
                    Code::HttpClient,
                    "openssl s_client is a network client",
                    shown,
                );
            }
            "npx" | "bunx" | "pnpx" | "npm" | "pnpm" | "yarn" | "bun" => {
                let agent = args.iter().any(|arg| {
                    let lower = arg.text.to_ascii_lowercase();
                    lower.contains("claude-code")
                        || lower.contains("@anthropic-ai/claude")
                        || lower.contains("@openai/codex")
                });
                if agent {
                    return self.deny(Code::Delegation, "the command starts a nested agent", shown);
                }
            }
            "systemctl" => {
                let stops = args.iter().any(|arg| {
                    [
                        "stop",
                        "kill",
                        "disable",
                        "mask",
                        "restart",
                        "try-restart",
                        "reload-or-restart",
                        "isolate",
                    ]
                    .contains(&arg.text.as_str())
                });
                return if stops {
                    self.deny(Code::GuardTamper, "systemctl stops a service", shown)
                } else {
                    self.deny(
                        Code::Delegation,
                        "systemctl starts work outside Tiller",
                        shown,
                    )
                };
            }
            "pkill" | "killall" => {
                return self.deny(
                    Code::GuardTamper,
                    format!("{name} can stop the daemon or another session"),
                    shown,
                );
            }
            "kill" => return self.kill(args, &shown, appended),
            "treehouse" | "herdr" => {
                return self.deny(
                    Code::WorktreeAdmin,
                    format!("{name} administers worktrees and panes"),
                    shown,
                );
            }
            "git" => return self.git(args, &shown, appended),
            "gh" | "gh-axi" => return self.gh(name, args, &shown, appended),
            "tiller" => return self.tiller(program, args, &shown, appended),
            "ssh" | "mosh" | "et" => return self.remote_shell(name, args, command, &shown),
            "cd" | "pushd" if persists && ctx.role == Role::Orchestrator => {
                return self.cd(args, &shown);
            }
            "find" => {
                let mut j = 0;
                while j < args.len() {
                    if ["-exec", "-execdir", "-ok", "-okdir"].contains(&args[j].text.as_str()) {
                        let start = j + 1;
                        let end = args[start..]
                            .iter()
                            .position(|arg| arg.is(";") || arg.is("+"))
                            .map_or(args.len(), |at| start + at);
                        if start < end
                            && let Some(found) =
                                self.exec(&args[start..end], command, script, false, false)
                        {
                            return Some(found);
                        }
                        j = end;
                    }
                    j += 1;
                }
            }
            _ => {}
        }
        None
    }

    fn shell(
        &mut self,
        args: &[Arg],
        command: &Simple,
        script: &Script,
        appended: bool,
    ) -> Option<Denial> {
        let mut j = 0;
        let mut string = false;
        let mut stdin = false;
        while let Some(arg) = args.get(j) {
            let text = arg.text.as_str();
            if text == "--" {
                j += 1;
                break;
            }
            if text.starts_with("--") {
                j += if text == "--rcfile" || text == "--init-file" {
                    2
                } else {
                    1
                };
                continue;
            }
            if (text.starts_with('-') || text.starts_with('+')) && text.len() > 1 {
                let flags = &text[1..];
                if text.starts_with('-') && flags.contains('c') {
                    string = true;
                }
                if flags.contains('s') {
                    stdin = true;
                }
                // `-o <option>` and `-O <shopt>`, also at the end of a
                // cluster such as `-euo pipefail`.
                j += if flags.ends_with(['o', 'O']) { 2 } else { 1 };
                continue;
            }
            break;
        }
        if string {
            match args.get(j) {
                Some(payload) => return self.payload(payload, false),
                // `echo '<script>' | xargs sh -c`: the script is the input.
                None if appended => {}
                None => return None,
            }
        } else if j < args.len() && !stdin {
            // A script file: what it runs is the role's convention, and the
            // forge ruleset on a gated project.
            return None;
        }
        for redirect in command.redirects.iter().filter(|r| r.stdin_text) {
            if let Some(found) = self.text(&redirect.target.text, false) {
                return Some(found);
            }
        }
        if command.piped_in {
            // `echo '<script>' | sh`: every quoted word elsewhere in the call
            // may be the script, and so may the words after each program
            // (`echo gh pr merge 12 | sh`).
            for other in &script.commands {
                let fixed: Vec<&str> = other
                    .words
                    .iter()
                    .filter(|w| !w.dynamic)
                    .map(|w| w.text.as_str())
                    .collect();
                let joined = fixed.get(1..).unwrap_or_default().join(" ");
                let quoted = fixed.iter().copied().filter(|text| text.contains(' '));
                for text in quoted.chain(std::iter::once(joined.as_str())) {
                    if let Ok(inner) = shell::parse(text)
                        && let Some(found) = self.script(&inner, false)
                    {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    fn arg_paths(&self, name: &str, args: &[Arg], shown: &str) -> Option<Denial> {
        if PRINTERS.contains(&name) || name == "tiller" {
            return None;
        }
        let has = |flag: &str| {
            args.iter()
                .any(|arg| arg.text == flag || arg.text.starts_with(&format!("{flag}=")))
        };
        let writes = match name {
            "sed" => {
                has("-i")
                    || args
                        .iter()
                        .any(|arg| arg.text.starts_with("-i") || arg.text == "--in-place")
            }
            "find" => [
                "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls",
            ]
            .iter()
            .any(|flag| has(flag)),
            "sort" => has("-o") || has("--output"),
            _ => !READ_ONLY.contains(&name),
        };
        for arg in args {
            for text in std::iter::once(arg.text.as_str())
                .chain(arg.text.split_once('=').map(|(_, value)| value))
            {
                if text.is_empty() || text.contains(char::is_whitespace) {
                    continue;
                }
                if let Some((code, why)) = path_denial(text, writes, self.ctx) {
                    return self.deny(code, why, shown);
                }
            }
        }
        None
    }

    fn kill(&self, args: &[Arg], shown: &str, appended: bool) -> Option<Denial> {
        if appended {
            return self.deny(
                Code::GuardTamper,
                "kill of process ids the guard cannot see",
                shown,
            );
        }
        let mut j = 0;
        while let Some(arg) = args.get(j) {
            let text = arg.text.as_str();
            if text == "-l" || text == "-L" {
                return None;
            }
            if text == "-s" || text == "-n" {
                j += 2;
                continue;
            }
            if text == "--" {
                j += 1;
                break;
            }
            if text.starts_with('-')
                && text.len() > 1
                && !text[1..].chars().all(|c| c.is_ascii_digit())
            {
                j += 1;
                continue;
            }
            if text.starts_with('-') && text.len() > 1 && j + 1 < args.len() {
                // `kill -9 <pid>`: a signal number.
                j += 1;
                continue;
            }
            break;
        }
        for target in &args[j.min(args.len())..] {
            let text = target.text.as_str();
            let variable = text
                .strip_prefix("${")
                .and_then(|rest| rest.strip_suffix('}'))
                .or_else(|| text.strip_prefix('$'));
            // A job of this script, or a variable set from `$!`. Any other
            // process id, fixed or computed, may be the daemon's.
            let own = (text.starts_with('%') && target.fixed)
                || text == "$!"
                || variable.is_some_and(|name| self.background.contains(name));
            if !own {
                return self.deny(
                    Code::GuardTamper,
                    format!("kill {text} may stop the daemon or another session"),
                    shown,
                );
            }
        }
        None
    }

    fn cd(&self, args: &[Arg], shown: &str) -> Option<Denial> {
        // Options in any cluster (`-LP`, and `pushd -n`, which puts the
        // directory on the stack for a later `pushd +1` or `popd`), up to
        // `--`. Only a directory this check passed can reach the stack.
        let mut j = 0;
        while let Some(arg) = args.get(j) {
            let text = arg.text.as_str();
            if text == "--" {
                j += 1;
                break;
            }
            let options = text
                .strip_prefix('-')
                .filter(|rest| !rest.is_empty() && rest.chars().all(|c| "LPe@n".contains(c)));
            if options.is_none() {
                break;
            }
            j += 1;
        }
        let target = args.get(j)?;
        let rotates = |text: &str| {
            text.strip_prefix(['+', '-'])
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        };
        if target.is("-") || (target.fixed && rotates(&target.text)) {
            return None;
        }
        if !target.fixed {
            return self.deny(
                Code::PersistentCd,
                format!(
                    "cd {} persists into a directory the guard cannot see",
                    target.text
                ),
                shown,
            );
        }
        let path = resolve(&target.text, self.ctx);
        let inside = self
            .ctx
            .env
            .protected_roots
            .iter()
            .any(|root| path.starts_with(root));
        inside.then(|| {
            Denial::new(
                Code::PersistentCd,
                format!(
                    "cd {} would leave the orchestrator shell in a checkout",
                    target.text
                ),
                shown,
            )
        })
    }

    fn git(&self, args: &[Arg], shown: &str, appended: bool) -> Option<Denial> {
        let mut dir = self.ctx.cwd.clone();
        let mut i = 0;
        while let Some(arg) = args.get(i) {
            let text = arg.text.as_str();
            match text {
                "-C" => {
                    if let Some(next) = args.get(i + 1) {
                        dir = resolve_from(&dir, &next.text, self.ctx);
                    }
                    i += 2;
                }
                "-c" => {
                    if let Some(found) = args
                        .get(i + 1)
                        .and_then(|kv| self.git_config_key(&kv.text, shown))
                    {
                        return Some(found);
                    }
                    i += 2;
                }
                "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix" | "--list-cmds"
                | "--attr-source" => i += 2,
                "--config-env" => {
                    if let Some(found) = args
                        .get(i + 1)
                        .and_then(|kv| self.git_config_key(&kv.text, shown))
                    {
                        return Some(found);
                    }
                    i += 2;
                }
                _ if text.starts_with("--config-env=") => {
                    if let Some(found) = self.git_config_key(&text["--config-env=".len()..], shown)
                    {
                        return Some(found);
                    }
                    i += 1;
                }
                _ if text.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = args.get(i) else {
            return if appended {
                self.deny(
                    Code::GuardTamper,
                    "xargs picks the git subcommand from its input",
                    shown,
                )
            } else {
                None
            };
        };
        let rest = &args[i + 1..];
        if !sub.fixed {
            return self.deny(
                Code::GuardTamper,
                "the git subcommand is not known before it runs",
                shown,
            );
        }
        match sub.text.as_str() {
            "push" => self.push(rest, &dir, shown, appended),
            "worktree" => {
                let action = rest.first().map(|arg| arg.text.as_str()).unwrap_or("");
                ["add", "remove", "move", "prune", "lock", "unlock", "repair"]
                    .contains(&action)
                    .then(|| {
                        Denial::new(
                            Code::WorktreeAdmin,
                            format!("git worktree {action} changes worktrees"),
                            shown,
                        )
                    })
            }
            "config" => self.git_config(rest, shown),
            "send-pack" | "http-push" => self.deny(
                Code::DefaultPush,
                format!("git {} pushes refs directly", sub.text),
                shown,
            ),
            "subtree" if rest.iter().any(|arg| arg.is("push")) => self.deny(
                Code::DefaultPush,
                "git subtree push pushes refs directly",
                shown,
            ),
            "svn" if rest.iter().any(|arg| arg.is("dcommit")) => self.deny(
                Code::DefaultPush,
                "git svn dcommit pushes to the remote",
                shown,
            ),
            "p4" if rest.iter().any(|arg| arg.is("submit")) => self.deny(
                Code::DefaultPush,
                "git p4 submit pushes to the remote",
                shown,
            ),
            _ => None,
        }
    }

    /// A config key set for one git call (`-c`, `--config-env`).
    fn git_config_key(&self, pair: &str, shown: &str) -> Option<Denial> {
        let key = pair.split('=').next().unwrap_or(pair).to_ascii_lowercase();
        if key == "core.hookspath"
            || key.starts_with("alias.")
            || key.starts_with("include.")
            || key.starts_with("includeif.")
        {
            return self.deny(
                Code::GuardTamper,
                format!("git config {key} runs or skips the guards' hooks"),
                shown,
            );
        }
        if key.starts_with("push.")
            || key.starts_with("remote.")
            || key.starts_with("url.")
            || key.starts_with("branch.")
        {
            return self.deny(
                Code::DefaultPush,
                format!("git config {key} reroutes a push"),
                shown,
            );
        }
        None
    }

    fn git_config(&self, args: &[Arg], shown: &str) -> Option<Denial> {
        const READS: &[&str] = &[
            "get",
            "list",
            "get-regexp",
            "get-urlmatch",
            "get-color",
            "get-colorbool",
        ];
        const READ_FLAGS: &[&str] = &[
            "--get",
            "--get-all",
            "--get-regexp",
            "--get-urlmatch",
            "-l",
            "--list",
            "--get-color",
            "--get-colorbool",
        ];
        if args
            .iter()
            .any(|arg| arg.is("-e") || arg.is("--edit") || arg.is("edit"))
        {
            return self.deny(Code::GuardTamper, "git config --edit can set hooks", shown);
        }
        if args
            .iter()
            .any(|arg| READ_FLAGS.contains(&arg.text.as_str()))
        {
            return None;
        }
        let mut positionals = Vec::new();
        let mut j = 0;
        while let Some(arg) = args.get(j) {
            let text = arg.text.as_str();
            if [
                "-f",
                "--file",
                "--blob",
                "--type",
                "--default",
                "--comment",
                "--value",
            ]
            .contains(&text)
            {
                j += 2;
                continue;
            }
            if !text.starts_with('-') {
                positionals.push(arg);
            }
            j += 1;
        }
        let (key, writes) = match positionals.first().map(|arg| arg.text.as_str()) {
            Some(first) if READS.contains(&first) => return None,
            Some("set" | "unset" | "rename-section" | "remove-section") => {
                (positionals.get(1), true)
            }
            Some(_) => (positionals.first(), positionals.len() >= 2),
            None => return None,
        };
        let key = key?;
        if !key.fixed {
            return writes.then(|| {
                Denial::new(
                    Code::GuardTamper,
                    "git config of a key not known before it runs",
                    shown,
                )
            });
        }
        if writes {
            self.git_config_key(&key.text, shown)
        } else {
            None
        }
    }

    fn push(&self, args: &[Arg], dir: &Path, shown: &str, appended: bool) -> Option<Denial> {
        if self.ctx.role != Role::Builder {
            return self.deny(
                Code::ForeignBranch,
                format!("a {} does not push", self.ctx.role.name()),
                shown,
            );
        }
        if appended {
            return self.deny(
                Code::DefaultPush,
                "the push target comes from xargs, so the guard cannot see it",
                shown,
            );
        }
        let mut positionals = Vec::new();
        let mut j = 0;
        let mut rest_positional = false;
        while let Some(arg) = args.get(j) {
            let text = arg.text.as_str();
            j += 1;
            if rest_positional || !text.starts_with('-') || text == "-" {
                positionals.push(arg.clone());
                continue;
            }
            match text {
                "--" => rest_positional = true,
                "--force"
                | "--force-if-includes"
                | "--mirror"
                | "--all"
                | "--branches"
                | "--prune"
                | "--delete" => {
                    let why = match text {
                        "--force" | "--force-if-includes" => "a force push",
                        "--delete" => "a push that deletes a branch",
                        _ => "a push of more than one branch",
                    };
                    return self.deny(
                        Code::DefaultPush,
                        format!("git push {text} is {why}"),
                        shown,
                    );
                }
                "--tags" | "--follow-tags" => {
                    return self.deny(
                        Code::ForeignBranch,
                        format!("git push {text} pushes tags"),
                        shown,
                    );
                }
                "--repo" | "-o" | "--push-option" => j += 1,
                "--receive-pack" | "--exec" => {
                    return self.deny(
                        Code::GuardTamper,
                        format!("git push {text} runs a program"),
                        shown,
                    );
                }
                _ if text.starts_with("--force-with-lease") || text.starts_with("--force") => {
                    return self.deny(
                        Code::DefaultPush,
                        format!("git push {text} is a force push"),
                        shown,
                    );
                }
                _ if text.starts_with("--receive-pack=") || text.starts_with("--exec=") => {
                    return self.deny(
                        Code::GuardTamper,
                        "git push --receive-pack runs a program",
                        shown,
                    );
                }
                _ if text.starts_with("--") => {}
                _ => {
                    // A cluster of short flags.
                    let flags = &text[1..];
                    if flags.contains('f') {
                        return self.deny(
                            Code::DefaultPush,
                            format!("git push {text} is a force push"),
                            shown,
                        );
                    }
                    if flags.contains('d') {
                        return self.deny(Code::DefaultPush, "git push -d deletes a branch", shown);
                    }
                    if flags.ends_with('o') {
                        j += 1;
                    }
                }
            }
        }
        let current = || current_branch(dir);
        let refspecs = positionals.get(1..).unwrap_or(&[]);
        if refspecs.is_empty() {
            return match current() {
                Some(branch) => self.push_destination(&branch, shown),
                None => self.deny(
                    Code::ForeignBranch,
                    "git push with no refspec from a detached or unknown HEAD",
                    shown,
                ),
            };
        }
        for refspec in refspecs {
            if !refspec.fixed {
                return self.deny(
                    Code::ForeignBranch,
                    format!(
                        "the push target {} is not known before it runs",
                        refspec.text
                    ),
                    shown,
                );
            }
            let text = refspec.text.as_str();
            if text.starts_with('+') {
                return self.deny(Code::DefaultPush, format!("{text} is a force push"), shown);
            }
            if text.starts_with(':') {
                return self.deny(
                    Code::DefaultPush,
                    format!("{text} deletes a remote branch"),
                    shown,
                );
            }
            if text.contains('*') {
                return self.deny(
                    Code::DefaultPush,
                    format!("{text} pushes many branches"),
                    shown,
                );
            }
            let (source, destination) = match text.split_once(':') {
                Some((source, destination)) => (source, destination),
                None => (text, text),
            };
            let destination = if matches!(destination, "HEAD" | "@")
                || (destination == source && is_sha(source))
            {
                if is_sha(source) {
                    return self.deny(
                        Code::ForeignBranch,
                        format!("{text} names no branch to push to"),
                        shown,
                    );
                }
                match current() {
                    Some(branch) => branch,
                    None => {
                        return self.deny(
                            Code::ForeignBranch,
                            "git push HEAD from a detached or unknown HEAD",
                            shown,
                        );
                    }
                }
            } else {
                destination.to_owned()
            };
            if let Some(found) = self.push_destination(&destination, shown) {
                return Some(found);
            }
        }
        None
    }

    fn push_destination(&self, destination: &str, shown: &str) -> Option<Denial> {
        let branch = destination
            .strip_prefix("refs/heads/")
            .unwrap_or(destination);
        if destination.starts_with("refs/") && !destination.starts_with("refs/heads/") {
            return self.deny(
                Code::ForeignBranch,
                format!("{destination} is not a branch"),
                shown,
            );
        }
        if self.is_default_branch(branch) {
            return self.deny(
                Code::DefaultPush,
                format!("the push goes to the default branch {branch}"),
                shown,
            );
        }
        match &self.ctx.facts().branch {
            Some(own) if own == branch => None,
            Some(own) => self.deny(
                Code::ForeignBranch,
                format!("the push goes to {branch}, not this run's branch {own}"),
                shown,
            ),
            None => self.deny(
                Code::ForeignBranch,
                "this run has no branch on record",
                shown,
            ),
        }
    }

    /// Whether `branch` is the project's default branch, or `main` or
    /// `master` when the run has none on record.
    fn is_default_branch(&self, branch: &str) -> bool {
        let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        match &self.ctx.facts().default_branch {
            Some(default) => branch == default,
            None => branch == "main" || branch == "master",
        }
    }

    fn gh(&self, name: &str, args: &[Arg], shown: &str, appended: bool) -> Option<Denial> {
        const KNOWN: &[&str] = &[
            "auth",
            "browse",
            "codespace",
            "gist",
            "issue",
            "org",
            "pr",
            "project",
            "release",
            "repo",
            "cache",
            "run",
            "search",
            "secret",
            "ssh-key",
            "status",
            "variable",
            "workflow",
            "label",
            "ruleset",
            "attestation",
            "config",
            "extension",
            "ext",
            "extensions",
            "gpg-key",
            "alias",
            "api",
            "completion",
            "help",
            "copilot",
            "preview",
            "accessibility",
            "stack",
            "setup",
            "update",
            "agent-task",
        ];
        let mut positionals: Vec<&Arg> = Vec::new();
        let mut j = 0;
        while let Some(arg) = args.get(j) {
            let text = arg.text.as_str();
            if ["-R", "--repo", "--hostname"].contains(&text) {
                j += 2;
                continue;
            }
            if !text.starts_with('-') {
                positionals.push(arg);
                if positionals.len() == 2 {
                    break;
                }
            }
            j += 1;
        }
        let Some(top) = positionals.first() else {
            return if appended {
                self.deny(
                    Code::Merge,
                    format!("xargs picks the {name} command from its input"),
                    shown,
                )
            } else {
                None
            };
        };
        if !top.fixed || positionals.get(1).is_some_and(|sub| !sub.fixed) {
            return self.deny(
                Code::Merge,
                format!("the {name} command is not known before it runs"),
                shown,
            );
        }
        let sub = positionals
            .get(1)
            .map(|arg| arg.text.as_str())
            .unwrap_or("");
        // Arguments xargs appends can name the subcommand, or add the
        // method, fields and flags the checks below read.
        if appended
            && (positionals.len() < 2
                || top.text == "api"
                || (top.text == "pr" && ["close", "review", "edit"].contains(&sub))
                || (top.text == "repo" && sub == "edit"))
        {
            return self.deny(
                Code::Merge,
                format!(
                    "xargs adds arguments that decide what {name} {} does",
                    top.text
                ),
                shown,
            );
        }
        let has = |flag: &str| {
            args.iter()
                .any(|arg| arg.text == flag || arg.text.starts_with(&format!("{flag}=")))
        };
        match top.text.as_str() {
            "api" => {
                let start = args
                    .iter()
                    .position(|arg| arg.is("api"))
                    .map_or(0, |at| at + 1);
                return self.gh_api(name, &args[start..], shown);
            }
            "auth" if sub != "status" => {
                return self.deny(
                    Code::Merge,
                    format!("{name} auth hands out or changes the forge token"),
                    shown,
                );
            }
            "alias" if sub == "set" || sub == "import" => {
                return self.deny(
                    Code::GuardTamper,
                    format!("{name} alias defines a command the guard cannot see"),
                    shown,
                );
            }
            "extension" | "ext" | "extensions"
                if ["install", "upgrade", "exec", "create"].contains(&sub) =>
            {
                return self.deny(
                    Code::GuardTamper,
                    format!("{name} extension runs a program the guard cannot see"),
                    shown,
                );
            }
            "setup" | "update" if name == "gh-axi" => {
                return self.deny(
                    Code::GuardTamper,
                    "gh-axi setup and update change the installed tools and hooks",
                    shown,
                );
            }
            "pr" if sub == "close" && (has("-d") || has("--delete-branch")) => {
                return self.deny(
                    Code::Merge,
                    "gh pr close --delete-branch deletes a branch",
                    shown,
                );
            }
            "repo" if ["delete", "rename", "archive", "unarchive"].contains(&sub) => {
                return self.deny(
                    Code::Merge,
                    format!("{name} repo {sub} changes the repository"),
                    shown,
                );
            }
            "repo" if sub == "edit" && has("--default-branch") => {
                return self.deny(
                    Code::DefaultPush,
                    "gh repo edit --default-branch moves the default branch",
                    shown,
                );
            }
            "pr" if sub == "review" && review_approves(args) => {
                return self.deny(
                    Code::Merge,
                    format!("{name} pr review --approve approves a PR"),
                    shown,
                );
            }
            "pr" if sub == "edit" => {
                if let Some(base) = pr_base(args)
                    && (!base.fixed || self.is_default_branch(&base.text))
                {
                    return self.deny(
                        Code::Merge,
                        format!("{name} pr edit --base retargets a PR at the default branch"),
                        shown,
                    );
                }
            }
            "release" if sub == "delete" || sub == "delete-asset" => {
                return self.deny(
                    Code::Merge,
                    format!("{name} release {sub} deletes a release"),
                    shown,
                );
            }
            "stack" if ["push", "submit", "sync", "rebase", "link", "unstack"].contains(&sub) => {
                return self.deny(
                    Code::ForeignBranch,
                    format!("{name} stack {sub} pushes more than this run's branch"),
                    shown,
                );
            }
            _ => {}
        }
        if sub == "merge" || sub == "land" || top.text == "merge" {
            return self.deny(
                Code::Merge,
                format!("{name} {} {sub} merges", top.text),
                shown,
            );
        }
        if name == "gh" && !KNOWN.contains(&top.text.as_str()) {
            return self.deny(
                Code::Merge,
                format!(
                    "gh {} is an alias or an extension the guard cannot read",
                    top.text
                ),
                shown,
            );
        }
        None
    }

    fn gh_api(&self, name: &str, args: &[Arg], shown: &str) -> Option<Denial> {
        let mut endpoint = None;
        let mut method: Option<&Arg> = None;
        let mut fields: Vec<&Arg> = Vec::new();
        let mut input = false;
        let mut j = 0;
        while let Some(arg) = args.get(j) {
            let text = arg.text.as_str();
            let value = args.get(j + 1);
            match text {
                "-X" | "--method" => {
                    method = value;
                    j += 2;
                    continue;
                }
                "-f" | "-F" | "--field" | "--raw-field" => {
                    if let Some(value) = value {
                        fields.push(value);
                    }
                    j += 2;
                    continue;
                }
                "--input" => {
                    input = true;
                    j += 2;
                    continue;
                }
                "-H" | "--header" | "-q" | "--jq" | "-t" | "--template" | "-p" | "--preview"
                | "--cache" | "--hostname" => {
                    j += 2;
                    continue;
                }
                _ => {}
            }
            if let Some(rest) = text
                .strip_prefix("-X")
                .filter(|r| !r.is_empty())
                .or_else(|| text.strip_prefix("--method="))
            {
                return self.api_method(name, rest, arg.fixed, shown);
            }
            if text.starts_with("--field=")
                || text.starts_with("--raw-field=")
                || (text.len() > 2 && (text.starts_with("-f") || text.starts_with("-F")))
            {
                fields.push(arg);
            } else if text.starts_with("--input=") {
                input = true;
            } else if !text.starts_with('-') && endpoint.is_none() {
                endpoint = Some(arg);
            }
            j += 1;
        }
        if let Some(method) = method
            && let Some(found) = self.api_method(name, &method.text, method.fixed, shown)
        {
            return Some(found);
        }
        let graphql = endpoint.is_some_and(|e| e.text == "graphql");
        if graphql {
            if input {
                return self.deny(
                    Code::Merge,
                    format!("{name} api graphql with an input file the guard cannot read"),
                    shown,
                );
            }
            for field in &fields {
                let text = field.text.to_ascii_lowercase();
                if !field.fixed || text.contains("mutation") || text.contains("=@") {
                    return self.deny(
                        Code::Merge,
                        format!("{name} api graphql runs a mutation"),
                        shown,
                    );
                }
            }
            return None;
        }
        if method.is_none() && (input || !fields.is_empty()) {
            return self.deny(
                Code::Merge,
                format!("{name} api with fields is a POST, a forge write"),
                shown,
            );
        }
        if endpoint.is_some_and(|e| !e.fixed) {
            return self.deny(
                Code::Merge,
                format!("{name} api to an endpoint not known before it runs"),
                shown,
            );
        }
        None
    }

    fn api_method(&self, name: &str, method: &str, fixed: bool, shown: &str) -> Option<Denial> {
        let upper = method.to_ascii_uppercase();
        (!fixed || (upper != "GET" && upper != "HEAD")).then(|| {
            Denial::new(
                Code::Merge,
                format!("{name} api {upper} is a forge write"),
                shown,
            )
        })
    }

    /// `ssh`, `mosh` or `et` that runs `tiller` on another host: a peer's
    /// `tiller` is reached only by the board host's daemon (G43). The remote
    /// command, each option that holds a command, and a script fed on
    /// standard input are read as scripts on that host, and a word in them
    /// not known before it runs could name `tiller` too.
    fn remote_shell(
        &mut self,
        name: &str,
        args: &[Arg],
        command: &Simple,
        shown: &str,
    ) -> Option<Denial> {
        let parsed = match remote_args(name, args) {
            Ok(parsed) => parsed,
            Err(why) => return self.deny(Code::Peer, why, shown),
        };
        for script in &parsed.scripts {
            if !script.fixed {
                return self.deny(
                    Code::Peer,
                    format!("a {name} option that runs a command is not known before it runs"),
                    shown,
                );
            }
            if let Some(found) = self.remote(&script.text) {
                return Some(found);
            }
        }
        if parsed.command.iter().any(|word| !word.fixed) {
            return self.deny(
                Code::Peer,
                format!("the {name} remote command is not known before it runs"),
                shown,
            );
        }
        if !parsed.command.is_empty()
            && let Some(found) = self.remote(&texts(&parsed.command))
        {
            return Some(found);
        }
        if !parsed.destination || !reads_script(&parsed.command) {
            return None;
        }
        for redirect in &command.redirects {
            if redirect.stdin_text {
                if let Some(found) = self.remote(&redirect.target.text) {
                    return Some(found);
                }
            } else if reads_stdin(redirect) {
                return self.deny(
                    Code::Peer,
                    format!("{name} reads a remote script from a file"),
                    shown,
                );
            }
        }
        command.piped_in.then(|| {
            Denial::new(
                Code::Peer,
                format!("{name} reads a remote script from standard input"),
                shown,
            )
        })
    }

    /// Reads `text` as a script that runs on another host, where this
    /// script's variables are not set and any `tiller` is a peer's.
    fn remote(&mut self, text: &str) -> Option<Denial> {
        if shell::parse(text).is_err() {
            return self.deny(Code::Peer, "the remote command could not be read", text);
        }
        let vars = std::mem::take(&mut self.vars);
        let background = std::mem::take(&mut self.background);
        let remote = std::mem::replace(&mut self.remote, true);
        let found = self.text(text, false);
        self.vars = vars;
        self.background = background;
        self.remote = remote;
        found
    }

    fn tiller(&self, program: &Arg, args: &[Arg], shown: &str, appended: bool) -> Option<Denial> {
        let ctx = self.ctx;
        if program.text.contains('/') {
            let path = resolve(&program.text, ctx);
            if !is_packaged(&path, ctx.env.packaged_tiller.as_deref()) {
                return self.deny(
                    Code::GuardTamper,
                    format!("{} is not the packaged tiller", program.text),
                    shown,
                );
            }
        }
        for arg in args {
            for text in
                std::iter::once(arg.text.as_str()).chain(arg.text.split_once('=').map(|(_, v)| v))
            {
                if is_secret_path(text) {
                    return self.deny(
                        Code::StateWrite,
                        format!("tiller runs outside the sandbox and would read the secret {text}"),
                        shown,
                    );
                }
                if let Some((code, why)) = path_denial(text, false, ctx) {
                    return self.deny(code, why, shown);
                }
            }
        }
        let mut positionals: Vec<&Arg> = Vec::new();
        let mut j = 0;
        while let Some(arg) = args.get(j) {
            if arg.is("--db") {
                j += 2;
                continue;
            }
            if !arg.text.starts_with('-') {
                positionals.push(arg);
            }
            j += 1;
        }
        let Some(sub) = positionals.first() else {
            return if appended {
                self.deny(
                    Code::OwnerOnly,
                    "xargs picks the tiller command from its input",
                    shown,
                )
            } else {
                None
            };
        };
        if !sub.fixed {
            return self.deny(
                Code::OwnerOnly,
                "the tiller command is not known before it runs",
                shown,
            );
        }
        // `xargs tiller migrate` takes `down` from its input.
        let unseen = appended && positionals.len() < 2;
        let second = positionals
            .get(1)
            .map(|arg| arg.text.as_str())
            .unwrap_or("");
        if sub.text == "peer" {
            return self.deny(Code::Peer, "tiller peer runs a peer link", shown);
        }
        let owner_only = match sub.text.as_str() {
            "approve" => {
                if ctx.role == Role::Orchestrator && !appended && chat_approve(args) {
                    return None;
                }
                true
            }
            "grant" | "away" | "land" | "board" | "import" | "hook" | "guard" | "owner"
            | "init" | "backup" | "export" | "restore" | "daemon" | "host" => true,
            "migrate" => unseen || second == "down",
            "config" => unseen || second == "set",
            "project" => second != "list",
            "mem" => unseen || second == "approve",
            _ => false,
        };
        owner_only.then(|| {
            Denial::new(
                Code::OwnerOnly,
                format!("tiller {} is the owner's", sub.text),
                shown,
            )
        })
    }
}

/// `tiller approve <id> --ask <ask>`, and nothing else but `--json`: the one
/// approve form the orchestrator may run (CP2 decision on G06, `authority.md`
/// section 8).
fn chat_approve(args: &[Arg]) -> bool {
    let mut ids = 0;
    let mut asks = 0;
    let mut verb = false;
    let mut j = 0;
    while let Some(arg) = args.get(j) {
        if !arg.fixed {
            return false;
        }
        match arg.text.as_str() {
            "approve" if !verb => verb = true,
            "--json" => {}
            "--ask" => match args.get(j + 1) {
                Some(value) if value.fixed && !value.text.starts_with('-') => {
                    asks += 1;
                    j += 1;
                }
                _ => return false,
            },
            text if text.starts_with("--ask=") && text.len() > "--ask=".len() => asks += 1,
            text if text.starts_with('-') => return false,
            _ => ids += 1,
        }
        j += 1;
    }
    ids == 1 && asks == 1
}

/// `ssh`, `mosh` or `et` arguments, read the way the client reads them.
#[derive(Default)]
struct Remote {
    /// A host is named, so the client connects to it.
    destination: bool,
    /// Option values that hold a command (`-o ProxyCommand=...`,
    /// `mosh --server`, `et -c`).
    scripts: Vec<Arg>,
    /// The words after the destination: the remote command.
    command: Vec<Arg>,
}

/// `ssh` flags that take no value.
const SSH_FLAGS: &str = "46AaCfGgKkMNnqsTtVvXxYy";
/// `ssh` options that take a value, attached or as the next word.
const SSH_VALUES: &str = "BbcDEeFIiJLlmOoPpQRSWw";
/// `ssh -o` keys whose value is a command.
const SSH_COMMAND_KEYS: &[&str] = &[
    "proxycommand",
    "localcommand",
    "remotecommand",
    "knownhostscommand",
];

/// The options a client takes, by name.
struct Options {
    flags: &'static [&'static str],
    values: &'static [&'static str],
    /// The options among `values` whose value is a command.
    commands: &'static [&'static str],
}

const MOSH_OPTIONS: Options = Options {
    flags: &[
        "-a",
        "-n",
        "-4",
        "-6",
        "-o",
        "--predict-overwrite",
        "--no-init",
        "--local",
        "--ssh-pty",
        "--no-ssh-pty",
        "--help",
        "--version",
    ],
    values: &[
        "-p",
        "--port",
        "--ssh",
        "--server",
        "--client",
        "--predict",
        "--family",
        "--bind-server",
        "--experimental-remote-ip",
    ],
    commands: &["--ssh", "--server", "--client"],
};

const ET_OPTIONS: Options = Options {
    flags: &[
        "-x",
        "-N",
        "-f",
        "-h",
        "--kill-other-sessions",
        "--no-terminal",
        "--forward-ssh-agent",
        "--silent",
        "--noexit",
        "--help",
        "--version",
    ],
    values: &[
        "-c",
        "--command",
        "-p",
        "--port",
        "-u",
        "-t",
        "--tunnel",
        "-r",
        "--reversetunnel",
        "--jumphost",
        "--jport",
        "-v",
        "--verbose",
        "--serverfifo",
        "--logdir",
        "--ssh-option",
        "--terminal-path",
    ],
    commands: &["-c", "--command", "--terminal-path", "--ssh-option"],
};

/// Splits a client's arguments into options, the destination and the remote
/// command. Options may follow the destination; the first word after it that
/// is not an option, or `--`, starts the command. An option the guard does
/// not know is refused, since its value could hold a command.
fn remote_args(name: &str, args: &[Arg]) -> Result<Remote, String> {
    let mut out = Remote::default();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        i += 1;
        if arg.is("--") {
            if !out.destination && i < args.len() {
                out.destination = true;
                i += 1;
            }
            out.command = args.get(i..).unwrap_or_default().to_vec();
            return Ok(out);
        }
        if !arg.fixed || !arg.text.starts_with('-') || arg.text == "-" {
            if out.destination {
                out.command = args[i - 1..].to_vec();
                return Ok(out);
            }
            out.destination = true;
            continue;
        }
        let (used, script) = match name {
            "ssh" => ssh_option(arg, args.get(i))?,
            "mosh" => listed_option(name, &MOSH_OPTIONS, arg, args.get(i))?,
            _ => listed_option(name, &ET_OPTIONS, arg, args.get(i))?,
        };
        i += used;
        out.scripts.extend(script);
    }
    Ok(out)
}

/// One `ssh` option cluster such as `-tt`, `-p2222` or `-o Key=value`: how
/// many of the following words it takes, and the command it holds, if any.
fn ssh_option(arg: &Arg, next: Option<&Arg>) -> Result<(usize, Option<Arg>), String> {
    for (at, letter) in arg.text.char_indices().skip(1) {
        if SSH_FLAGS.contains(letter) {
            continue;
        }
        if !SSH_VALUES.contains(letter) {
            return Err(format!("ssh option -{letter} is not one the guard knows"));
        }
        let rest = &arg.text[at + letter.len_utf8()..];
        let (value, used) = if rest.is_empty() {
            match next {
                Some(next) => (next.clone(), 1),
                None => return Ok((0, None)),
            }
        } else {
            (
                Arg {
                    text: rest.to_owned(),
                    fixed: arg.fixed,
                },
                0,
            )
        };
        if letter != 'o' {
            return Ok((used, None));
        }
        if !value.fixed {
            return Ok((used, Some(value)));
        }
        let option = value.text.trim_start();
        let split = option
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(option.len());
        let (key, command) = option.split_at(split);
        let command = command.trim_start_matches(|c: char| c == '=' || c.is_whitespace());
        let runs = SSH_COMMAND_KEYS.contains(&key.to_ascii_lowercase().as_str());
        return Ok((used, runs.then(|| Arg::fixed(command))));
    }
    Ok((0, None))
}

/// One `mosh` or `et` option, by the client's table: how many of the
/// following words it takes, and the command it holds, if any.
fn listed_option(
    name: &str,
    options: &Options,
    arg: &Arg,
    next: Option<&Arg>,
) -> Result<(usize, Option<Arg>), String> {
    let text = arg.text.as_str();
    let (key, attached) = match text.split_once('=') {
        Some((key, value)) if key.starts_with("--") => (key, Some(value)),
        _ if !text.starts_with("--") => match text.split_at_checked(2) {
            Some((key, value)) if !value.is_empty() => (key, Some(value)),
            _ => (text, None),
        },
        _ => (text, None),
    };
    if attached.is_none() && options.flags.contains(&key) {
        return Ok((0, None));
    }
    if !options.values.contains(&key) {
        return Err(format!("{name} option {key} is not one the guard knows"));
    }
    let (value, used) = match attached {
        Some(value) => (
            Arg {
                text: value.to_owned(),
                fixed: arg.fixed,
            },
            0,
        ),
        None => match next {
            Some(next) => (next.clone(), 1),
            None => return Ok((0, None)),
        },
    };
    Ok((used, options.commands.contains(&key).then_some(value)))
}

/// Programs that read a script on standard input when given none.
const SCRIPT_READERS: &[&str] = &[
    "python", "python3", "perl", "ruby", "node", "bun", "deno", "php", "lua", "xargs", "source",
    ".",
];

/// A remote command that reads a script on standard input. With no command,
/// the remote login shell reads one.
fn reads_script(words: &[Arg]) -> bool {
    words.is_empty()
        || words.iter().any(|word| {
            word.text
                .split(|c: char| !(c.is_ascii_alphanumeric() || "-_./".contains(c)))
                .any(|token| {
                    let name = basename(token);
                    SHELLS.contains(&name) || SCRIPT_READERS.contains(&name)
                })
        })
}

/// A redirection that feeds a file on standard input (`<`, `0<`, `<&`).
fn reads_stdin(redirect: &Redirect) -> bool {
    let fd_len = redirect
        .op
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(redirect.op.len());
    let (fd, op) = redirect.op.split_at(fd_len);
    matches!(fd, "" | "0") && op.starts_with('<') && redirect.target.text != "/dev/null"
}

fn is_sha(text: &str) -> bool {
    (7..=40).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn resolve_from(dir: &Path, text: &str, ctx: &Context<'_>) -> PathBuf {
    if text.starts_with('/') || text.starts_with('~') || text.starts_with("$HOME") {
        resolve(text, ctx)
    } else {
        let joined = dir.join(text);
        resolve(&joined.to_string_lossy(), ctx)
    }
}

/// The branch `HEAD` names in the repository at or above `dir`, read from
/// the git directory without running git.
pub(super) fn current_branch(dir: &Path) -> Option<String> {
    let mut at = Some(dir);
    while let Some(here) = at {
        let dot_git = here.join(".git");
        if dot_git.is_dir() {
            return head_branch(&dot_git);
        }
        if dot_git.is_file() {
            let text = fs::read_to_string(&dot_git).ok()?;
            let gitdir = text.trim().strip_prefix("gitdir:")?.trim();
            let gitdir = if Path::new(gitdir).is_absolute() {
                PathBuf::from(gitdir)
            } else {
                here.join(gitdir)
            };
            return head_branch(&gitdir);
        }
        at = here.parent();
    }
    None
}

fn head_branch(gitdir: &Path) -> Option<String> {
    let head = fs::read_to_string(gitdir.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_owned)
}
