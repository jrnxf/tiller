//! The sandbox audit: what a replay touched, read back after it ran. A
//! replay has no side effects when every path and remote it recorded is
//! under the sandbox, no origin moved past its seed, nothing landed or was
//! delivered, every pool lease came back, the bundle is byte for byte what
//! it was, and the real Tiller home holds no row a replay wrote.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::Error;
use crate::sandbox::{SESSION_PREFIX, Sandbox, git};

/// Event kinds that mean work left the sandbox: a landing, a PR, a deploy,
/// or a message delivered to a pane.
const LEAVING: &[&str] = &[
    "approval.",
    "pr.",
    "deploy.",
    "outbox.delivered",
    "outbox.acked",
];

#[derive(Clone, Debug, Default, Serialize)]
pub struct Audit {
    /// The fake herdr's calls, by verb.
    pub herdr_calls: BTreeMap<String, usize>,
    pub leases_taken: usize,
    pub leases_returned: usize,
    /// Events the replay wrote, by kind.
    pub events: BTreeMap<String, usize>,
    pub runs: usize,
    pub origins: usize,
    pub bundle_sha256: String,
    /// What the real Tiller home holds that a replay could have written,
    /// when one was given.
    pub real_home: Option<BTreeMap<String, i64>>,
    /// Each side effect found. Empty is a clean sandbox.
    pub findings: Vec<String>,
}

impl Audit {
    pub fn clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Audits `sandbox`. `bundle_sha256` is [`fingerprint`] of the bundle
    /// before the replay; `real_home` is a Tiller home to check, read-only.
    pub fn run(
        sandbox: &Sandbox,
        bundle: &Path,
        bundle_sha256: &str,
        real_home: Option<&Path>,
    ) -> Result<Self, Error> {
        let mut audit = Self::default();
        let root = &sandbox.root;
        let conn = sandbox.store.conn();

        for call in sandbox.herdr.calls() {
            let verb = call
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned();
            if verb == "send_text" {
                // A pane send launches a script; it must be one spawn wrote
                // into the sandbox.
                for path in call.split('\'').filter(|part| part.starts_with('/')) {
                    if !Path::new(path).starts_with(root) {
                        audit
                            .findings
                            .push(format!("a pane was sent {path}, outside the sandbox"));
                    }
                }
            }
            *audit.herdr_calls.entry(verb).or_default() += 1;
        }

        let leases = sandbox.pool.leases();
        audit.leases_taken = leases.len();
        audit.leases_returned = leases.iter().filter(|l| l.returned).count();
        for lease in leases.iter().filter(|l| !l.returned) {
            audit.findings.push(format!(
                "pool lease {} was not returned",
                lease.path.display()
            ));
        }

        let paths = column(
            conn,
            "SELECT worktree_path FROM runs WHERE worktree_path IS NOT NULL",
        )?
        .into_iter()
        .chain(column(conn, "SELECT path FROM worktrees")?)
        .chain(leases.iter().map(|l| l.path.to_string_lossy().into_owned()));
        for path in paths {
            if !Path::new(&path).starts_with(root) {
                audit
                    .findings
                    .push(format!("worktree {path} is outside the sandbox"));
            }
        }
        for dir in sandbox.config.checkouts.values() {
            if !dir.starts_with(root) {
                audit
                    .findings
                    .push(format!("checkout {} is outside the sandbox", dir.display()));
            }
        }

        let mut repos = sandbox
            .config
            .checkouts
            .values()
            .cloned()
            .collect::<Vec<_>>();
        repos.extend(leases.iter().map(|l| l.path.clone()).filter(|p| p.exists()));
        for repo in &repos {
            for line in git(repo, &["remote", "-v"])?.lines() {
                let url = line.split_whitespace().nth(1).unwrap_or_default();
                if !Path::new(url).starts_with(root) {
                    audit.findings.push(format!(
                        "{} has remote {url} outside the sandbox",
                        repo.display()
                    ));
                }
            }
        }

        for dir in sandbox.config.checkouts.values() {
            let Some(origin) = dir.parent().map(|d| d.join("origin.git")) else {
                continue;
            };
            audit.origins += 1;
            let refs = git(&origin, &["for-each-ref", "--format=%(refname)"])?;
            if refs.lines().count() != 1 {
                audit.findings.push(format!(
                    "{} holds refs beyond its seed: {}",
                    origin.display(),
                    refs.replace('\n', " ")
                ));
            }
            let commits = git(&origin, &["rev-list", "--all", "--count"])?;
            if commits != "1" {
                audit.findings.push(format!(
                    "{} holds {commits} commits, not its one seed",
                    origin.display()
                ));
            }
        }

        let mut stmt = conn
            .prepare("SELECT kind, COUNT(*) FROM events WHERE seq > ?1 GROUP BY kind")
            .map_err(tiller_core::Error::from)?;
        let kinds = stmt
            .query_map(params![sandbox.mark], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(tiller_core::Error::from)?;
        for (kind, count) in kinds {
            let count = usize::try_from(count).unwrap_or_default();
            if LEAVING.iter().any(|l| kind.starts_with(l)) {
                audit.findings.push(format!("{count} {kind} event(s)"));
            }
            audit.events.insert(kind, count);
        }
        let count = |sql: &str| -> Result<i64, Error> {
            Ok(conn
                .query_row(sql, [], |row| row.get(0))
                .map_err(tiller_core::Error::from)?)
        };
        audit.runs = usize::try_from(count("SELECT COUNT(*) FROM runs")?).unwrap_or_default();
        let landed = count("SELECT COUNT(*) FROM runs WHERE outcome = 'landed'")?;
        if landed > 0 {
            audit.findings.push(format!("{landed} run(s) landed"));
        }
        let delivered = count("SELECT COUNT(*) FROM outbox WHERE state IN ('delivered', 'acked')")?;
        if delivered > 0 {
            audit
                .findings
                .push(format!("{delivered} outbox message(s) delivered"));
        }
        let open = count("SELECT COUNT(*) FROM runs WHERE ended_at IS NULL")?;
        if open > 0 {
            audit.findings.push(format!("{open} run(s) still open"));
        }

        audit.bundle_sha256 = fingerprint(bundle)?;
        if audit.bundle_sha256 != bundle_sha256 {
            audit
                .findings
                .push("the bundle changed during the replay".into());
        }

        if let Some(home) = real_home {
            let found = real_rows(home)?;
            for (what, n) in &found {
                if *n > 0 {
                    audit
                        .findings
                        .push(format!("the real home holds {n} replay {what}"));
                }
            }
            audit.real_home = Some(found);
        }
        Ok(audit)
    }
}

fn column(conn: &Connection, sql: &str) -> Result<Vec<String>, Error> {
    let mut stmt = conn.prepare(sql).map_err(tiller_core::Error::from)?;
    let rows = stmt
        .query_map([], |row| row.get(0))
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<String>>>())
        .map_err(tiller_core::Error::from)?;
    Ok(rows)
}

/// Rows a replay would leave in a real home, read without writing: its
/// orchestrator sessions, the asks its prompts would record, and their
/// observations. A missing database holds none.
fn real_rows(home: &Path) -> Result<BTreeMap<String, i64>, Error> {
    let db = home.join("state.db");
    let mut found = BTreeMap::new();
    if !db.exists() {
        return Ok(found);
    }
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(tiller_core::Error::from)?;
    let like = format!("{SESSION_PREFIX}%");
    let prompts = format!("prompt:{SESSION_PREFIX}%");
    for (what, sql, arg) in [
        (
            "sessions",
            "SELECT COUNT(*) FROM sessions WHERE claude_session_id LIKE ?1",
            &like,
        ),
        (
            "events",
            "SELECT COUNT(*) FROM events WHERE idempotency_key LIKE ?1",
            &prompts,
        ),
        (
            "idempotency keys",
            "SELECT COUNT(*) FROM idempotency_keys WHERE key LIKE ?1",
            &prompts,
        ),
    ] {
        let n: i64 = conn
            .query_row(sql, params![arg], |row| row.get(0))
            .map_err(tiller_core::Error::from)?;
        found.insert(what.to_owned(), n);
    }
    Ok(found)
}

/// SHA-256 over every file under `dir`: each relative path and its bytes,
/// in path order.
pub fn fingerprint(dir: &Path) -> Result<String, Error> {
    let mut files = Vec::new();
    walk(dir, &mut files)?;
    files.sort();
    let mut hash = Sha256::new();
    for file in files {
        let relative = file.strip_prefix(dir).unwrap_or(&file);
        hash.update(relative.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update(fs::read(&file)?);
        hash.update([0]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(&entry.path(), files)?;
        } else {
            files.push(entry.path());
        }
    }
    Ok(())
}
