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
}

export interface AskedRow {
  id: string;
  project_id: string;
  session_id: string | null;
  received_at: string;
  age_seconds: number;
}

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
}

export interface AgentsResponse {
  orchestrators: AgentRow[];
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
  deps: { display_id: string; released_at: string | null }[];
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
  // For a paired peer (section 9): the link state and the pairing's scope; null
  // or absent for this host and for a herdr-only machine.
  link?: LinkState | null;
  peer_scope?: PeerScope | null;
  // Why the peer refused the link (`revoked`, `unknown_key`, `bad_sig`,
  // `clock_skew`, `version`, `accept_off`), when it did.
  link_reason?: string | null;
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

// `GET /api/v1/projects/{id}/retire`: whether an archive applies now, and the
// running agents it can stop first.
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
  machines_live: number;
  machines_total: number;
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

export interface MemoryChange {
  proposal_id: string;
  kind: "added" | "kept" | "edit" | "remove" | "discard";
  text: string | null;
  from: string;
  at: string;
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
