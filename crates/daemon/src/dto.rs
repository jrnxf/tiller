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
    /// The card's category, for example `tiller-bug` (`state.md`, Cards);
    /// absent with none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The project of the session that filed a Tiller bug, from its
    /// `card.routed` event; absent for every other card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filed_from: Option<String>,
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
    /// Where the ask came from: `prompt` (typed in an agent's pane),
    /// `board`, `script` or `import`. Absent on a row from a peer that
    /// predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The ask's first line, at most 200 characters. Only on this host's
    /// rows: a peer link carries no ask text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
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
    /// When the agent last produced output: a tool call, a subagent or the
    /// end of a turn, as hooks report them (`protocol.md` 7.3). Null before
    /// its first, and on a host with hooks blocked.
    pub last_output_at: Option<String>,
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
    /// Present when the session runs an older tiller than its host does now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<StaleInfo>,
    /// The run's dated pause (`run.pause`), while its date is ahead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<PauseInfo>,
}

/// A run's dated pause, as the newest `run.paused` event records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PauseInfo {
    /// When the pause stops silencing the run's alarms.
    pub until: String,
    /// When the pause was taken.
    pub at: String,
    /// Who took it: `board`, `owner`, the worker's own session or its
    /// orchestrator's.
    pub by: String,
    pub reason: Option<String>,
}

/// How a session's launch differs from what its host's tiller would write
/// now, so the board can offer a restart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StaleInfo {
    /// The tiller version it started on, or `unknown` when its launch left
    /// no record.
    pub since: String,
    /// Whether the role prompt or hook settings differ, not just the version.
    pub prompt_changed: bool,
    /// A restart waits for the session to go idle.
    pub restart_queued: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AgentsResponse {
    /// Orchestrators with their workers; workers whose orchestrator has ended
    /// are listed at the top level.
    pub orchestrators: Vec<AgentRow>,
    /// The tiller version of the host that answered.
    #[serde(default)]
    pub tiller_version: String,
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
    /// The PR's page on the forge.
    pub url: String,
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
    /// Why the dependency was released: the target's satisfying outcome
    /// (`landed`, `reported`, `answered`, `completed`), or `removed` by hand.
    /// Absent while it is open, and on a row from a peer that predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_reason: Option<String>,
    /// The card waited on: its title, its board column (`queued`, `working`,
    /// `landing` or `done`) and its badge (`state.md` section 6), so the card
    /// page can show its state. Absent on a row from a peer that predates them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
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
    /// The full commit SHA the review covered (B11). Null on a row from
    /// before migration 16, and absent from an older peer.
    pub head_sha: Option<String>,
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
    /// The display ids of every card that waits on this one, directly or
    /// through other cards. This card cannot wait on any of them: that would
    /// make a loop (`state.md` section 8).
    #[serde(default)]
    pub waiting_on_this: Vec<String>,
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
    /// The words the line quotes: a reply's body on `ask.replied`. Absent
    /// from a peer that predates replies.
    #[serde(default)]
    pub quote: Option<String>,
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
    /// The `tiller version` string this host runs, for example
    /// `0.1.0-pre.17`: this host's own build, or what a paired peer reports
    /// on its own row. Absent for a herdr-only machine and for a peer that
    /// has not reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// A paired peer's link state (`protocol.md` 10.2); absent for this
    /// host and for a herdr-only machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkState>,
    /// `read` or `act`, for a paired peer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_scope: Option<String>,
    /// Why the peer refused the link: `revoked`, `unknown_key`, `bad_sig`,
    /// `clock_skew`, `replayed`, `version`, `accept_off`, or `host_changed`
    /// when its `host_id` is not the paired one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_reason: Option<String>,
    /// The open project `tiller bug file` routes to on this host (`state.md`,
    /// Host); absent with none, when Tiller bugs wait in `fleet`. A peer's
    /// is what its own row reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bug_project: Option<String>,
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
    /// When `rev` last moved: an edit, a stage change, a new card. Null only
    /// on a peer row from a tiller that predates it.
    #[serde(default)]
    pub edited_at: Option<String>,
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
    /// Mutations the owner made at a terminal through the CLI (`authority.md`
    /// G23). Not an incident: it is there so the owner can spot one they did
    /// not make.
    pub cli_decisions: i64,
}

/// One mutation the owner made through the CLI, and where it came from:
/// what its events' `payload.via_terminal` says (`authority.md` G23).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SignalsCliDecision {
    pub at: String,
    /// The mutation's name, like `approval.decide`.
    pub txn: String,
    /// The card it touched, when it touched one.
    pub display_id: Option<String>,
    /// The terminal device, like `/dev/pts/3`.
    pub tty: Option<String>,
    /// The parent process's executable name and pid.
    pub parent: Option<String>,
    pub parent_pid: Option<i64>,
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
    /// The latest CLI decisions across the weeks, newest first, at most 20.
    pub cli_decisions: Vec<SignalsCliDecision>,
    pub machines_live: i64,
    pub machines_total: i64,
    /// When the store's first event happened: the day Tiller started on this
    /// host. Null on a store with no events.
    pub first_event_at: Option<String>,
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

/// One check's verdict in `tiller doctor` and `GET /api/v1/doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

/// One of the host's checks (`tiller doctor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    /// A failed required check fails `tiller doctor`; a failed optional one
    /// is a warning.
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub detail: String,
    /// Which case of the check this is, for the checks the board words
    /// itself: the owner marker (`unset`, `tiller`, `named_other`,
    /// `other_system`, `other_system_daemon_running`) and the other system's
    /// processes (`not_running`, `kept_read_only`, `both_dispatch`,
    /// `processes_unreadable`). Absent on every other check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl DoctorCheck {
    pub fn new(name: &str, required: bool, status: CheckStatus, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            status,
            required,
            version: None,
            detail: detail.into(),
            code: None,
        }
    }

    #[must_use]
    pub fn version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }

    #[must_use]
    pub fn code(mut self, code: &str) -> Self {
        self.code = Some(code.to_owned());
        self
    }

    /// A check that did not pass: `fail` when required, else `warn`.
    pub fn missing(name: &str, required: bool, detail: impl Into<String>) -> Self {
        let status = if required {
            CheckStatus::Fail
        } else {
            CheckStatus::Warn
        };
        Self::new(name, required, status, detail)
    }

    /// A required check that failed, which fails the whole report.
    pub fn failed(&self) -> bool {
        self.required && self.status == CheckStatus::Fail
    }
}

/// One project's stored merge gate, as `tiller doctor` reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DoctorGate {
    pub project: String,
    pub merge_gate: String,
    pub status: CheckStatus,
    /// The newest `project.merge_gate_changed` event. The hourly ruleset
    /// check only writes when the value moves, so no check time is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_changed_at: Option<String>,
    pub detail: String,
}

/// `tiller doctor`'s report: `ok` is false when a required check failed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DoctorReport {
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
    pub merge_gates: Vec<DoctorGate>,
}

/// `GET /api/v1/doctor`: the report of the checks `tiller doctor` runs, run
/// on this host just now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Doctor {
    /// When the checks ran.
    pub checked_at: String,
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
    pub merge_gates: Vec<DoctorGate>,
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
    /// What the owner should know about a mutation that applied, such as
    /// why a re-added project's cards do not start at 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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

/// `POST /api/v1/login`: the 8-digit code `tiller board --login` printed.
/// No `Debug`, so the code cannot reach a log line by accident.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CodeLoginBody {
    /// Exactly 8 ASCII digits.
    #[schema(example = "12345678")]
    pub code: String,
}

/// `GET /api/v1/session`, and the answer to signing in or out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionView {
    /// Whether this browser holds a live owner cookie.
    pub owner: bool,
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
    /// At most 1,000 characters (`protocol.md` 7.4).
    #[schema(max_length = 1000)]
    pub title: String,
    /// At most 100,000 characters (`protocol.md` 7.4).
    #[schema(max_length = 100000)]
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    /// The card's category; the board's Move sends `tiller-bug`.
    #[serde(default)]
    pub category: Option<String>,
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
    /// Absent or empty: the id. The board's Add project asks for no name.
    #[serde(default)]
    pub name: Option<String>,
    /// The GitHub repo, `owner/name`; absent for a project with none.
    #[serde(default)]
    pub repo: Option<String>,
    /// `no-mistakes`, `no-mistakes-prod-only`, `direct-pr`, `direct-push` or
    /// `local-only`.
    pub delivery: String,
    /// How many builders may run at once; absent takes the default.
    #[serde(default)]
    pub capacity: Option<i64>,
    /// The local checkout to set as `checkouts.<id>`, as `tiller config set`
    /// does; absent leaves the key as it is.
    #[serde(default)]
    pub checkout: Option<NewCheckout>,
}

/// Add project's Local checkout: a folder that is a checkout of the project
/// already, or one to clone the project into before it is added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewCheckout {
    /// An absolute path, or one that starts with `~/`. Empty:
    /// `<spawn.checkouts_root>/<id>`.
    #[serde(default)]
    pub path: String,
    /// Clone the repo into the folder, which must be missing or empty.
    /// `false`: the folder must be a checkout of the repo already.
    #[serde(default)]
    pub clone: bool,
}

/// `GET /api/v1/projects/checkout`: what Add project would do with a Local
/// checkout folder, checked as it is typed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CheckoutCheck {
    /// The folder `checkouts.<id>` would name; absent when the path itself
    /// is refused.
    pub path: Option<String>,
    /// `use` (a checkout of the repo is there) or `clone`; absent when the
    /// folder is refused.
    pub plan: Option<String>,
    /// The remote a clone would fetch.
    pub remote: Option<String>,
    /// Why the folder does not suit; absent when it does.
    pub problem: Option<String>,
}

/// `POST /api/v1/peers`: Add a host in Settings pairs this host with another
/// one, as `tiller peer add` does (`protocol.md` 10.1, From the board).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddPeer {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a pairing is never forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// The name the new host goes by on this board: a letter or digit, then
    /// letters, digits, `_` or `-`.
    pub label: String,
    /// The ssh alias that reaches it from this host, with no password.
    pub ssh: String,
    /// `act`: read it and forward the owner's mutations; or `read`.
    pub scope: String,
}

/// `POST /api/v1/projects/{id}/asks`: a note typed in the board's ask box,
/// recorded and sent to the project's orchestrator as `tiller ask note` does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AskNote {
    pub idempotency_key: String,
    /// A paired peer's label: the note is recorded on that host
    /// (`protocol.md` 7.4).
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
    /// At most 1,000 characters (`protocol.md` 7.4).
    #[schema(max_length = 1000)]
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    /// At most 100,000 characters (`protocol.md` 7.4). An empty body clears
    /// it, as `tiller card edit --body ""` does.
    #[schema(max_length = 100000)]
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

/// `PATCH /api/v1/projects/{id}`: `tiller project edit` from the board,
/// against the `rev` the board read. An absent field stays as it is. An
/// empty text clears `repo`, `remote`, `deploy_workflow` and
/// `deploy_environment`, as an empty CLI flag does. Local to the owning
/// host; it is not forwarded to a peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EditProject {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a project edit is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    pub rev: i64,
    #[serde(default)]
    pub name: Option<String>,
    /// The GitHub repo, `owner/name`.
    #[serde(default)]
    pub repo: Option<String>,
    /// The git remote builders push to.
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    /// `no-mistakes`, `no-mistakes-prod-only`, `direct-pr`, `direct-push` or
    /// `local-only`.
    #[serde(default)]
    pub delivery: Option<String>,
    /// The commands a `direct-push` builder runs: a list, or `{"full": [..],
    /// "no_logic": [..]}` with a lighter set for a change with no logic.
    #[serde(default)]
    pub checks: Option<Value>,
    /// How many builders may run at once; at least 1.
    #[serde(default)]
    pub capacity: Option<i64>,
    #[serde(default)]
    pub deploy_workflow: Option<String>,
    #[serde(default)]
    pub deploy_environment: Option<String>,
    /// At least 1.
    #[serde(default)]
    pub deploy_timeout_minutes: Option<i64>,
}

/// `GET /api/v1/projects/{id}/grants`: every standing grant of the project,
/// newest first, live and ended alike (`authority.md`, Standing grants).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectGrants {
    pub project: String,
    pub grants: Vec<GrantRow>,
}

/// One grant row and how many approvals it accepted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct GrantRow {
    pub id: String,
    /// The last 7 characters of the id, as `tiller grant revoke` takes it.
    pub short: String,
    pub project_id: String,
    /// `merge` or `land_direct`.
    pub kind: String,
    /// The version 1 conditions object (`authority.md` section 6). An
    /// `away_approval_id` marks a grant away mode made.
    #[schema(value_type = Object)]
    pub conditions: Value,
    pub granted_at: String,
    pub granted_by: String,
    /// Null while the grant is live. A grant past its `expires_at` stops
    /// matching without being revoked.
    pub revoked_at: Option<String>,
    pub revoke_reason: Option<String>,
    /// The approvals the grant accepted.
    pub uses: i64,
}

/// `POST /api/v1/projects/{id}/grants`, as `tiller grant create` does. Owner
/// only, local to the owning host; it is not forwarded to a peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateGrant {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a grant is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// `merge` or `land_direct`.
    pub kind: String,
    /// The version 1 conditions object (`authority.md` section 6).
    #[schema(value_type = Object)]
    pub conditions: Value,
    /// When the grant ends, as `--expires` takes it; it sets
    /// `conditions.expires_at`.
    #[serde(default)]
    pub expires: Option<String>,
}

/// `DELETE /api/v1/grants/{id}`, as `tiller grant revoke` does. Owner only,
/// local to the owning host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeGrant {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a revoke is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// Why the grant ends; "revoked by the owner" when absent.
    #[serde(default)]
    pub reason: Option<String>,
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

/// One setting an orchestrator start would refuse (`protocol.md` 6.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SettingProblem {
    /// The `config.toml` key: `checkouts.<id>`, `spawn.pool_root` or
    /// `spawn.checkouts_root` (or the home or scratch key, which the board
    /// does not set).
    pub key: String,
    /// `missing` (not in `config.toml`) or `invalid` (set, and refused).
    pub kind: String,
    /// What a start would say about it.
    pub problem: String,
}

impl From<tiller_agents::spawn::settings::Problem> for SettingProblem {
    fn from(problem: tiller_agents::spawn::settings::Problem) -> Self {
        Self {
            key: problem.key,
            kind: match problem.kind {
                tiller_agents::spawn::settings::Kind::Missing => "missing",
                tiller_agents::spawn::settings::Kind::Invalid => "invalid",
            }
            .to_owned(),
            problem: problem.problem,
        }
    }
}

/// `GET /api/v1/start-settings`: the host-level settings an orchestrator
/// start reads, as this host's `config.toml` has them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct StartSettings {
    /// `spawn.pool_root`; absent when unset.
    pub pool_root: Option<String>,
    /// `spawn.checkouts_root`; absent when unset.
    pub checkouts_root: Option<String>,
    /// Where each project's orchestrator home sits (`spawn.orchestrator_home`
    /// or its default). Derived: the board shows it, and does not set it.
    pub orchestrator_home: Option<String>,
    /// The run scratch root (`spawn.scratch_root` or its default). Derived.
    pub scratch_root: Option<String>,
    /// What a start would refuse about the host-level settings. A project's
    /// checkout is judged with the project.
    pub problems: Vec<SettingProblem>,
    /// Why `config.toml` could not be read; the rest is then absent.
    pub config_error: Option<String>,
}

/// `GET /api/v1/host-config`: the other host keys the board sets, as this
/// host's `config.toml` has them now, with the default each key falls back
/// to. `owner_guard.decider_key` is never here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostConfig {
    /// `herdr.terminal`; absent when unset, so Open in herdr only focuses.
    pub herdr_terminal: Option<String>,
    /// `stop_hook_mode`: `off`, `observe` or `block-once`. An unset or
    /// unknown value reads as `observe`.
    pub stop_hook_mode: String,
    /// `federation.accept`: unset reads as true.
    pub accept_peers: bool,
    /// `memory.remote`; absent or empty keeps memory on this host.
    pub memory_remote: Option<String>,
    /// `personal` or `work`. A work host never pushes its memory repo.
    pub boundary: Option<String>,
    pub learning: LearningConfig,
    /// Why `config.toml` could not be read; the rest is then the defaults.
    pub config_error: Option<String>,
}

/// The `[learning]` keys with their defaults (`learning-loop.md` section 10).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LearningConfig {
    /// One routine card in this many gets a full retro.
    pub sample_every: i64,
    /// Cards a kept change set is measured over.
    pub window_cards: i64,
    pub curator_model: String,
    pub curator_timeout_minutes: i64,
    /// The checkout that holds `evals/run.sh`; absent when unset.
    pub evals_dir: Option<String>,
}

/// `POST /api/v1/config`: `tiller config set` for the settings an
/// orchestrator start needs and the host keys in `GET /api/v1/host-config`,
/// and no other key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetConfig {
    pub idempotency_key: String,
    /// Always refused with a peer label: a setting is written on the host
    /// that owns it (`protocol.md` 7.4).
    #[serde(default)]
    pub host: Option<String>,
    /// A start setting (`checkouts.<project>`, `spawn.pool_root` or
    /// `spawn.checkouts_root`) or one of the keys `GET /api/v1/host-config`
    /// shows. Any other key is refused.
    pub key: String,
    /// A full path for a start setting, the key's own text otherwise
    /// (`true`, `block-once`, `20`).
    pub value: String,
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

/// `GET /api/v1/projects/{id}`: the fields `tiller project edit` sets, for
/// one of this host's projects, with the `rev` an edit is sent against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ProjectFields {
    pub id: String,
    pub name: String,
    /// The GitHub repo, `owner/name`; absent for a project with none.
    pub repo: Option<String>,
    pub remote: Option<String>,
    pub default_branch: String,
    pub delivery: String,
    /// A list of commands, or `{"full": [..], "no_logic": [..]}`.
    pub checks: Value,
    pub capacity: i64,
    pub deploy_workflow: Option<String>,
    pub deploy_environment: Option<String>,
    pub deploy_timeout_minutes: i64,
    pub rev: i64,
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

/// `POST /api/v1/bug-project`: pick or clear the project `tiller bug file`
/// routes to on this host, as `tiller host edit --bug-project` does. Local to
/// this host; it is not forwarded to a peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PickBugProject {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a bug project pick is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// An open project this host owns, never `fleet`; null clears it.
    pub project: Option<String>,
    /// This host's `rev`, from its Machines row.
    pub rev: i64,
}

/// `GET /api/v1/host`: the fields `tiller host edit` sets on this host, what
/// it runs and has free now, and the `rev` an edit is sent against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HostFields {
    pub id: String,
    pub label: String,
    /// `personal` or `work`.
    pub boundary: Option<String>,
    /// What already records the boundary, so it stays: `memory`, `a backup`
    /// or `an export`. Null while the owner may still change it.
    pub boundary_recorded_by: Option<String>,
    /// How many agents may run at once.
    pub lane_capacity: Option<i64>,
    /// No new agent starts while free RAM is under it; null for no floor.
    pub ram_floor_mb: Option<i64>,
    /// No new agent starts while free disk is under it; null for no floor.
    pub disk_floor_mb: Option<i64>,
    /// Lanes held now: one per agent a spawn reserved, until it ends. A
    /// new agent starts only while this is under `lane_capacity`.
    pub lanes_busy: i64,
    /// Free RAM now; null when it cannot be read.
    pub ram_free_mb: Option<i64>,
    /// Free disk under the Tiller home now; null when it cannot be read.
    pub disk_free_mb: Option<i64>,
    pub rev: i64,
}

/// A field that may be absent, null or a value: `Some(None)` is a null the
/// caller sent, which clears the field.
fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// `PATCH /api/v1/host`: edit this host's label, capacity, floors or
/// boundary against its `rev`, as `tiller host edit` does. Local to this
/// host; it is not forwarded to a peer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EditHost {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a host edit is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
    /// This host's `rev`, from `GET /api/v1/host`.
    pub rev: i64,
    /// A word: a letter or digit, then letters, digits, `_` or `-`. Refused
    /// while a peer is paired, since each peer knows this host by it.
    #[serde(default)]
    pub label: Option<String>,
    /// At least 1.
    #[serde(default)]
    pub lane_capacity: Option<i64>,
    /// MB; null clears the floor.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<i64>)]
    pub ram_floor_mb: Option<Option<i64>>,
    /// MB; null clears the floor.
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<i64>)]
    pub disk_floor_mb: Option<Option<i64>>,
    /// `personal` or `work`. Refused once memory, a backup or an export
    /// records the boundary.
    #[serde(default)]
    pub boundary: Option<String>,
}

/// The owner's way past a refused delete.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeleteUnblock {
    /// What to do, as a short imperative ("cancel its card").
    pub step: String,
    /// The command that starts it, when there is one. The CLI prints it;
    /// the board reads `action` instead.
    pub command: Option<String>,
    /// The same step as an action the board takes through its own route,
    /// when it is one.
    pub action: Option<DeleteAction>,
}

/// A step past a refused delete as the board takes it: `agent.stop` on a
/// session (`POST /api/v1/agents/{session}/stop`) or `card.cancel` on a
/// card's display id (`POST /api/v1/cards/{id}/cancel`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeleteAction {
    pub id: DeleteActionId,
    /// The session for `agent.stop`, the card's display id for
    /// `card.cancel`.
    pub target: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum DeleteActionId {
    #[serde(rename = "agent.stop")]
    AgentStop,
    #[serde(rename = "card.cancel")]
    CardCancel,
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

/// `GET /api/v1/recap`: what happened since the owner last marked a recap
/// seen (the Inbox's "Since you left" card).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RecapResponse {
    /// When the owner last marked a recap seen; null before the first.
    pub seen_at: Option<String>,
    /// The start of the window counted: `seen_at`, or a day back.
    pub since: String,
    /// Cards that closed landed.
    pub landed: i64,
    /// Decisions opened for the owner.
    pub decisions: i64,
    /// Runs that ended failed.
    pub failed_runs: i64,
    /// Memory proposals made.
    pub suggestions: i64,
}

/// `POST /api/v1/recap/seen`: moves the recap mark to now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkRecapSeen {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: the mark is this daemon's.
    #[serde(default)]
    pub host: Option<String>,
}

/// The card a reply links to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReplyCard {
    pub id: String,
    pub display_id: String,
    pub title: String,
    /// How the ask touched the card: `created` or `followup`; null for a
    /// comment's card or a card named with `--card`.
    pub relation: Option<String>,
}

/// One reply from the orchestrator, with the question it answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReplyItem {
    pub id: String,
    pub project: String,
    /// `ask` or `comment`.
    pub to: String,
    pub ask_id: Option<String>,
    pub message_id: Option<String>,
    /// The owner's words: the ask, or the comment without its card tag.
    pub question: String,
    pub asked_at: String,
    pub body: String,
    pub replied_at: String,
    pub read_at: Option<String>,
    pub rev: i64,
    pub card: Option<ReplyCard>,
}

/// `GET /api/v1/projects/{id}/replies`: the unread replies, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RepliesResponse {
    pub replies: Vec<ReplyItem>,
}

/// `POST /api/v1/replies/{id}/read`: marks a reply read. Marking it again
/// changes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkReplyRead {
    pub idempotency_key: String,
    /// A paired peer's label. Refused: a reply is read on the daemon that
    /// holds it, not forwarded.
    #[serde(default)]
    pub host: Option<String>,
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

/// `POST /api/v1/cards/{id}/deps` and `DELETE /api/v1/cards/{id}/deps`: the
/// card starts or stops waiting on another (`state.md` section 8). With a
/// peer `host`, forwarded to the host that owns both cards.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DepBody {
    pub idempotency_key: String,
    /// A paired peer's card: the change is forwarded to that host.
    #[serde(default)]
    pub host: Option<String>,
    /// The card waited on: a display id, ULID or short form.
    pub depends_on: String,
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
    /// At most 20,000 characters (`protocol.md` 7.4).
    #[schema(max_length = 20000)]
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
    /// `close` only, and required there: the outcome, such as `completed`,
    /// `abandoned` or `superseded`.
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
    /// The owner's exact words, at most 20,000 characters (`protocol.md`
    /// 7.4).
    #[schema(max_length = 20000)]
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

/// `POST` and `PUT /api/v1/away`: the scope `tiller away start` takes. `POST`
/// turns away mode on, `PUT` changes the running contract in place.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AwayScopeBody {
    pub idempotency_key: String,
    /// When away mode ends by itself: RFC 3339 with an offset, a date (09:00
    /// Lisbon) or a date and time without an offset (Lisbon).
    pub until: String,
    /// The projects the scope covers, by id.
    pub projects: Vec<String>,
    /// Standing orders the orchestrators read.
    #[serde(default)]
    pub orders: Option<String>,
    /// No run starts once this many have started since the owner left.
    #[serde(default)]
    pub max_new_runs: Option<i64>,
    /// The weekly quota floor while away, 0 to 100.
    #[serde(default)]
    pub quota_floor_percent: Option<f64>,
    /// `"none"`, `"grants"` or a grant conditions object.
    #[serde(default)]
    pub merge: Option<Value>,
}

/// `DELETE /api/v1/away`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AwayEndBody {
    pub idempotency_key: String,
    /// Why away mode ended; "ended by the owner" when absent.
    #[serde(default)]
    pub reason: Option<String>,
}

/// `GET /api/v1/away/brief`: the return brief of the newest away contract,
/// or `null` when the owner has never been away. `brief.away` says whether
/// the contract still binds.
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
pub struct AwayBriefResponse {
    #[schema(value_type = Option<Object>)]
    pub brief: Option<tiller_agents::handoff::ReturnBrief>,
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
    /// A paired peer's label: the peer starts its own orchestrator
    /// (`protocol.md` 7.4).
    #[serde(default)]
    pub host: Option<String>,
    /// Stop the live orchestrator first and start a new one, as `tiller
    /// orchestrator start --restart` does: only on the owner's explicit
    /// Restart, and never for a peer's (its forwarded owner may not).
    #[serde(default)]
    pub restart: bool,
}

/// `POST /api/v1/agents/restart-stale`: restarts the sessions that run an
/// older tiller. An idle one restarts now; a working one is queued and
/// restarts when it next goes idle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestartStale {
    pub idempotency_key: String,
    /// Only these sessions, the ones the person saw; all stale sessions when
    /// absent.
    #[serde(default)]
    pub sessions: Option<Vec<String>>,
    /// A paired peer's label: the peer restarts its own sessions
    /// (`protocol.md` 7.4).
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/pollers/{source}/run`: poll that source now (the owner's
/// Run now on a failing poller's check line).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RunPoller {
    pub idempotency_key: String,
    /// Always refused with a peer label: each host runs its own pollers
    /// (`protocol.md` 7.4).
    #[serde(default)]
    pub host: Option<String>,
}

/// What a restart did to each session it covered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RestartStaleResponse {
    pub sessions: Vec<RestartResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RestartResult {
    pub session_id: String,
    /// `restarted`, `queued` (it restarts when it next goes idle) or
    /// `failed`.
    pub outcome: String,
    pub error: Option<String>,
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

/// `POST /api/v1/agents/{session}/terminal/history`: how much of the
/// pane's scrollback to read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalHistoryAsk {
    /// 1 to 1000 rendered rows, counted from the bottom of the screen;
    /// absent means 1000.
    #[serde(default)]
    pub lines: Option<u16>,
    /// A paired peer's label; a history read is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// The pane's recent rows as herdr renders them: wrapped at the pane's
/// width, oldest first, the screen itself last, with their colors as ANSI
/// escapes and rows split by `\n`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TerminalHistory {
    pub session_id: String,
    pub text: String,
}

/// `POST /api/v1/agents/{session}/terminal/tail`: nothing to ask beyond
/// the agent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalTailAsk {
    /// A paired peer's label; a tail read is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// The pane's visible screen as herdr renders it, one row per line split by
/// `\n`, colors as ANSI escapes, and the pane's size in cells.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TerminalTail {
    pub session_id: String,
    pub cols: u16,
    pub rows: u16,
    pub text: String,
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

/// `POST /api/v1/agents/{session}/prompt`: nothing to ask beyond the agent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptAsk {
    /// A paired peer's label; a prompt read is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// The dialog on a waiting agent's screen, or why there is none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PromptRead {
    pub session_id: String,
    pub read_at: String,
    pub prompt: Option<PromptDialog>,
    /// Why `prompt` is null; null with a prompt.
    pub reason: Option<NoPrompt>,
}

/// Why a prompt read found no dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoPrompt {
    /// The agent waits on neither a permission prompt nor an answer; its
    /// screen is not read.
    NotWaiting,
    /// The screen holds no dialog this daemon reads.
    NoDialog,
}

/// A dialog as Claude Code draws it, read off the agent's screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PromptDialog {
    /// Which of the layouts the daemon reads this is.
    pub layout: PromptLayout,
    /// The tool the session's hook named; the dialog's title is one Claude
    /// Code gives it. Empty for a dialog no tool asks, such as `trust`.
    pub tool: String,
    pub title: String,
    /// The command a Bash dialog shows; null for other tools.
    pub command: Option<String>,
    /// The folder a `trust` dialog asks about; null for other layouts.
    pub folder: Option<String>,
    pub question: String,
    /// In screen order, top to bottom.
    pub options: Vec<PromptOption>,
    /// A hash of the dialog's rows: an answer carries it back, and the
    /// daemon types nothing when the dialog on the screen no longer matches.
    pub screen: String,
}

/// The dialog layouts the daemon reads (`protocol.md` 7.4, Prompt).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptLayout {
    /// The numbered permission dialog: an option is its digit.
    Permission,
    /// The folder trust dialog, a cursor menu with no digits: an option is
    /// the arrow keys to its row, then Enter.
    Trust,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PromptOption {
    pub n: u8,
    pub text: String,
}

/// `POST /api/v1/agents/{session}/prompt/answer`: the option to press, on
/// the dialog `screen` names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PromptAnswer {
    pub option: u8,
    /// The `screen` of the prompt read the buttons were drawn from.
    pub screen: String,
    /// A paired peer's label; an answer is not forwarded.
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

/// `POST /api/v1/agents/{session}/steer`: one line for the agent, typed
/// into its pane when the pane is idle (`protocol.md` 3.3, 7.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SteerBody {
    pub idempotency_key: String,
    /// One line, at most 20,000 characters.
    pub text: String,
    /// A paired peer's label; a steer is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/agents/{session}/interrupt`: Escape to a working agent's
/// pane, and whether its turn ended within 10 s (`protocol.md` 6.4, 7.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InterruptBody {
    pub idempotency_key: String,
    /// A paired peer's label; an interrupt is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/agents/{session}/stop` and `.../resume`: `tiller agent
/// stop` and `tiller agent resume` through the commands' own path
/// (`protocol.md` 6.4, 7.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentControlBody {
    pub idempotency_key: String,
    /// A paired peer's label; a stop or a resume is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/agents/{session}/pause`: `tiller run pause` on the
/// session's run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PauseBody {
    pub idempotency_key: String,
    /// RFC 3339, in the future.
    pub until: String,
    /// Why the run waits, at most 20,000 characters.
    pub reason: String,
    /// A paired peer's label; a pause is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /api/v1/agents/{session}/relaunch`: `tiller agent relaunch` on
/// the session's run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RelaunchBody {
    pub idempotency_key: String,
    /// The progress the new session starts from, at most 20,000
    /// characters.
    pub note: String,
    /// End the run (`relaunched`) and start attempt n+1 on the run's own
    /// brief.
    #[serde(default)]
    pub new_attempt: bool,
    /// A paired peer's label; a relaunch is not forwarded.
    #[serde(default)]
    pub host: Option<String>,
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
