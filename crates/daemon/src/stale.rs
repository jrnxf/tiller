//! Restarting the sessions that run an older tiller (`POST
//! /api/v1/agents/restart-stale`), and the task that restarts the queued ones
//! once they go idle. The restart itself is the binary's: the same one
//! `tiller orchestrator start --restart` and `tiller agent relaunch` use.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tiller_agents::stale::{self, Outcome, Report};
use tiller_core::{Actor, Ctx, Store};
use tokio::sync::watch;

use crate::api::OrchestratorStart;
use crate::dto::{RestartResult, RestartStaleResponse};
use crate::readonly::Damage;

/// How often the queued restarts are checked for an idle session.
const DRAIN_EVERY: Duration = Duration::from_secs(10);

/// One restart pass at a time: a board click and the queue's drain must not
/// both stop and relaunch the same session, and the second one reads the
/// stamp the first wrote.
static PASS: Mutex<()> = Mutex::new(());

fn one_at_a_time() -> MutexGuard<'static, ()> {
    PASS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Restarts the idle stale sessions, in `only` when it is given, and queues
/// the working ones. Runs as the owner: the board is signed in as them.
pub fn restart_stale(
    store: &mut Store,
    starter: &dyn OrchestratorStart,
    only: Option<&[String]>,
) -> tiller_core::Result<RestartStaleResponse> {
    let _pass = one_at_a_time();
    let owner = Ctx::new(Actor::Owner);
    let board = Ctx::new(Actor::Board);
    let reports = stale::restart_stale(store, &board, only, &mut |store, session| {
        starter.restart(store, &owner, session)
    })?;
    Ok(response(&reports))
}

fn response(reports: &[Report]) -> RestartStaleResponse {
    RestartStaleResponse {
        sessions: reports
            .iter()
            .map(|report| {
                let (outcome, error) = match &report.outcome {
                    Outcome::Restarted => ("restarted", None),
                    Outcome::Queued => ("queued", None),
                    Outcome::Failed(why) => ("failed", Some(why.clone())),
                };
                RestartResult {
                    session_id: report.session.id.clone(),
                    outcome: outcome.to_owned(),
                    error,
                }
            })
            .collect(),
    }
}

/// One pass over the queue: restarts every queued session that is idle now.
pub fn drain(store: &mut Store, starter: &dyn OrchestratorStart) -> tiller_core::Result<usize> {
    let _pass = one_at_a_time();
    let owner = Ctx::new(Actor::Owner);
    let board = Ctx::new(Actor::Board);
    let reports = stale::drain_queue(store, &board, &mut |store, session| {
        starter.restart(store, &owner, session)
    })?;
    for report in &reports {
        match &report.outcome {
            Outcome::Restarted => tracing::info!(
                session = %report.session.id,
                "restarted a queued session that went idle"
            ),
            Outcome::Failed(why) => tracing::warn!(
                session = %report.session.id,
                error = %why,
                "a queued restart failed; the session shows as stale again"
            ),
            Outcome::Queued => {}
        }
    }
    Ok(reports.len())
}

/// Checks the queue until `stop` turns true. The queue is in `state.db`, so a
/// daemon that restarts picks it up where the last one left off.
pub async fn drain_loop(
    db: PathBuf,
    starter: Arc<dyn OrchestratorStart>,
    damage: Damage,
    mut stop: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(DRAIN_EVERY);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stop.wait_for(|stop| *stop) => return,
        }
        // A restart is outside work: none while the owner marker names
        // another system (R20).
        let held = db
            .parent()
            .is_some_and(|home| tiller_core::owner::check(home).is_err());
        if !db.exists() || damage.reason().is_some() || held {
            continue;
        }
        let (path, starter) = (db.clone(), Arc::clone(&starter));
        let pass = tokio::task::spawn_blocking(move || {
            let mut store = Store::open_current(&path)?;
            if store.restart_queued()?.is_empty() {
                return Ok(0);
            }
            drain(&mut store, starter.as_ref())
        })
        .await;
        match pass {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                damage.suspect(&db, &error);
                tracing::warn!(%error, "could not restart the queued sessions");
            }
            Err(error) => tracing::warn!(%error, "the queued-restart pass panicked"),
        }
    }
}
