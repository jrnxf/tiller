// Stands in for the daemon's mutations (protocol.md section 7.4) on the dev and
// preview servers until T2.8 lands: a keyed replay table, the stale-head
// refusal, and the later receipt stages as change events.
import type { BoardRow, ChangeEvent, Machine, Project, PromptDialog } from "./api-types.ts";
import type {
  AgentsResponse,
  AttentionResponse,
  BoardResponse,
  InterventionKind,
  ProjectDeletePreview,
  ProjectRetirePreview,
  ProjectsResponse,
} from "./api-types.ts";
import { memoryMutation } from "./fixtures-memory.ts";
import {
  FIXTURE_HEAD_SHA,
  fixtureResource,
  restartStaleFixture,
  fixtureResponses,
  startOrchestratorFixture,
} from "./fixtures.ts";
import type { Scenario } from "./fixtures.ts";
import { relabelFixture } from "./fixtures-signals.ts";
import { agentsPath, attentionPath, boardPath, machinesPath, projectsPath } from "./queries.ts";

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
// The projects added since the server started, listed after the fixture's.
const added = new Map<string, Project>();

// A read the stand-in server answers, without the deleted projects' rows; a
// deleted or archived project's own previews are gone (a 404).
export function withoutDeleted(path: string, body: unknown, scenario: Scenario): unknown {
  const pathname = new URL(path, "http://localhost").pathname;
  const triaged = withTriage(pathname, body);
  if (triaged !== body) return triaged;
  if (deleted.size + retired.size + added.size === 0 || body === undefined) return body;
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
        ...projects.filter((project) => project.host || !gone(project.id)),
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
    /\/(stage|focus|label|delete|retire|bug-project)$/.test(path) ||
    path === projectsPath ||
    /\/memory\//.test(path)
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

// Tiller bug triage on the stand-in server: the cards a Move made, the cards
// it or "Close as not a bug" closed, and this host's bug project once the
// owner picks one. Every card the fixtures know is in the federated scenario.
const madeCards = new Map<string, BoardRow>();
const madeBodies = new Map<string, string>();
const closedCards = new Map<string, BoardRow>();
let pickedBugProject: { project: string | null; rev: number } | null = null;
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
  if (!local || !pickedBugProject) return local;
  const { project, rev } = pickedBugProject;
  return { ...local, rev, ...(project ? { bug_project: project } : { bug_project: null }) };
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
function withTriage(pathname: string, body: unknown): unknown {
  if (pathname === boardPath().split("?")[0] && body && madeCards.size + closedCards.size > 0) {
    const board = body as BoardResponse;
    const rows = board.rows.map((row) => closedCards.get(row.id) ?? row);
    const fresh = [...madeCards.values()].filter((row) => !rows.some((r) => r.id === row.id));
    return { ...board, rows: [...rows, ...fresh.map((row) => closedCards.get(row.id) ?? row)] };
  }
  if (pathname === machinesPath && body && pickedBugProject) {
    const { machines } = body as { machines: Machine[] };
    return {
      machines: machines.map((machine) =>
        machine.kind === "local" ? (localMachine("federated") ?? machine) : machine,
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

// `POST /api/v1/bug-project`: this host's pick, checked against its `rev`.
function pickBugProject(body: Record<string, unknown>, scenario: Scenario): MutationResult {
  const machine = localMachine(scenario);
  if (!machine) return refusal(400, "usage", "state.db is not initialized; run tiller init");
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
  pickedBugProject = { project, rev: machine.rev + 1 };
  const next = localMachine(scenario);
  // Like the daemon, the reply is the host record, not its Machines row.
  const host = {
    id: machine.id,
    label: machine.label,
    bug_project_id: project,
    rev: machine.rev + 1,
  };
  return {
    reply: recorded(host, null, null),
    followUps: [
      {
        delayMs: 100,
        event: {
          kind: "host.edited",
          txn: "host.edit",
          entity: "host",
          entity_id: machine.id,
          rev_after: next?.rev ?? null,
          card_id: null,
          entity_state: next,
        },
      },
    ],
  };
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
  if (method === "POST" && path === "/api/v1/cards" && body.category === "tiller-bug") {
    return createBug(body);
  }
  const close = /^\/api\/v1\/cards\/([^/]+)\/close$/.exec(path);
  if (method === "POST" && close?.[1]) return closeCard(close[1], body);
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
  const memory = memoryMutation(method, path, body);
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
  const orchestrator = /^\/api\/v1\/projects\/([^/]+)\/orchestrator$/.exec(path);
  if (method === "POST" && orchestrator?.[1]) {
    const project = orchestrator[1];
    const { live } = startOrchestratorFixture(
      project,
      typeof body.host === "string" ? body.host : undefined,
    );
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
  if (method === "POST" && /^\/api\/v1\/agents\/[^/]+\/focus$/.test(path)) {
    return { reply: recorded({}, null, null), followUps: [] };
  }
  // The permission dialog a waiting agent has up. The first tap finds it
  // changed, to see the board refuse a stale answer; a tap on the new one
  // is sent.
  const prompt = /^\/api\/v1\/agents\/([^/]+)\/prompt(\/answer)?$/.exec(path);
  if (method === "POST" && prompt?.[1]) {
    const session = prompt[1];
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

function fixturePrompt(redrawn: boolean): PromptDialog {
  const command = redrawn
    ? "rm -rf target && cargo build -p gamma-cert"
    : "rm -rf target/debug/incremental && cargo build -p gamma-cert";
  return {
    tool: "Bash",
    title: "Bash command",
    command,
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
