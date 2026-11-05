//! The daemon's publish of a direct-pr builder run: `pr.describe`,
//! `publish.done` and `publish.failed` (`state.md`, Publish). The worker only
//! commits and describes its PR; the daemon pushes the branch and opens the
//! PR outside the sandbox, and records each step here. A retry or a restart
//! pushes the same head again at most, which git leaves as it is, and never
//! opens a second PR.

use rusqlite::{Connection, params};
use serde_json::json;

use super::projects::require_daemon;
use super::runs::require_active;
use super::{find, get, required, resolve_id};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};

/// The publish row of a run, if its worker described a PR.
pub fn publish(conn: &Connection, run: &str) -> Result<Option<Publish>> {
    find(conn, "run_id = ?1", &[&run])
}

/// The open PR of the run's card that holds the run's work, read from the
/// last recorded reading of its worktree: no uncommitted changes, and its
/// head is the head the forge reports for the PR or the head the daemon
/// recorded pushing as the run's publish. Then nothing in the worktree is
/// only there: a builder may end `reported` and leave the PR to the card
/// (`recovery.md` R13). `None` when the run has no worktree, the reading is
/// unknown, or the head is on no open PR of the card. It trusts the latest
/// reading, so `tiller done` takes one first on the run's own host.
pub fn published_head(conn: &Connection, run: &Run) -> Result<Option<Pr>> {
    let Some(path) = &run.worktree_path else {
        return Ok(None);
    };
    let worktree: Option<Worktree> = find(
        conn,
        "host_id = ?1 AND path = ?2 AND gone_at IS NULL",
        &[&run.host_id, path],
    )?;
    let Some(head) = worktree
        .filter(|w| w.dirty == Some(false) && w.unlanded_commits.is_some())
        .and_then(|w| w.head_sha)
    else {
        return Ok(None);
    };
    let forge: Option<Pr> = find(
        conn,
        "card_id = ?1 AND state = 'open' AND head_sha = ?2 ORDER BY number",
        &[&run.card_id, &head],
    )?;
    if forge.is_some() {
        return Ok(forge);
    }
    let Some(pr_id) = publish(conn, &run.id)?
        .filter(|record| record.pushed_sha.as_deref() == Some(head.as_str()))
        .and_then(|record| record.pr_id)
    else {
        return Ok(None);
    };
    let pr: Pr = get(conn, "pr", &pr_id)?;
    Ok((pr.card_id == run.card_id && pr.state == PrState::Open).then_some(pr))
}

/// Whether `record` already covers `head` for `run`: pushed, its PR
/// recorded, no failure since, no new description and no done report since
/// the push. A description or a report in the same millisecond as the push
/// counts as later, so the worst case is one extra publish, never a missed
/// one.
pub fn current(record: &Publish, run: &Run, head: &str) -> bool {
    let Some(pushed_at) = record.pushed_at else {
        return false;
    };
    record.error.is_none()
        && record.pr_id.is_some()
        && record.pushed_sha.as_deref() == Some(head)
        && record.described_at < pushed_at
        && run
            .last_status_at
            .is_none_or(|reported| reported < pushed_at)
}

fn load(tx: &Tx<'_>, run: &str) -> Result<(Run, Publish)> {
    let id = resolve_id(tx.conn(), "runs", "run", run)?;
    let run: Run = get(tx.conn(), "run", &id)?;
    let publish = publish(tx.conn(), &run.id)?
        .ok_or_else(|| Error::refused("run", &run.id, "no PR is described for this run"))?;
    Ok((run, publish))
}

fn reload(tx: &Tx<'_>, run: &str) -> Result<Publish> {
    publish(tx.conn(), run)?.ok_or_else(|| Error::not_found("publish", run))
}

impl Store {
    /// `pr.describe` (`tiller pr describe`): the title and body the daemon
    /// opens the run's PR with. A builder run only. Natural: the same title
    /// and body write nothing.
    pub fn pr_describe(
        &mut self,
        ctx: &Ctx,
        run: &str,
        title: &str,
        body: &str,
    ) -> Result<Applied<Publish>> {
        required("title", title)?;
        required("body", body)?;
        self.write(ctx, "pr.describe", None, |tx| {
            let id = resolve_id(tx.conn(), "runs", "run", run)?;
            let run: Run = get(tx.conn(), "run", &id)?;
            require_active(&run)?;
            if run.role != RunRole::Builder {
                return Err(Error::refused(
                    "run",
                    &run.id,
                    format!("only a builder describes a PR; this run is a {}", run.role),
                ));
            }
            let title = title.trim();
            let body = body.trim();
            let rev = match publish(tx.conn(), &run.id)? {
                Some(old) if old.title == title && old.body == body => return Ok(old),
                Some(old) => {
                    tx.conn().execute(
                        "UPDATE publishes SET title = ?2, body = ?3, described_at = ?4, rev = rev + 1
                         WHERE run_id = ?1",
                        params![run.id, title, body, tx.now()],
                    )?;
                    old.rev + 1
                }
                None => {
                    tx.conn().execute(
                        "INSERT INTO publishes (run_id, title, body, described_at) VALUES (?1, ?2, ?3, ?4)",
                        params![run.id, title, body, tx.now()],
                    )?;
                    1
                }
            };
            tx.event(
                NewEvent::new("publish", &run.id, "pr.described")
                    .rev(rev)
                    .card(&run.card_id)
                    .payload(json!({"title": title})),
            )?;
            reload(tx, &run.id)
        })
    }

    /// `publish.done` (daemon only): the daemon pushed `sha` as the run's
    /// branch and the run's PR is `pr`, opened or found. Clears a recorded
    /// failure. Natural: a record already [`current`] for `sha` writes
    /// nothing; another PR is refused, since a run has one.
    pub fn publish_done(
        &mut self,
        ctx: &Ctx,
        run: &str,
        sha: &str,
        pr: &str,
    ) -> Result<Applied<Publish>> {
        require_daemon(&ctx.actor, "publish.done")?;
        required("sha", sha)?;
        self.write(ctx, "publish.done", None, |tx| {
            let (run, old) = load(tx, run)?;
            let pr: Pr = get(tx.conn(), "pr", pr)?;
            if let Some(id) = &old.pr_id
                && *id != pr.id
            {
                return Err(Error::refused(
                    "publish",
                    &run.id,
                    format!("the run's PR is {id}, not {}", pr.id),
                ));
            }
            if current(&old, &run, sha) {
                return Ok(old);
            }
            tx.conn().execute(
                "UPDATE publishes SET pushed_sha = ?2, pushed_at = ?3, pr_id = ?4, error = NULL, rev = rev + 1
                 WHERE run_id = ?1",
                params![run.id, sha, tx.now(), pr.id],
            )?;
            tx.event(
                NewEvent::new("publish", &run.id, "publish.done")
                    .rev(old.rev + 1)
                    .card(&run.card_id)
                    .payload(json!({
                        "sha": sha,
                        "branch": run.branch,
                        "pr_id": pr.id,
                        "url": pr.url,
                        "opened": old.pr_id.is_none(),
                    })),
            )?;
            reload(tx, &run.id)
        })
    }

    /// `publish.failed` (daemon only): a publish of `sha` (none when the
    /// branch head could not be read) failed with `error`, the real message.
    /// The daemon tries again at once for a new head or a new done report,
    /// and after a backoff from `attempted_at` otherwise.
    pub fn publish_failed(
        &mut self,
        ctx: &Ctx,
        run: &str,
        sha: Option<&str>,
        error: &str,
    ) -> Result<Applied<Publish>> {
        require_daemon(&ctx.actor, "publish.failed")?;
        required("error", error)?;
        self.write(ctx, "publish.failed", None, |tx| {
            let (run, old) = load(tx, run)?;
            tx.conn().execute(
                "UPDATE publishes SET attempted_sha = ?2, attempted_at = ?3, error = ?4, rev = rev + 1
                 WHERE run_id = ?1",
                params![run.id, sha, tx.now(), error],
            )?;
            tx.event(
                NewEvent::new("publish", &run.id, "publish.failed")
                    .rev(old.rev + 1)
                    .card(&run.card_id)
                    .payload(json!({"sha": sha, "error": error})),
            )?;
            reload(tx, &run.id)
        })
    }
}
