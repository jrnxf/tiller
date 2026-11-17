//! Reads: the derived views and the event log (`state.md` sections 6 and 9).
//! Reads never write and need no transaction of their own.

use rusqlite::{OptionalExtension, params};

use super::projects::local_host;
use super::{get, list, resolve_card};
use crate::domain::*;
use crate::error::Result;
use crate::store::Store;
use crate::time::Timestamp;

/// An unresolved ask with its age (the `asked` view).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AskedRow {
    pub ask: Ask,
    pub age_seconds: f64,
}

impl Store {
    pub fn card(&self, reference: &str) -> Result<Card> {
        resolve_card(self.conn(), reference)
    }

    pub fn project(&self, id: &str) -> Result<Project> {
        get(self.conn(), "project", id)
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        list(self.conn(), "1 = 1 ORDER BY id", &[])
    }

    pub fn hosts(&self) -> Result<Vec<Host>> {
        list(self.conn(), "1 = 1 ORDER BY kind, label", &[])
    }

    /// This host's own row, refused before `tiller init`.
    pub fn local_host(&self) -> Result<Host> {
        local_host(self.conn())
    }

    /// The `card_column` view: open cards and those closed in the last three
    /// days, for one project or all.
    pub fn card_columns(&self, project: Option<&str>) -> Result<Vec<CardColumn>> {
        let mut stmt = self.conn().prepare(
            "SELECT * FROM card_column WHERE ?1 IS NULL OR project_id = ?1 ORDER BY project_id, display_id",
        )?;
        let rows = stmt.query_map([project], CardColumn::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn card_column(&self, card_id: &str) -> Result<Option<CardColumn>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT * FROM card_column WHERE id = ?1")?;
        let mut rows = stmt.query_map([card_id], CardColumn::from_row)?;
        Ok(rows.next().transpose()?)
    }

    /// The `asked` view: unresolved asks, oldest first.
    pub fn asked(&self, project: Option<&str>) -> Result<Vec<AskedRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, age_seconds FROM asked WHERE ?1 IS NULL OR project_id = ?1 ORDER BY received_at, id",
        )?;
        let ids: Vec<(String, f64)> = stmt
            .query_map([project], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        ids.into_iter()
            .map(|(id, age_seconds)| {
                Ok(AskedRow {
                    ask: get(self.conn(), "ask", &id)?,
                    age_seconds,
                })
            })
            .collect()
    }

    /// How many of a project's run slots are free: its capacity less the
    /// open reservations, never below zero. The same count `run.start`
    /// refuses on.
    pub fn project_free_runs(&self, id: &str) -> Result<i64> {
        let project = self.project(id)?;
        let reserved: i64 = self.conn().query_row(
            "SELECT count(*) FROM reservations WHERE project_id = ?1 AND released_at IS NULL",
            [id],
            |row| row.get(0),
        )?;
        Ok((project.capacity - reserved).max(0))
    }

    /// The `capacity` view: free run slots on each local host.
    pub fn capacity(&self) -> Result<Vec<HostCapacity>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT * FROM capacity ORDER BY label")?;
        let rows = stmt.query_map([], HostCapacity::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Events after `seq`, oldest first (SSE resume, `tiller events`).
    pub fn events_since(&self, seq: i64, limit: i64) -> Result<Vec<Event>> {
        list(
            self.conn(),
            "seq > ?1 ORDER BY seq LIMIT ?2",
            &[&seq, &limit],
        )
    }

    /// One page of a card's timeline, newest first: at most `limit` events
    /// older than `before` (the board and `tiller card timeline`). One
    /// notification is one line: the write-ahead `attention.notified`
    /// record is left out once an outcome follows it (`protocol.md` 3.4).
    pub fn timeline(&self, card_id: &str, before: i64, limit: i64) -> Result<Vec<Event>> {
        list(
            self.conn(),
            &format!(
                "card_id = ?1 AND seq < ?2 AND {}
                 ORDER BY seq DESC LIMIT ?3",
                super::notifications::ONE_LINE_PER_NOTIFICATION
            ),
            &[&card_id, &before, &limit],
        )
    }

    /// The events of a project's orchestrator chat thread older than
    /// `before`, newest first, `limit` at a time: every event an orchestrator
    /// session of the project wrote, the start of each of those sessions, and
    /// the project's asks, replies, approval requests and attention items as
    /// they opened. An attention item bound to an approval is that
    /// approval's question, so the approval stands for it. Each branch reads an index in `seq` order, so a page
    /// stops early on a long log.
    pub fn thread_events(&self, project_id: &str, before: i64, limit: i64) -> Result<Vec<Event>> {
        list(
            self.conn(),
            "seq IN (
               SELECT seq FROM events WHERE seq < ?2 AND actor IN
                 (SELECT 'session:' || id FROM sessions WHERE project_id = ?1 AND role = 'orchestrator')
               UNION SELECT seq FROM events WHERE kind = 'session.started' AND seq < ?2 AND entity_id IN
                 (SELECT id FROM sessions WHERE project_id = ?1 AND role = 'orchestrator')
               UNION SELECT seq FROM events WHERE kind = 'ask.recorded' AND seq < ?2 AND entity_id IN
                 (SELECT id FROM asks WHERE project_id = ?1)
               UNION SELECT seq FROM events WHERE kind = 'ask.replied' AND seq < ?2 AND entity_id IN
                 (SELECT id FROM replies WHERE project_id = ?1)
               UNION SELECT seq FROM events WHERE kind = 'approval.requested' AND seq < ?2 AND entity_id IN
                 (SELECT id FROM approvals WHERE project_id = ?1)
               UNION SELECT seq FROM events WHERE kind = 'attention.opened' AND seq < ?2 AND entity_id IN
                 (SELECT attention.id FROM attention JOIN cards ON cards.id = attention.card_id
                  WHERE cards.project_id = ?1
                    AND NOT (json_valid(attention.evidence)
                             AND json_extract(attention.evidence, '$.approval_id') IS NOT NULL))
               ORDER BY seq DESC LIMIT ?3)
             ORDER BY seq DESC",
            &[&project_id, &before, &limit],
        )
    }

    /// When the daemon's ruleset check last moved a project's merge gate, or
    /// `None` when it never has (`tiller doctor`).
    pub fn merge_gate_changed_at(&self, project_id: &str) -> Result<Option<Timestamp>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT at FROM events WHERE entity = 'project' AND entity_id = ?1
                   AND kind = 'project.merge_gate_changed' ORDER BY seq DESC LIMIT 1",
                params![project_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// When the newest `config.changed` event was written, or `None` when no
    /// `tiller config set` ever recorded one (`tiller doctor`).
    pub fn config_changed_at(&self) -> Result<Option<Timestamp>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT at FROM events WHERE kind = 'config.changed' ORDER BY seq DESC LIMIT 1",
                params![],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The problem of the local host's `check` while it is degraded: the
    /// newest `host.degraded` with no `host.recovered` after it. Health
    /// shows it as a warning (`recovery.md` R22).
    pub fn host_degraded(&self, check: &str) -> Result<Option<String>> {
        let last: Option<(String, Option<String>)> = self
            .conn()
            .query_row(
                "SELECT kind, json_extract(payload, '$.problem') FROM events
                  WHERE kind IN ('host.degraded', 'host.recovered') AND entity = 'host'
                    AND entity_id = (SELECT value FROM meta WHERE key = 'host_id')
                    AND json_extract(payload, '$.check') = ?1
                  ORDER BY seq DESC LIMIT 1",
                params![check],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(last.and_then(|(kind, problem)| (kind == "host.degraded").then_some(problem).flatten()))
    }

    /// Why this host refuses every new run now: one line per breached RAM or
    /// disk floor, with the numbers the host check measured (`recovery.md`
    /// R16, "The Machines row shows the breached floor"). Empty while the
    /// host can start a run. It reads the host check's `host.degraded`
    /// events, so it costs no reading of its own; a floor the owner cleared
    /// is dropped at once, before the next check.
    pub fn refused_starts(&self) -> Result<Vec<String>> {
        let host = self.local_host()?;
        let mut reasons = Vec::new();
        for (check, floor) in [("ram", host.ram_floor_mb), ("disk", host.disk_floor_mb)] {
            if floor.is_none() {
                continue;
            }
            if let Some(problem) = self.host_degraded(check)? {
                reasons.push(problem);
            }
        }
        Ok(reasons)
    }

    /// The newest event `seq`, or 0 on an empty log.
    pub fn last_seq(&self) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT coalesce(max(seq), 0) FROM events",
            params![],
            |row| row.get(0),
        )?)
    }
}
