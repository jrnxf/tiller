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
    process_chain(std::os::unix::process::parent_id(), 128)
}

/// Process `start` and its ancestors, nearest first, at most `limit` of
/// them, up to but not including pid 1.
fn process_chain(start: u32, limit: usize) -> std::io::Result<Vec<Process>> {
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
    walk(start, limit, read)
}

/// The daemon's check of a caller on its owner socket (`authority.md`
/// section 7, Owner CLI socket): the kernel gave the caller's `uid` and
/// `pid`. The caller is the owner when its uid is the daemon's own
/// (`own_uid`) and neither it nor any of its ancestors is Claude Code. The
/// daemon cannot read another process's environment, so the CLI runs its
/// own owner check (`CLAUDECODE`, stdin a terminal) before it connects. In
/// a test build with `TILLER_TEST_OWNER` set on the daemon, only the
/// caller itself is looked at, as [`owner_at_terminal`] skips the chain
/// there.
pub fn socket_owner(uid: u32, own_uid: u32, pid: Option<i32>) -> Result<(), String> {
    let limit = if test_owner() { 1 } else { 128 };
    socket_owner_check(uid, own_uid, pid, |pid| process_chain(pid, limit))
}

/// [`socket_owner`] on facts read elsewhere: `chain` reads the caller and
/// its ancestors, nearest first.
pub fn socket_owner_check(
    uid: u32,
    own_uid: u32,
    pid: Option<i32>,
    chain: impl FnOnce(u32) -> std::io::Result<Vec<Process>>,
) -> Result<(), String> {
    if uid != own_uid {
        return Err(format!("uid {uid} is not the daemon's own uid {own_uid}"));
    }
    let Some(pid) = pid
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 1)
    else {
        return Err("the system does not say which process is calling".into());
    };
    let chain = chain(pid).map_err(|error| format!("the caller's process chain: {error}"))?;
    match claude_ancestor(&chain) {
        Some(found) if found == pid => Err(format!("the caller (pid {pid}) is Claude Code")),
        Some(found) => Err(format!(
            "Claude Code (pid {found}) is among the caller's ancestors"
        )),
        None => Ok(()),
    }
}

fn walk(
    start: u32,
    limit: usize,
    mut read: impl FnMut(u32) -> std::io::Result<(u32, Process)>,
) -> std::io::Result<Vec<Process>> {
    let mut chain = Vec::new();
    let mut pid = start;
    while pid > 1 && chain.len() < limit {
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
        let output = std::process::Command::new("/bin/ps")
            .args(["-ax", "-o", &format!("pid=,ppid=,{field}=")])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other("ps failed"));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let args: std::collections::HashMap<u32, String> = ps_rows(&ps("args")?)
        .into_iter()
        .map(|(pid, _, args)| (pid, args))
        .collect();
    Ok(ps_rows(&ps("comm")?)
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

/// The rows of `ps -o pid=,ppid=,<field>=`: `pid ppid rest`, where the rest
/// (a command name or the arguments) may hold spaces. `ps` pads each column
/// to its widest value, so any run of spaces separates the fields.
#[cfg(any(test, not(target_os = "linux")))]
fn ps_rows(text: &str) -> Vec<(u32, u32, String)> {
    text.lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, rest) = rest.trim_start().split_once(char::is_whitespace)?;
            Some((
                pid.parse().ok()?,
                parent.parse().ok()?,
                rest.trim().to_owned(),
            ))
        })
        .collect()
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
        let chain = walk(30, 128, |pid| {
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
        assert!(walk(30, 128, |_| Err(std::io::Error::other("gone"))).is_err());
    }

    /// `ps` pads its columns, so a short ppid (a terminal app's 1) sits
    /// after several spaces; that row must still parse.
    #[test]
    fn ps_rows_read_padded_columns() {
        let text = "    1     0 /sbin/launchd\n\
                    \x2018528     1 /Applications/Terminal.app/Contents/MacOS/Terminal\n\
                    \x2095292 18528 login -pf user\n\
                    not a row\n";
        assert_eq!(
            ps_rows(text),
            [
                (1, 0, "/sbin/launchd".to_owned()),
                (
                    18528,
                    1,
                    "/Applications/Terminal.app/Contents/MacOS/Terminal".to_owned()
                ),
                (95292, 18528, "login -pf user".to_owned()),
            ]
        );
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

    /// The owner socket's rule (`authority.md` section 7): the daemon's own
    /// uid, a caller the kernel names, and no Claude Code in its chain.
    /// The test host has no second uid, so the rule runs on crafted facts.
    #[test]
    fn the_socket_serves_only_the_owners_uid_with_no_claude_in_the_chain() {
        let chain = |names: &'static [&'static str]| {
            move |pid: u32| -> std::io::Result<Vec<Process>> {
                Ok(names
                    .iter()
                    .enumerate()
                    .map(|(at, comm)| process_at(pid + at as u32, comm))
                    .collect())
            }
        };
        assert_eq!(
            socket_owner_check(501, 501, Some(40), chain(&["tiller", "zsh", "Terminal"])),
            Ok(())
        );
        for uid in [0, 502, u32::MAX] {
            let refused = socket_owner_check(uid, 501, Some(40), chain(&["tiller"])).unwrap_err();
            assert!(
                refused.contains("not the daemon's own uid 501"),
                "{refused}"
            );
        }
        let unknown = socket_owner_check(501, 501, None, chain(&["tiller"])).unwrap_err();
        assert!(unknown.contains("does not say"), "{unknown}");
        let itself = socket_owner_check(501, 501, Some(40), chain(&["claude"])).unwrap_err();
        assert_eq!(itself, "the caller (pid 40) is Claude Code");
        let below = socket_owner_check(501, 501, Some(40), chain(&["tiller", "zsh", "claude"]))
            .unwrap_err();
        assert_eq!(
            below,
            "Claude Code (pid 42) is among the caller's ancestors"
        );
        let gone = socket_owner_check(501, 501, Some(40), |_| Err(std::io::Error::other("gone")))
            .unwrap_err();
        assert!(gone.contains("gone"), "{gone}");
    }
}
