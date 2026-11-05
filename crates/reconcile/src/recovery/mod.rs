//! The rows of the recovery table (`recovery.md` section 5), grouped by the
//! pass step that runs them. Each function checks its own precondition
//! inside the mutation it calls, so a pass may run it any number of times.
//!
//! - [`startup`](Reconciler::startup) and [`host_checks`](Reconciler::host_checks): R01, R04 (opening the check), R11, R16, R20.
//! - [`timers`](Reconciler::timers): holds, date obligations, away mode.
//! - [`sessions`]: the ladder, R02, R03, R04 (the restore), R12.
//! - [`evidence`]: R13, R14, R15, run ownership, adoption after a restore (R10, R20).
//! - [`approvals`]: R07, R08, R19, and G04's decision check (`authority.md` section 11).

pub mod approvals;
pub mod evidence;
pub mod gates;
pub mod sessions;

use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use tiller_core::domain::{ApprovalState, HoldReleaseReason, ObligationState, TriggerKind};
use tiller_core::state::attention::NewAttention;
use tiller_core::state::{list, projects::meta};
use tiller_core::{NewEvent, Result, Timestamp};

use crate::pass::{Reconciler, Report, minutes_since};

impl Reconciler {
    /// Step 1 (R01, R11, R20, and opening R04): the owner marker was read by
    /// the caller; the schema is this binary's (`Store::open` migrated it);
    /// the boot id decides whether a reboot restore opens; `fleet-1` must
    /// exist; `daemon.started` turns every `executing` approval `uncertain`.
    pub(crate) fn startup(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        self.health_card()?;
        let recorded = meta(self.conn(), "boot_id")?;
        let boot_id = match (&self.config.boot_id, &recorded) {
            // First start on this host, or a host that reports no boot id:
            // record what we know, no restore.
            (Some(current), None) => Some(current.as_str()),
            (Some(current), Some(seen)) if seen == current => None,
            (Some(current), Some(seen)) => {
                report.repair(format!(
                    "R04 boot id changed from {seen} to {current}: the reboot check is open"
                ));
                None
            }
            (None, _) => None,
        };
        let versions = self.sources.readings.read().versions;
        let uncertain = self.store.daemon_start(&self.ctx(now), versions, boot_id)?;
        for approval in &uncertain.value {
            report.repair(format!(
                "R01 approval {approval} was executing at the restart: now uncertain"
            ));
        }
        Ok(())
    }

    /// Step 2 (R01, R16): the state directory's modes, tool version skew and
    /// the floors. A new condition writes `host.degraded`; the pass that
    /// sees it cleared writes `host.recovered`; one that stays long enough
    /// goes to the developer on `fleet-1`.
    pub(crate) fn host_checks(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let ctx = self.ctx(now);
        let readings = self.sources.readings.read();
        let host: tiller_core::domain::Host =
            tiller_core::state::get(self.conn(), "host", &self.config.host_id)?;

        // The state directory and its files are private, whatever the umask.
        let mut loose = Vec::new();
        for (path, want) in [
            (self.config.tiller_home.clone(), 0o700),
            (self.config.tiller_home.join("state.db"), 0o600),
        ] {
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let mode = metadata.permissions().mode() & 0o777;
            if mode & !want != 0 {
                loose.push(format!("{} was {mode:o}", path.display()));
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(want));
            }
        }
        let dir_problem = (!loose.is_empty()).then(|| format!("repaired: {}", loose.join(", ")));
        if self
            .store
            .host_health(&ctx, "state_dir", dir_problem.as_deref())?
            .value
        {
            report.repair("R01 state directory modes repaired");
        }
        // Repaired at once: the next check clears it.
        if dir_problem.is_some() {
            self.store.host_health(&ctx, "state_dir", None)?;
        }

        self.store
            .host_health(&ctx, "tool_versions", readings.version_skew.as_deref())?;
        // A floor with no reading is unknown: the check writes nothing, so
        // a breached floor is never read as recovered by a failed read.
        let floor = |name: &str, floor: Option<i64>, free: Option<i64>| -> Option<Option<String>> {
            match (floor, free) {
                (Some(_), None) => None,
                (Some(floor), Some(free)) if free < floor => {
                    Some(Some(format!("{name}: {free} MB free, floor {floor} MB")))
                }
                _ => Some(None),
            }
        };
        if let Some(ram) = floor("RAM", host.ram_floor_mb, readings.ram_free_mb) {
            self.store.host_health(&ctx, "ram", ram.as_deref())?;
        }
        if let Some(disk) = floor("disk", host.disk_floor_mb, readings.disk_free_mb) {
            self.store.host_health(&ctx, "disk", disk.as_deref())?;
        }

        // Escalations count from the `host.degraded` event.
        let t = self.config.thresholds.clone();
        for (check, after, question) in [
            (
                "ram",
                t.floor_breach_developer,
                "RAM has been under its floor for",
            ),
            (
                "disk",
                t.floor_breach_developer,
                "Disk has been under its floor for",
            ),
            (
                "tool_versions",
                t.version_skew_developer,
                "A tool version skew has stood for",
            ),
        ] {
            let Some((at, payload)) = self.degraded_since(check)? else {
                continue;
            };
            if at.plus_minutes(after) > now {
                continue;
            }
            let problem = payload
                .get("problem")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_owned();
            let consumers = self.biggest_consumers();
            let key = format!("reconcile:R16:{check}:{at}");
            let opened = self.recovery(
                now,
                &key,
                NewAttention {
                    question: format!("{question} {} minutes: {problem}. Run the sweep, or raise the floor?", minutes_since(at, now)),
                    evidence: Self::json_text(json!({"check": check, "degraded_at": at.to_string(), "consumers": consumers})),
                    recommendation: Some("run the dotfiles' daily sweep and Nix garbage collection; Tiller never deletes".into()),
                    ..NewAttention::default()
                },
            )?;
            if opened {
                report.repair(format!(
                    "R16 {check} breached since {at}: recovery item on fleet-1"
                ));
            }
        }
        Ok(())
    }

    /// When `check` went degraded, if it has not recovered since.
    fn degraded_since(&self, check: &str) -> Result<Option<(Timestamp, serde_json::Value)>> {
        let host = self.config.host_id.clone();
        let row = self.conn().query_row(
            "SELECT kind, at, payload FROM events WHERE kind IN ('host.degraded', 'host.recovered')
                   AND entity = 'host' AND entity_id = ?1 AND json_extract(payload, '$.check') = ?2
                 ORDER BY seq DESC LIMIT 1",
            [host, check.to_owned()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Timestamp>(1)?,
                    row.get::<_, serde_json::Value>(2)?,
                ))
            },
        );
        match row {
            Ok((kind, at, payload)) if kind == "host.degraded" => Ok(Some((at, payload))),
            Ok(_) => Ok(None),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// The worktree paths the last scan knows, biggest first by what it can
    /// tell (dirty and unlanded ones first), for the R16 item.
    fn biggest_consumers(&self) -> Vec<String> {
        let mut paths: Vec<(bool, String)> = self
            .scans
            .values()
            .filter(|s| s.exists)
            .map(|s| {
                (
                    s.dirty == Some(true) || s.unlanded_commits.unwrap_or(0) > 0,
                    s.path.display().to_string(),
                )
            })
            .collect();
        paths.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        paths.into_iter().map(|(_, p)| p).take(10).collect()
    }

    /// Step 3: holds whose date passed (the `decision` item reopens), date
    /// obligations due, away approvals past their end. A paused run simply
    /// stops being silenced: the view reads `pause_until` itself.
    pub(crate) fn timers(&mut self, now: Timestamp, report: &mut Report) -> Result<()> {
        let ctx = self.ctx(now);
        let due_holds: Vec<tiller_core::domain::Hold> = list(
            self.conn(),
            "released_at IS NULL AND until_at <= ?1 ORDER BY until_at",
            &[&now],
        )?;
        for hold in due_holds {
            match self
                .store
                .hold_release(&ctx, &hold.card_id, HoldReleaseReason::Due)
            {
                Ok(_) => report.repair(format!(
                    "timer: hold {} on {} was due",
                    hold.id, hold.card_id
                )),
                Err(error) => report.error("hold.release", &error),
            }
        }
        let due: Vec<tiller_core::domain::Obligation> = list(
            self.conn(),
            "state = ?1 AND trigger_kind = ?2 AND due_at <= ?3 ORDER BY due_at",
            &[&ObligationState::Open, &TriggerKind::Date, &now],
        )?;
        for obligation in due {
            let trigger = NewEvent::new("obligation", &obligation.id, "obligation.due")
                .card_opt(obligation.card_id.clone())
                .payload(json!({"due_at": obligation.due_at.map(|t| t.to_string())}));
            match self.store.obligation_meet(&ctx, &obligation.id, trigger) {
                Ok(_) => report.repair(format!("timer: obligation {} was due", obligation.id)),
                Err(error) => report.error("obligation.meet", &error),
            }
        }
        let away: Vec<tiller_core::domain::Approval> = list(
            self.conn(),
            "kind = 'away' AND state = ?1 ORDER BY id",
            &[&ApprovalState::Accepted],
        )?;
        for approval in away {
            let until = approval
                .scope
                .as_ref()
                .and_then(|s| s.get("until_at"))
                .and_then(|u| u.as_str())
                .and_then(|u| u.parse::<Timestamp>().ok());
            if until.is_some_and(|until| until <= now) {
                match self.store.away_end(&ctx, "the away window ended") {
                    Ok(_) => report.repair(format!("timer: away approval {} ended", approval.id)),
                    Err(error) => report.error("away.end", &error),
                }
            }
        }
        Ok(())
    }
}
