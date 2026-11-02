// Fixtures that follow the board API in docs/spec/protocol.md section 7, used
// until the daemon (T2.8) serves it: by the dev and preview servers, and by
// tests. Names are placeholders. No email, account id or credential appears
// here, and a test keeps it that way.
import type {
  AgentsResponse,
  AttentionResponse,
  BoardResponse,
  BoardRow,
  Machine,
  MachinesResponse,
  ProjectsResponse,
  QuotaDot,
  QuotaResponse,
  QuotaWindow,
  Snapshot,
} from "./api-types.ts";
import {
  agentsPath,
  attentionPath,
  boardPath,
  machinesPath,
  projectsPath,
  quotaPath,
} from "./queries.ts";

export type Scenario =
  | "default"
  | "quota-amber"
  | "quota-red"
  | "quota-grey"
  | "stale-machine"
  | "empty";

export const SCENARIOS: readonly Scenario[] = [
  "default",
  "quota-amber",
  "quota-red",
  "quota-grey",
  "stale-machine",
  "empty",
];

const WEEKLY_FLOOR_PERCENT = 3;

interface DotInput {
  percentRemaining: number | null;
  resetsAt: string | null;
  projectedExhaustionAt: string | null;
  pace: string | null;
  stale: boolean;
}

// The dot table from protocol.md section 8, on the weekly window.
export function quotaDot(input: DotInput): QuotaDot {
  if (input.stale || input.percentRemaining === null) return "grey";
  const exhaustsBeforeReset =
    input.projectedExhaustionAt !== null &&
    input.resetsAt !== null &&
    Date.parse(input.projectedExhaustionAt) < Date.parse(input.resetsAt);
  if (input.percentRemaining <= WEEKLY_FLOOR_PERCENT || exhaustsBeforeReset) return "red";
  if (input.pace === "ahead") return "amber";
  return "green";
}

const minutes = (now: number, delta: number) => new Date(now + delta * 60_000).toISOString();

function weeklyWindow(
  now: number,
  fields: Partial<Omit<QuotaWindow, "dot" | "label" | "kind" | "title">> = {},
): QuotaWindow {
  const base = {
    percent_remaining: 27,
    resets_at: "2026-11-03T14:00:00.000Z",
    pace: "behind",
    reserve: 8,
    burn_multiple: 0.8,
    elapsed_percent: 71,
    projected_exhaustion_at: null,
    fetched_at: minutes(now, -2),
    stale: false,
    ...fields,
  };
  return {
    ...base,
    label: "claude:seven_day",
    title: "Weekly",
    kind: "weekly",
    dot: quotaDot({
      percentRemaining: base.percent_remaining,
      resetsAt: base.resets_at,
      projectedExhaustionAt: base.projected_exhaustion_at,
      pace: base.pace,
      stale: base.stale,
    }),
  };
}

function sessionWindow(now: number): QuotaWindow {
  return {
    label: "claude:five_hour",
    title: "Session",
    kind: "session",
    percent_remaining: 64,
    resets_at: minutes(now, 140),
    pace: "behind",
    reserve: 12,
    burn_multiple: 0.6,
    elapsed_percent: 52,
    projected_exhaustion_at: null,
    fetched_at: minutes(now, -2),
    stale: false,
    dot: "green",
  };
}

function quotaFor(scenario: Scenario, now: number): QuotaResponse {
  switch (scenario) {
    case "quota-amber":
      return {
        windows: [
          weeklyWindow(now, {
            percent_remaining: 41,
            pace: "ahead",
            burn_multiple: 1.4,
            reserve: -9,
          }),
          sessionWindow(now),
        ],
      };
    case "quota-red":
      return {
        windows: [
          weeklyWindow(now, {
            percent_remaining: 2,
            pace: "ahead",
            burn_multiple: 2.2,
            reserve: -31,
            projected_exhaustion_at: "2026-11-02T20:00:00.000Z",
          }),
          sessionWindow(now),
        ],
      };
    case "quota-grey":
      return {
        windows: [
          weeklyWindow(now, { stale: true, fetched_at: minutes(now, -95) }),
          sessionWindow(now),
        ],
      };
    default:
      return { windows: [weeklyWindow(now), sessionWindow(now)] };
  }
}

function machinesFor(scenario: Scenario, now: number): MachinesResponse {
  const hostA: Machine = {
    id: "01JHOSTA00000000000000000A",
    label: "host-a",
    kind: "local",
    view_only: false,
    health: "ok",
    running_agents: 3,
    capacity: 5,
    free_slots: 2,
    last_seen_at: minutes(now, 0),
    stale_reason: null,
    rev: 4,
  };
  const hostB: Machine = {
    id: "01JHOSTB00000000000000000B",
    label: "host-b",
    kind: "remote",
    view_only: true,
    health: "ok",
    running_agents: 2,
    capacity: null,
    free_slots: null,
    last_seen_at: minutes(now, -1),
    stale_reason: null,
    rev: 7,
  };
  const hostC: Machine = {
    id: "01JHOSTC00000000000000000C",
    label: "host-c",
    kind: "remote",
    view_only: true,
    health: scenario === "stale-machine" ? "stale" : "ok",
    running_agents: scenario === "stale-machine" ? null : 1,
    capacity: null,
    free_slots: null,
    last_seen_at: minutes(now, scenario === "stale-machine" ? -17 : -1),
    stale_reason: scenario === "stale-machine" ? "herdr_link_down" : null,
    rev: 2,
  };
  return { machines: [hostA, hostB, hostC] };
}

const PROJECTS: ProjectsResponse = {
  projects: [
    { id: "alpha", name: "alpha", delivery: "no-mistakes", capacity: 2, rev: 1 },
    { id: "beta", name: "beta", delivery: "direct-push", capacity: 2, rev: 1 },
    { id: "gamma", name: "gamma", delivery: "direct-pr", capacity: 1, rev: 3 },
  ],
};

function card(
  n: number,
  project: string,
  column: BoardRow["column"],
  badge: string,
  title: string,
  host: string | null = "01JHOSTA00000000000000000A",
): BoardRow {
  return {
    id: `01JCARD${String(n).padStart(19, "0")}`,
    display_id: `${project}-${n}`,
    project_id: project,
    host_id: host,
    kind: "ship",
    title,
    column,
    badge,
    parked_until: null,
    parked_reason: null,
    blocked_by: null,
    closed_at: column === "done" ? "2026-11-01T18:20:00.000Z" : null,
  };
}

function boardFor(scenario: Scenario): BoardResponse {
  if (scenario === "empty") return { rows: [], asked: [] };
  return {
    rows: [
      card(41, "alpha", "needs_you", "decision", "Pick the retry policy for the export job"),
      card(42, "alpha", "working", "working", "Add the machines summary to the sidebar"),
      card(
        43,
        "alpha",
        "landing",
        "checks",
        "Move the poller behind a trait",
        "01JHOSTB00000000000000000B",
      ),
      card(7, "beta", "working", "stale", "Rewrite the importer's dry run"),
      card(8, "beta", "queued", "filed", "Document the cutover steps"),
      card(9, "beta", "done", "merged", "Pin the toolchain version"),
      card(12, "gamma", "needs_you", "permission_prompt", "Renew the deploy certificate"),
      card(13, "gamma", "queued", "parked", "Trim the build cache"),
    ],
    asked: [],
  };
}

function attentionFor(scenario: Scenario, board: BoardResponse): AttentionResponse {
  if (scenario === "empty") return { items: [], permission_prompts: [] };
  const needs = board.rows.filter((row) => row.column === "needs_you");
  return {
    items: needs.map((row, index) => ({
      id: `01JATTN${String(index).padStart(19, "0")}`,
      card_id: row.id,
      kind: "decision" as const,
      question: `Decide: ${row.title}`,
      evidence: null,
      recommendation: null,
      consequence: null,
      opened_at: "2026-11-02T09:00:00.000Z",
      still_current: true,
      answered_at: null,
      withdrawn_at: null,
      rev: 1,
    })),
    permission_prompts: [],
  };
}

function agentsFor(): AgentsResponse {
  return {
    orchestrators: [
      {
        session_id: "01JSESS000000000000000000A",
        role: "orchestrator",
        model: "fable",
        activity: "working",
        activity_observed_at: "2026-11-02T11:00:00.000Z",
        last_status_line: "Waiting on the builder",
        herdr_name: "alpha-42-orchestrator",
        card_id: null,
        host_id: "01JHOSTA00000000000000000A",
        workers: [],
      },
    ],
  };
}

export function fixtureResponses(scenario: Scenario = "default", now: number = Date.now()) {
  const board = boardFor(scenario);
  return {
    [boardPath()]: board,
    [attentionPath]: attentionFor(scenario, board),
    [agentsPath()]: agentsFor(),
    [machinesPath]: machinesFor(scenario, now),
    [quotaPath]: quotaFor(scenario, now),
    [projectsPath]: PROJECTS,
  } satisfies Record<string, unknown>;
}

export function fixtureSnapshot(
  scenario: Scenario = "default",
  now: number = Date.now(),
): Snapshot {
  return {
    v: 1,
    server_time: new Date(now).toISOString(),
    last_event_seq: 4181,
    thresholds: { orchestrator_stale_minutes: 30, worker_stale_minutes: 15 },
    queries: fixtureResponses(scenario, now),
  };
}

// The text the daemon puts in index.html: escaped so it cannot end the script
// element or break a line terminator (protocol.md section 7.6).
export function bootScript(snapshot: Snapshot): string {
  const json = JSON.stringify(snapshot)
    .replaceAll("<", "\\u003c")
    .replaceAll(">", "\\u003e")
    .replaceAll("&", "\\u0026")
    .replaceAll(" ", "\\u2028")
    .replaceAll(" ", "\\u2029");
  return `<script type="application/json" id="tiller-boot">${json}</script>`;
}
