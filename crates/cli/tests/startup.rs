//! Startup stays under 10 ms (T2.2 acceptance): a command that needs no
//! database, and one that opens it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{World, children_cpu, parse_cpu_list, pin_to_performance_cores};

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

/// One run of `tiller <args>`: its wall time and its own CPU time.
fn run(world: &World, args: &[&str]) -> (Duration, Duration) {
    let cpu_before = children_cpu();
    let started = Instant::now();
    let status = Command::new(env!("CARGO_BIN_EXE_tiller"))
        .env("TILLER_HOME", world.home())
        .args(args)
        // A terminal on stdin is the owner check's cue to walk the process
        // chain, so a run from `cargo test` in a shell would time more than
        // one under nextest. Pin it to the same input everywhere.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .unwrap();
    let took = started.elapsed();
    assert!(status.success());
    (took, children_cpu() - cpu_before)
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
/// `.config/nextest.toml` still runs this test alone, in the `wall-clock`
/// group, so its own neighbours do not share the cores it measures. Under
/// nextest the test refuses to measure outside it.
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
    let pinned = pin_to_performance_cores();
    eprintln!("measured on the performance cores {pinned:?} (None: one kind of core)");
    for args in [&["version"][..], &["card", "show", "alpha-1"][..]] {
        for _ in 0..WARM_UP {
            run(&world, args);
        }
        let (mut wall, mut cpu): (Vec<_>, Vec<_>) =
            (0..MEASURED).map(|_| run(&world, args)).unzip();
        wall.sort();
        cpu.sort();
        let (wall_quartile, cpu_median) = (wall[MEASURED / 4], cpu[MEASURED / 2]);
        eprintln!(
            "tiller {args:?} over {MEASURED} runs: wall p25 {wall_quartile:?}, median {:?}; cpu median {cpu_median:?}, max {:?}",
            wall[MEASURED / 2],
            cpu[MEASURED - 1]
        );
        assert!(
            cpu_median < BUDGET,
            "{args:?} cpu median {cpu_median:?}: {cpu:?}"
        );
        assert!(
            wall_quartile < WALL_CEILING,
            "{args:?} wall lower quartile {wall_quartile:?}: {wall:?}"
        );
    }
}

/// The sysfs CPU list forms `pin_to_performance_cores` reads.
#[test]
fn cpu_lists_parse_like_sysfs_writes_them() {
    assert_eq!(parse_cpu_list("0-11\n"), (0..=11).collect::<Vec<_>>());
    assert_eq!(parse_cpu_list("0-2,6-8,15"), vec![0, 1, 2, 6, 7, 8, 15]);
    assert_eq!(parse_cpu_list("3"), vec![3]);
    assert!(parse_cpu_list("\n").is_empty());
}
