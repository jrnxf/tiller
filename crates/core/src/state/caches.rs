//! Caches of facts other systems own, and the daemon's bookkeeping:
//! `cache.refresh`, `reservation.release`, `daemon.start`, `daemon.stop`,
//! `guard.breach`, `config.set` (`state.md` section 8).
//!
//! Cache rows are rebuildable (`state.md` section 10): a refresh rewrites
//! them every poll and writes an event only when a fact changed.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::projects::{local_host_id, require_daemon};
use super::runs::release_reservation;
use super::{find, get, list, required};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};
use crate::time::Timestamp;

/// One pane as herdr reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaneReading {
    pub pane_id: String,
    pub workspace_label: Option<String>,
    pub agent_name: Option<String>,
    pub agent_status: Option<String>,
    pub session_id: Option<String>,
    pub metadata: Value,
}

/// One worktree as treehouse and git report it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorktreeReading {
    pub path: String,
    pub project_id: Option<String>,
    pub run_id: Option<String>,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub dirty: Option<bool>,
    pub unlanded_commits: Option<i64>,
}

/// One quota window as quota-axi reports it. Never an account id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuotaReading {
    pub label: String,
    pub percent_remaining: Option<f64>,
    pub resets_at: Option<Timestamp>,
    pub pace: Option<String>,
    pub reserve: Option<f64>,
    pub burn_multiple: Option<f64>,
    pub elapsed_percent: Option<f64>,
    pub projected_exhaustion_at: Option<Timestamp>,
    pub title: Option<String>,
    pub kind: Option<String>,
    pub fetch_error: Option<String>,
}

/// A full snapshot of one host's panes. Panes missing from it are gone.
fn refresh_panes(tx: &mut Tx<'_>, host_id: &str, panes: &[PaneReading]) -> Result<usize> {
    let mut changes = 0;
    for pane in panes {
        let old: Option<Pane> = find(
            tx.conn(),
            "host_id = ?1 AND pane_id = ?2",
            &[&host_id, &pane.pane_id],
        )?;
        tx.conn().execute(
            "INSERT INTO panes (pane_id, host_id, workspace_label, agent_name, agent_status, session_id, metadata, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (host_id, pane_id) DO UPDATE SET
               workspace_label = excluded.workspace_label, agent_name = excluded.agent_name,
               agent_status = excluded.agent_status, session_id = excluded.session_id,
               metadata = excluded.metadata, fetched_at = excluded.fetched_at, gone_at = NULL",
            params![
                pane.pane_id,
                host_id,
                pane.workspace_label,
                pane.agent_name,
                pane.agent_status,
                pane.session_id,
                pane.metadata,
                tx.now(),
            ],
        )?;
        let changed = match &old {
            None => true,
            Some(old) => {
                old.gone_at.is_some()
                    || old.agent_status != pane.agent_status
                    || old.session_id != pane.session_id
                    || old.workspace_label != pane.workspace_label
                    || old.agent_name != pane.agent_name
            }
        };
        if changed {
            changes += 1;
            tx.event(
                NewEvent::new(
                    "pane",
                    format!("{host_id}:{}", pane.pane_id),
                    "pane.changed",
                )
                .payload(json!({
                    "host_id": host_id,
                    "pane_id": pane.pane_id,
                    "agent_status": pane.agent_status,
                    "session_id": pane.session_id,
                    "new": old.is_none(),
                })),
            )?;
        }
    }
    let seen: Vec<&str> = panes.iter().map(|p| p.pane_id.as_str()).collect();
    let live: Vec<Pane> = list(tx.conn(), "host_id = ?1 AND gone_at IS NULL", &[&host_id])?;
    for pane in live
        .into_iter()
        .filter(|p| !seen.contains(&p.pane_id.as_str()))
    {
        tx.conn().execute(
            "UPDATE panes SET gone_at = ?3 WHERE host_id = ?1 AND pane_id = ?2",
            params![host_id, pane.pane_id, tx.now()],
        )?;
        changes += 1;
        tx.event(
            NewEvent::new(
                "pane",
                format!("{host_id}:{}", pane.pane_id),
                "pane.changed",
            )
            .payload(json!({"host_id": host_id, "pane_id": pane.pane_id, "gone": true})),
        )?;
    }
    Ok(changes)
}

/// A full snapshot of one host's worktrees. Worktrees missing from it are gone.
fn refresh_worktrees(
    tx: &mut Tx<'_>,
    host_id: &str,
    worktrees: &[WorktreeReading],
) -> Result<usize> {
    let mut changes = 0;
    for worktree in worktrees {
        let old: Option<Worktree> = find(
            tx.conn(),
            "host_id = ?1 AND path = ?2",
            &[&host_id, &worktree.path],
        )?;
        tx.conn().execute(
            "INSERT INTO worktrees (path, host_id, project_id, run_id, branch, head_sha, dirty, unlanded_commits, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (host_id, path) DO UPDATE SET
               project_id = excluded.project_id, run_id = excluded.run_id, branch = excluded.branch,
               head_sha = excluded.head_sha, dirty = excluded.dirty, unlanded_commits = excluded.unlanded_commits,
               fetched_at = excluded.fetched_at, gone_at = NULL",
            params![
                worktree.path,
                host_id,
                worktree.project_id,
                worktree.run_id,
                worktree.branch,
                worktree.head_sha,
                worktree.dirty,
                worktree.unlanded_commits,
                tx.now(),
            ],
        )?;
        let changed = match &old {
            None => true,
            Some(old) => {
                old.gone_at.is_some()
                    || old.head_sha != worktree.head_sha
                    || old.branch != worktree.branch
                    || old.dirty != worktree.dirty
                    || old.unlanded_commits != worktree.unlanded_commits
                    || old.run_id != worktree.run_id
            }
        };
        if changed {
            changes += 1;
            tx.event(
                NewEvent::new(
                    "worktree",
                    format!("{host_id}:{}", worktree.path),
                    "worktree.changed",
                )
                .payload(json!({
                    "host_id": host_id,
                    "path": worktree.path,
                    "head_sha": worktree.head_sha,
                    "dirty": worktree.dirty,
                    "unlanded_commits": worktree.unlanded_commits,
                })),
            )?;
        }
    }
    let seen: Vec<&str> = worktrees.iter().map(|w| w.path.as_str()).collect();
    let live: Vec<Worktree> = list(tx.conn(), "host_id = ?1 AND gone_at IS NULL", &[&host_id])?;
    for worktree in live
        .into_iter()
        .filter(|w| !seen.contains(&w.path.as_str()))
    {
        tx.conn().execute(
            "UPDATE worktrees SET gone_at = ?3 WHERE host_id = ?1 AND path = ?2",
            params![host_id, worktree.path, tx.now()],
        )?;
        changes += 1;
        tx.event(
            NewEvent::new(
                "worktree",
                format!("{host_id}:{}", worktree.path),
                "worktree.changed",
            )
            .payload(json!({"host_id": host_id, "path": worktree.path, "gone": true})),
        )?;
    }
    Ok(changes)
}

fn refresh_quota(tx: &mut Tx<'_>, windows: &[QuotaReading]) -> Result<usize> {
    let mut changes = 0;
    for window in windows {
        let old: Option<QuotaWindow> = find(tx.conn(), "label = ?1", &[&window.label])?;
        tx.conn().execute(
            "INSERT INTO quota_windows (label, percent_remaining, resets_at, pace, reserve, burn_multiple, elapsed_percent,
                                        projected_exhaustion_at, title, kind, fetched_at, fetch_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT (label) DO UPDATE SET
               percent_remaining = excluded.percent_remaining, resets_at = excluded.resets_at, pace = excluded.pace,
               reserve = excluded.reserve, burn_multiple = excluded.burn_multiple,
               elapsed_percent = excluded.elapsed_percent, projected_exhaustion_at = excluded.projected_exhaustion_at,
               title = excluded.title, kind = excluded.kind, fetched_at = excluded.fetched_at,
               fetch_error = excluded.fetch_error",
            params![
                window.label,
                window.percent_remaining,
                window.resets_at,
                window.pace,
                window.reserve,
                window.burn_multiple,
                window.elapsed_percent,
                window.projected_exhaustion_at,
                window.title,
                window.kind,
                tx.now(),
                window.fetch_error,
            ],
        )?;
        let changed = match &old {
            None => true,
            Some(old) => {
                old.percent_remaining != window.percent_remaining
                    || old.resets_at != window.resets_at
                    || old.pace != window.pace
                    || old.fetch_error != window.fetch_error
            }
        };
        if changed {
            changes += 1;
            tx.event(
                NewEvent::new("quota", &window.label, "quota.changed").payload(json!({
                    "label": window.label,
                    "percent_remaining": window.percent_remaining,
                    "resets_at": window.resets_at,
                    "fetch_error": window.fetch_error,
                })),
            )?;
        }
    }
    Ok(changes)
}

impl Store {
    /// `cache.refresh` for one host's panes (herdr poller).
    pub fn cache_panes(
        &mut self,
        ctx: &Ctx,
        host_id: &str,
        panes: Vec<PaneReading>,
    ) -> Result<Applied<usize>> {
        self.write(ctx, "cache.refresh", None, |tx| {
            tx.rebuildable();
            get::<Host>(tx.conn(), "host", host_id)?;
            refresh_panes(tx, host_id, &panes)
        })
    }

    /// `cache.refresh` for one host's worktrees (treehouse and git).
    pub fn cache_worktrees(
        &mut self,
        ctx: &Ctx,
        host_id: &str,
        worktrees: Vec<WorktreeReading>,
    ) -> Result<Applied<usize>> {
        self.write(ctx, "cache.refresh", None, |tx| {
            tx.rebuildable();
            get::<Host>(tx.conn(), "host", host_id)?;
            refresh_worktrees(tx, host_id, &worktrees)
        })
    }

    /// `cache.refresh` for the quota feed.
    pub fn cache_quota(&mut self, ctx: &Ctx, windows: Vec<QuotaReading>) -> Result<Applied<usize>> {
        self.write(ctx, "cache.refresh", None, |tx| {
            tx.rebuildable();
            refresh_quota(tx, &windows)
        })
    }

    /// `cache.refresh` of `hosts.last_seen_at`; `stale_reason` set means the
    /// host could not be reached. An event only when reachability changes.
    pub fn host_seen(
        &mut self,
        ctx: &Ctx,
        host_id: &str,
        stale_reason: Option<&str>,
    ) -> Result<Applied<Host>> {
        self.write(ctx, "cache.refresh", None, |tx| {
            tx.rebuildable();
            let host: Host = get(tx.conn(), "host", host_id)?;
            match stale_reason {
                None => tx.conn().execute(
                    "UPDATE hosts SET last_seen_at = ?2, stale_reason = NULL WHERE id = ?1",
                    params![host.id, tx.now()],
                )?,
                Some(reason) => tx.conn().execute(
                    "UPDATE hosts SET stale_reason = ?2 WHERE id = ?1",
                    params![host.id, reason],
                )?,
            };
            if host.stale_reason.as_deref() != stale_reason {
                tx.event(
                    NewEvent::new("host", &host.id, "host.changed")
                        .payload(json!({"stale_reason": stale_reason, "was": host.stale_reason})),
                )?;
            }
            get(tx.conn(), "host", &host.id)
        })
    }

    /// `reservation.release` (reconcile): releases an open reservation whose
    /// run ended or is missing. Natural: a released reservation is returned.
    pub fn reservation_release(
        &mut self,
        ctx: &Ctx,
        reservation: &str,
    ) -> Result<Applied<Reservation>> {
        self.write(ctx, "reservation.release", None, |tx| {
            let reservation: Reservation = get(tx.conn(), "reservation", reservation)?;
            if reservation.released_at.is_some() {
                return Ok(reservation);
            }
            let run: Option<Run> = find(tx.conn(), "id = ?1", &[&reservation.run_id])?;
            match run {
                Some(run) if run.ended_at.is_none() => {
                    return Err(Error::refused("reservation", &reservation.id, format!("run {} is active", run.id)));
                }
                Some(run) => {
                    release_reservation(tx, &run, ReservationRelease::Reconciled)?;
                }
                None => {
                    tx.conn().execute(
                        "UPDATE reservations SET released_at = ?2, release_reason = 'reconciled' WHERE id = ?1",
                        params![reservation.id, tx.now()],
                    )?;
                    tx.event(
                        NewEvent::new("reservation", &reservation.id, "reservation.released")
                            .payload(json!({"reason": "reconciled", "run_id": reservation.run_id, "run_missing": true})),
                    )?;
                }
            }
            get(tx.conn(), "reservation", &reservation.id)
        })
    }

    /// `daemon.start`: every `executing` approval becomes `uncertain`, since
    /// its result was never read; `boot_id` is written when given (last, in
    /// a reboot restore).
    pub fn daemon_start(
        &mut self,
        ctx: &Ctx,
        versions: Value,
        boot_id: Option<&str>,
    ) -> Result<Applied<Vec<String>>> {
        require_daemon(&ctx.actor, "daemon.start")?;
        self.write(ctx, "daemon.start", None, |tx| {
            local_host_id(tx.conn())?;
            let host = local_host_id(tx.conn())?;
            tx.event(NewEvent::new("host", &host, "daemon.started").payload(json!({"versions": versions, "boot_id": boot_id})))?;
            let executing: Vec<Approval> = list(tx.conn(), "state = 'executing' ORDER BY id", &[])?;
            let mut uncertain = Vec::with_capacity(executing.len());
            for approval in executing {
                approval.state.finish(Finish::Uncertain).map_err(super::refuse("approval", &approval.id))?;
                tx.conn().execute(
                    "UPDATE approvals SET state = 'uncertain', rev = rev + 1 WHERE id = ?1",
                    [&approval.id],
                )?;
                tx.event(
                    NewEvent::new("approval", &approval.id, "approval.finished")
                        .rev(approval.rev + 1)
                        .card_opt(approval.card_id.clone())
                        .payload(json!({"state": "uncertain", "reason": "the daemon restarted while it executed"})),
                )?;
                uncertain.push(approval.id);
            }
            if let Some(boot_id) = boot_id {
                tx.conn().execute(
                    "INSERT INTO meta (key, value) VALUES ('boot_id', ?1)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    [boot_id],
                )?;
            }
            Ok(uncertain)
        })
    }

    /// `daemon.stop`.
    pub fn daemon_stop(&mut self, ctx: &Ctx, reason: &str) -> Result<Applied<()>> {
        require_daemon(&ctx.actor, "daemon.stop")?;
        self.write(ctx, "daemon.stop", None, |tx| {
            let host = local_host_id(tx.conn())?;
            tx.event(
                NewEvent::new("host", &host, "daemon.stopped").payload(json!({"reason": reason})),
            )?;
            Ok(())
        })
    }

    /// The daemon's host check: `host.degraded` the first time a floor or a
    /// tool check fails, `host.recovered` when it clears. `problem` is `None`
    /// when the host is healthy. Writes nothing when nothing changed.
    pub fn host_health(
        &mut self,
        ctx: &Ctx,
        check: &str,
        problem: Option<&str>,
    ) -> Result<Applied<bool>> {
        require_daemon(&ctx.actor, "daemon.start")?;
        self.write(ctx, "daemon.start", None, |tx| {
            let host = local_host_id(tx.conn())?;
            let last: Option<String> = tx
                .conn()
                .query_row(
                    "SELECT kind FROM events WHERE kind IN ('host.degraded', 'host.recovered')
                       AND entity = 'host' AND entity_id = ?1 AND json_extract(payload, '$.check') = ?2
                     ORDER BY seq DESC LIMIT 1",
                    params![host, check],
                    |row| row.get(0),
                )
                .optional()?;
            let degraded = last.as_deref() == Some("host.degraded");
            match problem {
                Some(problem) if !degraded => {
                    tx.event(
                        NewEvent::new("host", &host, "host.degraded").payload(json!({"check": check, "problem": problem})),
                    )?;
                }
                None if degraded => {
                    tx.event(NewEvent::new("host", &host, "host.recovered").payload(json!({"check": check})))?;
                }
                _ => {}
            }
            Ok(problem.is_some())
        })
    }

    /// `guard.breach`: nothing but the event.
    pub fn guard_breach(
        &mut self,
        ctx: &Ctx,
        guard: &str,
        card_id: Option<&str>,
        details: Value,
    ) -> Result<Applied<i64>> {
        required("guard", guard)?;
        self.write(ctx, "guard.breach", None, |tx| {
            let seq = tx.event(
                NewEvent::new("guard", guard, "guard.breached")
                    .card_opt(card_id)
                    .payload(details),
            )?;
            Ok(seq)
        })
    }

    /// `config.set`: the config file lives outside `state.db`; this records
    /// the change.
    pub fn config_changed(&mut self, ctx: &Ctx, key: &str, value: Value) -> Result<Applied<i64>> {
        super::projects::require_owner(&ctx.actor, "config.set")?;
        required("key", key)?;
        self.write(ctx, "config.set", None, |tx| {
            tx.event(
                NewEvent::new("config", key, "config.changed")
                    .payload(json!({"key": key, "value": value})),
            )
        })
    }
}
