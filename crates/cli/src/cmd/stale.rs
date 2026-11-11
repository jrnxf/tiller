//! `tiller agent restart --stale`: restarts the sessions that run an older
//! tiller. The restart itself is the existing one - `tiller orchestrator
//! start --restart` for an orchestrator, `tiller agent relaunch` on the same
//! run for a worker - so this only picks the sessions and the moment.

use std::path::Path;

use serde_json::{Value, json};
use tiller_agents::control::{self, RelaunchRequest, Waits};
use tiller_agents::spawn::readings;
use tiller_agents::stale::{self, Outcome as Restarted, Report, StaleSession};
use tiller_core::domain::Session;
use tiller_core::state::get;
use tiller_core::{Ctx, Store};

use super::control::disk_free_mb;
use super::spawn::{Tools, failure, orchestrator_homes, start_orchestrator};
use crate::app::{App, Done, Outcome};

/// Restarts one session on this binary: the same path the commands take.
pub fn restart_session(
    tools: &Tools,
    homes: &Path,
    store: &mut Store,
    ctx: &Ctx,
    session: &Session,
) -> Result<(), String> {
    if stale::is_orchestrator(session) {
        return start_orchestrator(tools, homes, store, ctx, &session.project_id, true)
            .map(|_| ())
            .map_err(|error| failure(error).message.clone());
    }
    let run_id = session
        .run_id
        .as_deref()
        .ok_or_else(|| format!("session {} has no run to relaunch", session.id))?;
    let run = get(store.conn(), "run", run_id).map_err(|error| error.to_string())?;
    let world = tools.world(readings::ram_free_mb(), disk_free_mb(&run));
    control::relaunch(
        store,
        ctx,
        None,
        &world,
        &Waits::default(),
        &run,
        RelaunchRequest {
            note: format!(
                "Restarted on tiller {} to pick up the current role prompt and hooks.",
                stale::current_version()
            ),
            new_attempt: false,
            brief: None,
            asked: None,
        },
    )
    .map(|_| ())
    .map_err(|error| failure(error).message.clone())
}

/// The session as the listing and the restart report show it.
fn row(session: &Session, stale: &stale::Stale) -> Value {
    json!({
        "session": session.id,
        "role": session.role,
        "project": session.project_id,
        "run": session.run_id,
        "started_on": stale.since,
        "prompt_changed": stale.prompt_changed,
        "restart_queued": session.restart_queued_at.is_some(),
    })
}

fn listed(item: &StaleSession) -> Value {
    let mut row = row(&item.session, &item.stale);
    row["idle"] = json!(item.idle);
    row["action"] = json!(if item.idle {
        "restarts now"
    } else {
        "restarts when idle"
    });
    row
}

fn reported(report: &Report) -> Value {
    let mut row = row(&report.session, &report.stale);
    let (outcome, error) = match &report.outcome {
        Restarted::Restarted => ("restarted", None),
        Restarted::Queued => ("queued", None),
        Restarted::Failed(why) => ("failed", Some(why.as_str())),
    };
    row["outcome"] = json!(outcome);
    row["error"] = json!(error);
    row
}

/// `tiller agent restart --stale [--dry-run]`.
pub fn restart(app: &App, dry_run: bool) -> Done {
    app.require_owner("tiller agent restart")?;
    let now = stale::current_version();
    if dry_run {
        let store = app.read()?;
        let sessions: Vec<Value> = stale::stale_sessions(&store)
            .map_err(failure_state)?
            .iter()
            .map(listed)
            .collect();
        return Ok(Outcome::read(json!({
            "tiller_version": now,
            "dry_run": true,
            "sessions": sessions,
        })));
    }
    if stale::stale_sessions(&app.read()?)
        .map_err(failure_state)?
        .is_empty()
    {
        return Ok(Outcome::read(json!({
            "tiller_version": now,
            "dry_run": false,
            "sessions": [],
        })));
    }
    let tools = Tools::load(app)?;
    let homes = orchestrator_homes(&tools)?;
    let mut store = app.write()?;
    let ctx = app.ctx();
    let reports = stale::restart_stale(&mut store, &ctx, None, &mut |store, ctx, session| {
        restart_session(&tools, &homes, store, ctx, session)
    })
    .map_err(failure_state)?;
    let failed = reports
        .iter()
        .filter(|report| matches!(report.outcome, Restarted::Failed(_)))
        .count();
    let warning = (failed > 0).then(|| format!("{failed} session(s) did not restart"));
    Ok(Outcome::read(json!({
        "tiller_version": now,
        "dry_run": false,
        "sessions": reports.iter().map(reported).collect::<Vec<_>>(),
    }))
    .with_warning(warning))
}

fn failure_state(error: tiller_core::Error) -> crate::app::Failure {
    error.into()
}
