//! Settings > Backups (`protocol.md` section 7): the recent backups of this
//! host, and "Back up now".
//!
//! "Back up now" runs the code `tiller backup` runs
//! ([`tiller_core::backup::backup`]): a copy through SQLite's online backup
//! API on a fresh connection, verified, listed in `backups/manifest.json`,
//! then retention. The daemon holds no connection of its own that the copy
//! would wait for, so it needs no stop. Only the owner may ask for one, and
//! only one runs at a time. The run owns its place and records its own
//! outcome, so a request dropped mid-run (the tab closed) changes neither.
//! The list reads the same manifest: each copy in it verified when it was
//! taken, and the read checks its file is still there with the manifest's
//! size.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use tiller_core::backup::{self, Entry, EntryKind, Manifest, Paths};
use tiller_core::{Error, Store, Timestamp};
use utoipa::ToSchema;

use crate::api::{
    ApiError, ApiResult, AppState, body, guard, key, not_forwarded, owner_only, uninitialized,
    writable,
};
use crate::dto::{ErrorBody, MutationResponse, Receipt};
use crate::readonly::Damage;

/// The most rows the list returns: as many as Settings > Backups shows.
const LISTED: usize = 6;

/// What the board knows of a backup run it started: the key of the one
/// running, why the last one by hand failed, and the last run's key and row,
/// which a retry with that key gets again. A failed run leaves nothing in the
/// manifest, so the daemon keeps its reason until a newer backup verifies.
#[derive(Clone, Default)]
pub struct BackupJobs(Arc<Mutex<Jobs>>);

#[derive(Default)]
struct Jobs {
    running: Option<String>,
    failed: Option<BackupRow>,
    last: Option<(String, BackupRow)>,
}

impl BackupJobs {
    fn lock(&self) -> MutexGuard<'_, Jobs> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the one place to run in for a run under `key`, or `None` while
    /// a backup runs.
    fn begin(&self, key: &str) -> Option<Running> {
        let mut jobs = self.lock();
        if jobs.running.is_some() {
            return None;
        }
        jobs.running = Some(key.to_owned());
        Some(Running(self.clone()))
    }

    /// The row the run under `key` left. A retry that arrives while that
    /// run is still going waits for it. `None` when no run under `key` is
    /// known, or the one that ran left no row.
    async fn replay(&self, key: &str) -> Option<BackupRow> {
        loop {
            {
                let jobs = self.lock();
                if let Some((last, row)) = &jobs.last
                    && last == key
                {
                    return Some(row.clone());
                }
                if jobs.running.as_deref() != Some(key) {
                    return None;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}

/// Holds the place to run in; the place frees when this drops, even on a
/// panic.
struct Running(BackupJobs);

impl Running {
    /// One Back up now, on the blocking pool: the copy, then its outcome
    /// recorded, then the place freed. `Err` is a refusal to start (the
    /// database cannot be opened); a copy that does not verify is the
    /// run's outcome. A failed copy can be the first sign of a damaged
    /// live database, so the live one is checked then (`recovery.md` R10).
    fn run(
        self,
        db: &Path,
        paths: &Paths,
        damage: &Damage,
        key: String,
    ) -> Result<BackupRow, Error> {
        let now = Timestamp::now();
        let store = Store::open_current(db).inspect_err(|error| damage.suspect(db, error))?;
        let row = match backup::backup(store.conn(), paths, now) {
            Ok(taken) => entry_row(&taken.entry, "manual", None),
            Err(error) => {
                drop(store);
                damage.check(db);
                failed_row("manual", now, error.to_string())
            }
        };
        let mut jobs = self.0.lock();
        jobs.failed = (row.status == "failed").then(|| row.clone());
        jobs.last = Some((key, row.clone()));
        Ok(row)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.lock().running = None;
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

fn entry_row(entry: &Entry, kind: &str, problem: Option<String>) -> BackupRow {
    BackupRow {
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
    }
}

/// A manifest entry as a row: `failed` when its file is gone or its size
/// changed.
fn row_of(paths: &Paths, entry: &Entry, kind: &str) -> BackupRow {
    let problem = match std::fs::metadata(paths.home.join(&entry.file)) {
        Ok(meta) if meta.len() == entry.size => None,
        Ok(meta) => Some(format!(
            "the file is {} bytes, the manifest says {}",
            meta.len(),
            entry.size
        )),
        Err(error) => Some(format!("the file cannot be read: {error}")),
    };
    entry_row(entry, kind, problem)
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

/// The rows of a manifest and the failed attempts, newest first. A failed
/// attempt (the daemon's own run, or a night in the manifest) shows only
/// until a newer backup verifies, whoever took it.
fn rows(paths: &Paths, manifest: &Manifest, failed: Option<BackupRow>) -> Vec<BackupRow> {
    let listed: Vec<(&Entry, &'static str)> = manifest
        .live()
        .into_iter()
        .filter_map(|entry| kind_of(entry.kind).map(|kind| (entry, kind)))
        .collect();
    let newest = listed.first().map(|(entry, _)| entry.at);
    let current = |row: &BackupRow| newest.is_none_or(|newest| Some(newest) < at(row));
    let mut rows: Vec<BackupRow> = listed
        .into_iter()
        .take(LISTED)
        .map(|(entry, kind)| row_of(paths, entry, kind))
        .collect();
    rows.extend(
        manifest
            .failures
            .iter()
            .map(|failure| failed_row("nightly", failure.at, failure.error.clone()))
            .filter(current),
    );
    rows.extend(failed.filter(current));
    rows.sort_by(|a, b| at(b).cmp(&at(a)).then_with(|| b.id.cmp(&a.id)));
    rows.truncate(LISTED);
    rows
}

/// When a row was taken or tried. The text is not fixed-width (the fraction
/// of a second varies), so rows order by the parsed time.
fn at(row: &BackupRow) -> Option<Timestamp> {
    row.at.parse().ok()
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
            (jobs.running.is_some(), jobs.failed.clone())
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
    responses((status = 200, body = MutationResponse, description = "`data` is the `BackupRow` the run left: `verified`, or `failed` with the reason. A failed run keeps the previous verified backup. A retry with the last run's key gets its row again, `replayed`, and waits for it while that run is going"),
              (status = 400, body = ErrorBody), (status = 403, body = ErrorBody),
              (status = 409, body = ErrorBody, description = "`conflict`: a backup is running"),
              (status = 502, body = ErrorBody, description = "`peer_refused`: not forwarded"),
              (status = 503, body = ErrorBody, description = "`unavailable`: before `tiller init`, or read only")))]
pub(crate) async fn take(
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> ApiResult<MutationResponse> {
    guard(&state, &headers).await?;
    let request: TakeBackup = body(&bytes)?;
    key(&request.idempotency_key)?;
    not_forwarded(&state, request.host.as_deref(), "a backup").await?;
    writable(&state)?;
    if let Some(row) = state.0.backups.replay(&request.idempotency_key).await {
        return answer(&row, true);
    }
    let db = state.db();
    if !db.exists() {
        return Err(uninitialized());
    }
    let Some(running) = state.0.backups.begin(&request.idempotency_key) else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            "a backup is already running",
        ));
    };
    let paths = Paths::in_home(&state.0.home);
    let damage = state.0.damage.clone();
    let key = request.idempotency_key;
    // The blocking task runs to its end even when this request is dropped.
    let row = tokio::task::spawn_blocking(move || running.run(&db, &paths, &damage, key))
        .await
        .map_err(|error| Error::Internal(error.to_string()))??;
    answer(&row, false)
}

fn answer(row: &BackupRow, replayed: bool) -> ApiResult<MutationResponse> {
    let data = serde_json::to_value(row).map_err(Error::from)?;
    Ok(Json(MutationResponse {
        ok: true,
        replayed,
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

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::{Duration, Instant};

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use serde_json::{Value, json};
    use tiller_panes::testing::Fleet;

    use crate::api::tests::{Fixture, HOST, app_with, mutation, owner_cookie, send};
    use crate::assets::Assets;
    use crate::readonly::Damage;

    use super::*;

    async fn listed(app: &Router, cookie: &str) -> Value {
        let request = Request::get("/api/v1/backups")
            .header(header::HOST, HOST)
            .header(header::COOKIE, cookie)
            .body(Body::empty())
            .unwrap();
        let answer = send(app, request).await;
        assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
        serde_json::from_str(&answer.body).unwrap()
    }

    fn back_up_now(key: &str, cookie: &str) -> Request<Body> {
        mutation(
            "POST",
            "/api/v1/backups",
            json!({"idempotency_key": key}),
            &[("cookie", cookie)],
        )
    }

    /// Polls the list until `running` is `want`.
    async fn until_running(app: &Router, cookie: &str, want: bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let list = listed(app, cookie).await;
            if list["running"] == want {
                return list;
            }
            assert!(Instant::now() < deadline, "running never became {want}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Holds `backups/manifest.lock`, so a copy waits until it drops.
    fn hold_manifest_lock(home: &std::path::Path) -> File {
        std::fs::create_dir_all(home.join("backups")).unwrap();
        let file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(home.join("backups/manifest.lock"))
            .unwrap();
        file.lock().unwrap();
        file
    }

    /// The board tab closes while Back up now runs: the run goes on, still
    /// holds the one place to run in, and records its own outcome.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dropped_request_keeps_the_run_and_its_outcome() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app_with(home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(home);
        let held = hold_manifest_lock(home);

        let post = tokio::spawn({
            let (app, request) = (app.clone(), back_up_now("k-1", &cookie));
            async move { send(&app, request).await }
        });
        until_running(&app, &cookie, true).await;
        post.abort();
        assert!(post.await.is_err_and(|error| error.is_cancelled()));

        let list = listed(&app, &cookie).await;
        assert_eq!(list["running"], true, "the copy still runs: {list}");
        let second = send(&app, back_up_now("k-2", &cookie)).await;
        assert_eq!(second.status, StatusCode::CONFLICT, "{}", second.body);
        // The board retries the dropped request with its key: it waits for
        // the run and gets its row, not a conflict.
        let retry = tokio::spawn({
            let (app, request) = (app.clone(), back_up_now("k-1", &cookie));
            async move { send(&app, request).await }
        });

        held.unlock().unwrap();
        drop(held);
        let retry = retry.await.unwrap();
        assert_eq!(retry.status, StatusCode::OK, "{}", retry.body);
        let retry: Value = serde_json::from_str(&retry.body).unwrap();
        assert_eq!(retry["replayed"], true, "{retry}");
        assert_eq!(retry["data"]["status"], "verified", "{retry}");
        let list = until_running(&app, &cookie, false).await;
        let rows = list["backups"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{list}");
        assert_eq!(rows[0]["kind"], "manual");
        assert_eq!(rows[0]["status"], "verified");
    }

    /// A failed Back up now shows until any newer backup verifies, also
    /// one `tiller backup` took in a terminal.
    #[test]
    fn a_failed_run_goes_once_a_newer_backup_verifies() {
        let fleet = Fleet::new();
        let paths = Paths::in_home(fleet.dir.path());
        let failed_at = Timestamp::now();
        let failed = failed_row("manual", failed_at, "disk full".into());
        let shown = rows(
            &paths,
            &Manifest::load(&paths).unwrap(),
            Some(failed.clone()),
        );
        assert_eq!(shown, vec![failed.clone()]);

        // `tiller backup` in a terminal, a minute later.
        backup::backup(fleet.store.conn(), &paths, failed_at.plus_minutes(1)).unwrap();
        let shown = rows(&paths, &Manifest::load(&paths).unwrap(), Some(failed));
        assert_eq!(shown.len(), 1, "the failed row is gone: {shown:?}");
        assert_eq!(shown[0].status, "verified");
    }

    /// A failed night in the manifest shows until a newer backup verifies,
    /// also one taken by hand.
    #[test]
    fn a_failed_night_goes_once_a_newer_backup_verifies() {
        let fleet = Fleet::new();
        let paths = Paths::in_home(fleet.dir.path());
        let failed_at = "2026-11-03T03:00:00Z".parse::<Timestamp>().unwrap();
        let with_failure = |mut manifest: Manifest| {
            manifest.failures.push(backup::Failure {
                at: failed_at,
                error: "disk full".into(),
                retry: false,
            });
            manifest
        };
        let shown = rows(&paths, &with_failure(Manifest::load(&paths).unwrap()), None);
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert_eq!(
            (shown[0].kind.as_str(), shown[0].status.as_str()),
            ("nightly", "failed")
        );

        backup::backup(fleet.store.conn(), &paths, failed_at.plus_minutes(5)).unwrap();
        let shown = rows(&paths, &with_failure(Manifest::load(&paths).unwrap()), None);
        assert_eq!(shown.len(), 1, "the failed night is gone: {shown:?}");
        assert_eq!(shown[0].status, "verified");
    }

    /// A retry of Back up now with the last run's key (the response was
    /// lost) gets that run's row again and takes no second copy.
    #[tokio::test]
    async fn a_retry_with_the_same_key_replays_the_run() {
        let fleet = Fleet::new();
        let home = fleet.dir.path();
        let app = app_with(home, Assets::of::<Fixture>(), Damage::default());
        let cookie = owner_cookie(home);

        let first = send(&app, back_up_now("k-1", &cookie)).await;
        assert_eq!(first.status, StatusCode::OK, "{}", first.body);
        let first: Value = serde_json::from_str(&first.body).unwrap();
        assert_eq!(first["replayed"], false);
        let again = send(&app, back_up_now("k-1", &cookie)).await;
        assert_eq!(again.status, StatusCode::OK, "{}", again.body);
        let again: Value = serde_json::from_str(&again.body).unwrap();
        assert_eq!(again["replayed"], true);
        assert_eq!(again["data"], first["data"]);
        let list = listed(&app, &cookie).await;
        assert_eq!(list["backups"].as_array().unwrap().len(), 1, "{list}");
    }

    /// The list stops at the rows Settings > Backups shows, newest first.
    #[test]
    fn the_list_stops_at_what_the_board_shows() {
        let fleet = Fleet::new();
        let paths = Paths::in_home(fleet.dir.path());
        let first = "2026-11-01T03:00:00Z".parse::<Timestamp>().unwrap();
        for day in 0..8 {
            let at = first.plus_minutes(day * 24 * 60);
            backup::backup(fleet.store.conn(), &paths, at).unwrap();
        }
        let shown = rows(&paths, &Manifest::load(&paths).unwrap(), None);
        assert_eq!(shown.len(), LISTED);
        assert_eq!(shown[0].at, first.plus_minutes(7 * 24 * 60).to_string());
    }
}
