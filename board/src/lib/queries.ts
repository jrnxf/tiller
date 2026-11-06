import { queryOptions } from "@tanstack/react-query";

import type {
  AgentsResponse,
  AttentionResponse,
  BoardFilter,
  BoardResponse,
  CardDetail,
  ContributionsResponse,
  DaemonHealth,
  MachinesResponse,
  MemoryResponse,
  MemoryRevision,
  ProjectDeletePreview,
  ProjectRetirePreview,
  ProjectSetup,
  ProjectsResponse,
  QuotaResponse,
  RunDetail,
  SessionView,
  SignalsResponse,
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

export const signalsPath: QueryPath = "/api/v1/signals";
export const signalsQuery = () => readQuery<SignalsResponse>(signalsPath);

export const projectsPath: QueryPath = "/api/v1/projects";
export const projectsQuery = () => readQuery<ProjectsResponse>(projectsPath);

// This host's archived (retired) projects, which only a delete still touches.
export const retiredProjectsQuery = () =>
  readQuery<ProjectsResponse>(withParams(projectsPath, { retired: "true" }));

const contributionsPath = (project?: string): QueryPath =>
  withParams("/api/v1/contributions", { project });
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

// The project detail's merge gate and checkout. A project event refreshes it,
// since the hourly ruleset check moves the gate (lib/sse.ts).
export const projectSetupPath = (projectId: string): QueryPath =>
  `/api/v1/projects/${projectId}/setup`;
export const projectSetupQuery = (projectId: string) =>
  queryOptions({ ...readQuery<ProjectSetup>(projectSetupPath(projectId)), staleTime: 0 });

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

export const revisionQuery = (id: string) =>
  readQuery<MemoryRevision>(`/api/v1/memory/revisions/${id}`);
