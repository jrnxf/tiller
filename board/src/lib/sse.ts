import type { QueryClient } from "@tanstack/react-query";

import type {
  AttentionItem,
  AttentionResponse,
  BoardResponse,
  BoardRow,
  ChangeEvent,
  Machine,
  MachinesResponse,
  Project,
  ProjectsResponse,
  QuotaResponse,
  QuotaWindow,
  Snapshot,
} from "./api-types";
import { connectionStore } from "./connection";
import { hydrateFromSnapshot } from "./boot";
import { fetchJson, machinesPath, projectsPath, quotaPath, queryKeyFor } from "./queries";
import type { QueryPath } from "./queries";

const EVENTS_PATH = "/api/v1/events";
export const SNAPSHOT_PATH = "/api/v1/snapshot";

const attentionKey = queryKeyFor("/api/v1/attention?state=open");

function hasRev(value: unknown): value is { rev: number } {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { rev?: unknown }).rev === "number"
  );
}

// Insert or replace by `key`, unless the cache already holds a higher `rev`
// (a later write may already be included in `entity_state`; the client keeps
// whichever rev is higher, protocol.md section 7.5).
function upsert<T>(list: T[], item: T, same: (other: T) => boolean): T[] {
  const index = list.findIndex(same);
  if (index === -1) return [...list, item];
  const current = list[index];
  if (current !== undefined && hasRev(current) && hasRev(item) && current.rev > item.rev) {
    return list;
  }
  return list.map((existing, i) => (i === index ? item : existing));
}

function upsertBoardRow(
  response: BoardResponse | undefined,
  row: BoardRow,
): BoardResponse | undefined {
  if (!response) return response;
  const index = response.rows.findIndex((existing) => existing.id === row.id);
  const rows =
    index === -1
      ? [...response.rows, row]
      : response.rows.map((existing, i) => (i === index ? row : existing));
  return { ...response, rows };
}

function writeBoard(queryClient: QueryClient, row: BoardRow) {
  // Only the unfiltered board can take a row in place: a filtered list may need
  // the card to enter or leave, which only the server can say. Those refetch
  // quietly; their old data stays on screen until the answer lands.
  const unfiltered = queryKeyFor("/api/v1/board?filter=all");
  queryClient.setQueryData<BoardResponse | undefined>(unfiltered, (old) =>
    upsertBoardRow(old, row),
  );
  queryClient.invalidateQueries({
    queryKey: ["api"],
    predicate: (query) => {
      const path = String(query.queryKey[1] ?? "");
      return path.startsWith("/api/v1/board") && path !== "/api/v1/board?filter=all";
    },
    refetchType: "active",
  });
}

function writeAttention(queryClient: QueryClient, item: AttentionItem) {
  queryClient.setQueryData<AttentionResponse | undefined>(attentionKey, (old) => {
    if (!old) return old;
    const closed = item.answered_at !== null || item.withdrawn_at !== null;
    if (closed) return { ...old, items: old.items.filter((other) => other.id !== item.id) };
    return { ...old, items: upsert(old.items, item, (other) => other.id === item.id) };
  });
}

function writeMachine(queryClient: QueryClient, machine: Machine) {
  queryClient.setQueryData<MachinesResponse | undefined>(queryKeyFor(machinesPath), (old) =>
    old ? { machines: upsert(old.machines, machine, (other) => other.id === machine.id) } : old,
  );
}

function writeQuota(queryClient: QueryClient, window: QuotaWindow) {
  queryClient.setQueryData<QuotaResponse | undefined>(queryKeyFor(quotaPath), (old) =>
    old
      ? { ...old, windows: upsert(old.windows, window, (other) => other.label === window.label) }
      : old,
  );
}

function writeProject(queryClient: QueryClient, project: Project) {
  queryClient.setQueryData<ProjectsResponse | undefined>(queryKeyFor(projectsPath), (old) =>
    old ? { projects: upsert(old.projects, project, (other) => other.id === project.id) } : old,
  );
}

// Entities whose API shape is a join of several tables: the event says
// something moved, and the quiet refetch fetches the joined answer.
const REFETCH_ENTITIES = new Set(["session", "run", "pane", "observation", "worktree"]);

export function applyChange(queryClient: QueryClient, event: ChangeEvent): void {
  const state = event.entity_state;

  switch (event.entity) {
    case "attention":
      if (state) writeAttention(queryClient, state as AttentionItem);
      break;
    case "host":
      if (state) writeMachine(queryClient, state as Machine);
      break;
    case "quota_window":
      if (state) writeQuota(queryClient, state as QuotaWindow);
      break;
    case "project":
      if (state) writeProject(queryClient, state as Project);
      break;
    default:
      if (REFETCH_ENTITIES.has(event.entity)) {
        queryClient.invalidateQueries({
          queryKey: ["api"],
          predicate: (query) => String(query.queryKey[1] ?? "").startsWith("/api/v1/agents"),
          refetchType: "active",
        });
      }
  }

  if (event.card) {
    writeBoard(queryClient, event.card);
  }
  if (event.card_id) {
    const detail: QueryPath = `/api/v1/cards/${event.card_id}`;
    queryClient.invalidateQueries({ queryKey: queryKeyFor(detail), refetchType: "active" });
  }
}

export interface EventSourceLike {
  readyState: number;
  addEventListener(type: "open" | "error", listener: () => void): void;
  addEventListener(type: string, listener: (event: MessageEvent<string>) => void): void;
  close(): void;
}

export interface EventsOptions {
  queryClient: QueryClient;
  lastEventSeq: number;
  createEventSource?: (url: string) => EventSourceLike;
  fetchSnapshot?: () => Promise<Snapshot>;
}

const CLOSED = 2;

// Connects the stream and writes each event into the Query cache. Returns a
// function that stops it. The browser reconnects by itself and sends
// `Last-Event-ID`, so a short gap replays; a long one arrives as `reset`.
export function connectEvents(options: EventsOptions): () => void {
  const { queryClient } = options;
  const createEventSource =
    options.createEventSource ?? ((url) => new EventSource(url) as EventSourceLike);
  const fetchSnapshot = options.fetchSnapshot ?? (() => fetchJson<Snapshot>(SNAPSHOT_PATH));

  let lastSeq = options.lastEventSeq;
  let source: EventSourceLike | null = null;
  let stopped = false;
  let needsResync = false;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;

  // Refresh every query from a new snapshot while the UI stays put: only the
  // degraded pill shows, never a spinner or an empty state.
  async function resync() {
    try {
      const snapshot = await fetchSnapshot();
      if (stopped) return;
      // Exact, not max: after a restored database the newest seq goes down.
      lastSeq = hydrateFromSnapshot(queryClient, snapshot);
    } catch {
      // Still unreachable: the next reconnect tries again.
    }
  }

  function open() {
    if (stopped) return;
    const next = createEventSource(EVENTS_PATH);
    source = next;

    next.addEventListener("open", () => {
      connectionStore.set("live");
      // After the browser gave up and we opened a new stream there is no
      // `Last-Event-ID`; the snapshot taken after this open covers the gap.
      if (needsResync) {
        needsResync = false;
        void resync();
      }
    });
    next.addEventListener("error", () => {
      connectionStore.set("reconnecting");
      if (next.readyState === CLOSED && !stopped) {
        next.close();
        needsResync = true;
        retryTimer = setTimeout(open, 1000);
      }
    });
    next.addEventListener("change", (message) => {
      const event = JSON.parse(message.data) as ChangeEvent;
      if (event.seq <= lastSeq) return;
      lastSeq = event.seq;
      applyChange(queryClient, event);
    });
    next.addEventListener("reset", () => {
      void resync();
    });
  }

  open();

  return () => {
    stopped = true;
    clearTimeout(retryTimer);
    source?.close();
  };
}
