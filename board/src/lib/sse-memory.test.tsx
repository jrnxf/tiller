// A note the fleet suggests, or an edit made elsewhere, arrives as one
// `memory.*` change. The open Memory page reads the queue again, and a draft
// in progress is never lost.

import { QueryClientProvider } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { ChangeEvent } from "./api-types";
import { hydrateFromSnapshot } from "./boot";
import { fixtureMutation } from "./fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "./fixtures";
import { curatorNote, resetMemoryFixture } from "./fixtures-memory";
import { loadPrefs, prefsStore } from "./prefs";
import { receiptsStore } from "./receipts";
import { createAppRouter } from "./router";
import { applyChange } from "./sse";
import { createTestQueryClient, loadRouteChunks } from "../test-app";

await loadRouteChunks();

const NOW = Date.parse("2026-11-02T12:00:00Z");
const FIRST = "A new owner-only route also needs an authority test";
const FIRST_ID = "01JPEND0000000000000000001";
const DRAFT = "My unsaved wording";

// The memory reads the page made, in order.
let reads: string[] = [];
let writes = 0;

// The stand-in daemon, backed by the same stateful fixture the dev server
// uses, so a change made elsewhere shows on the next read.
function stubDaemon() {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      if (path.startsWith("/api/v1/memory")) reads.push(path);
      const data = fixtureResource(path, "default", NOW);
      if (data === undefined) return new Response("{}", { status: 404 });
      return new Response(JSON.stringify(data), { status: 200 });
    }
    writes += 1;
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    const { reply } = fixtureMutation(method, path, body);
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  resetMemoryFixture(NOW);
  reads = [];
  writes = 0;
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
  window.scrollTo = () => {};
  stubDaemon();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

async function renderMemory() {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default", NOW));
  const router = createAppRouter(
    queryClient,
    createMemoryHistory({ initialEntries: ["/memory?project=alpha"] }),
  );
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  await screen.findByText(FIRST);
  return queryClient;
}

const pending = () => within(screen.getByRole("list", { name: "Pending notes" }));

const changed = (seq: number, kind: string): ChangeEvent => ({
  seq,
  at: "2026-11-02T12:00:05.000Z",
  kind,
  txn: kind.replace("memory.", "mem."),
  entity: "memory_pending",
  entity_id: "x",
  rev_after: null,
  card_id: null,
  entity_state: null,
});

async function streamDelivers(queryClient: QueryClient, event: ChangeEvent) {
  const before = reads.length;
  act(() => applyChange(queryClient, event));
  await waitFor(() => expect(reads.length).toBeGreaterThan(before));
  await act(async () => {
    await new Promise((done) => setTimeout(done, 20));
  });
}

test("a note the curator suggests shows in Pending at once", async () => {
  const queryClient = await renderMemory();

  curatorNote(
    "alpha",
    "01JCARD0000000000000000014",
    "01JRETRO000000000000000014",
    "Drain the export queue first",
    "A split mid-run loses the jobs in flight.",
    new Date(NOW).toISOString(),
  );
  await streamDelivers(queryClient, changed(4200, "memory.noted"));

  expect(await pending().findByText("Drain the export queue first")).toBeTruthy();
  expect(writes).toBe(0);
});

test("an edit made elsewhere keeps the open draft, and its Save says the note changed", async () => {
  const queryClient = await renderMemory();
  fireEvent.click(screen.getByRole("button", { name: `Edit "${FIRST}"` }));
  const title = screen.getByRole("textbox", { name: "Title" }) as HTMLInputElement;
  fireEvent.change(title, { target: { value: DRAFT } });

  fixtureMutation("PATCH", `/api/v1/memory/pending/${FIRST_ID}`, {
    idempotency_key: "elsewhere-1",
    rev: 1,
    body: "Rewritten on another board.",
  });
  await streamDelivers(queryClient, changed(4201, "memory.edited"));

  expect((screen.getByRole("textbox", { name: "Title" }) as HTMLInputElement).value).toBe(DRAFT);
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toMatch(/changed since you opened it/);
  expect(writes).toBe(1);
});
