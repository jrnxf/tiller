import { queryOptions } from "@tanstack/react-query";

import type {
  AgentsResponse,
  AttentionResponse,
  BoardFilter,
  BoardResponse,
  MachinesResponse,
  ProjectsResponse,
  QuotaResponse,
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

export async function fetchJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const response = await fetch(path, { headers: { accept: "application/json" }, signal });
  if (!response.ok) {
    throw new Error(`${path} answered ${response.status}`);
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

export const projectsPath: QueryPath = "/api/v1/projects";
export const projectsQuery = () => readQuery<ProjectsResponse>(projectsPath);
