//! Placeholder daemon: serves the embedded board on localhost.
//! The real daemon (pollers, reconcile, API) arrives with its own crate.

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::Context;
use axum::Router;
use axum::http::{StatusCode, Uri, header};
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
    println!("listening on http://{bound}");
    axum::serve(listener, router::<Board>())
        .with_graceful_shutdown(shutdown())
        .await
        .context("serve")
}

async fn shutdown() {
    // If the signal handler cannot be installed, run until killed.
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn router<E: RustEmbed + 'static>() -> Router {
    Router::new().fallback(asset::<E>)
}

async fn asset<E: RustEmbed>(uri: Uri) -> Response {
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
    // Paths with a file extension are real asset misses. Anything else is a
    // client-side route, so hand it the app shell.
    let is_file = requested
        .rsplit('/')
        .next()
        .is_some_and(|name| name.contains('.'));
    if is_file {
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
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        file.data.into_owned(),
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
        let response = router::<E>()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
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
    async fn missing_asset_is_not_found() {
        let (status, _, _) = get::<Fixture>("/assets/gone.js").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unbuilt_board_says_so() {
        let (status, _, body) = get::<Missing>("/").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("not built"));
    }
}
