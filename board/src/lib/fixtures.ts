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
  | "stale-approval"
  | "empty";

export const SCENARIOS: readonly Scenario[] = [
  "default",
  "quota-amber",
  "quota-red",
  "quota-grey",
  "stale-machine",
  "stale-approval",
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
    priority: 2,
    column,
    badge,
    parked_until: null,
    parked_reason: null,
    blocked_by: null,
    closed_at: column === "done" ? "2026-11-01T18:20:00.000Z" : null,
    rev: 3,
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
      card(44, "alpha", "needs_you", "merge", "Retry failed exports with backoff"),
      card(7, "beta", "working", "stale", "Rewrite the importer's dry run"),
      card(8, "beta", "queued", "filed", "Document the cutover steps"),
      card(9, "beta", "done", "merged", "Pin the toolchain version"),
      card(12, "gamma", "needs_you", "permission_prompt", "Renew the deploy certificate"),
      card(13, "gamma", "queued", "parked", "Trim the build cache"),
    ],
    asked: [],
  };
}

// The head of the alpha-44 PR. In `stale-approval` the board shows the head
// from before the last push, so approving it is refused (T3.3).
export const FIXTURE_HEAD_SHA = "d82f4051c3a7e9b2f60d18a4c5e7b9d3a1f20c64";
const FIXTURE_OLD_HEAD_SHA = "7c3e9a1b52d8f04e6a9c3b7d1e5f2a8c4b6d0e93";

function attentionFor(scenario: Scenario, board: BoardResponse): AttentionResponse {
  if (scenario === "empty") return { items: [], permission_prompts: [] };
  const byDisplay = new Map(board.rows.map((row) => [row.display_id, row.id]));
  const cardId = (display: string) => byDisplay.get(display) ?? display;
  return {
    items: [
      {
        id: "01JATTN0000000000000000000",
        card_id: cardId("alpha-41"),
        kind: "decision",
        question: "Which retry policy should the export job use?",
        evidence: JSON.stringify([
          "3 of the last 40 exports failed on a storage timeout and all three passed on a manual retry",
          "The storage client already retries once inside a request",
          "https://example.com/alpha/exports/runbook",
        ]),
        recommendation:
          "Exponential backoff, three attempts. It covers every failure seen this month and adds at most 70 s to a bad export.",
        consequence: "The builder implements the policy you pick and opens a PR within the hour.",
        options: [
          {
            label: "Exponential backoff, three attempts",
            consequence: "Waits 10 s, 20 s, then 40 s. A failing export gives up after about 70 s.",
            recommended: true,
          },
          {
            label: "Fixed delay, five attempts",
            consequence:
              "Retries every 15 s. Simpler to reason about, but a long outage costs 75 s.",
            recommended: false,
          },
          {
            label: "No automatic retry",
            consequence: "A failed export opens an attention item instead, and you retry by hand.",
            recommended: false,
          },
        ],
        approval: null,
        opened_by_session_id: "01JSESS000000000000000000A",
        opened_at: "2026-11-02T09:00:00.000Z",
        still_current: true,
        answered_at: null,
        withdrawn_at: null,
        rev: 1,
      },
      {
        id: "01JATTN0000000000000000001",
        card_id: cardId("alpha-44"),
        kind: "merge",
        question: "Merge PR #52 at its current head?",
        evidence: JSON.stringify([
          "CI green on the head (4 of 4 checks)",
          "Independent review: no blocking findings",
          "https://example.com/alpha/pull/52",
        ]),
        recommendation: "Approve. The review found nothing blocking and every check passed.",
        consequence:
          "Approving binds to this exact head. The daemon re-reads the head before it merges, and a new push voids the approval.",
        options: null,
        approval: {
          id: "01JAPPR0000000000000000000",
          kind: "merge",
          head_sha: scenario === "stale-approval" ? FIXTURE_OLD_HEAD_SHA : FIXTURE_HEAD_SHA,
          pr_number: 52,
          state: "requested",
        },
        opened_by_session_id: "01JSESS000000000000000000A",
        opened_at: "2026-11-02T10:35:00.000Z",
        still_current: true,
        answered_at: null,
        withdrawn_at: null,
        rev: 1,
      },
      {
        id: "01JATTN0000000000000000002",
        card_id: cardId("beta-7"),
        kind: "recovery",
        question: "The importer's dry-run worker went quiet. Relaunch it or end the run?",
        evidence: "No transcript or hook activity for 41 minutes.\nThe pane is still open.",
        recommendation: null,
        consequence: null,
        options: [
          { label: "Relaunch the worker", consequence: null, recommended: false },
          { label: "End the run", consequence: null, recommended: false },
        ],
        approval: null,
        opened_by_session_id: null,
        opened_at: "2026-11-02T08:10:00.000Z",
        still_current: false,
        answered_at: null,
        withdrawn_at: null,
        rev: 2,
      },
    ],
    permission_prompts: [
      {
        session_id: "01JSESS000000000000000000C",
        card_id: cardId("gamma-12"),
        tool: "Bash",
        detail: "rm -rf target/debug/incremental && cargo build -p gamma-cert",
        since: "2026-11-02T11:58:00.000Z",
      },
    ],
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

// What the stand-in server answers a later fetch with. In `stale-approval` the
// boot snapshot holds the head the page first showed, while the server already
// has the pushed one, so the refetch after a refusal brings the new head.
export function serverResponses(scenario: Scenario = "default", now: number = Date.now()) {
  return fixtureResponses(scenario === "stale-approval" ? "default" : scenario, now);
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
