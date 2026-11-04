//! The import plan: every row a real run would write, worked out from the
//! bundle and the current `state.db` without writing anything. A dry run
//! prints it; a real run applies exactly it (`apply.rs`).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::bundle::{Bundle, BundleCard, BundleMemory};
use crate::domain::lifecycle::{allowed_outcomes, outcome_allowed, outcome_satisfies_dependants};
use crate::domain::*;
use crate::error::{Error, Result};
use crate::ids::{self, FLEET_PROJECT};
use crate::state::find;
use crate::state::projects::{first_card_seq, meta};
use crate::time::Timestamp;

/// The git blob of an empty file: the base every legacy placement diff is
/// made against, since the importer runs before any Tiller document exists.
pub const EMPTY_BLOB: &str = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";

/// The document section a placed entry goes under (`memory.md` section 3).
pub const RULE_SECTION: &str = "How we work here";
pub const NOTE_SECTION: &str = "Watch out for";

/// What the bundle holds that the importer does not write, and where it goes.
const NOT_IMPORTED: &[&str] = &[
    "inflight/: live work is drained, adopted or respawned at cutover, not imported as runs",
    "tasks/: per-task records stay in the bundle; cards point at their legacy id",
    "card runs: kept in each card.created event, since a run row needs a Tiller session",
    "capabilities.md, regressions.md, memory/requirements.md: inputs to the parity record",
    "config.json, commitments.jsonl, handoff-letter.md, open-questions.md, CUTOVER.md: read by the owner at cutover",
    "baseline/: the historical baseline, read by the T6 comparison",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Written by this import.
    Create,
    /// Already in `state.db` (an earlier import, or the owner added it); left as it is.
    Exists,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub schema: String,
    pub mode: String,
    pub host_label: String,
    pub generated_at: Option<String>,
    pub manifest_sha256: String,
    /// The same manifest was imported before: a real run writes nothing.
    pub already_imported: bool,
    /// The count checks: each bundle count against what the plan accounts for.
    pub checks: Vec<Check>,
    /// Cards per bundle column and the Tiller column they land in.
    pub columns: BTreeMap<String, BTreeMap<String, usize>>,
    pub projects: Vec<ProjectPlan>,
    pub cards: Vec<CardPlan>,
    pub memory: Vec<MemoryPlan>,
    pub inflight: Vec<InflightPlan>,
    pub not_imported: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub manifest: usize,
    pub planned: usize,
    pub ok: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectPlan {
    /// The bundle's project id; `id` differs when it is not a valid slug.
    pub legacy_id: String,
    pub id: String,
    pub action: Action,
    pub delivery: Delivery,
    pub repo: Option<String>,
    pub remote: Option<String>,
    pub default_branch: Option<String>,
    pub capacity: Option<i64>,
    pub added_at: Option<Timestamp>,
    /// False for a project only cards or memory name; it is created retired
    /// when none of its cards is open.
    pub in_bundle_projects: bool,
    pub retire: bool,
    pub cards: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardPlan {
    pub legacy_id: String,
    pub project_id: String,
    pub display_id: String,
    pub action: Action,
    pub kind: CardKind,
    pub title: String,
    pub priority: i64,
    pub state: CardState,
    pub outcome: Option<CardOutcome>,
    /// The bundle's done outcome, as Firstmate wrote it.
    pub bundle_outcome: Option<String>,
    pub created_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub bundle_column: String,
    /// The column `card_column` derives once the import is written.
    pub column: String,
    pub waits: Vec<Wait>,
    pub pr: Option<PrPlan>,
    #[serde(skip)]
    pub(crate) detail: CardDetail,
}

/// What a card carries that the plan does not print.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CardDetail {
    pub body: Option<String>,
    pub category: Option<String>,
    pub size: Option<Size>,
    pub runs: Vec<Value>,
    pub since: Option<String>,
    pub firstmate_legacy_id: Option<String>,
}

/// Why an open card waits, as Tiller records it (`state.md`, Hold and Attention).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "snake_case")]
pub enum Wait {
    /// An undated wait for the owner's word.
    Attention {
        kind: AttentionKind,
        question: String,
    },
    /// A dated park that re-asks on its date.
    Hold { until_at: Timestamp, reason: String },
    /// A blocked wait: a dependency on another card.
    Dependency {
        on: String,
        display_id: String,
        released: Option<DepReleaseReason>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrPlan {
    pub url: String,
    pub number: Option<i64>,
    /// The project whose repo holds the PR, which the `prs` row is filed under.
    pub project_id: Option<String>,
    pub state: PrState,
    pub action: Action,
    /// Why the PR is not linked as a row, when it is not; the URL stays in the
    /// card's event either way.
    pub not_linked: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MemoryPlan {
    pub legacy_id: String,
    pub class: String,
    pub project_id: String,
    pub placement: Placement,
    pub action: Action,
    #[serde(skip)]
    pub(crate) entry: Option<BundleMemory>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "as", rename_all = "snake_case")]
pub enum Placement {
    /// A proposal adding the entry to a Tiller document, for the owner to approve.
    Proposal { target: String, section: String },
    /// A requirement on Tiller's own mechanics: a parity record row, not memory.
    Requirement,
    /// Listed and not placed.
    NotPlaced { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InflightPlan {
    pub legacy_id: String,
    pub display_id: Option<String>,
    pub recommendation: Option<String>,
}

impl Plan {
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|check| check.ok)
    }

    /// True when every project, card and memory entry is already in `state.db`.
    pub fn writes_nothing(&self) -> bool {
        self.projects.iter().all(|p| p.action == Action::Exists)
            && self.cards.iter().all(|c| c.action == Action::Exists)
            && self.memory.iter().all(|m| m.action == Action::Exists)
    }
}

/// Works out the plan for `bundle` against the database behind `conn`.
pub fn plan(conn: &Connection, bundle: &Bundle, now: Timestamp) -> Result<Plan> {
    let boundary = meta(conn, "boundary")?.ok_or_else(|| {
        Error::refused(
            "host",
            "local",
            "state.db is not initialized; run tiller init",
        )
    })?;
    if let Some(theirs) = &bundle.manifest.boundary
        && *theirs != boundary
    {
        return Err(Error::refused(
            "bundle",
            &bundle.manifest_sha256,
            format!("the bundle's boundary is {theirs}, this host's is {boundary}"),
        ));
    }
    let already_imported =
        meta(conn, "import_manifest_sha256")?.as_deref() == Some(bundle.manifest_sha256.as_str());

    let slugs = project_slugs(conn, bundle)?;
    let mut cards = plan_cards(conn, bundle, &slugs, now)?;
    let projects = plan_projects(conn, bundle, &slugs, &cards)?;
    assign_display_ids(conn, &projects, &mut cards)?;
    resolve_dependencies(conn, bundle, &mut cards)?;
    let memory = plan_memory(conn, bundle, &slugs)?;
    check_memory_targets(conn, &memory)?;

    let by_legacy: HashMap<&str, &CardPlan> = cards
        .iter()
        .map(|card| (card.legacy_id.as_str(), card))
        .collect();
    let inflight: Vec<InflightPlan> = bundle
        .inflight
        .iter()
        .map(|item| InflightPlan {
            legacy_id: item.id.clone(),
            display_id: by_legacy
                .get(item.id.as_str())
                .map(|card| card.display_id.clone()),
            recommendation: item.recommendation.clone(),
        })
        .collect();

    let mut columns: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for card in &cards {
        *columns
            .entry(card.bundle_column.clone())
            .or_default()
            .entry(card.column.clone())
            .or_default() += 1;
    }

    let counts = &bundle.manifest.counts;
    let placed = memory
        .iter()
        .filter(|m| matches!(m.placement, Placement::Proposal { .. }))
        .count();
    let requirements = memory
        .iter()
        .filter(|m| m.placement == Placement::Requirement)
        .count();
    let not_placed = memory
        .iter()
        .filter(|m| matches!(m.placement, Placement::NotPlaced { .. }))
        .count();
    let checks = vec![
        check(
            "projects",
            counts.projects,
            projects.iter().filter(|p| p.in_bundle_projects).count(),
        ),
        check("cards", counts.cards, cards.len()),
        check(
            "memory: placed + requirements + not placed",
            counts.memory,
            placed + requirements + not_placed,
        ),
        check(
            "inflight: matched to a card",
            counts.inflight,
            inflight.iter().filter(|i| i.display_id.is_some()).count(),
        ),
    ];

    Ok(Plan {
        schema: bundle.manifest.schema.clone(),
        mode: bundle.manifest.mode.clone(),
        host_label: bundle.manifest.host_label.clone(),
        generated_at: bundle.manifest.generated_at.clone(),
        manifest_sha256: bundle.manifest_sha256.clone(),
        already_imported,
        checks,
        columns,
        projects,
        cards,
        memory,
        inflight,
        not_imported: NOT_IMPORTED.iter().map(|line| (*line).to_owned()).collect(),
    })
}

fn check(name: &str, manifest: usize, planned: usize) -> Check {
    Check {
        name: name.to_owned(),
        manifest,
        planned,
        ok: manifest == planned,
    }
}

/// Maps every project id the bundle names (in `projects.json`, on a card or
/// in a memory scope) to a Tiller slug. A valid slug is kept; any other id is
/// lowercased, stripped to letters and digits and cut to 16 characters, with
/// a number added when that collides, including with a project in `state.db`
/// that an earlier import of the same id did not create.
fn project_slugs(conn: &Connection, bundle: &Bundle) -> Result<BTreeMap<String, String>> {
    let mut named: Vec<String> = bundle.projects.iter().map(|p| p.id.clone()).collect();
    let mut extra = BTreeSet::new();
    for card in &bundle.cards {
        extra.insert(card.project.clone());
    }
    for entry in &bundle.memory {
        if let Some(project) = memory_scope(entry)? {
            extra.insert(project.to_owned());
        }
    }
    for id in extra {
        if !named.contains(&id) {
            named.push(id);
        }
    }

    let mut taken: BTreeSet<String> = BTreeSet::from([FLEET_PROJECT.to_owned()]);
    let mut slugs = BTreeMap::new();
    for id in &named {
        if id == FLEET_PROJECT || ids::validate_project_id(id).is_ok() {
            taken.insert(id.clone());
            slugs.insert(id.clone(), id.clone());
        }
    }
    for id in &named {
        if slugs.contains_key(id) {
            continue;
        }
        let mut base: String = id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if !base.starts_with(|c: char| c.is_ascii_lowercase()) {
            base.insert(0, 'p');
        }
        while base.len() < 2 {
            base.push('p');
        }
        base.truncate(16);
        let foreign = |slug: &str| -> Result<bool> {
            Ok(find::<Project>(conn, "id = ?1", &[&slug])?.is_some_and(|p| p.name != *id))
        };
        let mut slug = base.clone();
        let mut n = 2;
        while taken.contains(&slug) || foreign(&slug)? {
            let suffix = n.to_string();
            slug = format!("{}{suffix}", &base[..base.len().min(16 - suffix.len())]);
            n += 1;
        }
        taken.insert(slug.clone());
        slugs.insert(id.clone(), slug);
    }
    Ok(slugs)
}

/// The project a memory entry is scoped to, or `None` for `global`.
fn memory_scope(entry: &BundleMemory) -> Result<Option<&str>> {
    if entry.scope == "global" {
        return Ok(None);
    }
    match entry.scope.strip_prefix("project:") {
        Some(project) if !project.is_empty() => Ok(Some(project)),
        _ => Err(Error::refused(
            "memory",
            &entry.id,
            format!("scope {:?} is neither global nor project:<id>", entry.scope),
        )),
    }
}

fn plan_projects(
    conn: &Connection,
    bundle: &Bundle,
    slugs: &BTreeMap<String, String>,
    cards: &[CardPlan],
) -> Result<Vec<ProjectPlan>> {
    let mut out = Vec::new();
    for (legacy_id, id) in slugs {
        let from_bundle = bundle.projects.iter().find(|p| &p.id == legacy_id);
        let exists = find::<Project>(conn, "id = ?1", &[id])?.is_some();
        if id == FLEET_PROJECT && !exists {
            return Err(Error::refused(
                "project",
                FLEET_PROJECT,
                "state.db has no fleet project; run tiller init",
            ));
        }
        let own_cards: Vec<&CardPlan> = cards.iter().filter(|c| &c.project_id == id).collect();
        let (delivery, repo, remote, default_branch, capacity, added_at) = match from_bundle {
            Some(p) => (
                p.delivery
                    .parse::<Delivery>()
                    .map_err(|error| Error::refused("project", legacy_id, error.to_string()))?,
                p.repo.clone().filter(|r| !r.is_empty()),
                p.remote.clone().filter(|r| !r.is_empty()),
                p.default_branch.clone().filter(|b| !b.is_empty()),
                p.capacity,
                p.added.as_deref().map(bundle_time).transpose()?,
            ),
            None => (Delivery::LocalOnly, None, None, None, None, None),
        };
        if let Some(n) = capacity
            && n < 1
        {
            return Err(Error::refused(
                "project",
                legacy_id,
                format!("capacity {n} is below 1"),
            ));
        }
        out.push(ProjectPlan {
            legacy_id: legacy_id.clone(),
            id: id.clone(),
            action: if exists {
                Action::Exists
            } else {
                Action::Create
            },
            delivery,
            repo,
            remote,
            default_branch,
            capacity,
            added_at,
            in_bundle_projects: from_bundle.is_some(),
            retire: !exists
                && from_bundle.is_none()
                && own_cards.iter().all(|c| c.state == CardState::Closed),
            cards: own_cards.len(),
        });
    }
    // projects.json order first, then the ones only cards or memory name.
    out.sort_by_key(|p| {
        (
            bundle
                .projects
                .iter()
                .position(|b| b.id == p.legacy_id)
                .unwrap_or(usize::MAX),
            p.legacy_id.clone(),
        )
    });
    Ok(out)
}

fn plan_cards(
    conn: &Connection,
    bundle: &Bundle,
    slugs: &BTreeMap<String, String>,
    now: Timestamp,
) -> Result<Vec<CardPlan>> {
    let repos = project_repos(conn, bundle, slugs)?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(bundle.cards.len());
    for card in &bundle.cards {
        if !seen.insert(card.id.as_str()) {
            return Err(Error::refused(
                "card",
                &card.id,
                "the bundle lists this card twice",
            ));
        }
        out.push(plan_card(conn, &repos, card, slugs, now)?);
    }
    Ok(out)
}

fn plan_card(
    conn: &Connection,
    repos: &Repos,
    card: &BundleCard,
    slugs: &BTreeMap<String, String>,
    now: Timestamp,
) -> Result<CardPlan> {
    let refuse = |fact: String| Error::refused("card", &card.id, fact);
    if card.title.trim().is_empty() {
        return Err(refuse("the card has no title".into()));
    }
    let project_id = slugs
        .get(&card.project)
        .cloned()
        .ok_or_else(|| refuse(format!("project {} has no slug", card.project)))?;
    let kind = match card.kind.as_str() {
        "ship" => CardKind::Ship,
        "scout" => CardKind::Scout,
        "captain" => CardKind::Question,
        "other" => CardKind::Ops,
        other => return Err(refuse(format!("kind {other:?} has no Tiller kind"))),
    };
    let priority = card.priority.unwrap_or(2);
    if !(0..=4).contains(&priority) {
        return Err(refuse(format!("priority {priority} is outside 0 to 4")));
    }
    let size = card
        .size
        .as_deref()
        .map(str::parse::<Size>)
        .transpose()
        .map_err(|error| refuse(error.to_string()))?;
    let created_at = card
        .since
        .as_deref()
        .map(bundle_time)
        .transpose()?
        .unwrap_or(now);

    let (state, outcome, closed_at) = match (&card.done, card.column.as_str()) {
        (Some(done), _) => {
            let delivered = match done.outcome.as_str() {
                "merged" => CardOutcome::Landed,
                "reported" => CardOutcome::Reported,
                "answered" => CardOutcome::Answered,
                "completed" => CardOutcome::Completed,
                "cancelled" | "dropped" => CardOutcome::Cancelled,
                "failed" => CardOutcome::Failed,
                "abandoned" => CardOutcome::Abandoned,
                other => {
                    return Err(refuse(format!(
                        "done outcome {other:?} has no Tiller outcome"
                    )));
                }
            };
            // Firstmate closed any kind with any outcome; a delivered outcome
            // another kind owns becomes this kind's own (`allowed_outcomes`).
            let outcome = if outcome_allowed(kind, delivered) {
                delivered
            } else {
                allowed_outcomes(kind).first().copied().unwrap_or(delivered)
            };
            let at = done
                .at
                .as_deref()
                .map(bundle_time)
                .transpose()?
                .unwrap_or(now);
            (CardState::Closed, Some(outcome), Some(at))
        }
        (None, "done") => {
            return Err(refuse(
                "the column is done but the card has no done record".into(),
            ));
        }
        (None, "asked" | "queued" | "working" | "needs" | "landing") => {
            (CardState::Open, None, None)
        }
        (None, other) => {
            return Err(refuse(format!(
                "column {other:?} is not a tiller-import/v1 column"
            )));
        }
    };

    let mut waits = Vec::new();
    if state == CardState::Open {
        let reason = card
            .hold
            .as_ref()
            .and_then(|hold| hold.reason.clone())
            .filter(|reason| !reason.trim().is_empty());
        match card.hold.as_ref().map(|hold| hold.kind.as_str()) {
            // A blocked wait becomes its dependencies; with none it is the owner's call.
            Some("blocked") if !card.depends_on.is_empty() => {}
            Some(hold_kind) => {
                let until = card
                    .hold
                    .as_ref()
                    .and_then(|hold| hold.until.as_deref())
                    .map(bundle_time)
                    .transpose()?;
                match until {
                    Some(until_at) if until_at > now => waits.push(Wait::Hold {
                        until_at,
                        reason: reason.unwrap_or_else(|| "parked in Firstmate".to_owned()),
                    }),
                    _ => waits.push(Wait::Attention {
                        kind: if hold_kind == "credential" {
                            AttentionKind::Credential
                        } else {
                            AttentionKind::Decision
                        },
                        question: reason.unwrap_or_else(|| NO_REASON.to_owned()),
                    }),
                }
            }
            None if card.column == "needs" => waits.push(Wait::Attention {
                kind: if card.pr.is_some() && kind == CardKind::Ship {
                    AttentionKind::Merge
                } else {
                    AttentionKind::Decision
                },
                question: NO_REASON.to_owned(),
            }),
            None => {}
        }
    }

    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT display_id, project_id FROM cards WHERE project_id = ?1 AND legacy_id = ?2",
            params![project_id, card.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let action = if existing.is_some() {
        Action::Exists
    } else {
        Action::Create
    };
    // Closed history may sit in a retired project; open work may not (`card.create`).
    if action == Action::Create
        && state == CardState::Open
        && find::<Project>(conn, "id = ?1", &[&project_id])?.is_some_and(|p| p.retired_at.is_some())
    {
        return Err(refuse(format!("project {project_id} is retired")));
    }

    let merged = card
        .done
        .as_ref()
        .is_some_and(|done| done.outcome == "merged");
    let pr = card
        .pr
        .as_deref()
        .filter(|url| !url.is_empty())
        .map(|url| plan_pr(conn, repos, &project_id, url, state, merged))
        .transpose()?;

    let body = match &card.body {
        Some(Value::Array(lines)) => {
            let text: Vec<&str> = lines.iter().filter_map(Value::as_str).collect();
            Some(text.join("\n"))
        }
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    }
    .filter(|text| !text.trim().is_empty());

    Ok(CardPlan {
        legacy_id: card.id.clone(),
        project_id,
        display_id: existing
            .map(|(display_id, _)| display_id)
            .unwrap_or_default(),
        action,
        kind,
        title: card.title.clone(),
        priority,
        state,
        outcome,
        bundle_outcome: card.done.as_ref().map(|done| done.outcome.clone()),
        created_at,
        closed_at,
        bundle_column: card.column.clone(),
        column: column(state, &waits),
        waits,
        pr,
        detail: CardDetail {
            body,
            category: card.category.clone(),
            size,
            runs: card.runs.clone(),
            since: card.since.clone(),
            firstmate_legacy_id: card.legacy_id.clone().filter(|id| *id != card.id),
        },
    })
}

/// The question an undated wait asks when Firstmate recorded no reason.
const NO_REASON: &str = "Firstmate had this card waiting on the owner with no recorded reason; answer it or close the card.";

/// The `card_column` value the card derives once written: no run is
/// imported, so an open card is Needs you when it has a question and Queued
/// otherwise.
fn column(state: CardState, waits: &[Wait]) -> String {
    if state == CardState::Closed {
        "done"
    } else if waits.iter().any(|w| matches!(w, Wait::Attention { .. })) {
        "needs_you"
    } else {
        "queued"
    }
    .to_owned()
}

fn plan_pr(
    conn: &Connection,
    repos: &Repos,
    project_id: &str,
    url: &str,
    state: CardState,
    merged: bool,
) -> Result<PrPlan> {
    let pr_state = match (state, merged) {
        (CardState::Open, _) => PrState::Open,
        (_, true) => PrState::Merged,
        _ => PrState::Closed,
    };
    let parsed = parse_pr_url(url);
    let mut plan = PrPlan {
        url: url.to_owned(),
        number: parsed.as_ref().map(|(_, n)| *n),
        project_id: None,
        state: pr_state,
        action: Action::Create,
        not_linked: None,
    };
    let Some((pr_repo, number)) = parsed else {
        plan.not_linked = Some("not a GitHub pull request URL".into());
        return Ok(plan);
    };
    // The card's own project when its repo holds the PR, else the project
    // whose repo does: a fleet card's PR lands in some project's repo.
    let owners = repos.get(&pr_repo.to_ascii_lowercase());
    let owner = owners.and_then(|ids| {
        ids.iter()
            .find(|id| id.as_str() == project_id)
            .or_else(|| ids.first())
    });
    let Some(owner) = owner else {
        plan.not_linked = Some(format!(
            "no imported or existing project has the repo {pr_repo}"
        ));
        return Ok(plan);
    };
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM prs WHERE project_id = ?1 AND number = ?2)",
        params![owner, number],
        |row| row.get(0),
    )?;
    if exists {
        plan.action = Action::Exists;
    }
    plan.project_id = Some(owner.clone());
    Ok(plan)
}

/// Project slugs by lowercase `owner/name` repo, bundle projects first.
type Repos = BTreeMap<String, Vec<String>>;

fn project_repos(
    conn: &Connection,
    bundle: &Bundle,
    slugs: &BTreeMap<String, String>,
) -> Result<Repos> {
    let mut repos = Repos::new();
    for project in &bundle.projects {
        if let (Some(repo), Some(slug)) = (&project.repo, slugs.get(&project.id))
            && !repo.is_empty()
        {
            repos
                .entry(repo.to_ascii_lowercase())
                .or_default()
                .push(slug.clone());
        }
    }
    let mut stmt =
        conn.prepare("SELECT id, repo FROM projects WHERE repo IS NOT NULL ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (id, repo) = row?;
        let ids = repos.entry(repo.to_ascii_lowercase()).or_default();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(repos)
}

/// `https://github.com/<owner>/<name>/pull/<n>` as (`owner/name`, n).
fn parse_pr_url(url: &str) -> Option<(String, i64)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let (owner, name, pull, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || pull != "pull" || owner.is_empty() || name.is_empty() {
        return None;
    }
    let number: i64 = number.parse().ok().filter(|n| *n > 0)?;
    Some((format!("{owner}/{name}"), number))
}

/// Gives each new card its display id: per project, in `since` order (cards
/// with no date last, bundle order within a day), from the project's
/// `next_card_seq`. A PR number two cards share is linked to the first only.
fn assign_display_ids(
    conn: &Connection,
    projects: &[ProjectPlan],
    cards: &mut [CardPlan],
) -> Result<()> {
    for project in projects {
        let first = match find::<Project>(conn, "id = ?1", &[&project.id])? {
            Some(existing) => existing.next_card_seq,
            None => first_card_seq(conn, &project.id)?,
        };
        let mut order: Vec<usize> = (0..cards.len())
            .filter(|&i| cards[i].project_id == project.id && cards[i].action == Action::Create)
            .collect();
        order.sort_by_key(|&i| (cards[i].detail.since.is_none(), cards[i].created_at));
        for (i, seq) in order.into_iter().zip(first..) {
            cards[i].display_id = ids::display_id(&project.id, seq);
        }
    }
    let mut linked: BTreeSet<(String, i64)> = BTreeSet::new();
    for card in cards.iter_mut() {
        if let Some(pr) = &mut card.pr
            && pr.not_linked.is_none()
            && let Some(number) = pr.number
            && let Some(owner) = &pr.project_id
            && !linked.insert((owner.clone(), number))
        {
            pr.not_linked = Some("another imported card already links this PR".into());
        }
    }
    Ok(())
}

/// Turns each open card's `depends_on` into dependencies on the cards they
/// name, in the bundle or already in `state.db` (whose state there counts for
/// a card both hold). A dependency on a card that closed undelivered stays
/// and asks the owner, as `card.close` does; a cycle is refused (`dep.add`).
fn resolve_dependencies(conn: &Connection, bundle: &Bundle, cards: &mut [CardPlan]) -> Result<()> {
    let mut index: HashMap<String, (String, CardState, Option<CardOutcome>)> = HashMap::new();
    for card in cards.iter() {
        let target = match card.action {
            Action::Create => (card.display_id.clone(), card.state, card.outcome),
            Action::Exists => conn.query_row(
                "SELECT display_id, state, outcome FROM cards WHERE display_id = ?1",
                [&card.display_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?,
        };
        index.insert(card.legacy_id.clone(), target);
    }
    for (card, source) in cards.iter_mut().zip(&bundle.cards) {
        if card.state != CardState::Open {
            continue;
        }
        for on in &source.depends_on {
            if *on == card.legacy_id {
                return Err(Error::refused(
                    "card",
                    &card.legacy_id,
                    "the card depends on itself",
                ));
            }
            let target = match index.get(on) {
                Some(found) => Some(found.clone()),
                None => conn
                    .query_row(
                        "SELECT display_id, state, outcome FROM cards WHERE legacy_id = ?1 ORDER BY display_id LIMIT 1",
                        [on],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?,
            };
            let Some((display_id, state, outcome)) = target else {
                return Err(Error::refused(
                    "card",
                    &card.legacy_id,
                    format!("it depends on {on}, which is neither in the bundle nor in state.db"),
                ));
            };
            let released = match (state, outcome) {
                (CardState::Closed, Some(outcome)) if outcome_satisfies_dependants(outcome) => {
                    Some(outcome.as_str().parse::<DepReleaseReason>()?)
                }
                _ => None,
            };
            if let (CardState::Closed, Some(outcome), None) = (state, outcome, released) {
                card.waits.push(Wait::Attention {
                    kind: AttentionKind::Decision,
                    question: format!(
                        "{} waits on {display_id}, which closed {outcome}. Drop the dependency, or cancel {}?",
                        card.display_id, card.display_id
                    ),
                });
            }
            card.waits.push(Wait::Dependency {
                on: on.clone(),
                display_id,
                released,
            });
        }
        card.column = column(card.state, &card.waits);
    }
    refuse_cycles(cards)
}

/// Refuses a cycle among the unreleased dependencies this import writes. An
/// existing card's dependencies never reach a new card, so only these count.
fn refuse_cycles(cards: &[CardPlan]) -> Result<()> {
    let edges: HashMap<&str, Vec<&str>> = cards
        .iter()
        .filter(|card| card.action == Action::Create)
        .map(|card| {
            let on = card
                .waits
                .iter()
                .filter_map(|wait| match wait {
                    Wait::Dependency {
                        on, released: None, ..
                    } => Some(on.as_str()),
                    _ => None,
                })
                .collect();
            (card.legacy_id.as_str(), on)
        })
        .collect();
    // Depth-first from each card; `path` is the chain being walked.
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for start in edges.keys() {
        let mut path: Vec<&str> = Vec::new();
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        while let Some((node, next)) = stack.pop() {
            if next == 0 {
                if let Some(at) = path.iter().position(|seen| *seen == node) {
                    let mut chain = path[at..].to_vec();
                    chain.push(node);
                    return Err(Error::refused(
                        "card",
                        node,
                        format!(
                            "the bundle's dependencies form a cycle: {}",
                            chain.join(" -> ")
                        ),
                    ));
                }
                if done.contains(node) {
                    continue;
                }
                path.push(node);
            }
            match edges.get(node).and_then(|on| on.get(next)) {
                Some(child) => {
                    stack.push((node, next + 1));
                    stack.push((child, 0));
                }
                None => {
                    path.pop();
                    done.insert(node);
                }
            }
        }
    }
    Ok(())
}

fn plan_memory(
    conn: &Connection,
    bundle: &Bundle,
    slugs: &BTreeMap<String, String>,
) -> Result<Vec<MemoryPlan>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(bundle.memory.len());
    for entry in &bundle.memory {
        if !seen.insert(entry.id.as_str()) {
            return Err(Error::refused(
                "memory",
                &entry.id,
                "the bundle lists this entry twice",
            ));
        }
        let project_id = match memory_scope(entry)? {
            Some(project) => slugs
                .get(project)
                .cloned()
                .ok_or_else(|| Error::refused("memory", &entry.id, "its project has no slug"))?,
            None => FLEET_PROJECT.to_owned(),
        };
        let placement = match entry.class.as_str() {
            "rule" | "note" => Placement::Proposal {
                target: document_path(&project_id),
                section: if entry.class == "rule" {
                    RULE_SECTION
                } else {
                    NOTE_SECTION
                }
                .to_owned(),
            },
            "requirement" => Placement::Requirement,
            "stale" => Placement::NotPlaced {
                reason: entry
                    .reason
                    .clone()
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or_else(|| "stale".to_owned()),
            },
            other => Placement::NotPlaced {
                reason: format!("class {other:?} is not rule, note, requirement or stale"),
            },
        };
        let exists: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM evidence WHERE kind = 'legacy'
                             AND json_extract(ref, '$.entry') = ?1 AND excerpt = ?2)",
            params![entry.id, entry.text],
            |row| row.get(0),
        )?;
        out.push(MemoryPlan {
            legacy_id: entry.id.clone(),
            class: entry.class.clone(),
            project_id,
            placement,
            action: if exists {
                Action::Exists
            } else {
                Action::Create
            },
            entry: Some(entry.clone()),
        });
    }
    Ok(out)
}

/// The Tiller document a project's entries are placed in, relative to the
/// host's memory repo (`memory.md` section 3).
pub fn document_path(project_id: &str) -> String {
    if project_id == FLEET_PROJECT {
        "fleet.md".to_owned()
    } else {
        format!("projects/{project_id}.md")
    }
}

/// Refuses a placement into a document that already has content: every
/// placement diff is made against the empty file.
fn check_memory_targets(conn: &Connection, memory: &[MemoryPlan]) -> Result<()> {
    let targets: BTreeSet<(&str, &str)> = memory
        .iter()
        .filter(|m| m.action == Action::Create)
        .filter_map(|m| match &m.placement {
            Placement::Proposal { target, .. } => Some((m.project_id.as_str(), target.as_str())),
            _ => None,
        })
        .collect();
    for (project_id, target) in targets {
        let blob: Option<String> = conn
            .query_row(
                "SELECT r.blob_sha FROM memory_sources s JOIN memory_revisions r ON r.source_id = s.id
                 WHERE s.project_id = ?1 AND s.kind = 'tiller_doc' AND s.canonical_path = ?2
                   AND s.retired_at IS NULL
                 ORDER BY r.recorded_at DESC, r.id DESC LIMIT 1",
                params![project_id, target],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(blob) = blob
            && blob != EMPTY_BLOB
        {
            return Err(Error::refused(
                "memory_source",
                target,
                "the document already has content; legacy placement diffs need the empty document",
            ));
        }
    }
    Ok(())
}

/// A bundle date (`YYYY-MM-DD`, a Lisbon day) as 09:00 Lisbon that day, the
/// CLI's reading of a bare date (`protocol.md` 6.1); a full RFC 3339 time is
/// taken as given.
pub(crate) fn bundle_time(text: &str) -> Result<Timestamp> {
    if let Ok(ts) = text.parse::<Timestamp>() {
        return Ok(ts);
    }
    let date: jiff::civil::Date = text
        .parse()
        .map_err(|error| Error::Usage(format!("not a date: {text:?}: {error}")))?;
    let summer = last_sunday(date.year(), 3).is_some_and(|start| date >= start)
        && last_sunday(date.year(), 10).is_some_and(|end| date < end);
    let utc_hour = if summer { 8 } else { 9 };
    let ts = date
        .at(utc_hour, 0, 0, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .map_err(|error| Error::Usage(format!("not a date: {text:?}: {error}")))?
        .timestamp();
    Ok(Timestamp::from_jiff(ts))
}

/// The last Sunday of `month`: the EU summer-time switch day.
fn last_sunday(year: i16, month: i8) -> Option<jiff::civil::Date> {
    let mut day = jiff::civil::Date::new(year, month, 1).ok()?.last_of_month();
    while day.weekday() != jiff::civil::Weekday::Sunday {
        day = day.yesterday().ok()?;
    }
    Some(day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_date_is_nine_in_the_morning_lisbon() {
        assert_eq!(
            bundle_time("2026-01-15").unwrap().to_string(),
            "2026-01-15T09:00:00.000Z"
        );
        assert_eq!(
            bundle_time("2026-07-15").unwrap().to_string(),
            "2026-07-15T08:00:00.000Z"
        );
        // 2026's switches are on Sunday 2026-03-29 and Sunday 2026-10-25.
        assert_eq!(
            bundle_time("2026-03-29").unwrap().to_string(),
            "2026-03-29T08:00:00.000Z"
        );
        assert_eq!(
            bundle_time("2026-10-25").unwrap().to_string(),
            "2026-10-25T09:00:00.000Z"
        );
        assert_eq!(
            bundle_time("2026-10-24T22:30:00+01:00")
                .unwrap()
                .to_string(),
            "2026-10-24T21:30:00.000Z"
        );
        assert!(bundle_time("yesterday").is_err());
    }

    #[test]
    fn pr_urls_parse_only_as_github_pulls() {
        assert_eq!(
            parse_pr_url("https://github.com/example/alpha/pull/12"),
            Some(("example/alpha".to_owned(), 12))
        );
        assert_eq!(
            parse_pr_url("https://github.com/example/alpha/issues/12"),
            None
        );
        assert_eq!(
            parse_pr_url("https://example.com/example/alpha/pull/12"),
            None
        );
        assert_eq!(
            parse_pr_url("https://github.com/example/alpha/pull/0"),
            None
        );
    }
}
