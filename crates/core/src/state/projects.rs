//! Hosts and projects: `tiller init`, `host.*` and `project.*` (`state.md`
//! sections 3 and 8).

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::attention::{NewAttention, open_attention};
use super::cards::{NewCard, create_card};
use super::{check_rev, find, get, required};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, FLEET_PROJECT, HOST_HEALTH_CARD};
use crate::store::{Actor, Applied, Ctx, NewEvent, Store, Tx};

/// `tiller init`: this host's identity, its local host row, the `fleet`
/// project and the host health card `fleet-1`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Init {
    pub host_label: String,
    pub lane_capacity: i64,
    pub ram_floor_mb: Option<i64>,
    pub disk_floor_mb: Option<i64>,
    pub boundary: Boundary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Initialized {
    pub host: Host,
    pub fleet: Project,
    pub health_card: Card,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewRemoteHost {
    pub label: String,
    pub herdr_machine: String,
}

/// The owner-editable host fields; `None` leaves a field as it is.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostEdit {
    pub lane_capacity: Option<i64>,
    pub ram_floor_mb: Option<Option<i64>>,
    pub disk_floor_mb: Option<Option<i64>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewProject {
    pub id: String,
    pub name: String,
    pub repo: Option<String>,
    pub remote: Option<String>,
    pub default_branch: Option<String>,
    pub delivery: Delivery,
    pub checks: Option<Value>,
    pub capacity: Option<i64>,
    pub deploy_workflow: Option<String>,
    pub deploy_environment: Option<String>,
    pub deploy_timeout_minutes: Option<i64>,
}

/// The owner-editable project fields; `None` leaves a field as it is.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProjectEdit {
    pub name: Option<String>,
    pub repo: Option<Option<String>>,
    pub remote: Option<Option<String>>,
    pub default_branch: Option<String>,
    pub delivery: Option<Delivery>,
    pub checks: Option<Value>,
    pub capacity: Option<i64>,
    pub deploy_workflow: Option<Option<String>>,
    pub deploy_environment: Option<Option<String>>,
    pub deploy_timeout_minutes: Option<i64>,
}

pub(crate) fn require_owner(ctx_actor: &Actor, what: &str) -> Result<()> {
    if ctx_actor.is_owner() {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "{what} is owner only; {ctx_actor} may not"
        )))
    }
}

pub(crate) fn require_daemon(ctx_actor: &Actor, what: &str) -> Result<()> {
    if *ctx_actor == Actor::Daemon {
        Ok(())
    } else {
        Err(Error::Forbidden(format!(
            "{what} is daemon only; {ctx_actor} may not"
        )))
    }
}

/// This host's id from `meta.host_id`; refuses before `tiller init`.
pub(crate) fn local_host_id(conn: &rusqlite::Connection) -> Result<String> {
    conn.query_row("SELECT value FROM meta WHERE key = 'host_id'", [], |row| {
        row.get(0)
    })
    .optional()?
    .ok_or_else(|| {
        Error::refused(
            "host",
            "local",
            "state.db is not initialized; run tiller init",
        )
    })
}

/// Reads one `meta` value.
pub fn meta(conn: &rusqlite::Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

pub(crate) fn positive(name: &str, value: i64) -> Result<()> {
    if value >= 1 {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "{name} must be at least 1, got {value}"
        )))
    }
}

fn non_negative(name: &str, value: Option<i64>) -> Result<()> {
    match value {
        Some(n) if n < 0 => Err(Error::Usage(format!(
            "{name} must not be negative, got {n}"
        ))),
        _ => Ok(()),
    }
}

/// `project.edit` inside a mutation, after the caller checked the owner.
pub(crate) fn edit_project(
    tx: &mut Tx<'_>,
    project_id: &str,
    expected_rev: i64,
    edit: &ProjectEdit,
) -> Result<Project> {
    let project: Project = get(tx.conn(), "project", project_id)?;
    let mut next = project.clone();
    let mut changed = serde_json::Map::new();
    let mut named = None;
    macro_rules! apply {
        ($field:ident) => {
            if let Some(value) = edit.$field.clone() {
                named.get_or_insert(stringify!($field));
                if next.$field != value {
                    changed.insert(stringify!($field).into(), json!(value));
                    next.$field = value;
                }
            }
        };
    }
    apply!(name);
    apply!(repo);
    apply!(remote);
    apply!(default_branch);
    apply!(delivery);
    apply!(checks);
    apply!(capacity);
    apply!(deploy_workflow);
    apply!(deploy_environment);
    apply!(deploy_timeout_minutes);
    let Some(named) = named else {
        return Ok(project);
    };
    let first = changed.keys().next().map_or(named, String::as_str);
    let current = serde_json::to_value(&project)?;
    check_rev(
        "project",
        &project.id,
        project.rev,
        expected_rev,
        Some((first, current[first].clone())),
    )?;
    if changed.is_empty() {
        return Ok(project);
    }
    if project.retired_at.is_some() {
        return Err(Error::refused("project", &project.id, "project is retired"));
    }
    required("project name", &next.name)?;
    positive("capacity", next.capacity)?;
    positive("deploy timeout", next.deploy_timeout_minutes)?;
    tx.conn().execute(
        "UPDATE projects SET name = ?2, repo = ?3, remote = ?4, default_branch = ?5, delivery = ?6,
                    checks = ?7, capacity = ?8, deploy_workflow = ?9, deploy_environment = ?10,
                    deploy_timeout_minutes = ?11, rev = rev + 1
             WHERE id = ?1",
        params![
            next.id,
            next.name,
            next.repo,
            next.remote,
            next.default_branch,
            next.delivery,
            next.checks,
            next.capacity,
            next.deploy_workflow,
            next.deploy_environment,
            next.deploy_timeout_minutes,
        ],
    )?;
    tx.event(
        NewEvent::new("project", &project.id, "project.edited")
            .rev(project.rev + 1)
            .payload(json!({"changed": changed, "expected_rev": expected_rev})),
    )?;
    if next.capacity > project.capacity {
        super::runs::note_slot_open(
            tx,
            &project.id,
            None,
            format!(
                "project capacity raised from {} to {}; retry any queued spawn",
                project.capacity, next.capacity
            ),
        )?;
    }
    get(tx.conn(), "project", &project.id)
}

impl Store {
    /// `tiller init`. Natural: a host already initialized with the same label
    /// returns what exists.
    pub fn init(&mut self, ctx: &Ctx, init: Init) -> Result<Applied<Initialized>> {
        require_owner(&ctx.actor, "tiller init")?;
        required("host label", &init.host_label)?;
        positive("lane capacity", init.lane_capacity)?;
        non_negative("ram floor", init.ram_floor_mb)?;
        non_negative("disk floor", init.disk_floor_mb)?;
        self.write(ctx, "init", None, |tx| {
            if let Some(host_id) = meta(tx.conn(), "host_id")? {
                let host: Host = get(tx.conn(), "host", &host_id)?;
                if host.label != init.host_label {
                    return Err(Error::refused(
                        "host",
                        &host.id,
                        format!("state.db already belongs to host {}", host.label),
                    ));
                }
                return Ok(Initialized {
                    host,
                    fleet: get(tx.conn(), "project", FLEET_PROJECT)?,
                    health_card: find(tx.conn(), "display_id = ?1", &[&HOST_HEALTH_CARD])?
                        .ok_or_else(|| Error::not_found("card", HOST_HEALTH_CARD))?,
                });
            }
            let now = tx.now();
            let host_id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO meta (key, value) VALUES ('host_id', ?1), ('created_at', ?2), ('boundary', ?3)",
                params![host_id, now, init.boundary],
            )?;
            tx.conn().execute(
                "INSERT INTO hosts (id, label, kind, lane_capacity, ram_floor_mb, disk_floor_mb)
                 VALUES (?1, ?2, 'local', ?3, ?4, ?5)",
                params![host_id, init.host_label, init.lane_capacity, init.ram_floor_mb, init.disk_floor_mb],
            )?;
            tx.event(
                NewEvent::new("host", &host_id, "host.added")
                    .rev(1)
                    .payload(json!({"label": init.host_label, "kind": "local", "boundary": init.boundary})),
            )?;
            insert_project(
                tx,
                &NewProject {
                    id: FLEET_PROJECT.to_owned(),
                    name: "Fleet".to_owned(),
                    repo: None,
                    remote: None,
                    default_branch: None,
                    delivery: Delivery::LocalOnly,
                    checks: None,
                    capacity: None,
                    deploy_workflow: None,
                    deploy_environment: None,
                    deploy_timeout_minutes: None,
                },
                &host_id,
            )?;
            let health_card = create_card(
                tx,
                &NewCard {
                    project_id: FLEET_PROJECT.to_owned(),
                    kind: CardKind::Ops,
                    title: "Host health".to_owned(),
                    ..NewCard::default()
                },
                None,
            )?;
            Ok(Initialized {
                host: get(tx.conn(), "host", &host_id)?,
                fleet: get(tx.conn(), "project", FLEET_PROJECT)?,
                health_card,
            })
        })
    }

    /// `host.add` for a remote peer (`tiller peer add`). Natural: the same
    /// label and machine return the existing row.
    pub fn host_add(&mut self, ctx: &Ctx, new: NewRemoteHost) -> Result<Applied<Host>> {
        require_owner(&ctx.actor, "host.add")?;
        required("host label", &new.label)?;
        required("herdr machine", &new.herdr_machine)?;
        self.write(ctx, "host.add", None, |tx| {
            if let Some(host) = find::<Host>(tx.conn(), "label = ?1", &[&new.label])? {
                if host.kind == HostKind::Remote
                    && host.herdr_machine.as_deref() == Some(&new.herdr_machine)
                {
                    return Ok(host);
                }
                return Err(Error::refused(
                    "host",
                    &host.id,
                    format!("label {} is taken", new.label),
                ));
            }
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO hosts (id, label, kind, herdr_machine) VALUES (?1, ?2, 'remote', ?3)",
                params![id, new.label, new.herdr_machine],
            )?;
            tx.event(NewEvent::new("host", &id, "host.added").rev(1).payload(
                json!({"label": new.label, "kind": "remote", "herdr_machine": new.herdr_machine}),
            ))?;
            get(tx.conn(), "host", &id)
        })
    }

    /// `host.edit`: capacity and resource floors, against `expected_rev`.
    pub fn host_edit(
        &mut self,
        ctx: &Ctx,
        host_id: &str,
        expected_rev: i64,
        edit: HostEdit,
    ) -> Result<Applied<Host>> {
        require_owner(&ctx.actor, "host.edit")?;
        if let Some(n) = edit.lane_capacity {
            positive("lane capacity", n)?;
        }
        non_negative("ram floor", edit.ram_floor_mb.flatten())?;
        non_negative("disk floor", edit.disk_floor_mb.flatten())?;
        self.write(ctx, "host.edit", None, |tx| {
            let host: Host = get(tx.conn(), "host", host_id)?;
            let mut changed = serde_json::Map::new();
            if let Some(n) = edit.lane_capacity.filter(|n| host.lane_capacity != Some(*n)) {
                changed.insert("lane_capacity".into(), json!(n));
            }
            if let Some(n) = edit.ram_floor_mb.filter(|n| host.ram_floor_mb != *n) {
                changed.insert("ram_floor_mb".into(), json!(n));
            }
            if let Some(n) = edit.disk_floor_mb.filter(|n| host.disk_floor_mb != *n) {
                changed.insert("disk_floor_mb".into(), json!(n));
            }
            let named = [
                ("lane_capacity", edit.lane_capacity.is_some()),
                ("ram_floor_mb", edit.ram_floor_mb.is_some()),
                ("disk_floor_mb", edit.disk_floor_mb.is_some()),
            ]
            .into_iter()
            .find_map(|(field, sent)| sent.then_some(field));
            let Some(named) = named else {
                return Ok(host);
            };
            let first = changed.keys().next().map_or(named, String::as_str);
            let current = serde_json::to_value(&host)?;
            check_rev("host", &host.id, host.rev, expected_rev, Some((first, current[first].clone())))?;
            if changed.is_empty() {
                return Ok(host);
            }
            if host.kind == HostKind::Remote && changed.contains_key("lane_capacity") {
                return Err(Error::refused("host", &host.id, "a remote host has no lane capacity here"));
            }
            tx.conn().execute(
                "UPDATE hosts SET lane_capacity = ?2, ram_floor_mb = ?3, disk_floor_mb = ?4, rev = rev + 1 WHERE id = ?1",
                params![
                    host.id,
                    edit.lane_capacity.or(host.lane_capacity),
                    edit.ram_floor_mb.unwrap_or(host.ram_floor_mb),
                    edit.disk_floor_mb.unwrap_or(host.disk_floor_mb),
                ],
            )?;
            tx.event(
                NewEvent::new("host", &host.id, "host.edited")
                    .rev(host.rev + 1)
                    .payload(json!({"changed": changed, "expected_rev": expected_rev})),
            )?;
            get(tx.conn(), "host", &host.id)
        })
    }

    /// `project.add`. Natural: the same values return the existing row.
    pub fn project_add(&mut self, ctx: &Ctx, new: NewProject) -> Result<Applied<Project>> {
        require_owner(&ctx.actor, "project.add")?;
        ids::validate_project_id(&new.id)?;
        if new.id == FLEET_PROJECT {
            return Err(Error::Usage(format!(
                "project id {FLEET_PROJECT} is reserved"
            )));
        }
        required("project name", &new.name)?;
        self.write(ctx, "project.add", None, |tx| {
            let host_id = local_host_id(tx.conn())?;
            if let Some(existing) = find::<Project>(tx.conn(), "id = ?1", &[&new.id])? {
                if project_matches(&existing, &new) {
                    return Ok(existing);
                }
                return Err(Error::refused("project", &new.id, "project id is taken"));
            }
            insert_project(tx, &new, &host_id)?;
            get(tx.conn(), "project", &new.id)
        })
    }

    /// `project.edit` against `expected_rev`. A stale `rev` is a conflict even
    /// when nothing would change; at the current `rev` that is a no-op.
    pub fn project_edit(
        &mut self,
        ctx: &Ctx,
        project_id: &str,
        expected_rev: i64,
        edit: ProjectEdit,
    ) -> Result<Applied<Project>> {
        require_owner(&ctx.actor, "project.edit")?;
        self.write(ctx, "project.edit", None, |tx| {
            edit_project(tx, project_id, expected_rev, &edit)
        })
    }

    /// `project.retire`: refused while the project has an open card.
    pub fn project_retire(&mut self, ctx: &Ctx, project_id: &str) -> Result<Applied<Project>> {
        require_owner(&ctx.actor, "project.retire")?;
        self.write(ctx, "project.retire", None, |tx| {
            let project: Project = get(tx.conn(), "project", project_id)?;
            if project.retired_at.is_some() {
                return Ok(project);
            }
            if project.id == FLEET_PROJECT {
                return Err(Error::refused("project", &project.id, "the fleet project never retires"));
            }
            let open: Option<String> = tx
                .conn()
                .query_row(
                    "SELECT display_id FROM cards WHERE project_id = ?1 AND state = 'open' ORDER BY display_id LIMIT 1",
                    [&project.id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(card) = open {
                return Err(Error::refused("project", &project.id, format!("card {card} is open")));
            }
            tx.conn().execute(
                "UPDATE projects SET retired_at = ?2, rev = rev + 1 WHERE id = ?1",
                params![project.id, tx.now()],
            )?;
            tx.event(NewEvent::new("project", &project.id, "project.retired").rev(project.rev + 1))?;
            get(tx.conn(), "project", &project.id)
        })
    }

    /// `project.merge_gate`: the daemon's ruleset check moved the project's
    /// merge gate. A move to `detect` also degrades the host and opens a
    /// `recovery` item on `fleet-1` (`authority.md`, Enforced merge gate).
    pub fn project_set_merge_gate(
        &mut self,
        ctx: &Ctx,
        project_id: &str,
        gate: MergeGate,
        reason: &str,
    ) -> Result<Applied<Project>> {
        require_daemon(&ctx.actor, "project.merge_gate")?;
        self.write(ctx, "project.merge_gate", None, |tx| {
            let project: Project = get(tx.conn(), "project", project_id)?;
            if project.merge_gate == gate {
                return Ok(project);
            }
            if project.retired_at.is_some() {
                return Err(Error::refused("project", &project.id, "project is retired"));
            }
            tx.conn().execute(
                "UPDATE projects SET merge_gate = ?2, rev = rev + 1 WHERE id = ?1",
                params![project.id, gate],
            )?;
            tx.event(
                NewEvent::new("project", &project.id, "project.merge_gate_changed")
                    .rev(project.rev + 1)
                    .payload(json!({"from": project.merge_gate, "to": gate, "reason": reason})),
            )?;
            if gate == MergeGate::Detect {
                let host_id = local_host_id(tx.conn())?;
                tx.event(
                    NewEvent::new("host", &host_id, "host.degraded")
                        .payload(json!({"check": "merge_gate", "project": project.id, "reason": reason})),
                )?;
                let health: Card = find(tx.conn(), "display_id = ?1", &[&HOST_HEALTH_CARD])?
                    .ok_or_else(|| Error::not_found("card", HOST_HEALTH_CARD))?;
                open_attention(
                    tx,
                    &NewAttention {
                        card: health.id,
                        kind: AttentionKind::Recovery,
                        question: format!(
                            "The merge gate for {} is no longer enforced: {reason}. Merges are detected, not prevented.",
                            project.id
                        ),
                        ..NewAttention::default()
                    },
                )?;
            }
            get(tx.conn(), "project", &project.id)
        })
    }
}

fn project_matches(existing: &Project, new: &NewProject) -> bool {
    existing.name == new.name
        && existing.repo == new.repo
        && existing.remote == new.remote
        && Some(existing.default_branch.as_str()) == new.default_branch.as_deref().or(Some("main"))
        && existing.delivery == new.delivery
        && new
            .checks
            .as_ref()
            .is_none_or(|checks| *checks == existing.checks)
        && new.capacity.is_none_or(|n| n == existing.capacity)
        && existing.deploy_workflow == new.deploy_workflow
        && existing.deploy_environment == new.deploy_environment
        && new
            .deploy_timeout_minutes
            .is_none_or(|n| n == existing.deploy_timeout_minutes)
}

/// The `next_card_seq` a new project with this id starts at: 1, or where a
/// deleted project of the same id stopped, so display ids are never reused
/// (state.md, invariant 9). `project.add` and import both start here.
pub(crate) fn first_card_seq(conn: &rusqlite::Connection, project_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT coalesce(max(json_extract(payload, '$.next_card_seq')), 1) FROM events
         WHERE kind = 'project.deleted' AND entity = 'project' AND entity_id = ?1",
        [project_id],
        |row| row.get(0),
    )?)
}

fn insert_project(tx: &mut Tx<'_>, new: &NewProject, host_id: &str) -> Result<()> {
    let capacity = new.capacity.unwrap_or(2);
    positive("capacity", capacity)?;
    let timeout = new.deploy_timeout_minutes.unwrap_or(60);
    positive("deploy timeout", timeout)?;
    let checks = new.checks.clone().unwrap_or_else(|| json!([]));
    let first_seq = first_card_seq(tx.conn(), &new.id)?;
    tx.conn().execute(
        "INSERT INTO projects (id, name, repo, remote, default_branch, delivery, checks, capacity,
                               deploy_workflow, deploy_environment, deploy_timeout_minutes,
                               owner_host_id, added_at, next_card_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            new.id,
            new.name,
            new.repo,
            new.remote,
            new.default_branch.as_deref().unwrap_or("main"),
            new.delivery,
            checks,
            capacity,
            new.deploy_workflow,
            new.deploy_environment,
            timeout,
            host_id,
            tx.now(),
            first_seq,
        ],
    )?;
    tx.event(
        NewEvent::new("project", &new.id, "project.added")
            .rev(1)
            .payload(json!({"name": new.name, "repo": new.repo, "delivery": new.delivery})),
    )?;
    Ok(())
}
