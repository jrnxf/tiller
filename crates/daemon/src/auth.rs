//! The board API credential (`protocol.md` 7.1, `authority.md` "Board API
//! credential"): a one-time login link minted by `tiller board --login`, and
//! the `tiller_owner` cookie it sets.
//!
//! Only SHA-256 hashes are stored, under `~/.tiller`, which every fleet role
//! is denied. `board-login` holds pending login tokens (ten minutes each),
//! `board-owner` the minted cookies (30 days each). Each line is
//! `<hex sha256> <expiry RFC 3339>`.

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tiller_core::Timestamp;

/// The cookie's name.
pub const COOKIE: &str = "tiller_owner";
/// How long a cookie lives: 30 days (CP2, C15).
pub const COOKIE_SECONDS: i64 = 2_592_000;
/// How long a login link stays usable.
pub const LOGIN_MINUTES: i64 = 10;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("read the system random source: {0}")]
    Random(std::io::Error),
    #[error("lock {path}: {source}")]
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn login_file(home: &Path) -> PathBuf {
    home.join("board-login")
}

fn owner_file(home: &Path) -> PathBuf {
    home.join("board-owner")
}

/// 32 random bytes as hex, from the OS.
fn secret() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(AuthError::Random)?;
    Ok(hex(&bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash(secret: &str) -> String {
    hex(&Sha256::digest(secret.as_bytes()))
}

/// The unexpired `(hash, expiry)` lines of one file.
fn read_lines(path: &Path, now: Timestamp) -> Vec<(String, Timestamp)> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (hash, expiry) = line.split_once(' ')?;
            let expiry: Timestamp = expiry.trim().parse().ok()?;
            (expiry > now).then(|| (hash.to_owned(), expiry))
        })
        .collect()
}

/// Rewrites a file atomically, readable by the owner only.
fn write_lines(path: &Path, lines: &[(String, Timestamp)]) -> Result<(), AuthError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let error = |source| AuthError::Write {
        path: path.to_owned(),
        source,
    };
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(error)?;
    for (hash, expiry) in lines {
        writeln!(file, "{hash} {expiry}").map_err(error)?;
    }
    file.sync_all().map_err(error)?;
    std::fs::rename(&temporary, path).map_err(error)
}

/// Runs `change` holding `<home>/board-auth.lock`, so the CLI minting and
/// the daemon redeeming (on several threads) never interleave their
/// read-modify-write of the two files.
fn locked<T>(home: &Path, change: impl FnOnce() -> Result<T, AuthError>) -> Result<T, AuthError> {
    use std::os::unix::fs::OpenOptionsExt;

    let path = home.join("board-auth.lock");
    let error = |source| AuthError::Lock {
        path: path.clone(),
        source,
    };
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(error)?;
    file.lock().map_err(error)?;
    // Dropping the file releases the lock.
    change()
}

/// Mints a one-time login token and records its hash. The caller prints
/// `/login/<token>`; the token itself is never stored.
pub fn mint_login(home: &Path, now: Timestamp) -> Result<String, AuthError> {
    let token = secret()?;
    locked(home, || {
        let mut lines = read_lines(&login_file(home), now);
        lines.push((hash(&token), now.plus_minutes(LOGIN_MINUTES)));
        write_lines(&login_file(home), &lines)
    })?;
    Ok(token)
}

/// Spends a login token: true once for a valid, unexpired token, then never
/// again. On success the cookie value to set is returned.
pub fn redeem_login(home: &Path, token: &str, now: Timestamp) -> Result<Option<String>, AuthError> {
    let wanted = hash(token);
    locked(home, || {
        let mut lines = read_lines(&login_file(home), now);
        let before = lines.len();
        lines.retain(|(hash, _)| !constant_eq(hash, &wanted));
        if lines.len() == before {
            return Ok(None);
        }
        write_lines(&login_file(home), &lines)?;
        let cookie = secret()?;
        let mut owners = read_lines(&owner_file(home), now);
        owners.push((hash(&cookie), later(now, COOKIE_SECONDS)));
        write_lines(&owner_file(home), &owners)?;
        Ok(Some(cookie))
    })
}

/// Whether `cookie` is a minted, unexpired owner cookie.
pub fn is_owner(home: &Path, cookie: &str, now: Timestamp) -> bool {
    let wanted = hash(cookie);
    read_lines(&owner_file(home), now)
        .iter()
        .any(|(hash, _)| constant_eq(hash, &wanted))
}

/// The `Set-Cookie` value for a fresh cookie.
pub fn set_cookie(value: &str) -> String {
    format!("{COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_SECONDS}")
}

/// The owner cookie's value from a `Cookie` header.
pub fn cookie_value(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.trim().split_once('=')?;
        (name == COOKIE).then_some(value)
    })
}

/// `seconds` after `now`.
fn later(now: Timestamp, seconds: i64) -> Timestamp {
    Timestamp::from_millis(now.as_millis().saturating_add(seconds.saturating_mul(1000)))
        .unwrap_or(now)
}

fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_redeemed_at_once_on_many_threads_works_once() {
        let home = tempfile::tempdir().unwrap();
        let now = Timestamp::now();
        // Other links minted meanwhile must survive the redeems.
        let token = mint_login(home.path(), now).unwrap();
        let redeems: Vec<_> = (0..8)
            .map(|_| {
                let (home, token) = (home.path().to_owned(), token.clone());
                std::thread::spawn(move || redeem_login(&home, &token, now).unwrap())
            })
            .collect();
        let minted: Vec<_> = (0..4)
            .map(|_| {
                let home = home.path().to_owned();
                std::thread::spawn(move || mint_login(&home, now).unwrap())
            })
            .collect();
        let cookies: Vec<String> = redeems
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .collect();
        assert_eq!(cookies.len(), 1);
        assert!(is_owner(home.path(), &cookies[0], now));
        for other in minted {
            let other = other.join().unwrap();
            assert!(redeem_login(home.path(), &other, now).unwrap().is_some());
        }
    }

    fn now() -> Timestamp {
        "2026-11-02T14:00:00.000Z".parse().unwrap()
    }

    #[test]
    fn a_login_link_works_once_and_only_its_cookie_is_owner() {
        let home = tempfile::tempdir().unwrap();
        let token = mint_login(home.path(), now()).unwrap();
        let cookie = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        assert!(is_owner(home.path(), &cookie, now()));
        assert!(!is_owner(home.path(), &token, now()));
        assert_eq!(redeem_login(home.path(), &token, now()).unwrap(), None);
        // Neither secret is on disk, only hashes.
        for file in ["board-login", "board-owner"] {
            let text = std::fs::read_to_string(home.path().join(file)).unwrap();
            assert!(!text.contains(&token) && !text.contains(&cookie), "{file}");
        }
    }

    #[test]
    fn links_and_cookies_expire() {
        let home = tempfile::tempdir().unwrap();
        let token = mint_login(home.path(), now()).unwrap();
        let late = now().plus_minutes(LOGIN_MINUTES + 1);
        assert_eq!(redeem_login(home.path(), &token, late).unwrap(), None);

        let token = mint_login(home.path(), now()).unwrap();
        let cookie = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        assert!(is_owner(
            home.path(),
            &cookie,
            later(now(), COOKIE_SECONDS - 1)
        ));
        assert!(!is_owner(
            home.path(),
            &cookie,
            later(now(), COOKIE_SECONDS)
        ));
    }

    #[test]
    fn the_cookie_is_read_from_among_others() {
        assert_eq!(cookie_value("a=1; tiller_owner=abc; b=2"), Some("abc"));
        assert_eq!(cookie_value("tiller_owner_x=abc"), None);
    }
}
