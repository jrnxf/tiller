//! What the daemon's end-to-end tests share.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

/// The owner's cookie value for the daemon of `home` on `port`, signed in
/// as the owner does it: `tiller board --login` mints a login, and opening
/// its link has the daemon set the cookie. The daemon is the only writer of
/// the owner cookies and keeps them in memory, so a cookie written into the
/// file behind its back would not be seen.
pub fn sign_in(home: &Path, port: u16) -> String {
    let now = tiller_core::Timestamp::now();
    let token = tiller_daemon::auth::mint_login(home, now).unwrap().token;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    write!(
        stream,
        "GET /login/{token} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let named = format!("{}=", tiller_daemon::auth::COOKIE);
    answer
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            let value = value.trim().strip_prefix(named.as_str())?;
            name.eq_ignore_ascii_case("set-cookie")
                .then(|| value.split(';').next().unwrap_or_default().to_owned())
        })
        .unwrap_or_else(|| panic!("the login link set no cookie: {answer}"))
}
