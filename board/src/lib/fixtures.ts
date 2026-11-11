// Fixtures that follow the board API in docs/spec/protocol.md section 7, used
// until the daemon (T2.8) serves it: by the dev and preview servers, and by
// tests. Names are placeholders. No email, account id or credential appears
// here, and a test keeps it that way.
import type {
  AgentRow,
  AgentsResponse,
  RestartStaleResponse,
  SessionStale,
  AttentionItem,
  AttentionResponse,
  BoardResponse,
  BoardRow,
  CardDetail,
  CardPr,
  CheckoutCheck,
  DaemonHealth,
  ContributionsResponse,
  EventLine,
  Machine,
  MachinesResponse,
  Project,
  ProjectAgent,
  ProjectDeletePreview,
  ProjectRetirePreview,
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
  workerActivity,
} from "./fixtures-federated.ts";
import { memoryResource } from "./fixtures-memory.ts";
import { recapFor } from "./fixtures-recap.ts";
import { signalsFor } from "./fixtures-signals.ts";
import { isPeerRow } from "./peers.ts";
import {
  agentsPath,
  attentionPath,
  awayBriefPath,
  boardPath,
  machinesPath,
  projectsPath,
  quotaPath,
  recapPath,
  signalsPath,
} from "./queries.ts";
import { awayFor } from "./fixtures-away.ts";

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
  | "federated"
  | "stale-sessions"
  | "setup-needed"
  | "away"
  | "away-back"
  | "new-install"
  | "blocked";

// The scenarios with paired peers. `stale-sessions` has them so a peer's older
// sessions show on this host's board.
function hasPeers(scenario: Scenario): boolean {
  return scenario === "federated" || scenario === "stale-sessions";
}

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
  "stale-sessions",
  "setup-needed",
  "away",
  "away-back",
  "new-install",
  "blocked",
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

function modelWindow(now: number): QuotaWindow {
  return {
    label: "claude:model:fable",
    title: "Fable week",
    kind: "model",
    percent_remaining: 94,
    resets_at: "2026-11-03T14:00:00.000Z",
    pace: "behind",
    reserve: 22,
    burn_multiple: 0.4,
    elapsed_percent: 71,
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
          modelWindow(now),
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
          modelWindow(now),
        ],
      };
    case "quota-grey":
      return {
        windows: [
          weeklyWindow(now, { stale: true, fetched_at: minutes(now, -95) }),
          sessionWindow(now),
          modelWindow(now),
        ],
      };
    default:
      return { windows: [weeklyWindow(now), sessionWindow(now), modelWindow(now)] };
  }
}

// What the fixture board host runs; the federated peers report their own.
const FIXTURE_VERSION = "0.1.0-pre.17";

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
    version: FIXTURE_VERSION,
    // In the federated scenario this host's `tiller` project takes Tiller bugs.
    ...(hasPeers(scenario) ? { bug_project: "tiller" } : {}),
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
  return { machines: hasPeers(scenario) ? [...machines, ...peerMachines(now)] : machines };
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
  if (hasPeers(scenario)) {
    return {
      projects: [
        ...DEFAULT_PROJECTS.projects,
        launched("tiller", "direct-pr", 2, 1),
        launched("fleet", "direct-pr", 1, 1),
        ...peerProjects(now),
      ],
    };
  }
  if (scenario === "degraded") return DEGRADED_PROJECTS;
  if (scenario === "setup-needed") {
    return { projects: [...DEFAULT_PROJECTS.projects, launched("kiln", "direct-push", 0, 0)] };
  }
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
      "working",
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

// The badges of a card that waits on the owner: an attention kind, or a
// permission prompt (state.md, section 6).
const WAITS_ON_OWNER = new Set<string>([
  "decision",
  "credential",
  "merge",
  "review",
  "recovery",
  "report",
  "permission_prompt",
] satisfies (AttentionItem["kind"] | "permission_prompt")[]);

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
    needs_you: WAITS_ON_OWNER.has(badge),
    parked_until: extras.parkedReason ? minutes(now, 3 * 24 * 60) : null,
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
      "working",
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
      "landing",
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
      "working",
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
// three working (one between runs, one waiting on the owner), two landing (one
// with its approval next) and three recently done.
function busyRows(now: number): BoardRow[] {
  const rows: BoardRow[] = [];
  const plan: [BoardRow["column"], string][] = [
    ["queued", "filed"],
    ["queued", "parked"],
    ["working", "working"],
    ["working", "between_runs"],
    ["working", "decision"],
    ["landing", "checks"],
    ["landing", "ready"],
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
            observedMinutes: column === "working" || column === "landing" ? 0.1 + index / 10 : null,
            parkedReason: badge === "parked" ? "waiting for the release" : null,
          },
          now,
        ),
      );
    });
  });
  return rows;
}

// The federated scenario's Tiller bugs on this host: tiller-7 was routed to
// the bug project, and fleet-2 was filed before that project took them, so it
// still waits in `fleet` beside Host health.
function bugRows(now: number): BoardRow[] {
  return [
    {
      ...card(1, "fleet", "queued", "filed", "Host health", { kind: "ops" }, now),
      id: "01JCARDBUG0000000000000001",
    },
    {
      ...card(2, "fleet", "queued", "filed", "tiller card close drops the reason", {}, now),
      id: "01JCARDBUG0000000000000002",
      category: "tiller-bug",
      filed_from: "beta",
    },
    {
      ...card(7, "tiller", "queued", "filed", "tiller agent stop leaves the pane open", {}, now),
      id: "01JCARDBUG0000000000000007",
      category: "tiller-bug",
      filed_from: "alpha",
    },
  ];
}

// What a Tiller bug's filer wrote, for the fixture bugs that have one.
const BUG_BODIES: Record<string, string> = {
  "tiller pr describe misreads TMPDIR in the builder sandbox":
    "The builder on gamma-14 ran `tiller pr describe` and got an empty body. The sandbox sets TMPDIR to /home/user/.cache/tmp, and the command still reads /tmp.\n\nExpected: the PR body from the card. Seen twice today.",
  "tiller card close drops the reason":
    "`tiller card close beta-8 --outcome cancelled --reason ...` closed the card, and the timeline shows no reason.",
};

// A new install: the first cards are filed and running, and nothing has landed.
function newInstallRows(now: number): BoardRow[] {
  return [
    card(1, "alpha", "working", "working", "Set up the export job", { observedMinutes: 0.2 }, now),
    card(2, "alpha", "queued", "filed", "Document the cutover steps", { kind: "scout" }, now),
  ];
}

// The default board plus a card that waits on another, for the "Blocked by" line.
function blockedRows(now: number): BoardRow[] {
  return [
    ...defaultRows(now),
    card(
      14,
      "gamma",
      "queued",
      "blocked",
      "Rotate the signing key",
      { kind: "ops", blockedBy: "gamma-12" },
      now,
    ),
  ];
}

function boardFor(scenario: Scenario, now: number): BoardResponse {
  if (scenario === "empty") return { rows: [], asked: [] };
  if (scenario === "new-install") return { rows: newInstallRows(now), asked: [] };
  if (scenario === "blocked") return { rows: blockedRows(now), asked: [] };
  if (scenario === "degraded") return { rows: degradedRows(now), asked: [] };
  if (hasPeers(scenario)) {
    return { rows: [...defaultRows(now), ...bugRows(now), ...peerRows(now)], asked: [] };
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
          source: "prompt",
          summary: "Can the card fade on exit instead of sliding?",
        },
        {
          id: "01JASK000000000000000000A2",
          project_id: "gamma",
          session_id: null,
          received_at: minutes(now, -1),
          age_seconds: 60,
          source: "script",
          summary: "Nightly check: the staging certificate expires in 6 days",
        },
      ],
    };
  }
  return {
    rows: defaultRows(now),
    // One untagged ask from each source: typed in the orchestrator's pane,
    // sent from the board's ask box (long enough to be cut), and a script's.
    asked: [
      {
        id: "01JASK000000000000000000A1",
        project_id: "alpha",
        session_id: "01JSESS000000000000000000A",
        received_at: minutes(now, -2),
        age_seconds: 120,
        source: "prompt",
        summary: "Can the card fade on exit instead of sliding?",
      },
      {
        id: "01JASK000000000000000000B1",
        project_id: "beta",
        session_id: null,
        received_at: minutes(now, -6),
        age_seconds: 360,
        source: "board",
        summary:
          "Move the export settings into their own page, keep the old links working, and say in the release notes where each one went",
      },
      {
        id: "01JASK000000000000000000C1",
        project_id: "gamma",
        session_id: null,
        received_at: minutes(now, -14),
        age_seconds: 840,
        source: "script",
        summary: "Nightly check: the staging certificate expires in 6 days",
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
      {
        id: "01JATTN0000000000000000003",
        card_id: cardId("gamma-13"),
        kind: "decision",
        question: "Raise gamma capacity to 2 so the cache trim starts now, or let it wait?",
        evidence: "gamma-12 holds gamma's only slot while it waits on a permission prompt.",
        recommendation: "Raise it. host-a has two free slots.",
        consequence: null,
        options: [
          {
            label: "Raise to 2",
            consequence: "The cache trim starts on the free slot.",
            recommended: true,
            action: { kind: "project_capacity", project: "gamma", from: 1, to: 2 },
          },
          {
            label: "Let it wait",
            consequence: "The cache trim starts when gamma-12 lands.",
            recommended: false,
          },
        ],
        approval: null,
        opened_by_session_id: null,
        opened_at: "2026-11-02T11:40:00.000Z",
        still_current: true,
        answered_at: null,
        withdrawn_at: null,
        rev: 1,
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
  if (!hasPeers(scenario)) return local;
  const peer = peerAttention(now, board.rows);
  return {
    items: [...local.items, ...peer.items],
    permission_prompts: [...local.permission_prompts, ...peer.permission_prompts],
  };
}

function sessionId(key: string): string {
  return `01JSESS${key.replace(/\W/g, "").padEnd(19, "0").slice(0, 19).toUpperCase()}`;
}

// Minutes since a worker's last output: a waiting or idle one spoke when its
// turn ended, and alpha-42 runs a long test suite, so it has gone quiet.
function lastOutputMinutes(row: BoardRow, stale: boolean, index: number): number {
  if (stale) return 22;
  if (workerActivity(row) !== "working") return 8 + index * 3;
  return row.display_id === "alpha-42" ? 12 : 0.3 + index / 10;
}

function workerFor(row: BoardRow, now: number, index: number): AgentRow {
  const role = row.kind === "scout" ? "scout" : "build";
  const stale = row.badge === "stale";
  return {
    session_id: sessionId(`W${row.display_id}`),
    role,
    model: row.model ?? "sonnet",
    activity: workerActivity(row),
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
    last_output_at: minutes(now, -lastOutputMinutes(row, stale, index)),
  };
}

// kiln was registered and its orchestrator never started; the dev and preview
// servers keep a Start orchestrator so the lane shows it running on the next
// read.
const UNSTARTED = new Set(["kiln"]);
const startedOrchestrators = new Set<string>();

// Whether this host's project has an orchestrator running now.
export function orchestratorLive(projectId: string): boolean {
  return !UNSTARTED.has(projectId) || startedOrchestrators.has(projectId);
}

// Starts the project's orchestrator, and says whether one was already live.
// A peer's project is its own: this host's `gamma` running says nothing of
// host-d's, so a forwarded start is live only once it was started here.
export function startOrchestratorFixture(projectId: string, host?: string): { live: boolean } {
  const key = host ? `${host}/${projectId}` : projectId;
  const live = (!host && !UNSTARTED.has(projectId)) || startedOrchestrators.has(key);
  startedOrchestrators.add(key);
  return { live };
}

const FIXTURE_TILLER_VERSION = "0.1.0-pre.21";

// What `restart-stale` did to a fixture session: restarted ones are current
// again, queued ones wait for idle.
const staleRestarts = new Map<string, "restarted" | "queued">();

// Asks the fixture to restart these sessions: an idle one restarts now, a
// working one is queued.
export function restartStaleFixture(ids: string[]): RestartStaleResponse {
  return {
    sessions: ids.map((id) => {
      const idle = id === sessionId("Oalpha");
      staleRestarts.set(id, idle ? "restarted" : "queued");
      return { session_id: id, outcome: idle ? "restarted" : "queued" };
    }),
  };
}

function staleOf(id: string, since = "0.1.0-pre.20"): { stale?: SessionStale } {
  const done = staleRestarts.get(id);
  if (done === "restarted") return {};
  return { stale: { since, prompt_changed: true, restart_queued: done === "queued" } };
}

function agentsFor(
  board: BoardResponse,
  projects: ProjectsResponse,
  now: number,
  scenario: Scenario,
): AgentsResponse {
  if (board.rows.length === 0) return { orchestrators: [] };
  const staleTiller = scenario === "stale-sessions";
  const peerProjectRows = projects.projects.filter(isPeerRow);
  const localOrchestrators = projects.projects
    .filter((p) => !isPeerRow(p) && (!UNSTARTED.has(p.id) || startedOrchestrators.has(p.id)))
    .map((project, projectIndex) => {
      const live = board.rows.filter(
        (row) =>
          !isPeerRow(row) &&
          row.project_id === project.id &&
          (row.column === "working" || row.column === "landing"),
      );
      const host = live.find((row) => row.host_id)?.host_id ?? HOST_A;
      // `degraded`: beta's orchestrator last spoke 41 minutes ago.
      const quiet = scenario === "degraded" && project.id === "beta";
      return {
        session_id: sessionId(`O${project.id}`),
        role: "orchestrator",
        model: "opus",
        activity: staleTiller && projectIndex === 0 ? "idle" : "working",
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
        last_output_at: minutes(now, quiet ? -41 : -(0.2 + projectIndex)),
        ...(staleTiller && projectIndex < 2 ? staleOf(sessionId(`O${project.id}`)) : {}),
      };
    });
  return {
    orchestrators: [
      ...localOrchestrators,
      ...peerAgents(now, board.rows, peerProjectRows, staleTiller ? staleOf : undefined),
    ],
    tiller_version: FIXTURE_TILLER_VERSION,
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
  const active = row.column === "working" || row.column === "landing";
  const role = row.kind === "scout" ? "scout" : "build";
  const sha = fakeSha(row.id);
  const prs: CardPr[] = row.pr_number
    ? [
        {
          id: `01JPR${String(row.pr_number).padStart(21, "0")}`,
          number: row.pr_number,
          repo: `example/${row.project_id}`,
          url: `https://github.com/example/${row.project_id}/pull/${row.pr_number}`,
          title: row.title,
          state: done ? "merged" : "open",
          head_sha: row.id.endsWith("44") ? FIXTURE_HEAD_SHA : sha,
          base: "main",
          checks: row.badge === "checks" ? "pending" : "passing",
          checks_summary: row.badge === "checks" ? "2 of 4 checks running" : "4 of 4 checks green",
          diff_summary: "+142 -37, 6 files",
        },
      ]
    : [];
  // The first landing card carries a stale approval so the screens show it.
  const stale = row.id.endsWith("43");
  const waiting = row.id.endsWith("44");
  const approvals = prs.map((pr) => ({
    id: waiting ? "01JAPPR0000000000000000000" : `01JAPPR${pr.id.slice(-19)}`,
    kind: "merge",
    // alpha-44 waits on the owner: the approval the Inbox shows for it.
    state: waiting ? "requested" : stale ? "accepted" : done ? "succeeded" : "accepted",
    pr_id: pr.id,
    head_sha: stale ? fakeSha(`${row.id}-old`) : pr.head_sha,
    decided_at: waiting ? null : minutes(now, -80),
    decided_by: waiting ? null : "owner",
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
            activity: workerActivity(row),
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
      body:
        BUG_BODIES[row.title] ??
        "A placeholder description for this fixture card. It says what the card is for in a sentence or two.",
      priority: row.priority,
      slug: slugOf(row.display_id, row.title),
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
        ? [
            {
              id: "01JHOLD0000000000000000001",
              until_at: row.parked_until,
              reason: row.parked_reason,
            },
          ]
        : [],
    deps: row.blocked_by ? [{ display_id: row.blocked_by, released_at: null }] : [],
    attention: waiting
      ? attentionFor("default", board, now).items.filter(
          (item) => item.card_id === row.id && item.approval,
        )
      : [],
    runs,
    prs,
    approvals,
    // A card with a PR has had a mockup made for it.
    artifacts: prs.length
      ? [
          {
            id: `01JART${row.id.slice(-20)}`,
            kind: "page",
            title: `Mockup for ${row.title}`,
            url: `https://pages.example.com/${row.display_id}/mockup.html`,
            created_at: minutes(now, -90),
          },
        ]
      : [],
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

// The display id, then whole title words up to 28 characters, as the daemon makes it.
function slugOf(displayId: string, title: string): string {
  const words = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  let kept = "";
  for (const word of words.split("-")) {
    const joined = kept === "" ? word : `${kept}-${word}`;
    if (joined.length > 28) {
      if (kept === "") kept = word.slice(0, 28);
      break;
    }
    kept = joined;
  }
  return `${displayId}-${kept}`;
}

function timelineFor(
  board: BoardResponse,
  cardId: string,
  before: number | null,
  now: number = Date.now(),
): TimelineResponse | null {
  const row = board.rows.find((candidate) => candidate.id === cardId);
  if (!row) return null;
  if (row.category === "tiller-bug") {
    const into =
      row.project_id === "fleet" ? "fleet: this host has no bug project" : row.project_id;
    const from = row.filed_from ? ` from ${row.filed_from}` : "";
    return {
      entries: [
        {
          seq: 4100,
          at: minutes(now, -40),
          actor: "system",
          text: `filed as a Tiller bug${from} into ${into}`,
        },
      ],
      next_before: null,
    };
  }
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
    [signalsPath]: signalsFor(
      scenario === "health-bad",
      now,
      scenario === "new-install" ? "new-install" : scenario === "empty" ? "no-events" : "full",
    ),
    [awayBriefPath]: awayFor(
      scenario === "away" ? "running" : scenario === "away-back" ? "back" : "idle",
    ),
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

// The PRs the local board rows carry, newest first; `project` narrows them.
function contributionsFor(
  board: BoardResponse,
  project: string | null,
  now: number,
): ContributionsResponse {
  const rows = board.rows.filter(
    (row) => row.pr_number && !row.host && (project === null || row.project_id === project),
  );
  return {
    contributions: rows.map((row, index) => {
      const number = row.pr_number ?? 0;
      const merged = row.column === "done";
      return {
        id: `01JPR${String(number).padStart(21, "0")}`,
        project_id: row.project_id,
        card_id: row.id,
        card_display_id: row.display_id,
        card_title: row.title,
        repo: `example/${row.project_id}`,
        number,
        url: `https://github.com/example/${row.project_id}/pull/${number}`,
        state: merged ? "merged" : "open",
        linked_at: minutes(now, -60 * (index + 1)),
        merged_at: merged ? minutes(now, -30 * (index + 1)) : null,
      };
    }),
  };
}

// The running agents of a fixture project, as a delete or archive would stop
// them: the orchestrator first, then each worker in board order.
function liveAgentsFor(
  scenario: Scenario,
  board: BoardResponse,
  projectId: string,
  now: number,
): ProjectAgent[] {
  const orchestrator = agentsFor(
    board,
    projectsFor(scenario, now),
    now,
    scenario,
  ).orchestrators.find((agent) => agent.project_id === projectId && !isPeerRow(agent));
  if (!orchestrator) return [];
  const display = new Map(board.rows.map((row) => [row.id, row.display_id]));
  return [orchestrator, ...orchestrator.workers].map((agent) => ({
    role: agent.role === "build" ? "builder" : agent.role,
    session: agent.session_id,
    card: agent.card_id ? (display.get(agent.card_id) ?? null) : null,
    run: agent.card_id ? (agent.run_id ?? null) : null,
    local: true,
  }));
}

// What deleting a fixture project would remove, counted from its board rows,
// and the agents it stops first: a working card's builder among them.
function deletePreviewFor(
  scenario: Scenario,
  board: BoardResponse,
  projectId: string,
  now: number,
): ProjectDeletePreview | undefined {
  const known = projectsFor(scenario, now).projects.some(
    (project) => project.id === projectId && !project.host,
  );
  if (!known) return undefined;
  const rows = board.rows.filter((row) => row.project_id === projectId && !row.host);
  const started = rows.filter((row) => row.column !== "queued");
  const working = rows.filter((row) => row.column === "working");
  return {
    project: projectId,
    removed: {
      projects: 1,
      cards: rows.length,
      runs: started.length,
      sessions: started.length + 1,
      attention: rows.filter((row) => row.needs_you).length,
      artifacts: started.length * 2,
      memories: 3,
      memory_revisions: 4,
      events: rows.length * 9 + 6,
    },
    unlinked: projectId === "gamma" ? { "cards.fixes_card_id": 1 } : {},
    refused: null,
    unblock: null,
    agents: liveAgentsFor(scenario, board, projectId, now),
    unlanded: working.map((row, index) => ({
      path: `/home/user/worktrees/${row.display_id}`,
      branch: `tiller/${row.display_id}`,
      card: row.display_id,
      unlanded_commits: index + 2,
      dirty: index === 0,
    })),
    open_prs: rows
      .filter((row) => row.pr_number && row.column !== "done")
      .map((row) => ({
        card: row.display_id,
        number: row.pr_number ?? 0,
        url: `https://github.com/example/${projectId}/pull/${row.pr_number}`,
      })),
  };
}

// Whether archiving a fixture project applies now: refused while a card of it
// is open, as `project.retire` is.
function retirePreviewFor(
  scenario: Scenario,
  board: BoardResponse,
  projectId: string,
  now: number,
): ProjectRetirePreview | undefined {
  const known = projectsFor(scenario, now).projects.some(
    (project) => project.id === projectId && !project.host,
  );
  if (!known) return undefined;
  const open = board.rows.find(
    (row) => row.project_id === projectId && !row.host && row.column !== "done",
  );
  return {
    project: projectId,
    refused: open ? `card ${open.display_id} is open` : null,
    agents: liveAgentsFor(scenario, board, projectId, now),
  };
}

// The stand-in host's `spawn.checkouts_root` and home, where the fixture's
// own projects are checked out as `example/<id>`.
const CHECKOUTS_ROOT = "/home/user/src";
const FIXTURE_HOME = "/home/user";
const FIXTURE_CHECKOUTS = new Set(["alpha", "beta", "gamma"]);

const problem = (text: string): CheckoutCheck => ({
  path: null,
  plan: null,
  remote: null,
  problem: text,
});

// `GET /api/v1/projects/checkout`, by the daemon's rules for the folder.
function checkoutFor(url: URL): CheckoutCheck {
  const project = url.searchParams.get("project") ?? "";
  const repo = url.searchParams.get("repo") ?? "";
  const given = url.searchParams.get("path") ?? "";
  const clone = url.searchParams.get("clone") === "true";
  if (!given && !/^[a-z][a-z0-9]*(-[a-z][a-z0-9]*)*$/.test(project)) {
    return problem("fill in the id first: the folder is named after it");
  }
  const path = !given
    ? `${CHECKOUTS_ROOT}/${project}`
    : given.startsWith("~/")
      ? `${FIXTURE_HOME}/${given.slice(2)}`
      : given.replace(/\/+$/, "");
  if (!path.startsWith("/")) return problem(`${given} is not a full path; start it with / or ~/`);
  if (path.split("/").includes("..")) {
    return problem(`${given} has a \`..\` in it; give the folder without one`);
  }
  if (!path.startsWith(`${CHECKOUTS_ROOT}/`)) {
    return problem(
      `${path} is not under spawn.checkouts_root (${CHECKOUTS_ROOT}), where the orchestrator looks for checkouts`,
    );
  }
  const name = path.slice(CHECKOUTS_ROOT.length + 1);
  const existing = FIXTURE_CHECKOUTS.has(name) ? `example/${name}` : null;
  if (!clone) {
    if (!existing) return problem(`nothing is at ${path}; choose Clone to make it`);
    if (repo && repo.toLowerCase() !== existing) {
      return problem(`${path} is a checkout of ${existing}, not ${repo}`);
    }
    return { path, plan: "use", remote: null, problem: null };
  }
  if (!repo) return problem("a clone needs the project's repo; fill in Repo as owner/name");
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repo)) {
    return problem(`repo ${repo} is not owner/name, like acme/web`);
  }
  if (existing === repo.toLowerCase()) return { path, plan: "use", remote: null, problem: null };
  if (existing) return problem(`${path} is not empty; give a new or empty folder to clone into`);
  return { path, plan: "clone", remote: `https://github.com/${repo}.git`, problem: null };
}

// Anything the stand-in server can answer: the board's lists, a card, its
// timeline, a run. `undefined` means no such resource (a 404).
function healthFor(now: number): DaemonHealth {
  return {
    ok: true,
    version: FIXTURE_VERSION,
    initialized: true,
    schema: 12,
    pid: 4242,
    started_at: minutes(now, -3 * 60),
    merge_app: {
      status: "missing",
      app_id: null,
      detail:
        "Create the merge App and put its app-id and private-key.pem in /home/user/.tiller/forge-app (docs/plan/merge-app-setup.md). Until then every project runs as detect.",
    },
  };
}

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
  if (url.pathname === "/api/v1/contributions") {
    return contributionsFor(board, url.searchParams.get("project"), now);
  }
  const deletion = /^\/api\/v1\/projects\/([^/]+)\/delete$/.exec(url.pathname);
  if (deletion?.[1]) return deletePreviewFor(served, board, deletion[1], now);
  const retire = /^\/api\/v1\/projects\/([^/]+)\/retire$/.exec(url.pathname);
  if (retire?.[1]) return retirePreviewFor(served, board, retire[1], now);
  const run = /^\/api\/v1\/runs\/([^/]+)$/.exec(url.pathname);
  if (run?.[1]) return runDetailFor(run[1], now);
  if (url.pathname === recapPath) return recapFor(served, now, null);
  if (url.pathname === "/api/v1/health") return healthFor(now);
  if (url.pathname === "/api/v1/projects/checkout") return checkoutFor(url);
  if (url.pathname.startsWith("/api/v1/memory")) return memoryResource(url);
  // No project starts archived; the stand-in server adds the ones archived since.
  if (url.pathname === projectsPath && url.searchParams.has("retired")) return { projects: [] };
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
    ...(hasPeers(scenario) ? { peers: peerSnapshot(now) } : {}),
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
