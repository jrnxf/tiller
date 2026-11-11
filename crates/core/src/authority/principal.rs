//! The CLI owner principal (`authority.md` section 3, G22): no
//! `CLAUDECODE`, stdin a terminal, and no Claude Code process among the
//! caller's ancestors (the parent chain read from the OS). A CLI call with no
//! session id that fails any of these is a script, never the owner.
//!
//! A caller that passes keeps what the check saw as a [`Terminal`]. Every
//! event of its mutations carries it, so the owner can spot a decision they
//! did not make (G23, Detected).

use std::io::IsTerminal;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

/// One process in the caller's parent chain.
#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    /// The executable's name: the kernel's `comm` on Linux, the file name of
    /// `ps`'s path elsewhere.
    pub comm: String,
    pub args: String,
}

/// What the owner check saw of a caller that passed it: the terminal device
/// on stdin and the parent chain it walked, nearest first, so `chain[0]` is
/// the parent process. Every event of the caller's mutations carries it as
/// `payload.via_terminal` (`authority.md` G23, Detected). It holds a device
/// path, pids and executable names only: never a process's arguments or the
/// environment, which may hold secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Terminal {
    /// The device stdin reads, like `/dev/pts/3`. `None` when stdin is no
    /// terminal, which only the command tests' stand-in lets through.
    pub tty: Option<String>,
    pub chain: Vec<Ancestor>,
}

/// One process of a [`Terminal`]'s chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ancestor {
    pub pid: u32,
    /// The executable's name ([`Process::comm`]), without its directory.
    pub name: String,
}

impl Terminal {
    pub fn new(tty: Option<String>, chain: &[Process]) -> Self {
        let chain = chain
            .iter()
            .map(|process| Ancestor {
                pid: process.pid,
                name: process.comm.clone(),
            })
            .collect();
        Self { tty, chain }
    }

    /// The record an event's payload carries.
    pub fn record(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Why this process is not the owner at a terminal, or what the check saw
/// when it is.
pub fn owner_at_terminal() -> Result<Terminal, String> {
    let claudecode = std::env::var_os("CLAUDECODE").is_some_and(|value| !value.is_empty());
    if test_owner() {
        owner_check(claudecode, true, || Ok(Vec::new()))?;
        // The stand-in skips the chain check, but the record still names the
        // real parent chain, so the command tests see what a release records.
        return Ok(Terminal::new(
            stdin_device(),
            &ancestors().unwrap_or_default(),
        ));
    }
    let chain = owner_check(claudecode, std::io::stdin().is_terminal(), ancestors)?;
    Ok(Terminal::new(stdin_device(), &chain))
}

/// The terminal device stdin reads, when it reads one.
fn stdin_device() -> Option<String> {
    if !std::io::stdin().is_terminal() {
        return None;
    }
    #[cfg(target_os = "linux")]
    if let Ok(path) = std::fs::read_link("/proc/self/fd/0") {
        return Some(path.to_string_lossy().into_owned());
    }
    use std::os::unix::fs::MetadataExt;
    let device = std::fs::metadata("/dev/stdin").ok()?.rdev();
    device_named(device, &[Path::new("/dev/pts"), Path::new("/dev")])
}

/// The path of the first character device numbered `device` in `dirs`. With
/// no `/proc` and no unsafe `ttyname`, this is how stdin's device gets its
/// name.
fn device_named(device: u64, dirs: &[&Path]) -> Option<String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    // `/dev` holds hundreds of entries: look at the terminals (`ttys003`)
    // before the rest, so a usual lookup reads a handful, not all of them.
    let (terminals, rest): (Vec<_>, Vec<_>) = dirs
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .partition(|entry| entry.file_name().to_string_lossy().starts_with("tty"));
    terminals
        .into_iter()
        .chain(rest)
        .find(|entry| {
            // `DirEntry::metadata` does not follow a link, so `/dev/stdin`
            // and its kin are skipped.
            entry
                .metadata()
                .is_ok_and(|meta| meta.file_type().is_char_device() && meta.rdev() == device)
        })
        .map(|entry| entry.path().to_string_lossy().into_owned())
}

/// Test builds only: the `test-owner` feature, which `tiller-cli`'s
/// dev-dependencies turn on, lets `TILLER_TEST_OWNER` stand in for a
/// terminal with no Claude Code behind it, so the command tests run owner
/// commands from a pipe. A release build has no such switch.
fn test_owner() -> bool {
    cfg!(feature = "test-owner") && std::env::var_os("TILLER_TEST_OWNER").is_some()
}

/// The owner checks on facts read elsewhere, in the order the cheap ones
/// come first: the parent chain is read only when the rest pass. It returns
/// the chain it walked.
pub fn owner_check(
    claudecode: bool,
    stdin_terminal: bool,
    ancestors: impl FnOnce() -> std::io::Result<Vec<Process>>,
) -> Result<Vec<Process>, String> {
    if claudecode {
        return Err("CLAUDECODE is set".into());
    }
    if !stdin_terminal {
        return Err("stdin is not a terminal".into());
    }
    let chain = ancestors().map_err(|error| format!("the parent chain: {error}"))?;
    match claude_ancestor(&chain) {
        Some(pid) => Err(format!("Claude Code (pid {pid}) is among its ancestors")),
        None => Ok(chain),
    }
}

/// The pid of the first Claude Code process in `chain`.
pub fn claude_ancestor(chain: &[Process]) -> Option<u32> {
    chain
        .iter()
        .find(|process| is_claude_code(process))
        .map(|process| process.pid)
}

/// A Claude Code process, however it was installed: the native binary
/// (`claude`, or `.claude-wrapped` behind a Nix wrapper) or the npm package.
pub fn is_claude_code(process: &Process) -> bool {
    let base = |path: &str| {
        Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let named = |name: &str| {
        let name = name.trim_start_matches('.');
        name.strip_suffix("-wrapped").unwrap_or(name) == "claude"
    };
    let argv0 = process.args.split_whitespace().next().unwrap_or_default();
    named(&base(&process.comm))
        || named(&base(argv0))
        || process.args.contains("@anthropic-ai/claude-code")
}

/// The caller's ancestors, nearest first, up to but not including pid 1.
pub fn ancestors() -> std::io::Result<Vec<Process>> {
    #[cfg(target_os = "linux")]
    let read = read_process;
    #[cfg(not(target_os = "linux"))]
    let table = process_table()?;
    #[cfg(not(target_os = "linux"))]
    let read = |pid: u32| {
        table
            .get(&pid)
            .cloned()
            .ok_or_else(|| std::io::Error::other(format!("pid {pid}: no such process")))
    };
    walk(std::os::unix::process::parent_id(), read)
}

fn walk(
    start: u32,
    mut read: impl FnMut(u32) -> std::io::Result<(u32, Process)>,
) -> std::io::Result<Vec<Process>> {
    let mut chain = Vec::new();
    let mut pid = start;
    while pid > 1 && chain.len() < 128 {
        let (parent, process) = read(pid)?;
        chain.push(process);
        pid = parent;
    }
    Ok(chain)
}

#[cfg(target_os = "linux")]
fn read_process(pid: u32) -> std::io::Result<(u32, Process)> {
    let invalid = |what: &str| std::io::Error::other(format!("pid {pid}: {what}"));
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let open = stat.find('(').ok_or_else(|| invalid("no command name"))?;
    let close = stat.rfind(')').ok_or_else(|| invalid("no command name"))?;
    let comm = stat.get(open + 1..close).unwrap_or_default().to_owned();
    let parent = stat
        .get(close + 1..)
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|ppid| ppid.parse().ok())
        .ok_or_else(|| invalid("no parent pid"))?;
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))?;
    let args = String::from_utf8_lossy(&cmdline)
        .split('\0')
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Ok((parent, Process { pid, comm, args }))
}

/// Off Linux there is no `/proc`, and the workspace forbids the unsafe code
/// that `sysctl` or `libproc` need, so the table comes from `ps`. It runs
/// twice for the whole chain, not three times per ancestor, and only when a
/// command needs to know whether the caller is the owner.
#[cfg(not(target_os = "linux"))]
fn process_table() -> std::io::Result<std::collections::HashMap<u32, (u32, Process)>> {
    let ps = |field: &str| -> std::io::Result<String> {
        let output = std::process::Command::new("ps")
            .args(["-ax", "-o", &format!("pid=,ppid=,{field}=")])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other("ps failed"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    // `pid ppid rest`, where the rest (a command name or the arguments) may
    // hold spaces.
    let rows = |text: &str| -> Vec<(u32, u32, String)> {
        text.lines()
            .filter_map(|line| {
                let mut fields = line.trim_start().splitn(3, char::is_whitespace);
                let pid = fields.next()?.parse().ok()?;
                let parent = fields.next()?.parse().ok()?;
                Some((pid, parent, fields.next()?.trim().to_owned()))
            })
            .collect()
    };
    let args: std::collections::HashMap<u32, String> = rows(&ps("args")?)
        .into_iter()
        .map(|(pid, _, args)| (pid, args))
        .collect();
    Ok(rows(&ps("comm")?)
        .into_iter()
        .map(|(pid, parent, path)| {
            let args = args.get(&pid).cloned().unwrap_or_default();
            // `ps` names the executable by its path; keep its file name, as
            // the kernel's `comm` is on Linux.
            let comm = Path::new(&path)
                .file_name()
                .map_or(path.clone(), |name| name.to_string_lossy().into_owned());
            (pid, (parent, Process { pid, comm, args }))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(comm: &str, args: &str) -> Process {
        Process {
            pid: 42,
            comm: comm.into(),
            args: args.into(),
        }
    }

    fn chain(processes: Vec<Process>) -> impl FnOnce() -> std::io::Result<Vec<Process>> {
        move || Ok(processes)
    }

    fn shell() -> Vec<Process> {
        vec![process("zsh", "-zsh"), process("sshd", "sshd: user@pts/0")]
    }

    #[test]
    fn owner_commands_require_tty() {
        owner_check(false, true, chain(shell())).unwrap();
        let refused = owner_check(false, false, chain(shell())).unwrap_err();
        assert!(refused.contains("not a terminal"), "{refused}");
        let refused = owner_check(true, true, chain(shell())).unwrap_err();
        assert!(refused.contains("CLAUDECODE"), "{refused}");
    }

    #[test]
    fn owner_check_refuses_claude_ancestor() {
        for claude in [
            process("claude", "claude"),
            process(
                ".claude-wrapped",
                "/home/user/.nix-profile/bin/claude --resume",
            ),
            process(
                "node",
                "node /home/user/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ),
            process("2.1.0", "/home/user/.local/bin/claude"),
        ] {
            let mut processes = shell();
            processes.insert(1, claude.clone());
            let refused = owner_check(false, true, chain(processes)).unwrap_err();
            assert!(refused.contains("Claude Code"), "{claude:?}");
        }
        let unreadable =
            owner_check(false, true, || Err(std::io::Error::other("gone"))).unwrap_err();
        assert!(
            unreadable.contains("parent chain"),
            "an unreadable chain is not the owner"
        );
    }

    /// G22: a caller that carries a session id is that session, and one that
    /// carries a retro id is the curator. The CLI names either before it runs
    /// the owner check, and neither is ever the owner, even at a terminal
    /// with no `CLAUDECODE` and no Claude Code ancestor.
    #[test]
    fn owner_commands_refuse_session_callers() {
        use crate::store::Actor;

        // The facts every caller below shares pass the owner check on their own.
        owner_check(false, true, chain(shell())).unwrap();
        for caller in [
            Actor::Session("01ARZ3NDEKTSV4RRFFQ69G5FAV".into()),
            Actor::Curator("01JRETRO".into()),
            Actor::Script,
        ] {
            assert!(!caller.is_owner(), "{caller} resolved as the owner");
            assert!(
                !caller.to_string().starts_with("owner"),
                "{caller} is written as the owner"
            );
        }
        assert!(
            Actor::Owner.is_owner(),
            "the owner principal stays the owner"
        );
    }

    #[test]
    fn walk_follows_parents_and_stops_before_pid_one() {
        let parents = [(30, 20, "zsh"), (20, 10, "claude"), (10, 1, "launchd")];
        let chain = walk(30, |pid| {
            let (_, parent, comm) = parents
                .iter()
                .find(|(candidate, ..)| *candidate == pid)
                .ok_or_else(|| std::io::Error::other("gone"))?;
            Ok((*parent, process_at(pid, comm)))
        })
        .unwrap();
        let comms: Vec<_> = chain.iter().map(|process| process.comm.as_str()).collect();
        assert_eq!(comms, ["zsh", "claude", "launchd"]);
        assert_eq!(claude_ancestor(&chain), Some(20));
        assert!(walk(30, |_| Err(std::io::Error::other("gone"))).is_err());
    }

    fn process_at(pid: u32, comm: &str) -> Process {
        Process {
            pid,
            comm: comm.into(),
            args: comm.into(),
        }
    }

    /// G23: the check hands back the chain it walked, and the record keeps
    /// each process's pid and file name only, never its arguments.
    #[test]
    fn a_passed_check_records_the_chain_without_arguments() {
        let walked = owner_check(
            false,
            true,
            chain(vec![
                process_at(30, "zsh"),
                Process {
                    pid: 20,
                    comm: "sshd".into(),
                    args: "sshd: user@pts/0 TOKEN=hunter2".into(),
                },
            ]),
        )
        .unwrap();
        let terminal = Terminal::new(Some("/dev/pts/3".into()), &walked);
        let record = terminal.record();
        assert_eq!(
            record,
            serde_json::json!({
                "tty": "/dev/pts/3",
                "chain": [{"pid": 30, "name": "zsh"}, {"pid": 20, "name": "sshd"}],
            })
        );
        assert!(!record.to_string().contains("hunter2"), "{record}");
    }

    /// Off Linux, stdin's device is named by its number: `/dev/null` is a
    /// character device every host has.
    #[test]
    fn a_device_is_named_by_its_number() {
        use std::os::unix::fs::MetadataExt;
        let null = std::fs::metadata("/dev/null").unwrap().rdev();
        assert_eq!(
            device_named(null, &[Path::new("/dev")]).as_deref(),
            Some("/dev/null")
        );
        assert_eq!(device_named(null, &[Path::new("/nonexistent")]), None);
    }

    #[test]
    fn other_programs_named_like_claude_are_not_claude_code() {
        for other in [
            process("claude-notes", "claude-notes"),
            process("vim", "vim claude.md"),
        ] {
            assert!(!is_claude_code(&other), "{other:?}");
        }
    }
}
