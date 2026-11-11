//! Waiting for the threads the daemon's board runs (curators, holdout
//! evals) run on, when it stops.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Joins each thread that ends before `deadline`, and leaves the rest
/// running.
pub(crate) fn join_until(threads: Vec<JoinHandle<()>>, deadline: Instant) {
    for thread in threads {
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if thread.is_finished() {
            let _ = thread.join();
        }
    }
}
