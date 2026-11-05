import { QueryClient } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type {
  AttentionResponse,
  BoardResponse,
  BoardRow,
  ChangeEvent,
  MachinesResponse,
  ProjectsResponse,
  QuotaResponse,
} from "./api-types";
import { connectionStore } from "./connection";
import { fixtureSnapshot } from "./fixtures";
import { hydrateFromSnapshot } from "./boot";
import {
  attentionPath,
  boardPath,
  machinesPath,
  projectsPath,
  queryKeyFor,
  quotaPath,
} from "./queries";
import { applyChange, connectEvents } from "./sse";
import type { EventSourceLike } from "./sse";

const NOW = Date.parse("2026-11-02T12:00:00.000Z");

function seededClient() {
  const queryClient = new QueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default", NOW));
  return queryClient;
}

function event(fields: Partial<ChangeEvent>): ChangeEvent {
  return {
    seq: 4182,
    at: "2026-11-02T12:00:01.000Z",
    kind: "x.changed",
    txn: "x",
    entity: "x",
    entity_id: "x",
    rev_after: null,
    card_id: null,
    entity_state: null,
    ...fields,
  };
}

test("a card event replaces the card's row in the unfiltered board and adds a new one", () => {
  const queryClient = seededClient();
  const before = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  const first = before?.rows[0] as BoardRow;

  applyChange(
    queryClient,
    event({
      entity: "card",
      card_id: first.id,
      card: { ...first, column: "done", badge: "merged" },
    }),
  );
  applyChange(
    queryClient,
    event({
      seq: 4183,
      entity: "card",
      card_id: "new",
      card: { ...first, id: "new", display_id: "alpha-99" },
    }),
  );

  const after = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  expect(after?.rows.find((row) => row.id === first.id)?.column).toBe("done");
  expect(after?.rows).toHaveLength((before?.rows.length ?? 0) + 1);
});

test("an entity event older than the cached rev is ignored, a newer one wins", () => {
  const queryClient = seededClient();
  const machines = queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath));
  const hostA = machines?.machines[0];
  if (!hostA) throw new Error("fixture has no machine");

  applyChange(
    queryClient,
    event({
      entity: "host",
      entity_id: hostA.id,
      rev_after: hostA.rev - 1,
      entity_state: { ...hostA, rev: hostA.rev - 1, running_agents: 99 },
    }),
  );
  expect(
    queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath))?.machines[0]
      ?.running_agents,
  ).toBe(hostA.running_agents);

  applyChange(
    queryClient,
    event({
      seq: 4183,
      entity: "host",
      entity_id: hostA.id,
      rev_after: hostA.rev + 1,
      entity_state: { ...hostA, rev: hostA.rev + 1, running_agents: 4 },
    }),
  );
  expect(
    queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath))?.machines[0]
      ?.running_agents,
  ).toBe(4);
});

test("an answered attention item leaves the open queue", () => {
  const queryClient = seededClient();
  const open = queryClient.getQueryData<AttentionResponse>(queryKeyFor(attentionPath));
  const item = open?.items[0];
  if (!item) throw new Error("fixture has no attention item");

  applyChange(
    queryClient,
    event({
      entity: "attention",
      entity_id: item.id,
      entity_state: { ...item, rev: item.rev + 1, answered_at: "2026-11-02T12:00:00.000Z" },
    }),
  );

  const after = queryClient.getQueryData<AttentionResponse>(queryKeyFor(attentionPath));
  expect(after?.items.find((other) => other.id === item.id)).toBeUndefined();
});

test("a quota window event updates the window by label", () => {
  const queryClient = seededClient();
  const quota = queryClient.getQueryData<QuotaResponse>(queryKeyFor(quotaPath));
  const weekly = quota?.windows.find((window) => window.kind === "weekly");
  if (!weekly) throw new Error("fixture has no weekly window");

  applyChange(
    queryClient,
    event({
      entity: "quota",
      entity_id: weekly.label,
      entity_state: { ...weekly, percent_remaining: 12, dot: "amber" },
    }),
  );

  const after = queryClient.getQueryData<QuotaResponse>(queryKeyFor(quotaPath));
  expect(after?.windows.find((window) => window.label === weekly.label)?.dot).toBe("amber");
  expect(after?.windows).toHaveLength(quota?.windows.length ?? 0);
});

class FakeSource implements EventSourceLike {
  static all: FakeSource[] = [];
  readyState = 1;
  closed = false;
  private listeners = new Map<string, ((event: MessageEvent<string>) => void)[]>();
  constructor(readonly url: string) {
    FakeSource.all.push(this);
  }
  addEventListener(type: string, listener: (event: MessageEvent<string>) => void) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }
  close() {
    this.closed = true;
  }
  emit(type: string, data?: unknown) {
    for (const listener of this.listeners.get(type) ?? []) {
      listener({ data: JSON.stringify(data) } as MessageEvent<string>);
    }
  }
}

beforeEach(() => {
  FakeSource.all = [];
  connectionStore.set("connecting");
});

afterEach(() => {
  vi.useRealTimers();
});

function connect(
  queryClient: QueryClient,
  lastEventSeq = 4181,
  snapshot = fixtureSnapshot("default", NOW),
) {
  return connectEvents({
    queryClient,
    lastEventSeq,
    createEventSource: (url) => new FakeSource(url),
    fetchSnapshot: () => Promise.resolve(snapshot),
  });
}

test("the stream applies each event once and skips a replayed seq", () => {
  const queryClient = seededClient();
  const stop = connect(queryClient);
  const source = FakeSource.all[0] as FakeSource;
  const machines = queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath));
  const hostA = machines?.machines[0];
  if (!hostA) throw new Error("fixture has no machine");

  const changed = event({
    seq: 4182,
    entity: "host",
    entity_id: hostA.id,
    entity_state: { ...hostA, rev: hostA.rev + 1, running_agents: 5 },
  });
  source.emit("change", changed);
  source.emit(
    "change",
    event({
      seq: 4182,
      entity: "host",
      entity_id: hostA.id,
      entity_state: { ...hostA, rev: hostA.rev + 2, running_agents: 0 },
    }),
  );
  source.emit(
    "change",
    event({
      seq: 4100,
      entity: "host",
      entity_id: hostA.id,
      entity_state: { ...hostA, rev: hostA.rev + 3, running_agents: 0 },
    }),
  );

  expect(
    queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath))?.machines[0]
      ?.running_agents,
  ).toBe(5);
  stop();
});

test("a reset refetches the snapshot into the cache without clearing it first", async () => {
  const queryClient = seededClient();
  const next = fixtureSnapshot("quota-red", NOW);
  const stop = connect(queryClient, 4181, next);
  const source = FakeSource.all[0] as FakeSource;

  source.emit("reset", { seq: 9000 });
  // The old data is still there until the new snapshot lands.
  expect(queryClient.getQueryData<QuotaResponse>(queryKeyFor(quotaPath))?.windows[0]?.dot).toBe(
    "green",
  );
  await vi.waitFor(() => {
    expect(queryClient.getQueryData<QuotaResponse>(queryKeyFor(quotaPath))?.windows[0]?.dot).toBe(
      "red",
    );
  });
  stop();
});

test("after a reset to a restored database, a newer change is applied before the snapshot lands", () => {
  const queryClient = seededClient();
  const stop = connectEvents({
    queryClient,
    lastEventSeq: 4181,
    createEventSource: (url) => new FakeSource(url),
    fetchSnapshot: () => new Promise<never>(() => {}),
  });
  const source = FakeSource.all[0] as FakeSource;
  const hostA = queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath))?.machines[0];
  if (!hostA) throw new Error("fixture has no machine");

  // The restored log ends at 100, below the 4181 this page had seen.
  source.emit("reset", { seq: 100 });
  source.emit(
    "change",
    event({
      seq: 100,
      entity: "host",
      entity_id: hostA.id,
      entity_state: { ...hostA, rev: hostA.rev + 1, running_agents: 7 },
    }),
  );
  expect(
    queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath))?.machines[0]
      ?.running_agents,
  ).toBe(hostA.running_agents);
  source.emit(
    "change",
    event({
      seq: 101,
      entity: "host",
      entity_id: hostA.id,
      entity_state: { ...hostA, rev: hostA.rev + 2, running_agents: 5 },
    }),
  );
  expect(
    queryClient.getQueryData<MachinesResponse>(queryKeyFor(machinesPath))?.machines[0]
      ?.running_agents,
  ).toBe(5);
  stop();
});

test("without a boot snapshot the stream starts live and fetches the snapshot once open", async () => {
  vi.useFakeTimers();
  const queryClient = seededClient();
  const snapshot = fixtureSnapshot("quota-red", NOW);
  const fetchSnapshot = vi.fn(() => Promise.resolve(snapshot));
  const stop = connectEvents({
    queryClient,
    lastEventSeq: null,
    createEventSource: (url) => new FakeSource(url),
    fetchSnapshot,
  });
  const first = FakeSource.all[0] as FakeSource;
  // No `since=0`: that would replay the whole log.
  expect(first.url).toBe("/api/v1/events");

  first.emit("open");
  expect(fetchSnapshot).toHaveBeenCalledTimes(1);
  await vi.waitFor(() => {
    expect(queryClient.getQueryData<QuotaResponse>(queryKeyFor(quotaPath))?.windows[0]?.dot).toBe(
      "red",
    );
  });

  // A reopen resumes after the snapshot's seq and does not fetch again.
  first.readyState = 2;
  first.emit("error");
  await vi.advanceTimersByTimeAsync(1000);
  const second = FakeSource.all[1] as FakeSource;
  expect(second.url).toBe(`/api/v1/events?since=${snapshot.last_event_seq}`);
  second.emit("open");
  expect(fetchSnapshot).toHaveBeenCalledTimes(1);
  stop();
});

test("the stream starts after the boot snapshot's seq", () => {
  const stop = connect(seededClient(), 4181);
  expect((FakeSource.all[0] as FakeSource).url).toBe("/api/v1/events?since=4181");
  stop();
});

test("the connection state follows open and error, and a closed stream reopens after the last applied seq", async () => {
  vi.useFakeTimers();
  const queryClient = seededClient();
  const fetchSnapshot = vi.fn(() => Promise.resolve(fixtureSnapshot("quota-amber", NOW)));
  const stop = connectEvents({
    queryClient,
    lastEventSeq: 4181,
    createEventSource: (url) => new FakeSource(url),
    fetchSnapshot,
  });
  const first = FakeSource.all[0] as FakeSource;

  first.emit("open");
  expect(connectionStore.get()).toBe("live");
  first.emit("change", event({ seq: 4185, entity: "session" }));

  first.readyState = 2;
  first.emit("error");
  expect(connectionStore.get()).toBe("reconnecting");
  expect(first.closed).toBe(true);

  await vi.advanceTimersByTimeAsync(1000);
  const second = FakeSource.all[1] as FakeSource;
  expect(second?.url).toBe("/api/v1/events?since=4185");
  second.emit("open");
  expect(connectionStore.get()).toBe("live");
  // The replay covers the gap: no snapshot refetch, no flash.
  expect(fetchSnapshot).not.toHaveBeenCalled();
  stop();
});

test("a card event marks that card's detail and timeline stale, and no other card's", () => {
  const queryClient = seededClient();
  const detail = queryKeyFor("/api/v1/cards/c1");
  const timeline = queryKeyFor("/api/v1/cards/c1/timeline");
  const other = queryKeyFor("/api/v1/cards/c10");
  for (const key of [detail, timeline, other]) queryClient.setQueryData(key, {});

  applyChange(queryClient, event({ entity: "run", card_id: "c1" }));

  expect(queryClient.getQueryState(detail)?.isInvalidated).toBe(true);
  expect(queryClient.getQueryState(timeline)?.isInvalidated).toBe(true);
  // "c10" starts with "c1": only an exact card path or its children match.
  expect(queryClient.getQueryState(other)?.isInvalidated).toBe(false);
});

test("a session or run event marks the agents and run details stale", () => {
  const queryClient = seededClient();
  const run = queryKeyFor("/api/v1/runs/r1");
  queryClient.setQueryData(run, {});

  applyChange(queryClient, event({ entity: "session" }));

  expect(queryClient.getQueryState(run)?.isInvalidated).toBe(true);
  expect(queryClient.getQueryState(queryKeyFor("/api/v1/agents"))?.isInvalidated).toBe(true);
});

test("an archived project leaves the list instead of being written back", () => {
  const queryClient = seededClient();
  const alpha = queryClient
    .getQueryData<ProjectsResponse>(queryKeyFor(projectsPath))
    ?.projects.find((project) => project.id === "alpha");

  applyChange(
    queryClient,
    event({
      kind: "project.retired",
      entity: "project",
      entity_id: "alpha",
      entity_state: { ...alpha, retired_at: "2026-11-05T19:00:00Z" },
    }),
  );

  const projects = queryClient.getQueryData<ProjectsResponse>(queryKeyFor(projectsPath));
  expect(projects?.projects.map((project) => project.id)).not.toContain("alpha");
  expect(queryClient.getQueryState(queryKeyFor(boardPath()))?.isInvalidated).toBe(true);
});

test("a deleted project leaves the list and every read is marked stale", () => {
  const queryClient = seededClient();
  const peerGamma = {
    ...(queryClient.getQueryData<ProjectsResponse>(queryKeyFor(projectsPath))
      ?.projects[0] as ProjectsResponse["projects"][number]),
    id: "gamma",
    host: "host-d",
    link: "live" as const,
  };
  queryClient.setQueryData<ProjectsResponse>(queryKeyFor(projectsPath), (old) => ({
    projects: [...(old?.projects ?? []), peerGamma],
  }));

  applyChange(
    queryClient,
    event({ kind: "project.deleted", entity: "project", entity_id: "gamma" }),
  );

  const projects = queryClient.getQueryData<ProjectsResponse>(queryKeyFor(projectsPath));
  // This host's gamma is gone; the peer's project of the same id stays.
  expect(projects?.projects.filter((project) => project.id === "gamma")).toEqual([peerGamma]);
  expect(projects?.projects.map((project) => project.id)).toContain("alpha");
  expect(queryClient.getQueryState(queryKeyFor(boardPath()))?.isInvalidated).toBe(true);
  expect(queryClient.getQueryState(queryKeyFor(attentionPath))?.isInvalidated).toBe(true);
});

test("a review pass or obligation event marks the projects stale", () => {
  for (const entity of ["review_pass", "obligation"]) {
    const queryClient = seededClient();
    const projects = queryKeyFor(projectsPath);
    queryClient.setQueryData(projects, { projects: [] });

    applyChange(queryClient, event({ entity }));

    expect(queryClient.getQueryState(projects)?.isInvalidated).toBe(true);
  }
});

test("a proposal or memory event marks every memory view and revision stale, and nothing else", () => {
  for (const entity of ["proposal", "memory", "memory_revision"]) {
    const queryClient = seededClient();
    const view = queryKeyFor("/api/v1/memory?scope=alpha");
    const revision = queryKeyFor("/api/v1/memory/revisions/01JREV");
    queryClient.setQueryData(view, {});
    queryClient.setQueryData(revision, {});

    applyChange(queryClient, event({ entity }));

    expect(queryClient.getQueryState(view)?.isInvalidated).toBe(true);
    expect(queryClient.getQueryState(revision)?.isInvalidated).toBe(true);
    expect(queryClient.getQueryState(queryKeyFor(projectsPath))?.isInvalidated).toBe(false);
  }
});
