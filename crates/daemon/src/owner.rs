//! The owner marker in the daemon (`recovery.md` R20). While it names
//! another system the store refuses every write, and the daemon does no
//! outside work either: the board API refuses a mutation before it starts
//! (`api::writable`), and every worker holds its polls ([`Gated`]). Reads
//! keep working, and the workers go on at the flip back.

use std::path::PathBuf;
use std::time::Duration;

use tiller_core::Timestamp;
use tiller_poll::{PollError, Source};

/// A worker that skips its poll while the owner marker in `home` names
/// another system, read again before every poll. A held poll is not a
/// failure: it backs nothing off and reports no error to Health.
pub struct Gated<S> {
    home: PathBuf,
    inner: S,
    held: bool,
}

impl<S: Source> Gated<S> {
    pub fn new(home: impl Into<PathBuf>, inner: S) -> Self {
        Self {
            home: home.into(),
            inner,
            held: false,
        }
    }
}

impl<S: Source> Source for Gated<S> {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn interval(&self) -> Duration {
        self.inner.interval()
    }

    fn retry_after(&self, failures: u32) -> Duration {
        self.inner.retry_after(failures)
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        if let Err(error) = tiller_core::owner::check(&self.home) {
            if !self.held {
                tracing::info!(source = %self.inner.name(), %error, "worker held");
                self.held = true;
            }
            return Ok(());
        }
        if std::mem::take(&mut self.held) {
            tracing::info!(
                source = %self.inner.name(),
                "the owner marker names tiller again; worker resumed"
            );
        }
        self.inner.poll(now)
    }

    fn failed(&mut self, now: Timestamp, failures: u32, error: &PollError) {
        self.inner.failed(now, failures, error);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use tiller_core::owner::MARKER;

    struct Counting(Arc<AtomicU32>);

    impl Source for Counting {
        fn name(&self) -> String {
            "counting".into()
        }

        fn interval(&self) -> Duration {
            Duration::from_secs(1)
        }

        fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// R20: a worker does nothing while the marker names another system,
    /// and polls again once it names tiller, without a daemon restart.
    #[test]
    fn a_worker_holds_while_marker_names_other_system() {
        let dir = tempfile::tempdir().unwrap();
        let polls = Arc::new(AtomicU32::new(0));
        let mut worker = Gated::new(dir.path(), Counting(Arc::clone(&polls)));
        worker.poll(Timestamp::now()).unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 1, "no marker: it polls");

        std::fs::write(dir.path().join(MARKER), "firstmate\n").unwrap();
        worker.poll(Timestamp::now()).unwrap();
        worker.poll(Timestamp::now()).unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 1, "held: no poll");

        std::fs::write(dir.path().join(MARKER), "tiller\n").unwrap();
        worker.poll(Timestamp::now()).unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 2, "the flip back resumes it");
    }

    /// R20 through the board API: while the marker names another system, a
    /// mutation is refused with `owner_marker` and writes nothing, one with
    /// outside work is refused before it, reads keep working, and the flip
    /// back lets the same daemon write again.
    #[tokio::test]
    async fn board_mutations_refused_while_marker_names_other_system() {
        use axum::http::StatusCode;
        use serde_json::{Value, json};
        use tiller_panes::testing::Fleet;

        use crate::api::tests::{
            Fixture, app_with, cards, create, get, mutation, owner_cookie, send,
        };
        use crate::assets::Assets;
        use crate::readonly::Damage;

        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let cookie = owner_cookie(home);
        let app = app_with(home, Assets::of::<Fixture>(), Damage::default());
        let before = cards(&fleet);

        // The marker flips while the daemon runs.
        std::fs::write(home.join(MARKER), "firstmate\n").unwrap();
        let refused = send(&app, create(&[("cookie", &cookie)])).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
        let body: Value = serde_json::from_str(&refused.body).unwrap();
        assert_eq!(body["error"]["code"], "owner_marker");
        assert_eq!(
            body["error"]["message"],
            "read only: the owner marker names \"firstmate\", not \"tiller\""
        );
        assert_eq!(body["error"]["hint"], "tiller owner set tiller");
        assert_eq!(cards(&fleet), before, "a refused mutation writes nothing");

        // Refused before its outside work: this daemon has no orchestrator
        // starter, and would otherwise say so.
        let start = send(
            &app,
            mutation(
                "POST",
                "/api/v1/projects/alpha/orchestrator",
                json!({"idempotency_key": "s-1"}),
                &[("cookie", cookie.as_str())],
            ),
        )
        .await;
        assert_eq!(start.status, StatusCode::FORBIDDEN, "{}", start.body);
        assert!(start.body.contains("\"owner_marker\""), "{}", start.body);

        for path in ["/api/v1/board", "/api/v1/health", "/api/v1/agents"] {
            let read = get(&app, path).await;
            assert_eq!(read.status, StatusCode::OK, "{path}: {}", read.body);
        }

        std::fs::write(home.join(MARKER), "tiller\n").unwrap();
        let applied = send(&app, create(&[("cookie", &cookie)])).await;
        assert_eq!(applied.status, StatusCode::OK, "{}", applied.body);
        assert_eq!(cards(&fleet), before + 1);
    }
}
