//! `tiller spawn`: capacity, quota, worktree, pane, role and brief (T4.2),
//! and the two ends of a run that touch the world: `tiller done` and
//! cleanup.
//!
//! A spawn runs in this order (`state.md`, Reservation; `protocol.md` 6.4):
//!
//! 1. dispatch resolves the model and effort from the rules (P02);
//! 2. `run.start` reserves capacity, checks the RAM, disk and quota floors
//!    and inserts the run and its placeholder session, all in one
//!    `BEGIN IMMEDIATE` transaction, so two spawns cannot both take the last
//!    lane (P03, P04);
//! 3. treehouse leases a worktree on a new branch, `run.place` records its
//!    path and branch at once, it is brought to the current head of
//!    `origin/<default>` (P05, P06), and the scan records it clean;
//! 4. the launch files are written and `run.place` records the base and
//!    fragment hash;
//! 5. herdr opens the worker's workspace, `run.place` binds its pane to the
//!    session, the row tokens are reported and the layout pass moves the
//!    workspace under its project's orchestrator and fixes the glyphs
//!    (`docs/design/herdr-layout.md`);
//! 6. the launch line is typed into the pane.
//!
//! A step after `run.start` that fails ends the run `failed`, releases the
//! reservation `spawn_failed`, and cleans up what it made through the same
//! [`cleanup`] every run goes through, which returns a worktree only when a
//! scan taken just now shows it holds nothing (G18).

pub mod config;
pub mod dispatch;
pub mod launch;
pub mod orchestrator;
pub mod pool;
pub mod readings;
pub mod trust;

#[cfg(test)]
pub(crate) mod tests;

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;
use tiller_core::domain::{
    Card, Delivery, Effort, Project, Reservation, Run, RunOutcome, RunRole, Session, Worktree,
};
use tiller_core::state::caches::{WorktreeReading, landed_heads};
use tiller_core::state::projects::meta;
use tiller_core::state::runs::{EndRun, NewRun, Placement, SpawnChecks, Started, live_session};
use tiller_core::state::{find, get, list, resolve_card};
use tiller_core::{Applied, Ctx, Store};
use tiller_herdr::client::{Herdr, HerdrError};
use tiller_herdr::layout;
use tiller_reconcile::sources::{Scanner, WorktreeScan};

use self::config::SpawnConfig;
use self::launch::LaunchSpec;
use self::pool::Pool;
use self::trust::ClaudeConfig;
use crate::roles::{self, RoleError};

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("{0}")]
    Usage(String),
    #[error("config.toml: {0}")]
    Config(String),
    /// The orchestrator asked for less than the dispatch rule gives.
    #[error("{0}")]
    Downgrade(String),
    /// A precondition outside `state.db` failed: unlanded work, a live pane.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    State(#[from] tiller_core::Error),
    #[error(transparent)]
    Herdr(#[from] HerdrError),
    #[error(transparent)]
    Role(#[from] RoleError),
    #[error("worktree pool: {0}")]
    Pool(String),
    #[error("git: {0}")]
    Git(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A spawn that failed, with what undoing it could not do. The caller
    /// judges `error`; `notes` only add to its message.
    #[error("{error}; {notes}")]
    Undone {
        error: Box<SpawnError>,
        notes: String,
    },
}

pub type Result<T, E = SpawnError> = std::result::Result<T, E>;

/// What spawn is asked for.
#[derive(Clone, Debug)]
pub struct Request {
    /// A card reference.
    pub card: String,
    pub role: RunRole,
    /// The spawn's own `--model --effort`, which may only raise the rule.
    pub asked: Option<(String, Effort)>,
    /// The brief; `None` briefs the worker with the card itself.
    pub brief: Option<String>,
}

/// The brief a spawn without `--brief-file` gives: the card.
pub fn card_brief(card: &Card) -> String {
    let mut brief = format!("Card {}: {}\n", card.display_id, card.title);
    if let Some(body) = card.body.as_deref().filter(|b| !b.trim().is_empty()) {
        brief.push_str(&format!("\n{}\n", body.trim()));
    }
    if let Some(acceptance) = card.acceptance.as_deref().filter(|a| !a.trim().is_empty()) {
        brief.push_str(&format!("\n## Acceptance\n\n{}\n", acceptance.trim()));
    }
    brief
}

/// The delivery line a builder's brief ends with: the project's effective
/// delivery (`docs/design/pre-launch-validation.md` section 2). A direct-pr
/// worker only commits; the daemon pushes and opens the PR (`authority.md`
/// G12).
pub fn delivery_line(project: &Project) -> String {
    let effective = project.effective_delivery();
    let mut line = format!("\nDelivery: {effective}.");
    if effective != project.delivery {
        line.push_str(&format!(
            " The project is {}: skip no-mistakes and keep your own /code-review.",
            project.stage
        ));
    }
    if effective == Delivery::DirectPr {
        line.push_str(
            " Commit only, never push: describe the PR with `tiller pr describe` and report done; the daemon pushes the branch and opens the PR.",
        );
    }
    line.push('\n');
    line
}

/// The world a spawn, a `tiller done` and a cleanup act on.
pub struct World<'a> {
    pub config: &'a SpawnConfig,
    pub pool: &'a dyn Pool,
    pub herdr: &'a dyn Herdr,
    pub scanner: &'a dyn Scanner,
    /// The packaged `tiller` binary the fragment's hooks call.
    pub tiller: &'a Path,
    /// The run scratch root; each run gets `<root>/<run id>`.
    pub scratch_root: &'a Path,
    /// The effective Tiller home: hidden from every session's sandbox and
    /// exported to it as `TILLER_HOME`.
    pub tiller_home: &'a Path,
    /// Where Claude Code keeps the user config launches record trust in.
    pub claude_config: &'a ClaudeConfig,
    /// Free RAM and disk, read just before `run.start`.
    pub ram_free_mb: Option<i64>,
    pub disk_free_mb: Option<i64>,
}

/// What a spawn made.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Spawned {
    pub run: Run,
    pub session: Session,
    pub reservation: Reservation,
    pub worktree: String,
    pub branch: String,
    pub base_sha: String,
    pub pane_id: String,
    pub workspace_id: String,
    pub label: String,
    /// The herdr agent name, `<card>-<role>`.
    pub name: String,
    pub dispatch: Value,
    /// A layout repair that failed: the workspace is usable, out of order.
    pub warning: Option<String>,
}

/// The outcome of a spawn: a replayed key returns the run it started.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Outcome {
    Spawned(Box<Spawned>),
    Replayed {
        run: Box<Run>,
        session: Box<Session>,
    },
}

/// The herdr agent name of a run: `alpha-42-build`, then `alpha-42-build-2`.
pub fn agent_name(display_id: &str, role: RunRole, attempt: i64) -> String {
    match attempt {
        1 => format!("{display_id}-{}", role.short_name()),
        n => format!("{display_id}-{}-{n}", role.short_name()),
    }
}

/// The run's worktree branch, `tiller/<agent name>`, so a retry never
/// collides with an earlier attempt's branch.
pub fn branch_name(display_id: &str, role: RunRole, attempt: i64) -> String {
    format!("tiller/{}", agent_name(display_id, role, attempt))
}

/// The treehouse lease holder of a run.
fn holder(run: &Run) -> String {
    format!("tiller:{}", run.id)
}

pub(crate) fn role_of(role: RunRole) -> Result<roles::Role> {
    roles::Role::parse(role.as_str())
        .ok_or_else(|| SpawnError::Usage(format!("no packaged role for {role}")))
}

/// The weekly floor this spawn checks: this host's share of the account
/// floor, plus the review reserve for new work. `None` when quota is off.
pub fn quota_floor(config: &SpawnConfig, role: RunRole) -> Option<f64> {
    let quota = &config.quota;
    quota.enabled.then(|| match role {
        RunRole::Scout | RunRole::Builder => quota.host_floor() + quota.review_reserve_percent,
        RunRole::Reviewer | RunRole::Verifier => quota.host_floor(),
    })
}

pub(crate) fn local_host(store: &Store) -> Result<String> {
    meta(store.conn(), "host_id")?
        .ok_or_else(|| SpawnError::Usage("this host is not initialized; run tiller init".into()))
}

/// Spawns one worker run. `key` is the spawn's idempotency key.
pub fn spawn(
    store: &mut Store,
    ctx: &Ctx,
    key: Option<&str>,
    world: &World<'_>,
    request: Request,
) -> Result<Applied<Outcome>> {
    let card = resolve_card(store.conn(), &request.card)?;
    let project: Project = get(store.conn(), "project", &card.project_id)?;
    let checkout = world.config.checkouts.get(&project.id).ok_or_else(|| {
        SpawnError::Config(format!(
            "no checkout is configured for project {0}; set it with tiller config set checkouts.{0} <path>",
            project.id
        ))
    })?;
    let role = role_of(request.role)?;
    let asked = request.asked.as_ref().map(|(m, e)| (m.as_str(), *e));
    let resolved = dispatch::resolve(&world.config.dispatch, &card, asked, ctx.actor.is_owner())?;

    let started = store.run_start(
        ctx,
        key,
        NewRun {
            card: card.id.clone(),
            role: request.role,
            model: resolved.model.clone(),
            effort: resolved.effort,
            host_id: None,
            worktree_path: None,
            branch: None,
            base_branch: None,
            base_sha: None,
            config_hash: None,
            checks: SpawnChecks {
                ram_free_mb: world.ram_free_mb,
                disk_free_mb: world.disk_free_mb,
                quota_floor_percent: quota_floor(world.config, request.role),
                quota_max_age_minutes: world.config.quota.max_age_minutes,
                dispatch: resolved.payload.clone(),
            },
        },
    )?;
    if started.replayed {
        let run: Run = get(store.conn(), "run", &started.value.run.id)?;
        let session: Session = get(store.conn(), "session", &started.value.session.id)?;
        // The key's first answer was an error, so its replay is one too.
        if session.end_reason.as_deref() == Some("spawn_failed") {
            return Err(SpawnError::Refused(format!(
                "this key already spawned run {}, which failed: {}; spawn again with a new key",
                run.id,
                run.outcome_note.as_deref().unwrap_or("no reason recorded")
            )));
        }
        if run.ended_at.is_none() && session.pane_id.is_none() {
            return Err(SpawnError::Refused(format!(
                "this key already started run {}, and its spawn stopped before a pane opened; end it with tiller done {} --outcome failed, then spawn again",
                run.id, run.id
            )));
        }
        return Ok(Applied {
            value: Outcome::Replayed {
                run: Box::new(run),
                session: Box::new(session),
            },
            events: started.events,
            replayed: true,
        });
    }
    let mut made = Made {
        events: started.events,
        ..Made::default()
    };
    let started = started.value;
    let placed = place(
        store, ctx, world, &card, &project, checkout, role, &request, &started, &mut made,
    );
    match placed {
        Ok(mut spawned) => {
            spawned.dispatch = resolved.payload;
            Ok(Applied {
                value: Outcome::Spawned(Box::new(spawned)),
                events: made.events,
                replayed: false,
            })
        }
        Err(error) => Err(fail(store, ctx, world, &started.run, &made, error)),
    }
}

/// What a spawn made, so a step that fails can undo it, and the events its
/// mutations wrote.
#[derive(Default)]
struct Made {
    worktree: Option<PathBuf>,
    pane: Option<String>,
    events: Vec<i64>,
}

#[allow(clippy::too_many_arguments)]
fn place(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    card: &Card,
    project: &Project,
    checkout: &Path,
    role: roles::Role,
    request: &Request,
    started: &Started,
    made: &mut Made,
) -> Result<Spawned> {
    let (run, session) = (&started.run, &started.session);
    let branch = branch_name(&card.display_id, run.role, run.attempt);
    let worktree = world
        .pool
        .take(checkout, &branch, &project.default_branch, &holder(run))
        .map_err(SpawnError::Pool)?;
    made.worktree = Some(worktree.clone());
    let worktree_text = worktree.to_string_lossy().into_owned();
    // Recorded before anything else can fail, so a failed spawn's cleanup
    // finds the lease and returns it.
    let placed = store.run_place(
        ctx,
        &run.id,
        Placement {
            worktree_path: Some(worktree_text.clone()),
            branch: Some(branch.clone()),
            base_branch: Some(project.default_branch.clone()),
            ..Placement::default()
        },
    )?;
    made.events.extend(placed.events);
    let base_sha =
        pool::start_from_origin(&worktree, &project.default_branch).map_err(SpawnError::Git)?;
    let scan = scan_and_record(store, ctx, world.scanner, run, project, &worktree)?;
    made.events.extend(scan.1);
    let scan = scan.0;
    if !scan.provably_clean() {
        return Err(SpawnError::Refused(format!(
            "the new worktree {} is not provably clean: {scan:?}",
            worktree.display()
        )));
    }

    let scratch = world.scratch_root.join(&run.id);
    let mut brief = match &request.brief {
        Some(brief) => brief.clone(),
        None => card_brief(card),
    };
    if run.role == RunRole::Builder {
        brief.push_str(&delivery_line(project));
    }
    let claude_version = launch::claude_version(&world.config.spawn.claude);
    let launched = launch::write(&LaunchSpec {
        role,
        session_id: &session.id,
        run_id: &run.id,
        worktree: &worktree,
        scratch: &scratch,
        tiller_home: world.tiller_home,
        model: &run.model,
        effort: run.effort,
        brief: Some(&brief),
        roots: None,
        tiller: world.tiller,
        default_branch: &project.default_branch,
        network_allow: &world.config.spawn.network_allow,
        no_mistakes_socket: world.config.spawn.no_mistakes_socket.as_deref(),
        gh_login_readable: project.effective_delivery() == Delivery::NoMistakes,
        claude: &world.config.spawn.claude,
        claude_config: world.claude_config,
        claude_version: claude_version.as_deref(),
        memory: &[],
        note: None,
    })?;
    // The manifest first, so a hash on a run always resolves to its parts.
    let recorded = store.config_manifest_record(ctx, &launched.config_hash, &launched.manifest)?;
    made.events.extend(recorded.events);
    let placed = store.run_place(
        ctx,
        &run.id,
        Placement {
            base_sha: Some(base_sha.clone()),
            config_hash: Some(launched.config_hash.clone()),
            ..Placement::default()
        },
    )?;
    made.events.extend(placed.events);

    let label = layout::worker_label(&card.display_id, run.role, true);
    let created = world.herdr.create_workspace(&label, Some(&worktree))?;
    made.pane = Some(created.pane_id.clone());
    let placed = store.run_place(
        ctx,
        &run.id,
        Placement {
            pane_id: Some(created.pane_id.clone()),
            ..Placement::default()
        },
    )?;
    made.events.extend(placed.events);
    let session: Session = get(store.conn(), "session", &session.id)?;
    let bindings = layout::bindings(store.conn(), &run.host_id)?;
    // The tokens name the session before anything else reads the pane.
    if let Some(binding) = bindings.iter().find(|b| b.session_id == session.id) {
        world
            .herdr
            .report_tokens(&created.pane_id, &binding.tokens())?;
    }
    let warning = relayout(world.herdr, &bindings).err();
    world.herdr.send_text(&created.pane_id, &launched.line)?;
    world.herdr.send_enter(&created.pane_id)?;

    Ok(Spawned {
        name: agent_name(&card.display_id, run.role, run.attempt),
        run: placed.value,
        session,
        reservation: started.reservation.clone(),
        worktree: worktree_text,
        branch,
        base_sha,
        pane_id: created.pane_id,
        workspace_id: created.workspace_id,
        label,
        dispatch: Value::Null,
        warning,
    })
}

/// One layout pass: tokens, then renames, then moves. A herdr that refuses
/// `workspace.move` leaves correct labels in the wrong order, which is still
/// usable (`herdr-layout.md`, Known limits), so the caller only reports it.
pub(crate) fn relayout(herdr: &dyn Herdr, bindings: &[layout::Binding]) -> Result<(), String> {
    let snapshot = herdr.snapshot().map_err(|e| e.to_string())?;
    layout::repair(herdr, &snapshot, bindings)
        .map(drop)
        .map_err(|e| format!("layout repair: {e}"))
}

/// Ends a spawn that failed after `run.start`: the run `failed`, the
/// reservation `spawn_failed`, then the normal cleanup of whatever it made.
/// Returns the original error, with what cleanup could not do appended.
fn fail(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    run: &Run,
    made: &Made,
    error: SpawnError,
) -> SpawnError {
    let mut notes = Vec::new();
    if let Err(e) = store.run_spawn_failed(ctx, &run.id, &error.to_string()) {
        notes.push(format!("ending the run failed: {e}"));
    }
    // Nothing was launched, so a pane spawn opened holds a bare shell.
    if let Some(pane) = &made.pane
        && let Err(e) = world.herdr.close_pane(pane)
    {
        notes.push(format!("closing pane {pane} failed: {e}"));
    }
    if made.worktree.is_some() {
        match get::<Run>(store.conn(), "run", &run.id)
            .map_err(SpawnError::from)
            .and_then(|run| cleanup(store, ctx, world, &run))
        {
            Ok(_) => {}
            Err(e) => notes.push(format!("the worktree was kept: {e}")),
        }
    }
    if notes.is_empty() {
        return error;
    }
    SpawnError::Undone {
        error: Box::new(error),
        notes: notes.join("; "),
    }
}

/// Scans a run's worktree now and records the reading; returns the scan
/// and the events the record wrote.
pub(crate) fn scan_and_record(
    store: &mut Store,
    ctx: &Ctx,
    scanner: &dyn Scanner,
    run: &Run,
    project: &Project,
    path: &Path,
) -> Result<(WorktreeScan, Vec<i64>)> {
    let text = path.to_string_lossy().into_owned();
    let heads = landed_heads(store.conn(), &run.host_id, &text)?;
    let scan = scanner.scan(path, project, &heads);
    let mut events = Vec::new();
    if scan.exists {
        let applied = store.cache_worktree(
            ctx,
            &run.host_id,
            WorktreeReading {
                path: text,
                project_id: Some(project.id.clone()),
                run_id: Some(run.id.clone()),
                branch: scan.branch.clone(),
                head_sha: scan.head_sha.clone(),
                dirty: scan.dirty,
                unlanded_commits: scan.unlanded_commits,
            },
        )?;
        events = applied.events;
    }
    Ok((scan, events))
}

pub(crate) fn project_of(store: &Store, run: &Run) -> Result<Project> {
    let card: Card = get(store.conn(), "card", &run.card_id)?;
    Ok(get(store.conn(), "project", &card.project_id)?)
}

pub(crate) fn require_local(store: &Store, run: &Run) -> Result<()> {
    if run.host_id == local_host(store)? {
        Ok(())
    } else {
        Err(SpawnError::Refused(format!(
            "run {} is on another host; end and clean it there",
            run.id
        )))
    }
}

/// What `tiller done` did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ended {
    pub run: Run,
    pub scan: Option<WorktreeScan>,
    /// Set when the run ended `failed` or `abandoned` with work in its
    /// worktree: cleanup keeps it, and the orchestrator decides.
    pub kept: Option<String>,
}

/// `tiller done`: reads the run's worktree now and refuses while it holds
/// commits that are not landed or changes that are not committed (P51).
/// Only `failed` and `abandoned` end a run over such work, and then the
/// worktree stays for the orchestrator; cleanup will not return it.
///
/// It needs no config, since a worker session runs it and fleet roles
/// cannot read `config.toml`. A run on another host is judged by the last
/// reading this host holds of its worktree.
pub fn done(
    store: &mut Store,
    ctx: &Ctx,
    scanner: &dyn Scanner,
    run: &Run,
    outcome: RunOutcome,
) -> Result<Applied<Ended>> {
    let run: &Run = &get(store.conn(), "run", &run.id)?;
    let mut scan = None;
    let mut kept = None;
    let mut events = Vec::new();
    if run.outcome.is_none()
        && let Some(path) = &run.worktree_path
    {
        let reading = if run.host_id == local_host(store)? {
            let project = project_of(store, run)?;
            let (scan, written) =
                scan_and_record(store, ctx, scanner, run, &project, Path::new(path))?;
            events = written;
            scan
        } else {
            recorded(store, run, path)?
        };
        if let Some(what) = holds_work(&reading) {
            if matches!(outcome, RunOutcome::Failed | RunOutcome::Abandoned) {
                let keeps = if reading.exists {
                    "; cleanup keeps the worktree"
                } else {
                    ""
                };
                kept = Some(format!("ended {outcome} while {path} {what}{keeps}"));
            } else {
                return Err(SpawnError::Refused(format!(
                    "{path} {what}; land it first, or end with --outcome failed or abandoned and the orchestrator decides what to keep"
                )));
            }
        }
        scan = Some(reading);
    }
    let applied = store.run_end(
        ctx,
        &run.id,
        EndRun {
            outcome,
            note: kept.clone(),
        },
    )?;
    events.extend(applied.events);
    Ok(Applied {
        value: Ended {
            run: applied.value,
            scan,
            kept,
        },
        events,
        replayed: applied.replayed,
    })
}

/// The last reading this host holds of a worktree, as a scan.
fn recorded(store: &Store, run: &Run, path: &str) -> Result<WorktreeScan> {
    let row: Option<Worktree> = find(
        store.conn(),
        "host_id = ?1 AND path = ?2 AND gone_at IS NULL",
        &[&run.host_id, &path],
    )?;
    let mut scan = WorktreeScan::missing(Path::new(path));
    if let Some(row) = row {
        scan.exists = true;
        scan.is_worktree = true;
        scan.branch = row.branch;
        scan.head_sha = row.head_sha;
        scan.dirty = row.dirty;
        scan.unlanded_commits = row.unlanded_commits;
    }
    Ok(scan)
}

/// What a scan says a worktree still holds, phrased to follow its path, or
/// `None` when it provably holds nothing. An unknown is work: the scan never
/// guesses. [`WorktreeScan::provably_clean`] decides; this only explains.
fn holds_work(scan: &WorktreeScan) -> Option<String> {
    if scan.provably_clean() {
        return None;
    }
    if !scan.exists {
        return Some("is missing, so whether its commits landed is unknown".into());
    }
    if !scan.is_worktree {
        return Some("is not a git worktree".into());
    }
    let mut parts = Vec::new();
    match scan.unlanded_commits {
        Some(0) => {}
        Some(n) => parts.push(format!(
            "{n} unlanded commit{}",
            if n == 1 { "" } else { "s" }
        )),
        None => parts.push("commits whose landing is unknown".into()),
    }
    match scan.dirty {
        Some(false) => {}
        Some(true) => parts.push("uncommitted changes".into()),
        None => parts.push("changes that could not be read".into()),
    }
    Some(if parts.is_empty() {
        "is not provably clean".into()
    } else {
        format!("holds {}", parts.join(" and "))
    })
}

/// What cleanup did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Cleaned {
    pub run: Run,
    pub closed_panes: Vec<String>,
    pub returned: Option<String>,
    /// A step after `run.clean` that failed: a treehouse return or the
    /// relabel. The records are already right; reconcile retries.
    pub warnings: Vec<String>,
}

/// Cleanup (`tiller run clean`, and every failed spawn): closes the run's
/// pane and returns its worktree, only when all of these hold, read now:
/// the run has ended; no pane of the run hosts a live agent; the worktree is
/// a git worktree with no unlanded commits and no uncommitted changes. Any
/// fact that cannot be read refuses, and a refusal changes nothing but the
/// worktree reading it took.
pub fn cleanup(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    run: &Run,
) -> Result<Applied<Cleaned>> {
    // Judged as the run is now, not as the caller last read it.
    let run: Run = get(store.conn(), "run", &run.id)?;
    if run.ended_at.is_none() {
        return Err(SpawnError::Refused(format!(
            "run {} is active; end it with tiller done first",
            run.id
        )));
    }
    require_local(store, &run)?;
    // Cleaned already: the path may be another run's lease by now, so a
    // second cleanup reads nothing there and changes nothing.
    if cleaned_before(store, &run)? {
        return Ok(Applied {
            value: Cleaned {
                run,
                closed_panes: Vec::new(),
                returned: None,
                warnings: Vec::new(),
            },
            events: Vec::new(),
            replayed: true,
        });
    }
    let open = dead_panes(store, world.herdr, &run)?;
    let mut warnings = Vec::new();
    let Some(path) = run.worktree_path.clone() else {
        // No worktree was ever leased: there is nothing to keep, so the
        // panes close and the session ends.
        let mut events = Vec::new();
        let closed = close_panes(world.herdr, &open, &mut warnings);
        if let Some(session) = live_session(store.conn(), &run.id)? {
            events.extend(store.session_end(ctx, &session.id, "cleaned")?.events);
        }
        return Ok(Applied {
            value: Cleaned {
                run,
                closed_panes: closed,
                returned: None,
                warnings,
            },
            events,
            replayed: false,
        });
    };
    let project = project_of(store, &run)?;
    let (scan, mut events) =
        scan_and_record(store, ctx, world.scanner, &run, &project, Path::new(&path))?;
    if let Some(what) = holds_work(&scan) {
        return Err(SpawnError::Refused(format!(
            "{path} {what}; cleanup never removes unlanded work"
        )));
    }
    // Recorded before anything leaves: a pane closes and a worktree goes
    // back only once a record says the run no longer holds them. No pane
    // of the run runs an agent, so the ones left are bare shells.
    let cleaned = store.run_clean(ctx, &run.id, true)?;
    events.extend(cleaned.events);
    let closed = close_panes(world.herdr, &open, &mut warnings);
    // A pane that closed with its session still recorded live ends here, so
    // the layout and delivery stop addressing it.
    if let Some(session) = live_session(store.conn(), &run.id)? {
        events.extend(store.session_end(ctx, &session.id, "cleaned")?.events);
    }
    let mut returned = None;
    match world.pool.give_back(Path::new(&path), &holder(&run)) {
        Ok(()) => returned = Some(path),
        Err(e) => warnings.push(format!(
            "{path} is recorded clean and gone but treehouse did not take it back ({e}); return it with treehouse return"
        )),
    }
    // The launch files are the run's only files outside `state.db`, so
    // cleanup leaves no marker behind (P53).
    let scratch = world.scratch_root.join(&run.id);
    if let Err(e) = std::fs::remove_dir_all(&scratch)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warnings.push(format!("removing {}: {e}", scratch.display()));
    }
    let bindings = layout::bindings(store.conn(), &run.host_id)?;
    if let Err(e) = relayout(world.herdr, &bindings) {
        warnings.push(e);
    }
    Ok(Applied {
        value: Cleaned {
            run: cleaned.value,
            closed_panes: closed,
            returned,
            warnings,
        },
        events,
        replayed: false,
    })
}

/// Whether `run.clean` already recorded this run: the worktree row cannot
/// say, since a later run that leases the same path rewrites it.
fn cleaned_before(store: &Store, run: &Run) -> Result<bool> {
    Ok(store
        .conn()
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE entity = 'run' AND entity_id = ?1 AND kind = 'run.cleaned')",
            [&run.id],
            |row| row.get(0),
        )
        .map_err(tiller_core::Error::from)?)
}

/// The run's panes herdr still has, all of them bare shells: refuses when
/// one runs an agent. A pane is the run's only while its row tokens name
/// one of the run's sessions, since herdr may give a closed pane's id to
/// another workspace; a pane with no tokens is judged as the run's, so a
/// live agent there still refuses, but it is never closed.
fn dead_panes(store: &Store, herdr: &dyn Herdr, run: &Run) -> Result<Vec<String>> {
    let sessions: Vec<Session> = list(
        store.conn(),
        "run_id = ?1 AND pane_id IS NOT NULL ORDER BY started_at",
        &[&run.id],
    )?;
    let mut open = Vec::new();
    for session in &sessions {
        let Some(pane_id) = &session.pane_id else {
            continue;
        };
        let Some(pane) = herdr.pane(pane_id)? else {
            continue;
        };
        let owner = pane.tokens.get("session");
        if owner.is_some_and(|owner| sessions.iter().all(|s| &s.id != owner)) {
            continue;
        }
        if let Some(agent) = &pane.agent {
            return Err(SpawnError::Refused(format!(
                "pane {pane_id} still runs {agent}; a live pane is never closed"
            )));
        }
        if owner.is_some() && !open.contains(pane_id) {
            open.push(pane_id.clone());
        }
    }
    Ok(open)
}

/// Closes each pane; one that will not close is a warning, since the
/// records already say the run no longer holds it.
fn close_panes(herdr: &dyn Herdr, panes: &[String], warnings: &mut Vec<String>) -> Vec<String> {
    let mut closed = Vec::new();
    for pane in panes {
        match herdr.close_pane(pane) {
            Ok(()) => closed.push(pane.clone()),
            Err(e) => warnings.push(format!(
                "closing pane {pane} failed ({e}); close it in herdr"
            )),
        }
    }
    closed
}
