// Types for the board API in docs/spec/protocol.md section 7. They are written
// by hand until `contract/openapi.json` (T2.8) exists; then this file is
// replaced by types generated from it and only the names below must survive.

type CardColumn = "needs_you" | "working" | "landing" | "queued" | "done";

export type BoardFilter = "all" | "active" | "needs";

// One `card_column` row plus the card's own display fields.
export interface BoardRow {
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

interface AskedRow {
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

// One answer the asker offers, with what it sets in motion.
interface AttentionOption {
  label: string;
  consequence: string | null;
  recommended: boolean;
}

// The approval a `merge` item asks for, bound to the head the board shows.
interface ApprovalBinding {
  id: string;
  kind: "merge" | "land_direct";
  head_sha: string;
  pr_number: number | null;
  state: string;
}

export interface AttentionItem {
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
  answered_at: string | null;
  withdrawn_at: string | null;
  rev: number;
}

export interface PermissionPrompt {
  session_id: string;
  card_id: string;
  tool: string;
  // What the tool wants to run, from the session's newest observation.
  detail: string | null;
  since: string;
}

export interface AttentionResponse {
  items: AttentionItem[];
  permission_prompts: PermissionPrompt[];
}

export interface AgentRow {
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
  artifacts: { id: string; kind: string; title: string }[];
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

export type StaleReason = "herdr_link_down" | "host_asleep" | "poll_failed" | "never_seen";

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

export interface Project {
  id: string;
  name: string;
  delivery: string;
  capacity: number;
  rev: number;
}

export interface ProjectsResponse {
  projects: Project[];
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

export interface MutationResponse<T = unknown> {
  ok: true;
  replayed: boolean;
  events: number[];
  receipt: Receipt;
  data: T;
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
export interface Snapshot {
  v: 1;
  server_time: string;
  last_event_seq: number;
  thresholds: Record<string, unknown>;
  queries: Record<string, unknown>;
}

// `GET /api/v1/signals`: the Health page's counts, one entry per Lisbon week,
// oldest first, the last being the week in progress (protocol.md section 7.3).
export interface SignalsWeek {
  start: string;
  end: string;
  landed: number;
  reported: number;
  merged_prs: number;
  needed: number;
  median_answer_minutes: number | null;
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
  escaped: number;
  median_answer_minutes: number | null;
}

export interface SignalsResponse {
  weeks: SignalsWeek[];
  by_project: SignalsProject[];
  machines_live: number;
  machines_total: number;
}
