//! Settings > Backups (`protocol.md` section 7): the recent backups of this
//! host, and "Back up now".
//!
//! "Back up now" runs the code `tiller backup` runs
//! ([`tiller_core::backup::backup`]): a copy through SQLite's online backup
//! API on a fresh connection, verified, listed in `backups/manifest.json`,
//! then retention. The daemon holds no connection of its own that the copy
//! would wait for, so it needs no stop. Only the owner may ask for one, and
//! only one runs at a time. The list reads the same manifest: each copy in it
//! verified when it was taken, and the read checks its file is still there
//! with the manifest's size.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use tiller_core::backup::{self, Entry, EntryKind, Manifest, Nightly, Paths};
use tiller_core::{Error, Timestamp};
use utoipa::ToSchema;

use crate::api::{
    ApiError, ApiResult, AppState, body, guard, key, not_forwarded, owner_only, read,
};
use crate::dto::{ErrorBody, MutationResponse, Receipt};

/// The most rows the list returns.
const LISTED: usize = 30;

/// What the board knows of a backup run it started: whether one is running,
/// and why the last one by hand failed. A failed run leaves nothing in the
/// manifest, so the daemon keeps its reason until the next run verifies.
#[derive(Clone, Default)]
pub struct BackupJobs(Arc<Mutex<Jobs>>);

#[derive(Default)]
struct Jobs {
    running: bool,
    failed: Option<BackupRow>,
}

impl BackupJobs {
    fn lock(&self) -> MutexGuard<'_, Jobs> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the one place to run in, or `None` while a backup runs.
    fn begin(&self) -> Option<Running> {
        let mut jobs = self.lock();
        if jobs.running {
            return None;
        }
        jobs.running = true;
        Some(Running(self.clone()))
    }
}

/// Holds the place to run in; the place frees when this drops, even on a
/// panic.
struct Running(BackupJobs);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.lock().running = false;
    }
}

/// One backup in Settings > Backups.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BackupRow {
    /// The file name for a copy; `failed-<time>` for an attempt that left no
    /// copy.
    pub id: String,
    /// `nightly` (the daemon's timer) or `manual` (Back up now, or
    /// `tiller backup`; both are the owner's).
    pub kind: String,
    /// When it was taken, or tried.
    pub at: String,
    /// `verified`, or `failed`.
    pub status: String,
    /// Bytes; null for an attempt that left no copy.
    pub size: Option<u64>,
    /// Why it failed, or why a listed copy no longer holds: its file is
    /// gone or its size changed. Null for a verified copy.
    pub error: Option<String>,
}

/// `GET /api/v1/backups`: the recent backups, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BackupList {
    /// The folder the copies are kept in.
    pub folder: String,
    /// True while a backup runs.
    pub running: bool,
    pub backups: Vec<BackupRow>,
}

/// `POST /api/v1/backups`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TakeBackup {
    pub idempotency_key: String,
    /// Always refused with a peer label: a backup copies this host's own
    /// database and never leaves its folder.
    #[serde(default)]
    pub host: Option<String>,
}

fn kind_of(kind: EntryKind) -> Option<&'static str> {
    match kind {
        EntryKind::Nightly => Some("nightly"),
        EntryKind::Manual => Some("manual"),
        // A copy taken before a migration is not a backup the owner took.
        EntryKind::PreMigration => None,
    }
}

fn row_of(paths: &Paths, entry: &Entry) -> Option<BackupRow> {
    let kind = kind_of(entry.kind)?;
    let problem = match std::fs::metadata(paths.home.join(&entry.file)) {
        Ok(meta) if meta.len() == entry.size => None,
        Ok(meta) => Some(format!(
            "the file is {} bytes, the manifest says {}",
            meta.len(),
            entry.size
        )),
        Err(error) => Some(format!("the file cannot be read: {error}")),
    };
    Some(BackupRow {
        id: entry
            .file
            .rsplit('/')
            .next()
            .unwrap_or(&entry.file)
            .to_owned(),
        kind: kind.to_owned(),
        at: entry.at.to_string(),
        status: if problem.is_none() {
            "verified"
        } else {
            "failed"
        }
        .to_owned(),
        size: Some(entry.size),
        error: problem,
    })
}

fn failed_row(kind: &str, at: Timestamp, error: String) -> BackupRow {
    BackupRow {
        id: format!("failed-{}", at.as_millis()),
        kind: kind.to_owned(),
        at: at.to_string(),
        status: "failed".to_owned(),
        size: None,
        error: Some(error),
    }
}

/// The rows of a manifest and the daemon's own failed run, newest first.
fn rows(paths: &Paths, manifest: &Manifest, failed: Option<BackupRow>) -> Vec<BackupRow> {
    let mut rows: Vec<BackupRow> = manifest
        .live()
        .into_iter()
        .filter_map(|entry| row_of(paths, entry))
        .collect();
    rows.extend(
        manifest
            .failures
            .iter()
            .map(|failure| failed_row("nightly", failure.at, failure.error.clone())),
    );
    rows.extend(failed);
    // Timestamps are fixed-width RFC 3339 in UTC, so text order is time order.
    rows.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
    rows.truncate(LISTED);
    rows
}

#[utoipa::path(get, path = "/api/v1/backups", tag = "reads",
    responses((status = 200, body = BackupList), (status = 403, body = ErrorBody),
              (status = 500, body = ErrorBody, description = "`internal`: the manifest does not parse")))]
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<BackupList> {
    owner_only(&state, &headers).await?;
    let paths = Paths::in_home(&state.0.home);
    let jobs = state.0.backups.clone();
    let listed = tokio::task::spawn_blocking(move || {
        let manifest = Manifest::load(&paths)?;
        let (running, failed) = {
            let jobs = jobs.lock();
            (jobs.running, jobs.failed.clone())
        };
        Ok::<_, Error>(BackupList {
            folder: paths.backups.display().to_string(),
            running,
            backups: rows(&paths, &manifest, failed),
        })
    })
    .await
    .map_err(|error| Error::Internal(error.to_string()))??;
    Ok(Json(listed))
}

#[utoipa::path(post, path = "/api/v1/backups", tag = "mutations", request_body = TakeBackup,
    responses((status = 200, body = MutationResponse, description = "`data` is the `BackupRow` the run left: `verified`, or `failed` with the reason. A failed run keeps the previous verified backup"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "`conflict`: a backup is running"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "`unavailable`: before `tiller init`")))]
pub(crate) async fn take(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: TakeBackup = body(&bytes)?;
    key(&request.idempotency_key)?;
    not_forwarded(&state, request.host.as_deref(), "a backup").await?;
    let Some(_running) = state.0.backups.begin() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "a backup is already running",
        ));
    };
    let paths = Paths::in_home(&state.0.home);
    let now = Timestamp::now();
    // A refusal to start (no database yet) answers as an error; a copy that
    // does not verify is the run's outcome.
    let outcome = read(&state, move |store| {
        Ok(backup::backup(store.conn(), &paths, now))
    })
    .await;
    let jobs = state.0.backups.clone();
    let outcome = outcome?;
    let row = match outcome {
        Ok(Nightly::Done { entry, .. }) => {
            let paths = Paths::in_home(&state.0.home);
            let row = row_of(&paths, &entry).unwrap_or_else(|| {
                failed_row("manual", now, "the copy is not listed as a backup".into())
            });
            jobs.lock().failed = None;
            row
        }
        Ok(other) => {
            // `backup` returns `Done` or an error.
            let row = failed_row("manual", now, format!("unexpected outcome: {other:?}"));
            jobs.lock().failed = Some(row.clone());
            row
        }
        Err(error) => {
            let row = failed_row("manual", now, error.to_string());
            jobs.lock().failed = Some(row.clone());
            row
        }
    };
    let data = serde_json::to_value(&row).map_err(Error::from)?;
    Ok(Json(MutationResponse {
        ok: true,
        replayed: false,
        events: Vec::new(),
        receipt: Receipt {
            recorded_at: Timestamp::now().to_string(),
            outbox_id: None,
            approval_id: None,
        },
        data,
        host: None,
        warning: None,
    }))
}
