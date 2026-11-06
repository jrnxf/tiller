import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { MemoryResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import { resetMemoryFixture } from "../../lib/fixtures-memory";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";

const NOW = Date.parse("2026-11-02T12:00:00Z");

interface Call {
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];

// The stand-in daemon: reads and writes go to the same stateful fixture the
// dev server uses, so a landed change shows on the next read.
function stubDaemon() {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      const data = fixtureResource(path, "default", NOW);
      if (data === undefined) return new Response("{}", { status: 404 });
      return new Response(JSON.stringify(data), { status: 200 });
    }
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    calls.push({ path, body });
    const { reply } = fixtureMutation(method, path, body);
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
}

const view = (scope?: string) =>
  fixtureResource(scope ? `/api/v1/memory?scope=${scope}` : "/api/v1/memory") as MemoryResponse;

async function renderMemory(path = "/memory") {
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default", NOW));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { router, queryClient };
}

const flush = () => act(async () => {});

// The document itself; the rail repeats changed lines.
const page = () => within(screen.getByRole("article"));

const settle = async () => {
  for (let i = 0; i < 4; i += 1) await flush();
};

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  resetMemoryFixture(NOW);
  calls = [];
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    // xterm still listens the old way, for the device pixel ratio.
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

const rewrite = () => {
  const row = document.querySelector<HTMLElement>(".de.sug.edit");
  if (!row) throw new Error("no rewrite suggestion on the page");
  return row;
};

test("suggestions show inline as tinted text, a rewrite as a word diff, until kept or discarded", async () => {
  await renderMemory();

  expect(screen.getByRole("heading", { level: 1, name: "All projects (shared)" })).toBeTruthy();
  expect(screen.getByText(/2 suggestions from agents/)).toBeTruthy();
  const row = rewrite();
  expect([...row.querySelectorAll("del")].map((el) => el.textContent)).toEqual([
    "worker dispatch when",
    "weekly quota reaches 3%.",
  ]);
  expect([...row.querySelectorAll("ins")].map((el) => el.textContent)).toEqual([
    "starting",
    "small-model workers at 3% of",
    "week left, and new large-model workers at 8%.",
  ]);
  expect(within(row).getByText(/Suggested by the curator/).textContent).toMatch(
    /Startup prompt after: orchestrator 5,940, worker 4,210 tokens\./,
  );
  const add = document.querySelector(".de.sug.add .de-t.sg");
  expect(add?.textContent).toMatch(/^If a project's pipeline cannot run its checks off screen/);
  // Nothing is written by showing them.
  expect(calls).toEqual([]);
});

test("Keep sends one decision against the blob shown and the line takes the new text", async () => {
  const blob = view().doc.blob;
  await renderMemory();

  const bar = within(rewrite()).getByRole("group", { name: "Suggestion from the curator" });
  fireEvent.click(within(bar).getByRole("button", { name: "Keep" }));
  await settle();

  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toBe("/api/v1/memory/proposals/decide");
  expect(calls[0]?.body.decisions).toEqual([
    { proposal: expect.any(String), decision: "keep", base_blob: blob },
  ]);
  expect(document.querySelector(".de.sug.edit")).toBeNull();
  expect(page().getByText(/^Stop starting new small-model workers at 3%/)).toBeTruthy();
  const rail = screen.getByRole("complementary", { name: "Recent changes" });
  expect(within(rail).getByText("Edited a line")).toBeTruthy();
  expect(screen.getByText(/1 suggestion from agents/)).toBeTruthy();
});

test("Discard drops the suggestion and leaves the line as it was", async () => {
  const blob = view().doc.blob;
  await renderMemory();

  const bar = within(rewrite()).getByRole("group", { name: "Suggestion from the curator" });
  fireEvent.click(within(bar).getByRole("button", { name: "Discard" }));
  await settle();

  expect(calls[0]?.body.decisions).toEqual([
    { proposal: expect.any(String), decision: "discard", base_blob: blob },
  ]);
  expect(
    page().getByText("Stop new worker dispatch when the weekly quota reaches 3%."),
  ).toBeTruthy();
  // A discard records the decision only; the file is not written.
  expect(view().doc.blob).toBe(blob);
  const rail = screen.getByRole("complementary", { name: "Recent changes" });
  expect(within(rail).getAllByText("Discarded a suggestion")).toHaveLength(2);
});

test("editing a line in place sends a rewrite of that line; Esc puts the text back", async () => {
  const blob = view().doc.blob;
  await renderMemory();

  const [first] = screen.getAllByRole("textbox", { name: "Edit this line" });
  if (!first) throw new Error("no editable line");
  first.focus();
  first.textContent = "Escape me";
  fireEvent.keyDown(first, { key: "Escape" });
  await settle();
  expect(calls).toEqual([]);
  expect(first.textContent).toMatch(/^Before asking me to merge a sizeable PR/);

  first.textContent = "  Offer an independent review   before a sizeable merge. ";
  fireEvent.blur(first);
  await settle();

  expect(calls).toEqual([
    {
      path: "/api/v1/memory/edits",
      body: {
        idempotency_key: expect.any(String),
        project: "fleet",
        section: "How we work here",
        base_blob: blob,
        op: "rewrite",
        index: 0,
        text: "Offer an independent review before a sizeable merge.",
      },
    },
  ]);
  expect(page().getByText("Offer an independent review before a sizeable merge.")).toBeTruthy();
});

test("a write against a text that moved on is refused, shown, and the current text is read again", async () => {
  await renderMemory();

  // Someone else lands an edit after the page was read.
  const moved = view().doc;
  fixtureMutation("POST", "/api/v1/memory/edits", {
    idempotency_key: "elsewhere",
    project: "fleet",
    section: "Preferences",
    op: "add",
    text: "Added from another tab.",
    base_blob: moved.blob,
  });

  const bar = within(rewrite()).getByRole("group", { name: "Suggestion from the curator" });
  fireEvent.click(within(bar).getByRole("button", { name: "Keep" }));
  await settle();

  expect(screen.getByRole("alert").textContent).toBe(
    "fleet.md changed since you opened it; here is the current text",
  );
  expect(page().getByText("Added from another tab.")).toBeTruthy();
  // The suggestion is still waiting: the refused Keep changed nothing.
  expect(document.querySelector(".de.sug.edit")).toBeTruthy();
});

test("a document with uncommitted changes cannot be edited from the page", async () => {
  await renderMemory("/memory?doc=gamma");

  expect(screen.getByRole("heading", { level: 1, name: "gamma" })).toBeTruthy();
  expect(screen.getByRole("status").textContent).toMatch(
    /projects\/gamma\.md has changes that are not committed/,
  );
  const [line] = screen.getAllByRole("textbox", { name: "Edit this line" });
  expect(line?.getAttribute("contenteditable")).toBeNull();
  for (const button of screen.getAllByRole("button", { name: "Add a line" })) {
    expect((button as HTMLButtonElement).disabled).toBe(true);
  }
});

test("Undo reverts the newest change; an older one that overlaps is refused with the reason", async () => {
  await renderMemory();

  const bar = within(rewrite()).getByRole("group", { name: "Suggestion from the curator" });
  fireEvent.click(within(bar).getByRole("button", { name: "Keep" }));
  await settle();

  const rail = screen.getByRole("complementary", { name: "Recent changes" });
  const [newest, older] = within(rail).getAllByRole("button", { name: "Undo" });
  if (!newest || !older) throw new Error("expected two changes with Undo");
  const kept = view().doc.blob;
  fireEvent.click(newest);
  await settle();

  expect(calls[1]?.path).toMatch(/^\/api\/v1\/memory\/changes\/[^/]+\/undo$/);
  expect(calls[1]?.body.base_blob).toBe(kept);
  expect(
    page().getByText("Stop new worker dispatch when the weekly quota reaches 3%."),
  ).toBeTruthy();
  expect(within(rail).queryByText("Edited a line")).toBeNull();

  const [seeded] = within(rail).getAllByRole("button", { name: "Undo" });
  if (!seeded) throw new Error("expected the seeded change");
  fireEvent.click(seeded);
  await settle();
  expect(screen.getByRole("alert").textContent).toBe(
    "undoing it conflicts with a later edit to the same lines; change the line by hand",
  );
});

test("a line's source opens its revision: provenance, approval and the sessions that loaded it", async () => {
  await renderMemory("/memory?doc=alpha");

  fireEvent.click(screen.getAllByRole("button", { name: /^alpha-38, / })[0] as HTMLElement);
  await settle();

  const rail = screen.getByRole("complementary", { name: "Source of this line" });
  expect(within(rail).getByRole("heading", { name: "projects/alpha.md" })).toBeTruthy();
  expect(within(rail).getByText("Carried over")).toBeTruthy();
  expect(within(rail).getByText("Loaded by")).toBeTruthy();
  expect(within(rail).getByText(/^## How we work here/)).toBeTruthy();

  fireEvent.click(within(rail).getByRole("button", { name: "Recent changes" }));
  await settle();
  expect(screen.getByRole("complementary", { name: "Recent changes" })).toBeTruthy();
});

test("an entry written over several lines sends no write when it is focused and left as it was", async () => {
  // The file wraps the first entry; the page shows it as one line.
  const served = globalThis.fetch;
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const reply = await served(path, init);
    if (init?.method || !path.startsWith("/api/v1/memory")) return reply;
    const data = (await reply.json()) as MemoryResponse;
    const line = data.doc?.sections[0]?.lines[0];
    if (line) line.text = line.text.replace(" a sizeable", "\n  a sizeable");
    return new Response(JSON.stringify(data), { status: reply.status });
  });
  await renderMemory();

  const [first] = screen.getAllByRole("textbox", { name: "Edit this line" });
  if (!first) throw new Error("no editable line");
  expect(first.textContent).toContain("\n  a sizeable");
  first.focus();
  first.textContent = (first.textContent ?? "").replace("\n  ", " ");
  fireEvent.blur(first);
  await settle();
  expect(calls).toEqual([]);
});

test("a new line that is refused stays open with its text", async () => {
  await renderMemory();

  const [addLine] = screen.getAllByRole("button", { name: "Add a line" });
  if (!addLine) throw new Error("no Add a line button");
  fireEvent.click(addLine);
  await settle();
  const fresh = screen.getByRole("textbox", { name: "New line" });
  fresh.textContent = "Keep this wording";

  // Someone else lands an edit first, so the add is refused as stale.
  fixtureMutation("POST", "/api/v1/memory/edits", {
    idempotency_key: "elsewhere-add",
    project: "fleet",
    section: "Preferences",
    op: "add",
    text: "Added from another tab.",
    base_blob: view().doc.blob,
  });
  fireEvent.blur(fresh);
  await settle();

  expect(calls.map((c) => c.path)).toEqual(["/api/v1/memory/edits"]);
  expect(screen.getByRole("alert").textContent).toMatch(/changed since you opened it/);
  expect(screen.getByRole("textbox", { name: "New line" }).textContent).toBe("Keep this wording");
});

test("Keep waits for the holdout gate, with the reason and the command beside it", async () => {
  await renderMemory();

  const row = document.querySelector<HTMLElement>(".de.sug.add");
  if (!row) throw new Error("no add suggestion on the page");
  const keep = within(row).getByRole("button", { name: "Keep" }) as HTMLButtonElement;
  expect(keep.disabled).toBe(true);
  const note = document.getElementById(keep.getAttribute("aria-describedby") ?? "");
  expect(note?.textContent).toBe(
    "Keep waits for the holdout gate: change set off-screen checks has no holdout run. Next: tiller learn eval off-screen checks",
  );
  // Discard is not gated.
  const discard = within(row).getByRole("button", { name: "Discard" }) as HTMLButtonElement;
  expect(discard.disabled).toBe(false);
  // A suggestion that passes has no note.
  expect(within(rewrite()).queryByText(/Keep waits/)).toBeNull();
});
