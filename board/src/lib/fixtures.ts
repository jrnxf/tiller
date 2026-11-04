// Fixtures that follow the board API in docs/spec/protocol.md section 7, used
// until the daemon (T2.8) serves it: by the dev and preview servers, and by
// tests. Names are placeholders. No email, account id or credential appears
// here, and a test keeps it that way.
import type {
  AgentRow,
  AgentsResponse,
  AttentionResponse,
  BoardResponse,
  BoardRow,
  CardDetail,
  CardPr,
  EventLine,
  Machine,
  MachinesResponse,
  Project,
  ProjectsResponse,
  QuotaDot,
  QuotaResponse,
  QuotaWindow,
  RunDetail,
  Snapshot,
  TimelineResponse,
} from "./api-types.ts";
import {
  peerAgents,
  peerAttention,
  peerMachines,
  peerProjects,
  peerRows,
  peerSnapshot,
} from "./fixtures-federated.ts";
import { memoryResource } from "./fixtures-memory.ts";
import { signalsFor } from "./fixtures-signals.ts";
import { isPeerRow } from "./peers.ts";
import {
  agentsPath,
  attentionPath,
  boardPath,
  machinesPath,
  projectsPath,
  quotaPath,
  signalsPath,
} from "./queries.ts";

export type Scenario =
  | "default"
  | "quota-amber"
  | "quota-red"
  | "quota-grey"
  | "stale-machine"
  | "degraded"
  | "stale-approval"
  | "empty"
  | "busy"
  | "health-bad"
  | "live"
  | "federated";

export const SCENARIOS: readonly Scenario[] = [
  "default",
  "quota-amber",
  "quota-red",
  "quota-grey",
  "stale-machine",
  "degraded",
  "stale-approval",
  "empty",
  "busy",
  "health-bad",
  "live",
  "federated",
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
  if (input.percentRemaining <= WEEKLY_FLOOR_PERCENT) return "red";
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
    health: scenario === "stale-machine" || scenario === "degraded" ? "stale" : "ok",
    running_agents: scenario === "stale-machine" || scenario === "degraded" ? null : 1,
    capacity: null,
    free_slots: null,
    last_seen_at: minutes(now, scenario === "stale-machine" || scenario === "degraded" ? -17 : -1),
    stale_reason:
      scenario === "stale-machine" || scenario === "degraded" ? "herdr_link_down" : null,
    rev: 2,
  };
  const machines = [hostA, hostB, hostC];
  return { machines: scenario === "federated" ? [...machines, ...peerMachines(now)] : machines };
}

const HOST_A = "01JHOSTA00000000000000000A";
const HOST_B = "01JHOSTB00000000000000000B";

// A launched project with no review open; each fixture project overrides what
// makes it different.
function launched(
  id: string,
  delivery: string,
  capacity: number,
  rev: number,
  more: Partial<Project> = {},
): Project {
  return {
    id,
    name: id,
    delivery,
    capacity,
    rev,
    stage: "launched",
    effective_delivery: delivery,
    reviewed_through_sha: null,
    reviewed_at: null,
    stage_changed_at: null,
    next_review_due: null,
    review_overdue: false,
    review_open: null,
    ...more,
  };
}

// alpha is pre-launch: its no-mistakes pipeline is skipped (direct-pr), the
// last weekly review ended Saturday 31 Oct, and the next is due Saturday 7 Nov
// at 06:00 Lisbon time.
const ALPHA = launched("alpha", "no-mistakes", 2, 1, {
  stage: "pre-launch",
  effective_delivery: "direct-pr",
  reviewed_through_sha: "9c1e4f7a02b86d35e1a74c90b3f2d8e65a0c7b14",
  reviewed_at: "2026-10-31T07:12:00.000Z",
  stage_changed_at: "2026-10-24T09:30:00.000Z",
  next_review_due: "2026-11-07T06:00:00.000Z",
});

// delta was launched this week and its final review is still running.
const DELTA = launched("delta", "direct-pr", 2, 1, {
  reviewed_through_sha: "5b7d20e9c4a1f386d02e8b57a9c3140f6e2d8a71",
  reviewed_at: "2026-10-31T07:12:00.000Z",
  stage_changed_at: "2026-11-02T16:05:00.000Z",
  review_open: "final",
});

const DEFAULT_PROJECTS: ProjectsResponse = {
  projects: [ALPHA, launched("beta", "direct-push", 2, 1), launched("gamma", "direct-pr", 1, 3)],
};

const BUSY_PROJECTS: ProjectsResponse = {
  projects: [...DEFAULT_PROJECTS.projects, DELTA],
};

// `degraded`: delta runs on the stale host (a disconnected lane), beta's
// orchestrator has gone quiet (a stale lane), gamma has a card whose state is
// unknown, and kiln was registered and has no cards.
const DEGRADED_PROJECTS: ProjectsResponse = {
  projects: [...DEFAULT_PROJECTS.projects, DELTA, launched("kiln", "direct-push", 1, 1)],
};

function projectsFor(scenario: Scenario, now: number = Date.now()): ProjectsResponse {
  if (scenario === "federated") {
    return { projects: [...DEFAULT_PROJECTS.projects, ...peerProjects(now)] };
  }
  if (scenario === "degraded") return DEGRADED_PROJECTS;
  return scenario === "busy" ? BUSY_PROJECTS : DEFAULT_PROJECTS;
}

const HOST_C = "01JHOSTC00000000000000000C";

function degradedRows(now: number): BoardRow[] {
  return [
    ...defaultRows(now),
    card(
      30,
      "gamma",
      "working",
      "unknown",
      "Rotate the staging credentials",
      {
        model: "sonnet",
        observedMinutes: 6.2,
      },
      now,
    ),
    card(
      60,
      "delta",
      "working",
      "working",
      "Port the exporter to the new schema",
      {
        host: HOST_C,
        model: "sonnet",
        observedMinutes: 13,
      },
      now,
    ),
    card(
      61,
      "delta",
      "needs_you",
      "decision",
      "Choose the retention window",
      {
        host: HOST_C,
        model: "opus",
        observedMinutes: 14,
      },
      now,
    ),
    card(
      62,
      "delta",
      "queued",
      "filed",
      "Document the exporter flags",
      {
        host: HOST_C,
        kind: "scout",
      },
      now,
    ),
  ];
}

interface CardExtras {
  host?: string | null;
  kind?: string;
  model?: string | null;
  pr?: number | null;
  observedMinutes?: number | null;
  priority?: number;
  parkedReason?: string | null;
  blockedBy?: string | null;
}

function card(
  n: number,
  project: string,
  column: BoardRow["column"],
  badge: string,
  title: string,
  extras: CardExtras = {},
  now: number = Date.now(),
): BoardRow {
  const observed = extras.observedMinutes;
  return {
    id: `01JCARD${String(n).padStart(19, "0")}`,
    display_id: `${project}-${n}`,
    project_id: project,
    host_id: extras.host === undefined ? HOST_A : extras.host,
    kind: extras.kind ?? "ship",
    title,
    priority: extras.priority ?? 2,
    column,
    badge,
    parked_until: null,
    parked_reason: extras.parkedReason ?? null,
    blocked_by: extras.blockedBy ?? null,
    closed_at: column === "done" ? "2026-11-01T18:20:00.000Z" : null,
    rev: 3,
    model: extras.model ?? null,
    pr_number: extras.pr ?? null,
    observed_at: observed === null || observed === undefined ? null : minutes(now, -observed),
  };
}

function defaultRows(now: number): BoardRow[] {
  return [
    card(
      41,
      "alpha",
      "needs_you",
      "decision",
      "Pick the retry policy for the export job",
      {
        model: "sonnet",
        observedMinutes: 0.2,
      },
      now,
    ),
    card(
      42,
      "alpha",
      "working",
      "working",
      "Add the machines summary to the sidebar",
      {
        model: "sonnet",
        observedMinutes: 0.05,
        priority: 1,
      },
      now,
    ),
    card(
      43,
      "alpha",
      "landing",
      "checks",
      "Move the poller behind a trait",
      {
        host: HOST_B,
        model: "opus",
        pr: 52,
        observedMinutes: 0.7,
      },
      now,
    ),
    card(
      44,
      "alpha",
      "needs_you",
      "merge",
      "Retry failed exports with backoff",
      {
        model: "opus",
        pr: 53,
        observedMinutes: 0.4,
      },
      now,
    ),
    card(
      7,
      "beta",
      "working",
      "stale",
      "Rewrite the importer's dry run",
      {
        model: "sonnet",
        observedMinutes: 22,
      },
      now,
    ),
    card(8, "beta", "queued", "filed", "Document the cutover steps", { kind: "scout" }, now),
    card(9, "beta", "done", "landed", "Pin the toolchain version", { pr: 51 }, now),
    card(
      12,
      "gamma",
      "needs_you",
      "permission_prompt",
      "Renew the deploy certificate",
      {
        model: "haiku",
        observedMinutes: 1,
        priority: 0,
      },
      now,
    ),
    card(
      13,
      "gamma",
      "queued",
      "parked",
      "Trim the build cache",
      {
        kind: "ops",
        parkedReason: "waiting for the release",
      },
      now,
    ),
  ];
}

const BUSY_TITLES = [
  "Index the export queue",
  "Retry failed webhooks",
  "Trim the cache directory",
  "Document the cutover steps",
  "Pin the toolchain version",
  "Split the settings screen",
  "Rename the sync command",
  "Add a dry run to import",
  "Fix the timezone offset",
  "Show the host in the footer",
];

// 4 projects with 10 cards each, spread the way a busy week looks: two queued,
// two working, one waiting on the owner, one landing and four recently done.
function busyRows(now: number): BoardRow[] {
  const rows: BoardRow[] = [];
  const plan: [BoardRow["column"], string][] = [
    ["queued", "filed"],
    ["queued", "parked"],
    ["working", "working"],
    ["working", "idle"],
    ["needs_you", "decision"],
    ["landing", "checks"],
    ["done", "landed"],
    ["done", "landed"],
    ["done", "reported"],
    ["done", "completed"],
  ];
  BUSY_PROJECTS.projects.forEach((project, projectIndex) => {
    plan.forEach(([column, badge], index) => {
      const n = (projectIndex + 1) * 100 + index + 1;
      const title = BUSY_TITLES[(index + projectIndex * 3) % BUSY_TITLES.length] ?? "Fixture card";
      rows.push(
        card(
          n,
          project.id,
          column,
          badge,
          title,
          {
            host: projectIndex === 3 ? HOST_B : HOST_A,
            kind: index % 4 === 1 ? "scout" : "ship",
            model: index % 3 === 0 ? "opus" : index % 3 === 1 ? "sonnet" : "haiku",
            pr: column === "landing" || column === "done" ? 200 + n : null,
            observedMinutes:
              column === "working" || column === "landing" || column === "needs_you"
                ? 0.1 + index / 10
                : null,
            parkedReason: badge === "parked" ? "waiting for the release" : null,
          },
          now,
        ),
      );
    });
  });
  return rows;
}

function boardFor(scenario: Scenario, now: number): BoardResponse {
  if (scenario === "empty") return { rows: [], asked: [] };
  if (scenario === "degraded") return { rows: degradedRows(now), asked: [] };
  if (scenario === "federated") {
    return { rows: [...defaultRows(now), ...peerRows(now)], asked: [] };
  }
  if (scenario === "busy") {
    return {
      rows: busyRows(now),
      asked: [
        {
          id: "01JASK000000000000000000A1",
          project_id: "alpha",
          session_id: "01JSESS000000000000000000A",
          received_at: minutes(now, -2),
          age_seconds: 120,
        },
        {
          id: "01JASK000000000000000000A2",
          project_id: "gamma",
          session_id: null,
          received_at: minutes(now, -1),
          age_seconds: 60,
        },
      ],
    };
  }
  return {
    rows: defaultRows(now),
    asked: [
      {
        id: "01JASK000000000000000000A1",
        project_id: "alpha",
        session_id: "01JSESS000000000000000000A",
        received_at: minutes(now, -2),
        age_seconds: 120,
      },
    ],
  };
}

// The head of the alpha-44 PR. In `stale-approval` the board shows the head
// from before the last push, so approving it is refused (T3.3).
export const FIXTURE_HEAD_SHA = "d82f4051c3a7e9b2f60d18a4c5e7b9d3a1f20c64";
const FIXTURE_OLD_HEAD_SHA = "7c3e9a1b52d8f04e6a9c3b7d1e5f2a8c4b6d0e93";

function attentionFor(
  scenario: Scenario,
  board: BoardResponse,
  now: number = Date.now(),
): AttentionResponse {
  if (scenario === "empty") return { items: [], permission_prompts: [] };
  const byDisplay = new Map(
    board.rows.filter((row) => !isPeerRow(row)).map((row) => [row.display_id, row.id]),
  );
  const cardId = (display: string) => byDisplay.get(display) ?? display;
  const local: AttentionResponse = {
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
  if (scenario !== "federated") return local;
  const peer = peerAttention(now, board.rows);
  return {
    items: [...local.items, ...peer.items],
    permission_prompts: [...local.permission_prompts, ...peer.permission_prompts],
  };
}

function sessionId(key: string): string {
  return `01JSESS${key.replace(/\W/g, "").padEnd(19, "0").slice(0, 19).toUpperCase()}`;
}

function workerFor(row: BoardRow, now: number, index: number): AgentRow {
  const role = row.kind === "scout" ? "scout" : "build";
  const stale = row.badge === "stale";
  return {
    session_id: sessionId(`W${row.display_id}`),
    role,
    model: row.model ?? "sonnet",
    activity:
      row.column === "needs_you" ? "waiting_permission" : row.badge === "idle" ? "idle" : "working",
    activity_observed_at: minutes(now, stale ? -22 : -(0.1 + index / 10)),
    last_status_line: stale
      ? "Reading src/import/dry_run.rs"
      : `Running tests for ${row.display_id}`,
    herdr_name: `${row.display_id}-${role}`,
    card_id: row.id,
    host_id: row.host_id ?? HOST_A,
    workers: [],
    project_id: row.project_id,
    run_id: runIdFor(row.id, 1),
    pane_id: `pane-${row.display_id}`,
    workspace_label: `${row.display_id} ${role}`,
    state_since: minutes(now, -(8 + index * 3)),
  };
}

function agentsFor(
  board: BoardResponse,
  projects: ProjectsResponse,
  now: number,
  scenario: Scenario,
): AgentsResponse {
  if (board.rows.length === 0) return { orchestrators: [] };
  const peerProjectRows = projects.projects.filter(isPeerRow);
  const localOrchestrators = projects.projects
    .filter((p) => !isPeerRow(p))
    .map((project, projectIndex) => {
      const live = board.rows.filter(
        (row) =>
          !isPeerRow(row) &&
          row.project_id === project.id &&
          (row.column === "working" || row.column === "landing" || row.column === "needs_you"),
      );
      const host = live.find((row) => row.host_id)?.host_id ?? HOST_A;
      // `degraded`: beta's orchestrator last spoke 41 minutes ago.
      const quiet = scenario === "degraded" && project.id === "beta";
      return {
        session_id: sessionId(`O${project.id}`),
        role: "orchestrator",
        model: "opus",
        activity: "working",
        activity_observed_at: minutes(now, quiet ? -41 : -(0.2 + projectIndex)),
        last_status_line: `Waiting on ${live[0]?.display_id ?? "the next ask"}`,
        herdr_name: `${project.id}-orch`,
        card_id: null,
        host_id: host,
        workers: live.map((row, index) => workerFor(row, now, index)),
        project_id: project.id,
        run_id: runIdFor(`orch-${project.id}`, 1),
        pane_id: `pane-${project.id}`,
        workspace_label: project.id,
        state_since: minutes(now, -(40 + projectIndex * 9)),
      };
    });
  return {
    orchestrators: [...localOrchestrators, ...peerAgents(now, board.rows, peerProjectRows)],
  };
}

// ---- details served for a card, its timeline and a run ----

function runIdFor(cardKey: string, number: number): string {
  return `01JRUN${cardKey.replace(/\W/g, "").slice(-14).padEnd(14, "0").toUpperCase()}${number}`;
}

// A stable 40-digit hex string standing in for a commit hash.
function fakeSha(seed: string): string {
  let state = 2166136261;
  let out = "";
  while (out.length < 40) {
    for (const char of seed + out) {
      state = Math.imul(state ^ char.charCodeAt(0), 16777619) >>> 0;
    }
    out += state.toString(16).padStart(8, "0");
  }
  return out.slice(0, 40);
}

const HOME = "/home/user/work";

function eventsFor(row: BoardRow, now: number): EventLine[] {
  const events: EventLine[] = [
    {
      at: minutes(now, -1),
      source: "herdr",
      text: `pane.state ${row.badge === "idle" ? "idle" : "working"} (${row.display_id})`,
    },
    {
      at: minutes(now, -9),
      source: "tiller",
      text: `run.start run=1 worker=${row.kind === "scout" ? "scout" : "build"}`,
    },
    { at: minutes(now, -26), source: "tiller", text: `card.state ${row.column}` },
    { at: minutes(now, -300), source: "tiller", text: `card.filed ${row.display_id}` },
  ];
  if (row.pr_number) {
    events.splice(1, 0, {
      at: minutes(now, -5),
      source: "gh",
      text: `ci.pass 4/4 on ${fakeSha(row.id).slice(0, 7)}`,
    });
  }
  if (row.badge === "stale") {
    events.unshift({
      at: minutes(now, -4),
      source: "tiller",
      text: "observation.stale worker silent",
      tone: "warn",
    });
  }
  return events;
}

function cardDetailFor(
  board: BoardResponse,
  cardId: string,
  now: number = Date.now(),
): CardDetail | null {
  const row = board.rows.find((candidate) => candidate.id === cardId);
  if (!row) return null;
  const done = row.column === "done";
  const active = row.column === "working" || row.column === "landing" || row.column === "needs_you";
  const role = row.kind === "scout" ? "scout" : "build";
  const sha = fakeSha(row.id);
  const prs: CardPr[] = row.pr_number
    ? [
        {
          id: `01JPR${String(row.pr_number).padStart(21, "0")}`,
          number: row.pr_number,
          repo: `example/${row.project_id}`,
          title: row.title,
          state: done ? "merged" : "open",
          head_sha: sha,
          base: "main",
          checks: row.badge === "checks" ? "pending" : "passing",
          checks_summary: row.badge === "checks" ? "2 of 4 checks running" : "4 of 4 checks green",
          diff_summary: "+142 -37, 6 files",
        },
      ]
    : [];
  // The first landing card carries a stale approval so the screens show it.
  const stale = row.id.endsWith("43");
  const approvals = prs.map((pr) => ({
    id: `01JAPPR${pr.id.slice(-19)}`,
    kind: "merge",
    state: stale ? "accepted" : done ? "succeeded" : "accepted",
    pr_id: pr.id,
    head_sha: stale ? fakeSha(`${row.id}-old`) : pr.head_sha,
    decided_at: minutes(now, -80),
    decided_by: "owner",
    newer_commits: stale
      ? [
          {
            sha: fakeSha(`${row.id}-2`),
            subject: "undo: coalesce on pointerup",
            at: minutes(now, -12),
          },
          {
            sha: fakeSha(`${row.id}-1`),
            subject: "test: cancelled stroke leaves one undo step",
            at: minutes(now, -20),
          },
        ]
      : [],
  }));
  const runs = done
    ? [
        {
          id: runIdFor(row.id, 1),
          number: 1,
          role,
          model: row.model ?? "sonnet",
          started_at: minutes(now, -1500),
          ended_at: minutes(now, -1440),
          outcome: "passed" as const,
          note: row.kind === "scout" ? "Reported with a findings file." : "Finished, checks green.",
          session_id: null,
          host_id: row.host_id,
          activity: null,
          activity_observed_at: null,
        },
      ]
    : active
      ? [
          {
            id: runIdFor(row.id, 0),
            number: 1,
            role,
            model: row.model ?? "sonnet",
            started_at: minutes(now, -120),
            ended_at: minutes(now, -95),
            outcome: "failed" as const,
            note: "Tests failed, the worker stopped with exit 1.",
            session_id: null,
            host_id: row.host_id,
            activity: null,
            activity_observed_at: null,
          },
          {
            id: runIdFor(row.id, 1),
            number: 2,
            role,
            model: row.model ?? "sonnet",
            started_at: minutes(now, -9),
            ended_at: null,
            outcome: null,
            note: null,
            session_id: sessionId(`W${row.display_id}`),
            host_id: row.host_id,
            activity: row.column === "needs_you" ? "waiting_permission" : "working",
            activity_observed_at: row.observed_at ?? minutes(now, -1),
          },
        ]
      : [];
  return {
    card: {
      id: row.id,
      display_id: row.display_id,
      project_id: row.project_id,
      kind: row.kind,
      title: row.title,
      body: "A placeholder description for this fixture card. It says what the card is for in a sentence or two.",
      priority: row.priority,
      slug: `${row.display_id}-${row.title.toLowerCase().replace(/\W+/g, "-").slice(0, 28)}`,
      state: done ? "closed" : "open",
      outcome: done ? row.badge : null,
      model: row.model ?? null,
      branch: active ? `ship/${row.display_id}` : null,
      worktree: active ? `${HOME}/${row.display_id}` : null,
      rev: 3,
    },
    column: row,
    holds:
      row.badge === "parked"
        ? [{ id: "01JHOLD0000000000000000001", until_at: null, reason: row.parked_reason }]
        : [],
    deps: [],
    attention: [],
    runs,
    prs,
    approvals,
    artifacts: [],
    reviews: [],
    obligations: [],
    events: eventsFor(row, now),
    freshness: [
      {
        source: "Pane (herdr)",
        detail: active ? "working" : done ? "pane closed" : "no pane yet",
        observed_at: active ? minutes(now, -0.05) : null,
      },
      {
        source: "Hook events",
        detail: "last: turn.end",
        observed_at: active ? minutes(now, -3) : null,
      },
      {
        source: "GitHub PR and CI",
        detail: prs.length ? "polled" : "no PR",
        observed_at: prs.length ? minutes(now, -0.7) : null,
      },
      {
        source: "Worktree git status",
        detail: active ? "clean" : "none",
        observed_at: active ? minutes(now, -2) : null,
      },
    ],
  };
}

function timelineFor(
  board: BoardResponse,
  cardId: string,
  before: number | null,
  now: number = Date.now(),
): TimelineResponse | null {
  const row = board.rows.find((candidate) => candidate.id === cardId);
  if (!row) return null;
  const newer: TimelineResponse["entries"] = [
    {
      seq: 4180,
      at: minutes(now, -3),
      actor: "hook",
      text: `logged progress on ${row.display_id}`,
    },
    { seq: 4170, at: minutes(now, -9), actor: "system", text: "started attempt 2" },
    { seq: 4160, at: minutes(now, -95), actor: "system", text: "attempt 1 failed: exit 1" },
  ];
  const older: TimelineResponse["entries"] = [
    {
      seq: 4100,
      at: minutes(now, -130),
      actor: "orchestrator",
      text: `filed ${row.display_id}: ${row.title}`,
    },
    { seq: 4090, at: minutes(now, -135), actor: "you", text: "asked in the orchestrator pane" },
  ];
  if (before !== null) {
    return { entries: older.filter((entry) => entry.seq < before), next_before: null };
  }
  return { entries: newer, next_before: 4160 };
}

function runDetailFor(runId: string, now: number = Date.now()): RunDetail {
  return {
    id: runId,
    card_id: "",
    branch: "ship/fixture",
    worktree: `${HOME}/fixture`,
    started_at: minutes(now, -9),
    ended_at: null,
    events: [
      { at: minutes(now, -1), source: "herdr", text: "pane.state working" },
      { at: minutes(now, -2), source: "hook", text: "turn.start" },
      { at: minutes(now, -9), source: "tiller", text: "run.start" },
    ],
  };
}

export function fixtureResponses(scenario: Scenario = "default", now: number = Date.now()) {
  const board = boardFor(scenario, now);
  const projects = projectsFor(scenario, now);
  return {
    [boardPath()]: board,
    [attentionPath]: attentionFor(scenario, board, now),
    [agentsPath()]: agentsFor(board, projects, now, scenario),
    [machinesPath]: machinesFor(scenario, now),
    [quotaPath]: quotaFor(scenario, now),
    [projectsPath]: projects,
    [signalsPath]: signalsFor(scenario === "health-bad", now),
  } satisfies Record<string, unknown>;
}

// What the stand-in server answers a later fetch with. In `stale-approval` the
// boot snapshot holds the head the page first showed, while the server already
// has the pushed one, so the refetch after a refusal brings the new head.
function serverScenario(scenario: Scenario): Scenario {
  return scenario === "stale-approval" ? "default" : scenario;
}

export function serverResponses(scenario: Scenario = "default", now: number = Date.now()) {
  return fixtureResponses(serverScenario(scenario), now);
}

// Anything the stand-in server can answer: the board's lists, a card, its
// timeline, a run. `undefined` means no such resource (a 404).
export function fixtureResource(
  path: string,
  scenario: Scenario = "default",
  now: number = Date.now(),
): unknown {
  const served = serverScenario(scenario);
  const responses = fixtureResponses(served, now) as Record<string, unknown>;
  if (path in responses) return responses[path];
  const board = boardFor(served, now);
  const url = new URL(path, "http://localhost");
  const timeline = /^\/api\/v1\/cards\/([^/]+)\/timeline$/.exec(url.pathname);
  if (timeline?.[1]) {
    const before = url.searchParams.get("before");
    return (
      timelineFor(board, timeline[1], before === null ? null : Number(before), now) ?? undefined
    );
  }
  const detail = /^\/api\/v1\/cards\/([^/]+)$/.exec(url.pathname);
  if (detail?.[1]) return cardDetailFor(board, detail[1], now) ?? undefined;
  const run = /^\/api\/v1\/runs\/([^/]+)$/.exec(url.pathname);
  if (run?.[1]) return runDetailFor(run[1], now);
  if (url.pathname.startsWith("/api/v1/memory")) return memoryResource(url);
  return undefined;
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
    ...(scenario === "federated" ? { peers: peerSnapshot(now) } : {}),
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
