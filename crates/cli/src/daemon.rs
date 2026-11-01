//! Placeholder daemon: serves the embedded board on localhost.
//! The real daemon (pollers, reconcile, API) arrives with its own crate.

use std::borrow::Cow;
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Context;
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

const IMMUTABLE: &str = "public, max-age=31536000, immutable";

#[derive(clap::Args)]
pub struct Args {
    /// Port to listen on, 0 picks a free one. The daemon only binds localhost.
    #[arg(long, default_value_t = 7421)]
    port: u16,
}

/// The board build, embedded in release builds and read from disk in debug builds.
#[derive(RustEmbed)]
#[folder = "../../board/dist"]
#[allow_missing = true]
struct Board;

pub fn run(args: &Args) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start async runtime")?;
    runtime.block_on(serve(args.port))
}

async fn serve(port: u16) -> anyhow::Result<()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    let bound = listener.local_addr().context("read bound address")?;
    // The address line is the readiness signal for supervisors and the smoke
    // script. A closed stdout must not take the daemon down.
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "listening on http://{bound}").and_then(|()| stdout.flush());
    axum::serve(listener, router::<Board>())
        .with_graceful_shutdown(shutdown())
        .await
        .context("serve")
}

/// Resolves on SIGINT or SIGTERM, the signal launchd and systemd send on stop.
async fn shutdown() {
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

fn router<E: RustEmbed + 'static>() -> Router {
    Router::new()
        .fallback(asset::<E>)
        .layer(middleware::from_fn(local_host_only))
}

/// Reject requests whose `Host` is not a loopback name, so a page cannot reach
/// the daemon through DNS rebinding.
async fn local_host_only(request: Request, next: Next) -> Response {
    let allowed = request
        .headers()
        .get(header::HOST)
        .is_none_or(|host| host.to_str().is_ok_and(is_loopback_host));
    if allowed {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

fn is_loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split_once(']').map_or(rest, |(ip, _)| ip),
        None => host.split(':').next().unwrap_or(host),
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

async fn asset<E: RustEmbed>(method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let requested = uri.path().trim_start_matches('/');
    let path = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    if let Some(file) = E::get(path) {
        let cache = if path.starts_with("assets/") {
            IMMUTABLE
        } else {
            "no-cache"
        };
        return file_response(file, cache);
    }
    // A missing hashed asset must not turn into HTML. Anything else is a
    // client-side route, so hand it the app shell.
    if requested.starts_with("assets/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    match E::get("index.html") {
        Some(file) => file_response(file, "no-cache"),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "board assets are not built: run `bun run build` in board/",
        )
            .into_response(),
    }
}

fn file_response(file: rust_embed::EmbeddedFile, cache: &'static str) -> Response {
    // Embedded release data is `'static`: serve it without copying.
    let body = match file.data {
        Cow::Borrowed(bytes) => Body::from(bytes),
        Cow::Owned(bytes) => Body::from(bytes),
    };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/dist"]
    struct Fixture;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/empty"]
    #[allow_missing = true]
    struct Missing;

    async fn get<E: RustEmbed + 'static>(path: &str) -> (StatusCode, Option<String>, String) {
        send::<E>(Request::get(path).body(Body::empty()).unwrap()).await
    }

    async fn send<E: RustEmbed + 'static>(
        request: Request<Body>,
    ) -> (StatusCode, Option<String>, String) {
        let response = router::<E>().oneshot(request).await.unwrap();
        let status = response.status();
        let cache = response
            .headers()
            .get(header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap().to_owned());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, cache, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn root_serves_the_app_shell() {
        let (status, cache, body) = get::<Fixture>("/").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache.as_deref(), Some("no-cache"));
        assert!(body.contains(r#"<div id="root">"#));
    }

    #[tokio::test]
    async fn hashed_assets_are_immutable() {
        let (status, cache, body) = get::<Fixture>("/assets/app-abc123.js").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cache.as_deref(), Some(IMMUTABLE));
        assert_eq!(body.trim(), "console.log('tiller');");
    }

    #[tokio::test]
    async fn client_routes_fall_back_to_the_app_shell() {
        let (status, _, body) = get::<Fixture>("/cards/demo-42").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#"<div id="root">"#));
    }

    #[tokio::test]
    async fn client_routes_with_dots_fall_back_to_the_app_shell() {
        let (status, _, body) = get::<Fixture>("/projects/example.com").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#"<div id="root">"#));
    }

    #[tokio::test]
    async fn missing_asset_is_not_found() {
        for path in ["/assets/gone.js", "/assets/gone"] {
            let (status, _, _) = get::<Fixture>(path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn only_get_and_head_are_served() {
        let request = Request::post("/").body(Body::empty()).unwrap();
        let (status, _, _) = send::<Fixture>(request).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn foreign_host_is_refused() {
        for (host, expected) in [
            ("127.0.0.1:7421", StatusCode::OK),
            ("localhost:7421", StatusCode::OK),
            ("[::1]:7421", StatusCode::OK),
            ("evil.example.com:7421", StatusCode::FORBIDDEN),
            ("127.0.0.1.evil.example.com", StatusCode::FORBIDDEN),
        ] {
            let request = Request::get("/")
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap();
            let (status, _, _) = send::<Fixture>(request).await;
            assert_eq!(status, expected, "{host}");
        }
    }

    #[tokio::test]
    async fn unbuilt_board_says_so() {
        let (status, _, body) = get::<Missing>("/").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("not built"));
    }
}
