//! The board API credential (`protocol.md` 7.1, `authority.md` "Board API
//! credential"): a one-time login, minted by `tiller board --login` as a
//! link and an 8-digit code, and the `tiller_owner` cookie either sets.
//!
//! Only SHA-256 hashes are stored, under `~/.tiller`, which every fleet role
//! is denied. `board-login` holds pending logins (one minute each), one per
//! line as `<token hash> <expiry RFC 3339> <code hash or -> <wrong codes>`;
//! `board-owner` holds the minted cookies (30 days each), one per line as
//! `<hash> <expiry RFC 3339>`.

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tiller_core::Timestamp;
use tokio::sync::watch;

/// The cookie's name.
pub const COOKIE: &str = "tiller_owner";
/// How long a cookie lives: 30 days (CP2, C15).
pub const COOKIE_SECONDS: i64 = 2_592_000;
/// How long a login link stays usable: one minute, since it may be shown as
/// a QR code on a screen others can see.
pub const LOGIN_MINUTES: i64 = 1;
/// Wrong codes tried while a code is pending that burn it: its link still
/// works, its code no longer does.
pub const CODE_MISSES: u32 = 3;
/// Code tries the daemon takes in any one minute, from every client
/// together.
pub const CODE_TRIES_PER_MINUTE: usize = 10;

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

/// Fills `bytes` from the OS.
fn random(bytes: &mut [u8]) -> Result<(), AuthError> {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(bytes))
        .map_err(AuthError::Random)
}

/// 32 random bytes as hex, from the OS.
fn secret() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    random(&mut bytes)?;
    Ok(hex(&bytes))
}

/// A uniformly random 8-digit code, leading zeros kept.
fn code() -> Result<String, AuthError> {
    const SPAN: u64 = 100_000_000;
    // Values from here up would favor the low codes.
    const LIMIT: u64 = u64::MAX - u64::MAX % SPAN;
    loop {
        let mut bytes = [0u8; 8];
        random(&mut bytes)?;
        let value = u64::from_le_bytes(bytes);
        if value < LIMIT {
            return Ok(format!("{:08}", value % SPAN));
        }
    }
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

/// A login `tiller board --login` minted and nobody has used yet.
struct Pending {
    token: String,
    expiry: Timestamp,
    /// None once wrong codes burned it.
    code: Option<String>,
    misses: u32,
}

/// The unexpired pending logins.
fn read_pending(path: &Path, now: Timestamp) -> Vec<Pending> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let token = fields.next()?.to_owned();
            let expiry: Timestamp = fields.next()?.trim().parse().ok()?;
            let code = fields.next().filter(|code| *code != "-").map(str::to_owned);
            let misses = fields.next().and_then(|n| n.trim().parse().ok());
            (expiry > now).then(|| Pending {
                token,
                expiry,
                code,
                misses: misses.unwrap_or(0),
            })
        })
        .collect()
}

fn write_pending(path: &Path, pending: &[Pending]) -> Result<(), AuthError> {
    write_file(
        path,
        pending.iter().map(|login| {
            let code = login.code.as_deref().unwrap_or("-");
            format!("{} {} {code} {}", login.token, login.expiry, login.misses)
        }),
    )
}

fn write_lines(path: &Path, lines: &[(String, Timestamp)]) -> Result<(), AuthError> {
    write_file(
        path,
        lines
            .iter()
            .map(|(hash, expiry)| format!("{hash} {expiry}")),
    )
}

/// Rewrites a file atomically, readable by the owner only.
fn write_file(path: &Path, lines: impl Iterator<Item = String>) -> Result<(), AuthError> {
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
    for line in lines {
        writeln!(file, "{line}").map_err(error)?;
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

/// One login as `tiller board --login` hands it to the owner: the link's
/// token and the code to type. Either signs one browser in, once.
pub struct Login {
    pub token: String,
    pub code: String,
}

/// Mints a one-time login and records its hashes. The caller prints
/// `/login/<token>` and the code; neither is ever stored.
pub fn mint_login(home: &Path, now: Timestamp) -> Result<Login, AuthError> {
    let token = secret()?;
    locked(home, || {
        let mut pending = read_pending(&login_file(home), now);
        // Two pending logins never share a code, so a code names one.
        let code = loop {
            let code = code()?;
            let taken = pending
                .iter()
                .any(|login| login.code.as_deref() == Some(hash(&code).as_str()));
            if !taken {
                break code;
            }
        };
        pending.push(Pending {
            token: hash(&token),
            expiry: now.plus_minutes(LOGIN_MINUTES),
            code: Some(hash(&code)),
            misses: 0,
        });
        write_pending(&login_file(home), &pending)?;
        Ok(Login { token, code })
    })
}

/// Records a new owner cookie and returns its value. The caller holds the
/// lock.
fn mint_owner(home: &Path, now: Timestamp) -> Result<String, AuthError> {
    let cookie = secret()?;
    let mut owners = read_lines(&owner_file(home), now);
    owners.push((hash(&cookie), later(now, COOKIE_SECONDS)));
    write_lines(&owner_file(home), &owners)?;
    Ok(cookie)
}

/// Spends a login by its link's token: a cookie once for a valid, unexpired
/// token, then never again. Its code goes with it.
pub fn redeem_login(home: &Path, token: &str, now: Timestamp) -> Result<Option<String>, AuthError> {
    let wanted = hash(token);
    locked(home, || {
        let mut pending = read_pending(&login_file(home), now);
        let before = pending.len();
        pending.retain(|login| !constant_eq(&login.token, &wanted));
        if pending.len() == before {
            return Ok(None);
        }
        write_pending(&login_file(home), &pending)?;
        mint_owner(home, now).map(Some)
    })
}

/// What a typed code did.
#[derive(Debug, PartialEq, Eq)]
pub enum CodeLogin {
    /// It matched a pending login, now spent: the cookie value to set.
    Owner(String),
    /// It matched nothing pending: wrong, expired, used or burned.
    Wrong,
    /// It matched nothing, and that miss burned at least one pending code.
    Burned,
}

/// Spends a login by its code, as [`redeem_login`] does by its token. A
/// wrong code counts against every pending code, since it may have been
/// meant for any of them, and each burns on its [`CODE_MISSES`]th.
pub fn redeem_code(home: &Path, code: &str, now: Timestamp) -> Result<CodeLogin, AuthError> {
    let wanted = hash(code);
    locked(home, || {
        let mut pending = read_pending(&login_file(home), now);
        let hit = pending.iter().position(|login| {
            login
                .code
                .as_deref()
                .is_some_and(|code| constant_eq(code, &wanted))
        });
        if let Some(hit) = hit {
            pending.remove(hit);
            write_pending(&login_file(home), &pending)?;
            return mint_owner(home, now).map(CodeLogin::Owner);
        }
        let mut burned = false;
        for login in pending.iter_mut().filter(|login| login.code.is_some()) {
            login.misses += 1;
            if login.misses >= CODE_MISSES {
                login.code = None;
                burned = true;
            }
        }
        write_pending(&login_file(home), &pending)?;
        Ok(if burned {
            CodeLogin::Burned
        } else {
            CodeLogin::Wrong
        })
    })
}

/// Signs one browser out: its cookie is never owner again. False when it
/// was not a live owner cookie.
pub fn sign_out(home: &Path, cookie: &str, now: Timestamp) -> Result<bool, AuthError> {
    let wanted = hash(cookie);
    locked(home, || {
        let mut owners = read_lines(&owner_file(home), now);
        let before = owners.len();
        owners.retain(|(hash, _)| !constant_eq(hash, &wanted));
        if owners.len() == before {
            return Ok(false);
        }
        write_lines(&owner_file(home), &owners)?;
        Ok(true)
    })
}

/// Whether `cookie` is a minted, unexpired owner cookie.
pub fn is_owner(home: &Path, cookie: &str, now: Timestamp) -> bool {
    let wanted = hash(cookie);
    read_lines(&owner_file(home), now)
        .iter()
        .any(|(hash, _)| constant_eq(hash, &wanted))
}

/// The `Set-Cookie` value for a fresh cookie. `secure` marks it `Secure`,
/// for a board reached over https through a proxy.
pub fn set_cookie(value: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_SECONDS}{secure}")
}

/// The `Set-Cookie` value that removes the cookie from the browser.
pub fn clear_cookie(secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}")
}

/// The daemon's own side of signing in, in memory: the limit on code tries
/// and the signal that ends a signed-out browser's open streams.
pub struct Sessions {
    tries: Mutex<VecDeque<Instant>>,
    signed_out: watch::Sender<()>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            tries: Mutex::default(),
            signed_out: watch::channel(()).0,
        }
    }
}

impl Sessions {
    /// Takes one code try, or refuses it when the last minute already had
    /// [`CODE_TRIES_PER_MINUTE`].
    pub fn try_code(&self, now: Instant) -> bool {
        const WINDOW: Duration = Duration::from_secs(60);
        let mut tries = self.tries.lock().unwrap_or_else(PoisonError::into_inner);
        while tries
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW)
        {
            tries.pop_front();
        }
        if tries.len() >= CODE_TRIES_PER_MINUTE {
            return false;
        }
        tries.push_back(now);
        true
    }

    /// Tells every open stream a browser signed out.
    pub fn signed_out(&self) {
        self.signed_out.send_replace(());
    }

    /// Changes each time a browser signs out.
    pub fn sign_outs(&self) -> watch::Receiver<()> {
        self.signed_out.subscribe()
    }
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
        let token = mint_login(home.path(), now).unwrap().token;
        let redeems: Vec<_> = (0..8)
            .map(|_| {
                let (home, token) = (home.path().to_owned(), token.clone());
                std::thread::spawn(move || redeem_login(&home, &token, now).unwrap())
            })
            .collect();
        let minted: Vec<_> = (0..4)
            .map(|_| {
                let home = home.path().to_owned();
                std::thread::spawn(move || mint_login(&home, now).unwrap().token)
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
        let token = mint_login(home.path(), now()).unwrap().token;
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
        // A link lives one minute: good at 59 seconds, gone at 60.
        let token = mint_login(home.path(), now()).unwrap().token;
        assert_eq!(
            redeem_login(home.path(), &token, later(now(), 60)).unwrap(),
            None
        );
        let token = mint_login(home.path(), now()).unwrap().token;
        assert!(
            redeem_login(home.path(), &token, later(now(), 59))
                .unwrap()
                .is_some()
        );

        let token = mint_login(home.path(), now()).unwrap().token;
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

    /// A code no pending login has.
    fn other_than(login: &Login) -> String {
        let wrong = (login.code.parse::<u32>().unwrap() + 1) % 100_000_000;
        format!("{wrong:08}")
    }

    #[test]
    fn a_code_is_eight_digits_and_signs_in_once_like_its_link() {
        let home = tempfile::tempdir().unwrap();
        let login = mint_login(home.path(), now()).unwrap();
        assert_eq!(login.code.len(), 8);
        assert!(login.code.bytes().all(|b| b.is_ascii_digit()));
        let CodeLogin::Owner(cookie) = redeem_code(home.path(), &login.code, now()).unwrap() else {
            panic!("the code did not sign in");
        };
        assert!(is_owner(home.path(), &cookie, now()));
        // The code and its link are one login: both are spent.
        assert_eq!(
            redeem_code(home.path(), &login.code, now()).unwrap(),
            CodeLogin::Wrong
        );
        assert_eq!(
            redeem_login(home.path(), &login.token, now()).unwrap(),
            None
        );
        // And the link spends the code.
        let login = mint_login(home.path(), now()).unwrap();
        assert!(
            redeem_login(home.path(), &login.token, now())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            redeem_code(home.path(), &login.code, now()).unwrap(),
            CodeLogin::Wrong
        );
        // The code is stored only as a hash.
        let login = mint_login(home.path(), now()).unwrap();
        let text = std::fs::read_to_string(home.path().join("board-login")).unwrap();
        assert!(!text.contains(&login.code));
    }

    #[test]
    fn a_code_expires_with_its_link() {
        let home = tempfile::tempdir().unwrap();
        let login = mint_login(home.path(), now()).unwrap();
        assert_eq!(
            redeem_code(home.path(), &login.code, later(now(), 60)).unwrap(),
            CodeLogin::Wrong
        );
        let login = mint_login(home.path(), now()).unwrap();
        assert!(matches!(
            redeem_code(home.path(), &login.code, later(now(), 59)).unwrap(),
            CodeLogin::Owner(_)
        ));
    }

    #[test]
    fn wrong_codes_burn_the_code_but_not_its_link() {
        let home = tempfile::tempdir().unwrap();
        let login = mint_login(home.path(), now()).unwrap();
        let wrong = other_than(&login);
        for _ in 1..CODE_MISSES {
            assert_eq!(
                redeem_code(home.path(), &wrong, now()).unwrap(),
                CodeLogin::Wrong
            );
        }
        assert_eq!(
            redeem_code(home.path(), &wrong, now()).unwrap(),
            CodeLogin::Burned
        );
        // The right code is now refused too; the link still works.
        assert_eq!(
            redeem_code(home.path(), &login.code, now()).unwrap(),
            CodeLogin::Wrong
        );
        assert!(
            redeem_login(home.path(), &login.token, now())
                .unwrap()
                .is_some()
        );
        // A login minted after the misses starts afresh.
        let fresh = mint_login(home.path(), now()).unwrap();
        assert_eq!(
            redeem_code(home.path(), &other_than(&fresh), now()).unwrap(),
            CodeLogin::Wrong
        );
        assert!(matches!(
            redeem_code(home.path(), &fresh.code, now()).unwrap(),
            CodeLogin::Owner(_)
        ));
    }

    #[test]
    fn a_login_file_from_before_codes_still_redeems_its_link() {
        let home = tempfile::tempdir().unwrap();
        let token = "a".repeat(64);
        std::fs::write(
            home.path().join("board-login"),
            format!("{} {}\n", hash(&token), later(now(), 30)),
        )
        .unwrap();
        assert!(redeem_login(home.path(), &token, now()).unwrap().is_some());
    }

    #[test]
    fn signing_out_ends_only_that_cookie() {
        let home = tempfile::tempdir().unwrap();
        let mint = || {
            let token = mint_login(home.path(), now()).unwrap().token;
            redeem_login(home.path(), &token, now()).unwrap().unwrap()
        };
        let (phone, laptop) = (mint(), mint());
        assert!(sign_out(home.path(), &phone, now()).unwrap());
        assert!(!is_owner(home.path(), &phone, now()));
        assert!(is_owner(home.path(), &laptop, now()));
        assert!(!sign_out(home.path(), &phone, now()).unwrap());
        assert!(!sign_out(home.path(), "forged", now()).unwrap());
    }

    #[test]
    fn code_tries_are_limited_per_minute() {
        let sessions = Sessions::default();
        let start = Instant::now();
        for _ in 0..CODE_TRIES_PER_MINUTE {
            assert!(sessions.try_code(start));
        }
        assert!(!sessions.try_code(start + Duration::from_secs(59)));
        assert!(sessions.try_code(start + Duration::from_secs(60)));
    }

    #[test]
    fn the_cleared_cookie_matches_the_set_one() {
        assert_eq!(
            clear_cookie(true),
            "tiller_owner=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0; Secure"
        );
        assert!(!clear_cookie(false).contains("Secure"));
    }

    #[test]
    fn the_cookie_is_secure_only_when_asked() {
        assert!(!set_cookie("abc", false).contains("Secure"));
        assert!(set_cookie("abc", true).ends_with("; Secure"));
        assert!(set_cookie("abc", true).contains("HttpOnly; SameSite=Strict; Path=/"));
    }

    #[test]
    fn the_cookie_is_read_from_among_others() {
        assert_eq!(cookie_value("a=1; tiller_owner=abc; b=2"), Some("abc"));
        assert_eq!(cookie_value("tiller_owner_x=abc"), None);
    }
}
