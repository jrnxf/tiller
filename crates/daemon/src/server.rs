//! `tiller daemon`: the single-instance lock, the workers, the change feed
//! and the board API on `127.0.0.1` (`protocol.md` 6.4 and 7.1).
//!
//! A second instance finds the lock held and exits 0 naming the running
//! pid. Without `state.db` the daemon makes one itself under the lock, as
//! `tiller init` would with the host's name and two lanes, so a fresh machine
//! needs no manual step. It never touches a database that exists, and it
//! leaves a home whose owner marker names another system alone: that daemon
//! still serves the board, answers every other API call with 503, and starts
//! its workers once `tiller init` has made the database.

use std::ffi::OsString;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tiller_agents::land::{DaemonLock, LandError};
use tiller_core::domain::Boundary;
use tiller_core::state::projects::Init;
use tiller_core::{Actor, Ctx, Store, Timestamp};
use tiller_herdr::client::Cli;
use tiller_herdr::front::Terminal;
use tiller_herdr::poller::Registry;
use tokio::sync::watch;

use crate::api::{self, AppState, Inner};
use crate::assets::Assets;
use crate::peer::Timing;
use crate::peer::link::{Peers, SshConnector};
use crate::readonly::Damage;
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
    #[error("an allowed host is a host name such as tiller.example.com, not {0:?}")]
    AllowedHost(String),
}

/// Everything `run` needs.
pub struct Config {
    pub home: PathBuf,
    /// 0 picks a free port.
    pub port: u16,
    /// Extra `Host` values the board answers on (`Inner::allowed_hosts`).
    pub allowed_hosts: Vec<String>,
    pub herdr: Cli,
    /// The terminal app "Open in herdr" brings to the front
    /// (`[herdr] terminal`); none means a focus only.
    pub terminal: Option<Arc<dyn Terminal>>,
    pub quota: Vec<OsString>,
    /// `lavish-axi`: the review page tool whose annotations reach workers.
    pub lavish: Vec<OsString>,
    /// The hostnames the review server answers to (`spawn.review_allowed_hosts`).
    pub review_hosts: Vec<String>,
    /// The GitHub CLI the land loop and the direct-pr publish run as the
    /// owner's login: `gh`, or `TILLER_GH`.
    pub gh: String,
    /// The SSH client peer links and Add a host run (`protocol.md` 10.1,
    /// 10.2): `ssh`, or
    /// `TILLER_SSH` split on whitespace.
    pub ssh: Vec<OsString>,
    pub assets: Assets,
    /// The board's Start orchestrator (`tiller orchestrator start`).
    pub orchestrators: Option<Arc<dyn crate::api::OrchestratorStart>>,
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

/// `<home>/daemon.json`: the running daemon's pid, port and allowed hosts,
/// for `tiller board --login` and for a second instance to name.
fn info_path(home: &Path) -> PathBuf {
    home.join("daemon.json")
}

/// The running daemon as it recorded itself in `daemon.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Running {
    pub pid: u32,
    pub port: u16,
    /// Its `--allow-host` names, lowercase.
    pub allowed_hosts: Vec<String>,
}

/// The running daemon, as it recorded itself.
pub fn running(home: &Path) -> Option<Running> {
    let text = std::fs::read_to_string(info_path(home)).ok()?;
    let info: Value = serde_json::from_str(&text).ok()?;
    let pid = u32::try_from(info.get("pid")?.as_u64()?).ok()?;
    let port = u16::try_from(info.get("port")?.as_u64()?).ok()?;
    // A daemon from before allowed hosts were recorded has none here.
    let allowed_hosts = info
        .get("allowed_hosts")
        .and_then(Value::as_array)
        .map(|hosts| {
            hosts
                .iter()
                .filter_map(|host| host.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(Running {
        pid,
        port,
        allowed_hosts,
    })
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

pub fn run(mut config: Config) -> Result<Ended, DaemonError> {
    config.allowed_hosts = config
        .allowed_hosts
        .iter()
        .map(|host| allowed_host(host))
        .collect::<Result<_, _>>()?;
    private_dir(&config.home)?;
    let lock = match DaemonLock::acquire(&config.home) {
        Ok(lock) => lock,
        Err(LandError::Held(_)) => {
            return Ok(Ended::AlreadyRunning {
                pid: running(&config.home).map(|running| running.pid),
            });
        }
        Err(error) => return Err(DaemonError::Lock(error)),
    };
    init_if_missing(&config.home)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(DaemonError::Runtime)?;
    runtime.block_on(serve(config, Arc::new(lock)))
}

/// An allowed host as the board matches it: the `Host` header a browser
/// sends for that https origin, lowercase. It is a name, an IPv4 address or
/// a bracketed IPv6 address, with an optional port; `:443` is dropped,
/// since a browser leaves the https default port out of `Host` and
/// `Origin`. No scheme, path or wildcard: matching stays exact.
pub fn allowed_host(raw: &str) -> Result<String, DaemonError> {
    let refused = || DaemonError::AllowedHost(raw.to_owned());
    let host = raw.to_ascii_lowercase();
    let (name, port) = match host.strip_prefix('[') {
        Some(rest) => {
            let (address, port) = rest.split_once(']').ok_or_else(refused)?;
            let address_ok = !address.is_empty()
                && address
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'));
            if !address_ok {
                return Err(refused());
            }
            (&host[..address.len() + 2], port)
        }
        None => {
            let end = host.find(':').unwrap_or(host.len());
            let name = &host[..end];
            let name_ok = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
            if !name_ok {
                return Err(refused());
            }
            (name, &host[end..])
        }
    };
    if port.is_empty() {
        return Ok(name.to_owned());
    }
    let digits = port.strip_prefix(':').ok_or_else(refused)?;
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err(refused());
    }
    match digits.parse::<u16>() {
        Ok(443) => Ok(name.to_owned()),
        Ok(number) if number > 0 => Ok(format!("{name}:{number}")),
        _ => Err(refused()),
    }
}

/// Lanes a self-initialized host gets; `tiller host edit` changes it.
const DEFAULT_LANES: i64 = 2;

/// This machine's name, for the host label of a self-initialized database.
fn host_label() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .chain(
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok()),
        )
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "local".to_owned())
}

/// Makes `state.db` on a fresh home. Only the lock holder calls it, so two
/// daemons never race, and an existing database is never opened here.
fn init_if_missing(home: &Path) -> Result<(), DaemonError> {
    let db = home.join("state.db");
    if db.exists() {
        return Ok(());
    }
    // A marker naming another system makes every mutation refuse (R20).
    if let Err(error) = tiller_core::owner::check(home) {
        tracing::warn!(%error, "not creating state.db");
        return Ok(());
    }
    let mut store = Store::open(&db)?;
    let init = Init {
        host_label: host_label(),
        lane_capacity: DEFAULT_LANES,
        ram_floor_mb: None,
        disk_floor_mb: None,
        boundary: Boundary::Personal,
    };
    let host = init.host_label.clone();
    store.init(&Ctx::new(Actor::Owner), init)?;
    tracing::info!(host, "created state.db on first start");
    Ok(())
}

async fn serve(config: Config, lock: Arc<DaemonLock>) -> Result<Ended, DaemonError> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|source| DaemonError::Bind { addr, source })?;
    let bound = listener.local_addr().map_err(DaemonError::Serve)?;
    let started_at = Timestamp::now();
    let info = json!({
        "pid": std::process::id(),
        "port": bound.port(),
        "allowed_hosts": config.allowed_hosts,
        "started_at": started_at.to_string(),
    });
    std::fs::write(info_path(&config.home), info.to_string()).map_err(|source| {
        DaemonError::Home {
            path: info_path(&config.home),
            source,
        }
    })?;

    // A damaged database is served read-only and never written, not even by
    // the workers or the stop event (`recovery.md` R10).
    let damage = Damage::default();
    damage.check_at_start(&config.home.join("state.db"));

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
    let tool_versions = Arc::new(Mutex::new(Value::Null));
    let tools = Tools {
        herdr: config.herdr.clone(),
        quota: config.quota.clone(),
        gh: config.gh.clone(),
        lavish: config.lavish.clone(),
        review_hosts: config.review_hosts.clone(),
        versions: Arc::clone(&tool_versions),
    };
    let workers = tokio::spawn(start_workers(
        config.home.clone(),
        damage.clone(),
        lock,
        tools.clone(),
        registry.clone(),
        stop_tx.clone(),
        Arc::clone(&fatal),
    ));

    // Health answers before `tiller init`, when no worker reads the tools.
    {
        let tools = tools.clone();
        std::thread::spawn(move || {
            let versions = workers::read_tool_versions(&tools).tools;
            *tools
                .versions
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = versions;
        });
    }

    // One link task per paired peer; none waits on another or on a request.
    let peers = Peers::start(
        config.home.clone(),
        Arc::new(SshConnector {
            program: config.ssh.clone(),
        }),
        Timing::default(),
        damage.clone(),
        stop.clone(),
    );

    let state = AppState::new(Inner {
        home: config.home.clone(),
        port: bound.port(),
        allowed_hosts: config.allowed_hosts,
        assets: config.assets,
        newest,
        stop: stop.clone(),
        pollers: registry,
        peers,
        herdr: config.herdr,
        terminal: config.terminal,
        started_at,
        tools: tool_versions,
        damage: damage.clone(),
        orchestrators: config.orchestrators,
        mirrors: crate::mirror::Mirrors::new(&config.home),
        sessions: crate::auth::Sessions::default(),
        streams: crate::sse::Streams::default(),
        ssh: config.ssh.clone(),
        evals: crate::memory::eval::Evals::default(),
        backups: crate::backups::BackupJobs::default(),
    });

    // A session queued to restart when idle survives a daemon restart: the
    // queue is in `state.db`, and this task picks it up.
    if let Some(starter) = state.0.orchestrators.clone() {
        tokio::spawn(crate::stale::drain_loop(
            db.clone(),
            starter,
            damage.clone(),
            stop.clone(),
        ));
    }

    // A daemon that stopped mid-Fit gives those panes back their size.
    {
        let fits = state.0.mirrors.fits();
        let herdr = state.0.herdr.clone();
        tokio::spawn(async move { fits.restore_left(&herdr).await });
    }

    // Handlers first: a SIGTERM that follows the address line at once must
    // stop the daemon cleanly, not kill it.
    let signal = signal();
    // The address line is the readiness signal for supervisors and the smoke
    // script. A closed stdout must not take the daemon down.
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "listening on http://{bound}").and_then(|()| stdout.flush());
    tracing::info!(%bound, pid = std::process::id(), "daemon started");
    if !db.exists() {
        // A fresh install: the service runs before anyone has set it up, and
        // this log is where the owner looks first.
        tracing::warn!(
            home = %config.home.display(),
            "state.db is not initialized; the API answers 503 until `{}` runs",
            api::INIT_HINT,
        );
    }

    let stopping = stop_tx.clone();
    let mut stopped_early = stop.clone();
    let shutdown = async move {
        tokio::select! {
            () = signal => {}
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
    if db.exists() && damage.reason().is_none() {
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
/// init` if needed. The database is migrated first (`recovery.md` R11); a
/// damaged one gets no workers at all (R10). A
/// database newer than this binary, or a panic, stops the daemon through
/// `fatal`.
async fn start_workers(
    home: PathBuf,
    damage: Damage,
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
        if let Some(reason) = damage.reason() {
            tracing::error!(
                reason,
                "state.db is damaged; no workers run (read-only mode)"
            );
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

/// Installs the SIGINT and SIGTERM handlers, the signals launchd and systemd
/// send on stop, and resolves on the first of them.
fn signal() -> impl std::future::Future<Output = ()> {
    use tokio::signal::unix::{SignalKind, signal};

    let handlers = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    );
    async move {
        let (Ok(mut term), Ok(mut int)) = handlers else {
            // Without signal handlers, run until killed.
            std::future::pending::<()>().await;
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::allowed_host;

    #[test]
    fn an_allowed_host_is_a_plain_name_never_a_pattern_or_url() {
        for (raw, host) in [
            ("tiller.example.com", "tiller.example.com"),
            ("Tiller.Example.COM", "tiller.example.com"),
            ("tiller.example.com:8443", "tiller.example.com:8443"),
            ("tiller.example.com:443", "tiller.example.com"),
            ("192.0.2.4", "192.0.2.4"),
            ("[2001:DB8::1]", "[2001:db8::1]"),
            ("[2001:db8::1]:443", "[2001:db8::1]"),
        ] {
            assert_eq!(allowed_host(raw).unwrap(), host, "{raw}");
        }
        for raw in [
            "",
            " ",
            "*.example.com",
            "https://tiller.example.com",
            "tiller.example.com/",
            "a b",
            "tiller.example.com:",
            "tiller.example.com:0",
            "tiller.example.com:65536",
            "tiller.example.com:+80",
            "tiller.example.com:80:80",
            "2001:db8::1",
            "[]",
            "[2001:db8::1",
            "[2001:db8::1]8443",
        ] {
            assert!(allowed_host(raw).is_err(), "{raw:?}");
        }
    }
}
