//! Pollers: one per source, never overlapping, with backoff and a recorded
//! last success (T2.5; `recovery.md` R05, R18; `terminal-holder.md` 4.1).
//!
//! A [`Poller`] owns its source and runs it on one task, so a slow poll
//! delays the next one instead of overlapping it. A failed poll backs off
//! 5 s, 15 s, 60 s, then every 5 minutes (a source may cap it, as the
//! holder source does), and changes nothing a successful read would:
//! absence of evidence is never `gone` (`recovery.md`, principle 3).
//! [`Registry`] refuses a second poller for a source, serves each poller's
//! [`Health`] to `/health` and the board, and wakes a waiting poller when the
//! owner asks for a poll now ([`Registry::wake`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Serialize;
use tiller_core::Timestamp;
use tokio::sync::{Notify, watch};

/// How long to wait after the first, second and third failure in a row; every
/// later failure waits the last step.
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(300),
];

/// The wait after `failures` failed polls in a row.
pub fn backoff(failures: u32) -> Duration {
    let step = usize::try_from(failures.saturating_sub(1)).unwrap_or(usize::MAX);
    BACKOFF[step.min(BACKOFF.len() - 1)]
}

/// Why a poll failed: the source's own error, or the state store's.
#[derive(Debug, thiserror::Error)]
pub enum PollError {
    /// The source could not be read (the holder, a forge).
    #[error(transparent)]
    Source(Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error(transparent)]
    State(#[from] tiller_core::Error),
}

impl PollError {
    /// A source's error.
    pub fn from_source(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Source(Box::new(error))
    }

    /// The source's error as `E`, when it is one.
    pub fn source_as<E: std::error::Error + 'static>(&self) -> Option<&E> {
        match self {
            Self::Source(error) => error.downcast_ref(),
            Self::State(_) => None,
        }
    }
}

/// Something a poller reads on a schedule.
pub trait Source: Send + 'static {
    /// Unique among the daemon's pollers, such as `holder` or `quota`.
    fn name(&self) -> String;
    fn interval(&self) -> Duration;
    /// The wait after `failures` failed polls in a row.
    fn retry_after(&self, failures: u32) -> Duration {
        backoff(failures)
    }
    fn poll(&mut self, now: Timestamp) -> Result<(), PollError>;
    /// Told after every failed poll, with the count of failures in a row.
    fn failed(&mut self, _now: Timestamp, _failures: u32, _error: &PollError) {}
}

/// One poller's record, as `/health` shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Health {
    pub source: String,
    pub last_attempt_at: Option<Timestamp>,
    pub last_success_at: Option<Timestamp>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

/// A registered poller as the registry holds it: its health, and the bell
/// that ends its wait.
struct Entry {
    health: Arc<Mutex<Health>>,
    wake: Arc<Notify>,
}

type Shared = Arc<Mutex<BTreeMap<String, Entry>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The daemon's pollers, by source name.
#[derive(Clone, Default)]
pub struct Registry {
    pollers: Shared,
}

#[derive(Debug, thiserror::Error)]
#[error("a poller for {0} is already running")]
pub struct Duplicate(pub String);

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A poller for `source`, refused while another poller holds the name.
    pub fn register<S: Source>(&self, source: S) -> Result<Poller<S>, Duplicate> {
        let name = source.name();
        let mut pollers = lock(&self.pollers);
        if pollers.contains_key(&name) {
            return Err(Duplicate(name));
        }
        let health = Arc::new(Mutex::new(Health {
            source: name.clone(),
            ..Health::default()
        }));
        let wake = Arc::new(Notify::new());
        pollers.insert(
            name.clone(),
            Entry {
                health: Arc::clone(&health),
                wake: Arc::clone(&wake),
            },
        );
        Ok(Poller {
            source,
            name,
            health,
            wake,
            registry: Arc::clone(&self.pollers),
        })
    }

    /// Every poller's health, by source name.
    pub fn health(&self) -> Vec<Health> {
        lock(&self.pollers)
            .values()
            .map(|entry| lock(&entry.health).clone())
            .collect()
    }

    /// One poller's health; `None` for a source no poller holds.
    pub fn health_of(&self, source: &str) -> Option<Health> {
        lock(&self.pollers)
            .get(source)
            .map(|entry| lock(&entry.health).clone())
    }

    /// Ends the wait of the poller for `source`, so it polls now instead of
    /// at the end of its interval or backoff. A poller that is polling at
    /// that moment polls once more as soon as it is done. False for a source
    /// no poller holds.
    pub fn wake(&self, source: &str) -> bool {
        match lock(&self.pollers).get(source) {
            Some(entry) => {
                entry.wake.notify_one();
                true
            }
            None => false,
        }
    }
}

/// One source and its schedule. Dropping it frees the name.
pub struct Poller<S: Source> {
    source: S,
    name: String,
    health: Arc<Mutex<Health>>,
    wake: Arc<Notify>,
    registry: Shared,
}

impl<S: Source> Drop for Poller<S> {
    fn drop(&mut self) {
        lock(&self.registry).remove(&self.name);
    }
}

impl<S: Source> Poller<S> {
    pub fn health(&self) -> Health {
        lock(&self.health).clone()
    }

    pub fn source(&self) -> &S {
        &self.source
    }

    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// Polls once and returns how long to wait before the next poll.
    pub fn poll_once(&mut self, now: Timestamp) -> Duration {
        let outcome = self.source.poll(now);
        let mut health = lock(&self.health);
        health.last_attempt_at = Some(now);
        match outcome {
            Ok(()) => {
                health.last_success_at = Some(now);
                health.consecutive_failures = 0;
                health.last_error = None;
                self.source.interval()
            }
            Err(error) => {
                health.consecutive_failures = health.consecutive_failures.saturating_add(1);
                health.last_error = Some(error.to_string());
                let failures = health.consecutive_failures;
                drop(health);
                tracing::warn!(source = %self.name, failures, %error, "poll failed");
                self.source.failed(now, failures, &error);
                self.source.retry_after(failures)
            }
        }
    }

    /// Polls until `stop` turns true. Each poll runs on the blocking pool and
    /// the next is scheduled only after it returns, so polls never overlap.
    /// [`Registry::wake`] ends a wait early.
    pub async fn run(mut self, mut stop: watch::Receiver<bool>) {
        let wake = Arc::clone(&self.wake);
        loop {
            if *stop.borrow() {
                return;
            }
            let polled = tokio::task::spawn_blocking(move || {
                let wait = self.poll_once(Timestamp::now());
                (self, wait)
            })
            .await;
            let wait = match polled {
                Ok((poller, wait)) => {
                    self = poller;
                    wait
                }
                Err(error) => {
                    tracing::error!(%error, "poller task panicked; stopping it");
                    return;
                }
            };
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = wake.notified() => {}
                changed = stop.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A fixed instant plus `seconds`.
    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_millis(1_900_000_000_000 + seconds * 1000).unwrap()
    }

    struct Counting {
        name: &'static str,
        fail: bool,
        running: Arc<AtomicUsize>,
        overlapped: Arc<AtomicUsize>,
    }

    impl Source for Counting {
        fn name(&self) -> String {
            self.name.to_owned()
        }
        fn interval(&self) -> Duration {
            Duration::from_millis(1)
        }
        fn poll(&mut self, _now: Timestamp) -> Result<(), PollError> {
            if self.running.fetch_add(1, Ordering::SeqCst) > 0 {
                self.overlapped.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(5));
            self.running.fetch_sub(1, Ordering::SeqCst);
            if self.fail {
                Err(PollError::from_source(std::io::Error::other("down")))
            } else {
                Ok(())
            }
        }
    }

    fn counting(name: &'static str, fail: bool) -> Counting {
        Counting {
            name,
            fail,
            running: Arc::default(),
            overlapped: Arc::default(),
        }
    }

    #[test]
    fn backoff_steps_then_holds_at_five_minutes() {
        let waits: Vec<u64> = (1..=6).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(waits, [5, 15, 60, 300, 300, 300]);
    }

    #[test]
    fn one_poller_per_source_and_health_records_last_success() {
        let registry = Registry::new();
        let mut poller = registry.register(counting("local", false)).unwrap();
        assert!(registry.register(counting("local", false)).is_err());
        assert!(registry.register(counting("other", false)).is_ok());

        let wait = poller.poll_once(at(0));
        assert_eq!(wait, Duration::from_millis(1));
        poller.source_mut().fail = true;
        assert_eq!(poller.poll_once(at(1)), Duration::from_secs(5));
        assert_eq!(poller.poll_once(at(2)), Duration::from_secs(15));
        let health = registry
            .health()
            .into_iter()
            .find(|h| h.source == "local")
            .unwrap();
        assert_eq!(health.last_success_at, Some(at(0)));
        assert_eq!(health.last_attempt_at, Some(at(2)));
        assert_eq!(health.consecutive_failures, 2);
        assert!(health.last_error.unwrap().contains("down"));

        drop(poller);
        assert!(
            registry.register(counting("local", false)).is_ok(),
            "a stopped poller frees its source"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn polls_never_overlap() {
        let registry = Registry::new();
        let source = counting("slow", false);
        let overlapped = Arc::clone(&source.overlapped);
        let poller = registry.register(source).unwrap();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(poller.run(stopped));
        tokio::time::sleep(Duration::from_millis(80)).await;
        stop.send(true).unwrap();
        task.await.unwrap();
        let health = registry.health();
        assert!(health.is_empty(), "the finished poller freed its name");
        assert_eq!(overlapped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wake_ends_a_backoff_wait_at_once() {
        let registry = Registry::new();
        let poller = registry.register(counting("ci", true)).unwrap();
        assert!(!registry.wake("no-such-poller"));
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(poller.run(stopped));
        let attempts = |registry: &Registry| registry.health_of("ci").unwrap().consecutive_failures;
        let started = std::time::Instant::now();
        while attempts(&registry) < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The first failure waits 5 s; a wake polls again long before that.
        assert!(registry.wake("ci"));
        while attempts(&registry) < 2 {
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "the wake did not end the wait"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        stop.send(true).unwrap();
        task.await.unwrap();
        assert!(
            registry.health_of("ci").is_none(),
            "the finished poller freed its name"
        );
    }

    #[test]
    fn a_source_error_downcasts_to_its_type() {
        let error = PollError::from_source(std::io::Error::other("down"));
        assert_eq!(
            error.source_as::<std::io::Error>().map(ToString::to_string),
            Some("down".to_owned())
        );
        assert!(error.source_as::<std::fmt::Error>().is_none());
    }
}
