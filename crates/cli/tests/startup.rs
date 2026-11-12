//! Startup stays under 10 ms (T2.2 acceptance): a command that needs no
//! database, and one that opens it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{CALLER_ENV, World, children_usage, parse_cpu_list, pin_to_performance_cores};

/// The T2.2 budget for one command, start to exit.
const BUDGET: Duration = Duration::from_millis(10);
/// A wall-clock ceiling that only a wait would reach: a command that blocks on
/// a lock, a socket or a sleep takes far longer, while load adds a few
/// milliseconds (24 ms at a load average of 22 on a 12-core host).
const WALL_CEILING: Duration = Duration::from_millis(50);
/// Unmeasured runs that warm the page cache first.
const WARM_UP: usize = 3;
/// Measured runs of each command.
const MEASURED: usize = 40;

/// One measured run of `tiller <args>`.
#[derive(Debug)]
struct Run {
    wall: Duration,
    /// Its own CPU time, user plus system.
    cpu: Duration,
    /// How often the scheduler took its core away: high on a busy host,
    /// while its own work stays the same.
    preempted: i64,
    /// Its minor page faults: about the same from run to run (to within 1%
    /// across sets on one host) unless the binary does more work.
    faults: i64,
    /// The core its main thread ended on (`None` where `/proc` cannot say).
    core: Option<usize>,
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?} wall, {:?} cpu, {} preempted, {} faults",
            self.wall, self.cpu, self.preempted, self.faults
        )?;
        match self.core {
            Some(core) => write!(f, ", main thread ended on core {core}"),
            None => Ok(()),
        }
    }
}

/// The runs in the order they ran, one a line.
fn listed(runs: &[Run]) -> String {
    runs.iter()
        .enumerate()
        .map(|(index, run)| format!("\n  run {index}: {run}"))
        .collect()
}

fn run(world: &World, args: &[&str]) -> Run {
    let before = children_usage();
    let started = Instant::now();
    let mut child = world
        .bare_command()
        .args(args)
        // A terminal on stdin is the owner check's cue to walk the process
        // chain, so a run from `cargo test` in a shell would time more than
        // one under nextest. Pin it to the same input everywhere.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let exited = wait_unreaped(&child);
    let mut wall = started.elapsed();
    let core = if exited { last_core(&child) } else { None };
    let status = child.wait().unwrap();
    if !exited {
        // Without the unreaped wait, the clock stops at the reap.
        wall = started.elapsed();
    }
    assert!(status.success());
    let after = children_usage();
    Run {
        wall,
        cpu: after.cpu - before.cpu,
        preempted: after.preempted - before.preempted,
        faults: after.faults - before.faults,
        core,
    }
}

/// Waits for the child to exit but leaves it unreaped (`waitid` with
/// `WNOWAIT`), so its `/proc` entry stays readable. Its usage reaches
/// `getrusage` only at the reap, so the measurement does not change. False
/// where that wait is not available.
fn wait_unreaped(child: &std::process::Child) -> bool {
    #[cfg(target_os = "linux")]
    {
        use nix::errno::Errno;
        use nix::sys::wait::{Id, WaitPidFlag, waitid};
        let Ok(pid) = i32::try_from(child.id()) else {
            return false;
        };
        loop {
            match waitid(
                Id::Pid(nix::unistd::Pid::from_raw(pid)),
                WaitPidFlag::WEXITED | WaitPidFlag::WNOWAIT,
            ) {
                Err(Errno::EINTR) => {}
                result => return result.is_ok(),
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = child;
        false
    }
}

/// The core an exited, unreaped child ended on: field 39 of its
/// `/proc/<pid>/stat`, `processor`. It names the main thread's last core,
/// not every core the run used.
fn last_core(child: &std::process::Child) -> Option<usize> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id())).ok()?;
    // The command name in field 2 can hold spaces, so count from the `)`
    // that ends it: field 3 is the first one after it.
    let (_, after_name) = stat.rsplit_once(')')?;
    after_name.split_whitespace().nth(39 - 3)?.parse().ok()
}

/// The host's 1, 5 and 15 minute load averages.
fn load_average() -> String {
    std::fs::read_to_string("/proc/loadavg").map_or_else(
        |_| "unknown".to_owned(),
        |text| {
            text.split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ")
        },
    )
}

/// What a failure needs to tell a slower binary from a busy or slower host:
/// the load average before the runs and now, beside the host's online cores,
/// the cores this process may use (affinity and cgroup quota, before the
/// pin), the CPU model and the cores the runs were pinned to.
fn host(
    load_before: &str,
    usable: Option<std::num::NonZeroUsize>,
    pinned: Option<&[usize]>,
) -> String {
    let load = load_average();
    let count =
        |cores: Option<usize>| cores.map_or_else(|| "unknown".to_owned(), |n| n.to_string());
    let online = count(
        std::fs::read_to_string("/sys/devices/system/cpu/online")
            .ok()
            .map(|text| parse_cpu_list(&text).len())
            .filter(|&n| n > 0),
    );
    let usable = count(usable.map(usize::from));
    let model = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            // x86 names the model; Arm gives only the part number.
            ["model name", "CPU part"].iter().find_map(|&key| {
                text.lines()
                    .filter_map(|line| line.split_once(':'))
                    .filter(|(name, _)| name.trim() == key)
                    .map(|(_, value)| value.trim())
                    .find(|value| !value.is_empty())
                    .map(str::to_owned)
            })
        })
        .unwrap_or_else(|| "unknown".to_owned());
    format!(
        "load average {load_before} before the runs and {load} after, on {online} online cores ({usable} usable), cpu {model}, pinned to {pinned:?}"
    )
}

/// Wall time moves with whatever else the host does: one run measured 24 ms
/// at a load average of 22, with the binary unchanged. So the budget is
/// checked against the command's own CPU time (user plus system, from
/// `getrusage`): the median must stay inside it, which fails if every run
/// gets more work. The wall lower quartile must stay inside a ceiling that
/// only a wait reaches, which fails if startup blocks.
///
/// CPU time still grew with load on a hybrid CPU (a median of about 11 ms at
/// load averages of 18 to 25), because the scheduler moved the runs onto
/// efficient cores, which take about 1.5 times as long for the same work. So
/// the runs are pinned to the performance cores first, where the budget
/// applies.
///
/// The pin does not remove two more host effects: a busy SMT sibling and a
/// lower clock when the package runs hot. At a load average of about 7, one
/// set of `card show` runs took a CPU median of 11.9 ms against 6.8 ms a
/// minute later, with the same page faults (1274 and 1276): 8.5 ms on the
/// one core whose sibling was offline, 12 ms on the others.
///
/// `.config/nextest.toml` still runs this test alone, in the `wall-clock`
/// group, so its own neighbours do not share the cores it measures. Under
/// nextest the test refuses to measure outside it. Under plain `cargo test`
/// a neighbour's children can land in one run's usage.
///
/// A failure lists every run in order with its preemptions, page faults and
/// the core its main thread ended on, plus the load average and the CPU
/// model. Many more faults point at the binary; more preemptions, a busy
/// core or a slower CPU point at the host.
#[test]
fn startup_is_under_ten_milliseconds() {
    if std::env::var_os("NEXTEST_RUN_ID").is_some() {
        assert_eq!(
            std::env::var("NEXTEST_TEST_GROUP").as_deref(),
            Ok("wall-clock"),
            "the nextest override that runs this test alone no longer matches it"
        );
    }
    let world = World::initialized();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    let usable = std::thread::available_parallelism().ok();
    let pinned = pin_to_performance_cores();
    eprintln!("measured on the performance cores {pinned:?} (None: one kind of core)");
    for args in [&["version"][..], &["card", "show", "alpha-1"][..]] {
        let load_before = load_average();
        for _ in 0..WARM_UP {
            run(&world, args);
        }
        let runs: Vec<_> = (0..MEASURED).map(|_| run(&world, args)).collect();
        let mut wall: Vec<_> = runs.iter().map(|run| run.wall).collect();
        wall.sort();
        let mut cpu: Vec<_> = runs.iter().map(|run| run.cpu).collect();
        cpu.sort();
        let (wall_quartile, cpu_median) = (wall[MEASURED / 4], cpu[MEASURED / 2]);
        eprintln!(
            "tiller {args:?} over {MEASURED} runs: wall p25 {wall_quartile:?}, median {:?}; cpu median {cpu_median:?}, max {:?}",
            wall[MEASURED / 2],
            cpu[MEASURED - 1]
        );
        assert!(
            cpu_median < BUDGET,
            "{args:?} cpu median {cpu_median:?} at {}; the runs:{}",
            host(&load_before, usable, pinned.as_deref()),
            listed(&runs)
        );
        assert!(
            wall_quartile < WALL_CEILING,
            "{args:?} wall lower quartile {wall_quartile:?} at {}; the runs:{}",
            host(&load_before, usable, pinned.as_deref()),
            listed(&runs)
        );
    }
}

/// Set in the copy of this test binary that
/// `a_caller_environment_does_not_reach_the_timed_command` starts.
const CALLER_CHILD: &str = "TILLER_STARTUP_CALLER_CHILD";

/// The timed command runs against the test home whatever the caller's shell
/// sets: with a developer's `TILLER_DB`, `card show` would read another
/// database. A test cannot change its own environment safely, so it runs
/// this one test again in a child process with every [`CALLER_ENV`] name set.
#[test]
fn a_caller_environment_does_not_reach_the_timed_command() {
    if std::env::var_os(CALLER_CHILD).is_some() {
        let world = World::initialized();
        world.must(&[
            "card", "create", "alpha", "--kind", "ship", "--title", "One",
        ]);
        run(&world, &["card", "show", "alpha-1"]);
        return;
    }
    let elsewhere = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "a_caller_environment_does_not_reach_the_timed_command",
            "--test-threads",
            "1",
        ])
        .env(CALLER_CHILD, "1");
    for name in CALLER_ENV {
        child.env(name, elsewhere.path().join("caller.db"));
    }
    let out = child.output().unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "the child ran no test: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The sysfs CPU list forms `pin_to_performance_cores` reads.
#[test]
fn cpu_lists_parse_like_sysfs_writes_them() {
    assert_eq!(parse_cpu_list("0-11\n"), (0..=11).collect::<Vec<_>>());
    assert_eq!(parse_cpu_list("0-2,6-8,15"), vec![0, 1, 2, 6, 7, 8, 15]);
    assert_eq!(parse_cpu_list("3"), vec![3]);
    assert!(parse_cpu_list("\n").is_empty());
}
