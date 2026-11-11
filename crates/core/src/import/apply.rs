//! `import.apply`: writes a plan in one transaction (`state.md` section 8).
//! Every row gets its event under `actor = import`, and one `import.applied`
//! event closes the import. Memory never reaches a canonical file: each
//! placed entry becomes evidence plus an `add` proposal for the owner to
//! approve (`memory.md` section 10).

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use super::bundle::{Bundle, BundleMemory, sha256_hex};
use super::plan::{Action, CardPlan, EMPTY_BLOB, MemoryPlan, Placement, Plan, Wait, plan};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, FLEET_PROJECT};
use crate::state::attention::{NewAttention, open_attention};
use crate::state::projects::{first_card_seq, local_host_id, meta};
use crate::store::{Applied, Ctx, NewEvent, Store, Tx};

/// The host's memory repo, as `memory_sources` records it (`memory.md` section 3).
const MEMORY_REPO: &str = "~/.tiller/memory";

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

fn write_memory(tx: &mut Tx<'_>, plan: &Plan) -> Result<()> {
    let boundary = meta(tx.conn(), "boundary")?.ok_or_else(|| {
        Error::refused(
            "host",
            "local",
            format!("state.db has no boundary; {}", crate::RUN_INIT),
        )
    })?;
    // (source id, base revision id) per target document.
    let mut targets: HashMap<String, (String, String)> = HashMap::new();
    for item in plan.memory.iter().filter(|m| m.action == Action::Create) {
        let entry = item
            .entry
            .as_ref()
            .ok_or_else(|| Error::Internal(format!("memory {} has no entry", item.legacy_id)))?;
        let evidence_id = ids::new_ulid();
        let reference = json!({
            "bundle": {"host_label": plan.host_label, "mode": plan.mode, "manifest_sha256": plan.manifest_sha256},
            "entry": entry.id,
            "source": entry.source,
        });
        tx.conn().execute(
            "INSERT INTO evidence (id, project_id, boundary, kind, ref, excerpt, independence_key, captured_at, captured_by)
             VALUES (?1, ?2, ?3, 'legacy', ?4, ?5, ?6, ?7, ?8)",
            params![
                evidence_id,
                item.project_id,
                boundary,
                reference,
                entry.text,
                format!("legacy:{}", entry.id),
                tx.now(),
                tx.actor().to_string(),
            ],
        )?;
        tx.event(
            NewEvent::new("evidence", &evidence_id, "evidence.captured")
                .payload(json!({"kind": "legacy", "legacy_id": entry.id, "class": entry.class})),
        )?;

        let Placement::Proposal { target, section } = &item.placement else {
            continue;
        };
        let (source_id, revision_id) = match targets.get(target) {
            Some(found) => found.clone(),
            None => {
                let found = document(tx, &item.project_id, &boundary, target)?;
                targets.insert(target.clone(), found.clone());
                found
            }
        };
        propose(
            tx,
            item,
            entry,
            &boundary,
            &source_id,
            &revision_id,
            target,
            section,
            &evidence_id,
        )?;
    }
    Ok(())
}

/// The Tiller document `target` of `project_id`: its source row and the base
/// revision a placement is made against, created as the empty file's
/// `baseline` revision (unreviewed, so it loads nothing) when absent.
fn document(
    tx: &mut Tx<'_>,
    project_id: &str,
    boundary: &str,
    target: &str,
) -> Result<(String, String)> {
    let existing: Option<String> = tx
        .conn()
        .query_row(
            "SELECT id FROM memory_sources
             WHERE project_id = ?1 AND kind = 'tiller_doc' AND canonical_path = ?2 AND retired_at IS NULL",
            params![project_id, target],
            |row| row.get(0),
        )
        .optional()?;
    let source_id = match existing {
        Some(id) => id,
        None => {
            let id = ids::new_ulid();
            let loaded = format!("{MEMORY_REPO}/{target}");
            tx.conn().execute(
                "INSERT INTO memory_sources (id, project_id, boundary, kind, loaded_path, canonical_repo, canonical_path,
                                             apply_method, load_mode, added_at)
                 VALUES (?1, ?2, ?3, 'tiller_doc', ?4, ?5, ?6, 'tiller_commit', 'rendered', ?7)",
                params![id, project_id, boundary, loaded, MEMORY_REPO, target, tx.now()],
            )?;
            tx.event(
                NewEvent::new("memory_source", &id, "memory_source.added")
                    .rev(1)
                    .payload(json!({"project_id": project_id, "kind": "tiller_doc", "canonical_path": target})),
            )?;
            id
        }
    };
    let revision: Option<String> = tx
        .conn()
        .query_row(
            "SELECT id FROM memory_revisions WHERE source_id = ?1 AND blob_sha = ?2 AND installed_sha256 IS NULL",
            params![source_id, EMPTY_BLOB],
            |row| row.get(0),
        )
        .optional()?;
    let revision_id = match revision {
        Some(id) => id,
        None => {
            let id = ids::new_ulid();
            tx.conn().execute(
                "INSERT INTO memory_revisions (id, source_id, blob_sha, review, provenance, scrub, recorded_at, recorded_by)
                 VALUES (?1, ?2, ?3, 'unreviewed', 'baseline', 'passed', ?4, ?5)",
                params![id, source_id, EMPTY_BLOB, tx.now(), tx.actor().to_string()],
            )?;
            tx.event(
                NewEvent::new("memory_revision", &id, "memory_revision.recorded")
                    .payload(json!({"source_id": source_id, "blob_sha": EMPTY_BLOB, "provenance": "baseline"})),
            )?;
            id
        }
    };
    Ok((source_id, revision_id))
}

#[allow(clippy::too_many_arguments)]
fn propose(
    tx: &mut Tx<'_>,
    item: &MemoryPlan,
    entry: &BundleMemory,
    boundary: &str,
    source_id: &str,
    revision_id: &str,
    target: &str,
    section: &str,
    evidence_id: &str,
) -> Result<()> {
    let diff = placement_diff(target, section, &entry.text);
    let scope = if item.project_id == FLEET_PROJECT {
        json!({"kind": "fleet", "value": Value::Null})
    } else {
        json!({"kind": "project", "value": item.project_id})
    };
    let fields = json!({
        "scope": scope,
        "class": entry.class,
        "validity": "durable",
        "section": section,
        "provenance": "legacy-import",
        "legacy_id": entry.id,
        "legacy_section": entry.section,
        "secret_ref": entry.secret_ref,
    });
    let scope_name = if item.project_id == FLEET_PROJECT {
        "the fleet".to_owned()
    } else {
        item.project_id.clone()
    };
    let expected = format!(
        "Firstmate's {} {} loads into Tiller sessions for {scope_name} unchanged, with legacy-import provenance",
        entry.class, entry.id
    );
    let id = ids::new_ulid();
    tx.conn().execute(
        "INSERT INTO proposals (id, project_id, boundary, target_source_id, op, base_revision_id, base_blob, diff,
                                diff_sha256, memory_fields, rationale, expected_effect, created_at, created_by)
         VALUES (?1, ?2, ?3, ?4, 'add', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            item.project_id,
            boundary,
            source_id,
            revision_id,
            EMPTY_BLOB,
            diff,
            sha256_hex(diff.as_bytes()),
            fields,
            entry.rationale.as_deref().filter(|r| !r.trim().is_empty()),
            expected,
            tx.now(),
            tx.actor().to_string(),
        ],
    )?;
    tx.conn().execute(
        "INSERT INTO proposal_evidence (proposal_id, evidence_id) VALUES (?1, ?2)",
        params![id, evidence_id],
    )?;
    tx.event(
        NewEvent::new("proposal", &id, "proposal.created")
            .rev(1)
            .payload(json!({"op": "add", "target": target, "legacy_id": entry.id, "evidence": [evidence_id]})),
    )?;
    Ok(())
}

/// The unified diff that adds one entry, under its section, to the empty
/// document: one list item, continuation lines indented under it.
pub(crate) fn placement_diff(target: &str, section: &str, text: &str) -> String {
    let mut lines = vec![format!("## {section}"), String::new()];
    for (n, line) in text.trim_end().lines().enumerate() {
        lines.push(match (n, line.is_empty()) {
            (0, _) => format!("- {line}"),
            (_, true) => String::new(),
            _ => format!("  {line}"),
        });
    }
    let mut diff = format!(
        "--- a/{target}\n+++ b/{target}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for line in lines {
        diff.push('+');
        diff.push_str(&line);
        diff.push('\n');
    }
    diff
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
    let mut memory = 0;
    for entry in &bundle.memory {
        let found: bool = tx.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM evidence WHERE kind = 'legacy'
                             AND json_extract(ref, '$.entry') = ?1 AND excerpt = ?2)",
            params![entry.id, entry.text],
            |row| row.get(0),
        )?;
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
