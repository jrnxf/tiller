//! The board API's wire shapes (`protocol.md` section 7). Every response and
//! request body the daemon serves is one of these structs, so the OpenAPI
//! document (`contract/openapi.json`) describes exactly what goes over the
//! wire. Times are RFC 3339 UTC strings with milliseconds, as `state.db`
//! stores them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

// -- reads ------------------------------------------------------------------

/// One `card_column` row plus the card's own display fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BoardRow {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    /// The host of the card's newest run; null for a card with no run.
    pub host_id: Option<String>,
    pub kind: String,
    pub title: String,
    /// 0 is the most urgent, 4 the least.
    pub priority: i64,
    /// `asked`, `queued`, `working`, `landing` or `done`: how far the card's
    /// work has come.
    pub column: String,
    pub badge: String,
    /// The card waits on the owner (an open attention item or a permission
    /// prompt) in whatever column it is; the Needs you filter keeps it.
    #[serde(default)]
    pub needs_you: bool,
    pub parked_until: Option<String>,
    pub parked_reason: Option<String>,
    pub blocked_by: Option<String>,
    pub closed_at: Option<String>,
    pub rev: i64,
    /// The model of the card's newest run.
    pub model: Option<String>,
    /// The newest PR linked to the card.
    pub pr_number: Option<i64>,
    /// When the newest observation of the card's active run was made.
    pub observed_at: Option<String>,
    /// The owning host's label, on every row once a peer is paired
    /// (`protocol.md` 7.3); absent on a host with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A peer row's freshness: the peer's `server_time` of the newest frame
    /// its cache took. Absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// A peer row's link state; absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
}

/// An unresolved ask, from the `asked` view.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AskedRow {
    pub id: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub received_at: String,
    pub age_seconds: f64,
    /// The owning host's label, on every row once a peer is paired
    /// (`protocol.md` 7.3); absent on a host with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A peer row's freshness: the peer's `server_time` of the newest frame
    /// its cache took. Absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// A peer row's link state; absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BoardResponse {
    pub rows: Vec<BoardRow>,
    pub asked: Vec<AskedRow>,
}

/// One answer the asker offers, with what it sets in motion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AttentionOption {
    pub label: String,
    pub consequence: Option<String>,
    pub recommended: bool,
    /// The owner-only change picking this option applies on this host's
    /// board; absent when it applies none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<OwnerAction>,
}

/// An owner-only change an option applies when the owner picks it
/// (`state.md`, Attention).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OwnerAction {
    /// Sets one project's capacity, refused when it is no longer `from`.
    ProjectCapacity { project: String, from: i64, to: i64 },
}

/// The approval a `merge` item asks for, bound to the head the board shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ApprovalBinding {
    pub id: String,
    /// `merge` or `land_direct`.
    pub kind: String,
    pub head_sha: String,
    pub pr_number: Option<i64>,
    pub state: String,
}

/// What made an open attention item stale (`state.md`, Attention).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StaleItem {
    /// `card_closed`, `run_ended`, `session_ended`, `project_deleted` or
    /// `project_retired`.
    pub reason: String,
    /// The card display id, run, session or project the reason is about.
    pub subject: Option<String>,
    /// When it happened, when the state records it.
    pub at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AttentionItem {
    pub id: String,
    pub card_id: String,
    /// `decision`, `credential`, `merge`, `review`, `recovery` or `report`.
    pub kind: String,
    pub question: String,
    /// Text, or a JSON list of pointers (event ids, artifact ids, PR urls).
    pub evidence: Option<String>,
    pub recommendation: Option<String>,
    pub consequence: Option<String>,
    pub options: Option<Vec<AttentionOption>>,
    pub approval: Option<ApprovalBinding>,
    pub opened_by_session_id: Option<String>,
    pub opened_at: String,
    pub still_current: bool,
    /// Why an open item no longer applies; absent while it does and once it
    /// is answered or withdrawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<StaleItem>,
    /// The live session the question is about, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_session_id: Option<String>,
    /// The project the question is about, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_project_id: Option<String>,
    pub answered_at: Option<String>,
    pub withdrawn_at: Option<String>,
    pub rev: i64,
    /// The owning host's id, once a peer is paired; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// The owning host's label, on every row once a peer is paired
    /// (`protocol.md` 7.3); absent on a host with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A peer row's freshness: the peer's `server_time` of the newest frame
    /// its cache took. Absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// A peer row's link state; absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
}

/// A session waiting on a permission prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PermissionPrompt {
    pub session_id: String,
    pub card_id: String,
    pub tool: String,
    /// What the tool wants to run, from the session's newest observation.
    pub detail: Option<String>,
    pub since: String,
    /// The owning host's label, on every row once a peer is paired
    /// (`protocol.md` 7.3); absent on a host with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A peer row's freshness: the peer's `server_time` of the newest frame
    /// its cache took. Absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// A peer row's link state; absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AttentionResponse {
    pub items: Vec<AttentionItem>,
    pub permission_prompts: Vec<PermissionPrompt>,
}

/// A live session: an orchestrator with its workers nested, or a worker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentRow {
    pub session_id: String,
    /// The short role name: `orchestrator`, `scout`, `build`, `review` or
    /// `verify`.
    pub role: String,
    pub model: String,
    /// The effective activity (`session_activity`), staleness included.
    pub activity: String,
    pub activity_observed_at: Option<String>,
    pub last_status_line: Option<String>,
    pub herdr_name: Option<String>,
    pub card_id: Option<String>,
    pub host_id: String,
    #[schema(no_recursion)]
    pub workers: Vec<AgentRow>,
    pub project_id: String,
    pub run_id: Option<String>,
    pub pane_id: Option<String>,
    pub workspace_label: Option<String>,
    /// When the session entered its current activity.
    pub state_since: Option<String>,
    /// The owning host's label, on every row once a peer is paired
    /// (`protocol.md` 7.3); absent on a host with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A peer row's freshness: the peer's `server_time` of the newest frame
    /// its cache took. Absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// A peer row's link state; absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentsResponse {
    /// Orchestrators with their workers; workers whose orchestrator has ended
    /// are listed at the top level.
    pub orchestrators: Vec<AgentRow>,
}

/// One line of an event list, as the detail pages show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EventLine {
    pub at: String,
    /// `tiller`, `hook`, `herdr`, `transcript`, `liveness`, `status_line` or
    /// `gh`.
    pub source: String,
    pub text: String,
    /// `warn` or `bad`, for a line the page colors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
}

/// `GET /api/v1/runs/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RunDetail {
    pub id: String,
    pub card_id: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub events: Vec<EventLine>,
    /// The last lines of the run's live herdr pane, at most 200; empty when
    /// the run has no live pane or herdr did not answer.
    pub pane_tail: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CardRun {
    pub id: String,
    pub number: i64,
    pub role: String,
    pub model: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// `passed` (landed or reported), `failed` (failed or cut) or `cancelled`
    /// (cancelled, abandoned or relaunched); null while the run is active.
    pub outcome: Option<String>,
    pub note: Option<String>,
    pub session_id: Option<String>,
    pub host_id: Option<String>,
    pub activity: Option<String>,
    pub activity_observed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CardPr {
    pub id: String,
    pub number: i64,
    pub repo: String,
    pub title: String,
    /// `open`, `merged` or `closed`.
    pub state: String,
    pub head_sha: String,
    pub base: String,
    /// `pending`, `passing` or `failing`.
    pub checks: String,
    pub checks_summary: Option<String>,
    pub diff_summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NewerCommit {
    pub sha: String,
    pub subject: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CardApproval {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub pr_id: Option<String>,
    /// The commit the approval is bound to.
    pub head_sha: Option<String>,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    /// Commits pushed after the approval was bound, newest first.
    pub newer_commits: Vec<NewerCommit>,
}

/// How fresh one source of the card's facts is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Freshness {
    pub source: String,
    pub detail: String,
    pub observed_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CardFields {
    pub id: String,
    pub display_id: String,
    pub project_id: String,
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub priority: i64,
    pub slug: String,
    /// `open` or `closed`.
    pub state: String,
    pub outcome: Option<String>,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub rev: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HoldRow {
    pub id: String,
    pub until_at: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DepRow {
    pub display_id: String,
    pub released_at: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtifactRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    /// Where the page is served; absent for an artifact that is only a local file.
    pub url: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReviewRow {
    pub id: String,
    pub verdict: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ObligationRow {
    pub id: String,
    pub summary: String,
    pub state: String,
}

/// `GET /api/v1/cards/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CardDetail {
    pub card: CardFields,
    pub column: BoardRow,
    pub holds: Vec<HoldRow>,
    pub deps: Vec<DepRow>,
    pub attention: Vec<AttentionItem>,
    pub runs: Vec<CardRun>,
    pub prs: Vec<CardPr>,
    pub approvals: Vec<CardApproval>,
    pub artifacts: Vec<ArtifactRow>,
    pub reviews: Vec<ReviewRow>,
    pub obligations: Vec<ObligationRow>,
    pub events: Vec<EventLine>,
    pub freshness: Vec<Freshness>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TimelineEntry {
    pub seq: i64,
    pub at: String,
    /// `you`, `orchestrator`, `hook` or `system`.
    pub actor: String,
    pub text: String,
}

/// `GET /api/v1/cards/{id}/timeline`: newest first, paged by `before`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TimelineResponse {
    pub entries: Vec<TimelineEntry>,
    pub next_before: Option<i64>,
}

/// One host the board shows (`protocol.md` section 9).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Machine {
    pub id: String,
    pub label: String,
    /// `local` or `remote`.
    pub kind: String,
    pub view_only: bool,
    /// `ok`, `degraded` or `down` for this host; `ok` or `stale` for a
    /// remote one.
    pub health: String,
    pub running_agents: Option<i64>,
    pub capacity: Option<i64>,
    pub free_slots: Option<i64>,
    pub last_seen_at: Option<String>,
    /// `herdr_link_down`, `host_asleep`, `poll_failed` or `never_seen`; for a
    /// paired peer also `peer_link_down`, `peer_refused` or `peer_version`.
    pub stale_reason: Option<String>,
    /// A paired peer's link state (`protocol.md` 10.2); absent for this
    /// host and for a herdr-only machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
    /// `read` or `act`, for a paired peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_scope: Option<String>,
    /// Why the peer refused the link: `revoked`, `unknown_key`, `bad_sig`,
    /// `clock_skew`, `version`, `accept_off`, or `host_changed` when its
    /// `host_id` is not the paired one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_reason: Option<String>,
    pub rev: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MachinesResponse {
    pub machines: Vec<Machine>,
}

/// One quota window (`protocol.md` section 8).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct QuotaWindow {
    /// `<provider>:<window id>`, for example `claude:seven_day`.
    pub label: String,
    pub title: String,
    pub kind: String,
    pub percent_remaining: Option<f64>,
    pub resets_at: Option<String>,
    pub pace: Option<String>,
    pub reserve: Option<f64>,
    pub burn_multiple: Option<f64>,
    pub elapsed_percent: Option<f64>,
    pub projected_exhaustion_at: Option<String>,
    pub fetched_at: String,
    /// Older than 15 minutes.
    pub stale: bool,
    /// `green`, `amber`, `red` or `grey`, read from the weekly window.
    pub dot: String,
    /// A short code for the last failed run, never quota-axi's stderr.
    pub fetch_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct QuotaResponse {
    pub windows: Vec<QuotaWindow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub delivery: String,
    pub capacity: i64,
    pub rev: i64,
    /// `pre-launch` or `launched`.
    pub stage: String,
    /// The mode builders and the land loop follow: `delivery`, or
    /// `direct-pr` for a pre-launch project that otherwise runs `no-mistakes`.
    pub effective_delivery: String,
    /// The last commit a batched review covered.
    pub reviewed_through_sha: Option<String>,
    /// When `reviewed_through_sha` was last set: a pass finished, or the
    /// project became pre-launch.
    pub reviewed_at: Option<String>,
    pub stage_changed_at: Option<String>,
    /// When the next weekly review is due, while pre-launch.
    pub next_review_due: Option<String>,
    /// A weekly review came due over 24 hours ago and no pass started.
    pub review_overdue: bool,
    /// `weekly` or `final` while a review pass is pending or running.
    pub review_open: Option<String>,
    /// The owning host's id, once a peer is paired; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// The owning host's label, on every row once a peer is paired
    /// (`protocol.md` 7.3); absent on a host with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// A peer row's freshness: the peer's `server_time` of the newest frame
    /// its cache took. Absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
    /// A peer row's link state; absent on this host's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
    /// When the project was retired; present only on the archived list
    /// (`?retired=true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
}

/// One published PR on the contributions view: the `prs` row with the card
/// that produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Contribution {
    pub id: String,
    pub project_id: String,
    pub card_id: String,
    pub card_display_id: String,
    pub card_title: String,
    pub repo: String,
    pub number: i64,
    pub url: String,
    /// `open`, `merged` or `closed`.
    pub state: String,
    pub linked_at: String,
    pub merged_at: Option<String>,
}

/// `GET /api/v1/contributions`: published PRs, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ContributionsResponse {
    pub contributions: Vec<Contribution>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectsResponse {
    pub projects: Vec<Project>,
}

/// One Lisbon week of the Health page's counts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SignalsWeek {
    pub start: String,
    pub end: String,
    pub landed: i64,
    pub reported: i64,
    pub merged_prs: i64,
    pub needed: i64,
    /// Items answered in the week: the sample behind the median.
    pub answered: i64,
    pub median_answer_minutes: Option<f64>,
    /// Why the developer was needed, after the developer's own relabels.
    pub interventions: SignalsInterventions,
    pub reverts: i64,
    pub escaped: i64,
    pub rework: i64,
    pub lost_work: i64,
    pub duplicate_dispatches: i64,
    pub unauthorized: i64,
}

/// Interventions of each kind in one week. `planned` is the routine share
/// and is never counted as avoidable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SignalsInterventions {
    pub correction: i64,
    pub unplanned_escalation: i64,
    pub rework: i64,
    pub planned: i64,
}

/// One time the developer was needed, with the label it counts under.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SignalsIntervention {
    pub id: String,
    /// `attention` or `note`.
    pub source: String,
    pub card_id: String,
    pub display_id: String,
    pub project_id: String,
    pub at: String,
    pub summary: String,
    /// What the rules said: `correction`, `unplanned_escalation`, `rework`
    /// or `planned`.
    pub classified: String,
    /// What counts: the developer's label when there is one, else `classified`.
    pub kind: String,
    pub relabeled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SignalsProject {
    pub project_id: String,
    pub accepted: i64,
    pub needed: i64,
    /// Avoidable interventions: corrections, unplanned escalations, rework.
    pub avoidable: i64,
    pub escaped: i64,
    pub answered: i64,
    pub median_answer_minutes: Option<f64>,
}

/// `GET /api/v1/signals`: the last 8 Lisbon weeks, oldest first, the last
/// being the week in progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SignalsResponse {
    pub weeks: Vec<SignalsWeek>,
    pub by_project: Vec<SignalsProject>,
    /// The latest interventions across the weeks, newest first, to relabel.
    pub interventions: Vec<SignalsIntervention>,
    pub machines_live: i64,
    pub machines_total: i64,
}

/// One poller's record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PollerHealth {
    pub source: String,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

/// `GET /api/v1/health`: the daemon's own state. No work data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub ok: bool,
    pub version: String,
    /// The schema this binary migrates to.
    pub schema: usize,
    /// False until `tiller init` has made `state.db`.
    pub initialized: bool,
    pub pid: u32,
    pub started_at: String,
    pub server_time: String,
    pub last_event_seq: i64,
    pub pollers: Vec<PollerHealth>,
    /// Tool versions read at start, by tool name; null when not found.
    #[schema(value_type = Object)]
    pub tools: Value,
    /// Each peer link's last success and last error (`protocol.md` 10.4);
    /// absent on a host with no peers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkHealth>,
    /// The merge App key, as `tiller doctor`'s `merge app` line reads it.
    pub merge_app: MergeAppHealth,
}

/// The merge App key under `~/.tiller/forge-app/`: names files and the
/// reason, never the key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MergeAppHealth {
    /// `ok`, `missing` (every project runs as `detect`), `warn` (the key is
    /// not used) or `fail` (the key is readable by group or others).
    pub status: String,
    /// The App's numeric id, when the key loads.
    pub app_id: Option<u64>,
    pub detail: String,
}

/// The inlined `<script id="tiller-boot">` body and `GET /api/v1/snapshot`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Snapshot {
    /// Always 1.
    pub v: u8,
    pub server_time: String,
    pub last_event_seq: i64,
    /// Staleness thresholds in minutes, by name.
    #[schema(value_type = Object)]
    pub thresholds: Value,
    /// Each entry is exactly the response of that GET, keyed by its path
    /// and query string.
    #[schema(value_type = Object)]
    pub queries: Value,
    /// One entry per paired peer (`protocol.md` 7.6); absent on a host with
    /// none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<SnapshotPeer>,
}

/// A peer link's state (`protocol.md` 10.2): `live` with a frame in the last
/// 25 s, `lagging` up to 2 min, `down` after that or while reconnecting
/// fails, `refused` after a refused `hello` or a changed `host_id`, `never`
/// before the first `welcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    Live,
    Lagging,
    Down,
    Refused,
    Never,
}

impl LinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Lagging => "lagging",
            Self::Down => "down",
            Self::Refused => "refused",
            Self::Never => "never",
        }
    }
}

/// One paired peer in the boot snapshot (`protocol.md` 7.6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SnapshotPeer {
    /// The peer's label.
    pub host: String,
    /// The peer's `hosts` row id here, the id its Machines row carries.
    pub host_id: String,
    pub link: LinkState,
    pub as_of: Option<String>,
    /// The peer's clock minus this host's, from its newest `welcome` or
    /// `beat`; staleness of its agents is computed against its clock.
    pub server_time_offset_ms: i64,
    /// `read` or `act`.
    pub scope: String,
    /// The peer `seq` this host last applied.
    pub last_event_seq: Option<i64>,
}

/// One peer link in `GET /api/v1/health`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LinkHealth {
    pub host: String,
    pub link: LinkState,
    /// When the link last took a frame.
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
}

// -- the event stream -------------------------------------------------------

/// One `event: change` message on `GET /api/v1/events`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChangeEvent {
    pub seq: i64,
    pub at: String,
    pub kind: String,
    pub txn: String,
    pub entity: String,
    pub entity_id: String,
    pub rev_after: Option<i64>,
    pub card_id: Option<String>,
    /// The entity's API shape when the event was sent, with its current
    /// `rev`: an attention item, a machine, a quota window, a project, or
    /// `{"state": ...}` for an outbox message or an approval. Null for an
    /// entity the board refetches instead.
    #[schema(value_type = Object, nullable)]
    pub entity_state: Value,
    /// The card's recomputed `card_column` row, when `card_id` is set and the
    /// card is still on the board.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<BoardRow>,
}

/// One `event: reset` message: the client refetches the snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ResetEvent {
    pub seq: i64,
    pub server_time: String,
}

/// One `event: peer` message: a paired peer's change, with no SSE `id`, since
/// a peer's `seq` is not this host's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PeerEvent {
    pub host: String,
    pub host_id: String,
    pub as_of: Option<String>,
    pub change: ChangeEvent,
}

/// One `event: link` message: a peer link changed state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LinkEvent {
    pub host: String,
    pub link: LinkState,
    pub as_of: Option<String>,
    /// `peer_link_down`, `peer_refused` or `peer_version`; null while live.
    pub stale_reason: Option<String>,
}

/// One `event: peer_reset` message, sent per peer after a reconnecting
/// stream's replay: the peer's cached rows replace what the board holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PeerResetEvent {
    pub host: String,
    pub host_id: String,
    pub link: LinkState,
    pub as_of: Option<String>,
    /// The peer's rows by snapshot path, as the federated reads return them.
    #[schema(value_type = Object)]
    pub queries: Value,
}

// -- mutations --------------------------------------------------------------

/// The receipt every mutation returns (`protocol.md` section 7.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Receipt {
    pub recorded_at: String,
    pub outbox_id: Option<String>,
    pub approval_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MutationResponse {
    /// Always true.
    pub ok: bool,
    pub replayed: bool,
    pub events: Vec<i64>,
    pub receipt: Receipt,
    /// The changed entity.
    #[schema(value_type = Object)]
    pub data: Value,
    /// The peer that applied a forwarded mutation (`protocol.md` 10.3);
    /// absent for this host's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub current: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// `peer_refused` only: which check refused (`protocol.md` 10.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The peer a forwarded mutation was for (`protocol.md` 10.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    /// Always false.
    pub ok: bool,
    pub error: ErrorDetail,
}

/// `POST /api/v1/cards`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateCard {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    pub project: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
}

/// `POST /api/v1/projects`: register a project on this host, as `tiller
/// project add` does with the fields the board asks for. `--checks`,
/// `--remote`, `--default-branch` and the deploy flags stay CLI-only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddProject {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a project add is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    pub id: String,
    pub name: String,
    /// The GitHub repo, `owner/name`; absent for a project with none.
    #[serde(default)]
    pub repo: Option<String>,
    /// `no-mistakes`, `no-mistakes-prod-only`, `direct-pr`, `direct-push` or
    /// `local-only`.
    pub delivery: String,
    /// How many builders may run at once; absent takes the default.
    #[serde(default)]
    pub capacity: Option<i64>,
}

/// `POST /api/v1/projects/{id}/asks`: a note typed in the board's ask box,
/// recorded and sent to the project's orchestrator as `tiller ask note` does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AskNote {
    pub idempotency_key: String,
    /// A paired peer's label. Not forwarded: a note for a peer's project is
    /// 502 `peer_refused` (`protocol.md` 7.4).
    #[serde(default)]
    pub host: Option<String>,
    /// The ask, in the owner's words; multi-line.
    pub text: String,
}

/// `PATCH /api/v1/cards/{id}`: `rev` plus the changed fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EditCard {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    pub rev: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub body: Option<String>,
}

/// `POST /api/v1/projects/{id}/stage`: the owner's stage switch, against
/// the `rev` the board read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StageProject {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    pub rev: i64,
    /// `pre-launch` or `launched`.
    pub stage: String,
}

/// `GET /api/v1/projects/{id}/delete`: what a project delete would remove,
/// without removing anything (`state.md`, Deleting a project).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectDeletePreview {
    pub project: String,
    /// Rows per table the delete removes, the project row and its events
    /// included.
    pub removed: BTreeMap<String, i64>,
    /// `table.column` references on rows that stay, set to `NULL`.
    pub unlinked: BTreeMap<String, i64>,
    /// Why the delete is refused now (a live run or session, the fleet
    /// project, a project another host owns); absent when it would apply.
    pub refused: Option<String>,
    /// What clears `refused`, when the owner can clear it: absent for the
    /// fleet project and another host's project.
    pub unblock: Option<DeleteUnblock>,
    /// The running agents the delete stops first, the orchestrator first.
    pub agents: Vec<ProjectAgent>,
    /// Worktrees with commits not landed or uncommitted changes. The delete
    /// leaves them on disk.
    pub unlanded: Vec<ProjectUnlanded>,
    /// Pull requests still open. The delete leaves them on GitHub.
    pub open_prs: Vec<ProjectOpenPr>,
}

/// A running agent of a project: a live session, or an active run with no
/// live session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectAgent {
    /// `orchestrator`, `builder`, `reviewer` and so on.
    pub role: String,
    /// The live session; absent for an active run with no live session.
    pub session: Option<String>,
    /// The display id of the worker's card; absent for an orchestrator.
    pub card: Option<String>,
    pub run: Option<String>,
    /// Whether it runs on this host, the only one that can stop it.
    pub local: bool,
}

/// A worktree of a project with work not landed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectUnlanded {
    pub path: String,
    pub branch: Option<String>,
    /// The display id of the card whose run made it, when known.
    pub card: Option<String>,
    /// Commits not on the base branch; absent when not yet read.
    pub unlanded_commits: Option<i64>,
    /// Uncommitted changes; absent when not yet read.
    pub dirty: Option<bool>,
}

/// An open pull request of a project's card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectOpenPr {
    /// The display id of its card.
    pub card: String,
    pub number: i64,
    pub url: String,
}

/// `GET /api/v1/projects/{id}/retire`: whether a retire would apply now
/// and which running agents it could stop first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectRetirePreview {
    pub project: String,
    /// Why the retire is refused now (an open card, the fleet project);
    /// absent when it would apply.
    pub refused: Option<String>,
    /// The project's running agents, the orchestrator first.
    pub agents: Vec<ProjectAgent>,
}

/// `GET /api/v1/projects/{id}/setup`: what `tiller doctor`'s `merge_gates`
/// line and `tiller config get checkouts.<id>` show for one of this host's
/// projects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectSetup {
    pub project: String,
    /// The GitHub repo, `owner/name`; absent for a project with none.
    pub repo: Option<String>,
    /// `enforced` (GitHub refuses a merge the App did not make) or `detect`.
    pub merge_gate: String,
    /// The newest `project.merge_gate_changed` event. The hourly ruleset
    /// check only writes when the value moves.
    pub merge_gate_changed_at: Option<String>,
    /// `checkouts.<id>` from `config.toml`; absent when unset.
    pub checkout: Option<String>,
    /// Why `config.toml` could not be read; `checkout` is then absent.
    pub config_error: Option<String>,
}

/// `POST /api/v1/projects/{id}/retire`: retire one project, off the board.
/// Local to the owning host; it is not forwarded to a peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RetireProject {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a project retire is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// Stop the project's running agents first, as a delete does. Unset
    /// leaves them running.
    #[serde(default)]
    pub stop_agents: bool,
}

/// The owner's way past a refused delete.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeleteUnblock {
    /// What to do, as a short imperative ("cancel its card").
    pub step: String,
    /// The command that starts it, when there is one.
    pub command: Option<String>,
}

/// `POST /api/v1/projects/{id}/delete`: the hard delete of one project.
/// Local to the owning host; it is not forwarded to a peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteProject {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a project delete is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// The project id typed again: the delete applies only when it matches.
    pub confirm: String,
}

/// `POST /api/v1/interventions/{id}/label`: the developer's label for an
/// intervention. Local to this host; it is not forwarded to a peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RelabelBody {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a relabel is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// `correction`, `unplanned_escalation`, `rework` or `planned`; absent
    /// or null clears the label so the classifier's own counts again.
    #[serde(default)]
    pub label: Option<String>,
}

/// `POST /api/v1/cards/{id}/comment`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Comment {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    pub text: String,
}

/// `POST /api/v1/cards/{id}/hold`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HoldBody {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    /// RFC 3339.
    pub until: String,
    pub reason: String,
    /// The rev the board showed, for the conflict message only.
    #[serde(default)]
    pub rev: Option<i64>,
}

/// A transition's body: `DELETE .../hold`, `close`, `cancel`, `reopen`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    /// `close` only: the outcome, `completed` when omitted.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Required by `cancel` and `reopen`.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub rev: Option<i64>,
}

/// `POST /api/v1/attention/{id}/answer`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerBody {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    /// The owner's exact words.
    pub answer: String,
    #[serde(default)]
    pub rev: Option<i64>,
}

/// `POST /api/v1/attention/{id}/withdraw`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WithdrawBody {
    pub idempotency_key: String,
    /// A paired peer's label: the withdraw is not forwarded, so a peer's
    /// item is refused (`protocol.md` 7.4).
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/approvals/{id}/decide`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideBody {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
    /// `accept` or `reject`.
    pub decision: String,
    /// The head the board showed; a mismatch is 422 with the bound head.
    pub head_sha: String,
    #[serde(default)]
    pub rev: Option<i64>,
}

/// `POST /api/v1/projects/{id}/orchestrator`. The key is taken like every
/// mutation's, but a retry is natural: a live orchestrator is returned, not a
/// second one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartOrchestrator {
    pub idempotency_key: String,
    /// A paired peer's label; a start is not forwarded (`protocol.md` 7.4).
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/agents/{session}/terminal`: opens the terminal mirror.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalOpen {
    /// A paired peer's label; the mirror is not forwarded (`protocol.md`
    /// 7.4).
    #[serde(default)]
    pub host: Option<String>,
    /// The board's own size for the pane: herdr resizes the pane to it while
    /// the stream is open and back when it ends (Terminal mirror, Fit).
    /// Absent means the pane keeps its size and only the picture scales.
    #[serde(default)]
    pub fit: Option<TerminalFit>,
}

/// The size the Fit asks herdr to give a pane, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalFit {
    /// 20 to 500.
    pub cols: u16,
    /// 5 to 200.
    pub rows: u16,
}

/// `POST /api/v1/agents/{session}/terminal/fit`: a new board size for the
/// Fit an open stream holds on the agent's pane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalRefit {
    /// 20 to 500.
    pub cols: u16,
    /// 5 to 200.
    pub rows: u16,
    /// A paired peer's label; a refit is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/agents/{session}/terminal/input`: exactly one of `keys`
/// and `text`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalInput {
    /// Keys pressed in order, as `herdr pane send-keys` names them:
    /// `enter`, `esc`, `ctrl+c`, `left`, `space`, or one character. Up to 64.
    #[serde(default)]
    pub keys: Option<Vec<String>>,
    /// Text pasted as one bracketed paste, up to 4096 bytes.
    #[serde(default)]
    pub text: Option<String>,
    /// A paired peer's label; input is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// One line of the terminal mirror's NDJSON stream (`protocol.md` 7.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminalLine {
    /// The pane's size in cells. It opens the stream and comes again when
    /// the pane is resized; a full frame follows it.
    Size { cols: u16, rows: u16 },
    /// One herdr frame: the ANSI bytes, base64. A `full` frame redraws the
    /// whole screen; the frames after it are diffs, `seq` counting up by
    /// one.
    Frame { seq: u64, full: bool, bytes: String },
    /// The pane closed; the stream ends.
    Closed { reason: Option<String> },
    /// herdr stopped answering; the stream ends.
    Unavailable { message: String },
    /// herdr would not resize the pane for the Fit, most often because
    /// another client controls it. The stream goes on at the pane's own size,
    /// with a `size` line next.
    Unfit { message: String },
}

/// `POST /api/v1/agents/{session}/focus`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FocusBody {
    pub idempotency_key: String,
    /// A paired peer's label: forward the mutation to it (`protocol.md`
    /// 7.4, 10.3). Absent means this host.
    #[serde(default)]
    pub host: Option<String>,
}
