//! The embedded board: hashed assets served `immutable`, every other path
//! the app shell, with the boot snapshot inlined (`protocol.md` 7.6).

use std::borrow::Cow;

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::{EmbeddedFile, RustEmbed};

pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// The board build, embedded in release builds and read from disk in debug
/// builds.
#[derive(RustEmbed)]
#[folder = "../../board/dist"]
#[allow_missing = true]
pub struct Board;

/// A board build to serve: the real one, or a test fixture.
#[derive(Clone, Copy)]
pub struct Assets {
    get: fn(&str) -> Option<EmbeddedFile>,
}

impl Assets {
    pub fn of<E: RustEmbed>() -> Self {
        Self { get: E::get }
    }

    pub fn board() -> Self {
        Self::of::<Board>()
    }

    /// The app shell's HTML, if the board is built.
    pub fn index(&self) -> Option<String> {
        (self.get)("index.html").map(|file| String::from_utf8_lossy(&file.data).into_owned())
    }

    /// Whether `path` is a built file [`Assets::file`] answers, found or
    /// not, rather than the app shell.
    pub fn is_file(&self, path: &str) -> bool {
        !matches!(self.resolve(path), Resolved::Shell)
    }

    /// A built file other than the app shell, with its cache policy.
    /// `None` for a path the app shell handles; `Err` for a missing hashed
    /// asset, which must never turn into HTML.
    pub fn file(&self, path: &str) -> Result<Option<Response>, StatusCode> {
        match self.resolve(path) {
            Resolved::Shell => Ok(None),
            Resolved::File(file, cache) => Ok(Some(file_response(file, cache))),
            Resolved::Missing => Err(StatusCode::NOT_FOUND),
        }
    }

    /// What `path` names: the one rule [`Assets::is_file`] and
    /// [`Assets::file`] share.
    fn resolve(&self, path: &str) -> Resolved {
        let path = path.trim_start_matches('/');
        if path.is_empty() || path == "index.html" {
            return Resolved::Shell;
        }
        let hashed = path.starts_with("assets/");
        match (self.get)(path) {
            Some(file) => Resolved::File(file, if hashed { IMMUTABLE } else { "no-cache" }),
            None if hashed => Resolved::Missing,
            None => Resolved::Shell,
        }
    }
}

/// A request path as the board build sees it.
enum Resolved {
    /// The app shell answers it.
    Shell,
    /// A built file, with its `Cache-Control`.
    File(EmbeddedFile, &'static str),
    /// A hashed asset that is not in the build: 404, never HTML.
    Missing,
}

fn file_response(file: EmbeddedFile, cache: &'static str) -> Response {
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

/// The answer when the board was never built.
pub fn unbuilt() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "board assets are not built: run `bun run build` in board/",
    )
        .into_response()
}

/// JSON made safe to inline in HTML: `<`, `>`, `&` and U+2028/2029 as `\u`
/// escapes, so no string in it can close the script element.
pub fn escape_json(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// The app shell with the snapshot inlined just before `</head>`.
pub fn with_boot(index: &str, snapshot_json: &str) -> String {
    let script = format!(
        r#"<script type="application/json" id="tiller-boot">{}</script>"#,
        escape_json(snapshot_json)
    );
    match index.find("</head>") {
        Some(at) => format!("{}{script}{}", &index[..at], &index[at..]),
        None => format!("{script}{index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(RustEmbed)]
    #[folder = "tests/fixtures/dist"]
    struct Fixture;

    #[test]
    fn is_file_and_file_agree_on_every_kind_of_path() {
        let assets = Assets::of::<Fixture>();
        for (path, file) in [
            ("/", false),
            ("/index.html", false),
            ("/projects/alpha", false),
            ("/assets/app-abc123.js", true),
            ("/assets/gone-2.js", true),
        ] {
            assert_eq!(assets.is_file(path), file, "{path}");
            assert_eq!(!matches!(assets.file(path), Ok(None)), file, "{path}");
        }
        assert_eq!(
            assets.file("/assets/gone-2.js").err(),
            Some(StatusCode::NOT_FOUND)
        );
    }

    #[test]
    fn inlined_json_cannot_close_the_script() {
        let json = "{\"title\":\"</script><script>alert(1)</script> & \u{2028}\"}";
        let html = with_boot("<html><head></head><body></body></html>", json);
        assert_eq!(html.matches("</script>").count(), 1);
        assert!(!html.contains('\u{2028}'));
        let inner = html
            .split_once(r#"id="tiller-boot">"#)
            .unwrap()
            .1
            .split_once("</script>")
            .unwrap()
            .0;
        let back: serde_json::Value = serde_json::from_str(inner).unwrap();
        assert_eq!(
            back["title"],
            "</script><script>alert(1)</script> & \u{2028}"
        );
        assert!(html.find("tiller-boot").unwrap() < html.find("</head>").unwrap());
    }
}
