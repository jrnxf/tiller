//! `import.apply`: writes a plan in one transaction (`state.md` section 8).
//! Every row gets its event under `actor = import`, and one `import.applied`
//! event closes the import. Memory never reaches kb: each placed entry
//! becomes a pending note (`source = migration`) for the owner to keep or
//! discard (`memory.md` section 4).

use std::collections::HashMap;

use rusqlite::params;
use serde_json::{Value, json};

use super::bundle::Bundle;
use super::plan::{Action, CardPlan, Placement, Plan, Wait, imported, note_title, plan};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids;
use crate::state::attention::{NewAttention, open_attention};
use crate::state::projects::{first_card_seq, local_host_id};
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};

impl Store {
    /// A dry run: the plan a real import would apply now, writing nothing.
    pub fn import_plan(&self, bundle: &Bundle, now: crate::Timestamp) -> Result<Plan> {
        plan(self.conn(), bundle, now)
    }

    /// `import.apply`. Natural: a bundle whose manifest is already
    /// `meta.import_manifest_sha256`, or whose every record is already
    /// present (a re-export), writes nothing and returns its plan; any other
    /// bundle skips every project, card and memory entry already present, so
    /// a second import changes nothing.
    pub fn import_apply(&mut self, ctx: &Ctx, bundle: &Bundle) -> Result<Applied<Plan>> {
        self.write(ctx, "import.apply", None, |tx| {
            let plan = plan(tx.conn(), bundle, tx.now())?;
            if plan.already_imported || plan.writes_nothing() {
                return Ok(plan);
            }
            if !plan.ok() {
                let failed: Vec<String> = plan
                    .checks
                    .iter()
                    .filter(|c| !c.ok)
                    .map(|c| format!("{}: manifest {}, planned {}", c.name, c.manifest, c.planned))
                    .collect();
                return Err(Error::refused(
                    "bundle",
                    &plan.manifest_sha256,
                    failed.join("; "),
                ));
            }
            write_plan(tx, bundle, &plan)?;
            Ok(plan)
        })
    }
}

fn write_plan(tx: &mut Tx<'_>, bundle: &Bundle, plan: &Plan) -> Result<()> {
    let host_id = local_host_id(tx.conn())?;
    let now = tx.now();

    for project in plan.projects.iter().filter(|p| p.action == Action::Create) {
        tx.conn().execute(
            "INSERT INTO projects (id, name, repo, remote, default_branch, delivery, capacity, owner_host_id,
                                   added_at, next_card_seq, checks)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                project.id,
                project.legacy_id,
                project.repo,
                project.remote,
                project.default_branch.as_deref().unwrap_or("main"),
                project.delivery,
                project.capacity.unwrap_or(2),
                host_id,
                project.added_at.unwrap_or(now),
                first_card_seq(tx.conn(), &project.id)?,
                project.checks.clone().unwrap_or_else(|| json!([])),
            ],
        )?;
        tx.event(
            NewEvent::new("project", &project.id, "project.added")
                .rev(1)
                .payload(json!({
                    "name": project.legacy_id,
                    "repo": project.repo,
                    "delivery": project.delivery,
                    "legacy_id": project.legacy_id,
                })),
        )?;
    }

    // Card ids by legacy id, for the cards this import writes and those it finds.
    let mut card_ids: HashMap<&str, String> = HashMap::new();
    let mut created_per_project: HashMap<&str, i64> = HashMap::new();
    for card in &plan.cards {
        let id = match card.action {
            Action::Exists => tx.conn().query_row(
                "SELECT id FROM cards WHERE display_id = ?1",
                [&card.display_id],
                |row| row.get(0),
            )?,
            Action::Create => {
                *created_per_project
                    .entry(card.project_id.as_str())
                    .or_default() += 1;
                insert_card(tx, card)?
            }
        };
        card_ids.insert(card.legacy_id.as_str(), id);
    }
    for (project_id, created) in &created_per_project {
        tx.conn().execute(
            "UPDATE projects SET next_card_seq = next_card_seq + ?2, rev = rev + 1, edited_at = ?3
             WHERE id = ?1",
            params![project_id, created, now],
        )?;
    }

    for card in plan.cards.iter().filter(|c| c.action == Action::Create) {
        let card_id = card_ids
            .get(card.legacy_id.as_str())
            .cloned()
            .ok_or_else(|| Error::Internal(format!("card {} was not written", card.legacy_id)))?;
        for wait in &card.waits {
            write_wait(tx, &card_id, wait)?;
        }
        if let Some(pr) = &card.pr
            && pr.action == Action::Create
            && pr.not_linked.is_none()
            && let (Some(number), Some(project_id)) = (pr.number, &pr.project_id)
        {
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO prs (id, project_id, card_id, number, url, linked_at, linked_by, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![id, project_id, card_id, number, pr.url, now, tx.actor().to_string(), pr.state],
            )?;
            tx.event(
                NewEvent::new("pr", &id, "pr.linked")
                    .card(&card_id)
                    .payload(json!({"number": number, "url": pr.url, "state": pr.state})),
            )?;
        }
    }

    for project in plan.projects.iter().filter(|p| p.retire) {
        let rev: i64 = tx.conn().query_row(
            "UPDATE projects SET retired_at = ?2, rev = rev + 1, edited_at = ?2 WHERE id = ?1 RETURNING rev",
            params![project.id, now],
            |row| row.get(0),
        )?;
        tx.event(
            NewEvent::new("project", &project.id, "project.retired")
                .rev(rev)
                .payload(json!({"reason": "named only by imported history"})),
        )?;
    }

    write_memory(tx, plan)?;

    tx.conn().execute(
        "INSERT INTO meta (key, value) VALUES ('import_manifest_sha256', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [&plan.manifest_sha256],
    )?;

    verify(tx, bundle, plan)?;

    let created = |action: Action| plan.cards.iter().filter(|c| c.action == action).count();
    tx.event(
        NewEvent::new("import", &plan.manifest_sha256, "import.applied").payload(json!({
            "schema": plan.schema,
            "mode": plan.mode,
            "host_label": plan.host_label,
            "generated_at": plan.generated_at,
            "checks": plan.checks,
            "columns": plan.columns,
            "projects_created": plan.projects.iter().filter(|p| p.action == Action::Create).count(),
            "cards_created": created(Action::Create),
            "cards_existing": created(Action::Exists),
            "requirements": plan.memory.iter()
                .filter(|m| m.placement == Placement::Requirement)
                .map(|m| &m.legacy_id)
                .collect::<Vec<_>>(),
            "not_placed": plan.memory.iter()
                .filter_map(|m| match &m.placement {
                    Placement::NotPlaced { reason } => Some(json!({"legacy_id": m.legacy_id, "reason": reason})),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            "inflight": plan.inflight,
        })),
    )?;
    Ok(())
}

fn insert_card(tx: &mut Tx<'_>, card: &CardPlan) -> Result<String> {
    let id = ids::new_ulid();
    let closed_by = (card.state == CardState::Closed).then(|| tx.actor().to_string());
    tx.conn().execute(
        "INSERT INTO cards (id, display_id, legacy_id, project_id, kind, title, body, priority, category, size,
                            state, outcome, closed_at, closed_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            id,
            card.display_id,
            card.legacy_id,
            card.project_id,
            card.kind,
            card.title,
            card.detail.body,
            card.priority,
            card.detail.category,
            card.detail.size,
            card.state,
            card.outcome,
            card.closed_at,
            closed_by,
            card.created_at,
        ],
    )?;
    tx.event(
        NewEvent::new("card", &id, "card.created")
            .rev(1)
            .card(&id)
            .payload(json!({
                "display_id": card.display_id,
                "project_id": card.project_id,
                "kind": card.kind,
                "title": card.title,
                "ask_id": Value::Null,
                "legacy_id": card.legacy_id,
                "imported": {
                    "column": card.bundle_column,
                    "since": card.detail.since,
                    "state": card.state,
                    "outcome": card.outcome,
                    "bundle_outcome": card.bundle_outcome,
                    "closed_at": card.closed_at,
                    "pr": card.pr.as_ref().map(|pr| &pr.url),
                    "runs": card.detail.runs,
                    "firstmate_legacy_id": card.detail.firstmate_legacy_id,
                },
            })),
    )?;
    Ok(id)
}

fn write_wait(tx: &mut Tx<'_>, card_id: &str, wait: &Wait) -> Result<()> {
    match wait {
        Wait::Attention { kind, question } => {
            open_attention(
                tx,
                &NewAttention {
                    card: card_id.to_owned(),
                    kind: *kind,
                    question: question.clone(),
                    ..NewAttention::default()
                },
            )?;
        }
        Wait::Hold { until_at, reason } => {
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO holds (id, card_id, until_at, reason, set_at, set_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, card_id, until_at, reason, tx.now(), tx.actor().to_string()],
            )?;
            tx.event(
                NewEvent::new("hold", &id, "hold.set")
                    .card(card_id)
                    .payload(json!({"until_at": until_at, "reason": reason})),
            )?;
        }
        Wait::Dependency {
            display_id,
            released,
            ..
        } => {
            let target: String = tx.conn().query_row(
                "SELECT id FROM cards WHERE display_id = ?1",
                [display_id],
                |row| row.get(0),
            )?;
            let released_at = released.map(|_| tx.now());
            tx.conn().execute(
                "INSERT INTO card_deps (card_id, depends_on_card_id, added_at, added_by, released_at, release_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (card_id, depends_on_card_id) DO NOTHING",
                params![card_id, target, tx.now(), tx.actor().to_string(), released_at, released],
            )?;
            tx.event(
                NewEvent::new("dep", format!("{card_id}:{target}"), "dep.added")
                    .card(card_id)
                    .payload(json!({"depends_on": target, "depends_on_display_id": display_id, "released": released})),
            )?;
        }
    }
    Ok(())
}

/// One pending note per placed entry. The limits of `mem.note` do not hold
/// here, as they do not for the schema migration: the owner approved this
/// memory once already.
fn write_memory(tx: &mut Tx<'_>, plan: &Plan) -> Result<()> {
    for item in plan.memory.iter().filter(|m| m.action == Action::Create) {
        let Placement::Pending { kind } = &item.placement else {
            continue;
        };
        let entry = item
            .entry
            .as_ref()
            .ok_or_else(|| Error::Internal(format!("memory {} has no entry", item.legacy_id)))?;
        let id = ids::new_ulid();
        tx.conn().execute(
            "INSERT INTO memory_pending (id, project_id, kind, title, body, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'migration', ?6)",
            params![
                id,
                item.project_id,
                kind,
                note_title(&entry.text),
                entry.text.trim(),
                tx.now()
            ],
        )?;
        tx.event(
            NewEvent::new("memory_pending", &id, "memory.noted")
                .rev(1)
                .payload(json!({
                    "pending_id": id,
                    "project_id": item.project_id,
                    "source": "migration",
                    "legacy_id": entry.id,
                })),
        )?;
    }
    Ok(())
}

/// After the writes, inside the transaction: every bundle card, project and
/// memory entry has its row, or the import rolls back.
fn verify(tx: &Tx<'_>, bundle: &Bundle, plan: &Plan) -> Result<()> {
    let mut cards = 0;
    for card in &plan.cards {
        let found: bool = tx.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM cards WHERE project_id = ?1 AND legacy_id = ?2)",
            params![card.project_id, card.legacy_id],
            |row| row.get(0),
        )?;
        cards += usize::from(found);
    }
    let mut projects = 0;
    for project in plan.projects.iter().filter(|p| p.in_bundle_projects) {
        let found: bool = tx.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM projects WHERE id = ?1)",
            [&project.id],
            |row| row.get(0),
        )?;
        projects += usize::from(found);
    }
    // An entry the import lists and does not place is recorded by the
    // `import.applied` event this transaction writes next.
    let mut memory = 0;
    for item in &plan.memory {
        let found = match item.placement {
            Placement::Pending { .. } => imported(tx.conn(), &item.legacy_id, &item.placement)?,
            Placement::Requirement | Placement::NotPlaced { .. } => true,
        };
        memory += usize::from(found);
    }
    let counts = &bundle.manifest.counts;
    for (what, manifest, written) in [
        ("cards", counts.cards, cards),
        ("projects", counts.projects, projects),
        ("memory", counts.memory, memory),
    ] {
        if manifest != written {
            return Err(Error::Internal(format!(
                "import wrote {written} {what} of the manifest's {manifest}; rolled back"
            )));
        }
    }
    Ok(())
}
