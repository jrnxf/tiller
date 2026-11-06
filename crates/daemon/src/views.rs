//! The read side of the board API: each function builds one response from
//! `state.db`. The HTTP handlers, the boot snapshot and the SSE stream all
//! call these, so a snapshot entry is exactly what the matching GET returns
//! (`protocol.md` section 7.6).

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use tiller_core::domain::{
    Approval, Artifact, Attention, Card, CardDep, CardState, Checks, Event, Hold, HostKind,
    Obligation, Pane, PeerScope, Pr, QuotaWindow as QuotaRow, Review, Role, Run, RunOutcome,
    Session,
};
use tiller_core::state::attention::{OwnerAction as CoreOwnerAction, stale};
use tiller_core::state::interventions::InterventionSource;
use tiller_core::state::signals::lisbon_weeks;
use tiller_core::state::{find, get, list, review};
use tiller_core::{Result, Store, Timestamp};
use tiller_herdr::poller::Health as PollerRecord;

use crate::dto::*;
use crate::peer::{Federation, PeerView};

/// A quota window older than this is stale (`protocol.md` section 8).
pub const QUOTA_STALE_MINUTES: i64 = 15;
/// The weekly floor: at or under it the quota dot is red (AGENTS.md rule 8).
pub const WEEKLY_FLOOR_PERCENT: f64 = 3.0;
/// A remote host unseen this long is stale (`protocol.md` section 9).
pub const REMOTE_STALE_MINUTES: i64 = 2;
/// How many Lisbon weeks the Health page shows.
pub const SIGNAL_WEEKS: usize = 8;
/// One page of a card's timeline.
pub const TIMELINE_PAGE: i64 = 50;
/// The event lines a detail page shows.
const DETAIL_EVENTS: i64 = 20;

/// The board's filter (`GET /api/v1/board?filter=`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Active,
    Needs,
}

impl std::str::FromStr for Filter {
    type Err = tiller_core::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "all" => Ok(Filter::All),
            "active" => Ok(Filter::Active),
            "needs" => Ok(Filter::Needs),
            other => Err(tiller_core::Error::Usage(format!(
                "filter is all, active or needs, not {other:?}"
            ))),
        }
    }
}

impl Filter {
    /// Whether a row in `column`, waiting on the owner or not, passes the
    /// filter.
    fn keeps(self, column: &str, needs_you: bool) -> bool {
        match self {
            Filter::All => true,
            Filter::Active => matches!(column, "working" | "landing"),
            Filter::Needs => needs_you || column == "asked",
        }
    }
}

fn at(time: Timestamp) -> String {
    time.to_string()
}

fn at_opt(time: Option<Timestamp>) -> Option<String> {
    time.map(at)
}

fn one<T: rusqlite::types::FromSql>(
    store: &Store,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Option<T>> {
    Ok(store
        .conn()
        .query_row(sql, params, |row| row.get::<_, Option<T>>(0))
        .optional()?
        .flatten())
}

// -- board ------------------------------------------------------------------

/// The display fields the board joins onto a row.
struct RowExtras {
    /// The newest run's host and model.
    host_id: Option<String>,
    model: Option<String>,
    /// The newest PR's number.
    pr_number: Option<i64>,
    /// When the active run's session was last seen working.
    observed_at: Option<String>,
}

fn row_extras(store: &Store, card_id: &str) -> Result<RowExtras> {
    let newest: Option<Run> = find(
        store.conn(),
        "card_id = ?1 ORDER BY started_at DESC, id DESC",
        &[&card_id],
    )?;
    let pr: Option<i64> = one(
        store,
        "SELECT number FROM prs WHERE card_id = ?1 ORDER BY linked_at DESC, id DESC LIMIT 1",
        &[&card_id],
    )?;
    let observed: Option<Timestamp> = one(
        store,
        "SELECT max(s.activity_observed_at) FROM sessions s JOIN runs r ON r.id = s.run_id
         WHERE r.card_id = ?1 AND r.ended_at IS NULL AND s.ended_at IS NULL",
        &[&card_id],
    )?;
    Ok(RowExtras {
        host_id: newest.as_ref().map(|run| run.host_id.clone()),
        model: newest.map(|run| run.model),
        pr_number: pr,
        observed_at: at_opt(observed),
    })
}

/// One card's board row, or `None` when the card has left the board (closed
/// more than three days ago).
pub fn board_row(store: &Store, card_id: &str) -> Result<Option<BoardRow>> {
    let Some(column) = store.card_column(card_id)? else {
        return Ok(None);
    };
    let card: Card = get(store.conn(), "card", card_id)?;
    let RowExtras {
        host_id,
        model,
        pr_number,
        observed_at,
    } = row_extras(store, card_id)?;
    Ok(Some(BoardRow {
        id: card.id,
        display_id: card.display_id,
        project_id: card.project_id,
        host_id,
        kind: card.kind.to_string(),
        title: card.title,
        priority: card.priority,
        column: column.column.to_string(),
        badge: column.badge,
        needs_you: column.needs_you,
        parked_until: at_opt(column.parked_until),
        parked_reason: column.parked_reason,
        blocked_by: column.blocked_by,
        closed_at: at_opt(column.closed_at),
        rev: card.rev,
        model,
        pr_number,
        observed_at,
        host: None,
        as_of: None,
        link: None,
    }))
}

/// One card's row as the board lists it: [`board_row`], and `None` too when
/// the card's project is retired, as [`board`] leaves it out.
fn listed_row(store: &Store, card_id: &str) -> Result<Option<BoardRow>> {
    let Some(row) = board_row(store, card_id)? else {
        return Ok(None);
    };
    let project = store.project(&row.project_id)?;
    Ok(project.retired_at.is_none().then_some(row))
}

/// `GET /api/v1/board`. `host` is a host id or label; a card is on a host
/// when it has a run there.
pub fn board(
    store: &Store,
    project: Option<&str>,
    filter: Filter,
    host: Option<&str>,
) -> Result<BoardResponse> {
    let host_id = host.map(|host| host_id(store, host)).transpose()?;
    // A retired project has left the board, its closed cards with it.
    let retired: HashSet<String> = store
        .projects()?
        .into_iter()
        .filter(|project| project.retired_at.is_some())
        .map(|project| project.id)
        .collect();
    let mut rows = Vec::new();
    for column in store.card_columns(project)? {
        if !filter.keeps(column.column.as_str(), column.needs_you)
            || retired.contains(&column.project_id)
        {
            continue;
        }
        if let Some(host_id) = &host_id {
            let here: Option<i64> = one(
                store,
                "SELECT 1 FROM runs WHERE card_id = ?1 AND host_id = ?2 LIMIT 1",
                &[&column.id, host_id],
            )?;
            if here.is_none() {
                continue;
            }
        }
        if let Some(row) = board_row(store, &column.id)? {
            rows.push(row);
        }
    }
    let asked = store
        .asked(project)?
        .into_iter()
        .map(|row| AskedRow {
            id: row.ask.id,
            project_id: row.ask.project_id,
            session_id: row.ask.session_id,
            received_at: at(row.ask.received_at),
            age_seconds: row.age_seconds.round(),
            host: None,
            as_of: None,
            link: None,
        })
        .collect();
    Ok(BoardResponse { rows, asked })
}

pub(crate) fn host_id(store: &Store, host: &str) -> Result<String> {
    store
        .hosts()?
        .into_iter()
        .find(|row| row.id == host || row.label == host)
        .map(|row| row.id)
        .ok_or_else(|| tiller_core::Error::NotFound {
            entity: "host",
            id: host.to_owned(),
            candidates: Vec::new(),
        })
}

// -- attention ----------------------------------------------------------------

/// One attention item in its API shape.
pub fn attention_item(store: &Store, item: &Attention) -> Result<AttentionItem> {
    let options = item
        .options
        .as_ref()
        .and_then(Value::as_array)
        .map(|options| options.iter().map(attention_option).collect());
    // A merge item names the approval it asks for: the newest open merge
    // approval on the card.
    let approval: Option<Approval> = if item.kind.as_str() == "merge" {
        find(
            store.conn(),
            "card_id = ?1 AND kind IN ('merge', 'land_direct')
             AND state IN ('requested', 'accepted', 'stale')
             ORDER BY requested_at DESC, id DESC",
            &[&item.card_id],
        )?
    } else {
        None
    };
    let approval = match approval {
        Some(approval) => {
            let pr_number = match &approval.pr_id {
                Some(pr_id) => Some(get::<Pr>(store.conn(), "pr", pr_id)?.number),
                None => None,
            };
            Some(ApprovalBinding {
                id: approval.id,
                kind: approval.kind.to_string(),
                head_sha: approval.head_sha.unwrap_or_default(),
                pr_number,
                state: approval.state.to_string(),
            })
        }
        None => None,
    };
    let open = item.answered_at.is_none() && item.withdrawn_at.is_none();
    let stale = if open {
        stale(store.conn(), item)?
    } else {
        None
    };
    Ok(AttentionItem {
        id: item.id.clone(),
        card_id: item.card_id.clone(),
        kind: item.kind.to_string(),
        question: item.question.clone(),
        evidence: item.evidence.clone(),
        recommendation: item.recommendation.clone(),
        consequence: item.consequence.clone(),
        options,
        approval,
        opened_by_session_id: item.opened_by_session_id.clone(),
        opened_at: at(item.opened_at),
        still_current: open && stale.is_none(),
        stale: stale.map(|stale| StaleItem {
            reason: stale.reason.as_str().to_owned(),
            subject: stale.subject,
            at: at_opt(stale.at),
        }),
        subject_session_id: item.subject_session_id.clone(),
        subject_project_id: item.subject_project_id.clone(),
        answered_at: at_opt(item.answered_at),
        withdrawn_at: at_opt(item.withdrawn_at),
        rev: item.rev,
        host_id: None,
        host: None,
        as_of: None,
        link: None,
    })
}

impl From<CoreOwnerAction> for OwnerAction {
    fn from(action: CoreOwnerAction) -> Self {
        match action {
            CoreOwnerAction::ProjectCapacity { project, from, to } => {
                Self::ProjectCapacity { project, from, to }
            }
        }
    }
}

/// An option is either a bare label or `{label, consequence, recommended,
/// action}`; core reads the action as the answer does.
fn attention_option(option: &Value) -> AttentionOption {
    match option {
        Value::String(label) => AttentionOption {
            label: label.clone(),
            consequence: None,
            recommended: false,
            action: None,
        },
        other => AttentionOption {
            label: other
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            consequence: other
                .get("consequence")
                .and_then(Value::as_str)
                .map(str::to_owned),
            recommended: other
                .get("recommended")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            action: CoreOwnerAction::of_option(other).map(OwnerAction::from),
        },
    }
}

/// `GET /api/v1/attention?state=open`: the open queue, oldest first, and the
/// sessions waiting on a permission prompt.
pub fn attention(store: &Store) -> Result<AttentionResponse> {
    let open: Vec<Attention> = list(
        store.conn(),
        "answered_at IS NULL AND withdrawn_at IS NULL ORDER BY opened_at, id",
        &[],
    )?;
    let items = open
        .iter()
        .map(|item| attention_item(store, item))
        .collect::<Result<_>>()?;
    let waiting: Vec<Session> = list(
        store.conn(),
        "ended_at IS NULL AND activity = 'waiting_permission' ORDER BY activity_observed_at, id",
        &[],
    )?;
    let mut permission_prompts = Vec::new();
    for session in waiting {
        let card_id = match &session.run_id {
            Some(run_id) => get::<Run>(store.conn(), "run", run_id)?.card_id,
            None => continue,
        };
        let tool = prompt_tool(store, &session.id)?;
        permission_prompts.push(PermissionPrompt {
            session_id: session.id,
            card_id,
            tool: tool.unwrap_or_else(|| "a tool".to_owned()),
            // Tool inputs are never stored (`protocol.md` 4.1).
            detail: None,
            since: at_opt(session.activity_observed_at).unwrap_or_else(|| at(session.started_at)),
            host: None,
            as_of: None,
            link: None,
        });
    }
    Ok(AttentionResponse {
        items,
        permission_prompts,
    })
}

/// The tool a waiting session's permission prompt is for: the one the
/// newest `PermissionRequest` names, as the dialog opens, since a parallel
/// tool call can be observed after it. A harness that sends none falls back
/// to the newest observation that names a tool.
pub fn prompt_tool(store: &Store, session_id: &str) -> Result<Option<String>> {
    one(
        store,
        "SELECT json_extract(payload, '$.tool_name') FROM observations
         WHERE session_id = ?1 AND json_extract(payload, '$.tool_name') IS NOT NULL
         ORDER BY event = 'PermissionRequest' DESC, observed_at DESC, id DESC LIMIT 1",
        &[&session_id],
    )
}

// -- agents -------------------------------------------------------------------

fn agent_row(store: &Store, session: &Session) -> Result<AgentRow> {
    let effective: Option<String> = one(
        store,
        "SELECT effective_activity FROM session_activity WHERE session_id = ?1",
        &[&session.id],
    )?;
    let pane: Option<Pane> = find(
        store.conn(),
        "session_id = ?1 AND gone_at IS NULL",
        &[&session.id],
    )?;
    let run: Option<Run> = match &session.run_id {
        Some(run_id) => Some(get(store.conn(), "run", run_id)?),
        None => None,
    };
    let card: Option<Card> = match &run {
        Some(run) => Some(get(store.conn(), "card", &run.card_id)?),
        None => None,
    };
    let herdr_name = match (&run, &card) {
        (Some(run), Some(card)) => Some(herdr_name(&card.display_id, run)),
        _ => pane.as_ref().and_then(|pane| pane.agent_name.clone()),
    };
    // The model: the run's, else the newest one a hook reported.
    let reported: Option<String> = one(
        store,
        "SELECT json_extract(payload, '$.model') FROM observations
         WHERE session_id = ?1 AND json_extract(payload, '$.model') IS NOT NULL
         ORDER BY observed_at DESC, id DESC LIMIT 1",
        &[&session.id],
    )?;
    let model = run
        .as_ref()
        .map(|run| run.model.clone())
        .or(reported)
        .unwrap_or_default();
    let role = match (&run, session.role) {
        (_, Role::Orchestrator) => "orchestrator".to_owned(),
        (Some(run), _) => run.role.short_name().to_owned(),
        (None, role) => role.to_string(),
    };
    // When the session entered its current activity: the oldest observation
    // after the newest one with another verdict.
    let state_since: Option<Timestamp> = one(
        store,
        "SELECT min(observed_at) FROM observations
         WHERE session_id = ?1 AND verdict = ?2 AND observed_at > coalesce(
           (SELECT max(observed_at) FROM observations
            WHERE session_id = ?1 AND verdict IS NOT NULL AND verdict != ?2), '')",
        &[&session.id, &session.activity],
    )?;
    // The newest sign of the agent's own output: a tool call, a subagent or
    // the end of a turn, as hooks report them. Owner input, notifications and
    // herdr's polls do not count, and a host with hooks blocked sees no tool
    // calls, so it gets none. Tool calls are coalesced (4.3): this lags by up
    // to 60 s.
    let last_output_at: Option<Timestamp> = one(
        store,
        "SELECT observed_at FROM observations
         WHERE session_id = ?1 AND source = 'hook' AND (event IN ('PreToolUse', 'PostToolUse',
           'PostToolUseFailure', 'SubagentStart', 'SubagentStop', 'Stop')
           OR event LIKE 'StopFailure:%')
         ORDER BY observed_at DESC LIMIT 1",
        &[&session.id],
    )?;
    Ok(AgentRow {
        session_id: session.id.clone(),
        role,
        model,
        activity: effective.unwrap_or_else(|| session.activity.to_string()),
        activity_observed_at: at_opt(session.activity_observed_at),
        last_status_line: run.as_ref().and_then(|run| run.last_status_line.clone()),
        herdr_name,
        card_id: card.map(|card| card.id),
        host_id: session.host_id.clone(),
        workers: Vec::new(),
        project_id: session.project_id.clone(),
        run_id: session.run_id.clone(),
        pane_id: session.pane_id.clone(),
        workspace_label: pane.and_then(|pane| pane.workspace_label),
        state_since: at_opt(state_since),
        last_output_at: at_opt(last_output_at),
        host: None,
        as_of: None,
        link: None,
    })
}

/// A run's herdr agent name (`protocol.md` section 6.1).
pub fn herdr_name(display_id: &str, run: &Run) -> String {
    let base = format!("{display_id}-{}", run.role.short_name());
    if run.attempt > 1 {
        format!("{base}-{}", run.attempt)
    } else {
        base
    }
}

/// `GET /api/v1/agents`: live orchestrators with their live workers nested;
/// a worker whose orchestrator has ended is listed on its own. With `host`,
/// only sessions on that host (an orchestrator elsewhere still carries its
/// workers here).
pub fn agents(store: &Store, host: Option<&str>) -> Result<AgentsResponse> {
    let host_id = host.map(|host| host_id(store, host)).transpose()?;
    let on_host = |session: &Session| host_id.as_ref().is_none_or(|id| &session.host_id == id);
    let sessions: Vec<Session> = list(
        store.conn(),
        "ended_at IS NULL ORDER BY project_id, started_at, id",
        &[],
    )?;
    let mut supervisor: HashMap<&str, String> = HashMap::new();
    for session in &sessions {
        if let Some(run_id) = &session.run_id {
            let run: Run = get(store.conn(), "run", run_id)?;
            supervisor.insert(session.id.as_str(), run.supervisor_session_id);
        }
    }
    let orchestrator_ids: Vec<&str> = sessions
        .iter()
        .filter(|s| s.role == Role::Orchestrator)
        .map(|s| s.id.as_str())
        .collect();
    let mut orchestrators = Vec::new();
    for orchestrator in sessions.iter().filter(|s| s.role == Role::Orchestrator) {
        let mut workers = Vec::new();
        for worker in sessions.iter().filter(|s| s.role != Role::Orchestrator) {
            if supervisor.get(worker.id.as_str()).map(String::as_str)
                == Some(orchestrator.id.as_str())
                && on_host(worker)
            {
                workers.push(agent_row(store, worker)?);
            }
        }
        if on_host(orchestrator) || !workers.is_empty() {
            let mut row = agent_row(store, orchestrator)?;
            row.workers = workers;
            orchestrators.push(row);
        }
    }
    for worker in sessions.iter().filter(|s| s.role != Role::Orchestrator) {
        let supervised = supervisor
            .get(worker.id.as_str())
            .is_some_and(|id| orchestrator_ids.contains(&id.as_str()));
        if !supervised && on_host(worker) {
            orchestrators.push(agent_row(store, worker)?);
        }
    }
    Ok(AgentsResponse { orchestrators })
}

// -- machines -----------------------------------------------------------------

/// What the daemon knows about its own health beyond `state.db`.
#[derive(Clone, Debug, Default)]
pub struct LocalHealth {
    /// Pollers whose last poll failed.
    pub failing_pollers: Vec<String>,
    /// Every paired peer's link and cached rows (`protocol.md` 10.2).
    pub peers: Federation,
}

/// `GET /api/v1/machines` (`protocol.md` section 9).
pub fn machines(store: &Store, local: &LocalHealth, now: Timestamp) -> Result<MachinesResponse> {
    let capacity = store.capacity()?;
    let quota_stale = quota_is_stale(store, now)?;
    let mut machines = Vec::new();
    for host in store.hosts()? {
        let is_local = host.kind == HostKind::Local;
        let running: i64 = if is_local {
            store.conn().query_row(
                "SELECT count(*) FROM sessions s LEFT JOIN runs r ON r.id = s.run_id
                 WHERE s.host_id = ?1 AND s.ended_at IS NULL
                   AND (s.role = 'orchestrator' OR r.ended_at IS NULL)",
                [&host.id],
                |row| row.get(0),
            )?
        } else {
            store.conn().query_row(
                "SELECT count(*) FROM panes
                 WHERE host_id = ?1 AND gone_at IS NULL AND coalesce(agent_status, '') != 'done'",
                [&host.id],
                |row| row.get(0),
            )?
        };
        if let Some(peer) = local
            .peers
            .peers
            .iter()
            .find(|peer| peer.host_id == host.id)
        {
            machines.push(peer_machine(&host, peer));
            continue;
        }
        let slots = capacity.iter().find(|row| row.host_id == host.id);
        let (health, stale_reason, last_seen_at) = if is_local {
            let degraded =
                host.stale_reason.is_some() || !local.failing_pollers.is_empty() || quota_stale;
            (
                if degraded { "degraded" } else { "ok" },
                host.stale_reason.clone(),
                Some(now),
            )
        } else {
            match host.last_seen_at {
                None => (
                    "stale",
                    Some(
                        host.stale_reason
                            .clone()
                            .unwrap_or_else(|| "never_seen".into()),
                    ),
                    None,
                ),
                Some(seen) if seen < now.plus_minutes(-REMOTE_STALE_MINUTES) => (
                    "stale",
                    Some(
                        host.stale_reason
                            .clone()
                            .unwrap_or_else(|| "herdr_link_down".into()),
                    ),
                    Some(seen),
                ),
                Some(seen) => ("ok", None, Some(seen)),
            }
        };
        machines.push(Machine {
            id: host.id,
            label: host.label,
            kind: host.kind.to_string(),
            view_only: !is_local,
            health: health.to_owned(),
            running_agents: Some(running),
            capacity: if is_local {
                slots.and_then(|row| row.lane_capacity)
            } else {
                None
            },
            free_slots: if is_local {
                slots.and_then(|row| row.free)
            } else {
                None
            },
            last_seen_at: at_opt(last_seen_at),
            stale_reason,
            link: None,
            peer_scope: None,
            link_reason: None,
            rev: host.rev,
        });
    }
    Ok(MachinesResponse { machines })
}

/// One host's machine row, for an SSE `host.changed` event.
pub fn machine(
    store: &Store,
    local: &LocalHealth,
    host_id: &str,
    now: Timestamp,
) -> Result<Option<Machine>> {
    Ok(machines(store, local, now)?
        .machines
        .into_iter()
        .find(|machine| machine.id == host_id))
}

// -- quota --------------------------------------------------------------------

fn provider(label: &str) -> &str {
    label.split(':').next().unwrap_or_default()
}

fn window_stale(window: &QuotaRow, now: Timestamp) -> bool {
    window.fetched_at < now.plus_minutes(-QUOTA_STALE_MINUTES)
}

/// Whether the quota feed is stale: a weekly window older than 15 minutes,
/// or no window at all while the feed has run.
fn quota_is_stale(store: &Store, now: Timestamp) -> Result<bool> {
    let windows: Vec<QuotaRow> = list(store.conn(), "kind = 'weekly'", &[])?;
    Ok(windows.iter().any(|window| window_stale(window, now)))
}

/// The dot for one provider, read from its weekly window (`protocol.md` 8).
fn dot(windows: &[QuotaRow], provider_name: &str, now: Timestamp) -> &'static str {
    let Some(weekly) = windows
        .iter()
        .find(|w| provider(&w.label) == provider_name && w.kind.as_deref() == Some("weekly"))
    else {
        return "grey";
    };
    if window_stale(weekly, now) {
        return "grey";
    }
    if weekly
        .percent_remaining
        .is_some_and(|percent| percent <= WEEKLY_FLOOR_PERCENT)
    {
        "red"
    } else if weekly.pace.as_deref() == Some("ahead") {
        "amber"
    } else {
        "green"
    }
}

fn quota_window(windows: &[QuotaRow], window: &QuotaRow, now: Timestamp) -> QuotaWindow {
    QuotaWindow {
        label: window.label.clone(),
        title: window.title.clone().unwrap_or_else(|| window.label.clone()),
        kind: window.kind.clone().unwrap_or_default(),
        percent_remaining: window.percent_remaining,
        resets_at: at_opt(window.resets_at),
        pace: window.pace.clone(),
        reserve: window.reserve,
        burn_multiple: window.burn_multiple,
        elapsed_percent: window.elapsed_percent,
        projected_exhaustion_at: at_opt(window.projected_exhaustion_at),
        fetched_at: at(window.fetched_at),
        stale: window_stale(window, now),
        dot: dot(windows, provider(&window.label), now).to_owned(),
        fetch_error: window.fetch_error.clone(),
    }
}

/// `GET /api/v1/quota`.
pub fn quota(store: &Store, now: Timestamp) -> Result<QuotaResponse> {
    let rows: Vec<QuotaRow> = list(store.conn(), "1 = 1 ORDER BY label", &[])?;
    Ok(QuotaResponse {
        windows: rows
            .iter()
            .map(|window| quota_window(&rows, window, now))
            .collect(),
    })
}

/// One window, for an SSE `quota.changed` event.
pub fn quota_one(store: &Store, label: &str, now: Timestamp) -> Result<Option<QuotaWindow>> {
    let rows: Vec<QuotaRow> = list(store.conn(), "1 = 1 ORDER BY label", &[])?;
    Ok(rows
        .iter()
        .find(|window| window.label == label)
        .map(|window| quota_window(&rows, window, now)))
}

// -- projects -----------------------------------------------------------------

fn project_shape(
    store: &Store,
    project: tiller_core::domain::Project,
    now: Timestamp,
) -> Result<Project> {
    let conn = store.conn();
    // When `reviewed_through_sha` was last set: a pass finish, or a switch
    // to pre-launch that reset it to the head.
    let reviewed_at: Option<Timestamp> = one(
        store,
        "SELECT max(at) FROM events WHERE entity = 'project' AND entity_id = ?1
         AND (kind = 'project.reviewed'
              OR (kind = 'project.staged' AND json_extract(payload, '$.to') = 'pre-launch'))",
        &[&project.id],
    )?;
    let next = review::open_weekly(conn, &project.id)?.and_then(|due| due.due_at);
    let review_overdue = review::due_reviews(conn, Some(&project.id), now)?
        .iter()
        .any(|due| due.overdue);
    let review_open = review::live_pass(conn, &project.id)?.map(|pass| pass.kind.to_string());
    Ok(Project {
        effective_delivery: project.effective_delivery().to_string(),
        retired_at: at_opt(project.retired_at),
        stage: project.stage.to_string(),
        reviewed_through_sha: project.reviewed_through_sha,
        reviewed_at: at_opt(reviewed_at),
        stage_changed_at: at_opt(project.stage_changed_at),
        next_review_due: at_opt(next),
        review_overdue,
        review_open,
        id: project.id,
        name: project.name,
        delivery: project.delivery.to_string(),
        capacity: project.capacity,
        rev: project.rev,
        host_id: None,
        host: None,
        as_of: None,
        link: None,
    })
}

/// `GET /api/v1/projects`: projects not retired.
pub fn projects(store: &Store, now: Timestamp) -> Result<ProjectsResponse> {
    projects_where(store, now, false)
}

/// `GET /api/v1/projects?retired=true`: this host's retired projects, the
/// ones only a delete still touches.
pub fn retired_projects(store: &Store, now: Timestamp) -> Result<ProjectsResponse> {
    projects_where(store, now, true)
}

fn projects_where(store: &Store, now: Timestamp, retired: bool) -> Result<ProjectsResponse> {
    Ok(ProjectsResponse {
        projects: store
            .projects()?
            .into_iter()
            .filter(|project| project.retired_at.is_some() == retired)
            .map(|project| project_shape(store, project, now))
            .collect::<Result<_>>()?,
    })
}

/// `GET /api/v1/contributions`: the PRs published from this host's cards,
/// newest first; `project` narrows to one project.
pub fn contributions(store: &Store, project: Option<&str>) -> Result<ContributionsResponse> {
    let prs: Vec<Pr> = match project {
        Some(project) => list(
            store.conn(),
            "project_id = ?1 ORDER BY linked_at DESC, id DESC",
            &[&project],
        )?,
        None => list(store.conn(), "1 = 1 ORDER BY linked_at DESC, id DESC", &[])?,
    };
    let mut contributions = Vec::new();
    for pr in prs {
        let card: Card = get(store.conn(), "card", &pr.card_id)?;
        contributions.push(Contribution {
            repo: repo_of(&pr.url),
            id: pr.id,
            project_id: pr.project_id,
            card_id: card.id,
            card_display_id: card.display_id,
            card_title: card.title,
            number: pr.number,
            url: pr.url,
            state: pr.state.to_string(),
            linked_at: at(pr.linked_at),
            merged_at: at_opt(pr.merged_at),
        });
    }
    Ok(ContributionsResponse { contributions })
}

// -- card detail ----------------------------------------------------------------

fn run_outcome(outcome: Option<RunOutcome>) -> Option<String> {
    outcome.map(|outcome| {
        match outcome {
            RunOutcome::Landed | RunOutcome::Reported => "passed",
            RunOutcome::Failed | RunOutcome::Cut => "failed",
            RunOutcome::Abandoned | RunOutcome::Relaunched | RunOutcome::Cancelled => "cancelled",
        }
        .to_owned()
    })
}

fn checks(checks: Checks) -> (&'static str, Option<String>) {
    match checks {
        Checks::Success => ("passing", Some("checks green".into())),
        Checks::Failure => ("failing", Some("checks failing".into())),
        Checks::Pending => ("pending", Some("checks running".into())),
        Checks::None => ("pending", Some("no checks reported".into())),
    }
}

fn slug(display_id: &str, title: &str) -> String {
    let mut words = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            words.push(c.to_ascii_lowercase());
        } else if !words.ends_with('-') {
            words.push('-');
        }
    }
    let words: String = words.trim_matches('-').chars().take(28).collect();
    format!("{display_id}-{}", words.trim_end_matches('-'))
}

/// An event as one detail-page line.
fn event_line(event: &Event) -> EventLine {
    let source = match event.actor.as_str() {
        "hook" => "hook",
        _ if event.entity == "pane" => "herdr",
        _ if event.entity == "pr" => "gh",
        _ => "tiller",
    };
    let tone = match event.kind.as_str() {
        "attention.opened" | "session.ended" | "host.degraded" => Some("warn"),
        "guard.breach" | "approval.finished"
            if event.payload.get("state").and_then(Value::as_str) != Some("succeeded") =>
        {
            Some("bad")
        }
        _ => None,
    };
    EventLine {
        at: at(event.at),
        source: source.to_owned(),
        text: format!(
            "{} {}",
            event.kind,
            tiller_core::ids::short(&event.entity_id)
        ),
        tone: tone.map(str::to_owned),
    }
}

/// `GET /api/v1/cards/{id}`; `reference` is a ULID, short form or display id.
pub fn card_detail(store: &Store, reference: &str) -> Result<CardDetail> {
    let card = store.card(reference)?;
    let conn = store.conn();
    let column = match board_row(store, &card.id)? {
        Some(row) => row,
        // Closed more than a week ago: the card is off the board, but its
        // page still shows where it ended.
        None => BoardRow {
            id: card.id.clone(),
            display_id: card.display_id.clone(),
            project_id: card.project_id.clone(),
            host_id: None,
            kind: card.kind.to_string(),
            title: card.title.clone(),
            priority: card.priority,
            column: "done".into(),
            badge: card.outcome.map(|o| o.to_string()).unwrap_or_default(),
            needs_you: false,
            parked_until: None,
            parked_reason: None,
            blocked_by: None,
            closed_at: at_opt(card.closed_at),
            rev: card.rev,
            model: None,
            pr_number: None,
            observed_at: None,
            host: None,
            as_of: None,
            link: None,
        },
    };
    let runs: Vec<Run> = list(conn, "card_id = ?1 ORDER BY started_at, id", &[&card.id])?;
    let active = runs.iter().rev().find(|run| run.ended_at.is_none());
    let newest = runs.last();
    let mut card_runs = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let session: Option<Session> = find(
            conn,
            "run_id = ?1 ORDER BY started_at DESC, id DESC",
            &[&run.id],
        )?;
        let live = session.as_ref().filter(|s| s.ended_at.is_none());
        let effective: Option<String> = match live {
            Some(session) => one(
                store,
                "SELECT effective_activity FROM session_activity WHERE session_id = ?1",
                &[&session.id],
            )?,
            None => None,
        };
        card_runs.push(CardRun {
            id: run.id.clone(),
            number: i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1),
            role: run.role.short_name().to_owned(),
            model: run.model.clone(),
            started_at: at(run.started_at),
            ended_at: at_opt(run.ended_at),
            outcome: run_outcome(run.outcome),
            note: run
                .outcome_note
                .clone()
                .or_else(|| run.last_status_line.clone()),
            session_id: live.map(|s| s.id.clone()),
            host_id: Some(run.host_id.clone()),
            activity: effective,
            activity_observed_at: live.and_then(|s| at_opt(s.activity_observed_at)),
        });
    }
    let prs: Vec<Pr> = list(conn, "card_id = ?1 ORDER BY linked_at, id", &[&card.id])?;
    let card_prs = prs
        .iter()
        .map(|pr| {
            let (state, summary) = checks(pr.checks);
            CardPr {
                id: pr.id.clone(),
                number: pr.number,
                repo: repo_of(&pr.url),
                title: card.title.clone(),
                state: pr.state.to_string(),
                head_sha: pr.head_sha.clone().unwrap_or_default(),
                base: pr.base_ref.clone().unwrap_or_default(),
                checks: state.to_owned(),
                checks_summary: summary,
                diff_summary: None,
            }
        })
        .collect();
    let approvals: Vec<Approval> = list(
        conn,
        "card_id = ?1 AND kind IN ('merge', 'land_direct') ORDER BY requested_at, id",
        &[&card.id],
    )?;
    let card_approvals = approvals
        .into_iter()
        .map(|approval| CardApproval {
            id: approval.id,
            kind: approval.kind.to_string(),
            state: approval.state.to_string(),
            pr_id: approval.pr_id,
            head_sha: approval.head_sha,
            decided_at: at_opt(approval.decided_at),
            decided_by: approval.decided_by,
            // Commit subjects come from the forge, which Tiller does not cache.
            newer_commits: Vec::new(),
        })
        .collect();
    let holds: Vec<Hold> = list(
        conn,
        "card_id = ?1 AND released_at IS NULL ORDER BY set_at, id",
        &[&card.id],
    )?;
    let deps: Vec<CardDep> = list(conn, "card_id = ?1 ORDER BY added_at", &[&card.id])?;
    let mut dep_rows = Vec::new();
    for dep in deps {
        let other: Card = get(conn, "card", &dep.depends_on_card_id)?;
        dep_rows.push(DepRow {
            display_id: other.display_id,
            released_at: at_opt(dep.released_at),
        });
    }
    let items: Vec<Attention> = list(conn, "card_id = ?1 ORDER BY opened_at, id", &[&card.id])?;
    let attention = items
        .iter()
        .map(|item| attention_item(store, item))
        .collect::<Result<_>>()?;
    let artifacts: Vec<Artifact> = list(
        conn,
        "card_id = ?1 AND retired_at IS NULL ORDER BY created_at, id",
        &[&card.id],
    )?;
    let reviews: Vec<Review> = list(conn, "card_id = ?1 ORDER BY recorded_at, id", &[&card.id])?;
    let obligations: Vec<Obligation> =
        list(conn, "card_id = ?1 ORDER BY opened_at, id", &[&card.id])?;
    let events: Vec<Event> = list(
        conn,
        "card_id = ?1 ORDER BY seq DESC LIMIT ?2",
        &[&card.id, &DETAIL_EVENTS],
    )?;
    let freshness = freshness(store, active, &prs)?;
    Ok(CardDetail {
        card: CardFields {
            id: card.id.clone(),
            display_id: card.display_id.clone(),
            project_id: card.project_id.clone(),
            kind: card.kind.to_string(),
            title: card.title.clone(),
            body: card.body.clone(),
            priority: card.priority,
            slug: slug(&card.display_id, &card.title),
            state: match card.state {
                CardState::Open => "open",
                CardState::Closed => "closed",
            }
            .to_owned(),
            outcome: card.outcome.map(|o| o.to_string()),
            model: newest.map(|run| run.model.clone()),
            branch: active.and_then(|run| run.branch.clone()),
            worktree: active.and_then(|run| run.worktree_path.clone()),
            rev: card.rev,
        },
        column,
        holds: holds
            .into_iter()
            .map(|hold| HoldRow {
                id: hold.id,
                until_at: Some(at(hold.until_at)),
                reason: Some(hold.reason),
            })
            .collect(),
        deps: dep_rows,
        attention,
        runs: card_runs,
        prs: card_prs,
        approvals: card_approvals,
        artifacts: artifacts
            .into_iter()
            .map(|artifact| ArtifactRow {
                id: artifact.id,
                kind: artifact.kind.to_string(),
                title: artifact.title,
                url: artifact.url,
                created_at: at(artifact.created_at),
            })
            .collect(),
        reviews: reviews
            .into_iter()
            .map(|review| ReviewRow {
                id: review.id,
                verdict: review.verdict.to_string(),
                at: at(review.recorded_at),
            })
            .collect(),
        obligations: obligations
            .into_iter()
            .map(|obligation| ObligationRow {
                id: obligation.id,
                summary: obligation.what,
                state: obligation.state.to_string(),
            })
            .collect(),
        events: events.iter().map(event_line).collect(),
        freshness,
    })
}

/// `owner/name` from a PR url, or the url itself.
fn repo_of(url: &str) -> String {
    let path = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .splitn(4, '/')
        .collect::<Vec<_>>();
    match path.as_slice() {
        [_, owner, name, ..] => format!("{owner}/{name}"),
        _ => url.to_owned(),
    }
}

/// How fresh each source of the card's facts is: the pane, the hooks, the
/// forge and the worktree.
fn freshness(store: &Store, active: Option<&Run>, prs: &[Pr]) -> Result<Vec<Freshness>> {
    let conn = store.conn();
    let session: Option<Session> = match active {
        Some(run) => find(
            conn,
            "run_id = ?1 AND ended_at IS NULL ORDER BY started_at DESC",
            &[&run.id],
        )?,
        None => None,
    };
    let pane: Option<Pane> = match &session {
        Some(session) => find(
            conn,
            "session_id = ?1 ORDER BY fetched_at DESC",
            &[&session.id],
        )?,
        None => None,
    };
    let hook: Option<(String, Timestamp)> = match &session {
        Some(session) => conn
            .query_row(
                "SELECT event, observed_at FROM observations
                 WHERE session_id = ?1 AND source = 'hook' ORDER BY observed_at DESC, id DESC LIMIT 1",
                [&session.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?,
        None => None,
    };
    let worktree: Option<tiller_core::domain::Worktree> = match active {
        Some(run) => find(
            conn,
            "run_id = ?1 AND gone_at IS NULL ORDER BY fetched_at DESC",
            &[&run.id],
        )?,
        None => None,
    };
    let newest_pr = prs.iter().max_by_key(|pr| pr.fetched_at);
    Ok(vec![
        Freshness {
            source: "Pane (herdr)".into(),
            detail: match (&pane, &session) {
                (Some(pane), _) if pane.gone_at.is_some() => "pane closed".into(),
                (Some(pane), _) => pane
                    .agent_status
                    .clone()
                    .unwrap_or_else(|| "no status".into()),
                (None, Some(_)) => "no pane bound".into(),
                (None, None) => "no live session".into(),
            },
            observed_at: pane.as_ref().map(|pane| at(pane.fetched_at)),
        },
        Freshness {
            source: "Hook events".into(),
            detail: hook
                .as_ref()
                .map_or_else(|| "none yet".into(), |(event, _)| format!("last: {event}")),
            observed_at: hook.map(|(_, at_)| at(at_)),
        },
        Freshness {
            source: "GitHub PR and CI".into(),
            detail: match newest_pr {
                Some(pr) => pr
                    .fetch_error
                    .clone()
                    .map_or_else(|| "polled".into(), |error| format!("poll failed: {error}")),
                None => "no PR".into(),
            },
            observed_at: newest_pr.and_then(|pr| at_opt(pr.fetched_at)),
        },
        Freshness {
            source: "Worktree git status".into(),
            detail: match &worktree {
                Some(tree) => match (tree.dirty, tree.unlanded_commits) {
                    (Some(true), _) => "dirty".into(),
                    (Some(false), Some(0)) => "clean".into(),
                    (Some(false), Some(n)) => format!("{n} unlanded commits"),
                    _ => "unknown".into(),
                },
                None => "none".into(),
            },
            observed_at: worktree.map(|tree| at(tree.fetched_at)),
        },
    ])
}

/// `GET /api/v1/cards/{id}/timeline`: newest first, `TIMELINE_PAGE` at a
/// time, older than `before` when given.
pub fn timeline(store: &Store, reference: &str, before: Option<i64>) -> Result<TimelineResponse> {
    let card = store.card(reference)?;
    let before = before.unwrap_or(i64::MAX);
    let page = TIMELINE_PAGE + 1;
    let mut events: Vec<Event> = list(
        store.conn(),
        "card_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        &[&card.id, &before, &page],
    )?;
    let more = events.len() > usize::try_from(TIMELINE_PAGE).unwrap_or(usize::MAX);
    events.truncate(usize::try_from(TIMELINE_PAGE).unwrap_or(usize::MAX));
    let next_before = if more {
        events.last().map(|e| e.seq)
    } else {
        None
    };
    let entries = events
        .into_iter()
        .map(|event| TimelineEntry {
            seq: event.seq,
            at: at(event.at),
            actor: match event.actor.as_str() {
                "owner" | "board" => "you",
                // The owner on a paired board host (a forwarded mutation).
                actor if actor.starts_with("board@") => "you",
                "hook" => "hook",
                actor if actor.starts_with("session:") => "orchestrator",
                _ => "system",
            }
            .to_owned(),
            text: timeline_text(&event),
        })
        .collect();
    Ok(TimelineResponse {
        entries,
        next_before,
    })
}

fn timeline_text(event: &Event) -> String {
    let words = event.kind.replace(['.', '_'], " ");
    match event.payload.get("reason").and_then(Value::as_str) {
        Some(reason) => format!("{words}: {reason}"),
        None => words,
    }
}

// -- runs ---------------------------------------------------------------------

/// `GET /api/v1/runs/{id}` without its pane tail, which the handler reads
/// from herdr.
/// Where a run's live pane is: its id, and the saved machine that runs it
/// when the session is on a remote host.
pub struct PaneTarget {
    pub pane: String,
    pub machine: Option<String>,
}

pub fn run_detail(store: &Store, reference: &str) -> Result<(RunDetail, Option<PaneTarget>)> {
    let id = tiller_core::state::resolve_id(store.conn(), "runs", "run", reference)?;
    let run: Run = get(store.conn(), "run", &id)?;
    let sessions: Vec<Session> = list(store.conn(), "run_id = ?1", &[&run.id])?;
    let mut lines: Vec<(Timestamp, EventLine)> = Vec::new();
    let events: Vec<Event> = list(
        store.conn(),
        "entity = 'run' AND entity_id = ?1 ORDER BY seq DESC LIMIT ?2",
        &[&run.id, &DETAIL_EVENTS],
    )?;
    for event in &events {
        lines.push((event.at, event_line(event)));
    }
    for session in &sessions {
        let observations: Vec<tiller_core::domain::Observation> = list(
            store.conn(),
            "session_id = ?1 ORDER BY observed_at DESC, id DESC LIMIT ?2",
            &[&session.id, &DETAIL_EVENTS],
        )?;
        for observation in observations {
            let text = match observation.verdict {
                Some(verdict) => format!("{} {verdict}", observation.event),
                None => observation.event.clone(),
            };
            lines.push((
                observation.observed_at,
                EventLine {
                    at: at(observation.observed_at),
                    source: observation.source.to_string(),
                    text,
                    tone: None,
                },
            ));
        }
    }
    lines.sort_by_key(|line| std::cmp::Reverse(line.0));
    lines.truncate(usize::try_from(DETAIL_EVENTS).unwrap_or(usize::MAX));
    let live = sessions.iter().find(|session| session.ended_at.is_none());
    let pane = match live.and_then(|session| session.pane_id.clone().map(|pane| (session, pane))) {
        Some((session, pane)) => {
            let host: tiller_core::domain::Host = get(store.conn(), "host", &session.host_id)?;
            let machine = if host.kind == HostKind::Remote {
                // A remote pane is read through its machine, or not at all.
                host.herdr_machine
            } else {
                None
            };
            (host.kind == HostKind::Local || machine.is_some())
                .then_some(PaneTarget { pane, machine })
        }
        None => None,
    };
    Ok((
        RunDetail {
            id: run.id,
            card_id: run.card_id,
            branch: run.branch,
            worktree: run.worktree_path,
            started_at: at(run.started_at),
            ended_at: at_opt(run.ended_at),
            events: lines.into_iter().map(|(_, line)| line).collect(),
            pane_tail: Vec::new(),
        },
        pane,
    ))
}

// -- signals ------------------------------------------------------------------

/// `GET /api/v1/signals`.
pub fn signals(store: &Store, now: Timestamp) -> Result<SignalsResponse> {
    let signals = store.signals(&lisbon_weeks(now, SIGNAL_WEEKS), now)?;
    Ok(SignalsResponse {
        weeks: signals
            .weeks
            .into_iter()
            .map(|week| SignalsWeek {
                start: at(week.start),
                end: at(week.end),
                landed: week.landed,
                reported: week.reported,
                merged_prs: week.merged_prs,
                needed: week.needed,
                answered: week.answered,
                median_answer_minutes: week.median_answer_minutes,
                interventions: SignalsInterventions {
                    correction: week.interventions.correction,
                    unplanned_escalation: week.interventions.unplanned_escalation,
                    rework: week.interventions.rework,
                    planned: week.interventions.planned,
                },
                reverts: week.reverts,
                escaped: week.escaped,
                rework: week.rework,
                lost_work: week.lost_work,
                duplicate_dispatches: week.duplicate_dispatches,
                unauthorized: week.unauthorized,
            })
            .collect(),
        by_project: signals
            .by_project
            .into_iter()
            .map(|project| SignalsProject {
                project_id: project.project_id,
                accepted: project.accepted,
                needed: project.needed,
                avoidable: project.avoidable,
                escaped: project.escaped,
                answered: project.answered,
                median_answer_minutes: project.median_answer_minutes,
            })
            .collect(),
        interventions: signals
            .interventions
            .into_iter()
            .map(|item| SignalsIntervention {
                id: item.id,
                source: match item.source {
                    InterventionSource::Attention => "attention",
                    InterventionSource::Note => "note",
                }
                .to_owned(),
                card_id: item.card_id,
                display_id: item.display_id,
                project_id: item.project_id,
                at: at(item.at),
                summary: item.summary,
                classified: item.classified.to_string(),
                kind: item.kind.to_string(),
                relabeled: item.relabeled,
            })
            .collect(),
        machines_live: signals.machines_live,
        machines_total: signals.machines_total,
    })
}

// -- health -------------------------------------------------------------------

pub fn poller_health(records: Vec<PollerRecord>) -> Vec<PollerHealth> {
    records
        .into_iter()
        .map(|record| PollerHealth {
            source: record.source,
            last_attempt_at: at_opt(record.last_attempt_at),
            last_success_at: at_opt(record.last_success_at),
            consecutive_failures: record.consecutive_failures,
            last_error: record.last_error,
        })
        .collect()
}

// -- federation -----------------------------------------------------------------

/// Whose rows these are, once a peer is paired (`protocol.md` 7.3). This
/// host's rows get its label and id. A peer's rows also get the peer's
/// `as_of` and `link`, and every host id in them becomes the peer's `hosts`
/// row id here, the id its Machines row carries and the board filters by.
struct Owner {
    host: String,
    host_id: String,
    /// A peer's `as_of` and link state; `None` for this host.
    peer: Option<(Option<String>, LinkState)>,
}

impl Owner {
    fn local(store: &Store) -> Result<Self> {
        let host = store.this_host()?;
        Ok(Self {
            host: host.label,
            host_id: host.id,
            peer: None,
        })
    }

    fn peer(view: &PeerView) -> Self {
        Self {
            host: view.label.clone(),
            host_id: view.host_id.clone(),
            peer: Some((view.as_of.clone(), view.link)),
        }
    }

    /// `host`, `as_of` and `link`.
    fn tag(&self) -> (Option<String>, Option<String>, Option<LinkState>) {
        match &self.peer {
            None => (Some(self.host.clone()), None, None),
            Some((as_of, link)) => (Some(self.host.clone()), as_of.clone(), Some(*link)),
        }
    }

    fn board(&self, response: &mut BoardResponse) {
        for row in &mut response.rows {
            self.board_row(row);
        }
        for row in &mut response.asked {
            (row.host, row.as_of, row.link) = self.tag();
        }
    }

    fn board_row(&self, row: &mut BoardRow) {
        (row.host, row.as_of, row.link) = self.tag();
        if self.peer.is_some() {
            let (column, needs_you) = peer_column(row);
            (row.column, row.needs_you) = (column.to_owned(), needs_you);
            row.host_id = Some(self.host_id.clone());
        }
    }

    fn attention(&self, response: &mut AttentionResponse) {
        for item in &mut response.items {
            (item.host, item.as_of, item.link) = self.tag();
            item.host_id = Some(self.host_id.clone());
        }
        for prompt in &mut response.permission_prompts {
            (prompt.host, prompt.as_of, prompt.link) = self.tag();
        }
    }

    fn agents(&self, response: &mut AgentsResponse) {
        for row in &mut response.orchestrators {
            self.agent_row(row);
        }
    }

    fn agent_row(&self, row: &mut AgentRow) {
        (row.host, row.as_of, row.link) = self.tag();
        if self.peer.is_some() {
            row.host_id.clone_from(&self.host_id);
        }
        for worker in &mut row.workers {
            self.agent_row(worker);
        }
    }

    fn projects(&self, response: &mut ProjectsResponse) {
        for project in &mut response.projects {
            (project.host, project.as_of, project.link) = self.tag();
            project.host_id = Some(self.host_id.clone());
        }
    }

    /// Tags an SSE change's `entity_state` where its shape carries a host.
    fn entity_state(&self, entity: &str, state: &mut Value) {
        if !matches!(entity, "attention" | "project") {
            return;
        }
        let Some(fields) = state.as_object_mut() else {
            return;
        };
        let (host, as_of, link) = self.tag();
        fields.insert("host_id".into(), json!(self.host_id));
        fields.insert("host".into(), json!(host));
        if let Some(as_of) = as_of {
            fields.insert("as_of".into(), json!(as_of));
        }
        if let Some(link) = link {
            fields.insert("link".into(), json!(link));
        }
    }
}

/// Which rows a federated read returns for its `?host=`.
enum Scope<'a> {
    /// No `host`: this host's rows and every peer's.
    Everyone,
    /// A `host` that names no peer: this host's rows, filtered as before.
    Local,
    /// A `host` that names a peer: that peer's cached rows only.
    Peer(&'a PeerView),
}

fn scope<'a>(peers: &'a Federation, host: Option<&str>) -> Scope<'a> {
    match host {
        None => Scope::Everyone,
        Some(host) => peers.peer(host).map_or(Scope::Local, Scope::Peer),
    }
}

/// Where a peer's row sits on this host's board, and whether it waits on the
/// owner. A peer from before migration 11 still sends a Needs you column:
/// place the card where this host would. A merge approval waits in Landing,
/// a card that has run is in Working, any other is Queued.
fn peer_column(row: &BoardRow) -> (&str, bool) {
    if row.column != "needs_you" {
        return (&row.column, row.needs_you);
    }
    let column = if row.kind == "ship" && row.badge == "merge" {
        "landing"
    } else if row.host_id.is_some() {
        "working"
    } else {
        "queued"
    };
    (column, true)
}

/// A peer's cached board rows, filtered and tagged.
pub fn peer_board(view: &PeerView, project: Option<&str>, filter: Filter) -> BoardResponse {
    let in_project = |id: &str| project.is_none_or(|project| project == id);
    let mut response = BoardResponse {
        rows: Vec::new(),
        asked: Vec::new(),
    };
    if let Some(queries) = &view.queries {
        response.rows = (queries.board.rows.iter())
            .filter(|row| {
                let (column, needs_you) = peer_column(row);
                filter.keeps(column, needs_you) && in_project(&row.project_id)
            })
            .cloned()
            .collect();
        response.asked = (queries.board.asked.iter())
            .filter(|row| in_project(&row.project_id))
            .cloned()
            .collect();
    }
    Owner::peer(view).board(&mut response);
    response
}

fn peer_attention(view: &PeerView) -> AttentionResponse {
    let mut response = view.queries.as_ref().map_or_else(
        || AttentionResponse {
            items: Vec::new(),
            permission_prompts: Vec::new(),
        },
        |queries| queries.attention.clone(),
    );
    Owner::peer(view).attention(&mut response);
    response
}

fn peer_agents(view: &PeerView) -> AgentsResponse {
    let mut response = view.queries.as_ref().map_or_else(
        || AgentsResponse {
            orchestrators: Vec::new(),
        },
        |queries| queries.agents.clone(),
    );
    Owner::peer(view).agents(&mut response);
    response
}

fn peer_projects(view: &PeerView) -> ProjectsResponse {
    let mut response = view.queries.as_ref().map_or_else(
        || ProjectsResponse {
            projects: Vec::new(),
        },
        |queries| queries.projects.clone(),
    );
    Owner::peer(view).projects(&mut response);
    response
}

/// `GET /api/v1/board` across hosts (`protocol.md` 7.3).
pub fn federated_board(
    store: &Store,
    peers: &Federation,
    project: Option<&str>,
    filter: Filter,
    host: Option<&str>,
) -> Result<BoardResponse> {
    let scope = scope(peers, host);
    if let Scope::Peer(view) = scope {
        return Ok(peer_board(view, project, filter));
    }
    let mut response = board(store, project, filter, host)?;
    if peers.peers.is_empty() {
        return Ok(response);
    }
    Owner::local(store)?.board(&mut response);
    if let Scope::Everyone = scope {
        for view in &peers.peers {
            let theirs = peer_board(view, project, filter);
            response.rows.extend(theirs.rows);
            response.asked.extend(theirs.asked);
        }
    }
    Ok(response)
}

/// `GET /api/v1/attention?state=open` across hosts.
pub fn federated_attention(
    store: &Store,
    peers: &Federation,
    host: Option<&str>,
) -> Result<AttentionResponse> {
    let scope = scope(peers, host);
    if let Scope::Peer(view) = scope {
        return Ok(peer_attention(view));
    }
    let mut response = attention(store)?;
    if peers.peers.is_empty() {
        return Ok(response);
    }
    Owner::local(store)?.attention(&mut response);
    if let Scope::Everyone = scope {
        for view in &peers.peers {
            let theirs = peer_attention(view);
            response.items.extend(theirs.items);
            response
                .permission_prompts
                .extend(theirs.permission_prompts);
        }
    }
    Ok(response)
}

/// `GET /api/v1/agents` across hosts.
pub fn federated_agents(
    store: &Store,
    peers: &Federation,
    host: Option<&str>,
) -> Result<AgentsResponse> {
    let scope = scope(peers, host);
    if let Scope::Peer(view) = scope {
        return Ok(peer_agents(view));
    }
    let mut response = agents(store, host)?;
    if peers.peers.is_empty() {
        return Ok(response);
    }
    Owner::local(store)?.agents(&mut response);
    if let Scope::Everyone = scope {
        for view in &peers.peers {
            response
                .orchestrators
                .extend(peer_agents(view).orchestrators);
        }
    }
    Ok(response)
}

/// `GET /api/v1/projects` across hosts.
pub fn federated_projects(
    store: &Store,
    peers: &Federation,
    host: Option<&str>,
    now: Timestamp,
) -> Result<ProjectsResponse> {
    let scope = scope(peers, host);
    if let Scope::Peer(view) = scope {
        return Ok(peer_projects(view));
    }
    let mut response = projects(store, now)?;
    if peers.peers.is_empty() {
        return Ok(response);
    }
    Owner::local(store)?.projects(&mut response);
    if let Scope::Everyone = scope {
        for view in &peers.peers {
            response.projects.extend(peer_projects(view).projects);
        }
    }
    Ok(response)
}

/// A paired peer's Machines row (`protocol.md` section 9): its `hosts` row
/// here, with the link's state and the counts from the peer's own row.
fn peer_machine(host: &tiller_core::domain::Host, view: &PeerView) -> Machine {
    let cached = (view.queries.as_ref()).and_then(|queries| queries.machines.machines.first());
    let health = match (view.link, cached) {
        (LinkState::Live, Some(row)) => row.health.clone(),
        (LinkState::Live, None) => "ok".to_owned(),
        _ => "stale".to_owned(),
    };
    let stale_reason = match view.link {
        LinkState::Never => Some("never_seen".to_owned()),
        _ => view.stale_reason().map(str::to_owned),
    };
    Machine {
        id: host.id.clone(),
        label: host.label.clone(),
        kind: host.kind.to_string(),
        view_only: view.scope == PeerScope::Read,
        health,
        running_agents: cached.and_then(|row| row.running_agents),
        capacity: cached.and_then(|row| row.capacity),
        free_slots: cached.and_then(|row| row.free_slots),
        last_seen_at: at_opt(view.last_frame_at.max(host.last_seen_at)),
        stale_reason,
        link: Some(view.link),
        peer_scope: Some(view.scope.to_string()),
        link_reason: view.link_reason.clone(),
        rev: host.rev,
    }
}

fn snapshot_peer(view: &PeerView) -> SnapshotPeer {
    SnapshotPeer {
        host: view.label.clone(),
        host_id: view.host_id.clone(),
        link: view.link,
        as_of: view.as_of.clone(),
        server_time_offset_ms: view.offset_ms,
        scope: view.scope.to_string(),
        last_event_seq: view.last_event_seq,
    }
}

/// A peer's cached rows by snapshot path, tagged as the federated reads
/// return them: the `queries` of an SSE `peer_reset`.
pub fn peer_queries(view: &PeerView) -> Value {
    json!({
        SNAPSHOT_PATHS[0]: peer_board(view, None, Filter::All),
        SNAPSHOT_PATHS[1]: peer_attention(view),
        SNAPSHOT_PATHS[2]: peer_agents(view),
        SNAPSHOT_PATHS[5]: peer_projects(view),
    })
}

/// A peer's change as the board gets it (`event: peer`): its card and
/// entity tagged as the peer's. A host entity points at the peer's Machines
/// row here with no state, so the board refetches the federated row.
pub fn peer_change(view: &PeerView, mut change: ChangeEvent) -> ChangeEvent {
    let owner = Owner::peer(view);
    if let Some(card) = &mut change.card {
        owner.board_row(card);
    }
    if change.entity == "host" {
        if change.entity_id == view.peer_host_id {
            change.entity_id.clone_from(&view.host_id);
        }
        change.entity_state = Value::Null;
    } else {
        owner.entity_state(&change.entity, &mut change.entity_state);
    }
    change
}

// -- snapshot -----------------------------------------------------------------

/// The paths the boot snapshot carries, as the board's query option
/// factories key them.
pub const SNAPSHOT_PATHS: [&str; 6] = [
    "/api/v1/board?filter=all",
    "/api/v1/attention?state=open",
    "/api/v1/agents",
    "/api/v1/machines",
    "/api/v1/quota",
    "/api/v1/projects",
];

/// The staleness thresholds the board computes ages with.
pub fn thresholds() -> Value {
    let defaults = tiller_reconcile::thresholds::Thresholds::default();
    json!({
        "worker_stale_minutes": defaults.worker_stale,
        "orchestrator_stale_minutes": defaults.orchestrator_stale,
        "quota_stale_minutes": QUOTA_STALE_MINUTES,
        "remote_stale_minutes": REMOTE_STALE_MINUTES,
    })
}

/// The boot snapshot (`protocol.md` 7.6). Every read runs in one SQLite read
/// transaction with `last_event_seq`, so the client's first stream request
/// from that seq neither misses nor repeats an event.
pub fn snapshot(store: &Store, local: &LocalHealth, now: Timestamp) -> Result<Snapshot> {
    store.conn().execute_batch("BEGIN")?;
    let built = (|| -> Result<Snapshot> {
        let last_event_seq = store.last_seq()?;
        let peers = &local.peers;
        let queries = json!({
            SNAPSHOT_PATHS[0]: federated_board(store, peers, None, Filter::All, None)?,
            SNAPSHOT_PATHS[1]: federated_attention(store, peers, None)?,
            SNAPSHOT_PATHS[2]: federated_agents(store, peers, None)?,
            SNAPSHOT_PATHS[3]: machines(store, local, now)?,
            SNAPSHOT_PATHS[4]: quota(store, now)?,
            SNAPSHOT_PATHS[5]: federated_projects(store, peers, None, now)?,
        });
        Ok(Snapshot {
            v: 1,
            server_time: at(now),
            last_event_seq,
            thresholds: thresholds(),
            queries,
            peers: peers.peers.iter().map(snapshot_peer).collect(),
        })
    })();
    let ended = store.conn().execute_batch("COMMIT");
    let snapshot = built?;
    ended?;
    Ok(snapshot)
}

// -- change events --------------------------------------------------------------

/// One `events` row as an SSE change message: the entity's API shape as it is
/// now, and the card's recomputed board row.
///
/// A row whose entity or card cannot be read still goes out, with those
/// fields null: the board refetches what it names, and one bad row never
/// stalls the stream behind it.
pub fn change(store: &Store, local: &LocalHealth, event: &Event, now: Timestamp) -> ChangeEvent {
    let entity_state = entity_state(store, local, event, now).unwrap_or_else(|error| {
        tracing::warn!(seq = event.seq, %error, "change sent without its entity");
        Value::Null
    });
    let mut card = match &event.card_id {
        // A retired project's card stays off the board: the board upserts
        // any row a change carries.
        Some(card_id) => listed_row(store, card_id).unwrap_or_else(|error| {
            tracing::warn!(seq = event.seq, %error, "change sent without its card");
            None
        }),
        None => None,
    };
    let mut entity_state = entity_state;
    if !local.peers.peers.is_empty()
        && let Ok(owner) = Owner::local(store)
    {
        if let Some(card) = &mut card {
            owner.board_row(card);
        }
        owner.entity_state(&event.entity, &mut entity_state);
    }
    ChangeEvent {
        seq: event.seq,
        at: at(event.at),
        kind: event.kind.clone(),
        txn: event.txn.clone(),
        entity: event.entity.clone(),
        entity_id: event.entity_id.clone(),
        rev_after: event.rev_after,
        card_id: event.card_id.clone(),
        entity_state,
        card,
    }
}

fn entity_state(
    store: &Store,
    local: &LocalHealth,
    event: &Event,
    now: Timestamp,
) -> Result<Value> {
    let id = event.entity_id.as_str();
    let conn = store.conn();
    let value = match event.entity.as_str() {
        "attention" => match find::<Attention>(conn, "id = ?1", &[&id])? {
            Some(item) => serde_json::to_value(attention_item(store, &item)?)?,
            None => Value::Null,
        },
        "host" => match machine(store, local, id, now)? {
            Some(machine) => serde_json::to_value(machine)?,
            None => Value::Null,
        },
        "quota" => match quota_one(store, id, now)? {
            Some(window) => serde_json::to_value(window)?,
            None => Value::Null,
        },
        "project" => match find::<tiller_core::domain::Project>(conn, "id = ?1", &[&id])? {
            Some(project) => serde_json::to_value(project_shape(store, project, now)?)?,
            None => Value::Null,
        },
        "outbox" => {
            let state: Option<String> =
                one(store, "SELECT state FROM outbox WHERE id = ?1", &[&id])?;
            state.map_or(Value::Null, |state| json!({"id": id, "state": state}))
        }
        "approval" => {
            let row: Option<(String, i64)> = conn
                .query_row(
                    "SELECT state, rev FROM approvals WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            row.map_or(
                Value::Null,
                |(state, rev)| json!({"id": id, "state": state, "rev": rev}),
            )
        }
        _ => Value::Null,
    };
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_display_id_then_title_words() {
        assert_eq!(
            slug("alpha-42", "Undo: coalesce strokes!"),
            "alpha-42-undo-coalesce-strokes"
        );
    }

    /// A peer from before migration 11 sends a Needs you column and no flag;
    /// its card lands where this host would put it, flagged.
    #[test]
    fn an_old_peer_needs_you_row_keeps_its_progress() {
        let row = |kind: &str, badge: &str, host: Option<&str>| -> BoardRow {
            serde_json::from_value(serde_json::json!({
                "id": "c1", "display_id": "alpha-1", "project_id": "alpha",
                "host_id": host, "kind": kind, "title": "t", "priority": 2,
                "column": "needs_you", "badge": badge, "rev": 1,
            }))
            .unwrap()
        };
        let merge = row("ship", "merge", Some("h1"));
        let mid_run = row("ship", "decision", Some("h1"));
        let never_ran = row("question", "decision", None);
        assert_eq!(peer_column(&merge), ("landing", true));
        assert_eq!(peer_column(&mid_run), ("working", true));
        assert_eq!(peer_column(&never_ran), ("queued", true));
        assert!(!Filter::Active.keeps(peer_column(&never_ran).0, true));
        assert!(Filter::Needs.keeps(peer_column(&never_ran).0, true));
    }

    #[test]
    fn repo_comes_from_the_pr_url() {
        assert_eq!(
            repo_of("https://github.com/example/app/pull/7"),
            "example/app"
        );
    }
}
