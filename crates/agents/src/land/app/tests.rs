use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex, OnceLock};

use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use rsa::pkcs1::EncodeRsaPrivateKey;
use rsa::pkcs8::EncodePrivateKey;

use super::*;

/// A throwaway 2048-bit key, made once per test binary.
fn throwaway() -> &'static rsa::RsaPrivateKey {
    static KEY: OnceLock<rsa::RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap())
}

fn pkcs1_pem() -> String {
    throwaway()
        .to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
        .unwrap()
        .to_string()
}

/// An App directory holding `id` and `pem` with the key at `mode`.
fn app_dir(id: &str, pem: &str, mode: u32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(APP_ID_FILE), id).unwrap();
    let key = dir.path().join(KEY_FILE);
    std::fs::write(&key, pem).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(mode)).unwrap();
    dir
}

/// One request the fake forge saw.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    authorization: String,
    body: String,
}

/// A fake GitHub on a local port: answers each request from `route` with a
/// status and a JSON body, and records what it saw.
fn fake_forge(
    route: impl Fn(&str, &str) -> (u16, String) + Send + 'static,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut parts = line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let path = parts.next().unwrap_or_default().to_owned();
            let (mut length, mut authorization) = (0, String::new());
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                let (name, value) = header.split_once(':').unwrap();
                match name.to_ascii_lowercase().as_str() {
                    "content-length" => length = value.trim().parse().unwrap(),
                    "authorization" => value.trim().clone_into(&mut authorization),
                    _ => {}
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let (status, reply) = route(&method, &path);
            log.lock().unwrap().push(Seen {
                method,
                path,
                authorization,
                body: String::from_utf8(body).unwrap(),
            });
            write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });
    (url, seen)
}

fn github(method: &str, path: &str) -> (u16, String) {
    match (method, path) {
        ("GET", "/repos/example/alpha/installation") => (200, r#"{"id":77}"#.into()),
        ("POST", "/app/installations/77/access_tokens") => (
            201,
            r#"{"token":"ghs_fake","expires_at":"2026-11-05T12:00:00Z"}"#.into(),
        ),
        _ => (404, r#"{"message":"Not Found"}"#.into()),
    }
}

/// Checks a JWT the App sent: signed by the key, issued by App 123.
fn assert_signed(authorization: &str) {
    let jwt = authorization.strip_prefix("Bearer ").unwrap();
    let (message, signature) = jwt.rsplit_once('.').unwrap();
    let key = AppKey::load(app_dir("123", &pkcs1_pem(), 0o600).path())
        .unwrap()
        .unwrap();
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.pair.public().as_ref())
        .verify(
            message.as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).unwrap(),
        )
        .unwrap();
    let claims: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(message.split_once('.').unwrap().1)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(claims["iss"], "123");
    let (iat, exp) = (
        claims["iat"].as_u64().unwrap(),
        claims["exp"].as_u64().unwrap(),
    );
    assert_eq!(exp - iat, 600, "nine minutes ahead, one behind");
}

/// A key that is there but unusable: a read error, not "no App".
fn unusable(error: ForgeError) -> String {
    match error {
        ForgeError::Read(why) => why,
        other => panic!("expected Read, got {other:?}"),
    }
}

fn no_app(error: ForgeError) -> String {
    match error {
        ForgeError::NoApp { why, .. } => why,
        other => panic!("expected NoApp, got {other:?}"),
    }
}

#[test]
fn no_key_is_no_app() {
    let dir = tempfile::tempdir().unwrap();
    // Nothing listens on the discard port: a call would fail differently.
    let app = HostApp::with_api(dir.path().join(APP_DIR), "http://127.0.0.1:9");
    let why = no_app(app.mint("example/alpha").unwrap_err());
    assert_eq!(why, "no merge App is installed on this host");
    assert!(app.app_id().is_err());
    assert!(app.installed("example/alpha").is_err());
}

#[test]
fn a_key_readable_by_others_is_refused() {
    let pem = pkcs1_pem();
    for mode in [0o644, 0o640, 0o604] {
        let dir = app_dir("123", &pem, mode);
        let app = HostApp::with_api(dir.path().to_owned(), "http://127.0.0.1:9");
        let why = no_app(app.mint("example/alpha").unwrap_err());
        assert!(why.contains("chmod 600"), "{why}");
        assert!(!why.contains("PRIVATE KEY"), "the key leaked: {why}");
        assert!(unusable(app.app_id().unwrap_err()).contains("chmod 600"));
    }
}

#[test]
fn a_bad_key_or_id_is_refused_without_its_content() {
    let dir = app_dir(
        "123",
        "-----BEGIN RSA PRIVATE KEY-----\nc2VjcmV0\n-----END RSA PRIVATE KEY-----\n",
        0o600,
    );
    let why = unusable(
        HostApp::with_api(dir.path().to_owned(), "http://127.0.0.1:9")
            .app_id()
            .unwrap_err(),
    );
    assert!(why.contains("not a usable RSA key"), "{why}");
    assert!(!why.contains("c2VjcmV0"), "{why}");

    let dir = app_dir("tiller-merge", &pkcs1_pem(), 0o600);
    let why = unusable(
        HostApp::with_api(dir.path().to_owned(), "http://127.0.0.1:9")
            .app_id()
            .unwrap_err(),
    );
    assert!(why.contains("not a numeric App id"), "{why}");

    let dir = app_dir("123", "not a key", 0o600);
    let why = unusable(
        HostApp::with_api(dir.path().to_owned(), "http://127.0.0.1:9")
            .app_id()
            .unwrap_err(),
    );
    assert!(why.contains("not a PEM RSA private key"), "{why}");
}

#[test]
fn mint_signs_a_jwt_and_scopes_the_token_to_the_repo() {
    let (url, seen) = fake_forge(github);
    let dir = app_dir("123\n", &pkcs1_pem(), 0o600);
    let app = HostApp::with_api(dir.path().to_owned(), url);
    assert_eq!(app.app_id().unwrap(), 123);

    let token = app.mint("example/alpha").unwrap();
    assert_eq!(token.secret(), "ghs_fake");
    assert_eq!(format!("{token:?}"), "AppToken(..)");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(
        (seen[0].method.as_str(), seen[0].path.as_str()),
        ("GET", "/repos/example/alpha/installation")
    );
    assert_eq!(seen[1].path, "/app/installations/77/access_tokens");
    for request in &seen {
        assert_signed(&request.authorization);
    }
    let body: Value = serde_json::from_str(&seen[1].body).unwrap();
    assert_eq!(
        body,
        json!({
            "repositories": ["alpha"],
            "permissions": {"contents": "write", "pull_requests": "write", "workflows": "write"},
        })
    );
    app.installed("example/alpha").unwrap();
}

#[test]
fn a_pkcs8_key_signs_too() {
    let pem = throwaway()
        .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
        .unwrap()
        .to_string();
    let (url, seen) = fake_forge(github);
    let dir = app_dir("123", &pem, 0o400);
    let token = HostApp::with_api(dir.path().to_owned(), url)
        .mint("example/alpha")
        .unwrap();
    assert_eq!(token.secret(), "ghs_fake");
    assert_signed(&seen.lock().unwrap()[0].authorization);
}

#[test]
fn a_refused_mint_names_github_answer_and_never_the_jwt() {
    let pem = pkcs1_pem();
    // Not installed on the repo.
    let (url, _) = fake_forge(github);
    let dir = app_dir("123", &pem, 0o600);
    let app = HostApp::with_api(dir.path().to_owned(), url);
    let why = no_app(app.mint("example/other").unwrap_err());
    assert!(
        why.contains("not installed") && why.contains("404") && why.contains("Not Found"),
        "{why}"
    );
    assert!(!why.contains("Bearer") && !why.contains("eyJ"), "{why}");
    assert!(app.installed("example/other").is_err());

    // Installed, but GitHub refuses the token.
    let (url, _) = fake_forge(|method, path| {
        match path {
        "/app/installations/77/access_tokens" => (422, r#"{"message":"There is at least one repository that does not exist or is not accessible"}"#.into()),
        _ => github(method, path),
    }
    });
    let app = HostApp::with_api(dir.path().to_owned(), url);
    let why = no_app(app.mint("example/alpha").unwrap_err());
    assert!(
        why.contains("no token was minted") && why.contains("422"),
        "{why}"
    );
}

#[test]
fn a_failed_installation_lookup_is_a_read_error_not_an_uninstalled_app() {
    let dir = app_dir("123", &pkcs1_pem(), 0o600);
    // GitHub not reached.
    let app = HostApp::with_api(dir.path().to_owned(), "http://127.0.0.1:9");
    assert!(matches!(
        app.installed("example/alpha"),
        Err(ForgeError::Read(_))
    ));
    assert!(matches!(
        app.mint("example/alpha"),
        Err(ForgeError::Read(_))
    ));
    // GitHub answered, but not about the install.
    for status in [403, 502] {
        let (url, _) = fake_forge(move |_, _| (status, r#"{"message":"try later"}"#.into()));
        let app = HostApp::with_api(dir.path().to_owned(), url);
        let why = match app.installed("example/alpha") {
            Err(ForgeError::Read(why)) => why,
            other => panic!("{status}: {other:?}"),
        };
        assert!(why.contains(&status.to_string()), "{why}");
    }
}
