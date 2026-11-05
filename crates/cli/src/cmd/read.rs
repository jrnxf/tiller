//! Reads: the boot snapshot, the board, agents, machines, quota, events and
//! the intake statistics (`protocol.md` sections 2.5, 6.4 and 8 to 9). None
//! of them writes.

use std::io::Write;
use std::time::Duration;

use clap::Args;
use jiff::Timestamp as Jiff;
use serde::Serialize;
use serde_json::{Value, json};
use tiller_core::domain::{
    Ask, Card, Column, Host, HostKind, Observation, Pane, Project, QuotaWindow, Role, Run, Session,
    Size,
};
use tiller_core::quota_rule::{self, Hold, Verdict};
use tiller_core::state::{find, get, list};
use tiller_core::{Store, Timestamp};

use super::card::herdr_name;
use crate::app::{App, Done, Failure, Outcome};
use crate::lisbon;

/// The switching rule for the Stop hook (`protocol.md` 2.5; CP2 C14).
pub const STOP_HOOK_MIN_STOPS: i64 = 200;
pub const STOP_HOOK_MAX_RATE: f64 = 0.02;
const QUOTA_STALE_MINUTES: i64 = 15;
const WEEKLY_FLOOR_PERCENT: f64 = 3.0;
const REMOTE_STALE_MINUTES: i64 = 2;
/// A `would_block` stop is a false block when the session settles it this fast.
const FALSE_BLOCK_WINDOW_MINUTES: i64 = 2;

#[derive(Args, Debug)]
pub struct BoardArgs {
    #[arg(long)]
    pub project: Option<String>,
    /// all, active (working or landing) or needs (waits on you, or asked)
    #[arg(long, default_value = "all")]
    pub filter: Filter,
    /// Only this host's label
    #[arg(long)]
    pub host: Option<String>,
    /// Print the one-time login link that signs the browser in as the owner
    #[arg(long)]
    pub login: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Active,
    Needs,
}

#[derive(Args, Debug)]
pub struct AgentsArgs {
    #[arg(long)]
    pub host: Option<String>,
}

#[derive(Args, Debug)]
pub struct MachinesArgs {
    /// This host's own row only, which a peer polls
    #[arg(long = "local")]
    pub only_local: bool,
}

#[derive(Args, Debug)]
pub struct EventsArgs {
    /// Only events after this seq
    #[arg(long, default_value_t = 0)]
    pub since: i64,
    #[arg(long)]
    pub card: Option<String>,
    #[arg(long, default_value_t = 100)]
    pub limit: i64,
    /// Keep printing new events as JSON lines until interrupted
    #[arg(long)]
    pub follow: bool,
}

#[derive(Args, Debug)]
pub struct IntakeStatsArgs {
    #[arg(long, default_value_t = 7)]
    pub days: i64,
}

pub fn version() -> Outcome {
    Outcome::read(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "schema": tiller_core::migrate::LATEST,
    }))
}

// -- board ------------------------------------------------------------------

fn board_data(store: &Store, args: &BoardArgs) -> Result<Value, Failure> {
    let columns = store.card_columns(args.project.as_deref())?;
    let mut cards = Vec::new();
    for row in columns {
        let keep = match args.filter {
            Filter::All => true,
            Filter::Active => matches!(row.column, Column::Working | Column::Landing),
            Filter::Needs => row.needs_you || row.column == Column::Asked,
        };
        if !keep {
            continue;
        }
        let card: Card = get(store.conn(), "card", &row.id)?;
        if let Some(host) = &args.host
            && !card_on_host(store, &card, host)?
        {
            continue;
        }
        cards.push(json!({
            "id": card.id,
            "display_id": card.display_id,
            "project_id": card.project_id,
            "kind": card.kind,
            "title": card.title,
            "priority": card.priority,
            "state": card.state,
            "outcome": card.outcome,
            "column": row.column,
            "badge": row.badge,
            "needs_you": row.needs_you,
            "parked_until": row.parked_until,
            "parked_reason": row.parked_reason,
            "blocked_by": row.blocked_by,
            "closed_at": row.closed_at,
            "rev": card.rev,
        }));
    }
    let asked: Vec<Value> = store
        .asked(args.project.as_deref())?
        .into_iter()
        .map(|row| json!({"ask": row.ask, "age_seconds": row.age_seconds.round()}))
        .collect();
    Ok(json!({"cards": cards, "asked": asked}))
}

/// A card is on a host when it has a run there; a card with no run is on none.
fn card_on_host(store: &Store, card: &Card, label: &str) -> Result<bool, Failure> {
    let hit: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM runs r JOIN hosts h ON h.id = r.host_id
             WHERE r.card_id = ?1 AND h.label = ?2",
            rusqlite::params![card.id, label],
            |row| row.get(0),
        )
        .map_err(tiller_core::Error::from)?;
    Ok(hit > 0)
}

pub fn board(app: &App, args: BoardArgs) -> Done {
    if args.login {
        return login(app);
    }
    let store = app.read()?;
    Ok(Outcome::read(board_data(&store, &args)?))
}

/// `tiller board --login`: a one-time link that signs this browser in as the
/// owner (`protocol.md` 7.1). It needs the running daemon, whose port it
/// names; a used link stops working, so each call mints a new one.
fn login(app: &App) -> Done {
    app.require_owner("tiller board --login")?;
    let running = tiller_agents::land::DaemonLock::held(&app.home)
        .then(|| tiller_daemon::running(&app.home))
        .flatten();
    let Some((_, port)) = running else {
        return Err(Failure::unavailable(
            "tiller board --login mints the owner cookie link, and that needs the daemon, which is not running",
        )
        .hint("start the daemon with: tiller daemon"));
    };
    let token = tiller_daemon::auth::mint_login(&app.home, tiller_core::Timestamp::now())
        .map_err(|error| Failure::from(tiller_core::Error::Internal(error.to_string())))?;
    Ok(Outcome::read(json!({
        "url": format!("http://127.0.0.1:{port}/login/{token}"),
        "expires_in_minutes": tiller_daemon::auth::LOGIN_MINUTES,
    })))
}

// -- agents -----------------------------------------------------------------

fn session_row(store: &Store, session: &Session) -> Result<Value, Failure> {
    let conn = store.conn();
    let effective: Option<String> = conn
        .query_row(
            "SELECT effective_activity FROM session_activity WHERE session_id = ?1",
            [&session.id],
            |row| row.get(0),
        )
        .map_err(tiller_core::Error::from)?;
    let pane: Option<Pane> = find(conn, "session_id = ?1 AND gone_at IS NULL", &[&session.id])?;
    let run: Option<Run> = match &session.run_id {
        Some(run_id) => Some(get(conn, "run", run_id)?),
        None => None,
    };
    let card: Option<Card> = match &run {
        Some(run) => Some(get(conn, "card", &run.card_id)?),
        None => None,
    };
    let name = match (&run, &card) {
        (Some(run), Some(card)) => Some(herdr_name(&card.display_id, run)),
        _ => pane.as_ref().and_then(|pane| pane.agent_name.clone()),
    };
    Ok(json!({
        "session_id": session.id,
        "role": session.role,
        "project_id": session.project_id,
        "card": card.as_ref().map(|card| &card.display_id),
        "model": run.as_ref().map(|run| &run.model),
        "effort": run.as_ref().map(|run| run.effort),
        "activity": session.activity,
        "effective_activity": effective,
        "activity_observed_at": session.activity_observed_at,
        "last_status_line": run.as_ref().and_then(|run| run.last_status_line.clone()),
        "last_status_at": run.as_ref().and_then(|run| run.last_status_at),
        "herdr_name": name,
        "pane_id": session.pane_id,
        "workspace_label": pane.and_then(|pane| pane.workspace_label),
    }))
}

fn agents_data(store: &Store, host: Option<&str>) -> Result<Value, Failure> {
    let host_id: Option<String> = match host {
        Some(label) => Some(
            find::<Host>(store.conn(), "label = ?1", &[&label])?
                .ok_or_else(|| {
                    Failure::from(tiller_core::Error::NotFound {
                        entity: "host",
                        id: label.to_owned(),
                        candidates: Vec::new(),
                    })
                })?
                .id,
        ),
        None => None,
    };
    // Every live session first, so a worker is listed under its supervisor
    // even when the supervisor sits on another host, and under no
    // orchestrator when the supervisor has ended.
    let sessions: Vec<Session> = list(
        store.conn(),
        "ended_at IS NULL ORDER BY project_id, started_at, id",
        &[],
    )?;
    let on_host = |session: &Session| host_id.as_ref().is_none_or(|id| &session.host_id == id);
    let mut supervised: Vec<(&Session, Option<String>)> = Vec::new();
    for worker in sessions.iter().filter(|s| s.role != Role::Orchestrator) {
        let supervisor = match &worker.run_id {
            Some(run_id) => Some(get::<Run>(store.conn(), "run", run_id)?.supervisor_session_id),
            None => None,
        };
        supervised.push((worker, supervisor));
    }
    let mut groups = Vec::new();
    let orchestrators: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.role == Role::Orchestrator)
        .collect();
    for orchestrator in &orchestrators {
        let mut workers = Vec::new();
        for (worker, supervisor) in &supervised {
            if supervisor.as_deref() == Some(orchestrator.id.as_str()) && on_host(worker) {
                workers.push(session_row(store, worker)?);
            }
        }
        if on_host(orchestrator) || !workers.is_empty() {
            groups.push(json!({
                "orchestrator": session_row(store, orchestrator)?,
                "workers": workers,
            }));
        }
    }
    let mut orphans = Vec::new();
    for (worker, supervisor) in &supervised {
        let live = orchestrators
            .iter()
            .any(|o| supervisor.as_deref() == Some(o.id.as_str()));
        if !live && on_host(worker) {
            orphans.push(session_row(store, worker)?);
        }
    }
    if !orphans.is_empty() {
        groups.push(json!({"orchestrator": null, "workers": orphans}));
    }
    Ok(json!({"agents": groups}))
}

pub fn agents(app: &App, args: AgentsArgs) -> Done {
    let store = app.read()?;
    Ok(Outcome::read(agents_data(&store, args.host.as_deref())?))
}

// -- machines ---------------------------------------------------------------

fn machines_data(store: &Store, local_only: bool) -> Result<Value, Failure> {
    let now = Jiff::now();
    let capacity = store.capacity()?;
    let mut rows = Vec::new();
    for host in store.hosts()? {
        let local = host.kind == HostKind::Local;
        if local_only && !local {
            continue;
        }
        let running: i64 = if local {
            store.conn().query_row(
                "SELECT count(*) FROM sessions s LEFT JOIN runs r ON r.id = s.run_id
                 WHERE s.host_id = ?1 AND s.ended_at IS NULL
                   AND (s.role = 'orchestrator' OR r.ended_at IS NULL)",
                [&host.id],
                |row| row.get(0),
            )
        } else {
            store.conn().query_row(
                "SELECT count(*) FROM panes
                 WHERE host_id = ?1 AND gone_at IS NULL AND coalesce(agent_status, '') != 'done'",
                [&host.id],
                |row| row.get(0),
            )
        }
        .map_err(tiller_core::Error::from)?;
        let slots = capacity.iter().find(|row| row.host_id == host.id);
        let (health, stale_reason) = if local {
            let health = if host.stale_reason.is_some() {
                "degraded"
            } else {
                "ok"
            };
            (health, host.stale_reason.clone())
        } else {
            match host.last_seen_at {
                None => (
                    "stale",
                    Some(
                        host.stale_reason
                            .clone()
                            .unwrap_or_else(|| "never_seen".into()),
                    ),
                ),
                Some(seen) if seen.as_jiff() < lisbon::plus_minutes(now, -REMOTE_STALE_MINUTES) => {
                    ("stale", host.stale_reason.clone())
                }
                Some(_) => ("ok", None),
            }
        };
        rows.push(json!({
            "id": host.id,
            "label": host.label,
            "kind": host.kind,
            "view_only": !local,
            "health": health,
            "running_agents": running,
            "capacity": slots.and_then(|row| row.lane_capacity),
            "free_slots": slots.and_then(|row| row.free),
            "last_seen_at": host.last_seen_at,
            "stale_reason": stale_reason,
        }));
    }
    Ok(json!({"machines": rows}))
}

pub fn machines(app: &App, args: MachinesArgs) -> Done {
    let store = app.read()?;
    Ok(Outcome::read(machines_data(&store, args.only_local)?))
}

// -- quota ------------------------------------------------------------------

#[derive(Args, Debug)]
pub struct QuotaArgs {
    /// Size of the card to start (S, M or L); sets how long the window must
    /// last. Unsized cards: M.
    #[arg(long, default_value = "M")]
    pub size: Size,
}

/// The dispatch answer for one weekly window (`quota_rule`): whether to start
/// the work, and the one line the orchestrator carries in its note.
fn dispatch_of(window: &QuotaWindow, size: Size, now: Jiff) -> Value {
    let left = window
        .percent_remaining
        .map_or_else(|| "unknown".into(), |p| format!("{p:.0}%"));
    let at = |ts: Option<tiller_core::Timestamp>| {
        ts.map_or_else(|| "unknown".into(), |ts| lisbon::display(ts.as_jiff()))
    };
    let (decision, note) = match quota_rule::verdict(
        window,
        size,
        tiller_core::Timestamp::from_jiff(now),
    ) {
        Verdict::Go => ("go", None),
        Verdict::GoAheadOfPace => (
            "go",
            Some(format!(
                "Started ahead of pace: {left} of the week left, projected empty {} before the reset {}.",
                at(window.projected_exhaustion_at),
                at(window.resets_at),
            )),
        ),
        Verdict::Hold(Hold::LowRemaining) => (
            "hold",
            Some(format!(
                "Holding: {left} of the week left, under {}%.",
                quota_rule::HOLD_UNDER_PERCENT
            )),
        ),
        Verdict::Hold(Hold::RunsOutFirst) => (
            "hold",
            Some(format!(
                "Holding: the week is projected empty {} before a size {size} card could finish.",
                at(window.projected_exhaustion_at),
            )),
        ),
    };
    json!({"size": size, "decision": decision, "note": note})
}

fn quota_data(store: &Store, size: Size) -> Result<Value, Failure> {
    let now = Jiff::now();
    let windows: Vec<QuotaWindow> = list(store.conn(), "1 = 1 ORDER BY label", &[])?;
    let provider = |window: &QuotaWindow| window.label.split(':').next().unwrap_or("").to_owned();
    let stale = |window: &QuotaWindow| {
        window.fetched_at.as_jiff() < lisbon::plus_minutes(now, -QUOTA_STALE_MINUTES)
    };
    let dot_of = |name: &str| -> &'static str {
        let weekly = windows
            .iter()
            .find(|w| provider(w) == name && w.kind.as_deref() == Some("weekly"));
        let Some(weekly) = weekly else { return "grey" };
        if stale(weekly) {
            return "grey";
        }
        let exhausted_early = matches!(
            (weekly.projected_exhaustion_at, weekly.resets_at),
            (Some(exhaustion), Some(resets)) if exhaustion < resets
        );
        if weekly
            .percent_remaining
            .is_some_and(|p| p <= WEEKLY_FLOOR_PERCENT)
            || exhausted_early
        {
            "red"
        } else if weekly.pace.as_deref() == Some("ahead") {
            "amber"
        } else {
            "green"
        }
    };
    let rows: Vec<Value> = windows
        .iter()
        .map(|window| {
            json!({
                "label": window.label,
                "title": window.title,
                "kind": window.kind,
                "percent_remaining": window.percent_remaining,
                "resets_at": window.resets_at,
                "resets_at_text": window.resets_at.map(|ts| lisbon::display(ts.as_jiff())),
                "pace": window.pace,
                "reserve": window.reserve,
                "burn_multiple": window.burn_multiple,
                "elapsed_percent": window.elapsed_percent,
                "projected_exhaustion_at": window.projected_exhaustion_at,
                "fetched_at": window.fetched_at,
                "fetch_error": window.fetch_error,
                "stale": stale(window),
                "dot": dot_of(&provider(window)),
                "dispatch": (window.kind.as_deref() == Some("weekly") && !stale(window))
                    .then(|| dispatch_of(window, size, now)),
            })
        })
        .collect();
    let mut providers: Vec<String> = windows.iter().map(provider).collect();
    providers.dedup();
    let dots: Vec<Value> = providers
        .iter()
        .map(|name| json!({"provider": name, "dot": dot_of(name)}))
        .collect();
    Ok(json!({"windows": rows, "providers": dots}))
}

pub fn quota(app: &App, args: QuotaArgs) -> Done {
    let store = app.read()?;
    Ok(Outcome::read(quota_data(&store, args.size)?))
}

// -- attention for the snapshot ----------------------------------------------

fn attention_data(store: &Store) -> Result<Value, Failure> {
    let items: Vec<tiller_core::domain::Attention> = list(
        store.conn(),
        "answered_at IS NULL AND withdrawn_at IS NULL ORDER BY opened_at, id",
        &[],
    )?;
    let mut rows = Vec::new();
    for item in items {
        let current = tiller_core::state::attention::still_current(store.conn(), &item)?;
        rows.push(json!({"item": item, "still_current": current}));
    }
    let waiting: Vec<Session> = list(
        store.conn(),
        "ended_at IS NULL AND activity = 'waiting_permission' ORDER BY activity_observed_at, id",
        &[],
    )?;
    Ok(json!({"attention": rows, "waiting_permission": waiting}))
}

// -- snapshot -----------------------------------------------------------------

/// The boot snapshot (`protocol.md` 7.6): each entry is what the matching
/// read returns, so the page and the API cannot drift.
pub fn snapshot(app: &App) -> Done {
    let store = app.read()?;
    store
        .conn()
        .execute_batch("BEGIN")
        .map_err(tiller_core::Error::from)?;
    let all = BoardArgs {
        project: None,
        filter: Filter::All,
        host: None,
        login: false,
    };
    let built = (|| -> Result<Value, Failure> {
        let last = store.last_seq()?;
        let projects: Vec<Project> = store.projects()?;
        Ok(json!({
            "v": 1,
            "server_time": Timestamp::now(),
            "last_event_seq": last,
            "thresholds": {
                "worker_stale_minutes": 15,
                "orchestrator_stale_minutes": 30,
                "quota_stale_minutes": QUOTA_STALE_MINUTES,
            },
            "queries": {
                "/api/v1/board?filter=all": board_data(&store, &all)?,
                "/api/v1/attention?state=open": attention_data(&store)?,
                "/api/v1/agents": agents_data(&store, None)?,
                "/api/v1/machines": machines_data(&store, false)?,
                "/api/v1/quota": quota_data(&store, Size::M)?,
                "/api/v1/projects": json!({"projects": projects}),
            },
        }))
    })();
    let _ = store.conn().execute_batch("END");
    Ok(Outcome::read(built?))
}

// -- events ------------------------------------------------------------------

pub fn events(app: &App, args: EventsArgs) -> Done {
    let store = app.read()?;
    let card_id = args
        .card
        .as_deref()
        .map(|card| tiller_core::state::resolve_card(store.conn(), card).map(|card| card.id))
        .transpose()?;
    let fetch = |since: i64| -> Result<Vec<tiller_core::domain::Event>, Failure> {
        Ok(list(
            store.conn(),
            "seq > ?1 AND (?2 IS NULL OR card_id = ?2) ORDER BY seq LIMIT ?3",
            &[&since, &card_id, &args.limit],
        )?)
    };
    if !args.follow {
        let events = fetch(args.since)?;
        let last = events.last().map_or(args.since, |event| event.seq);
        return Ok(Outcome::read(json!({"events": events, "last_seq": last})));
    }
    // Follow: one JSON line per event, polling the way the daemon does.
    let mut since = args.since;
    let stdout = std::io::stdout();
    loop {
        for event in fetch(since)? {
            since = event.seq;
            let mut out = stdout.lock();
            if writeln!(out, "{}", serde_json::to_string(&event)?).is_err() {
                // The reader went away.
                std::process::exit(0);
            }
            let _ = out.flush();
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// -- intake statistics ----------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct IntakeStats {
    pub days: i64,
    pub stops: i64,
    pub would_block: i64,
    pub false_blocks: i64,
    /// Null until there is a would-block stop to judge.
    pub false_block_rate: Option<f64>,
    pub min_stops: i64,
    pub max_false_block_rate: f64,
    /// The owner may switch to `block-once` (`protocol.md` 2.5).
    pub rule_met: bool,
}

/// The Stop hook's measurement over a window (`protocol.md` 2.5).
///
/// A `would_block` stop is a false block when, within two minutes and without
/// another owner prompt, the same session resolves every listed ask and every
/// unknown tag turns out to be a card created in that window.
pub fn intake_stats(store: &Store, days: i64) -> Result<IntakeStats, Failure> {
    let since = Timestamp::from_jiff(lisbon::plus_minutes(Jiff::now(), -days * 24 * 60));
    let stops: Vec<Observation> = list(
        store.conn(),
        "event = 'Stop' AND observed_at >= ?1
           AND session_id IN (SELECT id FROM sessions WHERE role = 'orchestrator')
         ORDER BY observed_at, id",
        &[&since],
    )?;
    let mut would_block = 0;
    let mut false_blocks = 0;
    for stop in &stops {
        if stop.payload.get("would_block").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        would_block += 1;
        if is_false_block(store, stop)? {
            false_blocks += 1;
        }
    }
    let total = stops.len() as i64;
    let rate = (would_block > 0).then(|| false_blocks as f64 / would_block as f64);
    // The rate is over would-block stops; no would-block stop is a rate of 0.
    let judged = rate.unwrap_or(0.0);
    Ok(IntakeStats {
        days,
        stops: total,
        would_block,
        false_blocks,
        false_block_rate: rate,
        min_stops: STOP_HOOK_MIN_STOPS,
        max_false_block_rate: STOP_HOOK_MAX_RATE,
        rule_met: total >= STOP_HOOK_MIN_STOPS && judged < STOP_HOOK_MAX_RATE,
    })
}

fn is_false_block(store: &Store, stop: &Observation) -> Result<bool, Failure> {
    let Some(session) = &stop.session_id else {
        return Ok(false);
    };
    let until = Timestamp::from_jiff(lisbon::plus_minutes(
        stop.observed_at.as_jiff(),
        FALSE_BLOCK_WINDOW_MINUTES,
    ));
    // Another owner prompt in the window means the owner moved on: not false.
    let prompted: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM observations
             WHERE session_id = ?1 AND event = 'UserPromptSubmit' AND observed_at > ?2 AND observed_at <= ?3",
            rusqlite::params![session, stop.observed_at, until],
            |row| row.get(0),
        )
        .map_err(tiller_core::Error::from)?;
    if prompted > 0 {
        return Ok(false);
    }
    let strings = |key: &str| -> Vec<String> {
        stop.payload
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|text| text.trim_matches(['[', ']']).to_owned())
                    .collect()
            })
            .unwrap_or_default()
    };
    for ask_id in strings("unresolved_ask_ids") {
        let resolved: Option<Ask> = find(
            store.conn(),
            "id = ?1 AND disposition != 'unresolved' AND resolved_by_session_id = ?2
             AND resolved_at > ?3 AND resolved_at <= ?4",
            &[&ask_id, session, &stop.observed_at, &until],
        )?;
        if resolved.is_none() {
            return Ok(false);
        }
    }
    for display_id in strings("unknown_tags") {
        let created: Option<Card> = find(
            store.conn(),
            "display_id = ?1 AND created_at > ?2 AND created_at <= ?3",
            &[&display_id, &stop.observed_at, &until],
        )?;
        if created.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn intake(app: &App, args: IntakeStatsArgs) -> Done {
    let store = app.read()?;
    Ok(Outcome::read(intake_stats(&store, args.days)?))
}
