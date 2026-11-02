//! Test hooks compiled only into the core's own test binary.

/// Set to a file path, makes the next mutation write its name to that file
/// and then hang just before `COMMIT`, so a test can kill the process with
/// the transaction open.
pub(crate) const PAUSE_ENV: &str = "TILLER_TEST_PAUSE_BEFORE_COMMIT";

pub(crate) fn pause_before_commit(txn: &str) {
    if let Ok(marker) = std::env::var(PAUSE_ENV)
        && std::fs::write(&marker, txn).is_ok()
    {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}
