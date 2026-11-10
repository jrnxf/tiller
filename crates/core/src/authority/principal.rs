//! The CLI owner principal (`authority.md` section 3, G22): no
//! `CLAUDECODE`, stdin a terminal, and no Claude Code process among the
//! caller's ancestors (the parent chain read from the OS). A CLI call with no
//! session id that fails any of these is a script, never the owner.

use std::io::IsTerminal;
use std::path::Path;

/// One process in the caller's parent chain.
#[derive(Clone, Debug)]
pub struct Process {
    pub pid: u32,
    pub comm: String,
    pub args: String,
}

/// Why this process is not the owner at a terminal, or `Ok` when it is.
pub fn owner_at_terminal() -> Result<(), String> {
    let claudecode = std::env::var_os("CLAUDECODE").is_some_and(|value| !value.is_empty());
    if test_owner() {
        return owner_check(claudecode, true, || Ok(Vec::new()));
    }
    owner_check(claudecode, std::io::stdin().is_terminal(), ancestors)
}

/// Test builds only: the `test-owner` feature, which `tiller-cli`'s
/// dev-dependencies turn on, lets `TILLER_TEST_OWNER` stand in for a
/// terminal with no Claude Code behind it, so the command tests run owner
/// commands from a pipe. A release build has no such switch.
fn test_owner() -> bool {
    cfg!(feature = "test-owner") && std::env::var_os("TILLER_TEST_OWNER").is_some()
}

/// The owner checks on facts read elsewhere, in the order the cheap ones
/// come first: the parent chain is read only when the rest pass.
pub fn owner_check(
    claudecode: bool,
    stdin_terminal: bool,
    ancestors: impl FnOnce() -> std::io::Result<Vec<Process>>,
) -> Result<(), String> {
    if claudecode {
        return Err("CLAUDECODE is set".into());
    }
    if !stdin_terminal {
        return Err("stdin is not a terminal".into());
    }
    let chain = ancestors().map_err(|error| format!("the parent chain: {error}"))?;
    match claude_ancestor(&chain) {
        Some(pid) => Err(format!("Claude Code (pid {pid}) is among its ancestors")),
        None => Ok(()),
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
    let mut chain = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && chain.len() < 128 {
        let (parent, process) = read_process(pid)?;
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

#[cfg(not(target_os = "linux"))]
fn read_process(pid: u32) -> std::io::Result<(u32, Process)> {
    let ps = |field: &str| -> std::io::Result<String> {
        let output = std::process::Command::new("ps")
            .args(["-o", field, "-p", &pid.to_string()])
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!("ps found no pid {pid}")));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let parent = ps("ppid=")?
        .parse()
        .map_err(|_| std::io::Error::other(format!("pid {pid}: no parent pid")))?;
    let comm = ps("comm=")?;
    let args = ps("args=")?;
    Ok((parent, Process { pid, comm, args }))
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
