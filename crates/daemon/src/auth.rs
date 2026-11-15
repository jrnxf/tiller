//! The board API credential (`protocol.md` 7.1, `authority.md` "Board API
//! credential"): a one-time login, minted by `tiller board --login` as a
//! link and an 8-digit code, and the owner session either starts. A
//! session is two secrets, and every owner request carries both: the
//! `tiller_owner` cookie, and the owner token the board keeps in its
//! origin's `localStorage` and sends as [`TOKEN_HEADER`]. A cookie is sent
//! to every port of its host, so a server on another loopback port can be
//! handed it; it cannot read that origin's `localStorage`, so the cookie
//! alone is never the owner.
//!
//! Only SHA-256 hashes are stored, under `~/.tiller`, which every fleet role
//! is denied. `board-login` holds pending logins (one minute each), one per
//! line as `<token hash> <expiry RFC 3339> <code hash or -> <wrong codes>`;
//! `board-owner` holds the sessions (30 days each), one per line as
//! `<cookie hash> <expiry RFC 3339> <owner token hash>`. A line from before
//! owner tokens has no third field and is no session: that browser signs
//! in again.
//!
//! The board may be public, so anyone can type codes. The limits on them
//! count per [`Client`], and for IPv6 per network as well, so a stranger's
//! wrong codes spend only the stranger's own tries and never stop the
//! owner's code from working; the one limit shared by every client is
//! [`CODE_MISS_CEILING`], high enough that only a guesser spread over a
//! hundred networks or more can reach it.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tiller_core::Timestamp;
use tokio::sync::watch;

/// The cookie's name.
pub const COOKIE: &str = "tiller_owner";
/// The request header that carries the owner token.
pub const TOKEN_HEADER: &str = "x-tiller-owner";
/// How long a cookie lives: 30 days (CP2, C15).
pub const COOKIE_SECONDS: i64 = 2_592_000;
/// How long a login link stays usable: one minute, since it may be shown as
/// a QR code on a screen others can see.
pub const LOGIN_MINUTES: i64 = 1;
/// Wrong codes one client may try while a code is pending: on its third
/// the code burns for that client, and works for every other client still.
/// Its link works for everyone.
pub const CODE_MISSES: u32 = 3;
/// Wrong codes the IPv6 clients of one network may try while a code is
/// pending, together: on the tenth the code burns for all of them. One
/// subscriber can hold a /48, 65,536 clients, for free.
pub const NETWORK_MISSES: u32 = 10;
/// Wrong codes from every client together that burn a pending code for
/// everyone, its link aside. A code is one of 10^8 and lives a minute, so
/// the chance that guessing finds it is at most 1000 in 10^8, 1 in 100,000,
/// however many addresses the guesser has; and since each client has only
/// [`CODE_MISSES`] and each network [`NETWORK_MISSES`], denying the owner
/// the code takes 334 IPv4 addresses or 100 IPv6 networks guessing within
/// that minute.
pub const CODE_MISS_CEILING: u32 = 1000;
/// Code tries one client may make in any one minute.
pub const CODE_TRIES_PER_MINUTE: usize = 10;
/// Code tries the IPv6 clients of one network may make in any one minute,
/// together.
pub const NETWORK_TRIES_PER_MINUTE: usize = 30;

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

/// The unexpired sessions in one `board-owner`. A line with no owner
/// token hash, from before owner tokens, is left out.
fn read_sessions(path: &Path, now: Timestamp) -> Vec<Session> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let (cookie, expiry, token) = (fields.next()?, fields.next()?, fields.next()?);
            let expiry: Timestamp = expiry.trim().parse().ok()?;
            (expiry > now && !token.is_empty()).then(|| Session {
                cookie: cookie.to_owned(),
                expiry,
                token: token.trim().to_owned(),
            })
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

fn write_sessions(path: &Path, sessions: &[Session]) -> Result<(), AuthError> {
    write_file(
        path,
        sessions
            .iter()
            .map(|session| format!("{} {} {}", session.cookie, session.expiry, session.token)),
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

/// One owner session as `board-owner` holds it: the hashes of its two
/// secrets and when it ends.
#[derive(Clone, Debug)]
struct Session {
    cookie: String,
    expiry: Timestamp,
    token: String,
}

/// One signed-in browser's two secrets: the cookie the daemon sets, and
/// the owner token the board keeps in `localStorage`. They are minted
/// together, stored as one line, and end together; a cookie and a token
/// from two sign-ins are no owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub cookie: String,
    pub token: String,
}

/// Whether `owner` is an unexpired session of `sessions`: its cookie and
/// its token on one line. Every owner check answers through this one match.
fn live(sessions: &[Session], owner: &Owner, now: Timestamp) -> bool {
    let (cookie, token) = (hash(&owner.cookie), hash(&owner.token));
    sessions.iter().any(|session| {
        // Both compared, always: no early way out tells which one missed.
        let pair = constant_eq(&session.cookie, &cookie) & constant_eq(&session.token, &token);
        pair && session.expiry > now
    })
}

/// How often a kept owner file is looked at again, for a change this
/// process did not make: a sessions file cleared by hand revokes them
/// within this.
pub const OWNER_FILE_RECHECK: Duration = Duration::from_secs(1);

/// What identifies one version of a file: a write (an atomic rename) or
/// an edit by hand changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    inode: u64,
}

/// The file's [`Stamp`], or `None` when it is missing.
fn stamp(path: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;

    let meta = std::fs::metadata(path).ok()?;
    Some(Stamp {
        modified: meta.modified().ok(),
        len: meta.len(),
        inode: meta.ino(),
    })
}

/// One home's sessions as this process last read or wrote them.
struct Kept {
    sessions: Vec<Session>,
    stamp: Option<Stamp>,
    checked: Instant,
}

/// The sessions of each home. The daemon writes `board-owner` only through
/// [`write_owners`], which keeps the cache with it, so a check answers
/// from memory. Anything else may change the file too (the owner clearing
/// it by hand), so a kept copy older than [`OWNER_FILE_RECHECK`] is looked
/// at again ([`is_owner`], on the blocking pool) and read again when its
/// [`Stamp`] changed. Expiry needs no write: each check compares against
/// its own `now`.
static OWNERS: LazyLock<Mutex<HashMap<PathBuf, Kept>>> = LazyLock::new(Mutex::default);

fn owners() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Kept>> {
    OWNERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Rewrites `board-owner` and the cache with it. The caller holds the lock,
/// so a load ([`is_owner`]) never interleaves with it.
fn write_owners(home: &Path, sessions: Vec<Session>) -> Result<(), AuthError> {
    let path = owner_file(home);
    write_sessions(&path, &sessions)?;
    let kept = Kept {
        sessions,
        stamp: stamp(&path),
        checked: Instant::now(),
    };
    owners().insert(home.to_owned(), kept);
    Ok(())
}

/// Records a new session and returns its two secrets. The caller holds
/// the lock.
fn mint_owner(home: &Path, now: Timestamp) -> Result<Owner, AuthError> {
    let owner = Owner {
        cookie: secret()?,
        token: secret()?,
    };
    let mut sessions = read_sessions(&owner_file(home), now);
    sessions.push(Session {
        cookie: hash(&owner.cookie),
        expiry: later(now, COOKIE_SECONDS),
        token: hash(&owner.token),
    });
    write_owners(home, sessions)?;
    Ok(owner)
}

/// Spends a login by its link's token: a session once for a valid,
/// unexpired token, then never again. Its code goes with it.
pub fn redeem_login(home: &Path, token: &str, now: Timestamp) -> Result<Option<Owner>, AuthError> {
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
    /// It matched a pending login, now spent: the new session.
    Owner(Owner),
    /// It matched nothing this client may still use: wrong, expired, used
    /// or burned.
    Wrong,
    /// It matched nothing, and that miss burned at least one pending code
    /// for this client.
    Burned,
    /// The client had its [`CODE_TRIES_PER_MINUTE`], or its network its
    /// [`NETWORK_TRIES_PER_MINUTE`]; nothing was checked.
    TooManyTries,
}

/// Who typed a code, as the code limits count it: the browser's address,
/// with an IPv6 address cut to its /64, the block one device usually
/// holds, so rotating through it buys no extra tries. An IPv6 client also
/// counts against its network, the /48 around it, which one subscriber may
/// hold whole. No address at all is [`Client::UNKNOWN`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Client(Option<IpAddr>);

impl Client {
    /// Every browser whose address the daemon could not read, as one client.
    pub const UNKNOWN: Self = Self(None);

    pub fn new(address: IpAddr) -> Self {
        match address {
            IpAddr::V4(_) => Self(Some(address)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self(Some(IpAddr::V4(v4))),
                None => Self(Some(IpAddr::V6(
                    (u128::from(v6) & (u128::MAX << 64)).into(),
                ))),
            },
        }
    }

    /// What `self`'s tries count against, each with its tries a minute and
    /// its wrong codes a pending code: itself, and an IPv6 client's network.
    fn scopes(self) -> impl Iterator<Item = (Scope, usize, u32)> {
        let network = match self.0 {
            None | Some(IpAddr::V4(_)) => None,
            Some(IpAddr::V6(v6)) => Some((
                Scope::Network((u128::from(v6) & (u128::MAX << 80)).into()),
                NETWORK_TRIES_PER_MINUTE,
                NETWORK_MISSES,
            )),
        };
        std::iter::once((Scope::Client(self), CODE_TRIES_PER_MINUTE, CODE_MISSES)).chain(network)
    }
}

/// What a code limit counts: one client, or the IPv6 /48 of many.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Scope {
    Client(Client),
    Network(Ipv6Addr),
}

/// Signs one browser out: its session, named by its cookie, is never
/// owner again. False when it was not a live session.
pub fn sign_out(home: &Path, cookie: &str, now: Timestamp) -> Result<bool, AuthError> {
    let wanted = hash(cookie);
    locked(home, || {
        let mut sessions = read_sessions(&owner_file(home), now);
        let before = sessions.len();
        sessions.retain(|session| !constant_eq(&session.cookie, &wanted));
        if sessions.len() == before {
            return Ok(false);
        }
        write_owners(home, sessions)?;
        Ok(true)
    })
}

/// Whether `owner` is a live session, answered from memory: `None` when
/// this process has not read or written `home`'s owner file within
/// [`OWNER_FILE_RECHECK`]. It never touches the disk, so the runtime
/// thread may call it.
pub fn cached_owner(home: &Path, owner: &Owner, now: Timestamp) -> Option<bool> {
    let owners = owners();
    let kept = owners.get(home)?;
    (kept.checked.elapsed() < OWNER_FILE_RECHECK).then(|| live(&kept.sessions, owner, now))
}

/// Whether `owner` is a live session. A copy kept within
/// [`OWNER_FILE_RECHECK`] answers ([`cached_owner`]); an older one is
/// kept if the file's [`Stamp`] did not change, and otherwise the file is
/// read again, under the lock so no write interleaves. It touches the
/// disk, so call it on the blocking pool.
pub fn is_owner(home: &Path, owner: &Owner, now: Timestamp) -> bool {
    if let Some(answer) = cached_owner(home, owner, now) {
        return answer;
    }
    let path = owner_file(home);
    let current = stamp(&path);
    if let Some(kept) = owners().get_mut(home)
        && kept.stamp == current
    {
        kept.checked = Instant::now();
        return live(&kept.sessions, owner, now);
    }
    let loaded = locked(home, || {
        let kept = Kept {
            stamp: stamp(&path),
            sessions: read_sessions(&path, now),
            checked: Instant::now(),
        };
        let answer = live(&kept.sessions, owner, now);
        owners().insert(home.to_owned(), kept);
        Ok(answer)
    });
    // No lock, so nothing is kept: the file read alone answers.
    loaded.unwrap_or_else(|_| live(&read_sessions(&path, now), owner, now))
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

/// The daemon's own side of signing in, in memory: the limits on code tries
/// and the signal that ends a signed-out browser's open streams.
pub struct Sessions {
    codes: Mutex<Tries>,
    signed_out: watch::Sender<()>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            codes: Mutex::default(),
            signed_out: watch::channel(()).0,
        }
    }
}

/// The code tries each client and network made. In memory only: a restart
/// forgets them, which gives a guesser at most [`CODE_MISSES`] more against
/// a code from each client, and [`NETWORK_MISSES`] from each network, while [`CODE_MISS_CEILING`] is counted on disk.
#[derive(Default)]
struct Tries {
    /// Each scope's tries in the last minute.
    recent: HashMap<Scope, VecDeque<Instant>>,
    /// The wrong codes against each pending login, by the login's token
    /// hash: when it expires, and each scope's count, kept until then.
    misses: HashMap<String, (Timestamp, HashMap<Scope, u32>)>,
    /// When entries of clients long gone were last dropped.
    swept: Option<Instant>,
}

/// How far back a client's tries count.
const WINDOW: Duration = Duration::from_secs(60);

impl Tries {
    /// Takes one try for `client`, or refuses it when the last minute already
    /// had [`CODE_TRIES_PER_MINUTE`] of its own or its network had
    /// [`NETWORK_TRIES_PER_MINUTE`].
    fn take(&mut self, client: Client, at: Instant) -> bool {
        let full = client.scopes().any(|(scope, limit, _)| {
            let tries = self.recent.entry(scope).or_default();
            while tries
                .front()
                .is_some_and(|try_at| at.saturating_duration_since(*try_at) >= WINDOW)
            {
                tries.pop_front();
            }
            tries.len() >= limit
        });
        if full {
            return false;
        }
        for (scope, _, _) in client.scopes() {
            self.recent.entry(scope).or_default().push_back(at);
        }
        true
    }

    /// Whether `client` may still try the pending login `token`'s code: it
    /// burns for `client` once `client` or its network had all its misses.
    fn open(&self, client: Client, token: &str) -> bool {
        let counts = self.misses.get(token).map(|(_, counts)| counts);
        client.scopes().all(|(scope, _, limit)| {
            counts
                .and_then(|counts| counts.get(&scope))
                .map_or(0, |misses| *misses)
                < limit
        })
    }

    /// Counts one more wrong code by `client` against `login`; true when that
    /// was the one that burns it for `client`.
    fn miss(&mut self, client: Client, login: &Pending) -> bool {
        let (_, counts) = self
            .misses
            .entry(login.token.clone())
            .or_insert_with(|| (login.expiry, HashMap::new()));
        let mut burned = false;
        for (scope, _, limit) in client.scopes() {
            let misses = counts.entry(scope).or_default();
            *misses += 1;
            burned |= *misses >= limit;
        }
        burned
    }

    /// Drops what no longer counts, at most once a second, so a flood from
    /// many addresses costs memory only for its last minute.
    fn sweep(&mut self, now: Timestamp, at: Instant) {
        if self
            .swept
            .is_some_and(|swept| at.saturating_duration_since(swept) < Duration::from_secs(1))
        {
            return;
        }
        self.swept = Some(at);
        self.recent.retain(|_, tries| {
            tries
                .back()
                .is_some_and(|try_at| at.saturating_duration_since(*try_at) < WINDOW)
        });
        self.misses.retain(|_, (expiry, _)| *expiry > now);
    }
}

impl Sessions {
    /// Spends a login by its code, as [`redeem_login`] does by its token,
    /// for `client`. A wrong code counts against every pending code that
    /// `client` may still use, since it may have been meant for any of them:
    /// each burns for `client` on its [`CODE_MISSES`]th from `client` or its
    /// [`NETWORK_MISSES`]th from `client`'s network, and for everyone on its
    /// [`CODE_MISS_CEILING`]th from all clients. Only
    /// [`CODE_TRIES_PER_MINUTE`] tries a minute are checked per client, and
    /// [`NETWORK_TRIES_PER_MINUTE`] per network.
    pub fn redeem_code(
        &self,
        home: &Path,
        client: Client,
        code: &str,
        now: Timestamp,
        at: Instant,
    ) -> Result<CodeLogin, AuthError> {
        let mut tries = self.codes.lock().unwrap_or_else(PoisonError::into_inner);
        tries.sweep(now, at);
        if !tries.take(client, at) {
            return Ok(CodeLogin::TooManyTries);
        }
        let wanted = hash(code);
        locked(home, || {
            let mut pending = read_pending(&login_file(home), now);
            // The pending codes `client` may still try.
            let open: Vec<usize> = (0..pending.len())
                .filter(|&index| {
                    pending[index].code.is_some() && tries.open(client, &pending[index].token)
                })
                .collect();
            // Nothing to count, so nothing to write: a flood with no code
            // pending only reads.
            if open.is_empty() {
                return Ok(CodeLogin::Wrong);
            }
            let hit = open.iter().copied().find(|&index| {
                pending[index]
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
            for index in open {
                let login = &mut pending[index];
                login.misses += 1;
                burned |= tries.miss(client, login);
                if login.misses >= CODE_MISS_CEILING {
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

/// Records `owner` as a live session, so the API tests can sign their
/// reads in with values they know.
#[cfg(test)]
pub(crate) fn register_owner(home: &Path, owner: &Owner, now: Timestamp) -> Result<(), AuthError> {
    locked(home, || {
        let mut sessions = read_sessions(&owner_file(home), now);
        sessions.push(Session {
            cookie: hash(&owner.cookie),
            expiry: later(now, COOKIE_SECONDS),
            token: hash(&owner.token),
        });
        write_owners(home, sessions)
    })
}

/// Drops what this process keeps of `home`'s owner file, so the next
/// check reads it again.
#[cfg(test)]
pub(crate) fn forget_owners(home: &Path) {
    owners().remove(home);
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
        let cookies: Vec<Owner> = redeems
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
        let owner = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        assert!(is_owner(home.path(), &owner, now()));
        let link = Owner {
            cookie: token.clone(),
            token: owner.token.clone(),
        };
        assert!(!is_owner(home.path(), &link, now()));
        assert_eq!(redeem_login(home.path(), &token, now()).unwrap(), None);
        // No secret is on disk, only hashes.
        for file in ["board-login", "board-owner"] {
            let text = std::fs::read_to_string(home.path().join(file)).unwrap();
            for secret in [&token, &owner.cookie, &owner.token] {
                assert!(!text.contains(secret.as_str()), "{file}");
            }
        }
    }

    #[test]
    fn a_session_needs_its_cookie_and_its_own_token() {
        let home = tempfile::tempdir().unwrap();
        let mint = || {
            let token = mint_login(home.path(), now()).unwrap().token;
            redeem_login(home.path(), &token, now()).unwrap().unwrap()
        };
        let (phone, laptop) = (mint(), mint());
        assert!(is_owner(home.path(), &phone, now()));
        assert!(is_owner(home.path(), &laptop, now()));
        let pair = |cookie: &str, token: &str| Owner {
            cookie: cookie.to_owned(),
            token: token.to_owned(),
        };
        // A cookie with no token, a token with no cookie, and a cookie and
        // token of two sign-ins are no owner.
        assert!(!is_owner(home.path(), &pair(&phone.cookie, ""), now()));
        assert!(!is_owner(home.path(), &pair("", &phone.token), now()));
        assert!(!is_owner(
            home.path(),
            &pair(&phone.cookie, &laptop.token),
            now()
        ));
        assert!(!is_owner(
            home.path(),
            &pair(&laptop.cookie, &phone.token),
            now()
        ));
        // The cookie's line goes, and its token with it.
        assert!(sign_out(home.path(), &phone.cookie, now()).unwrap());
        assert!(!is_owner(home.path(), &phone, now()));
        assert!(is_owner(home.path(), &laptop, now()));
    }

    #[test]
    fn a_cookie_from_before_owner_tokens_is_signed_out() {
        let home = tempfile::tempdir().unwrap();
        let old = Owner {
            cookie: "old-cookie".to_owned(),
            token: String::new(),
        };
        let line = format!("{} {}\n", hash(&old.cookie), later(now(), COOKIE_SECONDS));
        std::fs::write(owner_file(home.path()), line).unwrap();
        assert!(!is_owner(home.path(), &old, now()));
        let guessed = Owner {
            token: "anything".to_owned(),
            ..old
        };
        assert!(!is_owner(home.path(), &guessed, now()));
        // Signing in again works beside it.
        let token = mint_login(home.path(), now()).unwrap().token;
        let fresh = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        assert!(is_owner(home.path(), &fresh, now()));
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
        let owner = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        assert!(is_owner(
            home.path(),
            &owner,
            later(now(), COOKIE_SECONDS - 1)
        ));
        assert!(!is_owner(home.path(), &owner, later(now(), COOKIE_SECONDS)));
    }

    /// The owner's browser, at its own address.
    fn owner() -> Client {
        Client::new("192.0.2.10".parse().unwrap())
    }

    /// A stranger's browser: the board is public, so anyone can type codes.
    fn stranger(n: u8) -> Client {
        Client::new(IpAddr::from([198, 51, 100, n]))
    }

    /// The owner types `code` at `now`.
    fn typed(sessions: &Sessions, home: &Path, code: &str, now: Timestamp) -> CodeLogin {
        typed_by(sessions, home, owner(), code, now)
    }

    /// `client` types `code` at `now`.
    fn typed_by(
        sessions: &Sessions,
        home: &Path,
        client: Client,
        code: &str,
        now: Timestamp,
    ) -> CodeLogin {
        sessions
            .redeem_code(home, client, code, now, Instant::now())
            .unwrap()
    }

    /// A code no pending login has.
    fn other_than(login: &Login) -> String {
        let wrong = (login.code.parse::<u32>().unwrap() + 1) % 100_000_000;
        format!("{wrong:08}")
    }

    #[test]
    fn a_code_is_eight_digits_and_signs_in_once_like_its_link() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Sessions::default();
        let login = mint_login(home.path(), now()).unwrap();
        assert_eq!(login.code.len(), 8);
        assert!(login.code.bytes().all(|b| b.is_ascii_digit()));
        let CodeLogin::Owner(owner) = typed(&sessions, home.path(), &login.code, now()) else {
            panic!("the code did not sign in");
        };
        assert!(is_owner(home.path(), &owner, now()));
        // The code and its link are one login: both are spent.
        assert_eq!(
            typed(&sessions, home.path(), &login.code, now()),
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
            typed(&sessions, home.path(), &login.code, now()),
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
        let sessions = Sessions::default();
        let login = mint_login(home.path(), now()).unwrap();
        assert_eq!(
            typed(&sessions, home.path(), &login.code, later(now(), 60)),
            CodeLogin::Wrong
        );
        let login = mint_login(home.path(), now()).unwrap();
        assert!(matches!(
            typed(&sessions, home.path(), &login.code, later(now(), 59)),
            CodeLogin::Owner(_)
        ));
    }

    #[test]
    fn wrong_codes_burn_the_code_but_not_its_link() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Sessions::default();
        let login = mint_login(home.path(), now()).unwrap();
        let wrong = other_than(&login);
        for _ in 1..CODE_MISSES {
            assert_eq!(
                typed(&sessions, home.path(), &wrong, now()),
                CodeLogin::Wrong
            );
        }
        assert_eq!(
            typed(&sessions, home.path(), &wrong, now()),
            CodeLogin::Burned
        );
        // The right code is now refused too; the link still works.
        assert_eq!(
            typed(&sessions, home.path(), &login.code, now()),
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
            typed(&sessions, home.path(), &other_than(&fresh), now()),
            CodeLogin::Wrong
        );
        assert!(matches!(
            typed(&sessions, home.path(), &fresh.code, now()),
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
    fn the_owner_file_is_kept_with_every_write_and_read_again_when_it_changes() {
        let home = tempfile::tempdir().unwrap();
        let token = mint_login(home.path(), now()).unwrap().token;
        let laptop = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        assert_eq!(cached_owner(home.path(), &laptop, now()), Some(true));
        // A sign-out writes the file and the cache together.
        assert!(sign_out(home.path(), &laptop.cookie, now()).unwrap());
        assert_eq!(cached_owner(home.path(), &laptop, now()), Some(false));
        let token = mint_login(home.path(), now()).unwrap().token;
        let phone = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        // A fresh process reads the file and finds the same answers.
        forget_owners(home.path());
        assert_eq!(cached_owner(home.path(), &phone, now()), None);
        assert!(is_owner(home.path(), &phone, now()));
        assert!(!is_owner(home.path(), &laptop, now()));
        // Expiry needs no write.
        let later = now().plus_minutes(31 * 24 * 60);
        assert_eq!(cached_owner(home.path(), &phone, later), Some(false));
        // The owner clears the file by hand: within a recheck the copy kept
        // still answers, after one the file is read again.
        std::fs::write(owner_file(home.path()), "").unwrap();
        assert_eq!(cached_owner(home.path(), &phone, now()), Some(true));
        std::thread::sleep(OWNER_FILE_RECHECK);
        assert_eq!(cached_owner(home.path(), &phone, now()), None);
        assert!(!is_owner(home.path(), &phone, now()));
        assert_eq!(cached_owner(home.path(), &phone, now()), Some(false));
    }

    #[test]
    fn an_unchanged_owner_file_is_looked_at_but_not_read_again() {
        let home = tempfile::tempdir().unwrap();
        let token = mint_login(home.path(), now()).unwrap().token;
        let owner = redeem_login(home.path(), &token, now()).unwrap().unwrap();
        std::thread::sleep(OWNER_FILE_RECHECK);
        assert_eq!(cached_owner(home.path(), &owner, now()), None);
        // Unreadable but unchanged: the kept copy answers.
        let file = owner_file(home.path());
        let mode = |bits| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(bits)).unwrap();
        };
        mode(0o000);
        let answer = is_owner(home.path(), &owner, now());
        mode(0o600);
        assert!(answer);
        assert_eq!(cached_owner(home.path(), &owner, now()), Some(true));
    }

    #[test]
    fn signing_out_ends_only_that_cookie() {
        let home = tempfile::tempdir().unwrap();
        let mint = || {
            let token = mint_login(home.path(), now()).unwrap().token;
            redeem_login(home.path(), &token, now()).unwrap().unwrap()
        };
        let (phone, laptop) = (mint(), mint());
        assert!(sign_out(home.path(), &phone.cookie, now()).unwrap());
        assert!(!is_owner(home.path(), &phone, now()));
        assert!(is_owner(home.path(), &laptop, now()));
        assert!(!sign_out(home.path(), &phone.cookie, now()).unwrap());
        assert!(!sign_out(home.path(), "forged", now()).unwrap());
    }

    #[test]
    fn a_guessers_wrong_codes_burn_the_code_only_for_the_guesser() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Sessions::default();
        let login = mint_login(home.path(), now()).unwrap();
        let wrong = other_than(&login);
        for _ in 1..CODE_MISSES {
            let miss = typed_by(&sessions, home.path(), stranger(7), &wrong, now());
            assert_eq!(miss, CodeLogin::Wrong);
        }
        let burned = typed_by(&sessions, home.path(), stranger(7), &wrong, now());
        assert_eq!(burned, CodeLogin::Burned);
        // Even the right code no longer works for the guesser.
        let right = typed_by(&sessions, home.path(), stranger(7), &login.code, now());
        assert_eq!(right, CodeLogin::Wrong);
        // A code minted after its misses is open to it again, three tries.
        let fresh = mint_login(home.path(), now()).unwrap();
        let miss = typed_by(
            &sessions,
            home.path(),
            stranger(7),
            &other_than(&fresh),
            now(),
        );
        assert_eq!(miss, CodeLogin::Wrong);
        // The owner's codes still work, the burned one and the fresh one.
        for code in [&login.code, &fresh.code] {
            assert!(matches!(
                typed(&sessions, home.path(), code, now()),
                CodeLogin::Owner(_)
            ));
        }
    }

    #[test]
    fn wrong_codes_from_many_clients_burn_the_code_for_everyone_at_the_ceiling() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Sessions::default();
        // Two pending codes, and every miss counts against both.
        let first = mint_login(home.path(), now()).unwrap();
        let second = mint_login(home.path(), now()).unwrap();
        let wrong = (0u32..)
            .map(|n| format!("{n:08}"))
            .find(|code| *code != first.code && *code != second.code)
            .unwrap();
        // Each address, a /64 of its own, spends all its misses until the
        // codes are one short of the ceiling.
        let mut left = CODE_MISS_CEILING - 1;
        for n in 0u16.. {
            if left == 0 {
                break;
            }
            let client = Client::new(IpAddr::from([0x2001, 0xdb8, n, 0, 0, 0, 0, 1]));
            for _ in 0..CODE_MISSES.min(left) {
                typed_by(&sessions, home.path(), client, &wrong, now());
                left -= 1;
            }
        }
        // One short of the ceiling, a code still signs the owner in.
        assert!(matches!(
            typed(&sessions, home.path(), &first.code, now()),
            CodeLogin::Owner(_)
        ));
        let last = typed_by(&sessions, home.path(), stranger(9), &wrong, now());
        assert_eq!(last, CodeLogin::Burned);
        assert_eq!(
            typed(&sessions, home.path(), &second.code, now()),
            CodeLogin::Wrong
        );
        // Its link still works, and a fresh code starts its own count.
        assert!(
            redeem_login(home.path(), &second.token, now())
                .unwrap()
                .is_some()
        );
        let fresh = mint_login(home.path(), now()).unwrap();
        assert!(matches!(
            typed(&sessions, home.path(), &fresh.code, now()),
            CodeLogin::Owner(_)
        ));
    }

    #[test]
    fn code_tries_are_limited_per_client_per_minute() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Sessions::default();
        let start = Instant::now();
        let try_at = |client, code: &str, at| {
            sessions
                .redeem_code(home.path(), client, code, now(), at)
                .unwrap()
        };
        for _ in 0..CODE_TRIES_PER_MINUTE {
            assert_eq!(try_at(stranger(7), "00000000", start), CodeLogin::Wrong);
        }
        let limited = try_at(stranger(7), "00000000", start + Duration::from_secs(59));
        assert_eq!(limited, CodeLogin::TooManyTries);
        // Past the limit even the right code goes unchecked, and is kept.
        let login = mint_login(home.path(), now()).unwrap();
        let limited = try_at(stranger(7), &login.code, start + Duration::from_secs(59));
        assert_eq!(limited, CodeLogin::TooManyTries);
        // Every other client still tries, the owner first.
        let signed = try_at(owner(), &login.code, start + Duration::from_secs(59));
        assert!(matches!(signed, CodeLogin::Owner(_)), "{signed:?}");
        // And the limited client again once its minute has passed.
        let again = try_at(stranger(7), "00000000", start + Duration::from_secs(60));
        assert_eq!(again, CodeLogin::Wrong);
    }

    #[test]
    fn tries_are_forgotten_once_they_no_longer_count() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Sessions::default();
        let start = Instant::now();
        let login = mint_login(home.path(), now()).unwrap();
        for n in 1..=100 {
            let miss =
                sessions.redeem_code(home.path(), stranger(n), &other_than(&login), now(), start);
            assert_eq!(miss.unwrap(), CodeLogin::Wrong);
        }
        let held = |sessions: &Sessions| {
            let tries = sessions.codes.lock().unwrap();
            let misses = tries.misses.values().map(|(_, counts)| counts.len());
            (tries.recent.len(), misses.sum::<usize>())
        };
        assert_eq!(held(&sessions), (100, 100));
        // A minute on, the login has expired and the tries are past their
        // window: the next try drops them all.
        let later_at = start + WINDOW;
        let miss =
            sessions.redeem_code(home.path(), owner(), "00000000", later(now(), 60), later_at);
        assert_eq!(miss.unwrap(), CodeLogin::Wrong);
        assert_eq!(held(&sessions), (1, 0));
    }

    #[test]
    fn the_clients_of_one_ipv6_network_share_its_limits() {
        let home = tempfile::tempdir().unwrap();
        let sessions = Sessions::default();
        let login = mint_login(home.path(), now()).unwrap();
        let wrong = other_than(&login);
        // Clients a /64 apart in one /48, each well under its own three.
        let neighbour = |n: u16| Client::new(IpAddr::from([0x2001, 0xdb8, 7, n, 0, 0, 0, 1]));
        for n in 1..NETWORK_MISSES {
            let miss = typed_by(&sessions, home.path(), neighbour(n as u16), &wrong, now());
            assert_eq!(miss, CodeLogin::Wrong);
        }
        let burned = typed_by(&sessions, home.path(), neighbour(100), &wrong, now());
        assert_eq!(burned, CodeLogin::Burned);
        // The code is burned for the whole network, even a client new to it.
        let right = typed_by(&sessions, home.path(), neighbour(200), &login.code, now());
        assert_eq!(right, CodeLogin::Wrong);
        // Another network still signs in with it.
        let elsewhere = Client::new(IpAddr::from([0x2001, 0xdb8, 8, 0, 0, 0, 0, 1]));
        let signed = typed_by(&sessions, home.path(), elsewhere, &login.code, now());
        assert!(matches!(signed, CodeLogin::Owner(_)), "{signed:?}");

        // The network's tries a minute are shared the same way.
        let start = Instant::now();
        let try_at = |client| {
            sessions
                .redeem_code(home.path(), client, "00000000", now(), start)
                .unwrap()
        };
        let other = |n: u16| Client::new(IpAddr::from([0x2001, 0xdb8, 9, n, 0, 0, 0, 1]));
        for n in 0..NETWORK_TRIES_PER_MINUTE {
            assert_eq!(try_at(other(n as u16)), CodeLogin::Wrong);
        }
        assert_eq!(try_at(other(1000)), CodeLogin::TooManyTries);
        assert_eq!(try_at(elsewhere), CodeLogin::Wrong);
    }

    #[test]
    fn a_client_is_an_ipv4_address_or_an_ipv6_64() {
        let client = |text: &str| Client::new(text.parse().unwrap());
        assert_eq!(
            client("2001:db8:1:2:aaaa::1"),
            client("2001:db8:1:2:bbbb::2")
        );
        assert_ne!(client("2001:db8:1:2::1"), client("2001:db8:1:3::1"));
        assert_eq!(client("::ffff:192.0.2.10"), client("192.0.2.10"));
        assert_ne!(client("192.0.2.10"), client("192.0.2.11"));
        for address in ["127.0.0.1", "::1", "::"] {
            assert_ne!(Client::UNKNOWN, client(address));
        }
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
