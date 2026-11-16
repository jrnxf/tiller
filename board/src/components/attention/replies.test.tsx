import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { RepliesResponse, ReplyItem } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureResource, fixtureSnapshot, serverResponses } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { applyChange } from "../../lib/sse";
import { createTestQueryClient, loadAskEditor, loadRouteChunks } from "../../test-app";
import { askPrefillStore, prefillDoc } from "../shell/ask-prefill";

await loadRouteChunks();
await loadAskEditor();

// jsdom lays nothing out, so a Range has no rects. A prefilled Ask box puts
// the caret at the end and scrolls to it, which reads them.
Range.prototype.getClientRects ??= () => [] as unknown as DOMRectList;
Range.prototype.getBoundingClientRect ??= () =>
  ({ x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 }) as DOMRect;

const ALPHA_REPLY = "01JREPLY000000000000000001";
const BETA_REPLY = "01JREPLY000000000000000002";
const GAMMA_REPLY = "01JREPLY000000000000000003";

interface Call {
  method: string;
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];

// A daemon stand-in: answers reads from the fixtures and records every
// mutation, which it accepts.
function stubDaemon() {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      const data =
        (serverResponses("default") as Record<string, unknown>)[path] ??
        fixtureResource(path, "default");
      return data === undefined
        ? new Response(JSON.stringify({ ok: false, error: { code: "not_found" } }), {
            status: 404,
          })
        : new Response(JSON.stringify(data), { status: 200 });
    }
    calls.push({ method, path, body: JSON.parse(String(init?.body)) as Record<string, unknown> });
    return new Response(
      JSON.stringify({
        ok: true,
        replayed: false,
        events: [5001],
        receipt: { recorded_at: "2026-11-02T14:05:09.000Z", outbox_id: null, approval_id: null },
        data: {},
      }),
      { status: 200 },
    );
  });
}

async function renderAt(path: string) {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default"));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { router, queryClient };
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  askPrefillStore.set(null);
  calls = [];
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
  window.scrollTo = () => {};
  // jsdom ranges have no layout. The Ask box focuses the end of a prefilled
  // question a frame later, and the editor measures that range to scroll to it.
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect();
  stubDaemon();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const flush = () => act(async () => {});

test("the Replies filter lists each project's unread replies, newest first, each marked unread", async () => {
  await renderAt("/inbox");
  const needs = screen.getByRole("region", { name: "Needs you" });
  const list = await screen.findByRole("region", { name: "Replies" });
  await within(list).findByTestId(`inbox-row-${ALPHA_REPLY}`);
  // All lists the replies after what waits on the owner, under their own heading.
  expect(within(needs).getAllByRole("link").length).toBeGreaterThan(0);
  expect(within(needs).queryByTestId(`inbox-row-${ALPHA_REPLY}`)).toBeNull();
  expect(list.querySelector(".lsub")?.textContent).toBe("Replies 3");
  expect(needs.compareDocumentPosition(list) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  const replies = screen.getByRole("button", { name: /^Replies/ });
  expect(replies.textContent).toBe("Replies 3");
  fireEvent.click(replies);
  expect(replies.getAttribute("aria-pressed")).toBe("true");
  const rows = within(list).getAllByRole("link");
  expect(rows.map((row) => row.getAttribute("data-testid"))).toEqual([
    `inbox-row-${ALPHA_REPLY}`,
    `inbox-row-${BETA_REPLY}`,
    `inbox-row-${GAMMA_REPLY}`,
  ]);
  for (const row of rows) {
    expect(row.querySelector(".ii.kind")?.getAttribute("title")).toBe("Reply");
    expect(within(row).getByRole("img", { name: "Unread" })).toBeTruthy();
  }
  expect(rows[0]?.textContent).toContain("Can the export job retry on its own");
  // Nothing that is no longer current shows under Replies.
  expect(screen.queryByRole("region", { name: "No longer current" })).toBeNull();

  // The filter shows replies only, and needs no second heading.
  expect(within(needs).queryAllByRole("link")).toHaveLength(0);
  expect(list.querySelector(".lsub")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "All" }));
  expect(within(needs).getAllByRole("link").length).toBeGreaterThan(0);
  expect(within(list).getAllByRole("link")).toHaveLength(3);
});

test("the Inbox badge counts the unread replies too", async () => {
  const { queryClient } = await renderAt("/");
  const nav = screen.getByRole("navigation", { name: "Screens" });
  const inbox = within(nav).getByRole("link", { name: /^Inbox/ });
  await waitFor(() =>
    expect(queryClient.getQueryData(["api", "/api/v1/projects/alpha/replies"])).toBeTruthy(),
  );
  const before = Number(inbox.querySelector(".badge")?.textContent);
  act(() => {
    queryClient.setQueryData(["api", "/api/v1/projects/alpha/replies"], { replies: [] });
  });
  await waitFor(() => expect(Number(inbox.querySelector(".badge")?.textContent)).toBe(before - 1));
  expect(inbox.querySelector(".badge.warn")).toBeNull();
});

test("a reply that arrives off the Inbox shows a toast that links to it", async () => {
  const { queryClient, router } = await renderAt("/");
  const key = ["api", "/api/v1/projects/alpha/replies"];
  await waitFor(() => expect(queryClient.getQueryData(key)).toBeTruthy());
  // The replies already there are not news.
  expect(screen.getByTestId("toast").getAttribute("data-open")).toBe("false");
  const old = queryClient.getQueryData<RepliesResponse>(key)?.replies ?? [];
  const fresh = {
    ...old[0],
    id: "01JREPLY000000000000000009",
    question: "What should we add next?",
    replied_at: "2026-11-02T14:06:00.000Z",
  } as ReplyItem;
  act(() => queryClient.setQueryData(key, { replies: [fresh, ...old] }));
  const toast = screen.getByTestId("toast");
  await waitFor(() => expect(toast.getAttribute("data-open")).toBe("true"));
  expect(toast.textContent).toContain("a reply to your ask");
  expect(toast.textContent).toContain("What should we add next?");
  const open = within(toast).getByRole("link", { name: "Open in Inbox" });
  expect(open.getAttribute("href")).toBe("/inbox?item=01JREPLY000000000000000009");
  fireEvent.click(open);
  await waitFor(() => expect(router.state.location.pathname).toBe("/inbox"));
  expect(toast.getAttribute("data-open")).toBe("false");

  // On the Inbox the new reply is in the list, so no toast.
  act(() =>
    queryClient.setQueryData(key, {
      replies: [{ ...fresh, id: "01JREPLY00000000000000000A" }, fresh, ...old],
    }),
  );
  await waitFor(() =>
    expect(screen.getByTestId("inbox-row-01JREPLY00000000000000000A")).toBeTruthy(),
  );
  expect(toast.getAttribute("data-open")).toBe("false");
});

test("a reply's detail shows the question, the answer and the card the ask filed", async () => {
  await renderAt(`/inbox?item=${ALPHA_REPLY}`);
  // The replies are not in the boot snapshot: while they load, a link to
  // one does not say it is gone.
  expect(screen.queryByText("Not in the inbox any more")).toBeNull();
  const asked = await screen.findByText("You asked");
  expect(asked.textContent).toContain("You asked");
  expect(screen.getByText("Orchestrator replied")).toBeTruthy();
  expect(screen.getByTestId("reply-question").textContent).toBe(
    "Can the export job retry on its own when storage times out?",
  );
  expect(screen.getByTestId("reply-body").textContent).toContain("I filed alpha-41");
  const card = screen.getByTestId("reply-card");
  expect(card.querySelector(".id")?.textContent).toBe("alpha-41");
  expect(card.textContent).toContain("Pick the retry policy for the export job");
  expect(card.textContent).toContain("new card");

  // Ask again is secondary; Mark read is the primary and sits rightmost.
  const buttons = [...document.querySelectorAll(".reply-acts > button")];
  expect(buttons.map((button) => button.textContent)).toEqual(["Ask again", "Mark read"]);
  expect([...(buttons[0]?.classList ?? [])]).not.toContain("primary");
  expect([...(buttons[1]?.classList ?? [])]).toContain("primary");
  expect(buttons[1]?.querySelector("svg")).toBeTruthy();
});

test("a comment's reply says the owner commented and names no new card", async () => {
  await renderAt(`/inbox?item=${BETA_REPLY}`);
  expect(await screen.findByText("You commented")).toBeTruthy();
  const card = screen.getByTestId("reply-card");
  expect(card.textContent).toContain("beta-8");
  expect(card.textContent).not.toContain("new card");
});

test("Mark read sends one keyed mark and takes the reply off the list", async () => {
  const { router } = await renderAt(`/inbox?item=${ALPHA_REPLY}`);
  const mark = await screen.findByRole("button", { name: "Mark read" });
  fireEvent.click(mark);
  fireEvent.click(mark);
  await flush();
  // The row shown next may read its agent's prompt, which is not a mark.
  const marks = calls.filter((call) => call.path.startsWith("/api/v1/replies/"));
  expect(marks).toHaveLength(1);
  expect(marks[0]).toMatchObject({ method: "POST", path: `/api/v1/replies/${ALPHA_REPLY}/read` });
  expect(Object.keys(marks[0]?.body ?? {})).toEqual(["idempotency_key"]);
  expect(screen.queryByTestId(`inbox-row-${ALPHA_REPLY}`)).toBeNull();
  expect(router.state.location.search).not.toHaveProperty("item");
});

test("Ask again opens the Ask box with the project and the question in it", async () => {
  await renderAt(`/inbox?item=${ALPHA_REPLY}`);
  fireEvent.click(await screen.findByRole("button", { name: "Ask again" }));
  const dialog = await screen.findByRole("dialog");
  const box = await within(dialog).findByRole("textbox");
  await flush();
  // The editor focuses the end of the question, and scrolls to it, a frame
  // later: the focus says that frame ran, so nothing is left to tear down.
  await waitFor(() => expect(document.activeElement).toBe(box));
  expect(box.querySelector(".ask-chip")?.textContent).toBe("alpha");
  expect(box.textContent).toContain("Can the export job retry on its own when storage times out?");
  expect(within(dialog).getByTestId("ask-row-alpha")).toBeTruthy();
  expect(calls).toHaveLength(0);

  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await flush();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(askPrefillStore.get()).toBeNull();
});

test("asking again leaves off the footer of an ask sent to several projects", () => {
  const doc = prefillDoc(
    { project: "alpha", text: "Pin the toolchain\nin CI too\n\nAlso asked of: beta, gamma" },
    "alpha",
  );
  expect(doc.content).toHaveLength(2);
  expect(doc.content?.[0]?.content?.[0]).toMatchObject({
    type: "mention",
    attrs: { id: "alpha", label: "alpha" },
  });
  expect(JSON.stringify(doc)).not.toContain("Also asked of");

  // A project the box does not list yet still keeps the words.
  const bare = prefillDoc({ project: "alpha", text: "Pin the toolchain" }, undefined);
  expect(bare.content?.[0]?.content).toEqual([{ type: "text", text: "Pin the toolchain" }]);
});

test("a reply event refetches every project's replies, and nothing else of the kind", async () => {
  const { queryClient } = await renderAt("/inbox");
  await screen.findByTestId(`inbox-row-${ALPHA_REPLY}`);
  const seen = vi.spyOn(queryClient, "invalidateQueries");
  act(() => {
    applyChange(queryClient, {
      seq: 9001,
      at: "2026-11-02T14:06:00.000Z",
      kind: "ask.replied",
      txn: "ask.reply",
      entity: "reply",
      entity_id: "01JREPLY000000000000000009",
      rev_after: 1,
      card_id: null,
      entity_state: null,
    });
  });
  const predicates = seen.mock.calls.flatMap(([filters]) =>
    filters?.predicate ? [filters.predicate] : [],
  );
  const matches = (path: string) =>
    predicates.some((predicate) =>
      predicate({ queryKey: ["api", path] } as unknown as Parameters<typeof predicate>[0]),
    );
  expect(matches("/api/v1/projects/alpha/replies")).toBe(true);
  expect(matches("/api/v1/projects/alpha/grants")).toBe(false);
});

test("a card's timeline quotes the orchestrator's reply under its line", async () => {
  await renderAt("/cards/01JCARD0000000000000000041");
  const line = await screen.findByText("replied to your comment");
  const entry = line.closest(".it");
  expect(entry?.textContent).toContain("Orchestrator");
  expect(entry?.querySelector(".quote")?.textContent).toContain(
    "I moved alpha-41 back to the queue",
  );
});
