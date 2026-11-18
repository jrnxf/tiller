import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import type { Editor } from "@tiptap/core";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, onTestFinished, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureResource, fixtureSnapshot, serverResponses } from "../../lib/fixtures";
import { resetThreadFixture } from "../../lib/fixtures-thread";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { toastStore } from "../board/toast";
import { createTestQueryClient, loadAskEditor, loadRouteChunks } from "../../test-app";
import { chatDrawerStore } from "./chat-drawer-store";

await loadRouteChunks();
await loadAskEditor();

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
        receipt: { recorded_at: "2026-11-17T18:05:09.000Z", outbox_id: null, approval_id: null },
        data: { marked: [] },
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

const prose = () => document.querySelector(".ProseMirror") as HTMLElement & { editor: Editor };

const callsTo = (suffix: string) => calls.filter((call) => call.path.endsWith(suffix));

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  chatDrawerStore.set(null);
  calls = [];
  onTestFinished(resetThreadFixture);
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
  window.scrollTo = () => {};
  // jsdom ranges have no layout, and the editor measures one to scroll to the caret.
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect();
  stubDaemon();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const flush = () => act(async () => {});

test("opening a chat with unread replies marks them read once, and refetches do not again", async () => {
  const { queryClient } = await renderAt("/chat/alpha");
  await screen.findAllByTestId("chat-reply");
  await waitFor(() => expect(callsTo("/projects/alpha/thread/read")).toHaveLength(1));
  const [read] = callsTo("/projects/alpha/thread/read");
  // This host's thread: the body names no host, which the route refuses.
  expect(Object.keys(read?.body ?? {})).toEqual(["idempotency_key"]);
  expect(read?.method).toBe("POST");
  await act(() => queryClient.invalidateQueries());
  await flush();
  expect(callsTo("/thread/read")).toHaveLength(1);
});

test("the list groups the orchestrators and marks the open chat", async () => {
  await renderAt("/chat/alpha");
  const groups = await screen.findByTestId("chat-groups");
  const labels = [...groups.querySelectorAll(".chat-glabel > span:first-child")].map(
    (label) => label.textContent,
  );
  expect(labels.length).toBeGreaterThan(0);
  for (const label of labels) expect(["Needs you", "Working", "Idle"]).toContain(label);
  const current = within(groups)
    .getAllByRole("link")
    .filter((link) => link.ariaCurrent === "page");
  expect(current).toHaveLength(1);
  expect(current[0]?.textContent).toContain("alpha");
});

test("/chat on a wide screen opens the chat at the top of the list", async () => {
  const { router } = await renderAt("/chat");
  await waitFor(() => expect(router.state.location.pathname).toMatch(/^\/chat\/[^/]+$/));
  const groups = await screen.findByTestId("chat-groups");
  const [top] = within(groups).getAllByRole("link");
  expect(top?.ariaCurrent).toBe("page");
});

test("a merge approval and its attention item are one decision, and Approve binds the head", async () => {
  await renderAt("/chat/alpha");
  const blocks = await screen.findAllByTestId("chat-confirm");
  const merges = blocks.filter((block) => block.textContent?.includes("Merge PR #52"));
  expect(merges).toHaveLength(1);
  const merge = merges[0] as HTMLElement;
  expect(within(merge).getByText("Merge approval")).toBeTruthy();
  fireEvent.click(within(merge).getByRole("button", { name: /^Approve / }));
  await waitFor(() => expect(callsTo("/decide")).toHaveLength(1));
  const [decide] = callsTo("/decide");
  expect(decide?.path).toBe("/api/v1/approvals/01JAPPR0000000000000000000/decide");
  expect(decide?.body).toMatchObject({ decision: "accept" });
  await within(merge).findByTestId("approve-recorded");
  // A closed decision says what the owner chose.
  const decided = screen.getAllByTestId("chat-decided");
  expect(decided.some((block) => block.textContent?.includes("You chose Approve"))).toBe(true);
});

test("a sent note shows its time, a failed one says why, and a card chip says how it is tied", async () => {
  await renderAt("/chat/alpha");
  await screen.findAllByTestId("chat-reply");
  const failed = screen.getAllByTestId("chat-undelivered").map((node) => node.textContent);
  expect(failed).toContain("Not delivered: the orchestrator's pane was gone");
  // A note that arrived says nothing of it.
  expect(screen.queryByText(/^(Read by orchestrator|Delivered|Sending)/)).toBeNull();
  const chips = [...document.querySelectorAll(".chat-msg.user .chat-ref")];
  expect(chips.some((chip) => chip.querySelector(".rel")?.textContent === "Filed")).toBe(true);
});

test("an owner's message keeps its time beside the bubble, not on a line under it", async () => {
  await renderAt("/chat/alpha");
  await screen.findAllByTestId("chat-reply");
  for (const ask of screen.getAllByTestId("chat-ask")) {
    expect(ask.querySelector(".chat-ask-row > time")).not.toBeNull();
    expect(ask.querySelector(".chat-meta time")).toBeNull();
  }
});

test("a chat opened with unread replies marks where they start, and the line stays once read", async () => {
  await renderAt("/chat/alpha");
  const line = await screen.findByTestId("chat-new");
  expect(line.textContent).toBe("New");
  expect(line.nextElementSibling?.getAttribute("data-testid")).toBe("chat-reply");
  await waitFor(() => expect(callsTo("/thread/read")).toHaveLength(1));
  await flush();
  expect(screen.getByTestId("chat-new")).toBe(line);
});

test("a send shows the message at once, and a refused one comes back to the box", async () => {
  await renderAt("/chat/beta");
  await screen.findAllByTestId("chat-reply");
  await waitFor(() => expect(prose()).toBeTruthy());
  vi.stubGlobal("fetch", async (_path: string, init?: RequestInit) => {
    if ((init?.method ?? "GET") === "GET") return new Response("{}", { status: 404 });
    return new Response(
      JSON.stringify({ ok: false, error: { code: "invalid", message: "No room for it." } }),
      { status: 409 },
    );
  });
  act(() => prose().editor.commands.setContent("Ship it"));
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  expect(prose().editor.getText()).toBe("");
  expect(screen.getByTestId("chat-pending").textContent).toBe("Ship it");
  await waitFor(() => expect(screen.queryByTestId("chat-pending")).toBeNull());
  expect(prose().editor.getText()).toBe("Ship it");
  expect(toastStore.get()).toMatchObject({ tone: "bad", title: "Could not send your message" });
});

test("a decision takes the owner's own words under its options, sent with Send", async () => {
  await renderAt("/chat/alpha");
  const blocks = await screen.findAllByTestId("chat-confirm");
  const block = blocks.find((node) => within(node).queryByRole("group", { name: "Answer" }));
  if (!block) throw new Error("no open question");
  const box = within(block).getByRole("textbox", { name: "Your own words" });
  // The words sit under the options.
  const options = within(block).getByRole("group", { name: "Answer" });
  expect(options.compareDocumentPosition(box) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  const send = within(block).getByRole("button", { name: "Send" }) as HTMLButtonElement;
  expect(send.disabled).toBe(true);
  fireEvent.change(box, { target: { value: "Retry once, then page me" } });
  fireEvent.click(send);
  await waitFor(() => expect(callsTo("/answer")).toHaveLength(1));
  expect(callsTo("/answer")[0]?.body).toMatchObject({ answer: "Retry once, then page me" });
});

test("a suggestion fills the box and Send posts it as an ask to the orchestrator", async () => {
  // beta's thread ends on an answer, which the suggestions follow.
  await renderAt("/chat/beta");
  const follow = await screen.findByTestId("chat-follow");
  const suggestion = within(follow).getAllByRole("button")[0] as HTMLElement;
  const words = suggestion.textContent ?? "";
  await waitFor(() => expect(prose()).toBeTruthy());
  fireEvent.click(suggestion);
  await flush();
  // Filling the box sends nothing.
  expect(callsTo("/asks")).toHaveLength(0);
  expect(prose().editor.getText()).toBe(words);
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await waitFor(() => expect(callsTo("/projects/beta/asks")).toHaveLength(1));
  expect(callsTo("/projects/beta/asks")[0]?.body).toMatchObject({ text: words });
  await waitFor(() => expect(prose().editor.getText()).toBe(""));
});

test("Reply to this quotes the reply in the box", async () => {
  await renderAt("/chat/alpha");
  const [reply] = await screen.findAllByTestId("chat-reply");
  await waitFor(() => expect(prose()).toBeTruthy());
  fireEvent.click(within(reply as HTMLElement).getByRole("button", { name: "Reply to this" }));
  await flush();
  const body = reply?.querySelector(".chat-prose")?.textContent ?? "";
  expect(prose().editor.getText()).toContain(`> ${body.split("\n")[0]?.slice(0, 40)}`);
});

test("the queue chip opens the project's cards by state, and Esc closes it", async () => {
  await renderAt("/chat/alpha");
  const chip = await screen.findByTestId("chat-queue-chip");
  fireEvent.click(chip);
  const queue = screen.getByRole("dialog", { name: "Queue" });
  expect(chip.getAttribute("aria-expanded")).toBe("true");
  expect(within(queue).getByText(/running, .* up next, .* done/)).toBeTruthy();
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Queue" })).toBeNull();
});

test("the drawer opens over the board, switches to the page, and Esc closes it", async () => {
  const { router } = await renderAt("/");
  act(() => chatDrawerStore.set({ project: "alpha", owner: undefined }));
  const drawer = await screen.findByRole("complementary", { name: "Chat" });
  await within(drawer).findAllByTestId("chat-reply");
  fireEvent.keyDown(window, { key: "Escape" });
  expect(screen.queryByRole("complementary", { name: "Chat" })).toBeNull();

  act(() => chatDrawerStore.set({ project: "alpha", owner: undefined }));
  const again = await screen.findByRole("complementary", { name: "Chat" });
  fireEvent.click(within(again).getByRole("button", { name: "Page" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/chat/alpha"));
  expect(chatDrawerStore.get()).toBeNull();
});
