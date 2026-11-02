//! Startup stays under 10 ms (T2.2 acceptance): the lower quartile of repeated runs
//! of a command that needs no database, and of one that opens it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::{Command, Stdio};
use std::time::Instant;

use common::World;

fn quartile_ms(world: &World, args: &[&str]) -> f64 {
    let run = || {
        let started = Instant::now();
        let status = Command::new(env!("CARGO_BIN_EXE_tiller"))
            .env("TILLER_HOME", world.home())
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        started.elapsed().as_secs_f64() * 1000.0
    };
    run(); // warm the file cache
    let mut times: Vec<f64> = (0..31).map(|_| run()).collect();
    times.sort_by(f64::total_cmp);
    // The lower quartile: scheduling noise only ever adds time, so it is the
    // figure that does not move when other test binaries share the machine.
    times[times.len() / 4]
}

#[test]
fn startup_is_under_ten_milliseconds() {
    let world = World::initialized();
    world.must(&[
        "card", "create", "alpha", "--kind", "ship", "--title", "One",
    ]);
    for args in [&["version"][..], &["card", "show", "alpha-1"][..]] {
        // Three rounds, so a noisy neighbour on a shared runner is not a failure;
        // a real regression is slow in every round.
        let quartiles: Vec<f64> = (0..3).map(|_| quartile_ms(&world, args)).collect();
        let best = quartiles.iter().copied().fold(f64::INFINITY, f64::min);
        assert!(
            best < 10.0,
            "{args:?} took {quartiles:.1?} ms at the quartile"
        );
    }
}
