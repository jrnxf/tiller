import { queryOptions } from "@tanstack/react-query";

import type {
  AgentsResponse,
  AwayBriefResponse,
  AttentionResponse,
  BackupList,
  PeerList,
  BoardFilter,
  BoardResponse,
  CardDetail,
  CheckoutCheck,
  ContributionsResponse,
  DaemonHealth,
  DoctorReport,
  HostConfig,
  HostFields,
  MachinesResponse,
  MemoryEvalPlan,
  MemoryResponse,
  MemoryRevision,
  ProjectDeletePreview,
  ProjectFields,
  ProjectGrants,
  ProjectRetirePreview,
  ProjectSetup,
  ProjectsResponse,
  QuotaResponse,
  RecapResponse,
  RepliesResponse,
  RetrosResponse,
  RunDetail,
  SessionView,
  SignalsResponse,
  StartSettings,
  TimelineResponse,
} from "./api-types.ts";

// A query key is the request path with its query string, exactly as the boot
// snapshot keys its entries (protocol.md section 7.6), so hydrating never
// drifts from fetching.
export type QueryPath = `/api/v1/${string}`;

export function queryKeyFor(path: QueryPath) {
  return ["api", path] as const;
}

export const API_KEY_PREFIX = ["api"] as const;

function withParams(path: QueryPath, params: Record<string, string | undefined>): QueryPath {
  const search = new URLSearchParams();
  for (const [name, value] of Object.entries(params)) {
    if (value) search.set(name, value);
  }
  const text = search.toString();
  return text ? (`${path}?${text}` as QueryPath) : path;
}

// A read the daemon refused, with its error body's message and hint when it
// sent them (protocol.md section 7.3), so the screen can say why and what next.
export class ReadError extends Error {
  readonly path: string;
  readonly status: number;
  readonly reason: string | null;
  readonly hint: string | null;
  // The error body's code, such as `signed_out`.
  readonly code: string | null;

  constructor(
    path: string,
    status: number,
    reason: string | null,
    hint: string | null = null,
    code: string | null = null,
  ) {
    super(`${path} answered ${status}${reason ? `: ${reason}` : ""}`);
    this.name = "ReadError";
    this.path = path;
    this.status = status;
    this.reason = reason;
    this.hint = hint;
    this.code = code;
  }
}

const text = (value: unknown) => (typeof value === "string" ? value : null);

async function readError(path: string, response: Response): Promise<ReadError> {
  try {
    const body = (await response.json()) as {
      error?: { code?: unknown; message?: unknown; hint?: unknown };
    };
    // The daemon's message repeats its code ("unavailable: ..."); the reason
    // is what follows.
    const code = text(body.error?.code);
    const message = text(body.error?.message);
    const reason =
      code && message?.startsWith(`${code}: `) ? message.slice(code.length + 2) : message;
    return new ReadError(path, response.status, reason, text(body.error?.hint), code);
  } catch {
    return new ReadError(path, response.status, null);
  }
}

export async function fetchJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(path, { headers: { accept: "application/json" }, signal });
  if (!response.ok) {
    throw await readError(path, response);
  }
  return (await response.json()) as T;
}

function readQuery<T>(path: QueryPath) {
  return queryOptions({
    queryKey: queryKeyFor(path),
    queryFn: ({ signal }) => fetchJson<T>(path, signal),
  });
}

export interface BoardParams {
  project?: string;
  filter?: BoardFilter;
  host?: string;
}

export function boardPath({ project, filter = "all", host }: BoardParams = {}): QueryPath {
  return withParams("/api/v1/board", { project, filter, host });
}

export const boardQuery = (params?: BoardParams) => readQuery<BoardResponse>(boardPath(params));

export const attentionPath: QueryPath = "/api/v1/attention?state=open";
export const attentionQuery = () => readQuery<AttentionResponse>(attentionPath);

// What happened since the owner last marked a recap seen. It is read after
// the page opens, not in the boot snapshot, and a change that moves a count
// refetches it (lib/sse.ts).
export const recapPath: QueryPath = "/api/v1/recap";
export const recapQuery = () => readQuery<RecapResponse>(recapPath);

// One of this host's projects' unread replies from the orchestrator, newest
// first. Never federated: a peer's replies stay on the peer. An `ask.replied`
// or `reply.read` event refreshes it (lib/sse.ts).
export const repliesPath = (projectId: string): QueryPath =>
  `/api/v1/projects/${projectId}/replies`;
export const repliesQuery = (projectId: string) =>
  readQuery<RepliesResponse>(repliesPath(projectId));

export const agentsPath = (host?: string): QueryPath => withParams("/api/v1/agents", { host });
export const agentsQuery = (host?: string) => readQuery<AgentsResponse>(agentsPath(host));

export const machinesPath: QueryPath = "/api/v1/machines";
export const machinesQuery = () => readQuery<MachinesResponse>(machinesPath);

export const quotaPath: QueryPath = "/api/v1/quota";
export const quotaQuery = () => readQuery<QuotaResponse>(quotaPath);

// Whether this browser is signed in as the owner, for Settings' Sign out.
export const sessionPath: QueryPath = "/api/v1/session";
export const sessionQuery = () => readQuery<SessionView>(sessionPath);

// The daemon's own facts (version, start time, merge App), for Settings.
const healthPath: QueryPath = "/api/v1/health";
export const healthQuery = () => readQuery<DaemonHealth>(healthPath);

// The checks `tiller doctor` runs, run on the board's host each time the host
// card opens or the owner asks again, so never cached. With a peer's label
// the peer runs them and the board reads its answer.
const doctorPath: QueryPath = "/api/v1/doctor";
export const doctorQuery = (host?: string) =>
  queryOptions({ ...readQuery<DoctorReport>(withParams(doctorPath, { host })), staleTime: 0 });

// The fields `tiller host edit` sets on this host, and what it holds and has
// free now. Read fresh each time its card opens, so a save goes against the
// current `rev`.
export const hostFieldsPath: QueryPath = "/api/v1/host";
export const hostFieldsQuery = () =>
  queryOptions({ ...readQuery<HostFields>(hostFieldsPath), staleTime: 0 });

// The recent backups of this host, and whether one runs. The owner's.
export const backupsPath: QueryPath = "/api/v1/backups";
export const backupsQuery = () =>
  queryOptions({ ...readQuery<BackupList>(backupsPath), staleTime: 0 });

// Both ends of this host's links: the hosts this board paired with and the
// boards whose keys this host accepted. The owner's.
export const peersPath: QueryPath = "/api/v1/peers";
// Each host card reads it, so a card that mounts uses the list read in the
// last 30 s; a `host` or `peer_key` event refetches it at once.
export const peersQuery = () =>
  queryOptions({ ...readQuery<PeerList>(peersPath), staleTime: 30_000 });

export const signalsPath: QueryPath = "/api/v1/signals";
export const signalsQuery = () => readQuery<SignalsResponse>(signalsPath);

// The newest away contract and its return brief. Read fresh each time it is
// shown, since the window it covers is still growing while the owner is away.
export const awayBriefPath: QueryPath = "/api/v1/away/brief";
export const awayBriefQuery = () =>
  queryOptions({ ...readQuery<AwayBriefResponse>(awayBriefPath), staleTime: 0 });

export const projectsPath: QueryPath = "/api/v1/projects";
export const projectsQuery = () => readQuery<ProjectsResponse>(projectsPath);

// This host's archived (retired) projects, which only a delete still touches.
export const retiredProjectsQuery = () =>
  readQuery<ProjectsResponse>(withParams(projectsPath, { retired: "true" }));

// The prefix of every contributions read, with or without a project filter.
export const CONTRIBUTIONS_PREFIX = "/api/v1/contributions";
const contributionsPath = (project?: string): QueryPath =>
  withParams(CONTRIBUTIONS_PREFIX, { project });
export const contributionsQuery = (project?: string) =>
  readQuery<ContributionsResponse>(contributionsPath(project));

// Counted fresh each time the delete dialog opens: it is never in the boot
// snapshot and never shared with another read.
export const projectDeleteQuery = (projectId: string) =>
  queryOptions({
    ...readQuery<ProjectDeletePreview>(`/api/v1/projects/${projectId}/delete`),
    staleTime: 0,
    gcTime: 0,
  });

// What archiving a project would do: always read fresh, like the delete's.
export const projectRetireQuery = (projectId: string) =>
  queryOptions({
    ...readQuery<ProjectRetirePreview>(`/api/v1/projects/${projectId}/retire`),
    staleTime: 0,
    gcTime: 0,
  });

// What Add project would do with a local checkout, asked as the owner types.
export const projectCheckoutQuery = (params: {
  project: string;
  repo: string;
  path: string;
  clone: boolean;
}) =>
  queryOptions({
    ...readQuery<CheckoutCheck>(
      withParams("/api/v1/projects/checkout", {
        project: params.project,
        repo: params.repo,
        path: params.path,
        clone: params.clone ? "true" : undefined,
      }),
    ),
    staleTime: 0,
  });

// The host-level settings an orchestrator start reads. A `config` event
// refreshes it (lib/sse.ts).
export const startSettingsPath: QueryPath = "/api/v1/start-settings";
export const startSettingsQuery = () =>
  queryOptions({ ...readQuery<StartSettings>(startSettingsPath), staleTime: 0 });

// The other host keys the board sets (`GET /api/v1/host-config`). A `config`
// event refreshes it (lib/sse.ts).
export const hostConfigPath: QueryPath = "/api/v1/host-config";
export const hostConfigQuery = () =>
  queryOptions({ ...readQuery<HostConfig>(hostConfigPath), staleTime: 0 });

// The project detail's merge gate and checkout. A project event refreshes it,
// since the hourly ruleset check moves the gate, and so does a `config` event,
// since `checkouts.<id>` lives in config.toml (lib/sse.ts).
export const projectSetupPath = (projectId: string): QueryPath =>
  `/api/v1/projects/${projectId}/setup`;
export const projectSetupQuery = (projectId: string) =>
  queryOptions({ ...readQuery<ProjectSetup>(projectSetupPath(projectId)), staleTime: 0 });

// One of this host's projects as the Edit tab shows it. The dialog reads it
// fresh each time it opens, and a `project` event refreshes it (lib/sse.ts).
export const projectFieldsPath = (projectId: string): QueryPath => `/api/v1/projects/${projectId}`;
export const projectFieldsQuery = (projectId: string) =>
  queryOptions({ ...readQuery<ProjectFields>(projectFieldsPath(projectId)), staleTime: 0 });

// Every grant of one of this host's projects, live and ended. A `grant` event
// refreshes it, and so does an `approval` one, which moves a grant's uses
// (lib/sse.ts).
const projectGrantsPath = (projectId: string): QueryPath => `/api/v1/projects/${projectId}/grants`;
export const projectGrantsQuery = (projectId: string) =>
  queryOptions({ ...readQuery<ProjectGrants>(projectGrantsPath(projectId)), staleTime: 0 });

// A single-entity read takes `?host=<label>` for a peer's record; absent means
// this host (protocol.md section 7.3).
const cardPath = (cardId: string, owner?: string): QueryPath =>
  withParams(`/api/v1/cards/${cardId}`, { host: owner });
export const cardQuery = (cardId: string, owner?: string) =>
  readQuery<CardDetail>(cardPath(cardId, owner));

export const timelinePath = (cardId: string, before?: number, owner?: string): QueryPath =>
  withParams(`/api/v1/cards/${cardId}/timeline`, {
    before: before === undefined ? undefined : String(before),
    host: owner,
  });
export const timelineQuery = (cardId: string, owner?: string) =>
  readQuery<TimelineResponse>(timelinePath(cardId, undefined, owner));

const runPath = (runId: string, owner?: string): QueryPath =>
  withParams(`/api/v1/runs/${runId}`, { host: owner });
export const runQuery = (runId: string, owner?: string) =>
  readQuery<RunDetail>(runPath(runId, owner));

// The memory view: the document list and one document, `fleet` for the shared
// one (protocol.md section 7.3). Memory is local, so there is no `host`.
const memoryPath = (scope?: string): QueryPath => withParams("/api/v1/memory", { scope });
export const memoryQuery = (scope?: string) => readQuery<MemoryResponse>(memoryPath(scope));

// The full retros of the last 30 days and every unfinished one, for one
// project or all (learning-loop.md section 4). Retros are local, so there is
// no `host`.
export const RETROS_PREFIX = "/api/v1/retros";
export const retrosQuery = (project?: string) =>
  readQuery<RetrosResponse>(withParams(RETROS_PREFIX, { project }));

export const revisionQuery = (id: string) =>
  readQuery<MemoryRevision>(`/api/v1/memory/revisions/${id}`);

// What a proposal's holdout eval will cost, and why it will not start. Read
// fresh each time the dialog opens: the quota moves under it.
export const evalPlanQuery = (proposal: string) =>
  queryOptions({
    ...readQuery<MemoryEvalPlan>(`/api/v1/memory/proposals/${proposal}/eval`),
    staleTime: 0,
    gcTime: 0,
  });
