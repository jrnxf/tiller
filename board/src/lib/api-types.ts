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
  column: CardColumn;
  badge: string;
  parked_until: string | null;
  parked_reason: string | null;
  blocked_by: string | null;
  closed_at: string | null;
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

export interface AttentionItem {
  id: string;
  card_id: string;
  kind: "decision" | "credential" | "merge" | "review" | "recovery" | "report";
  question: string;
  evidence: string | null;
  recommendation: string | null;
  consequence: string | null;
  opened_at: string;
  still_current: boolean;
  answered_at: string | null;
  withdrawn_at: string | null;
  rev: number;
}

interface PermissionPrompt {
  session_id: string;
  card_id: string;
  tool: string;
  since: string;
}

export interface AttentionResponse {
  items: AttentionItem[];
  permission_prompts: PermissionPrompt[];
}

interface AgentRow {
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
}

export interface AgentsResponse {
  orchestrators: AgentRow[];
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

// The inlined `<script id="tiller-boot">` body and `GET /api/v1/snapshot`.
export interface Snapshot {
  v: 1;
  server_time: string;
  last_event_seq: number;
  thresholds: Record<string, unknown>;
  queries: Record<string, unknown>;
}
