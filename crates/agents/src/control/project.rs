//! Deleting or retiring a project with its agents still running (`state.md`,
//! Deleting a project): the owner's delete stops them first, with the proven
//! stop `tiller agent stop` uses, cancels the cards their runs worked on,
//! then deletes. A retire stops them first when the owner asks. Only a stop
//! that fails refuses, naming the agent; nothing is removed then.

use std::collections::BTreeSet;

use serde::Serialize;
use tiller_core::domain::{Project, Run, Session};
use tiller_core::state::deletion::{Deletion, LiveAgent};
use tiller_core::state::get;
use tiller_core::{Actor, Applied, Ctx, Error, Store, Timestamp};
use tiller_herdr::client::Herdr;
use tiller_herdr::layout;

use super::{STOPPED, Waits, stop, stop_orchestrator};
use crate::spawn::{Result, SpawnError, local_host, relayout};

/// One agent a project delete or retire stopped.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StoppedAgent {
    #[serde(flatten)]
    pub agent: LiveAgent,
    /// How it was proven stopped; `None` for a run with no live session,
    /// which only had its card cancelled.
    pub stop_proof: Option<String>,
}

/// What stopping a project's agents did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AgentsStopped {
    pub stopped: Vec<StoppedAgent>,
    /// The cards cancelled because a run of theirs was still active.
    pub cancelled_cards: Vec<String>,
    /// What the stops left undone: a pane that would not close, panes not
    /// relabelled.
    pub warnings: Vec<String>,
}

/// What [`delete_project`] did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectDeleted {
    #[serde(flatten)]
    pub deletion: Deletion,
    #[serde(flatten)]
    pub agents: AgentsStopped,
}

/// What [`retire_project`] did.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProjectRetired {
    pub project: Project,
    #[serde(flatten)]
    pub agents: AgentsStopped,
}

/// The owner's `project.delete`: refuses first for what no stop can clear
/// (`fleet`, another host's project or agent, an executing approval, rows
/// of another project), then stops the project's agents, the orchestrator
/// first so it starts nothing new, cancels each card a run of the project
/// still works on, and deletes. A stop that fails refuses with the agent
/// named, and the project stays; agents stopped before it stay stopped.
/// `ctx.key` keys the delete itself, so a retry after it lands replays it.
pub fn delete_project(
    store: &mut Store,
    ctx: &Ctx,
    herdr: &dyn Herdr,
    waits: &Waits,
    project_id: &str,
) -> Result<Applied<ProjectDeleted>> {
    require_owner(ctx, "project.delete")?;
    let preview = match store.project_delete_preview(project_id) {
        Ok(preview) => preview,
        // Gone already: a retry of a delete that landed replays by its key,
        // and anything else is refused by the delete as it would be alone.
        Err(Error::NotFound { .. }) => {
            let deleted = store.project_delete(ctx, project_id)?;
            return Ok(Applied {
                value: ProjectDeleted {
                    deletion: deleted.value,
                    agents: AgentsStopped::default(),
                },
                events: deleted.events,
                replayed: deleted.replayed,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(fact) = preview.refused {
        return Err(Error::Refused {
            entity: "project",
            id: project_id.to_owned(),
            fact,
        }
        .into());
    }
    let reason = format!("deleting project {project_id}");
    // The stops' events are the project's, so the delete removes them: only
    // its tombstone is left to report.
    let (agents, _) = stop_project_agents(store, ctx, herdr, waits, project_id, &reason)?;
    // The delete records its own time, after the stops it waited on.
    let ctx = Ctx {
        now: Timestamp::now(),
        ..ctx.clone()
    };
    let deleted = store.project_delete(&ctx, project_id)?;
    Ok(Applied {
        value: ProjectDeleted {
            deletion: deleted.value,
            agents,
        },
        events: deleted.events,
        replayed: deleted.replayed,
    })
}

/// The owner's `project.retire`, with the project's running agents stopped
/// first when `stop_agents` is set. Refused, before any stop, when the
/// retire would be: an open card, or `fleet`.
pub fn retire_project(
    store: &mut Store,
    ctx: &Ctx,
    herdr: &dyn Herdr,
    waits: &Waits,
    project_id: &str,
    stop_agents: bool,
) -> Result<Applied<ProjectRetired>> {
    require_owner(ctx, "project.retire")?;
    let preview = store.project_retire_preview(project_id)?;
    if let Some(fact) = preview.refused {
        return Err(Error::Refused {
            entity: "project",
            id: project_id.to_owned(),
            fact,
        }
        .into());
    }
    let (agents, mut events) = if stop_agents && !preview.agents.is_empty() {
        let reason = format!("retiring project {project_id}");
        stop_project_agents(store, ctx, herdr, waits, project_id, &reason)?
    } else {
        (AgentsStopped::default(), Vec::new())
    };
    let ctx = Ctx {
        now: Timestamp::now(),
        ..ctx.clone()
    };
    let retired = store.project_retire(&ctx, project_id)?;
    events.extend(retired.events);
    Ok(Applied {
        value: ProjectRetired {
            project: retired.value,
            agents,
        },
        events,
        replayed: retired.replayed,
    })
}

/// Stops every running agent of the project with the proven stop and ends
/// its row `stopped`, then cancels the card of each run still active with
/// `reason`. The steps take no key: only the delete or retire after them is
/// keyed. Refuses at the first agent another host runs or that does not
/// stop, naming it.
fn stop_project_agents(
    store: &mut Store,
    ctx: &Ctx,
    herdr: &dyn Herdr,
    waits: &Waits,
    project_id: &str,
    reason: &str,
) -> Result<(AgentsStopped, Vec<i64>)> {
    let agents = store.project_live_agents(project_id)?;
    if let Some(remote) = agents.iter().find(|LiveAgent { local, .. }| !local) {
        return Err(SpawnError::Refused(format!(
            "{} runs on another host; stop it there first. Nothing was stopped, and project {project_id} was kept",
            remote.describe()
        )));
    }
    let host = local_host(store)?;
    let mut done = AgentsStopped::default();
    let mut events = Vec::new();
    let mut cancelled = BTreeSet::new();
    for agent in agents {
        let step = || Ctx::new(ctx.actor.clone());
        let mut stop_proof = None;
        if let Some(id) = &agent.session {
            // Judged as the session is now: it may have ended on its own.
            let session: Session = get(store.conn(), "session", id)?;
            if session.ended_at.is_none() {
                let stopped = if session.run_id.is_none() {
                    stop_orchestrator(store, &step(), herdr, waits, &session, STOPPED)
                        .map(|stopped| (stopped.proof, stopped.warning, stopped.events))
                } else {
                    stop_worker(store, &step(), herdr, waits, &session)
                };
                match stopped {
                    Ok((proof, warning, ended)) => {
                        events.extend(ended);
                        done.warnings.extend(warning);
                        stop_proof = Some(proof);
                    }
                    Err(SpawnError::State(error)) => {
                        return Err(midway(error, &agent, &done, project_id));
                    }
                    // Any other failure, herdr's included, is a stop that
                    // did not happen: refused with the agent named.
                    Err(error) => {
                        return Err(SpawnError::Refused(refusal(
                            &agent,
                            &error.to_string(),
                            &done,
                            project_id,
                        )));
                    }
                }
            }
        }
        // A worker's run ends with its card: the owner cannot end a run
        // directly, and the card goes with the project anyway.
        if let (Some(run), Some(card)) = (&agent.run, &agent.card) {
            let run: Run = get(store.conn(), "run", run)?;
            if run.ended_at.is_none() && cancelled.insert(card.clone()) {
                let cancel = store
                    .card_cancel(&step(), card, reason)
                    .map_err(|error| midway(error, &agent, &done, project_id))?;
                events.extend(cancel.events);
                done.cancelled_cards.push(card.clone());
            }
        }
        done.stopped.push(StoppedAgent { agent, stop_proof });
    }
    if done
        .stopped
        .iter()
        .any(|stopped| stopped.stop_proof.is_some())
    {
        // Panes closed: the rows are ended, so a layout left undone is a
        // warning.
        match layout::bindings(store.conn(), &host) {
            Ok(bindings) => {
                if let Err(e) = relayout(herdr, &bindings) {
                    done.warnings.push(e);
                }
            }
            Err(e) => done
                .warnings
                .push(format!("the panes were not relabelled: {e}")),
        }
    }
    Ok((done, events))
}

/// Stops a worker's agent with [`stop`], ends its row and closes its pane
/// when the pane still names it. Its run is left to the card cancel.
fn stop_worker(
    store: &mut Store,
    ctx: &Ctx,
    herdr: &dyn Herdr,
    waits: &Waits,
    session: &Session,
) -> Result<(String, Option<String>, Vec<i64>)> {
    let (proof, pane) = stop(store, herdr, waits, session)?;
    let events = store.session_end(ctx, &session.id, STOPPED)?.events;
    let warning = pane.and_then(|pane| {
        herdr.close_pane(&pane).err().map(|e| {
            format!(
                "the stopped session {} ({proof}) left pane {pane} open: {e}",
                session.id
            )
        })
    });
    Ok((proof, warning, events))
}

/// Why a stop refused, with the agents already stopped.
fn refusal(agent: &LiveAgent, why: &str, done: &AgentsStopped, project_id: &str) -> String {
    let mut message = format!(
        "could not stop the {}: {why}. Project {project_id} was kept",
        agent.describe()
    );
    let stopped: Vec<String> = done
        .stopped
        .iter()
        .filter(|stopped| stopped.stop_proof.is_some())
        .map(|stopped| stopped.agent.describe())
        .collect();
    if !stopped.is_empty() {
        message.push_str(&format!("; already stopped: {}", stopped.join(", ")));
    }
    if !done.cancelled_cards.is_empty() {
        message.push_str(&format!(
            "; already cancelled: {}",
            done.cancelled_cards.join(", ")
        ));
    }
    message
}

/// A store refusal part way through the stops, said like a failed stop:
/// with the agent and what was already stopped and cancelled.
fn midway(error: Error, agent: &LiveAgent, done: &AgentsStopped, project_id: &str) -> SpawnError {
    match error {
        Error::Refused { fact, .. } => SpawnError::Refused(refusal(agent, &fact, done, project_id)),
        other => other.into(),
    }
}

fn require_owner(ctx: &Ctx, what: &str) -> Result<()> {
    if matches!(ctx.actor, Actor::Owner | Actor::Board) {
        Ok(())
    } else {
        Err(Error::Forbidden(format!("{what} is owner only; {} may not", ctx.actor)).into())
    }
}
