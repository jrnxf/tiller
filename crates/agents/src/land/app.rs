//! The daemon's merge App (`authority.md` section 6, "Enforced merge gate";
//! CP2, S03): the one GitHub identity a gated repo's ruleset lets merge or
//! move its default branch.
//!
//! The owner creates the App and stores two files in the App directory,
//! `<tiller home>/forge-app/`, which every fleet role is denied (G20):
//!
//! - [`APP_ID_FILE`] (`app-id`): the App's numeric id, as GitHub shows it on
//!   the App's settings page. A ruleset names the App by this id.
//! - [`KEY_FILE`] (`private-key.pem`): the private key GitHub generated for
//!   it, as downloaded (PKCS#1, or PKCS#8). A key file readable by group or
//!   others is refused.
//!
//! [`HostApp`] reads both on every call, so a key the owner adds, fixes or
//! removes takes effect without a daemon restart. With no key file it is
//! [`NoApp`]. Otherwise it signs a short-lived App JWT (RS256, through
//! `ring`), finds the repo's installation and mints an installation token
//! for that repo alone, with only the permissions a land needs.
//!
//! The key, the JWT and the token never reach an error, a log line or a
//! command line: errors name files and GitHub's status, never their content.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};

use super::forge::{AppToken, AppTokens, ForgeError, NoApp};

/// The App directory under the Tiller home.
pub const APP_DIR: &str = "forge-app";
/// The file holding the App's numeric id.
pub const APP_ID_FILE: &str = "app-id";
/// The file holding the App's private key.
pub const KEY_FILE: &str = "private-key.pem";
/// GitHub's REST API.
pub const GITHUB_API: &str = "https://api.github.com";

/// Why the App's key cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("{} is readable by group or others (mode {mode:03o}); run chmod 600 on it", path.display())]
    Exposed { path: PathBuf, mode: u32 },
    #[error("{}: {why}", path.display())]
    Unreadable { path: PathBuf, why: String },
}

/// The App's id and private key, loaded from the App directory.
pub struct AppKey {
    id: u64,
    pair: RsaKeyPair,
}

impl fmt::Debug for AppKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AppKey {{ id: {}, .. }}", self.id)
    }
}

impl AppKey {
    /// Loads the key from `dir`. `None` when there is no key file: the host
    /// has no App.
    pub fn load(dir: &Path) -> Result<Option<Self>, KeyError> {
        let key_path = dir.join(KEY_FILE);
        let meta = match std::fs::metadata(&key_path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(KeyError::Unreadable {
                    path: key_path,
                    why: error.to_string(),
                });
            }
        };
        if !meta.is_file() {
            return Err(KeyError::Unreadable {
                path: key_path,
                why: "not a file".into(),
            });
        }
        let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777;
        if mode & 0o077 != 0 {
            return Err(KeyError::Exposed {
                path: key_path,
                mode,
            });
        }
        let id_path = dir.join(APP_ID_FILE);
        let id = std::fs::read_to_string(&id_path)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                text.trim()
                    .parse::<u64>()
                    .map_err(|_| "not a numeric App id".to_owned())
            })
            .map_err(|why| KeyError::Unreadable { path: id_path, why })?;
        let unreadable = |why: &str| KeyError::Unreadable {
            path: key_path.clone(),
            why: why.to_owned(),
        };
        let text =
            std::fs::read_to_string(&key_path).map_err(|error| unreadable(&error.to_string()))?;
        let pair = parse_pem(&text).map_err(|why| unreadable(&why))?;
        Ok(Some(Self { id, pair }))
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// The App JWT GitHub accepts for `/app` calls: issued a minute in the
    /// past against clock drift, valid for nine minutes (GitHub's limit is
    /// ten).
    pub fn jwt(&self, now: u64) -> Result<String, String> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = json!({
            "iat": now.saturating_sub(60),
            "exp": now + 540,
            "iss": self.id.to_string(),
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let message = format!("{header}.{claims}");
        let mut signature = vec![0; self.pair.public().modulus_len()];
        self.pair
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                message.as_bytes(),
                &mut signature,
            )
            .map_err(|_| "the App JWT could not be signed".to_owned())?;
        Ok(format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }
}

/// An RSA private key in PEM: GitHub's PKCS#1 download, or PKCS#8.
fn parse_pem(text: &str) -> Result<RsaKeyPair, String> {
    let block = |label: &str| -> Option<Vec<u8>> {
        let begin = format!("-----BEGIN {label}-----");
        let end = format!("-----END {label}-----");
        let body = text.split_once(&begin)?.1.split_once(&end)?.0;
        let body: String = body.split_whitespace().collect();
        STANDARD.decode(body).ok()
    };
    if let Some(der) = block("RSA PRIVATE KEY") {
        return RsaKeyPair::from_der(&der)
            .map_err(|error| format!("not a usable RSA key: {error}"));
    }
    if let Some(der) = block("PRIVATE KEY") {
        return RsaKeyPair::from_pkcs8(&der)
            .map_err(|error| format!("not a usable RSA key: {error}"));
    }
    Err("not a PEM RSA private key".into())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The merge App of this host, read from the App directory on every call.
pub struct HostApp {
    dir: PathBuf,
    api: String,
    agent: ureq::Agent,
}

impl HostApp {
    /// The App under `home`, talking to GitHub.
    pub fn new(home: &Path) -> Self {
        Self::with_api(home.join(APP_DIR), GITHUB_API)
    }

    /// The App in `dir`, talking to `api` (a fake forge in tests).
    pub fn with_api(dir: PathBuf, api: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent("tiller")
            .build()
            .into();
        Self {
            dir,
            api: api.into(),
            agent,
        }
    }

    /// The key, or `None` when the host has no App.
    fn key(&self, repo: &str) -> Result<Option<AppKey>, ForgeError> {
        AppKey::load(&self.dir).map_err(|error| ForgeError::NoApp {
            repo: repo.into(),
            why: error.to_string(),
        })
    }

    fn refused(repo: &str, why: impl Into<String>) -> ForgeError {
        ForgeError::NoApp {
            repo: repo.into(),
            why: why.into(),
        }
    }

    /// One call as the App: `GET` without a body, `POST` with one. Returns
    /// the JSON reply of a 2xx answer, and otherwise GitHub's status (none
    /// when GitHub was not reached) and a message naming it.
    fn call(
        &self,
        jwt_or_token: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, (Option<u16>, String)> {
        let url = format!("{}/{path}", self.api.trim_end_matches('/'));
        let auth = format!("Bearer {jwt_or_token}");
        let reply = match body {
            None => self
                .agent
                .get(&url)
                .header("Authorization", &auth)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .call(),
            Some(body) => self
                .agent
                .post(&url)
                .header("Authorization", &auth)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("Content-Type", "application/json")
                .send(body.to_string()),
        };
        let mut reply = reply.map_err(|error| (None, format!("{path}: {error}")))?;
        let status = reply.status().as_u16();
        let text = reply
            .body_mut()
            .read_to_string()
            .map_err(|error| (Some(status), format!("{path}: {error}")))?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(value);
        }
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("no message");
        Err((
            Some(status),
            format!("GitHub answered {status} to {path}: {message}"),
        ))
    }

    /// The App's installation on `repo`. GitHub's 404 says it is not
    /// installed; any other failure (no answer, a 5xx, a rate limit) says
    /// nothing about it and is a [`ForgeError::Read`].
    fn installation(&self, key: &AppKey, repo: &str) -> Result<u64, ForgeError> {
        let jwt = key
            .jwt(unix_now())
            .map_err(|why| Self::refused(repo, why))?;
        let reply = self
            .call(&jwt, &format!("repos/{repo}/installation"), None)
            .map_err(|(status, why)| match status {
                Some(404) => Self::refused(repo, format!("the App is not installed: {why}")),
                _ => ForgeError::Read(format!("the App's installation was not found: {why}")),
            })?;
        reply
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| Self::refused(repo, "unreadable installation reply"))
    }
}

impl fmt::Debug for HostApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostApp")
            .field("dir", &self.dir)
            .field("api", &self.api)
            .finish_non_exhaustive()
    }
}

impl AppTokens for HostApp {
    /// An installation token scoped to `repo` alone, with contents, pull
    /// requests and workflows write: the merge call and, on an opted-in
    /// `direct-push` project, the default-branch push. GitHub refuses an App
    /// a merge or push that changes `.github/workflows/` without workflows
    /// write.
    fn mint(&self, repo: &str) -> Result<AppToken, ForgeError> {
        let Some(key) = self.key(repo)? else {
            return NoApp.mint(repo);
        };
        let installation = self.installation(&key, repo)?;
        let name = repo.split_once('/').map_or(repo, |(_, name)| name);
        let jwt = key
            .jwt(unix_now())
            .map_err(|why| Self::refused(repo, why))?;
        let reply = self
            .call(
                &jwt,
                &format!("app/installations/{installation}/access_tokens"),
                Some(&json!({
                    "repositories": [name],
                    "permissions": {
                        "contents": "write",
                        "pull_requests": "write",
                        "workflows": "write",
                    },
                })),
            )
            .map_err(|(_, why)| Self::refused(repo, format!("no token was minted: {why}")))?;
        reply
            .get("token")
            .and_then(Value::as_str)
            .map(AppToken::new)
            .ok_or_else(|| Self::refused(repo, "unreadable token reply"))
    }

    /// A key file that is there but not usable (exposed, half written, not
    /// a key) is a [`ForgeError::Read`]: the ruleset check leaves the gate as
    /// it is while the owner fixes it, and `tiller doctor` names it. Only a
    /// missing key means the host has no App.
    fn app_id(&self) -> Result<u64, ForgeError> {
        match AppKey::load(&self.dir) {
            Ok(Some(key)) => Ok(key.id()),
            Ok(None) => NoApp.app_id(),
            Err(error) => Err(ForgeError::Read(error.to_string())),
        }
    }

    fn installed(&self, repo: &str) -> Result<(), ForgeError> {
        let Some(key) = self.key(repo)? else {
            return NoApp.installed(repo);
        };
        self.installation(&key, repo).map(|_| ())
    }
}

#[cfg(test)]
mod tests;
