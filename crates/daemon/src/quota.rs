//! The quota feed (`protocol.md` section 8): `quota-axi --provider <p> --full
//! --json` every five minutes, parsed by allowlist into `quota_windows`.
//!
//! The output structs below have only the allowlisted fields. serde skips
//! every other field without reading it into memory as a value, so the
//! `account` and `accountKeys` blocks, `attempts` and `source` can never be
//! stored, logged or sent, even if quota-axi renames a field. Raw stdout is
//! never logged, and a failure records a short code, never stderr.

use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tiller_core::state::caches::QuotaReading;
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_herdr::poller::{PollError, Source};

/// How often the feed reads.
pub const EVERY: Duration = Duration::from_secs(300);
/// One run's limit.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// An early read (after a rate limit) comes at most this often.
pub const EARLY_AT_MOST: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
pub struct Output {
    #[serde(rename = "schemaVersion")]
    pub schema_version: Option<i64>,
    #[serde(rename = "generatedAt")]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub providers: Vec<Provider>,
}

#[derive(Debug, Deserialize)]
pub struct Provider {
    pub provider: String,
    #[serde(default)]
    pub state: Option<State>,
    #[serde(default)]
    pub windows: Vec<Window>,
}

#[derive(Debug, Deserialize)]
pub struct State {
    pub status: Option<String>,
    #[serde(rename = "refreshedAt")]
    pub refreshed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Window {
    pub id: String,
    pub label: Option<String>,
    pub kind: Option<String>,
    #[serde(rename = "windowSeconds")]
    pub window_seconds: Option<f64>,
    #[serde(rename = "percentRemaining")]
    pub percent_remaining: Option<f64>,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<String>,
    #[serde(default)]
    pub pace: Option<Pace>,
}

#[derive(Debug, Deserialize)]
pub struct Pace {
    pub status: Option<String>,
    #[serde(rename = "reservePercentPoints")]
    pub reserve_percent_points: Option<f64>,
    #[serde(rename = "burnMultiple")]
    pub burn_multiple: Option<f64>,
    #[serde(rename = "elapsedPercent")]
    pub elapsed_percent: Option<f64>,
    #[serde(rename = "projectedExhaustedAt")]
    pub projected_exhausted_at: Option<String>,
}

/// Why a read failed, as the short code `fetch_error` records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    NotFound,
    Timeout,
    Exited,
    Unparsable,
    NoWindows,
}

impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Failure::NotFound => "not_installed",
            Failure::Timeout => "timeout",
            Failure::Exited => "exit_status",
            Failure::Unparsable => "unparsable",
            Failure::NoWindows => "no_windows",
        }
    }
}

fn time(text: Option<&str>) -> Option<Timestamp> {
    text.and_then(|text| text.parse().ok())
}

/// One provider's windows from quota-axi's stdout. Several entries for one
/// provider keep the first and mark it `multiple_accounts`.
pub fn readings(stdout: &[u8], provider: &str) -> Result<Vec<QuotaReading>, Failure> {
    let output: Output = serde_json::from_slice(stdout).map_err(|_| Failure::Unparsable)?;
    let mut entries = output
        .providers
        .into_iter()
        .filter(|p| p.provider == provider);
    let Some(entry) = entries.next() else {
        return Err(Failure::NoWindows);
    };
    let fetch_error = entries.next().map(|_| "multiple_accounts".to_owned());
    if entry.windows.is_empty() {
        return Err(Failure::NoWindows);
    }
    Ok(entry
        .windows
        .into_iter()
        .map(|window| {
            let pace = window.pace.unwrap_or(Pace {
                status: None,
                reserve_percent_points: None,
                burn_multiple: None,
                elapsed_percent: None,
                projected_exhausted_at: None,
            });
            QuotaReading {
                label: format!("{provider}:{}", window.id),
                percent_remaining: window.percent_remaining,
                resets_at: time(window.resets_at.as_deref()),
                pace: pace.status,
                reserve: pace.reserve_percent_points,
                burn_multiple: pace.burn_multiple,
                elapsed_percent: pace.elapsed_percent,
                projected_exhaustion_at: time(pace.projected_exhausted_at.as_deref()),
                title: window.label,
                kind: window.kind,
                fetch_error: fetch_error.clone(),
            }
        })
        .collect())
}

/// Runs `program --provider <p> --full --json` with the timeout and returns
/// its stdout. The child is killed when it runs over.
fn run(program: &[OsString], provider: &str) -> Result<Vec<u8>, Failure> {
    let (first, rest) = program.split_first().ok_or(Failure::NotFound)?;
    let mut child = Command::new(first)
        .args(rest)
        .args(["--provider", provider, "--full", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| Failure::NotFound)?;
    let mut stdout = child.stdout.take().ok_or(Failure::Exited)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return Err(Failure::Exited),
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| Failure::Exited)?
        .map_err(|_| Failure::Exited)?;
    if !status.success() {
        return Err(Failure::Exited);
    }
    Ok(bytes)
}

/// The feed as a poller. It ticks every minute and reads when five minutes
/// have passed, or after a rate limit or an auto resume was observed (at most
/// once a minute). Ticks never overlap, so neither do reads.
pub struct Feed {
    store: Store,
    program: Vec<OsString>,
    providers: Vec<String>,
    last_read: Option<Instant>,
    /// The newest early-read trigger already acted on.
    seen_trigger: Option<String>,
}

impl Feed {
    pub fn new(store: Store, program: Vec<OsString>, providers: Vec<String>) -> Self {
        Self {
            store,
            program,
            providers,
            last_read: None,
            seen_trigger: None,
        }
    }

    /// The newest observation that asks for an early read.
    fn newest_trigger(&self) -> Option<String> {
        self.store
            .conn()
            .query_row(
                "SELECT max(observed_at || ' ' || id) FROM observations
                 WHERE event IN ('StopFailure:rate_limit', 'Notification:quota_auto_resume_fired')",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
    }

    fn due(&mut self) -> bool {
        let trigger = self.newest_trigger();
        let Some(last) = self.last_read else {
            self.seen_trigger = trigger;
            return true;
        };
        let early = trigger.is_some() && trigger != self.seen_trigger;
        if last.elapsed() >= EVERY || (early && last.elapsed() >= EARLY_AT_MOST) {
            self.seen_trigger = trigger;
            return true;
        }
        false
    }

    /// Reads every provider once, one at a time.
    pub fn read_all(&mut self, now: Timestamp) -> Result<(), PollError> {
        let ctx = Ctx::at(Actor::Daemon, now);
        let mut failed = Vec::new();
        for provider in &self.providers {
            let read = run(&self.program, provider).and_then(|stdout| readings(&stdout, provider));
            match read {
                Ok(windows) => {
                    self.store.cache_quota(&ctx, windows)?;
                }
                Err(failure) => {
                    self.store
                        .cache_quota_failed(&ctx, provider, failure.code())?;
                    failed.push(format!("{provider}: {}", failure.code()));
                }
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(PollError::State(tiller_core::Error::Unavailable(format!(
                "quota-axi failed for {}",
                failed.join(", ")
            ))))
        }
    }
}

impl Source for Feed {
    fn name(&self) -> String {
        "quota".to_owned()
    }

    fn interval(&self) -> Duration {
        EARLY_AT_MOST
    }

    fn poll(&mut self, now: Timestamp) -> Result<(), PollError> {
        if !self.due() {
            return Ok(());
        }
        self.last_read = Some(Instant::now());
        self.read_all(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// quota-axi's real shape, with the account fields it carries in
    /// `--full --json` (values invented).
    const FIXTURE: &str = r#"{
      "schemaVersion": 5,
      "generatedAt": "2026-11-02T14:00:00.000Z",
      "providers": [{
        "provider": "claude",
        "label": "Claude",
        "plan": "max",
        "source": "keychain",
        "account": {"email": "owner@example.com", "id": "acct_0123456789", "organization": "Example Org"},
        "accountKeys": ["owner@example.com", "acct_0123456789"],
        "attempts": [{"source": "keychain", "status": "ok", "account": "owner@example.com"}],
        "state": {"status": "fresh", "refreshedAt": "2026-11-02T13:59:58.000Z", "sourcesTried": ["keychain"]},
        "windows": [
          {"id": "seven_day", "kind": "weekly", "label": "week", "windowSeconds": 604800,
           "percentRemaining": 27, "percentUsed": 73, "resetsAt": "2026-11-03T14:00:00.555677+00:00",
           "pace": {"status": "ahead", "reservePercentPoints": -4.5, "burnMultiple": 1.2,
                    "elapsedPercent": 70.1, "projectedExhaustedAt": "2026-11-03T10:00:00Z",
                    "reason": "spent faster than the clock"}},
          {"id": "extra_usage", "kind": "credits", "label": "extra usage", "windowSeconds": null,
           "percentRemaining": 100, "resetsAt": null, "spentUsd": 0, "limitUsd": 50}
        ]
      }]
    }"#;

    #[test]
    fn account_fields_never_deserialized() {
        let parsed: Output = serde_json::from_str(FIXTURE).unwrap();
        let held = format!("{parsed:?}");
        let windows = readings(FIXTURE.as_bytes(), "claude").unwrap();
        let stored = format!("{windows:?}");
        for secret in [
            "owner@example.com",
            "acct_0123456789",
            "Example Org",
            "keychain",
        ] {
            assert!(!held.contains(secret), "parsed output holds {secret}");
            assert!(!stored.contains(secret), "readings hold {secret}");
        }
        assert_eq!(windows.len(), 2);
        let week = &windows[0];
        assert_eq!(week.label, "claude:seven_day");
        assert_eq!(week.title.as_deref(), Some("week"));
        assert_eq!(week.kind.as_deref(), Some("weekly"));
        assert_eq!(week.percent_remaining, Some(27.0));
        assert_eq!(week.pace.as_deref(), Some("ahead"));
        assert_eq!(week.reserve, Some(-4.5));
        assert_eq!(
            week.resets_at.map(|t| t.to_string()).as_deref(),
            Some("2026-11-03T14:00:00.555Z")
        );
        assert!(week.projected_exhaustion_at.is_some());
        assert_eq!(windows[1].pace, None);
    }

    #[test]
    fn several_accounts_for_one_provider_keep_the_first_and_say_so() {
        let mut value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let first = value["providers"][0].clone();
        let mut second = first.clone();
        second["windows"][0]["percentRemaining"] = 90.into();
        value["providers"] = serde_json::json!([first, second]);
        let windows = readings(value.to_string().as_bytes(), "claude").unwrap();
        assert_eq!(windows[0].percent_remaining, Some(27.0));
        assert!(
            windows
                .iter()
                .all(|w| w.fetch_error.as_deref() == Some("multiple_accounts"))
        );
    }

    #[test]
    fn garbage_and_empty_output_are_failures_not_empty_plans() {
        assert_eq!(
            readings(b"not json", "claude").unwrap_err(),
            Failure::Unparsable
        );
        assert_eq!(
            readings(
                br#"{"providers": [{"provider": "claude", "windows": []}]}"#,
                "claude"
            )
            .unwrap_err(),
            Failure::NoWindows
        );
        assert_eq!(
            readings(br#"{"providers": []}"#, "claude").unwrap_err(),
            Failure::NoWindows
        );
    }

    fn script(dir: &std::path::Path, body: &str) -> Vec<OsString> {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("quota-axi");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        vec![path.into()]
    }

    #[test]
    fn a_failed_read_keeps_the_rows_and_their_age() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("out.json");
        std::fs::write(&fixture, FIXTURE).unwrap();
        let good = script(dir.path(), &format!("cat {}", fixture.display()));
        let fleet = tiller_herdr::testing::Fleet::new();
        let mut feed = Feed::new(fleet.open(), good, vec!["claude".into()]);
        let first: Timestamp = "2026-11-02T14:00:00.000Z".parse().unwrap();
        feed.read_all(first).unwrap();

        feed.program = script(dir.path(), "echo boom >&2; exit 3");
        let later = first.plus_minutes(5);
        assert!(feed.read_all(later).is_err());
        let rows: Vec<(String, String, Option<String>, Option<f64>)> = feed
            .store
            .conn()
            .prepare("SELECT label, fetched_at, fetch_error, percent_remaining FROM quota_windows ORDER BY label")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        for (label, fetched_at, error, _) in &rows {
            assert_eq!(fetched_at, &first.to_string(), "{label}");
            assert_eq!(error.as_deref(), Some("exit_status"), "{label}");
        }
        assert_eq!(rows[1].3, Some(27.0));
    }
}
