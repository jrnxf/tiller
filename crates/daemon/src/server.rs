//! `tiller daemon`: the single-instance lock, the workers, the change feed
//! and the board API on `127.0.0.1` (`protocol.md` 6.4 and 7.1).
//!
//! A second instance finds the lock held and exits 0 naming the running
//! pid. Without `state.db` the daemon still serves the board and its health
//! endpoint, answers every other API call with 503, and starts its workers
//! once `tiller init` has made the database.

use std::ffi::OsString;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tiller_agents::land::{DaemonLock, LandError};
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_herdr::client::Cli;
use tiller_herdr::poller::Registry;
use tokio::sync::watch;

use crate::api::{self, AppState, Inner};
use crate::assets::Assets;
use crate::workers::{self, Tools};

/// The board's port (`protocol.md` 7.1).
pub const PORT: u16 = 7421;
/// How long a stop waits for open requests before it exits anyway.
const DRAIN: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("create {path}: {source}")]
    Home {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("take the daemon lock: {0}")]
    Lock(LandError),
    #[error("start the async runtime: {0}")]
    Runtime(std::io::Error),
    #[error("bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("serve: {0}")]
    Serve(std::io::Error),
    #[error(transparent)]
    State(#[from] tiller_core::Error),
}

/// Everything `run` needs.
pub struct Config {
    pub home: PathBuf,
    /// 0 picks a free port.
    pub port: u16,
    pub herdr: Cli,
    pub quota: Vec<OsString>,
    pub assets: Assets,
}

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// Stopped by SIGTERM or SIGINT.
    Stopped,
    /// Another daemon holds the lock.
    AlreadyRunning { pid: Option<u32> },
    /// `state.db` is newer than this binary. The CLI exits 3, so the service
    /// manager restarts the daemon from the profile's new binary
    /// (`recovery.md` R11).
    NewerSchema { found: usize },
    /// Starting the workers panicked. The CLI exits 1, so the service
    /// manager restarts the daemon (and gives up after its start limit).
    WorkersFailed,
}

/// Why a daemon stopped itself.
#[derive(Clone, Copy, Debug)]
enum Fatal {
    NewerSchema(usize),
    WorkersFailed,
}

type FatalSlot = Arc<Mutex<Option<Fatal>>>;

fn fail(slot: &FatalSlot, stopping: &watch::Sender<bool>, fatal: Fatal) {
    *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(fatal);
    let _ = stopping.send(true);
}

/// `<home>/daemon.json`: the running daemon's pid and port, for `tiller
/// board --login` and for a second instance to name.
fn info_path(home: &Path) -> PathBuf {
    home.join("daemon.json")
}

/// The running daemon's `(pid, port)`, as it recorded them.
pub fn running(home: &Path) -> Option<(u32, u16)> {
    let text = std::fs::read_to_string(info_path(home)).ok()?;
    let info: Value = serde_json::from_str(&text).ok()?;
    let pid = u32::try_from(info.get("pid")?.as_u64()?).ok()?;
    let port = u16::try_from(info.get("port")?.as_u64()?).ok()?;
    Some((pid, port))
}

fn private_dir(path: &Path) -> Result<(), DaemonError> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|source| DaemonError::Home {
            path: path.to_owned(),
            source,
        })
}

pub fn run(config: Config) -> Result<Ended, DaemonError> {
    private_dir(&config.home)?;
    let lock = match DaemonLock::acquire(&config.home) {
        Ok(lock) => lock,
        Err(LandError::Held(_)) => {
            return Ok(Ended::AlreadyRunning {
                pid: running(&config.home).map(|(pid, _)| pid),
            });
        }
        Err(error) => return Err(DaemonError::Lock(error)),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(DaemonError::Runtime)?;
    runtime.block_on(serve(config, Arc::new(lock)))
}

async fn serve(config: Config, lock: Arc<DaemonLock>) -> Result<Ended, DaemonError> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| DaemonError::Bind { addr, source })?;
    let bound = listener.local_addr().map_err(DaemonError::Serve)?;
    let started_at = Timestamp::now();
    let info = json!({"pid": std::process::id(), "port": bound.port(), "started_at": started_at.to_string()});
    std::fs::write(info_path(&config.home), info.to_string()).map_err(|source| {
        DaemonError::Home {
            path: info_path(&config.home),
            source,
        }
    })?;

    let (stop_tx, stop) = watch::channel(false);
    let (newest_tx, newest) = watch::channel(0_i64);
    let db = config.home.join("state.db");
    // Set when `state.db` turns out newer than this binary, at start or
    // later; the daemon then stops without another write.
    let fatal: FatalSlot = Arc::default();
    {
        let (db, stop, stopping, fatal) = (
            db.clone(),
            stop.clone(),
            stop_tx.clone(),
            Arc::clone(&fatal),
        );
        std::thread::spawn(move || {
            crate::sse::watch_changes(db, newest_tx, stop, move |schema| {
                tracing::error!(schema, "state.db was migrated past this binary; stopping");
                fail(&fatal, &stopping, Fatal::NewerSchema(schema));
            });
        });
    }

    let registry = Registry::new();
    let tools = Tools {
        herdr: config.herdr.clone(),
        quota: config.quota.clone(),
    };
    let workers = tokio::spawn(start_workers(
        config.home.clone(),
        lock,
        tools.clone(),
        registry.clone(),
        stop_tx.clone(),
        Arc::clone(&fatal),
    ));

    let tool_versions = Arc::new(Mutex::new(Value::Null));
    {
        let (slot, tools) = (Arc::clone(&tool_versions), tools.clone());
        std::thread::spawn(move || {
            let versions = read_versions(&tools);
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = versions;
        });
    }

    let state = AppState::new(Inner {
        home: config.home.clone(),
        port: bound.port(),
        assets: config.assets,
        newest,
        stop: stop.clone(),
        pollers: registry,
        herdr: config.herdr,
        started_at,
        tools: tool_versions,
    });

    // The address line is the readiness signal for supervisors and the smoke
    // script. A closed stdout must not take the daemon down.
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "listening on http://{bound}").and_then(|()| stdout.flush());
    tracing::info!(%bound, pid = std::process::id(), "daemon started");

    let stopping = stop_tx.clone();
    let mut stopped_early = stop.clone();
    let shutdown = async move {
        tokio::select! {
            () = signal() => {}
            // A newer schema or a failed start stops the daemon from inside.
            _ = stopped_early.wait_for(|stop| *stop) => {}
        }
        // Streams and pollers end on this; then the server drains.
        let _ = stopping.send(true);
    };
    let server = axum::serve(listener, api::router(state)).with_graceful_shutdown(shutdown);
    let mut stopped = stop.clone();
    tokio::select! {
        served = server => served.map_err(DaemonError::Serve)?,
        () = async {
            let _ = stopped.wait_for(|stop| *stop).await;
            tokio::time::sleep(DRAIN).await;
        } => tracing::warn!("requests still open after the drain; exiting"),
    }
    let _ = stop_tx.send(true);
    if let Ok(Ok(handles)) = tokio::time::timeout(DRAIN, workers).await {
        for handle in handles {
            let _ = tokio::time::timeout(DRAIN, handle).await;
        }
    }
    // An older binary never writes to a newer database, not even its stop
    // event.
    let fatal = *fatal.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(Fatal::NewerSchema(found)) = fatal {
        let _ = std::fs::remove_file(info_path(&config.home));
        return Ok(Ended::NewerSchema { found });
    }
    if db.exists() {
        let home = config.home.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let mut store = Store::open_current(home.join("state.db"))?;
            store.daemon_stop(&Ctx::new(Actor::Daemon), "signal")
        })
        .await;
    }
    let _ = std::fs::remove_file(info_path(&config.home));
    Ok(match fatal {
        Some(Fatal::WorkersFailed) => Ended::WorkersFailed,
        _ => Ended::Stopped,
    })
}

/// Starts the workers once `state.db` is initialized, waiting for `tiller
/// init` if needed. The database is migrated first (`recovery.md` R11). A
/// database newer than this binary, or a panic, stops the daemon through
/// `fatal`.
async fn start_workers(
    home: PathBuf,
    lock: Arc<DaemonLock>,
    tools: Tools,
    registry: Registry,
    stopping: watch::Sender<bool>,
    fatal: FatalSlot,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut stop = stopping.subscribe();
    loop {
        if *stop.borrow() {
            return Vec::new();
        }
        let (home_, lock_, tools_, registry_, stop_) = (
            home.clone(),
            Arc::clone(&lock),
            tools.clone(),
            registry.clone(),
            stop.clone(),
        );
        // Blocking-pool threads keep the runtime context, so the workers
        // spawn from here.
        let started = tokio::task::spawn_blocking(move || {
            let db = home_.join("state.db");
            if !db.exists() {
                return Ok(None);
            }
            Store::open(&db)?;
            workers::start(&home_, lock_, &tools_, &registry_, &stop_).map(Some)
        })
        .await;
        match started {
            Ok(Ok(Some(handles))) => return handles,
            Ok(Ok(None)) => {}
            // Not initialized yet (no local host): wait for `tiller init`.
            Ok(Err(tiller_core::Error::Unavailable(_))) => {}
            Ok(Err(error @ tiller_core::Error::SchemaTooNew { found, .. })) => {
                tracing::error!(%error, "stopping for the newer binary");
                fail(&fatal, &stopping, Fatal::NewerSchema(found));
                return Vec::new();
            }
            Ok(Err(error)) => tracing::warn!(%error, "workers did not start; retrying"),
            Err(error) => {
                tracing::error!(%error, "worker start panicked; stopping");
                fail(&fatal, &stopping, Fatal::WorkersFailed);
                return Vec::new();
            }
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = stop.changed() => {}
        }
    }
}

/// The versions of the tools the daemon drives (P43), each `null` when not
/// found. Read once, off the request path.
fn read_versions(tools: &Tools) -> Value {
    let version = |program: &[OsString], arg: &str| -> Value {
        let Some((first, rest)) = program.split_first() else {
            return Value::Null;
        };
        let output = std::process::Command::new(first)
            .args(rest)
            .arg(arg)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                text.lines()
                    .next()
                    .map_or(Value::Null, |line| Value::String(line.trim().to_owned()))
            }
            _ => Value::Null,
        }
    };
    json!({
        "tiller": env!("CARGO_PKG_VERSION"),
        "quota-axi": version(&tools.quota, "--version"),
        "git": version(&[OsString::from("git")], "--version"),
    })
}

/// Resolves on SIGINT or SIGTERM, the signals launchd and systemd send on
/// stop.
async fn signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        // Without signal handlers, run until killed.
        std::future::pending::<()>().await;
        return;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}
