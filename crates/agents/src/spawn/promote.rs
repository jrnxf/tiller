//! `tiller promote`: the developer said go on a scout's report, so the card
//! becomes a ship card and a builder run starts on it in the scout's own
//! worktree and branch (P07). A spawn always takes a fresh pool worktree;
//! a promotion is the one start that keeps one, because the scout's
//! prototype and notes are what the builder builds on.
//!
//! It is the execute decision again (`authority.md`, section 4 notes), so it
//! passes `run.start`'s actor, capacity and floors, and `dispatch` resolves
//! the builder's model for a ship card.

use std::path::Path;

use tiller_core::domain::{Card, CardKind, Effort, Project, Run, RunRole};
use tiller_core::state::runs::{Promote, SpawnChecks};
use tiller_core::state::{find, get, resolve_card};
use tiller_core::{Applied, Ctx, Store};

use super::{
    Made, Outcome, Result, SpawnError, Spawned, World, card_brief, dispatch, fail, hold_for_quota,
    launch_in, quota_floor, replay, role_of, scan_and_record,
};

/// What a promotion is asked for.
#[derive(Clone, Debug)]
pub struct Request {
    /// A card reference; the card must be a scout card.
    pub card: String,
    /// The promotion's own `--model --effort`, which may only raise the rule.
    pub asked: Option<(String, Effort)>,
    /// The builder's brief; `None` briefs it with the card and the scout.
    pub brief: Option<String>,
    /// The harness to launch the builder on; G35 refuses one without the guards.
    pub harness: String,
    /// Promote past the quota floor on the owner's answered "Start now
    /// anyway" decision for this card.
    pub past_quota_floor: bool,
}

/// The brief a promotion without `--brief-file` gives: the card, and where
/// the scout left its work.
pub fn promoted_brief(card: &Card, scout: &Run) -> String {
    let mut brief = card_brief(card);
    brief.push_str(&format!(
        "\nThe developer approved the scout's report, so you build it. This worktree and branch are the scout run's ({}): read what it left here before you change anything.\n",
        scout.id
    ));
    brief
}

/// The scout run a promotion continues: the newest one of the card.
fn scout_of(store: &Store, card: &Card) -> Result<Run> {
    let scout: Option<Run> = find(
        store.conn(),
        "card_id = ?1 AND role = 'scout' AND outcome = 'reported' ORDER BY attempt DESC",
        &[&card.id],
    )?;
    scout.ok_or_else(|| {
        SpawnError::Refused(format!(
            "card {} has no reported scout run to promote; spawn a builder instead",
            card.display_id
        ))
    })
}

/// Promotes the card's scout: a builder run in the scout's worktree. `key`
/// is the promotion's idempotency key.
pub fn promote(
    store: &mut Store,
    ctx: &Ctx,
    key: Option<&str>,
    world: &World<'_>,
    request: Request,
) -> Result<Applied<Outcome>> {
    // G35, before anything is reserved or started.
    if let Some(reason) = tiller_observe::harness::refusal(&request.harness) {
        return Err(SpawnError::Refused(reason));
    }
    let card = resolve_card(store.conn(), &request.card)?;
    let project: Project = get(store.conn(), "project", &card.project_id)?;
    let scout = scout_of(store, &card)?;
    let role = role_of(RunRole::Builder)?;
    // The model follows the card the promotion makes, a ship card.
    let as_ship = Card {
        kind: CardKind::Ship,
        ..card.clone()
    };
    let asked = request.asked.as_ref().map(|(m, e)| (m.as_str(), *e));
    let resolved = dispatch::resolve(
        &world.config.dispatch,
        &as_ship,
        asked,
        ctx.actor.is_owner(),
    )?;

    let checks = SpawnChecks {
        ram_free_mb: world.ram_free_mb,
        disk_free_mb: world.disk_free_mb,
        quota_floor_percent: quota_floor(world.config, RunRole::Builder),
        quota_max_age_minutes: world.config.quota.max_age_minutes,
        past_quota_floor: request.past_quota_floor,
        dispatch: resolved.payload.clone(),
    };
    let started = store.run_promote(
        ctx,
        key,
        Promote {
            scout_run: scout.id.clone(),
            model: resolved.model.clone(),
            effort: resolved.effort,
            checks: checks.clone(),
        },
    );
    let started = started.map_err(|error| {
        hold_for_quota(store, ctx, &card, RunRole::Builder, &checks, error.into())
    })?;
    if started.replayed {
        return replay(store, started);
    }
    let mut made = Made {
        events: started.events,
        ..Made::default()
    };
    let started = started.value;
    let placed = place(
        store, ctx, world, &as_ship, &project, role, &scout, &request, &started, &mut made,
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
        // `made` holds no worktree: the scout's is not this run's to return.
        Err(error) => Err(fail(store, ctx, world, &started.run, &made, error)),
    }
}

#[allow(clippy::too_many_arguments)]
fn place(
    store: &mut Store,
    ctx: &Ctx,
    world: &World<'_>,
    card: &Card,
    project: &Project,
    role: crate::roles::Role,
    scout: &Run,
    request: &Request,
    started: &tiller_core::state::runs::Started,
    made: &mut Made,
) -> Result<Spawned> {
    let run = &started.run;
    let (Some(path), Some(base_sha), Some(branch)) =
        (&run.worktree_path, &run.base_sha, &run.branch)
    else {
        return Err(SpawnError::Refused(format!(
            "scout run {} recorded no worktree, branch or base, so it cannot be promoted",
            scout.id
        )));
    };
    let worktree = Path::new(path);
    // The scout's work stays; the reading only attributes the worktree to
    // the builder, and a worktree gone since is a refusal.
    let (scan, written) = scan_and_record(store, ctx, world.scanner, run, project, worktree)?;
    made.events.extend(written);
    if !scan.exists || !scan.is_worktree {
        return Err(SpawnError::Refused(format!(
            "the scout's worktree {path} is gone or not a git worktree"
        )));
    }
    let brief = match &request.brief {
        Some(brief) => brief.clone(),
        None => promoted_brief(card, scout),
    };
    launch_in(
        store,
        ctx,
        world,
        card,
        project,
        role,
        started,
        worktree,
        branch,
        base_sha.clone(),
        brief,
        made,
    )
}
