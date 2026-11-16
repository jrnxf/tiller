//! What other crates' end-to-end tests need of a running daemon: signing
//! in as the owner does it. Built only with the `testing` feature.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use crate::auth;

/// The owner session a browser holds once it signs in: the cookie the
/// daemon set, and the owner token the board keeps and sends with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedIn {
    pub cookie: String,
    pub token: String,
}

impl SignedIn {
    /// The request header lines that send this session, each ended with
    /// CRLF, as the board sends them.
    pub fn headers(&self) -> String {
        format!(
            "Cookie: {}={}\r\n{}: {}\r\n",
            auth::COOKIE,
            self.cookie,
            auth::TOKEN_HEADER,
            self.token
        )
    }
}

/// Signs in to the daemon of `home` on `port` as the owner does it:
/// `tiller board --login` mints a login, and opening its link has the
/// daemon set the cookie and hand the board its token in the fragment of
/// its redirect. The daemon is the only writer of the owner sessions and
/// keeps them in memory, so a session written into the file behind its
/// back would not be seen.
pub fn sign_in(home: &Path, port: u16) -> SignedIn {
    let now = tiller_core::Timestamp::now();
    let login = auth::mint_login(home, now).unwrap().token;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "GET /login/{login} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let header = |wanted: &str| {
        answer.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(wanted)
                .then(|| value.trim().to_owned())
        })
    };
    let cookie = header("set-cookie")
        .and_then(|set| {
            let pair = set.split(';').next()?.to_owned();
            auth::cookie_value(&pair).map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("the login link set no cookie: {answer}"));
    let token = header("location")
        .and_then(|location| location.split_once("#owner=").map(|(_, t)| t.to_owned()))
        .filter(|token| !token.is_empty())
        .unwrap_or_else(|| panic!("the login link handed no owner token: {answer}"));
    SignedIn { cookie, token }
}

/// The one script every [`stub_script`] links to: it runs the body that sits
/// beside the link it was started through.
const STUB: &str = "#!/bin/sh\n. \"$0.body\"\n";

/// Where the shared stub lives: one file for every test process of the user,
/// so a build of the tests pays for it once.
fn stub_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("tiller-test-stub-{:x}", STUB.len()))
}

/// A fake tool `dir/name` that runs the shell `body` and is cheap to start.
///
/// A script written fresh is checked by the OS the first time it runs (on
/// macOS that is 0.2 to 0.5 s, and a lot more with many tests starting
/// scripts at once), which sent a tool read past its 5 s deadline in the
/// doctor and health tests under load. The name is a symlink to one shared
/// script that the OS has checked once, and the body is a data file beside
/// it, so a fake tool never costs a fresh check. Returns the path to run.
pub fn stub_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let stub = stub_path();
    if !stub.exists() {
        let part = stub.with_extension(format!("part{}", std::process::id()));
        std::fs::write(&part, STUB).unwrap();
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Run it once now, so the OS checks it before any test waits on a
        // tool (it fails to find a body, which is no matter).
        let _ = std::process::Command::new(&part)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // The first writer wins; a later one keeps the file already there,
        // so its checked state is never thrown away.
        let _ = std::fs::hard_link(&part, &stub);
        let _ = std::fs::remove_file(&part);
    }
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    symlink(&stub, &path).unwrap();
    std::fs::write(dir.join(format!("{name}.body")), format!("{body}\n")).unwrap();
    path
}
