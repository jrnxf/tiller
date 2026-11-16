//! The owner CLI socket (`authority.md` section 7, Owner CLI socket;
//! `protocol.md` 7.1): a second listener, a Unix socket at
//! `<home>/run/daemon.sock` (mode `0600`, in the `0700` run directory),
//! through which the owner's `tiller` reaches what only the daemon holds:
//! the federated reads of a paired peer's rows and the mutations forwarded
//! to a peer (`protocol.md` 10.3).
//!
//! Every request names its caller by the kernel's credential of the
//! connection, never by a cookie. The caller must be the daemon's own uid,
//! and neither it nor an ancestor may be Claude Code
//! ([`principal::socket_owner`]). The routes are an allowlist
//! ([`SOCKET_ROUTES`]): reads, and the card, attention and ask mutations a
//! peer takes. A mutation of this host's own records is refused here; the
//! CLI writes those itself, and `spawn`, grants, away mode and landing stay
//! local (ADR 0003).

use std::path::{Path, PathBuf};

use axum::extract::connect_info::Connected;
use axum::http::Method;
use axum::serve::IncomingStream;
use tiller_core::authority::principal;
use tiller_holder::peer::PeerCred;
use tokio::net::UnixListener;

/// `<home>/run/daemon.sock`.
pub fn socket_path(home: &Path) -> PathBuf {
    tiller_panes::run_dir(home).join("daemon.sock")
}

/// Who is on the other end of a socket connection, as the kernel says.
#[derive(Clone, Debug)]
pub struct Caller(Result<PeerCred, String>);

impl Connected<IncomingStream<'_, UnixListener>> for Caller {
    fn connect_info(stream: IncomingStream<'_, UnixListener>) -> Self {
        Self(PeerCred::of(stream.io()).map_err(|error| error.to_string()))
    }
}

impl Caller {
    /// Why this caller is not the owner, if it is not
    /// (`authority.md` section 7, Owner CLI socket).
    pub fn refusal(&self) -> Option<String> {
        let cred = match &self.0 {
            Ok(cred) => cred,
            Err(error) => return Some(format!("the caller's credential: {error}")),
        };
        principal::socket_owner(cred.uid, tiller_holder::peer::own_uid(), cred.pid).err()
    }
}

/// Every route the socket serves, by the path it is routed under. A route
/// left out is 403 there. The writes are refused unless they name a paired
/// peer (`api::submit`).
pub const SOCKET_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/api/v1/board"),
    (Method::GET, "/api/v1/cards/{id}"),
    (Method::GET, "/api/v1/cards/{id}/timeline"),
    (Method::GET, "/api/v1/attention"),
    (Method::GET, "/api/v1/agents"),
    (Method::GET, "/api/v1/runs/{id}"),
    (Method::GET, "/api/v1/machines"),
    (Method::GET, "/api/v1/projects"),
    (Method::PATCH, "/api/v1/cards/{id}"),
    (Method::POST, "/api/v1/cards/{id}/comment"),
    (Method::POST, "/api/v1/cards/{id}/hold"),
    (Method::DELETE, "/api/v1/cards/{id}/hold"),
    (Method::POST, "/api/v1/cards/{id}/deps"),
    (Method::DELETE, "/api/v1/cards/{id}/deps"),
    (Method::POST, "/api/v1/cards/{id}/close"),
    (Method::POST, "/api/v1/cards/{id}/cancel"),
    (Method::POST, "/api/v1/cards/{id}/reopen"),
    (Method::POST, "/api/v1/attention/{id}/answer"),
    (Method::POST, "/api/v1/projects/{id}/asks"),
];

/// Whether the socket serves `method` on `route`.
pub fn serves(method: &Method, route: Option<&str>) -> bool {
    let Some(route) = route else {
        return false;
    };
    let method = if *method == Method::HEAD {
        &Method::GET
    } else {
        method
    };
    SOCKET_ROUTES
        .iter()
        .any(|(declared, path)| declared == method && *path == route)
}

/// Binds the socket. The run directory is made `0700`, a socket left by a
/// daemon that did not stop cleanly is removed (the caller holds the daemon
/// lock, so no live daemon serves it), and the new socket is made `0600`
/// before the first accept.
pub fn bind(home: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = tiller_panes::run_dir(home);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = socket_path(home);
    tiller_panes::paths::check_socket_path(&path).map_err(std::io::Error::other)?;
    match std::fs::remove_file(&path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_serves_reads_and_peer_mutations_only() {
        for (method, route) in [
            (Method::GET, "/api/v1/board"),
            (Method::HEAD, "/api/v1/agents"),
            (Method::POST, "/api/v1/cards/{id}/comment"),
            (Method::PATCH, "/api/v1/cards/{id}"),
        ] {
            assert!(serves(&method, Some(route)), "{method} {route}");
        }
        for (method, route) in [
            // A sign-in mints a cookie: never over the socket.
            (Method::GET, "/login/{token}"),
            (Method::POST, "/api/v1/login"),
            (Method::POST, "/api/v1/logout"),
            // ADR 0003: spawn, grants, away mode and landing stay local.
            (Method::POST, "/api/v1/away"),
            (Method::POST, "/api/v1/projects/{id}/grants"),
            (Method::POST, "/api/v1/approvals/{id}/decide"),
            (Method::POST, "/api/v1/projects/{id}/orchestrator"),
            (Method::POST, "/api/v1/cards"),
            (Method::POST, "/api/v1/agents/{session}/steer"),
            (Method::GET, "/api/v1/events"),
            (Method::GET, "/api/v1/snapshot"),
            (Method::DELETE, "/api/v1/cards/{id}"),
        ] {
            assert!(!serves(&method, Some(route)), "{method} {route}");
        }
        assert!(!serves(&Method::GET, None), "the fallback");
    }

    #[tokio::test]
    async fn the_socket_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        // A socket left by a daemon that was killed is replaced.
        std::fs::create_dir_all(tiller_panes::run_dir(home.path())).unwrap();
        std::fs::write(socket_path(home.path()), "stale").unwrap();
        let _listener = bind(home.path()).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&socket_path(home.path())), 0o600);
        assert_eq!(mode(&tiller_panes::run_dir(home.path())), 0o700);
        let stream = tokio::net::UnixStream::connect(socket_path(home.path()))
            .await
            .unwrap();
        // This test process is no Claude Code process, whoever runs it,
        // only when its own chain is clean; the uid is always its own.
        let cred = PeerCred::of(&stream).unwrap();
        assert_eq!(cred.uid, tiller_holder::peer::own_uid());
    }
}
