// U16: an edit made in the memory repo outside the board (an editor's save, a
// pull, a commit by hand) arrives as one `memory_repo` change. The open page
// reads the document again, and a draft in progress is never lost.

import { QueryClientProvider } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { ChangeEvent, MemoryResponse } from "./api-types";
import { hydrateFromSnapshot } from "./boot";
import { fixtureMutation } from "./fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "./fixtures";
import { resetMemoryFixture } from "./fixtures-memory";
import { loadPrefs, prefsStore } from "./prefs";
import { createQueryClient } from "./query-client";
import { receiptsStore } from "./receipts";
import { createAppRouter } from "./router";
import { applyChange } from "./sse";

const NOW = Date.parse("2026-11-02T12:00:00Z");
const FIRST = /^Before asking me to merge a sizeable PR/;
const SECOND = /^Stop new worker dispatch/;
const DRAFT = "My unsaved wording";

// The memory reads the page made, in order.
let reads: string[] = [];
let writes = 0;

// The stand-in daemon, backed by the same stateful fixture the dev server
// uses, so a change made "on disk" shows on the next read.
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
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default", NOW));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: ["/memory"] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  // The page has its document before the test edits it.
  await screen.findByText(FIRST, { selector: ".a-ln .tx" });
  return queryClient;
}

const page = () => within(screen.getByRole("article"));

const lineOf = (text: string | RegExp) => {
  const row = page().getByText(text, { selector: ".a-ln .tx" }).closest<HTMLElement>(".a-ln");
  if (!row) throw new Error("no line row");
  return row;
};

const draftBox = () =>
  screen.getByRole("textbox", { name: "Edit this line" }) as HTMLTextAreaElement;

// Someone rewrites a line of the shared document in the repo itself.
let outside = 0;
function rewriteOnDisk(index: number, text: string) {
  outside += 1;
  const doc = (fixtureResource("/api/v1/memory", "default", NOW) as MemoryResponse).doc;
  fixtureMutation("POST", "/api/v1/memory/edits", {
    idempotency_key: `outside-${outside}`,
    project: "fleet",
    base_blob: doc.blob,
    section: "How we work here",
    op: "rewrite",
    index,
    text,
  });
}

// The watcher's event, as the change stream delivers it.
const repoChanged = (seq: number): ChangeEvent => ({
  seq,
  at: "2026-11-02T12:00:05.000Z",
  kind: "memory_repo.changed",
  txn: "mem.outside_edit",
  entity: "memory_repo",
  entity_id: "memory",
  rev_after: null,
  card_id: null,
  entity_state: null,
});

async function streamDelivers(queryClient: QueryClient, seq: number) {
  const before = reads.length;
  act(() => applyChange(queryClient, repoChanged(seq)));
  // The page reads its document again, and the screen has caught up.
  await waitFor(() => expect(reads.length).toBeGreaterThan(before));
  await act(async () => {
    await new Promise((done) => setTimeout(done, 20));
  });
}

test("an outside edit to another line shows at once and the draft stays as typed", async () => {
  const queryClient = await renderMemory();
  fireEvent.click(lineOf(FIRST));
  fireEvent.change(draftBox(), { target: { value: DRAFT } });

  rewriteOnDisk(1, "Edited in the repo by hand.");
  await streamDelivers(queryClient, 4200);

  expect(reads.at(-1)).toMatch(/^\/api\/v1\/memory/);
  expect(await page().findByText("Edited in the repo by hand.")).toBeTruthy();
  expect(page().queryByText(SECOND)).toBeNull();
  // The line being edited did not change, so nothing interrupts the draft.
  expect(draftBox().value).toBe(DRAFT);
  expect(screen.queryByRole("alert")).toBeNull();
  expect(writes).toBe(0);
});

test("an outside edit to the line being edited keeps the draft and offers Save over or Discard", async () => {
  const queryClient = await renderMemory();
  fireEvent.click(lineOf(FIRST));
  fireEvent.change(draftBox(), { target: { value: DRAFT } });

  rewriteOnDisk(0, "Rewritten in the repo by hand.");
  await streamDelivers(queryClient, 4201);

  const notice = await screen.findByRole("alert");
  expect(notice.textContent).toMatch(/This line changed while you were editing/);
  expect(notice.textContent).toMatch(/Rewritten in the repo by hand\./);
  expect(draftBox().value).toBe(DRAFT);
  const editor = within(draftBox().closest(".ed") as HTMLElement);
  expect(editor.getByRole("button", { name: "Save over" })).toBeTruthy();
  expect(editor.getByRole("button", { name: "Discard" })).toBeTruthy();
  expect(writes).toBe(0);
});
