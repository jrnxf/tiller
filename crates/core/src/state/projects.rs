//! Hosts and projects: `tiller init`, `host.*` and `project.*` (`state.md`
//! sections 3 and 8).

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::attention::{NewAttention, open_attention};
use super::cards::{NewCard, create_card};
use super::deletion::LiveAgent;
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
    /// The local host's new label; a remote host keeps the name it was added
    /// under.
    pub label: Option<String>,
    pub lane_capacity: Option<i64>,
    pub ram_floor_mb: Option<Option<i64>>,
    pub disk_floor_mb: Option<Option<i64>>,
    /// The project `tiller bug file` routes to; `Some(None)` clears it.
    pub bug_project_id: Option<Option<String>>,
    /// The local host's new boundary (`meta.boundary`), only while nothing
    /// records the old one (`memory.md` section 8).
    pub boundary: Option<Boundary>,
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

/// Where the cards of a project added under a deleted project's id start
/// (`state.md`, Deleting a project).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Numbering {
    /// On from the tombstone's `next_card_seq`: a run branch of the deleted
    /// project may still be named after one of its display ids.
    Continue,
    /// At 1 again: the caller looked and found no run branch of the deleted
    /// project left in its checkout or on its remote.
    Restart,
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

/// [`Store::project_retire_preview`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RetirePreview {
    pub project: String,
    pub refused: Option<String>,
    /// The agents still running for the project. Retiring stops no agent:
    /// the owner stops them first, or keeps them knowingly.
    pub agents: Vec<LiveAgent>,
}

/// Whether `host` may take `label`: only the local host is renamed here, the
/// label is no other host's, and no peer is paired. A peer checks the board
/// host's label on every hello (`protocol.md` 10.2), so a rename under a
/// pairing would break the link until the owner paired again.
fn label_free(conn: &rusqlite::Connection, host: &Host, label: &str) -> Result<()> {
    if host.kind == HostKind::Remote {
        return Err(Error::refused(
            "host",
            &host.id,
            "a remote host keeps the label it was added under",
        ));
    }
    if find::<Host>(conn, "label = ?1 AND id != ?2", &[&label, &host.id])?.is_some() {
        return Err(Error::refused(
            "host",
            &host.id,
            format!("label {label} is taken"),
        ));
    }
    let paired: Vec<String> = conn
        .prepare("SELECT label FROM hosts WHERE peer_scope IS NOT NULL ORDER BY label")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if !paired.is_empty() {
        return Err(Error::refused(
            "host",
            &host.id,
            format!(
                "unpair first: {} {} this host by its label; remove each with tiller peer remove, rename, then pair again",
                paired.join(", "),
                if paired.len() == 1 { "knows" } else { "know" },
            ),
        ));
    }
    Ok(())
}

/// What already records this host's boundary, if anything: a memory row
/// stamped with it, or a backup or an export a restore or an import would
/// check it against (`memory.md` section 8). Until one exists nothing has
/// read the boundary, so the owner may still pick the one the host belongs
/// to.
fn boundary_record(
    conn: &rusqlite::Connection,
    home: Option<&std::path::Path>,
) -> Result<Option<&'static str>> {
    let stamped: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM memory_sources) OR EXISTS (SELECT 1 FROM evidence)
             OR EXISTS (SELECT 1 FROM proposals)",
        [],
        |row| row.get(0),
    )?;
    if stamped {
        return Ok(Some("memory"));
    }
    Ok(match home {
        Some(home) => {
            crate::backup::Manifest::load(&crate::backup::Paths::in_home(home))?.records_boundary()
        }
        None => None,
    })
}

/// Refuses a boundary change on a remote host, or once anything records the
/// boundary ([`boundary_record`]).
fn boundary_unused(
    conn: &rusqlite::Connection,
    host: &Host,
    home: Option<&std::path::Path>,
    kept: &str,
) -> Result<()> {
    if host.kind == HostKind::Remote {
        return Err(Error::refused(
            "host",
            &host.id,
            "a remote host keeps the boundary set on that host",
        ));
    }
    match boundary_record(conn, home)? {
        Some(what) => Err(Error::refused(
            "host",
            &host.id,
            format!(
                "{what} already records the {kept} boundary; it changes only before any memory, backup or export"
            ),
        )),
        None => Ok(()),
    }
}

/// Why a project that is not retired may not retire.
fn retire_refusal(conn: &rusqlite::Connection, project: &Project) -> Result<Option<String>> {
    if project.id == FLEET_PROJECT {
        return Ok(Some("the fleet project never retires".into()));
    }
    let open: Option<String> = conn
        .query_row(
            "SELECT display_id FROM cards WHERE project_id = ?1 AND state = 'open' ORDER BY display_id LIMIT 1",
            [&project.id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(open.map(|card| format!("card {card} is open")))
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

/// This host's own row, refused like [`local_host_id`] before `tiller init`.
pub(crate) fn local_host(conn: &rusqlite::Connection) -> Result<Host> {
    get(conn, "host", &local_host_id(conn)?)
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
    required("default branch", &next.default_branch)?;
    if changed.contains_key("remote") || changed.contains_key("delivery") {
        // The forge check of an add: the repo a GitHub remote names, and no
        // delivery mode Gerrit cannot run.
        let mut repo = next.repo.clone();
        detect_forge(&next.id, next.remote.as_deref(), next.delivery, &mut repo)?;
        // Only a new remote names a repo; a delivery change leaves it be.
        if changed.contains_key("remote") && repo != next.repo {
            next.repo = repo;
            changed.insert("repo".into(), json!(next.repo));
        }
    }
    check_plan(&next.checks)?;
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
                        format!(
                            "state.db already belongs to host {}; tiller host edit --label renames it",
                            host.label
                        ),
                    ));
                }
                // A replay never changes the boundary, so it must not
                // answer as if it had (`memory.md` section 8).
                if let Some(kept) = meta(tx.conn(), "boundary")?
                    && kept != init.boundary.as_str()
                {
                    return Err(Error::refused(
                        "host",
                        &host.id,
                        format!("state.db already keeps the {kept} boundary; tiller init does not change it"),
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
                "none",
                Numbering::Continue,
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

    /// What already records this host's boundary, so `host.edit` refuses to
    /// change it: `memory`, `a backup` or `an export`; `None` while nothing
    /// does.
    pub fn boundary_recorded(&self) -> Result<Option<&'static str>> {
        boundary_record(self.conn(), self.home())
    }

    /// `host.edit`: the label, capacity, resource floors, the bug project and
    /// the boundary, against `expected_rev`. The bug project must be an open
    /// project this host owns, never `fleet`, which is where a bug goes
    /// without one. The label must be free, and it changes only while this
    /// host has no paired peer: each peer knows its board host by that
    /// label. The boundary changes only while nothing records it.
    pub fn host_edit(
        &mut self,
        ctx: &Ctx,
        host_id: &str,
        expected_rev: i64,
        edit: HostEdit,
    ) -> Result<Applied<Host>> {
        require_owner(&ctx.actor, "host.edit")?;
        if let Some(label) = &edit.label {
            // A word on the peer's command line, as `tiller peer add` checks.
            super::peers::check_label(label)?;
        }
        if let Some(n) = edit.lane_capacity {
            positive("lane capacity", n)?;
        }
        non_negative("ram floor", edit.ram_floor_mb.flatten())?;
        non_negative("disk floor", edit.disk_floor_mb.flatten())?;
        let home = self.home().map(std::path::Path::to_path_buf);
        self.write(ctx, "host.edit", None, |tx| {
            let host: Host = get(tx.conn(), "host", host_id)?;
            // The boundary is the local host's, kept in `meta`, not `hosts`.
            let kept = match host.kind {
                HostKind::Local => meta(tx.conn(), "boundary")?,
                HostKind::Remote => None,
            };
            let mut changed = serde_json::Map::new();
            if let Some(label) = edit.label.as_ref().filter(|label| host.label != **label) {
                changed.insert("label".into(), json!(label));
            }
            if let Some(n) = edit.lane_capacity.filter(|n| host.lane_capacity != Some(*n)) {
                changed.insert("lane_capacity".into(), json!(n));
            }
            if let Some(n) = edit.ram_floor_mb.filter(|n| host.ram_floor_mb != *n) {
                changed.insert("ram_floor_mb".into(), json!(n));
            }
            if let Some(n) = edit.disk_floor_mb.filter(|n| host.disk_floor_mb != *n) {
                changed.insert("disk_floor_mb".into(), json!(n));
            }
            if let Some(id) = edit.bug_project_id.as_ref().filter(|id| host.bug_project_id != **id) {
                changed.insert("bug_project_id".into(), json!(id));
            }
            if let Some(boundary) = edit.boundary.filter(|b| kept.as_deref() != Some(b.as_str())) {
                changed.insert("boundary".into(), json!(boundary));
            }
            let named = [
                ("label", edit.label.is_some()),
                ("lane_capacity", edit.lane_capacity.is_some()),
                ("ram_floor_mb", edit.ram_floor_mb.is_some()),
                ("disk_floor_mb", edit.disk_floor_mb.is_some()),
                ("bug_project_id", edit.bug_project_id.is_some()),
                ("boundary", edit.boundary.is_some()),
            ]
            .into_iter()
            .find_map(|(field, sent)| sent.then_some(field));
            let Some(named) = named else {
                return Ok(host);
            };
            let first = changed.keys().next().map_or(named, String::as_str);
            let now = match first {
                "boundary" => json!(kept),
                field => serde_json::to_value(&host)?[field].clone(),
            };
            check_rev("host", &host.id, host.rev, expected_rev, Some((first, now)))?;
            if changed.is_empty() {
                return Ok(host);
            }
            if let Some(label) = changed.get("label").and_then(Value::as_str) {
                label_free(tx.conn(), &host, label)?;
            }
            if changed.contains_key("boundary") {
                boundary_unused(tx.conn(), &host, home.as_deref(), kept.as_deref().unwrap_or("unset"))?;
            }
            if host.kind == HostKind::Remote && changed.contains_key("lane_capacity") {
                return Err(Error::refused("host", &host.id, "a remote host has no lane capacity here"));
            }
            if let Some(Some(id)) = &edit.bug_project_id {
                if host.kind == HostKind::Remote {
                    return Err(Error::refused(
                        "host",
                        &host.id,
                        "a remote host picks its bug project on that host",
                    ));
                }
                let project: Project = get(tx.conn(), "project", id)?;
                if project.id == FLEET_PROJECT {
                    return Err(Error::refused(
                        "host",
                        &host.id,
                        "fleet cannot be the bug project; with none, Tiller bugs already land in fleet",
                    ));
                }
                if project.retired_at.is_some() {
                    return Err(Error::refused("host", &host.id, format!("{id} is retired")));
                }
                if project.owner_host_id != host.id {
                    return Err(Error::refused(
                        "host",
                        &host.id,
                        format!("{id} belongs to another host"),
                    ));
                }
            }
            tx.conn().execute(
                "UPDATE hosts SET lane_capacity = ?2, ram_floor_mb = ?3, disk_floor_mb = ?4,
                     bug_project_id = ?5, label = ?6, rev = rev + 1 WHERE id = ?1",
                params![
                    host.id,
                    edit.lane_capacity.or(host.lane_capacity),
                    edit.ram_floor_mb.unwrap_or(host.ram_floor_mb),
                    edit.disk_floor_mb.unwrap_or(host.disk_floor_mb),
                    edit.bug_project_id.clone().unwrap_or(host.bug_project_id.clone()),
                    edit.label.as_ref().unwrap_or(&host.label),
                ],
            )?;
            if let Some(boundary) = changed.get("boundary").and(edit.boundary) {
                tx.conn().execute(
                    "INSERT INTO meta (key, value) VALUES ('boundary', ?1)
                         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                    params![boundary],
                )?;
            }
            tx.event(
                NewEvent::new("host", &host.id, "host.edited")
                    .rev(host.rev + 1)
                    .payload(json!({"changed": changed, "expected_rev": expected_rev})),
            )?;
            get(tx.conn(), "host", &host.id)
        })
    }

    /// `project.add`. Natural: the same values return the existing row. The
    /// cards of a deleted project's id number on from its tombstone.
    pub fn project_add(&mut self, ctx: &Ctx, new: NewProject) -> Result<Applied<Project>> {
        self.project_add_numbered(ctx, new, Numbering::Continue)
    }

    /// The `next_card_seq` where a deleted project with this id stopped, or
    /// `None` when no project with this id was deleted or a live project holds
    /// the id (an add of it replays or is refused, and numbers nothing).
    pub fn deleted_card_seq(&self, project_id: &str) -> Result<Option<i64>> {
        if find::<Project>(self.conn(), "id = ?1", &[&project_id])?.is_some() {
            return Ok(None);
        }
        deleted_card_seq(self.conn(), project_id)
    }

    /// `project.add` with the numbering of a deleted project's id chosen by
    /// the caller, which looked for that project's run branches.
    pub fn project_add_numbered(
        &mut self,
        ctx: &Ctx,
        new: NewProject,
        numbering: Numbering,
    ) -> Result<Applied<Project>> {
        require_owner(&ctx.actor, "project.add")?;
        ids::validate_project_id(&new.id)?;
        if new.id == FLEET_PROJECT {
            return Err(Error::Usage(format!(
                "project id {FLEET_PROJECT} is reserved"
            )));
        }
        required("project name", &new.name)?;
        if let Some(branch) = &new.default_branch {
            required("default branch", branch)?;
        }
        let (new, forge) = resolve_forge(new)?;
        self.write(ctx, "project.add", None, |tx| {
            let host_id = local_host_id(tx.conn())?;
            if let Some(existing) = find::<Project>(tx.conn(), "id = ?1", &[&new.id])? {
                if project_matches(&existing, &new) {
                    return Ok(existing);
                }
                return Err(Error::refused("project", &new.id, "project id is taken"));
            }
            insert_project(tx, &new, &host_id, forge, numbering)?;
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

    /// What `project.retire` would do, without doing it: why it would be
    /// refused, and the agents still running for the project, which the
    /// owner stops or keeps before it retires. Read-only.
    pub fn project_retire_preview(&self, project_id: &str) -> Result<RetirePreview> {
        let project: Project = get(self.conn(), "project", project_id)?;
        let refused = match project.retired_at {
            Some(_) => None,
            None => retire_refusal(self.conn(), &project)?,
        };
        Ok(RetirePreview {
            agents: self.project_live_agents(&project.id)?,
            project: project.id,
            refused,
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
            if let Some(fact) = retire_refusal(tx.conn(), &project)? {
                return Err(Error::refused("project", &project.id, fact));
            }
            tx.conn().execute(
                "UPDATE projects SET retired_at = ?2, rev = rev + 1 WHERE id = ?1",
                params![project.id, tx.now()],
            )?;
            tx.event(
                NewEvent::new("project", &project.id, "project.retired").rev(project.rev + 1),
            )?;
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

/// A `checks` value the project may hold (`CheckPlan`'s two shapes).
fn check_plan(checks: &Value) -> Result<()> {
    CheckPlan::parse(checks).map(drop).map_err(Error::Usage)
}

/// Forge detection at `project.add` (P10): reads the remote URL, fills in
/// `repo` from a GitHub remote when none was given, and refuses a delivery
/// mode Tiller cannot run against a Gerrit remote. Returns the project to
/// add and the forge's name for the `project.added` event. Done before the
/// write so a replay of the same add compares against what was stored.
fn resolve_forge(mut new: NewProject) -> Result<(NewProject, &'static str)> {
    let forge = detect_forge(&new.id, new.remote.as_deref(), new.delivery, &mut new.repo)?;
    Ok((new, forge))
}

/// The forge check `project.add` and `project.edit` share: fills `repo` from
/// a GitHub remote when none is set, and refuses a delivery mode Tiller cannot
/// run against a Gerrit remote. Returns the forge's name.
fn detect_forge(
    id: &str,
    remote: Option<&str>,
    delivery: Delivery,
    repo: &mut Option<String>,
) -> Result<&'static str> {
    let Some(remote) = remote.filter(|remote| !remote.is_empty()) else {
        return Ok("none");
    };
    match forge::detect(remote) {
        Forge::Github { repo: found } => {
            if repo.is_none() {
                *repo = Some(found);
            }
            Ok("github")
        }
        Forge::Gerrit { evidence } => {
            if delivery != Delivery::LocalOnly {
                return Err(Error::Usage(format!(
                    "{evidence}: Gerrit is not a forge Tiller publishes to, so project {id} can only be local-only"
                )));
            }
            Ok("gerrit")
        }
        Forge::Other => Ok("other"),
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
/// deleted project of the same id stopped, so a display id that a run branch
/// may still carry is not minted again (state.md, invariant 9). Import and
/// `project.add` start here unless the add's caller found no such branch.
pub(crate) fn first_card_seq(conn: &rusqlite::Connection, project_id: &str) -> Result<i64> {
    Ok(deleted_card_seq(conn, project_id)?.unwrap_or(1))
}

fn deleted_card_seq(conn: &rusqlite::Connection, project_id: &str) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT max(json_extract(payload, '$.next_card_seq')) FROM events
         WHERE kind = 'project.deleted' AND entity = 'project' AND entity_id = ?1",
        [project_id],
        |row| row.get(0),
    )?)
}

fn insert_project(
    tx: &mut Tx<'_>,
    new: &NewProject,
    host_id: &str,
    forge: &str,
    numbering: Numbering,
) -> Result<()> {
    let capacity = new.capacity.unwrap_or(2);
    positive("capacity", capacity)?;
    let timeout = new.deploy_timeout_minutes.unwrap_or(60);
    positive("deploy timeout", timeout)?;
    let checks = new.checks.clone().unwrap_or_else(|| json!([]));
    check_plan(&checks)?;
    let first_seq = match numbering {
        Numbering::Continue => first_card_seq(tx.conn(), &new.id)?,
        Numbering::Restart => 1,
    };
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
            .payload(json!({"name": new.name, "repo": new.repo, "delivery": new.delivery, "forge": forge})),
    )?;
    Ok(())
}
