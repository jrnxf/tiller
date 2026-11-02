//! Process liveness checks: step 3 of the liveness ladder (`recovery.md`
//! section 2). The probe targets the agent process in the pane, never the
//! shell, which outlives a killed agent.

use std::collections::HashMap;
use std::process::Command;

use serde_json::json;
use tiller_core::domain::{Activity, ObservationSource, Session};
use tiller_core::state::runs::{NewObservation, Recorded};
use tiller_core::{Applied, Ctx, Store, Timestamp};

/// What a probe found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Alive,
    Dead,
    /// The probe itself failed; the ladder stops and tries next pass.
    Unknown,
}

/// The process name Claude Code runs as.
pub const AGENT_PROCESS: &str = "claude";

/// Whether process `pid` exists, for any user's process (a signal-0 probe
/// would read another user's as dead).
pub fn process_alive(pid: u32) -> Probe {
    if cfg!(target_os = "linux") {
        return match std::fs::metadata(format!("/proc/{pid}")) {
            Ok(_) => Probe::Alive,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Probe::Dead,
            Err(_) => Probe::Unknown,
        };
    }
    match Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
    {
        Ok(out) if out.status.success() && !out.stdout.trim_ascii().is_empty() => Probe::Alive,
        Ok(out) if out.stdout.trim_ascii().is_empty() => Probe::Dead,
        _ => Probe::Unknown,
    }
}

/// The first process named `name` under `root` (the pane's shell), searched
/// through the whole process tree.
pub fn find_descendant(root: u32, name: &str) -> Result<Option<u32>, String> {
    let table = if cfg!(target_os = "linux") {
        proc_table()?
    } else {
        let out = Command::new("ps")
            .args(["-A", "-o", "pid=,ppid=,comm="])
            .output()
            .map_err(|error| format!("ps failed: {error}"))?;
        if !out.status.success() {
            return Err(format!("ps exited {}", out.status));
        }
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    Ok(descendant_in(&table, root, name))
}

/// The process table from `/proc`, in the `ps -o pid=,ppid=,comm=` shape.
/// A process that exits mid-read is skipped.
fn proc_table() -> Result<String, String> {
    let entries = std::fs::read_dir("/proc").map_err(|error| format!("/proc: {error}"))?;
    let mut table = String::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid ...`; comm may hold spaces and parentheses.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let comm = &stat[open + 1..close];
        let Some(ppid) = stat[close + 1..].split_whitespace().nth(1) else {
            continue;
        };
        table.push_str(&format!("{pid} {ppid} {comm}\n"));
    }
    Ok(table)
}

/// [`find_descendant`] over `ps -o pid=,ppid=,comm=` output. `comm` may be a
/// full path on macOS, so only its last component is compared.
fn descendant_in(ps: &str, root: u32, name: &str) -> Option<u32> {
    let mut children: HashMap<u32, Vec<(u32, String)>> = HashMap::new();
    for line in ps.lines() {
        let mut fields = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse(), ppid.parse()) else {
            continue;
        };
        let comm = fields.collect::<Vec<_>>().join(" ");
        let base = comm.rsplit('/').next().unwrap_or_default().to_owned();
        children.entry(ppid).or_default().push((pid, base));
    }
    let mut queue = vec![root];
    while let Some(parent) = queue.pop() {
        for (pid, comm) in children.get(&parent).into_iter().flatten() {
            if *comm == name {
                return Some(*pid);
            }
            queue.push(*pid);
        }
    }
    None
}

/// The agent under a pane's shell: alive when a `claude` process runs under
/// it, dead when none does, unknown when `ps` fails.
pub fn agent_probe(pane_pid: u32) -> Probe {
    match find_descendant(pane_pid, AGENT_PROCESS) {
        Ok(Some(_)) => Probe::Alive,
        Ok(None) => Probe::Dead,
        Err(_) => Probe::Unknown,
    }
}

/// Records a probe as a `liveness` observation: `agent_alive` (no verdict,
/// the evidence for a silent session) or `agent_dead` (verdict `unknown`,
/// which the row keeps until reconcile acts). An unknown probe records
/// nothing.
pub fn record(
    store: &mut Store,
    ctx: &Ctx,
    session: &Session,
    probe: Probe,
    pid: Option<u32>,
) -> tiller_core::Result<Option<Applied<Recorded>>> {
    let (event, verdict) = match probe {
        Probe::Alive => ("agent_alive", None),
        Probe::Dead => ("agent_dead", Some(Activity::Unknown)),
        Probe::Unknown => return Ok(None),
    };
    store
        .observation_record(
            ctx,
            NewObservation {
                session_id: Some(session.id.clone()),
                run_id: session.run_id.clone(),
                source: ObservationSource::Liveness,
                event: event.to_owned(),
                verdict,
                observed_at: Timestamp::now(),
                payload: json!({"pid": pid}),
            },
        )
        .map(Some)
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use tiller_core::domain::Activity;
    use tiller_core::{Actor, Ctx, Store};

    use super::*;
    use crate::testing::{fleet, session};

    #[test]
    fn a_running_process_is_alive_and_a_reaped_one_is_dead() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        assert_eq!(process_alive(pid), Probe::Alive);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(process_alive(pid), Probe::Dead);
    }

    /// The probe looks for the agent under the pane's shell, not the shell:
    /// a shell whose agent died is dead.
    #[test]
    fn agent_under_a_live_shell_is_found_by_name() {
        let mut shell = Command::new("sh")
            .args(["-c", "sleep 30 & wait"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let root = shell.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        let found = loop {
            if let Some(pid) = find_descendant(root, "sleep").unwrap() {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "sleep never appeared under the shell"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_ne!(found, root);
        assert_eq!(agent_probe(root), Probe::Dead, "no claude under this shell");
        shell.kill().unwrap();
        shell.wait().unwrap();
        let _ = Command::new("kill").arg(found.to_string()).status();
    }

    #[test]
    fn descendant_search_walks_the_tree_and_compares_basenames() {
        let ps = "  1     0 /sbin/launchd\n 50     1 -zsh\n 51    50 node\n 52    51 /opt/bin/claude\n 60     1 claude\n";
        assert_eq!(descendant_in(ps, 50, "claude"), Some(52));
        assert_eq!(descendant_in(ps, 51, "claude"), Some(52));
        assert_eq!(descendant_in(ps, 52, "claude"), None);
    }

    #[test]
    fn dead_agent_reads_unknown_and_alive_moves_nothing() {
        let mut store = Store::open_in_memory().unwrap();
        let fleet = fleet(&mut store);
        let worker = session(&store, &fleet.worker);
        let ctx = Ctx::new(Actor::Daemon);
        store
            .observation_record(
                &ctx,
                NewObservation {
                    session_id: Some(worker.id.clone()),
                    run_id: worker.run_id.clone(),
                    source: ObservationSource::Hook,
                    event: "PostToolUse".into(),
                    verdict: Some(Activity::Working),
                    observed_at: Timestamp::from_millis(Timestamp::now().as_millis() - 1000)
                        .unwrap(),
                    payload: serde_json::json!({}),
                },
            )
            .unwrap();
        assert!(
            record(&mut store, &ctx, &worker, Probe::Unknown, None)
                .unwrap()
                .is_none()
        );
        let alive = record(&mut store, &ctx, &worker, Probe::Alive, Some(7))
            .unwrap()
            .unwrap();
        assert_eq!(alive.value.observation.event, "agent_alive");
        assert!(!alive.value.verdict_changed);
        assert_eq!(session(&store, &fleet.worker).activity, Activity::Working);
        record(&mut store, &ctx, &worker, Probe::Dead, Some(7)).unwrap();
        let after = session(&store, &fleet.worker);
        assert_eq!(after.activity, Activity::Unknown);
        assert!(after.ended_at.is_none(), "only reconcile ends a session");
    }
}
