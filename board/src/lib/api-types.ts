// Types for the board API in docs/spec/protocol.md section 7. They are written
// by hand until `contract/openapi.json` (T2.8) exists; then this file is
// replaced by types generated from it and only the names below must survive.

// How far a card's work has come (state.md, section 6). A card only moves right.
type CardColumn = "working" | "landing" | "queued" | "done";

export type BoardFilter = "all" | "active" | "needs";

// What a federated read adds to a row (protocol.md section 7.3): the owning
// host's label, and for a peer's row when the board host last heard it and the
// link state it is in. Absent on a host with no peers.
interface Federated {
  host?: string;
  as_of?: string | null;
  link?: LinkState | null;
}

// One `card_column` row plus the card's own display fields.
export interface BoardRow extends Federated {
  id: string;
  display_id: string;
  project_id: string;
  host_id: string | null;
  kind: string;
  title: string;
  // 0 is the most urgent, 4 the least (state.md, Card).
  priority: number;
  column: CardColumn;
  badge: string;
  // The card waits on the owner, in whatever column it is: the orange bar
  // and the Needs you filter.
  needs_you: boolean;
  parked_until: string | null;
  parked_reason: string | null;
  blocked_by: string | null;
  closed_at: string | null;
  rev: number;
  // Display fields the board joins onto the row (T3.2); absent on a card that
  // has none yet.
  model?: string | null;
  pr_number?: number | null;
  // When the newest observation of the card's active run was made.
  observed_at?: string | null;
  // `tiller-bug` for a card `tiller bug file` made (state.md, Card); absent or
  // null for any other card.
  category?: string | null;
  // The project the filer worked in, when `tiller bug file` routed the card
  // out of it.
  filed_from?: string | null;
}

export interface AskedRow {
  id: string;
  project_id: string;
  session_id: string | null;
  received_at: string;
  age_seconds: number;
  // Where the ask came from: typed in an agent's pane (`prompt`), the board's
  // ask box, a script or an import. Absent on a row from an older peer.
  source?: AskSource;
  // The ask's first line, at most 200 characters. Only on this host's rows:
  // a peer link carries no ask text.
  summary?: string;
}

export type AskSource = "prompt" | "board" | "script" | "import";

export interface BoardResponse {
  rows: BoardRow[];
  asked: AskedRow[];
}

// An owner-only change an option applies when the owner picks it, in the
// same write as the answer (state.md, Attention). Capacity carries the value
// the question saw, so a moved value refuses the pick.
export interface OwnerAction {
  kind: "project_capacity";
  project: string;
  from: number;
  to: number;
}

// One answer the asker offers, with what it sets in motion.
interface AttentionOption {
  label: string;
  consequence: string | null;
  recommended: boolean;
  action?: OwnerAction;
}

// The approval a `merge` item asks for, bound to the head the board shows.
interface ApprovalBinding {
  id: string;
  kind: "merge" | "land_direct";
  head_sha: string;
  pr_number: number | null;
  state: string;
}

export interface StaleInfo {
  reason: "card_closed" | "run_ended" | "session_ended" | "project_deleted" | "project_retired";
  subject: string | null;
  at: string | null;
}

export interface AttentionItem extends Federated {
  id: string;
  card_id: string;
  kind: "decision" | "credential" | "merge" | "review" | "recovery" | "report";
  question: string;
  // Text, or a JSON list of pointers (event ids, artifact ids, PR urls).
  evidence: string | null;
  recommendation: string | null;
  consequence: string | null;
  options: AttentionOption[] | null;
  approval: ApprovalBinding | null;
  opened_by_session_id: string | null;
  opened_at: string;
  still_current: boolean;
  // Why the question no longer applies; absent while it does.
  stale?: StaleInfo | null;
  // What the question is about. Soft ids: they outlive a deleted project.
  subject_session_id?: string | null;
  subject_project_id?: string | null;
  answered_at: string | null;
  withdrawn_at: string | null;
  rev: number;
}

export interface PermissionPrompt extends Federated {
  session_id: string;
  card_id: string;
  tool: string;
  // What the tool wants to run, from the session's newest observation.
  detail: string | null;
  since: string;
}

// The permission dialog on a waiting agent's screen (protocol.md 7.4,
// Prompt): `screen` goes back with an answer, so a changed dialog is refused.
export interface PromptDialog {
  tool: string;
  title: string;
  command: string | null;
  question: string;
  options: { n: number; text: string }[];
  screen: string;
}

export interface PromptRead {
  session_id: string;
  read_at: string;
  prompt: PromptDialog | null;
  reason: "not_waiting" | "no_dialog" | null;
}

// `POST /api/v1/agents/{session}/steer` (protocol.md 7.4): one line, queued
// in the outbox and typed when the pane is idle.
export interface SteerBody {
  idempotency_key: string;
  text: string;
}

// `data` of a steer: the queued outbox message.
export interface SteerQueued {
  id: string;
  target_session_id: string;
  kind: "steer";
  state: string;
  body: string;
}

// `data` of `POST /api/v1/agents/{session}/interrupt` (protocol.md 6.4, 7.4):
// `confirmed` only when a hook or the transcript showed the turn end within
// 10 s.
export interface Interrupted {
  run: string;
  session: string;
  pane_id: string;
  cancel: "confirmed" | "unconfirmed";
  evidence: string;
}

export interface AttentionResponse {
  items: AttentionItem[];
  permission_prompts: PermissionPrompt[];
}

export interface AgentRow extends Federated {
  session_id: string;
  role: string;
  model: string;
  activity: string;
  activity_observed_at: string;
  last_status_line: string | null;
  herdr_name: string | null;
  card_id: string | null;
  host_id: string;
  workers: AgentRow[];
  // The project the session works for, the active run (for `GET /runs/{id}`)
  // and herdr's pane and workspace label (protocol.md section 7.3).
  project_id?: string;
  run_id?: string | null;
  pane_id?: string | null;
  workspace_label?: string | null;
  // When the session entered its current state.
  state_since?: string | null;
  // When the agent last produced output: a tool call, a subagent or the end
  // of a turn (protocol.md section 7.3).
  last_output_at?: string | null;
  // Set when the session runs an older tiller than the daemon: the version it
  // started on ("unknown" with no record), whether its role prompt changed,
  // and whether a restart waits for it to go idle.
  stale?: SessionStale | null;
}

export interface SessionStale {
  since: string;
  prompt_changed: boolean;
  restart_queued: boolean;
}

export interface AgentsResponse {
  orchestrators: AgentRow[];
  // The version the running daemon would launch a session on.
  tiller_version?: string;
}

// `POST /api/v1/agents/restart-stale`: what happened to each session asked.
export interface RestartStaleResponse {
  sessions: {
    session_id: string;
    outcome: "restarted" | "queued" | "failed";
    error?: string | null;
  }[];
}

// `GET /api/v1/runs/{id}`: run detail with recent events and a pane tail.
export interface EventLine {
  at: string;
  source: string;
  text: string;
  tone?: "warn" | "bad";
}

export interface RunDetail {
  id: string;
  card_id: string;
  branch: string | null;
  worktree: string | null;
  started_at: string;
  ended_at: string | null;
  events: EventLine[];
}

// `GET /api/v1/cards/{id}`: the card with everything the detail page shows
// (protocol.md section 7.3).
export interface CardRun {
  id: string;
  number: number;
  role: string;
  model: string;
  started_at: string;
  ended_at: string | null;
  outcome: "passed" | "failed" | "cancelled" | null;
  note: string | null;
  session_id: string | null;
  host_id: string | null;
  activity: string | null;
  activity_observed_at: string | null;
}

export interface CardPr {
  id: string;
  number: number;
  repo: string;
  // The PR's page on the forge.
  url: string;
  title: string;
  state: "open" | "merged" | "closed";
  head_sha: string;
  base: string;
  checks: "pending" | "passing" | "failing";
  checks_summary: string | null;
  diff_summary: string | null;
}

export interface CardApproval {
  id: string;
  kind: string;
  // requested, accepted, rejected, executing, succeeded, failed, uncertain or
  // stale (schema.sql, `approvals.state`).
  state: string;
  pr_id: string | null;
  // The commit the approval is bound to.
  head_sha: string | null;
  decided_at: string | null;
  decided_by: string | null;
  // Commits pushed after the approval was bound, newest first.
  newer_commits: { sha: string; subject: string; at: string }[];
}

interface Freshness {
  source: string;
  detail: string;
  observed_at: string | null;
}

// A page, screenshot or report made for the card. `url` is where a page is
// served; an artifact that is only a local file has none.
export interface CardArtifact {
  id: string;
  kind: string;
  title: string;
  url: string | null;
  created_at: string;
}

// One dependency of a card (state.md, Dependency): open until the card waited
// on closes with a satisfying outcome (`release_reason` is that outcome) or it
// is removed by hand (`removed`). The waited-on card's title, column and badge
// are absent from a peer that predates them.
export interface CardDep {
  display_id: string;
  released_at: string | null;
  release_reason?: string;
  title?: string;
  column?: CardColumn;
  badge?: string;
}

export interface CardDetail {
  card: {
    id: string;
    display_id: string;
    project_id: string;
    kind: string;
    title: string;
    body: string | null;
    priority: number;
    slug: string;
    state: "open" | "closed";
    outcome: string | null;
    model: string | null;
    branch: string | null;
    worktree: string | null;
    rev: number;
  };
  column: BoardRow;
  holds: { id: string; until_at: string | null; reason: string | null }[];
  deps: CardDep[];
  // Every card that waits on this one, directly or through others: this card
  // cannot wait on any of them, since that would make a loop. Absent from a
  // peer that predates it.
  waiting_on_this?: string[];
  attention: AttentionItem[];
  runs: CardRun[];
  prs: CardPr[];
  approvals: CardApproval[];
  artifacts: CardArtifact[];
  reviews: { id: string; verdict: string; at: string }[];
  obligations: { id: string; summary: string; state: string }[];
  events: EventLine[];
  freshness: Freshness[];
}

// `GET /api/v1/cards/{id}/timeline`: newest first, paged by `before`.
export interface TimelineEntry {
  seq: number;
  at: string;
  actor: "you" | "orchestrator" | "hook" | "system";
  text: string;
}

export interface TimelineResponse {
  entries: TimelineEntry[];
  next_before: number | null;
}

export type MachineHealth = "ok" | "degraded" | "down" | "stale";

export type StaleReason =
  | "herdr_link_down"
  | "host_asleep"
  | "poll_failed"
  | "never_seen"
  | "peer_link_down"
  | "peer_refused"
  | "peer_version";

// A paired peer's link (protocol.md section 10.2) and what the pairing allows.
export type LinkState = "live" | "lagging" | "down" | "refused" | "never";
export type PeerScope = "read" | "act";

export interface Machine {
  id: string;
  label: string;
  kind: "local" | "remote";
  view_only: boolean;
  health: MachineHealth;
  running_agents: number | null;
  capacity: number | null;
  free_slots: number | null;
  last_seen_at: string | null;
  stale_reason: StaleReason | null;
  // The `tiller version` string the host runs (`0.1.0-pre.17`); absent for a
  // herdr-only machine and for a peer that has not reported one.
  version?: string | null;
  // For a paired peer (section 9): the link state and the pairing's scope; null
  // or absent for this host and for a herdr-only machine.
  link?: LinkState | null;
  peer_scope?: PeerScope | null;
  // Why the peer refused the link (`revoked`, `unknown_key`, `bad_sig`,
  // `clock_skew`, `replayed`, `version`, `accept_off`), when it did.
  link_reason?: string | null;
  // The project that takes Tiller bugs on this host (`bugs.project`), when one
  // is set and still open; absent for a herdr-only machine.
  bug_project?: string | null;
  rev: number;
}

export interface MachinesResponse {
  machines: Machine[];
}

export type QuotaDot = "green" | "amber" | "red" | "grey";

export interface QuotaWindow {
  label: string;
  title: string;
  kind: string;
  percent_remaining: number | null;
  resets_at: string | null;
  pace: string | null;
  reserve: number | null;
  burn_multiple: number | null;
  elapsed_percent: number | null;
  projected_exhaustion_at: string | null;
  fetched_at: string;
  stale: boolean;
  dot: QuotaDot;
}

export interface QuotaResponse {
  windows: QuotaWindow[];
}

export interface Project extends Federated {
  id: string;
  name: string;
  delivery: string;
  capacity: number;
  rev: number;
  stage: "pre-launch" | "launched";
  // What builders follow: `direct-pr` while a no-mistakes project is pre-launch.
  effective_delivery: string;
  // The last commit a batched review covered, and when that pass finished.
  reviewed_through_sha: string | null;
  reviewed_at: string | null;
  stage_changed_at: string | null;
  // The next weekly review, only while pre-launch.
  next_review_due: string | null;
  // A weekly review came due 24 hours ago or more and no pass started.
  review_overdue: boolean;
  // A review pass is pending or running.
  review_open: "weekly" | "final" | null;
  // When it was archived; only on the archived list (`?retired=true`).
  retired_at?: string;
}

export interface ProjectsResponse {
  projects: Project[];
}

// A PR published from a card (`GET /api/v1/contributions`).
export interface Contribution {
  id: string;
  project_id: string;
  card_id: string;
  card_display_id: string;
  card_title: string;
  repo: string;
  number: number;
  url: string;
  // open, merged or closed.
  state: string;
  linked_at: string;
  merged_at: string | null;
}

export interface ContributionsResponse {
  contributions: Contribution[];
}

// What deleting a project would remove (state.md, Deleting a project): rows
// per table, the `table.column` references elsewhere that are set to null, and
// why the delete is refused right now, if it is, with the owner's way past it.
export interface ProjectDeletePreview {
  project: string;
  removed: Record<string, number>;
  unlinked: Record<string, number>;
  refused: string | null;
  unblock: DeleteUnblock | null;
  // The running agents the delete stops first, the orchestrator first.
  agents: ProjectAgent[];
  // Worktrees with work not landed; the delete leaves them on disk.
  unlanded: ProjectUnlanded[];
  // Pull requests still open; the delete leaves them on GitHub.
  open_prs: ProjectOpenPr[];
}

// GET /api/v1/projects/{id}/setup: what `tiller doctor`'s merge_gates line and
// `tiller config get checkouts.<id>` show for one of this host's projects.
export interface ProjectSetup {
  project: string;
  repo: string | null;
  merge_gate: "enforced" | "detect";
  merge_gate_changed_at: string | null;
  checkout: string | null;
  // Why config.toml could not be read; `checkout` is then null.
  config_error: string | null;
}

// The commands a direct-push builder runs before it reports done: a list, or a
// full set with a lighter one for a change that touches no logic. A missing
// `no_logic` means the full set applies to every change.
export type ProjectChecks = string[] | { full?: string[]; no_logic?: string[] };

// GET /api/v1/projects/{id}: the fields `tiller project edit` sets, and the
// `rev` an edit is sent against.
export interface ProjectFields {
  id: string;
  name: string;
  repo: string | null;
  remote: string | null;
  default_branch: string;
  delivery: string;
  checks: ProjectChecks | null;
  capacity: number;
  deploy_workflow: string | null;
  deploy_environment: string | null;
  deploy_timeout_minutes: number;
  rev: number;
}

// `GET /api/v1/projects/checkout`: what Add project would do with a local
// checkout. Either `problem` is set and the rest is null, or the reverse.
export interface CheckoutCheck {
  // The full folder, with `~` expanded and an empty path read as the default.
  path: string | null;
  plan: "use" | "clone" | null;
  // The URL a clone takes.
  remote: string | null;
  problem: string | null;
}

// One setting an orchestrator start would refuse (protocol.md section 6.4).
export interface SettingProblem {
  // `checkouts.<id>`, `spawn.pool_root` or `spawn.checkouts_root`.
  key: string;
  // `missing`: not in config.toml. `invalid`: set, and refused.
  kind: "missing" | "invalid";
  // What a start would say about it.
  problem: string;
}

// `GET /api/v1/start-settings`: the host-level settings a start reads.
export interface StartSettings {
  pool_root: string | null;
  checkouts_root: string | null;
  // Derived from the two roots and read-only: the orchestrator home and the
  // scratch folder.
  orchestrator_home: string | null;
  scratch_root: string | null;
  problems: SettingProblem[];
  // Set when config.toml cannot be read at all.
  config_error: string | null;
}

// `GET /api/v1/host-config`: the other host keys the board sets, as this
// host's config.toml has them now, with the default each key falls back to.
// `owner_guard.decider_key` is never here.
export interface HostConfig {
  // Null when unset: Open in herdr only focuses.
  herdr_terminal: string | null;
  // `off`, `observe` or `block-once`; unset reads as `observe`.
  stop_hook_mode: string;
  // Unset reads as true.
  accept_peers: boolean;
  // Null or empty keeps memory on this host.
  memory_remote: string | null;
  // `personal` or `work`; a work host never pushes its memory repo.
  boundary: string | null;
  learning: LearningConfig;
  // Set when config.toml cannot be read; the rest is then the defaults.
  config_error: string | null;
}

interface LearningConfig {
  sample_every: number;
  window_cards: number;
  curator_model: string;
  curator_timeout_minutes: number;
  evals_dir: string | null;
}

// `GET /api/v1/projects/{id}/retire`: whether an archive applies now, and the
// running agents it can stop first.
export interface ProjectRetirePreview {
  project: string;
  refused: string | null;
  agents: ProjectAgent[];
}

// A running agent of a project: a live session, or an active run with none.
export interface ProjectAgent {
  role: string;
  session: string | null;
  // The worker's card display id; null for an orchestrator.
  card: string | null;
  run: string | null;
  // Only this host can stop it.
  local: boolean;
}

export interface ProjectUnlanded {
  path: string;
  branch: string | null;
  card: string | null;
  unlanded_commits: number | null;
  dirty: boolean | null;
}

interface ProjectOpenPr {
  card: string;
  number: number;
  url: string;
}

// The step that clears a refused delete, and the command that starts it when
// there is one.
interface DeleteUnblock {
  step: string;
  command: string | null;
}

// The change events on `GET /api/v1/events` (protocol.md section 7.5).
export interface ChangeEvent {
  seq: number;
  at: string;
  kind: string;
  txn: string;
  entity: string;
  entity_id: string;
  rev_after: number | null;
  card_id: string | null;
  entity_state: unknown;
  card?: BoardRow;
}

// `event: peer`: a paired peer's change, tagged as that peer's. It has no SSE
// `id`, since a peer's `seq` is not this host's (protocol.md section 7.5).
export interface PeerEvent {
  host: string;
  host_id: string;
  as_of: string | null;
  change: ChangeEvent;
}

// `event: link`: a peer link changed state.
export interface LinkEvent {
  host: string;
  link: LinkState;
  as_of: string | null;
  stale_reason: StaleReason | null;
}

// `event: peer_reset`: the peer's rows by snapshot path, which replace what the
// board holds for that host.
export interface PeerResetEvent {
  host: string;
  host_id: string;
  link: LinkState;
  as_of: string | null;
  queries: Record<string, unknown>;
}

// The receipt every mutation returns (protocol.md section 7.2). Recorded is
// this response; delivered and done follow over SSE from the outbox row or the
// approval it names.
export interface Receipt {
  recorded_at: string;
  outbox_id: string | null;
  approval_id: string | null;
}

// `data` of `POST /api/v1/projects/{id}/asks` (protocol.md section 7.4). The
// warning is `no_orchestrator` when nobody was told: the ask is still recorded.
export interface AskRecorded {
  ask: {
    id: string;
    project_id: string;
    session_id: string | null;
    source: string;
    text: string;
    received_at: string;
  };
  message: { id: string; target_session_id: string } | null;
  warning: string | null;
}

// `GET /api/v1/recap`: what happened since the owner last marked a recap seen.
// The window starts at `seen_at`, or a day back before the first mark.
export interface RecapResponse {
  seen_at: string | null;
  since: string;
  landed: number;
  decisions: number;
  failed_runs: number;
  suggestions: number;
}

export interface MutationResponse<T = unknown> {
  ok: true;
  replayed: boolean;
  events: number[];
  receipt: Receipt;
  data: T;
  // What the owner should know about a mutation that applied, such as why a
  // re-added project's cards do not start at 1.
  warning?: string;
}

export interface ApiErrorBody {
  ok: false;
  error: {
    code: string;
    message: string;
    entity?: string;
    id?: string;
    current?: Record<string, unknown>;
    hint?: string;
    // The peer that answered or refused a forwarded mutation (protocol.md 10.3).
    host?: string;
  };
}

// The inlined `<script id="tiller-boot">` body and `GET /api/v1/snapshot`.
export interface SnapshotPeer {
  host: string;
  host_id: string;
  link: LinkState;
  as_of: string | null;
  server_time_offset_ms: number;
  scope: PeerScope;
  last_event_seq: number | null;
}

export interface Snapshot {
  v: 1;
  server_time: string;
  last_event_seq: number;
  thresholds: Record<string, unknown>;
  queries: Record<string, unknown>;
  // One entry per paired peer; absent on a host with none (section 7.6).
  peers?: SnapshotPeer[];
}

// `GET /api/v1/signals`: the Health page's counts, one entry per Lisbon week,
// oldest first, the last being the week in progress (protocol.md section 7.3).
// Why the developer was needed. Corrections, unplanned escalations and rework
// are avoidable; planned attention (merges, reviews, reports) is not.
export type InterventionKind = "correction" | "unplanned_escalation" | "rework" | "planned";

export interface InterventionCounts {
  correction: number;
  unplanned_escalation: number;
  rework: number;
  planned: number;
}

// One time the developer was needed, with the label it counts under: the
// developer's own relabel when there is one, else what the rules said.
export interface SignalsIntervention {
  id: string;
  source: "attention" | "note";
  card_id: string;
  display_id: string;
  project_id: string;
  at: string;
  summary: string;
  classified: InterventionKind;
  kind: InterventionKind;
  relabeled: boolean;
}

export interface SignalsWeek {
  start: string;
  end: string;
  landed: number;
  reported: number;
  merged_prs: number;
  needed: number;
  // Items answered in the week: the sample behind the median.
  answered: number;
  median_answer_minutes: number | null;
  interventions: InterventionCounts;
  reverts: number;
  escaped: number;
  rework: number;
  lost_work: number;
  duplicate_dispatches: number;
  unauthorized: number;
  // Mutations the owner made at a terminal through the CLI (authority.md
  // G23). Not an incident: the owner reads them to spot one they did not make.
  cli_decisions: number;
}

// One mutation the owner made through the CLI, and where it came from: what
// its events' `payload.via_terminal` says.
export interface SignalsCliDecision {
  at: string;
  // The mutation's name, like `approval.decide`.
  txn: string;
  display_id: string | null;
  // The terminal device, like `/dev/pts/3`.
  tty: string | null;
  // The parent process's executable name and pid.
  parent: string | null;
  parent_pid: number | null;
}

interface SignalsProject {
  project_id: string;
  accepted: number;
  needed: number;
  avoidable: number;
  escaped: number;
  answered: number;
  median_answer_minutes: number | null;
}

export interface SignalsResponse {
  weeks: SignalsWeek[];
  by_project: SignalsProject[];
  // The latest interventions across the weeks, newest first, to relabel.
  interventions: SignalsIntervention[];
  // The latest CLI decisions across the weeks, newest first, at most 20.
  cli_decisions: SignalsCliDecision[];
  machines_live: number;
  machines_total: number;
  // When the store's first event happened: the day Tiller started on this
  // host. Null on a store with no events.
  first_event_at: string | null;
}

// `GET /api/v1/session`: whether this browser holds a live owner cookie.
export interface SessionView {
  owner: boolean;
}

// `GET /api/v1/health`: the daemon's own state, answered before `tiller init`
// too. Only what the board reads.
export interface DaemonHealth {
  ok: boolean;
  version: string;
  // False until `tiller init` has made state.db.
  initialized: boolean;
  // The schema this binary migrates to.
  schema: number;
  pid: number;
  started_at: string;
  // The merge App key, as `tiller doctor`'s `merge app` line reads it.
  merge_app: MergeAppHealth;
  // Every poller's last attempt and success, by source.
  pollers: PollerHealth[];
  // Tool versions read at start and every 5 minutes, by tool name; null when
  // not found.
  tools: Record<string, string | null> | null;
}

// One poller's record in `/api/v1/health`. A failing poller backs off and
// polls again; it never stops.
export interface PollerHealth {
  source: string;
  last_attempt_at: string | null;
  last_success_at: string | null;
  consecutive_failures: number;
  last_error: string | null;
}

export type CheckStatus = "ok" | "warn" | "fail";

// One of `tiller doctor`'s checks.
export interface DoctorCheck {
  name: string;
  status: CheckStatus;
  // A failed required check fails `tiller doctor`; a failed optional one is
  // a warning.
  required: boolean;
  version?: string;
  detail: string;
  // The case of the owner marker and other-system checks, which the board
  // words itself; absent on every other check.
  code?: string;
}

export interface DoctorGate {
  project: string;
  merge_gate: string;
  status: CheckStatus;
  last_changed_at?: string;
  detail: string;
}

// `GET /api/v1/doctor`: the checks `tiller doctor` runs, run on this host now.
export interface DoctorReport {
  checked_at: string;
  ok: boolean;
  checks: DoctorCheck[];
  merge_gates: DoctorGate[];
}

export interface MergeAppHealth {
  // `missing`: every project runs as detect. `fail`: the key has leaked.
  status: "ok" | "missing" | "warn" | "fail";
  app_id: number | null;
  detail: string;
}

// `GET /api/v1/memory?scope=`: the document list and the chosen document
// (memory.md section 7, protocol.md section 7.3).
export interface MemoryDocSummary {
  // `fleet` for "All projects (shared)".
  project_id: string;
  name: string;
  lines: number;
  // Open agent proposals against it.
  suggestions: number;
}

// The revision that last changed a line.
export interface MemoryProvenance {
  revision_id: string;
  card: string | null;
  // `you`, `carried over`, `outside Tiller` or the proposing session.
  note: string | null;
  at: string;
}

// `learning.gate` on a proposal, with one made against another text refused
// as stale.
interface MemoryGate {
  proposal: string;
  ok: boolean;
  // The failing step, when not ok.
  reason: string | null;
  // The command that clears it.
  hint: string | null;
  change_set: string | null;
  eval_run: string | null;
  // The set that is measuring and blocks every other, with cards to go.
  measuring: {
    change_set: string;
    name: string;
    cards_closed: number;
    window_cards: number;
  } | null;
}

export interface MemorySuggestion {
  proposal_id: string;
  op: "add" | "rewrite" | "retire";
  // The new text, for `add` and `rewrite`.
  text: string | null;
  by: string;
  card: string | null;
  at: string;
  why: string | null;
  // Tokens each role's prompt would carry once it is kept.
  token_budget_after: Record<string, number> | null;
  // The holdout gate's verdict on keeping it; Keep waits until it passes.
  gate: MemoryGate;
  // The current text of the line a rewrite or removal names, while the file
  // has it.
  target: string | null;
  // The document changed after it was proposed: "Re-propose on current text"
  // clears it.
  moved: boolean;
  // When Tiller recorded the text that moved it, if it did.
  moved_at: string | null;
  // A holdout run is the next step: the board offers "Run holdout eval...".
  eval_clears: boolean;
  // The newest holdout run of its change set.
  holdout: MemoryHoldout | null;
}

// The newest holdout run of a suggestion's change set.
export interface MemoryHoldout {
  eval_run: string;
  change_set: string;
  cases_passed: number;
  cases_total: number;
  passed: boolean;
  partial: boolean;
  // The run was made with the set's current members.
  current: boolean;
  ran_at: string;
}

export interface MemoryLine {
  // Its position in the section, as edits name it.
  index: number;
  text: string;
  memory_id: string | null;
  provenance: MemoryProvenance | null;
  // A rewrite or removal of this line.
  suggestion: MemorySuggestion | null;
}

export interface MemorySection {
  title: string;
  class: string;
  lines: MemoryLine[];
  // New lines, and suggestions whose line is gone.
  suggestions: MemorySuggestion[];
}

// One entry in a document's "Change history": a change applied or discarded
// in the last 30 days, newest first, undone or not.
export interface MemoryChange {
  proposal_id: string;
  kind: "added" | "kept" | "edit" | "remove" | "discard";
  text: string | null;
  // The card it came from, else who wrote it.
  from: string;
  // Who wrote it: `you`, `the curator` or the proposing session.
  by: string;
  card: string | null;
  section: string;
  // The text an edit replaced.
  old_text: string | null;
  at: string;
  // When it was undone: its reverse applied, or its discard proposed again.
  undone_at: string | null;
}

export interface MemoryDocument {
  project_id: string;
  name: string;
  path: string;
  // The blob at `HEAD`; every change sends it back as `base_blob`.
  blob: string;
  // The working tree differs from `HEAD`: changes are refused until it is
  // committed or reset.
  dirty: boolean;
  sections: MemorySection[];
  changes: MemoryChange[];
}

export interface MemoryResponse {
  // Fleet first.
  docs: MemoryDocSummary[];
  doc: MemoryDocument;
  // The holdout evals the board started that are running, or that failed
  // since the daemon started.
  evals: MemoryEvalJob[];
}

// A holdout eval the board started.
export interface MemoryEvalJob {
  proposal_id: string;
  change_set: string;
  cases: number;
  started_at: string;
  // The estimated minutes it runs.
  minutes: number;
  // Why it recorded nothing; absent while it runs.
  error: string | null;
  finished_at: string | null;
}

// `GET /api/v1/memory/proposals/{id}/eval`: what the holdout eval will cost,
// and why it will not start, before the owner starts it.
export interface MemoryEvalPlan {
  proposal_id: string;
  // The proposal's open change set, or the one-member set a start makes.
  change_set: string;
  cases: number;
  case_runs: number;
  judge_runs: number;
  model_runs: number;
  // The weekly quota percent it will probably take (an estimate).
  percent: number;
  // The minutes it will probably run (an estimate).
  minutes: number;
  quota: {
    // The weekly window with the least left.
    label: string | null;
    left: number | null;
    // This host's floor for new work; absent with no quota feed.
    floor: number | null;
    left_after: number | null;
    resets_at: string | null;
    fetched_at: string | null;
  };
  // Why a start is refused now; absent when it can start.
  refusal: string | null;
}

// A session that loaded a revision.
interface MemoryLoad {
  session_id: string;
  role: string;
  project_id: string;
  run_id: string | null;
  card: string | null;
  trigger: string;
  loaded_at: string;
}

// `GET /api/v1/memory/revisions/{id}`.
export interface MemoryRevision {
  id: string;
  project_id: string;
  path: string;
  blob_sha: string;
  commit_sha: string | null;
  review: "approved" | "unreviewed" | "revoked";
  provenance: string;
  recorded_at: string;
  approval: Record<string, unknown> | null;
  proposal_id: string | null;
  card: string | null;
  sessions: MemoryLoad[];
  // The file's text at this revision; absent once the blob is gone.
  text: string | null;
}

// What one memory change came to.
export interface MemoryLanded {
  proposal_id: string;
  approval_id: string | null;
  blob: string;
}

// What an away contract allowed on the merge side: `"none"`, `"grants"`, or
// the conditions of a grant that ends with away mode.
export type AwayMerge = "none" | "grants" | Record<string, unknown>;

interface AwayRun {
  run_id: string;
  card: string;
  title: string;
  role: string;
  started_at: string;
  outcome: string | null;
  status: string | null;
}

interface AwayCard {
  card: string;
  title: string;
  state: string;
  outcome: string | null;
}

interface AwayMergeLine {
  card: string;
  action: string;
  pr_url: string | null;
  grant_id: string | null;
  state: string;
  at: string;
}

interface AwayDecision {
  attention_id: string;
  card: string;
  kind: string;
  question: string;
  recommendation: string | null;
  standing: string;
  answer: string | null;
}

interface AwayWaitingApproval {
  approval_id: string;
  card: string | null;
  action: string;
  requested_at: string;
}

// `GET /api/v1/away/brief`: the newest away contract with what happened under
// it, read from the records, so it is the same each time. `away` is true
// while the contract still binds.
export interface AwayBrief {
  approval_id: string;
  away: boolean;
  started_at: string;
  until_at: string;
  ended_at: string | null;
  ended_reason: string | null;
  orders: string | null;
  projects: string[];
  merge: AwayMerge;
  spend: {
    max_new_runs: number | null;
    new_runs: number;
    quota_floor_percent: number | null;
    cap_reached: boolean;
  };
  runs: AwayRun[];
  landed: AwayCard[];
  merged_by_grant: AwayMergeLine[];
  waiting_decisions: AwayDecision[];
  waiting_approvals: AwayWaitingApproval[];
}

export interface AwayBriefResponse {
  brief: AwayBrief | null;
}
