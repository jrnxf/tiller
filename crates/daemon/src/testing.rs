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
