//! The board API's wire shapes (`protocol.md` section 7). Every response and
//! request body the daemon serves is one of these structs, so the OpenAPI
//! document (`contract/openapi.json`) describes exactly what goes over the
//! wire. Times are RFC 3339 UTC strings with milliseconds, as `state.db`
//! stores them.

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
    /// `needs_you`, `asked`, `working`, `landing`, `queued` or `done`.
    pub column: String,
    pub badge: String,
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
