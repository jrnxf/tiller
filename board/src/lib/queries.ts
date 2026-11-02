import { queryOptions } from "@tanstack/react-query";

import type {
  AgentsResponse,
  AttentionResponse,
  BoardFilter,
  BoardResponse,
  CardDetail,
  MachinesResponse,
  ProjectsResponse,
  QuotaResponse,
  RunDetail,
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

  constructor(path: string, status: number, reason: string | null, hint: string | null = null) {
    super(`${path} answered ${status}${reason ? `: ${reason}` : ""}`);
    this.name = "ReadError";
    this.path = path;
    this.status = status;
    this.reason = reason;
    this.hint = hint;
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
    return new ReadError(path, response.status, reason, text(body.error?.hint));
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

export const signalsPath: QueryPath = "/api/v1/signals";
export const signalsQuery = () => readQuery<SignalsResponse>(signalsPath);

export const projectsPath: QueryPath = "/api/v1/projects";
export const projectsQuery = () => readQuery<ProjectsResponse>(projectsPath);

const cardPath = (cardId: string): QueryPath => `/api/v1/cards/${cardId}`;
export const cardQuery = (cardId: string) => readQuery<CardDetail>(cardPath(cardId));

export const timelinePath = (cardId: string, before?: number): QueryPath =>
  withParams(`/api/v1/cards/${cardId}/timeline`, {
    before: before === undefined ? undefined : String(before),
  });
export const timelineQuery = (cardId: string) => readQuery<TimelineResponse>(timelinePath(cardId));

const runPath = (runId: string): QueryPath => `/api/v1/runs/${runId}`;
export const runQuery = (runId: string) => readQuery<RunDetail>(runPath(runId));
