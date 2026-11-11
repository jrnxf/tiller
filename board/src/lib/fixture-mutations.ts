// Stands in for the daemon's mutations (protocol.md section 7.4) on the dev and
// preview servers until T2.8 lands: a keyed replay table, the stale-head
// refusal, and the later receipt stages as change events.
import type { BoardRow, ChangeEvent, Machine, Project, PromptDialog } from "./api-types.ts";
import type {
  AgentsResponse,
  AttentionResponse,
  BoardResponse,
  CardDep,
  CardDetail,
  HostFields,
  InterventionKind,
  ProjectDeletePreview,
  ProjectFields,
  ProjectRetirePreview,
  ProjectsResponse,
  QuotaResponse,
} from "./api-types.ts";
import { awayBrief } from "./fixtures-away.ts";
import { grantMutation, grantsFor } from "./fixtures-grants.ts";
import { memoryMutation } from "./fixtures-memory.ts";
import { RUN_MS, retroMutation } from "./fixtures-retros.ts";
import {
  DIALOG_SESSIONS,
  FIXTURE_HEAD_SHA,
  fixtureResource,
  restartStaleFixture,
  fixtureResponses,
  orchestratorLive,
  orchestratorSession,
  startOrchestratorFixture,
} from "./fixtures.ts";
import { cancelCard, changeSession, sessionDrafted, sessionStopped } from "./fixtures-lifecycle.ts";
import { runPollerFixture } from "./fixture-doctor.ts";
import type { Scenario } from "./fixtures.ts";
import { relabelFixture } from "./fixtures-signals.ts";
import {
  hostConfigFor,
  projectSetupFor,
  setConfigFixture,
  startProblems,
  startSettingsFor,
} from "./fixture-settings.ts";
import { backupsFor, takeBackup } from "./fixtures-backups.ts";
import { anyUnpaired, peerLinkMutation, peersFor, withoutUnpaired } from "./fixtures-peers.ts";
import { recapFor } from "./fixtures-recap.ts";
import { markReplyRead } from "./fixtures-replies.ts";
import {
  agentsPath,
  attentionPath,
  boardPath,
  machinesPath,
  projectsPath,
  recapPath,
} from "./queries.ts";

interface Reply {
  status: number;
  body: unknown;
}

// A change event the server sends `delayMs` after the reply.
interface FollowUp {
  delayMs: number;
  event: Omit<ChangeEvent, "seq" | "at">;
}

export interface MutationResult {
  reply: Reply;
  followUps: FollowUp[];
}

const replies = new Map<string, MutationResult>();
let counter = 0;

function fakeId(prefix: string): string {
  counter += 1;
  return `${prefix}${String(counter).padStart(26 - prefix.length, "0")}`;
}

function refused(status: number, code: string, message: string, current?: object): Reply {
  return { status, body: { ok: false, error: { code, message, current } } };
}

function recorded(data: unknown, outboxId: string | null, approvalId: string | null): Reply {
  return {
    status: 200,
    body: {
      ok: true,
      replayed: false,
      events: [],
      receipt: {
        recorded_at: new Date().toISOString(),
        outbox_id: outboxId,
        approval_id: approvalId,
      },
      data,
    },
  };
}

function outboxStages(id: string): FollowUp[] {
  const event = (state: string) => ({
    kind: `outbox.${state}`,
    txn: `outbox.${state === "delivered" ? "deliver" : "ack"}`,
    entity: "outbox",
    entity_id: id,
    rev_after: null,
    card_id: null,
    entity_state: { id, state },
  });
  return [
    { delayMs: 1200, event: event("delivered") },
    { delayMs: 3200, event: event("acked") },
  ];
}

function approvalStages(id: string, decision: string): FollowUp[] {
  const event = (state: string) => ({
    kind: `approval.${state}`,
    txn: `approval.${state}`,
    entity: "approval",
    entity_id: id,
    rev_after: null,
    card_id: null,
    entity_state: { id, state },
  });
  if (decision === "reject") return [{ delayMs: 400, event: event("rejected") }];
  return [
    { delayMs: 1500, event: event("executing") },
    { delayMs: 4000, event: event("succeeded") },
  ];
}

// The item leaves the open queue once it is answered.
function answered(attentionId: string): FollowUp {
  const at = new Date().toISOString();
  return {
    delayMs: 300,
    event: {
      kind: "attention.answered",
      txn: "attention.answer",
      entity: "attention",
      entity_id: attentionId,
      rev_after: null,
      card_id: null,
      entity_state: { id: attentionId, answered_at: at, withdrawn_at: null },
    },
  };
}

function attentionFor(approvalId: string): string | null {
  const queue = fixtureResponses()[attentionPath] as AttentionResponse;
  return queue.items.find((item) => item.approval?.id === approvalId)?.id ?? null;
}

// The projects the owner has switched since the server started, so a second
// switch sends the new `rev` and a stale one is refused.
const switched = new Map<string, Project>();

function currentProject(id: string): Project | undefined {
  const known = (fixtureResponses("busy")[projectsPath] as ProjectsResponse).projects;
  return switched.get(id) ?? known.find((project) => project.id === id);
}

// The projects deleted or archived since the server started: the stand-in
// server leaves them out of every read it answers, except an archived one's
// row on the archived list and its delete. `retired` maps each to when it
// was archived. `stopped` holds the projects whose agents a delete or
// archive stopped.
const deleted = new Set<string>();
const retired = new Map<string, string>();
const stopped = new Set<string>();
// When the owner last marked the recap seen, since the server started.
let recapMarkedAt: string | null = null;

// The projects added since the server started, listed after the fixture's.
const added = new Map<string, Project>();

// The fields the Edit tab saved since the server started, by project.
const edited = new Map<string, ProjectFields>();

// One of this host's projects, not deleted or archived.
function localProject(id: string): boolean {
  const project = added.get(id) ?? currentProject(id);
  return !!project && !project.host && !gone(id);
}

// `GET /api/v1/projects/{id}`: one of this host's projects as the Edit tab
// reads it. alpha sets every field, so the dialog opens on all of them.
function projectFieldsFor(id: string): ProjectFields | undefined {
  const project = added.get(id) ?? currentProject(id);
  if (!project || project.host || gone(id)) return undefined;
  const saved = edited.get(id);
  if (saved) return { ...saved, rev: project.rev };
  const alpha = id === "alpha";
  return {
    id,
    name: project.name,
    repo: id === "fleet" ? null : `example/${id}`,
    remote: alpha ? "git@example.com:example/alpha.git" : null,
    default_branch: "main",
    delivery: project.delivery,
    checks: alpha
      ? { full: ["cargo nextest run --workspace", "bun run --cwd board test"], no_logic: [] }
      : [],
    capacity: project.capacity,
    deploy_workflow: alpha ? "deploy.yml" : null,
    deploy_environment: alpha ? "production" : null,
    deploy_timeout_minutes: alpha ? 20 : 60,
    rev: project.rev,
  };
}

const DELIVERIES = [
  "direct-pr",
  "local-only",
  "no-mistakes",
  "no-mistakes-prod-only",
  "direct-push",
];

// The fields an edit sets, in the order the daemon names the first changed one.
const EDIT_FIELDS = [
  "capacity",
  "checks",
  "default_branch",
  "delivery",
  "deploy_environment",
  "deploy_timeout_minutes",
  "deploy_workflow",
  "name",
  "remote",
  "repo",
] as const;

function commandList(value: unknown): value is string[] {
  return (
    Array.isArray(value) &&
    value.every((command) => typeof command === "string" && command.trim() !== "")
  );
}

// `checks` is a list of commands, or `full` with an optional `no_logic`.
function checksProblem(value: unknown): string | null {
  if (commandList(value)) return null;
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const plan = value as Record<string, unknown>;
    const unknown = Object.keys(plan).find((key) => key !== "full" && key !== "no_logic");
    if (unknown) return `checks has an unknown key "${unknown}"`;
    if ("full" in plan && !commandList(plan.full)) return "checks.full must be a list of commands";
    if ("no_logic" in plan && !commandList(plan.no_logic)) {
      return "checks.no_logic must be a list of commands";
    }
    return null;
  }
  return "checks must be a list of commands";
}

function positive(value: unknown): boolean {
  return typeof value === "number" && Number.isInteger(value) && value >= 1;
}

// `project.edit`, as `tiller project edit` runs it: the `rev` first, then each
// field checked, and an empty text clears an optional one.
function editProject(id: string, body: Record<string, unknown>): MutationResult {
  const fields = projectFieldsFor(id);
  const project = added.get(id) ?? currentProject(id);
  if (!fields || !project) return refusal(404, "not_found", `no project ${id}`);
  if (body.rev !== fields.rev) {
    const first = EDIT_FIELDS.find(
      (field) => field in body && JSON.stringify(body[field]) !== JSON.stringify(fields[field]),
    );
    return {
      reply: refused(
        409,
        "stale",
        `conflict: project ${id} is at rev ${fields.rev}, you sent ${String(body.rev)}`,
        {
          rev: fields.rev,
          ...(first ? { [first]: fields[first] } : {}),
          ...(project.edited_at ? { edited_at: project.edited_at } : {}),
        },
      ),
      followUps: [],
    };
  }
  const text = (field: string) => {
    const value = body[field];
    return typeof value === "string" ? value.trim() : null;
  };
  if ("name" in body && !text("name")) return refusal(400, "usage", "project name is required");
  if ("default_branch" in body && !text("default_branch")) {
    return refusal(400, "usage", "default branch is required");
  }
  if ("delivery" in body && !DELIVERIES.includes(String(body.delivery))) {
    return refusal(400, "usage", `unknown delivery "${String(body.delivery)}"`);
  }
  if ("capacity" in body && !positive(body.capacity)) {
    return refusal(400, "usage", "capacity must be at least 1");
  }
  if ("deploy_timeout_minutes" in body && !positive(body.deploy_timeout_minutes)) {
    return refusal(400, "usage", "deploy timeout must be at least 1 minute");
  }
  const problem = "checks" in body ? checksProblem(body.checks) : null;
  if (problem) return refusal(400, "usage", problem);
  const optional = (field: "repo" | "remote" | "deploy_workflow" | "deploy_environment") =>
    field in body ? text(field) || null : fields[field];
  const next: ProjectFields = {
    ...fields,
    name: text("name") ?? fields.name,
    repo: optional("repo"),
    remote: optional("remote"),
    default_branch: text("default_branch") ?? fields.default_branch,
    delivery: "delivery" in body ? String(body.delivery) : fields.delivery,
    checks: "checks" in body ? (body.checks as ProjectFields["checks"]) : fields.checks,
    capacity: "capacity" in body ? Number(body.capacity) : fields.capacity,
    deploy_workflow: optional("deploy_workflow"),
    deploy_environment: optional("deploy_environment"),
    deploy_timeout_minutes:
      "deploy_timeout_minutes" in body
        ? Number(body.deploy_timeout_minutes)
        : fields.deploy_timeout_minutes,
    rev: fields.rev + 1,
  };
  const direct = next.delivery === "no-mistakes" || next.delivery === "no-mistakes-prod-only";
  const row: Project = {
    ...project,
    name: next.name,
    delivery: next.delivery,
    capacity: next.capacity,
    rev: next.rev,
    edited_at: new Date().toISOString(),
    effective_delivery: project.stage === "pre-launch" && direct ? "direct-pr" : next.delivery,
  };
  edited.set(id, next);
  if (added.has(id)) added.set(id, row);
  else switched.set(id, row);
  return {
    reply: recorded(row, null, null),
    followUps: [
      {
        delayMs: 100,
        event: {
          kind: "project.edited",
          txn: "project.edit",
          entity: "project",
          entity_id: id,
          rev_after: row.rev,
          card_id: null,
          entity_state: row,
        },
      },
    ],
  };
}

// A read the stand-in server answers, without the deleted projects' rows; a
// deleted or archived project's own previews are gone (a 404).
export function withoutDeleted(path: string, body: unknown, scenario: Scenario): unknown {
  const pathname = new URL(path, "http://localhost").pathname;
  const triaged = withUnpaired(pathname, withTriage(pathname, body, scenario));
  if (triaged !== body) return triaged;
  const depended = withDeps(pathname, body, scenario);
  if (depended !== body) return depended;
  if (pathname === recapPath && recapMarkedAt !== null) {
    return recapFor(scenario, Date.now(), recapMarkedAt);
  }
  if (pathname === "/api/v1/start-settings") return startSettingsFor(scenario);
  if (pathname === "/api/v1/host-config") return hostConfigFor();
  if (pathname === "/api/v1/host") return hostFieldsFor(scenario);
  if (pathname === "/api/v1/backups") return backupsFor(scenario);
  if (pathname === "/api/v1/peers") return peersFor(scenario);
  const setup = /^\/api\/v1\/projects\/([^/]+)\/setup$/.exec(pathname);
  if (setup?.[1] && (body !== undefined || !gone(setup[1]))) {
    const saved = edited.get(setup[1]);
    const fresh = projectSetupFor(setup[1], scenario);
    return saved ? { ...fresh, repo: saved.repo } : fresh;
  }
  const fields = /^\/api\/v1\/projects\/([^/]+)$/.exec(pathname);
  if (fields?.[1] && body === undefined) return projectFieldsFor(fields[1]);
  const grants = /^\/api\/v1\/projects\/([^/]+)\/grants$/.exec(pathname);
  if (grants?.[1]) return localProject(grants[1]) ? grantsFor(grants[1]) : undefined;
  if (deleted.size + retired.size + added.size + switched.size === 0 || body === undefined) {
    return body;
  }
  const preview = /^\/api\/v1\/projects\/([^/]+)\/(delete|retire)$/.exec(pathname);
  if (preview?.[1] && (preview[2] === "delete" ? deleted.has(preview[1]) : gone(preview[1]))) {
    return undefined;
  }
  if (
    pathname === "/api/v1/projects" &&
    new URL(path, "http://localhost").searchParams.has("retired")
  ) {
    return {
      projects: [...retired]
        .filter(([id]) => !deleted.has(id))
        .flatMap(([id, at]) => {
          const project = added.get(id) ?? currentProject(id);
          return project ? [{ ...project, retired_at: at }] : [];
        }),
    };
  }
  if (pathname === "/api/v1/projects") {
    const { projects } = body as ProjectsResponse;
    return {
      projects: [
        ...projects
          .filter((project) => project.host || !gone(project.id))
          .map((project) => (project.host ? project : (switched.get(project.id) ?? project))),
        ...[...added.values()].filter((project) => !gone(project.id)),
      ],
    };
  }
  if (pathname === "/api/v1/agents") {
    const { orchestrators } = body as AgentsResponse;
    return {
      orchestrators: orchestrators.filter(
        (agent) => agent.host || !agent.project_id || !stopped.has(agent.project_id),
      ),
    };
  }
  if (pathname === "/api/v1/board") {
    const board = body as BoardResponse;
    return {
      ...board,
      rows: board.rows.filter((row) => row.host || !deleted.has(row.project_id)),
    };
  }
  if (pathname === "/api/v1/attention") {
    // An item names its card, and the card its project.
    const board = fixtureResource("/api/v1/board?filter=all", scenario) as BoardResponse;
    const cards = new Set(
      board.rows.filter((row) => deleted.has(row.project_id)).map((row) => row.id),
    );
    const attention = body as AttentionResponse;
    return {
      items: attention.items.filter((item) => item.host || !cards.has(item.card_id)),
      permission_prompts: attention.permission_prompts.filter(
        (prompt) => prompt.host || !cards.has(prompt.card_id),
      ),
    };
  }
  return body;
}

const lisbonHour = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  hour: "numeric",
  hourCycle: "h23",
});

// 06:00 Lisbon on the UTC day of `day`: 06:00 UTC in winter, 05:00 in summer.
function sixLisbon(day: Date): Date {
  const at = new Date(Date.UTC(day.getUTCFullYear(), day.getUTCMonth(), day.getUTCDate(), 6));
  const offset = Number(lisbonHour.format(at)) - 6;
  return new Date(at.getTime() - offset * 3_600_000);
}

// The next Saturday at 06:00 Lisbon after `from`.
function nextSaturday(from: Date): string {
  const day = new Date(from);
  day.setUTCDate(day.getUTCDate() + ((6 - day.getUTCDay() + 7) % 7));
  let next = sixLisbon(day);
  if (next.getTime() <= from.getTime()) {
    day.setUTCDate(day.getUTCDate() + 7);
    next = sixLisbon(day);
  }
  return next.toISOString();
}

function restaged(project: Project, stage: Project["stage"]): Project {
  const at = new Date();
  const direct = project.delivery === "no-mistakes" || project.delivery === "no-mistakes-prod-only";
  const prelaunch = stage === "pre-launch";
  return {
    ...project,
    rev: project.rev + 1,
    stage,
    effective_delivery: prelaunch && direct ? "direct-pr" : project.delivery,
    reviewed_through_sha: project.reviewed_through_sha ?? FIXTURE_HEAD_SHA,
    stage_changed_at: at.toISOString(),
    edited_at: at.toISOString(),
    next_review_due: prelaunch ? nextSaturday(at) : null,
    review_overdue: false,
    review_open: prelaunch ? null : "final",
  };
}

// The paired peers a mutation can name (see `fixtures-federated.ts`): host-e is
// down, host-f only reads, and a comment, title or answer on host-d that says
// "uncertain" is sent and never confirmed (protocol.md section 7.4). The retry
// keeps its key and is answered.
const unsure = new Set<string>();

function gone(id: string): boolean {
  return deleted.has(id) || retired.has(id);
}

// The event that takes a deleted or archived project off every open board.
function projectGone(kind: string, txn: string, id: string): FollowUp {
  return {
    delayMs: 100,
    event: {
      kind,
      txn,
      entity: "project",
      entity_id: id,
      rev_after: null,
      card_id: null,
      entity_state: null,
    },
  };
}

// `project.add`: an archived project keeps its id taken, a deleted one frees it.
function addProject(body: Record<string, unknown>): MutationResult {
  const id = typeof body.id === "string" ? body.id : "";
  // An absent or empty name is the id, as the daemon reads it.
  const name = (typeof body.name === "string" ? body.name.trim() : "") || id;
  if (!/^[a-z][a-z0-9]*(-[a-z][a-z0-9]*)*$/.test(id) || id.length < 2) {
    return refusal(
      400,
      "usage",
      `project id "${id}" must be words of lowercase letters and digits`,
    );
  }
  if (id === "fleet" || added.has(id) || (currentProject(id) && !deleted.has(id))) {
    return refusal(422, "refused", `project ${id}: project id is taken`);
  }
  const delivery = String(body.delivery);
  const project: Project = {
    id,
    name,
    delivery,
    capacity: typeof body.capacity === "number" ? body.capacity : 2,
    rev: 1,
    stage: "launched",
    effective_delivery: delivery,
    reviewed_through_sha: null,
    reviewed_at: null,
    stage_changed_at: null,
    edited_at: new Date().toISOString(),
    next_review_due: null,
    review_overdue: false,
    review_open: null,
  };
  added.set(id, project);
  return {
    reply: recorded(project, null, null),
    followUps: [
      {
        delayMs: 100,
        event: {
          kind: "project.added",
          txn: "project.add",
          entity: "project",
          entity_id: id,
          rev_after: 1,
          card_id: null,
          entity_state: project,
        },
      },
    ],
  };
}

// tiller_core::not_initialized(): init-hint.test.ts keeps the two equal.
const NOT_INITIALIZED =
  "state.db is not initialized; run tiller owner set tiller, then tiller init";

function refusal(status: number, code: string, message: string): MutationResult {
  return { reply: refused(status, code, message), followUps: [] };
}

function forwarded(
  method: string,
  path: string,
  body: Record<string, unknown>,
): MutationResult | null {
  const host = body.host;
  if (typeof host !== "string") return null;
  if (
    /\/(stage|focus|label|delete|retire|bug-project|grants)$/.test(path) ||
    path.startsWith("/api/v1/grants/") ||
    path.startsWith("/api/v1/peers/") ||
    path.startsWith("/api/v1/peer-keys/") ||
    path === projectsPath ||
    (method === "PATCH" && /^\/api\/v1\/projects\/[^/]+$/.test(path)) ||
    /\/memory\//.test(path) ||
    path.startsWith("/api/v1/pollers/")
  ) {
    return refusal(502, "peer_refused", `${method} ${path} is not forwarded to ${host}`);
  }
  if (host === "host-e") {
    return refusal(503, "peer_unreachable", `host-e is unreachable since the link went down`);
  }
  if (host === "host-f") {
    return refusal(502, "peer_refused", "host-f paired with a read scope: view only pairing");
  }
  const key = typeof body.idempotency_key === "string" ? body.idempotency_key : null;
  const text = [body.text, body.title, body.answer].find((value) => typeof value === "string");
  if (key && host === "host-d" && /uncertain/i.test(String(text)) && !unsure.has(key)) {
    unsure.add(key);
    return refusal(504, "forward_uncertain", "host-d did not confirm in time");
  }
  return null;
}

// Tiller bug triage on the stand-in server: the cards a Move made, and the
// cards it or "Close as not a bug" closed. Every card the fixtures know is in
// the federated scenario.
const madeCards = new Map<string, BoardRow>();
const madeBodies = new Map<string, string>();
const closedCards = new Map<string, BoardRow>();

// This host's row as the owner changed it: the bug project Triage picks and
// the fields Settings > Hosts edits, under the one host `rev` both bump.
interface HostOverlay {
  rev: number;
  bug_project?: string | null;
  label?: string;
  lane_capacity?: number;
  ram_floor_mb?: number | null;
  disk_floor_mb?: number | null;
  boundary?: "personal" | "work";
}
let hostOverlay: HostOverlay | null = null;
// The first Move that closes a host-d card is sent and never confirmed, so the
// board shows the partial state; the retry, with the same key, closes it.
const unsureCloses = new Set<string>();
let cardNumber = 20;

function knownCard(id: string): BoardRow | undefined {
  const board = fixtureResponses("federated")[boardPath()] as BoardResponse;
  return madeCards.get(id) ?? board.rows.find((row) => row.id === id);
}

function localMachine(scenario: Scenario): Machine | undefined {
  const { machines } = fixtureResponses(scenario)[machinesPath] as { machines: Machine[] };
  const local = machines.find((machine) => machine.kind === "local");
  if (!local || !hostOverlay) return local;
  const overlay = hostOverlay;
  const capacity = overlay.lane_capacity ?? local.capacity;
  const busy = lanesBusy(local);
  return {
    ...local,
    rev: overlay.rev,
    label: overlay.label ?? local.label,
    capacity,
    free_slots: capacity === null ? local.free_slots : capacity - busy,
    ...(overlay.bug_project === undefined ? {} : { bug_project: overlay.bug_project }),
  };
}

// The lanes this host's agents hold: its capacity less its free slots.
function lanesBusy(machine: Machine): number {
  return machine.capacity === null || machine.free_slots === null
    ? 0
    : machine.capacity - machine.free_slots;
}

// What the stand-in host has free, as a host reads it.
const FIXTURE_RAM_FREE_MB = 21 * 1024 + 310;
const FIXTURE_DISK_FREE_MB = 140 * 1024 + 520;

// `GET /api/v1/host`. The default host already keeps memory, so its boundary
// stays; the empty one has nothing that records it yet.
function hostFieldsFor(scenario: Scenario): HostFields | undefined {
  const machine = localMachine(scenario);
  if (!machine) return undefined;
  const overlay = hostOverlay ?? { rev: machine.rev };
  return {
    id: machine.id,
    label: machine.label,
    boundary: overlay.boundary ?? "personal",
    boundary_recorded_by: scenario === "empty" ? null : "memory",
    lane_capacity: machine.capacity,
    ram_floor_mb: overlay.ram_floor_mb === undefined ? 8 * 1024 : overlay.ram_floor_mb,
    disk_floor_mb: overlay.disk_floor_mb === undefined ? 20 * 1024 : overlay.disk_floor_mb,
    lanes_busy: lanesBusy(machine),
    ram_free_mb: FIXTURE_RAM_FREE_MB,
    disk_free_mb: FIXTURE_DISK_FREE_MB,
    rev: machine.rev,
  };
}

// The `host.edited` event a save sends, with the Machines row it leaves.
function hostEdited(scenario: Scenario, id: string): FollowUp {
  const next = localMachine(scenario);
  return {
    delayMs: 100,
    event: {
      kind: "host.edited",
      txn: "host.edit",
      entity: "host",
      entity_id: id,
      rev_after: next?.rev ?? null,
      card_id: null,
      entity_state: next,
    },
  };
}

const LABEL = /^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/;

// A floor in the body: absent keeps it, null clears it, else whole MB.
function floorOf(value: unknown): number | null | undefined | false {
  if (value === undefined || value === null) return value;
  return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : false;
}

// `PATCH /api/v1/host`: this host's label, lanes, floors and boundary,
// checked against its `rev` and refused as the daemon refuses them.
function editHost(body: Record<string, unknown>, scenario: Scenario): MutationResult {
  const fields = hostFieldsFor(scenario);
  if (!fields) return refusal(400, "usage", NOT_INITIALIZED);
  if (body.rev !== fields.rev) {
    return {
      reply: refused(
        409,
        "stale",
        `host ${fields.label} is at rev ${fields.rev}, you changed rev ${String(body.rev)}`,
        { rev: fields.rev },
      ),
      followUps: [],
    };
  }
  const label = typeof body.label === "string" ? body.label : undefined;
  if (label !== undefined && label !== fields.label) {
    if (!LABEL.test(label)) {
      return refusal(
        400,
        "usage",
        `a peer label is a letter or digit, then letters, digits, '_' or '-' (at most 64): "${label}"`,
      );
    }
    const { machines } = fixtureResponses(scenario)[machinesPath] as { machines: Machine[] };
    const paired = machines
      .map(withoutUnpaired)
      .filter((machine) => machine.peer_scope)
      .map((machine) => machine.label);
    paired.sort();
    if (paired.length > 0) {
      return refusal(
        422,
        "refused",
        `unpair first: ${paired.join(", ")} ${paired.length === 1 ? "knows" : "know"} this host by its label; remove each with tiller peer remove, rename, then pair again`,
      );
    }
  }
  const lanes = body.lane_capacity;
  if (
    lanes !== undefined &&
    !(typeof lanes === "number" && Number.isInteger(lanes) && lanes >= 1)
  ) {
    return refusal(400, "usage", "lane capacity must be at least 1");
  }
  const ram = floorOf(body.ram_floor_mb);
  const disk = floorOf(body.disk_floor_mb);
  if (ram === false || disk === false) {
    return refusal(400, "usage", "a floor is a whole number of MB");
  }
  const boundary =
    body.boundary === "personal" || body.boundary === "work" ? body.boundary : undefined;
  if (body.boundary !== undefined && boundary === undefined) {
    return refusal(400, "usage", `unknown boundary ${String(body.boundary)}`);
  }
  if (boundary !== undefined && boundary !== fields.boundary && fields.boundary_recorded_by) {
    return refusal(
      422,
      "refused",
      `${fields.boundary_recorded_by} already records the ${fields.boundary} boundary; it changes only before any memory, backup or export`,
    );
  }
  hostOverlay = {
    ...hostOverlay,
    rev: fields.rev + 1,
    ...(label === undefined ? {} : { label }),
    ...(typeof lanes === "number" ? { lane_capacity: lanes } : {}),
    ...(ram === undefined ? {} : { ram_floor_mb: ram }),
    ...(disk === undefined ? {} : { disk_floor_mb: disk }),
    ...(boundary === undefined ? {} : { boundary }),
  };
  const host = {
    id: fields.id,
    label: label ?? fields.label,
    kind: "local",
    lane_capacity: typeof lanes === "number" ? lanes : fields.lane_capacity,
    rev: fields.rev + 1,
  };
  return { reply: recorded(host, null, null), followUps: [hostEdited(scenario, fields.id)] };
}

// The machines read after an Unpair: that host is no longer a peer.
function withUnpaired(pathname: string, body: unknown): unknown {
  if (pathname !== machinesPath || !body || !anyUnpaired()) return body;
  const { machines } = body as { machines: Machine[] };
  return { machines: machines.map(withoutUnpaired) };
}

// Unpair and Revoke key, with the event each commits: `host.edited` on the
// unpaired host's row, `peer.revoked` on the key.
function linkMutation(method: string, path: string, scenario: Scenario): MutationResult | null {
  const reply = peerLinkMutation(method, path, scenario);
  if (!reply) return null;
  if (!reply.ok) return refusal(reply.status, reply.code, reply.message);
  if (reply.kind === "revoke") {
    const key = reply.data;
    const event = {
      kind: "peer.revoked",
      txn: "peer.revoke",
      entity: "peer_key",
      entity_id: key.id,
      rev_after: key.rev,
      card_id: null,
      entity_state: key,
    };
    return { reply: recorded(key, null, null), followUps: [{ delayMs: 100, event }] };
  }
  const { machines } = fixtureResponses(scenario)[machinesPath] as { machines: Machine[] };
  const row = machines.find((machine) => machine.label === reply.label);
  if (!row) return refusal(404, "not_found", `no host ${reply.label}`);
  const host = withoutUnpaired(row);
  // A host not paired is a no-op: its row as it is, and no event.
  const followUps: FollowUp[] = reply.changed
    ? [
        {
          delayMs: 100,
          event: {
            kind: "host.edited",
            txn: "peer.remove",
            entity: "host",
            entity_id: row.id,
            rev_after: row.rev + 1,
            card_id: null,
            entity_state: host,
          },
        },
      ]
    : [];
  const done = recorded(host, null, null);
  // A key file the unpair left: still applied, with why on `warning`.
  if (reply.warning) done.body = { ...(done.body as object), warning: reply.warning };
  return { reply: done, followUps };
}

function cardEvent(kind: string, txn: string, row: BoardRow): FollowUp {
  return {
    delayMs: 100,
    event: {
      kind,
      txn,
      entity: "card",
      entity_id: row.id,
      rev_after: row.rev,
      card_id: row.id,
      entity_state: null,
      card: row,
    },
  };
}

// The reads a triage changed: the board gains the moved cards and shows the
// closed ones in Done, a card read follows its row, and the local Machines
// row carries the bug project the owner picked.
function withTriage(pathname: string, body: unknown, scenario: Scenario): unknown {
  if (pathname === boardPath().split("?")[0] && body && madeCards.size + closedCards.size > 0) {
    const board = body as BoardResponse;
    const rows = board.rows.map((row) => closedCards.get(row.id) ?? row);
    const fresh = [...madeCards.values()].filter((row) => !rows.some((r) => r.id === row.id));
    return { ...board, rows: [...rows, ...fresh.map((row) => closedCards.get(row.id) ?? row)] };
  }
  if (pathname === machinesPath && body && hostOverlay) {
    const { machines } = body as { machines: Machine[] };
    return {
      machines: machines.map((machine) =>
        machine.kind === "local" ? (localMachine(scenario) ?? machine) : machine,
      ),
    };
  }
  const timeline = /^\/api\/v1\/cards\/([^/]+)\/timeline$/.exec(pathname);
  const moved = timeline?.[1] ? madeCards.get(timeline[1]) : undefined;
  if (body === undefined && moved) {
    return {
      entries: [
        {
          seq: 4200,
          at: new Date().toISOString(),
          actor: "you",
          text: `filed as a Tiller bug into ${moved.project_id}`,
        },
      ],
      next_before: null,
    };
  }
  const detail = /^\/api\/v1\/cards\/([^/]+)$/.exec(pathname);
  const id = detail?.[1];
  if (!id) return body;
  const made = madeCards.get(id);
  if (body === undefined && made) {
    const template = fixtureResource(`/api/v1/cards/${knownTemplate()}`, "federated") as {
      card: Record<string, unknown>;
    };
    return {
      ...template,
      card: {
        ...template.card,
        ...made,
        body: madeBodies.get(id) ?? null,
        state: "open",
        outcome: null,
      },
      column: closedCards.get(id) ?? made,
      runs: [],
      prs: [],
      approvals: [],
      artifacts: [],
      attention: [],
    };
  }
  const closed = closedCards.get(id);
  if (!closed || !body) return body;
  const read = body as { card: Record<string, unknown> };
  return {
    ...read,
    card: { ...read.card, state: "closed", outcome: closed.badge, rev: closed.rev },
    column: closed,
  };
}

// The dependencies the owner added or removed on the stand-in server: by card
// id, the display id of each card it now waits on (true) or no longer waits on
// (false).
const depChanges = new Map<string, Map<string, boolean>>();

// The rows of one host: this host's own, or a peer's by its label. A
// dependency joins two cards of the same host.
function hostRows(scenario: Scenario, host: string | undefined): BoardRow[] {
  const board = fixtureResponses(scenario)[boardPath()] as BoardResponse;
  return board.rows.filter((row) => (row.host ?? undefined) === host);
}

function openDeps(detail: CardDetail): string[] {
  return detail.deps.filter((dep) => dep.released_at === null).map((dep) => dep.display_id);
}

// A card read with the owner's dependency changes. A queued card with an open
// dependency is blocked, so the change also re-badges its row.
function withDepsOn(detail: CardDetail, rows: BoardRow[]): CardDetail {
  const changes = depChanges.get(detail.card.id);
  if (!changes) return detail;
  const now = new Date().toISOString();
  // A removed dependency keeps the time it was released, if it was.
  const deps: CardDep[] = detail.deps.map((dep) =>
    changes.get(dep.display_id) === false && dep.release_reason !== "removed"
      ? { ...dep, released_at: dep.released_at ?? now, release_reason: "removed" }
      : dep,
  );
  for (const [on, adds] of changes) {
    if (!adds || deps.some((dep) => dep.display_id === on && dep.released_at === null)) continue;
    const other = rows.find((row) => row.display_id === on);
    if (!other) continue;
    deps.push({
      display_id: on,
      released_at: null,
      title: other.title,
      column: other.column,
      badge: other.badge,
    });
  }
  const read = { ...detail, deps };
  return { ...read, column: rebadged(read) };
}

function rebadged(detail: CardDetail): BoardRow {
  const { column } = detail;
  const open = openDeps(detail);
  if (column.column !== "queued" || column.badge === "parked") return column;
  if (open.length > 0) return { ...column, badge: "blocked", blocked_by: open[0] ?? null };
  if (column.badge === "blocked") return { ...column, badge: "filed", blocked_by: null };
  return column;
}

function cardRead(id: string, scenario: Scenario): CardDetail | undefined {
  const read = fixtureResource(`/api/v1/cards/${id}`, scenario) as CardDetail | undefined;
  return read ? withDepsOn(read, hostRows(scenario, read.column.host)) : undefined;
}

// The reads a dependency change touched: the card's own read and its board row.
function withDeps(pathname: string, body: unknown, scenario: Scenario): unknown {
  if (depChanges.size === 0 || body === undefined) return body;
  if (pathname === boardPath().split("?")[0]) {
    const board = body as BoardResponse;
    return {
      ...board,
      rows: board.rows.map((row) =>
        depChanges.has(row.id) ? (cardRead(row.id, scenario)?.column ?? row) : row,
      ),
    };
  }
  const detail = /^\/api\/v1\/cards\/([^/]+)$/.exec(pathname);
  if (!detail?.[1]) return body;
  const read = body as CardDetail;
  return withDepsOn(read, hostRows(scenario, read.column.host));
}

// `POST` and `DELETE /api/v1/cards/{id}/deps`, with the checks the core makes:
// an unknown card, a card that waits on itself, a closed dependency and a loop.
function depMutation(
  method: string,
  id: string,
  body: Record<string, unknown>,
  scenario: Scenario,
): MutationResult {
  const rows = hostRows(scenario, typeof body.host === "string" ? body.host : undefined);
  const card = rows.find((row) => row.id === id || row.display_id === id);
  if (!card) return refusal(404, "not_found", `no card ${id}`);
  const on = typeof body.depends_on === "string" ? body.depends_on : "";
  const target = rows.find((row) => row.id === on || row.display_id === on);
  if (!target) return refusal(404, "not_found", `no card ${on}`);
  const before = cardRead(card.id, scenario);
  if (!before) return refusal(404, "not_found", `no card ${id}`);
  const open = openDeps(before);
  const changes = depChanges.get(card.id) ?? new Map<string, boolean>();
  if (method === "DELETE") {
    // Natural: with none left to remove, nothing changes. A released
    // dependency is still on the card, so it can be removed.
    const kept = before.deps.some(
      (dep) => dep.display_id === target.display_id && dep.release_reason !== "removed",
    );
    if (!kept) return { reply: recorded(null, null, null), followUps: [] };
    changes.set(target.display_id, false);
  } else if (!open.includes(target.display_id)) {
    const refuse = (fact: string) => refusal(422, "refused", `card ${card.display_id}: ${fact}`);
    if (target.id === card.id) return refuse("a card cannot wait on itself");
    if (target.column === "done") return refuse("the dependency is already closed");
    if ((before.waiting_on_this ?? []).includes(target.display_id)) {
      return refuse(`${target.display_id} already waits on ${card.display_id}`);
    }
    changes.set(target.display_id, true);
  }
  depChanges.set(card.id, changes);
  const after = cardRead(card.id, scenario);
  // The last row for the card is the newest one.
  let dep: CardDep | null = null;
  for (const row of after?.deps ?? []) if (row.display_id === target.display_id) dep = row;
  const removing = method === "DELETE";
  return {
    reply: recorded(dep, null, null),
    followUps: [
      {
        delayMs: 100,
        event: {
          kind: removing ? "dep.released" : "dep.added",
          txn: removing ? "dep.release" : "dep.add",
          entity: "dep",
          entity_id: `${card.id}:${target.id}`,
          rev_after: null,
          card_id: card.id,
          entity_state: null,
          card: after?.column ?? card,
        },
      },
    ],
  };
}

// A queued card whose read the fixtures answer, to give a moved card the
// fields no Move sets.
function knownTemplate(): string {
  const board = fixtureResponses("federated")[boardPath()] as BoardResponse;
  return board.rows.find((row) => row.category === "tiller-bug")?.id ?? "";
}

// `POST /api/v1/cards` for a Move: the card lands on the board as a Tiller bug.
function createBug(body: Record<string, unknown>): MutationResult {
  cardNumber += 1;
  const project = String(body.project);
  const host = typeof body.host === "string" ? body.host : undefined;
  const id = `01JCARDMOVE${String(cardNumber).padStart(15, "0")}`;
  const row: BoardRow = {
    id,
    display_id: `${project}-${cardNumber}`,
    project_id: project,
    host_id: null,
    kind: String(body.kind ?? "ship"),
    title: String(body.title ?? ""),
    priority: typeof body.priority === "number" ? body.priority : 2,
    column: "queued",
    badge: "filed",
    needs_you: false,
    parked_until: null,
    parked_reason: null,
    blocked_by: null,
    closed_at: null,
    rev: 1,
    category: "tiller-bug",
    ...(host ? { host, as_of: new Date().toISOString(), link: "live" as const } : {}),
  };
  madeCards.set(id, row);
  if (typeof body.body === "string") madeBodies.set(id, body.body);
  const outbox = fakeId("01JOUTB");
  return {
    reply: recorded({ card: row, message: { id: outbox } }, outbox, null),
    followUps: [cardEvent("card.created", "card.create", row), ...outboxStages(outbox)],
  };
}

// `POST /api/v1/cards/{id}/close`: the card moves to Done with its outcome.
function closeCard(id: string, body: Record<string, unknown>): MutationResult {
  const row = closedCards.get(id) ?? knownCard(id);
  if (!row) return refusal(404, "not_found", `no card ${id}`);
  if (row.host === "host-d" && body.outcome === "superseded" && !unsureCloses.has(row.id)) {
    unsureCloses.add(row.id);
    return refusal(504, "forward_uncertain", "host-d did not confirm in time");
  }
  const outcome = typeof body.outcome === "string" ? body.outcome : "done";
  const closed: BoardRow = {
    ...row,
    column: "done",
    badge: outcome,
    needs_you: false,
    closed_at: new Date().toISOString(),
    rev: row.rev + 1,
  };
  closedCards.set(id, closed);
  return {
    reply: recorded({ card: closed }, null, null),
    followUps: [cardEvent("card.closed", "card.close", closed)],
  };
}

// `POST /api/v1/cards/{id}/cancel`, by id or display id as the daemon takes
// it: the card moves to Cancelled and its runs end.
function cancelFixtureCard(card: string, body: Record<string, unknown>): MutationResult {
  const board = fixtureResponses("federated")[boardPath()] as BoardResponse;
  const known = knownCard(card) ?? board.rows.find((row) => row.display_id === card);
  const row = known ? (closedCards.get(known.id) ?? known) : undefined;
  if (!row) return refusal(404, "not_found", `no card ${card}`);
  if (typeof body.reason !== "string" || body.reason.trim() === "") {
    return refusal(400, "usage", "the reason is empty");
  }
  const closed: BoardRow = {
    ...row,
    column: "done",
    badge: "cancelled",
    needs_you: false,
    closed_at: new Date().toISOString(),
    rev: row.rev + 1,
  };
  closedCards.set(row.id, closed);
  cancelCard(row.id);
  return {
    reply: recorded({ card: closed }, null, null),
    followUps: [cardEvent("card.closed", "card.cancel", closed)],
  };
}

// `POST /api/v1/bug-project`: this host's pick, checked against its `rev`.
function pickBugProject(body: Record<string, unknown>, scenario: Scenario): MutationResult {
  const machine = localMachine(scenario);
  if (!machine) return refusal(400, "usage", NOT_INITIALIZED);
  if (body.rev !== machine.rev) {
    return {
      reply: refused(
        409,
        "stale",
        `host ${machine.label} is at rev ${machine.rev}, you changed rev ${String(body.rev)}`,
        { rev: machine.rev },
      ),
      followUps: [],
    };
  }
  const project = typeof body.project === "string" ? body.project : null;
  if (project === "fleet") {
    return refusal(422, "refused", "fleet cannot take Tiller bugs: pick another project");
  }
  const projects = (fixtureResponses(scenario)[projectsPath] as ProjectsResponse).projects;
  if (project && !projects.some((p) => p.id === project && !p.host && !gone(p.id))) {
    return refusal(404, "not_found", `no project ${project}`);
  }
  hostOverlay = { ...hostOverlay, rev: machine.rev + 1, bug_project: project };
  // Like the daemon, the reply is the host record, not its Machines row.
  const host = {
    id: machine.id,
    label: machine.label,
    bug_project_id: project,
    rev: machine.rev + 1,
  };
  return { reply: recorded(host, null, null), followUps: [hostEdited(scenario, machine.id)] };
}

// Starting, editing or ending away mode in the fixtures only records: the
// stand-in server does not keep a contract.
function awayMutation(method: string): MutationResult {
  const brief = awayBrief(false);
  const data = method === "DELETE" ? { away: null, brief } : { id: brief.approval_id };
  return { reply: recorded(data, null, brief.approval_id), followUps: [] };
}

function handle(
  method: string,
  path: string,
  body: Record<string, unknown>,
  scenario: Scenario,
): MutationResult {
  const peer = forwarded(method, path, body);
  if (peer) return peer;
  if (method === "POST" && path === "/api/v1/bug-project") return pickBugProject(body, scenario);
  if (method === "PATCH" && path === "/api/v1/host") return editHost(body, scenario);
  if (method === "POST" && path === "/api/v1/backups") {
    return { reply: recorded(takeBackup(), null, null), followUps: [] };
  }
  const link = linkMutation(method, path, scenario);
  if (link) return link;
  if (method === "POST" && path === "/api/v1/cards" && body.category === "tiller-bug") {
    return createBug(body);
  }
  if (path === "/api/v1/away") return awayMutation(method);
  const close = /^\/api\/v1\/cards\/([^/]+)\/close$/.exec(path);
  if (method === "POST" && close?.[1]) return closeCard(close[1], body);
  const cancel = /^\/api\/v1\/cards\/([^/]+)\/cancel$/.exec(path);
  if (method === "POST" && cancel?.[1]) return cancelFixtureCard(cancel[1], body);
  const deps = /^\/api\/v1\/cards\/([^/]+)\/deps$/.exec(path);
  if ((method === "POST" || method === "DELETE") && deps?.[1]) {
    return depMutation(method, deps[1], body, scenario);
  }
  const answer = /^\/api\/v1\/attention\/([^/]+)\/answer$/.exec(path);
  if (method === "POST" && answer) {
    const outbox = fakeId("01JOUTB");
    return {
      reply: recorded({ id: answer[1], answer: body.answer }, outbox, null),
      followUps: [answered(answer[1] ?? ""), ...outboxStages(outbox)],
    };
  }
  const decide = /^\/api\/v1\/approvals\/([^/]+)\/decide$/.exec(path);
  if (method === "POST" && decide?.[1]) {
    if (body.head_sha !== FIXTURE_HEAD_SHA) {
      return {
        reply: refused(
          422,
          "refused",
          `approval ${decide[1]}: the PR head is ${FIXTURE_HEAD_SHA}, you approved ${String(body.head_sha)}`,
          { head_sha: FIXTURE_HEAD_SHA },
        ),
        followUps: [],
      };
    }
    const item = attentionFor(decide[1]);
    return {
      reply: recorded({ id: decide[1], state: "accepted" }, null, decide[1]),
      followUps: [
        ...(item ? [answered(item)] : []),
        ...approvalStages(decide[1], String(body.decision)),
      ],
    };
  }
  if (method === "PATCH" && /^\/api\/v1\/cards\/[^/]+$/.test(path)) {
    return { reply: recorded({ priority: body.priority }, null, null), followUps: [] };
  }
  if (method === "POST" && /^\/api\/v1\/(cards\/[^/]+\/comment|cards)$/.test(path)) {
    const outbox = fakeId("01JOUTB");
    return { reply: recorded({}, outbox, null), followUps: outboxStages(outbox) };
  }
  const asks = /^\/api\/v1\/projects\/([^/]+)\/asks$/.exec(path);
  if (method === "POST" && asks?.[1]) {
    const project = asks[1];
    const live = (fixtureResponses()[agentsPath()] as AgentsResponse).orchestrators.some(
      (row) => row.project_id === project,
    );
    const id = fakeId("01JASK");
    const outbox = live ? fakeId("01JOUTB") : null;
    const at = new Date().toISOString();
    return {
      reply: recorded(
        {
          ask: {
            id,
            project_id: project,
            session_id: null,
            source: "board",
            text: body.text,
            received_at: at,
          },
          message: outbox ? { id: outbox, target_session_id: "01JSESSION" } : null,
          warning: live ? null : "no_orchestrator",
        },
        outbox,
        null,
      ),
      followUps: outbox ? outboxStages(outbox) : [],
    };
  }
  const stage = /^\/api\/v1\/projects\/([^/]+)\/stage$/.exec(path);
  if (method === "POST" && stage?.[1]) {
    const project = currentProject(stage[1]);
    if (!project) {
      return { reply: refused(404, "not_found", `no project ${stage[1]}`), followUps: [] };
    }
    if (body.rev !== project.rev) {
      return {
        reply: refused(
          409,
          "stale",
          `project ${project.id} is at rev ${project.rev}, you changed rev ${String(body.rev)}`,
          { rev: project.rev },
        ),
        followUps: [],
      };
    }
    if (body.stage !== "pre-launch" && body.stage !== "launched") {
      return {
        reply: refused(422, "refused", "stage must be pre-launch or launched"),
        followUps: [],
      };
    }
    const next = restaged(project, body.stage);
    switched.set(next.id, next);
    return {
      reply: recorded(next, null, null),
      followUps: [
        {
          delayMs: 300,
          event: {
            kind: "project.staged",
            txn: "project.stage",
            entity: "project",
            entity_id: next.id,
            rev_after: next.rev,
            card_id: null,
            entity_state: next,
          },
        },
      ],
    };
  }
  const deletion = /^\/api\/v1\/projects\/([^/]+)\/delete$/.exec(path);
  if (method === "POST" && deletion?.[1]) {
    const id = deletion[1];
    const preview = deleted.has(id)
      ? undefined
      : (fixtureResource(path, scenario) as ProjectDeletePreview | undefined);
    if (!preview) return refusal(404, "not_found", `no project ${id}`);
    if (body.confirm !== id) return refusal(400, "usage", `type ${id} to delete the project`);
    if (preview.refused) return refusal(422, "refused", `project ${id}: ${preview.refused}`);
    deleted.add(id);
    if (preview.agents.length > 0) stopped.add(id);
    return {
      reply: recorded(
        {
          project: id,
          removed: preview.removed,
          unlinked: preview.unlinked,
          stopped: preview.agents.map((agent) => ({ ...agent, stop_proof: "exited" })),
          cancelled_cards: preview.agents.flatMap((agent) => (agent.card ? [agent.card] : [])),
          warnings: [],
        },
        null,
        null,
      ),
      followUps: [projectGone("project.deleted", "project.delete", id)],
    };
  }
  if (method === "POST" && path === projectsPath) return addProject(body);
  const edit = /^\/api\/v1\/projects\/([^/]+)$/.exec(path);
  if (method === "PATCH" && edit?.[1]) return editProject(edit[1], body);
  if (method === "POST" && path === "/api/v1/agents/restart-stale") {
    const ids = Array.isArray(body.sessions) ? body.sessions.map(String) : [];
    return {
      reply: recorded(restartStaleFixture(ids), null, null),
      followUps: [
        {
          delayMs: 100,
          event: {
            kind: "session.restart",
            txn: "session.restart",
            entity: "session",
            entity_id: ids[0] ?? "",
            rev_after: null,
            card_id: null,
            entity_state: null,
          },
        },
      ],
    };
  }
  const retire = /^\/api\/v1\/projects\/([^/]+)\/retire$/.exec(path);
  if (method === "POST" && retire?.[1]) {
    const id = retire[1];
    const preview = gone(id)
      ? undefined
      : (fixtureResource(path, scenario) as ProjectRetirePreview | undefined);
    if (!preview) return refusal(404, "not_found", `no project ${id}`);
    if (preview.refused) return refusal(422, "refused", `project ${id}: ${preview.refused}`);
    retired.set(id, new Date().toISOString());
    const stop = body.stop_agents === true;
    if (stop && preview.agents.length > 0) stopped.add(id);
    return {
      reply: recorded(
        {
          project: currentProject(id) ?? { id },
          stopped: stop ? preview.agents.map((agent) => ({ ...agent, stop_proof: "exited" })) : [],
          cancelled_cards: [],
          warnings: [],
        },
        null,
        null,
      ),
      followUps: [projectGone("project.retired", "project.retire", id)],
    };
  }
  const relabel = /^\/api\/v1\/interventions\/([^/]+)\/label$/.exec(path);
  if (method === "POST" && relabel?.[1]) {
    const label = body.label ?? null;
    const kinds = ["correction", "unplanned_escalation", "rework", "planned"];
    if (label !== null && !kinds.includes(String(label))) {
      return {
        reply: refused(422, "invalid", `label must be one of ${kinds.join(", ")}`),
        followUps: [],
      };
    }
    const done = relabelFixture(relabel[1], label as InterventionKind | null);
    if (!done) {
      return { reply: refused(404, "not_found", `no intervention ${relabel[1]}`), followUps: [] };
    }
    return { reply: recorded(done, null, null), followUps: [] };
  }
  const grant = grantMutation(method, path, body, localProject);
  if (grant) {
    if (!grant.ok) return refusal(grant.status, grant.code, grant.message);
    return {
      reply: recorded(grant.data, null, null),
      followUps: [
        {
          delayMs: 100,
          event: {
            kind: grant.data.revoked_at ? "grant.revoked" : "grant.created",
            txn: grant.data.revoked_at ? "grant.revoke" : "grant.create",
            entity: "grant",
            entity_id: grant.data.id,
            rev_after: null,
            card_id: null,
            entity_state: null,
          },
        },
      ],
    };
  }
  const retro = retroMutation(method, path);
  if (retro) {
    if (!retro.ok) return refusal(retro.status, retro.code, retro.message);
    const changed = (kind: string, delayMs: number) => ({
      delayMs,
      event: {
        kind,
        txn: kind === "retro.started" ? "retro.start" : "retro.finish",
        entity: "retro",
        entity_id: retro.data.id,
        rev_after: null,
        card_id: null,
        entity_state: null,
      },
    });
    return {
      reply: recorded(retro.data, null, null),
      followUps: [changed("retro.started", 100), changed("retro.finished", RUN_MS + 200)],
    };
  }
  // A holdout eval plans against the scenario's quota, as the daemon's does.
  const quota = path.startsWith("/api/v1/memory/proposals/")
    ? ((fixtureResource("/api/v1/quota", scenario) as QuotaResponse | undefined)?.windows ?? [])
    : [];
  const memory = memoryMutation(method, path, body, quota, scenario);
  if (memory) {
    if (!memory.ok) {
      return {
        reply: refused(memory.status, memory.code, memory.message, memory.current),
        followUps: [],
      };
    }
    // The commit records a revision, which every open memory view refetches.
    return {
      reply: recorded(memory.data, null, memory.approvalId),
      followUps: [
        {
          delayMs: 100,
          event: {
            kind: "memory_revision.recorded",
            txn: "mem.apply",
            entity: "memory_revision",
            entity_id: path,
            rev_after: null,
            card_id: null,
            entity_state: null,
          },
        },
      ],
    };
  }
  const replyRead = /^\/api\/v1\/replies\/([^/]+)\/read$/.exec(path);
  if (method === "POST" && replyRead?.[1]) {
    const read = markReplyRead(replyRead[1], Date.now());
    if (!read) {
      return { reply: refused(404, "not_found", `no reply ${replyRead[1]}`), followUps: [] };
    }
    return {
      reply: recorded(read, null, null),
      followUps: [
        {
          delayMs: 200,
          event: {
            kind: "reply.read",
            txn: "reply.read",
            entity: "reply",
            entity_id: read.id,
            rev_after: read.rev,
            card_id: null,
            entity_state: null,
          },
        },
      ],
    };
  }
  if (method === "POST" && path === "/api/v1/recap/seen") {
    recapMarkedAt = new Date().toISOString();
    return {
      reply: recorded(recapFor(scenario, Date.now(), recapMarkedAt), null, null),
      followUps: [
        {
          delayMs: 200,
          event: {
            kind: "recap.seen",
            txn: "recap.mark_seen",
            entity: "recap",
            entity_id: "owner",
            rev_after: null,
            card_id: null,
            entity_state: null,
          },
        },
      ],
    };
  }
  if (method === "POST" && path === "/api/v1/config") {
    const saved = setConfigFixture(body, scenario);
    if (!saved.ok) {
      return {
        reply: refused(saved.status, saved.code, saved.message, saved.current),
        followUps: [],
      };
    }
    return { reply: recorded(saved.data, null, null), followUps: [] };
  }
  const orchestrator = /^\/api\/v1\/projects\/([^/]+)\/orchestrator$/.exec(path);
  if (method === "POST" && orchestrator?.[1]) {
    const project = orchestrator[1];
    const problems = typeof body.host === "string" ? [] : startProblems(project, scenario);
    const [first] = problems;
    if (first && !orchestratorLive(project)) {
      return {
        reply: refused(400, "usage", `config.toml: ${first.problem}`, { settings: problems }),
        followUps: [],
      };
    }
    const host = typeof body.host === "string" ? body.host : undefined;
    const restart = body.restart === true;
    if (restart && host) {
      const reply = refused(422, "refused", "a restart runs on the orchestrator's own host");
      return { reply, followUps: [] };
    }
    // A stopped or restarted orchestrator starts fresh: its pane is back.
    const own = orchestratorSession(project);
    const wasStopped = !host && sessionStopped(own);
    if (!host && (restart || wasStopped)) changeSession(own, { fresh: true, stopped: false });
    const { live: wasLive } = startOrchestratorFixture(project, host);
    const live = wasLive && !restart && !wasStopped;
    const session = fakeId("01JSESS");
    const data = {
      session: { id: session, project_id: project, role: "orchestrator" },
      label: project,
      warning: null,
    };
    const reply = recorded(data, null, null);
    if (live) {
      return {
        reply: { ...reply, body: { ...(reply.body as object), replayed: true } },
        followUps: [],
      };
    }
    return {
      reply,
      followUps: [
        {
          delayMs: 600,
          event: {
            kind: "session.started",
            txn: "session.start",
            entity: "session",
            entity_id: session,
            rev_after: null,
            card_id: null,
            entity_state: { id: session, role: "orchestrator", project_id: project },
          },
        },
      ],
    };
  }
  const control = /^\/api\/v1\/agents\/([^/]+)\/(steer|interrupt)$/.exec(path);
  if (method === "POST" && control?.[1]) {
    return agentControl(decodeURIComponent(control[1]), control[2] ?? "", body);
  }
  const lifecycle = /^\/api\/v1\/agents\/([^/]+)\/(stop|pause|unpause|resume|relaunch)$/.exec(path);
  if (method === "POST" && lifecycle?.[1]) {
    return agentLifecycle(decodeURIComponent(lifecycle[1]), lifecycle[2] ?? "", body, scenario);
  }
  const poller = /^\/api\/v1\/pollers\/([^/]+)\/run$/.exec(path);
  if (method === "POST" && poller?.[1]) {
    const source = decodeURIComponent(poller[1]);
    const health = runPollerFixture(source, Date.now());
    if (!health)
      return { reply: refused(404, "not_found", `no poller reads ${source}`), followUps: [] };
    return { reply: recorded(health, null, null), followUps: [] };
  }
  if (method === "POST" && /^\/api\/v1\/agents\/[^/]+\/focus$/.test(path)) {
    return { reply: recorded({}, null, null), followUps: [] };
  }
  // The permission dialog a waiting agent has up. The first tap finds it
  // changed, to see the board refuse a stale answer; a tap on the new one
  // is sent.
  const prompt = /^\/api\/v1\/agents\/([^/]+)\/prompt(\/answer)?$/.exec(path);
  if (method === "POST" && prompt?.[1]) {
    const session = prompt[1];
    if (session === DIALOG_SESSIONS.trust || session === DIALOG_SESSIONS.unknown) {
      return dialogMutation(session, !prompt[2], body);
    }
    if (!prompt[2]) {
      const read = { session_id: session, read_at: new Date().toISOString() };
      const dialog = fixturePrompt(changed.has(session));
      return {
        reply: { status: 200, body: { ...read, prompt: dialog, reason: null } },
        followUps: [],
      };
    }
    if (!changed.has(session) || body.screen !== fixturePrompt(true).screen) {
      changed.add(session);
      const reply = refused(
        409,
        "screen_changed",
        "the prompt changed after the buttons were drawn",
      );
      return { reply, followUps: [] };
    }
    const reply = recorded({ session_id: session, sent: { option: body.option } }, null, null);
    return { reply, followUps: [] };
  }
  return { reply: refused(404, "not_found", `no mutation at ${method} ${path}`), followUps: [] };
}

const changed = new Set<string>();

// A session's lifecycle change as a change event, so the board refetches the
// agents list.
function sessionEvent(session: string, kind: string): FollowUp {
  return {
    delayMs: 400,
    event: {
      kind,
      txn: kind,
      entity: "session",
      entity_id: session,
      rev_after: null,
      card_id: null,
      entity_state: null,
    },
  };
}

// Stop, pause, unpause, resume and relaunch (protocol.md 7.4), with the refusals core
// gives: an orchestrator has no run, and a worker whose run is still open is
// not stopped.
function agentLifecycle(
  session: string,
  verb: string,
  body: Record<string, unknown>,
  scenario: Scenario,
): MutationResult {
  const agent = (fixtureResponses(scenario)[agentsPath()] as AgentsResponse).orchestrators
    .flatMap((orchestrator) => [orchestrator, ...orchestrator.workers])
    .find((row) => row.session_id === session);
  if (!agent) return { reply: refused(404, "not_found", `no session ${session}`), followUps: [] };
  const name = agent.herdr_name ?? session;
  const orchestrator = agent.role === "orchestrator";
  const at = new Date().toISOString();
  const refuse = (fact: string) => ({
    reply: refused(422, "refused", `session ${session}: ${fact}`),
    followUps: [],
  });
  if (orchestrator && verb !== "stop") {
    return refuse(`it is an orchestrator, which has no run to ${verb}`);
  }
  const dead = agent.activity === "ended" || agent.activity === "errored";
  if (verb === "stop") {
    if (!orchestrator && !dead) {
      const run = agent.run_id ?? "";
      return refuse(
        `run ${run} is active; end it first with tiller done ${run} --outcome <outcome>, then stop its agent; nothing was sent`,
      );
    }
    // An orchestrator's first stop finds a draft in its composer, so the dev
    // server shows that refusal before the stop goes through.
    if (orchestrator && !sessionDrafted(session)) {
      changeSession(session, { drafted: true });
      return refuse(
        `the composer of pane ${agent.pane_id ?? name} holds a draft ("check the queue"); send or clear it first`,
      );
    }
    changeSession(session, { stopped: true });
    const data = { session_id: session, pane_id: agent.pane_id ?? null };
    return {
      reply: recorded(data, null, null),
      followUps: [sessionEvent(session, "session.ended")],
    };
  }
  if (verb === "pause") {
    const until = typeof body.until === "string" ? body.until : "";
    const reason = typeof body.reason === "string" ? body.reason.trim() : "";
    if (Number.isNaN(Date.parse(until)) || Date.parse(until) <= Date.now()) {
      return { reply: refused(400, "usage", "--until must be a time ahead"), followUps: [] };
    }
    if (reason === "")
      return { reply: refused(400, "usage", "the reason is empty"), followUps: [] };
    const pause = { until, at, by: "board", reason };
    changeSession(session, { pause });
    return {
      reply: recorded({ run_id: agent.run_id, pause }, null, null),
      followUps: [sessionEvent(session, "run.paused")],
    };
  }
  if (verb === "unpause") {
    const paused = agent.pause != null;
    changeSession(session, { pause: null });
    return {
      reply: recorded({ run_id: agent.run_id, pause_until: null }, null, null),
      followUps: paused ? [sessionEvent(session, "run.unpaused")] : [],
    };
  }
  if (verb === "resume") {
    // A manual resume clears the pause, a live agent left alone too.
    if (!dead) {
      changeSession(session, { pause: null });
      return {
        reply: recorded({ left_alone: true, pane_id: agent.pane_id }, null, null),
        followUps: [],
      };
    }
    changeSession(session, { fresh: true, activity: "working", at, pause: null });
    return {
      reply: recorded({ left_alone: false, pane_id: agent.pane_id }, null, null),
      followUps: [sessionEvent(session, "session.resumed")],
    };
  }
  const note = typeof body.note === "string" ? body.note.trim() : "";
  if (note === "") return { reply: refused(400, "usage", "the note is empty"), followUps: [] };
  changeSession(session, { fresh: true, activity: "working", at, pause: null });
  return {
    reply: recorded({ session_id: session, pane_id: agent.pane_id }, null, null),
    followUps: [sessionEvent(session, "session.relaunched")],
  };
}

// How many interrupts each fixture agent was sent: the first is confirmed, the
// next is not, so the dev server shows both results.
const interrupts = new Map<string, number>();

// Steer and interrupt (protocol.md 7.4). A steer line is typed a few seconds
// later, as when the pane goes idle. An interrupt reaches a working agent on
// a run only, as the daemon checks.
function agentControl(
  session: string,
  verb: string,
  body: Record<string, unknown>,
): MutationResult {
  const agent = (fixtureResponses()[agentsPath()] as AgentsResponse).orchestrators
    .flatMap((orchestrator) => [orchestrator, ...orchestrator.workers])
    .find((row) => row.session_id === session);
  if (!agent) return { reply: refused(404, "not_found", `no session ${session}`), followUps: [] };
  if (verb === "steer") {
    const text = typeof body.text === "string" ? body.text.trim() : "";
    if (text === "") return { reply: refused(400, "usage", "the message is empty"), followUps: [] };
    const outbox = fakeId("01JOUTB");
    const data = {
      id: outbox,
      target_session_id: session,
      kind: "steer",
      state: "pending",
      body: text,
    };
    return {
      reply: recorded(data, outbox, null),
      followUps: outboxStages(outbox).map((stage) => ({ ...stage, delayMs: stage.delayMs + 6000 })),
    };
  }
  if (!agent.run_id || agent.activity !== "working") {
    const fact = agent.run_id
      ? `${agent.herdr_name ?? session} is not working`
      : "it has no run to interrupt";
    return { reply: refused(422, "refused", `${fact}; nothing was sent`), followUps: [] };
  }
  const count = (interrupts.get(session) ?? 0) + 1;
  interrupts.set(session, count);
  const confirmed = count % 2 === 1;
  const data = {
    run: agent.run_id,
    session,
    pane_id: agent.pane_id ?? "w1:p1",
    cancel: confirmed ? "confirmed" : "unconfirmed",
    evidence: confirmed
      ? "observation Stop after the Escape"
      : "no interrupt marker or turn end within 10 s; herdr now shows working",
  };
  return { reply: recorded(data, null, null), followUps: [] };
}

// The `dialog` scenario's screens: the folder trust dialog, which a tap
// answers, and a menu the board does not know, so its read finds no dialog.
function dialogMutation(
  session: string,
  read: boolean,
  body: Record<string, unknown>,
): MutationResult {
  if (read) {
    const known = session === DIALOG_SESSIONS.trust;
    const reply = {
      session_id: session,
      read_at: new Date().toISOString(),
      prompt: known ? TRUST_PROMPT : null,
      reason: known ? null : "no_dialog",
    };
    return { reply: { status: 200, body: reply }, followUps: [] };
  }
  if (session !== DIALOG_SESSIONS.trust || body.screen !== TRUST_PROMPT.screen) {
    return {
      reply: refused(409, "screen_changed", "the prompt changed after the buttons were drawn"),
      followUps: [],
    };
  }
  const exit = body.option === 1;
  if (exit) changeSession(session, { stopped: true });
  else changeSession(session, { fresh: true, activity: "working", at: new Date().toISOString() });
  return {
    reply: recorded({ session_id: session, sent: { option: body.option } }, null, null),
    followUps: [sessionEvent(session, exit ? "session.ended" : "session.started")],
  };
}

const TRUST_PROMPT: PromptDialog = {
  layout: "trust",
  tool: "",
  title: "Accessing workspace:",
  command: null,
  folder: "/home/user/project",
  question: "Is this a project you created or one you trust?",
  options: [
    { n: 1, text: "No, exit" },
    { n: 2, text: "Yes, I trust this folder" },
  ],
  screen: "fixture-trust",
};

function fixturePrompt(redrawn: boolean): PromptDialog {
  const command = redrawn
    ? "rm -rf target && cargo build -p gamma-cert"
    : "rm -rf target/debug/incremental && cargo build -p gamma-cert";
  return {
    layout: "permission",
    tool: "Bash",
    title: "Bash command",
    command,
    folder: null,
    question: "Do you want to proceed?",
    options: [
      { n: 1, text: "Yes" },
      {
        n: 2,
        text: "Yes, and don't ask again for rm and cargo build commands in /home/user/gamma",
      },
      { n: 3, text: "No" },
    ],
    screen: redrawn ? "fixture-changed" : "fixture",
  };
}

// A known idempotency key returns the stored reply and does nothing again.
// `scenario` is the page's, for the project previews a delete or archive reads.
export function fixtureMutation(
  method: string,
  path: string,
  body: Record<string, unknown>,
  scenario: Scenario = "default",
): MutationResult {
  const key = typeof body.idempotency_key === "string" ? body.idempotency_key : null;
  const known = key ? replies.get(key) : undefined;
  if (known) {
    const replayed =
      known.reply.status === 200
        ? { ...known.reply, body: { ...(known.reply.body as object), replayed: true } }
        : known.reply;
    return { reply: replayed, followUps: [] };
  }
  const result = handle(method, path, body, scenario);
  // As on the daemon, only an applied mutation keeps its key: a refusal rolled
  // back, and a 504 was never confirmed, so the retry with the same key runs again.
  if (key && result.reply.status === 200) replies.set(key, result);
  return result;
}
