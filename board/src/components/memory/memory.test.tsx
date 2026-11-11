import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { MemoryResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
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
// The reads the page made.
let reads: string[] = [];

// The stand-in daemon: reads and writes go to the same stateful fixture the
// dev server uses, so a landed change shows on the next read. `scenario`
// sets the quota a holdout eval is planned against.
function stubDaemon(scenario: Scenario = "default") {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      reads.push(path);
      const data = fixtureResource(path, scenario, NOW);
      if (data === undefined) return new Response("{}", { status: 404 });
      return new Response(JSON.stringify(data), { status: 200 });
    }
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    calls.push({ path, body });
    const { reply } = fixtureMutation(method, path, body, scenario);
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

// The document itself.
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
  reads = [];
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

const card = (op: "add" | "rewrite" | "retire") => {
  const found = document.querySelector<HTMLElement>(`.a-sug[data-op="${op}"]`);
  if (!found) throw new Error(`no ${op} suggestion on the page`);
  return found;
};
const rewrite = () => card("rewrite");

// The line row that shows `text`.
const lineOf = (text: string | RegExp) => {
  const row = page().getByText(text, { selector: ".a-ln .tx" }).closest<HTMLElement>(".a-ln");
  if (!row) throw new Error("no line row");
  return row;
};

// Remove asks first; this answers the dialog that opened.
const removeDialog = () => screen.getByRole("dialog", { name: "Remove this line?" });
const confirmRemove = () =>
  fireEvent.click(within(removeDialog()).getByRole("button", { name: "Remove line" }));

const type = (box: HTMLElement, value: string) => fireEvent.change(box, { target: { value } });

test("each document is a tab, with the suggestions waiting on it or its line count", async () => {
  await renderMemory();

  const tabs = within(screen.getByRole("navigation", { name: "Memory documents" }));
  const fleet = tabs.getByRole("link", { name: /^host \/ host-a/ });
  expect(fleet.getAttribute("aria-current")).toBe("page");
  expect(fleet.textContent).toBe("host / host-a2");
  expect(tabs.getByRole("link", { name: /^alpha/ }).getAttribute("aria-current")).toBeNull();
});

test("only the open document's tab is current, not the shared one too", async () => {
  await renderMemory("/memory?doc=alpha");

  const tabs = within(screen.getByRole("navigation", { name: "Memory documents" }));
  const current = tabs.getAllByRole("link").filter((tab) => tab.getAttribute("aria-current"));
  expect(current.map((tab) => tab.textContent)).toEqual(["alpha1"]);
});

test("each line shows who wrote it and the day in their own columns, as plain text", async () => {
  await renderMemory("/memory?doc=alpha");

  const row = lineOf(/^The remote hook must read stdin/);
  expect(row.querySelector(":scope > .who")?.textContent).toBe("alpha-38");
  expect(row.querySelector(":scope > .day")?.textContent).toMatch(
    /^[A-Z][a-z]{2} \d{1,2} [A-Z][a-z]{2}$/,
  );
  // The source is in the editor, not a chip on the line.
  expect(within(row).queryByRole("button", { name: /alpha-38/ })).toBeNull();
  expect(
    within(row)
      .getAllByRole("button")
      .map((button) => button.getAttribute("aria-label")),
  ).toEqual(["Edit this line", "Remove this line"]);
});

test("suggestions show inline as tinted text, a rewrite as a word diff, until kept or discarded", async () => {
  await renderMemory();

  expect(screen.getByRole("heading", { level: 1, name: "host / host-a" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "2 suggestions to review" })).toBeTruthy();
  const row = rewrite();
  // A rewrite sits right under the line it changes.
  expect(row.previousElementSibling?.textContent).toMatch(/^Stop new worker dispatch/);
  expect(within(row).getByText("of the line above")).toBeTruthy();
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
  const add = card("add");
  expect(within(add).getByText("for Watch out for")).toBeTruthy();
  expect(add.querySelector("ins")?.textContent).toMatch(
    /^If a project's pipeline cannot run its checks off screen/,
  );
  // Nothing is written by showing them.
  expect(calls).toEqual([]);
});

test("Keep sends one decision against the blob shown and the line takes the new text", async () => {
  const blob = view().doc.blob;
  await renderMemory();

  const group = rewrite();
  expect(group.getAttribute("role")).toBe("group");
  expect(group.getAttribute("aria-label")).toBe("Suggestion from the curator");
  fireEvent.click(within(group).getByRole("button", { name: "Keep" }));
  await settle();

  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toBe("/api/v1/memory/proposals/decide");
  expect(calls[0]?.body.decisions).toEqual([
    { proposal: expect.any(String), decision: "keep", base_blob: blob },
  ]);
  expect(document.querySelector('.a-sug[data-op="rewrite"]')).toBeNull();
  expect(page().getByText(/^Stop starting new small-model workers at 3%/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "1 suggestion to review" })).toBeTruthy();
  expect(screen.getByTestId("toast").textContent).toBe("Kept.");
});

test("Discard drops the suggestion and leaves the line as it was", async () => {
  const blob = view().doc.blob;
  await renderMemory();

  fireEvent.click(within(rewrite()).getByRole("button", { name: "Discard" }));
  await settle();

  expect(calls[0]?.body.decisions).toEqual([
    { proposal: expect.any(String), decision: "discard", base_blob: blob },
  ]);
  expect(
    page().getByText("Stop new worker dispatch when the weekly quota reaches 3%."),
  ).toBeTruthy();
  // A discard records the decision only; the file is not written.
  expect(view().doc.blob).toBe(blob);
});

test("a click on a line opens its editor in place; Esc closes it, Enter sends a rewrite", async () => {
  const blob = view().doc.blob;
  await renderMemory();

  const first = /^Before asking me to merge a sizeable PR/;
  fireEvent.click(lineOf(first));
  let box = screen.getByRole("textbox", { name: "Edit this line" }) as HTMLTextAreaElement;
  expect(box.value).toMatch(first);
  expect(document.activeElement).toBe(box);
  // The line it edits steps aside for the editor.
  expect(lineOf(first).hidden).toBe(true);
  type(box, "Escape me");
  fireEvent.keyDown(box, { key: "Escape" });
  await settle();
  expect(calls).toEqual([]);
  expect(screen.queryByRole("textbox", { name: "Edit this line" })).toBeNull();
  expect(lineOf(first).hidden).toBe(false);

  fireEvent.click(within(lineOf(first)).getByRole("button", { name: "Edit this line" }));
  box = screen.getByRole("textbox", { name: "Edit this line" }) as HTMLTextAreaElement;
  type(box, "  Offer an independent review   before a sizeable merge. ");
  fireEvent.keyDown(box, { key: "Enter" });
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
  expect(screen.queryByRole("textbox", { name: "Edit this line" })).toBeNull();
});

test("an Enter that picks an input method's candidate does not save", async () => {
  await renderMemory();

  fireEvent.click(lineOf(/^Before asking me to merge a sizeable PR/));
  const box = screen.getByRole("textbox", { name: "Edit this line" });
  type(box, "Offer a review first.");
  fireEvent.keyDown(box, { key: "Enter", isComposing: true });
  await settle();
  expect(calls).toEqual([]);
  expect(screen.getByRole("textbox", { name: "Edit this line" })).toBe(box);
});

test("a line removed above an open editor leaves the editor open with its draft", async () => {
  await renderMemory("/memory?doc=alpha");

  const logo = /^Logo explorations run as five parallel scouts/;
  fireEvent.click(lineOf(logo));
  type(screen.getByRole("textbox", { name: "Edit this line" }), "Logo scouts run in parallel.");
  const hook = lineOf(/^The remote hook must read stdin/);
  fireEvent.click(within(hook).getByRole("button", { name: "Remove this line" }));
  confirmRemove();
  await settle();

  expect(calls[0]?.body).toMatchObject({ op: "remove", index: 0, section: "How we work here" });
  expect(page().queryByText(/^The remote hook must read stdin/)).toBeNull();
  const box = screen.getByRole("textbox", { name: "Edit this line" }) as HTMLTextAreaElement;
  expect(box.value).toBe("Logo scouts run in parallel.");
  // The line moved up, and the rewrite goes to where it is now.
  fireEvent.keyDown(box, { key: "Enter" });
  await settle();
  expect(calls[1]?.body).toMatchObject({
    op: "rewrite",
    index: 0,
    text: "Logo scouts run in parallel.",
  });
  expect(screen.queryByRole("textbox", { name: "Edit this line" })).toBeNull();
  expect(page().getByText("Logo scouts run in parallel.")).toBeTruthy();
});

test("Remove in the editor removes that line", async () => {
  await renderMemory();

  fireEvent.click(lineOf(/^Before asking me to merge a sizeable PR/));
  fireEvent.click(screen.getByRole("button", { name: "Remove" }));
  confirmRemove();
  await settle();

  expect(calls[0]?.body).toMatchObject({ op: "remove", index: 0, section: "How we work here" });
  expect(page().queryByText(/^Before asking me to merge a sizeable PR/)).toBeNull();
  expect(screen.getByTestId("toast").textContent).toBe("Removed.");
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

  fireEvent.click(within(rewrite()).getByRole("button", { name: "Keep" }));
  await settle();

  // It shows in the card that was pressed, and is announced there.
  const refusal = within(rewrite()).getByRole("alert");
  expect(refusal.textContent).toBe(
    "fleet.md changed since you opened it; here is the current text",
  );
  expect(screen.getAllByRole("alert")).toEqual([refusal]);
  expect(page().getByText("Added from another tab.")).toBeTruthy();
  // The suggestion is still waiting: the refused Keep changed nothing.
  expect(document.querySelector('.a-sug[data-op="rewrite"]')).toBeTruthy();
});

test("a document with uncommitted changes cannot be edited from the page", async () => {
  await renderMemory("/memory?doc=gamma");

  expect(screen.getByRole("heading", { level: 1, name: "gamma" })).toBeTruthy();
  expect(screen.getByRole("status").textContent).toMatch(
    /projects\/gamma\.md has changes that are not committed/,
  );
  const [line] = page().getAllByRole("button", { name: "Edit this line" });
  if (!line) throw new Error("no line");
  expect((line as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(line.closest(".a-ln") as HTMLElement);
  expect(screen.queryByRole("textbox")).toBeNull();
  for (const button of screen.getAllByRole("button", { name: "Add a line" })) {
    expect((button as HTMLButtonElement).disabled).toBe(true);
  }
});

test("the page has no Recent changes rail: the history is at the foot of the document", async () => {
  await renderMemory();

  expect(screen.queryByRole("complementary", { name: "Recent changes" })).toBeNull();
  const history = screen.getByRole("region", { name: "Change history" });
  // After the document's last section, not beside it.
  const article = screen.getByRole("article");
  expect(article.compareDocumentPosition(history) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(within(history).getByText("host / host-a · last 30 days")).toBeTruthy();
});

test("the Source link in a line's editor opens its revision over the page", async () => {
  await renderMemory("/memory?doc=alpha");

  fireEvent.click(lineOf(/^The remote hook must read stdin/));
  fireEvent.click(screen.getByRole("button", { name: "Source" }));
  await settle();

  const dialog = within(screen.getByRole("dialog", { name: "Source of this line" }));
  expect(dialog.getByRole("heading", { name: "projects/alpha.md" })).toBeTruthy();
  expect(dialog.getByText("Carried over")).toBeTruthy();
  expect(dialog.getByText("Loaded by")).toBeTruthy();
  expect(dialog.getByText(/^## How we work here/)).toBeTruthy();

  fireEvent.click(dialog.getByRole("button", { name: "Close" }));
  await settle();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.getByRole("textbox", { name: "Edit this line" })).toBeTruthy();
});

test("an entry written over several lines sends no write when it is saved as it was", async () => {
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

  fireEvent.click(lineOf(/^Before asking me to merge/));
  const box = screen.getByRole("textbox", { name: "Edit this line" }) as HTMLTextAreaElement;
  expect(box.value).toContain("\n  a sizeable");
  type(box, box.value.replace("\n  ", " "));
  fireEvent.keyDown(box, { key: "Enter" });
  await settle();
  expect(calls).toEqual([]);
  expect(screen.queryByRole("textbox", { name: "Edit this line" })).toBeNull();
});

test("Add a line opens the editor at the end of its section and sends an add", async () => {
  await renderMemory();

  const section = within(screen.getByRole("region", { name: "Watch out for" }));
  fireEvent.click(section.getByRole("button", { name: "Add a line" }));
  expect(section.queryByRole("button", { name: "Add a line" })).toBeNull();
  expect(section.getByText("Adds to Watch out for as")).toBeTruthy();
  const box = section.getByRole("textbox", { name: "New line" });
  type(box, "Ask me before you install anything outside the dev shell");
  fireEvent.click(section.getByRole("button", { name: "Add line" }));
  await settle();

  expect(calls[0]?.body).toMatchObject({
    op: "add",
    section: "Watch out for",
    text: "Ask me before you install anything outside the dev shell",
  });
  expect(
    section.getByText("Ask me before you install anything outside the dev shell"),
  ).toBeTruthy();
  expect(section.getByRole("button", { name: "Add a line" })).toBeTruthy();
});

test("a new line that is refused stays open with its text", async () => {
  await renderMemory();

  const [addLine] = screen.getAllByRole("button", { name: "Add a line" });
  if (!addLine) throw new Error("no Add a line button");
  fireEvent.click(addLine);
  const fresh = screen.getByRole("textbox", { name: "New line" });
  type(fresh, "Keep this wording");

  // Someone else lands an edit first, so the add is refused as stale.
  fixtureMutation("POST", "/api/v1/memory/edits", {
    idempotency_key: "elsewhere-add",
    project: "fleet",
    section: "Preferences",
    op: "add",
    text: "Added from another tab.",
    base_blob: view().doc.blob,
  });
  fireEvent.keyDown(fresh, { key: "Enter" });
  await settle();

  expect(calls.map((c) => c.path)).toEqual(["/api/v1/memory/edits"]);
  expect(screen.getByRole("alert").textContent).toMatch(/changed since you opened it/);
  expect((screen.getByRole("textbox", { name: "New line" }) as HTMLTextAreaElement).value).toBe(
    "Keep this wording",
  );
});

test("Keep waits for the holdout gate, with the reason, and the board offers the eval in place of the command", async () => {
  await renderMemory();

  const row = card("add");
  const keep = within(row).getByRole("button", { name: "Keep" }) as HTMLButtonElement;
  expect(keep.disabled).toBe(true);
  const note = document.getElementById(keep.getAttribute("aria-describedby") ?? "");
  expect(note?.textContent).toBe(
    "Keep waits for the holdout gate: change set off-screen checks has no holdout run.",
  );
  expect(within(row).getByRole("button", { name: "Run holdout eval..." })).toBeTruthy();
  // Discard is not gated.
  const discard = within(row).getByRole("button", { name: "Discard" }) as HTMLButtonElement;
  expect(discard.disabled).toBe(false);
  // A suggestion that passes has no note.
  expect(within(rewrite()).queryByText(/Keep waits/)).toBeNull();
});

const editButton = (row: HTMLElement) =>
  within(row).getByRole("button", { name: "Edit this line" });

// Someone else lands a change after the page read the document.
// The fixture remembers a key across tests, so each change has its own.
let elsewhereCount = 0;
const elsewhere = (body: Record<string, unknown>) => {
  elsewhereCount += 1;
  const result = fixtureMutation("POST", "/api/v1/memory/edits", {
    idempotency_key: `elsewhere-change-${elsewhereCount}`,
    project: "fleet",
    base_blob: view().doc.blob,
    ...body,
  });
  return result;
};

const rewriteElsewhere = (index: number, text: string) =>
  elsewhere({ section: "How we work here", op: "rewrite", index, text });

// The page reads the document again, and the cache has told the page by the
// time this returns (it notifies on a timer, not at once).
const readAgain = (queryClient: { invalidateQueries: () => Promise<void> }) =>
  act(async () => {
    await queryClient.invalidateQueries();
    await new Promise((done) => setTimeout(done, 20));
  });

const FIRST = /^Before asking me to merge a sizeable PR/;
const SECOND = /^Stop new worker dispatch/;
const DRAFT = "My unsaved wording";

const draftBox = () =>
  screen.getByRole("textbox", { name: "Edit this line" }) as HTMLTextAreaElement;

// U13: a draft is not lost without a prompt.

test("opening another line while a draft is changed asks first, and Keep editing keeps the draft", async () => {
  await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), DRAFT);
  fireEvent.click(lineOf(SECOND));
  await settle();

  const dialog = within(screen.getByRole("dialog", { name: "Discard your changes?" }));
  // The safe choice is first and has focus; the destructive one confirms.
  expect(dialog.getAllByRole("button").map((button) => button.textContent)).toEqual([
    "Keep editing",
    "Discard changes",
  ]);
  expect(document.activeElement).toBe(dialog.getByRole("button", { name: "Keep editing" }));
  fireEvent.click(dialog.getByRole("button", { name: "Keep editing" }));
  await settle();

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(draftBox().value).toBe(DRAFT);
  expect(calls).toEqual([]);
});

test("Discard changes drops the draft and opens the line that was asked for", async () => {
  await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), DRAFT);
  fireEvent.click(lineOf(SECOND));
  fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
  await settle();

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(draftBox().value).toMatch(SECOND);
  expect(lineOf(FIRST).hidden).toBe(false);
  expect(calls).toEqual([]);
});

test("Add a line asks before it drops a changed draft", async () => {
  await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), DRAFT);
  const section = within(screen.getByRole("region", { name: "Watch out for" }));
  fireEvent.click(section.getByRole("button", { name: "Add a line" }));
  await settle();

  expect(screen.getByRole("dialog", { name: "Discard your changes?" })).toBeTruthy();
  expect(draftBox().value).toBe(DRAFT);
  fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
  await settle();
  expect(section.getByRole("textbox", { name: "New line" })).toBeTruthy();
});

test("an unchanged draft closes with no prompt", async () => {
  await renderMemory();

  fireEvent.click(lineOf(FIRST));
  fireEvent.click(lineOf(SECOND));
  await settle();

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(draftBox().value).toMatch(SECOND);
});

test("going to another document with a changed draft asks first", async () => {
  const { router } = await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), DRAFT);
  const tabs = within(screen.getByRole("navigation", { name: "Memory documents" }));
  fireEvent.click(tabs.getByRole("link", { name: /^alpha/ }));
  await settle();

  expect(screen.getByRole("dialog", { name: "Discard your changes?" })).toBeTruthy();
  expect(router.state.location.search).toEqual({});
  fireEvent.click(screen.getByRole("button", { name: "Keep editing" }));
  await settle();
  expect(router.state.location.search).toEqual({});
  expect(draftBox().value).toBe(DRAFT);

  fireEvent.click(tabs.getByRole("link", { name: /^alpha/ }));
  await settle();
  fireEvent.click(screen.getByRole("button", { name: "Discard changes" }));
  await waitFor(() => expect(router.state.location.search).toEqual({ doc: "alpha" }));
  expect(await screen.findByRole("heading", { level: 1, name: "alpha" })).toBeTruthy();
});

test("a line changed elsewhere keeps the draft and offers Save over or Discard", async () => {
  const { queryClient } = await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), DRAFT);
  rewriteElsewhere(0, "Rewritten from another tab.");
  await readAgain(queryClient);
  await settle();

  // The editor stays, with the draft, and says what happened.
  const box = draftBox();
  expect(box.value).toBe(DRAFT);
  const notice = screen.getByRole("alert");
  expect(notice.textContent).toMatch(/This line changed while you were editing/);
  expect(notice.textContent).toMatch(/Rewritten from another tab\./);
  // Enter does not save over someone's change.
  fireEvent.keyDown(box, { key: "Enter" });
  await settle();
  expect(calls).toEqual([]);

  fireEvent.click(screen.getByRole("button", { name: "Save over" }));
  await settle();
  expect(calls).toHaveLength(1);
  expect(calls[0]?.body).toMatchObject({
    op: "rewrite",
    index: 0,
    section: "How we work here",
    text: DRAFT,
  });
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(page().getByText(DRAFT)).toBeTruthy();
});

test("Discard on a line changed elsewhere keeps the other text", async () => {
  const { queryClient } = await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), DRAFT);
  rewriteElsewhere(0, "Rewritten from another tab.");
  await readAgain(queryClient);
  await settle();
  const editor = within(draftBox().closest(".ed") as HTMLElement);
  expect(editor.queryByRole("button", { name: "Remove" })).toBeNull();
  fireEvent.click(editor.getByRole("button", { name: "Discard" }));
  await settle();

  expect(screen.queryByRole("textbox")).toBeNull();
  expect(page().getByText("Rewritten from another tab.")).toBeTruthy();
  expect(calls).toEqual([]);
});

test("a line removed elsewhere keeps the draft and offers it as a new line", async () => {
  const { queryClient } = await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), DRAFT);
  elsewhere({ section: "How we work here", op: "remove", index: 0 });
  await readAgain(queryClient);
  await settle();

  expect(screen.getByRole("alert").textContent).toMatch(
    /This line was removed while you were editing/,
  );
  const box = screen.getByRole("textbox", { name: "New line" }) as HTMLTextAreaElement;
  expect(box.value).toBe(DRAFT);
  fireEvent.click(screen.getByRole("button", { name: "Add as new line" }));
  await settle();
  expect(calls[0]?.body).toMatchObject({ op: "add", section: "How we work here", text: DRAFT });
});

test("an unchanged draft follows a line changed elsewhere, with no notice", async () => {
  const { queryClient } = await renderMemory();

  fireEvent.click(lineOf(FIRST));
  rewriteElsewhere(0, "Rewritten from another tab.");
  await readAgain(queryClient);
  await settle();

  expect(screen.queryByRole("alert")).toBeNull();
  expect(draftBox().value).toBe("Rewritten from another tab.");
});

// U16: focus.

test("after Save, focus is on the saved row's edit control", async () => {
  await renderMemory();

  fireEvent.click(lineOf(SECOND));
  type(draftBox(), "Stop new dispatch at 3% of the week.");
  fireEvent.keyDown(draftBox(), { key: "Enter" });
  await settle();

  expect(document.activeElement).toBe(editButton(lineOf("Stop new dispatch at 3% of the week.")));
});

test("after Remove in the editor, focus is on the next row", async () => {
  await renderMemory();

  fireEvent.click(lineOf(FIRST));
  fireEvent.click(screen.getByRole("button", { name: "Remove" }));
  confirmRemove();
  await settle();

  // The second line moved up into the removed line's place.
  expect(document.activeElement).toBe(editButton(lineOf(SECOND)));
});

test("after Remove, focus is on the next row, else the previous one, else Add a line", async () => {
  await renderMemory();
  const section = within(screen.getByRole("region", { name: "How we work here" }));

  // The first of two: the next row.
  fireEvent.click(within(lineOf(FIRST)).getByRole("button", { name: "Remove this line" }));
  confirmRemove();
  await settle();
  expect(document.activeElement).toBe(editButton(lineOf(SECOND)));

  // The only row: Add a line.
  fireEvent.click(section.getByRole("button", { name: "Remove this line" }));
  confirmRemove();
  await settle();
  expect(document.activeElement).toBe(section.getByRole("button", { name: "Add a line" }));
});

test("after Remove of the last row, focus is on the previous row", async () => {
  await renderMemory();

  fireEvent.click(within(lineOf(SECOND)).getByRole("button", { name: "Remove this line" }));
  confirmRemove();
  await settle();

  expect(document.activeElement).toBe(editButton(lineOf(FIRST)));
});

// Remove asks first.

test("Remove on a row asks first and sends nothing until it is confirmed", async () => {
  await renderMemory();

  const remove = within(lineOf(FIRST)).getByRole("button", { name: "Remove this line" });
  fireEvent.click(remove);

  const dialog = within(removeDialog());
  expect(dialog.getByText(/^Before asking me to merge a sizeable PR/)).toBeTruthy();
  expect(calls).toEqual([]);
  expect(page().getByText(FIRST)).toBeTruthy();

  fireEvent.click(dialog.getByRole("button", { name: "Remove line" }));
  await settle();
  expect(calls[0]?.body).toMatchObject({ op: "remove", index: 0, section: "How we work here" });
  expect(screen.queryByRole("dialog", { name: "Remove this line?" })).toBeNull();
  expect(page().queryByText(FIRST)).toBeNull();
});

test("Remove in the editor asks first too", async () => {
  await renderMemory();

  fireEvent.click(lineOf(FIRST));
  fireEvent.click(screen.getByRole("button", { name: "Remove" }));

  expect(removeDialog()).toBeTruthy();
  expect(calls).toEqual([]);
  fireEvent.click(within(removeDialog()).getByRole("button", { name: "Remove line" }));
  await settle();
  expect(calls[0]?.body).toMatchObject({ op: "remove", index: 0 });
});

test("the dialog has Cancel on the left and the destructive Remove rightmost, neither a ghost", async () => {
  await renderMemory();

  fireEvent.click(within(lineOf(FIRST)).getByRole("button", { name: "Remove this line" }));

  const buttons = within(removeDialog()).getAllByRole("button");
  expect(buttons.map((button) => button.textContent)).toEqual(["Cancel", "Remove line"]);
  expect(buttons[1]?.className).toMatch(/\bdanger\b/);
  for (const button of buttons) expect(button.className).not.toMatch(/ghost|primary/);
  // The safe choice has focus.
  expect(document.activeElement).toBe(buttons[0]);
});

test("Cancel, Escape and the backdrop keep the line, and focus goes back to Remove", async () => {
  await renderMemory();
  const row = lineOf(FIRST);
  const remove = () => within(row).getByRole("button", { name: "Remove this line" });

  const asked = () => {
    remove().focus();
    fireEvent.click(remove());
  };
  asked();
  fireEvent.click(within(removeDialog()).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("dialog", { name: "Remove this line?" })).toBeNull();
  expect(document.activeElement).toBe(remove());

  asked();
  fireEvent.keyDown(removeDialog(), { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Remove this line?" })).toBeNull();
  expect(document.activeElement).toBe(remove());

  asked();
  fireEvent.mouseDown(removeDialog().parentElement as HTMLElement);
  expect(screen.queryByRole("dialog", { name: "Remove this line?" })).toBeNull();
  expect(document.activeElement).toBe(remove());

  await settle();
  expect(calls).toEqual([]);
  expect(page().getByText(FIRST)).toBeTruthy();
});

test("Cancel in the editor's dialog leaves the editor open on Remove", async () => {
  await renderMemory();

  fireEvent.click(lineOf(FIRST));
  type(draftBox(), "A draft that stays.");
  const remove = screen.getByRole("button", { name: "Remove" });
  remove.focus();
  fireEvent.click(remove);
  fireEvent.click(within(removeDialog()).getByRole("button", { name: "Cancel" }));

  expect(draftBox().value).toBe("A draft that stays.");
  expect(document.activeElement).toBe(remove);
  expect(calls).toEqual([]);
});

test("a line that changed while the dialog was open is not removed", async () => {
  const { queryClient } = await renderMemory();

  fireEvent.click(within(lineOf(FIRST)).getByRole("button", { name: "Remove this line" }));
  expect(removeDialog()).toBeTruthy();
  rewriteElsewhere(0, "Rewritten from another tab.");
  await readAgain(queryClient);
  await settle();

  expect(screen.queryByRole("dialog", { name: "Remove this line?" })).toBeNull();
  expect(calls.filter((call) => call.body.op === "remove")).toEqual([]);
});

// U16: refusals.

test("a refused Remove shows beside that row, and goes when another control is used", async () => {
  await renderMemory();

  elsewhere({ section: "Watch out for", op: "add", text: "Added from another tab." });
  const row = lineOf(FIRST);
  fireEvent.click(within(row).getByRole("button", { name: "Remove this line" }));
  confirmRemove();
  await settle();

  const item = row.closest(".a-item") as HTMLElement;
  expect(within(item).getByRole("alert").textContent).toMatch(/changed since you opened it/);
  expect(screen.getAllByRole("alert")).toHaveLength(1);

  fireEvent.click(lineOf(SECOND));
  await settle();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a refused Keep goes when the suggestion is discarded", async () => {
  await renderMemory();

  elsewhere({ section: "Watch out for", op: "add", text: "Added from another tab." });
  fireEvent.click(within(rewrite()).getByRole("button", { name: "Keep" }));
  await settle();
  expect(within(rewrite()).getByRole("alert")).toBeTruthy();

  fireEvent.click(within(rewrite()).getByRole("button", { name: "Discard" }));
  await settle();
  expect(document.querySelector('.a-sug[data-op="rewrite"]')).toBeNull();
  expect(screen.queryByRole("alert")).toBeNull();
});

// U16: the card.

const labels = (op: "add" | "rewrite") =>
  within(card(op))
    .getAllByRole("button")
    .map((button) => button.textContent);

test("a suggestion has Discard before Keep, the primary action, rightmost", async () => {
  await renderMemory();

  expect(labels("rewrite")).toEqual(["Discard", "Keep"]);
  // The step that clears the gate leads, on the left.
  expect(labels("add")).toEqual(["Run holdout eval...", "Discard", "Keep"]);
  for (const op of ["rewrite", "add"] as const) {
    const buttons = within(card(op)).getAllByRole("button");
    expect(buttons.at(-1)?.className).toContain("primary");
    expect(buttons.filter((button) => button.className.includes("primary"))).toHaveLength(1);
  }
});

const why = (op: "add" | "rewrite") => card(op).querySelector(".why")?.textContent ?? "";

test("the rationale reads as sentences, capitalised and ended with a stop", async () => {
  const served = globalThis.fetch;
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const reply = await served(path, init);
    if (init?.method || !path.startsWith("/api/v1/memory")) return reply;
    const data = (await reply.json()) as MemoryResponse;
    for (const section of data.doc?.sections ?? []) {
      for (const line of section.lines) if (line.suggestion) line.suggestion.why = "seen twice";
      for (const each of section.suggestions) each.why = "  seen   in two runs  ";
    }
    return new Response(JSON.stringify(data), { status: reply.status });
  });
  await renderMemory();

  expect(why("rewrite")).toMatch(/\d{2}:\d{2}\. Seen twice\./);
  expect(why("add")).toMatch(/\d{2}:\d{2}\. Seen in two runs\./);
  expect(why("rewrite")).not.toMatch(/\. seen/);
});

test("a long gate hint wraps in the card instead of widening the page", () => {
  // jsdom lays nothing out, so this holds the rules that make it wrap.
  const memoryCss = readFileSync(resolve(import.meta.dirname, "memory.css"), "utf8");
  const rule = (selector: string) => {
    const escaped = selector.replace(/[.>]/g, String.raw`\$&`);
    return memoryCss.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
  };
  expect(rule(".docs .gate code")).toMatch(/overflow-wrap:\s*anywhere/);
  expect(rule(".docs .gate code")).toMatch(/white-space:\s*normal/);
  // The text column may shrink below its content, so the code can wrap.
  expect(rule(".docs .gate > span")).toMatch(/min-width:\s*0/);
});

test("on a phone a keyboard on a row action shows the actions whole, with their focus ring", () => {
  // jsdom lays nothing out, so this holds the rules. The phone hides the
  // actions in a 1px clipped box, which also clips a focus ring drawn inside it.
  const css = readFileSync(resolve(import.meta.dirname, "memory.css"), "utf8");
  const phone = css.slice(css.indexOf("@media (max-width: 820px)"));
  const rule = (selector: string) => {
    const escaped = selector.replace(/[.>():-]/g, String.raw`\$&`);
    return phone.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`))?.[1] ?? "";
  };
  // Hidden for a tap, as before.
  expect(rule(".docs .a-ln .acts")).toMatch(/clip-path:\s*inset\(50%\)/);
  // Shown, unclipped and in the flow, while a keyboard is on one of them.
  const shown = rule(".docs .a-ln:has(.acts :focus-visible) .acts");
  expect(shown).toMatch(/clip-path:\s*none/);
  expect(shown).toMatch(/overflow:\s*visible/);
  expect(shown).toMatch(/width:\s*auto/);
  expect(shown).toMatch(/height:\s*auto/);
  expect(shown).toMatch(/position:\s*static/);
});

// U16: documents.

test("an unknown document keeps the tab strip and says it was not found", async () => {
  await renderMemory("/memory?doc=nope");

  // The reads answer on timers, so wait for the page to say so.
  expect(await screen.findByRole("heading", { level: 1, name: "Not found" })).toBeTruthy();
  const tabs = within(await screen.findByRole("navigation", { name: "Memory documents" }));
  expect(tabs.getAllByRole("link").length).toBeGreaterThan(1);
  expect(screen.getByRole("status").textContent).toBe(
    "There is no memory document called nope. Pick one of the tabs above.",
  );
  // A document that is not there is not a read failure, so there is no retry.
  expect(screen.queryByRole("button", { name: /retry|try again/i })).toBeNull();
});

test("?doc=fleet reads the shared document once, as the page with no doc does", async () => {
  await renderMemory("/memory?doc=fleet");
  await settle();

  expect(screen.getByRole("heading", { level: 1, name: "host / host-a" })).toBeTruthy();
  const memory = reads.filter((path) => path.startsWith("/api/v1/memory"));
  expect(memory.filter((path) => path.includes("scope=fleet"))).toEqual([]);
  expect(memory.length).toBeLessThanOrEqual(1);
});

// P8: the holdout eval, re-propose and the change history, with no terminal.
// Every eval here is the fixture's stand-in: no model runs.

// The value a fact in the eval dialog shows, without its note.
const fact = (dialog: HTMLElement, name: string) => {
  const term = within(dialog).getByText(name, { selector: "dt" });
  const value = term.nextElementSibling;
  if (!value) throw new Error(`no value for ${name}`);
  const meta = value.querySelector(".meta")?.textContent ?? "";
  return {
    value: (value.textContent ?? "").slice(0, (value.textContent ?? "").length - meta.length),
    meta: value.querySelector(".meta")?.textContent ?? null,
    bad: value.hasAttribute("data-bad"),
  };
};

const openEval = async () => {
  fireEvent.click(within(card("add")).getByRole("button", { name: "Run holdout eval..." }));
  const dialog = await screen.findByRole("dialog", {
    name: "Run the holdout eval for off-screen checks?",
  });
  await waitFor(() => expect(fact(dialog, "Cases").value).toBe("11"));
  return dialog;
};

test("Run holdout eval... shows the model runs and the quota it will probably take before it starts", async () => {
  await renderMemory();

  const dialog = await openEval();
  expect(fact(dialog, "Model runs")).toMatchObject({
    value: "33",
    meta: "22 case runs, 11 judge runs",
  });
  expect(fact(dialog, "Quota").value).toBe("about 3.3% of the week");
  expect(fact(dialog, "Left after")).toMatchObject({
    value: "about 23.7%",
    meta: "floor 5%",
    bad: false,
  });
  expect(fact(dialog, "Time").value).toBe("about 11 min");
  // Nothing is sent until it is confirmed, and Cancel has the focus.
  expect(calls).toEqual([]);
  expect(document.activeElement?.textContent).toBe("Cancel");
  const buttons = within(dialog).getAllByRole("button");
  expect(buttons.map((button) => button.textContent)).toEqual(["Cancel", "Run eval"]);
  expect(buttons[1]?.className).toContain("primary");

  fireEvent.click(within(dialog).getByRole("button", { name: "Run eval" }));
  await settle();

  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toMatch(/^\/api\/v1\/memory\/proposals\/[^/]+\/eval$/);
  expect(Object.keys(calls[0]?.body ?? {})).toEqual(["idempotency_key"]);
  expect(screen.queryByRole("dialog")).toBeNull();
  // The card shows the run, and Keep waits for it.
  const row = card("add");
  expect(within(row).getByRole("status").textContent).toMatch(
    /^Holdout eval: 11 cases, started \d{2}:\d{2}/,
  );
  expect(within(row).getByRole("progressbar", { name: "Estimated progress" })).toBeTruthy();
  expect((within(row).getByRole("button", { name: "Keep" }) as HTMLButtonElement).disabled).toBe(
    true,
  );
  expect(within(row).queryByRole("button", { name: /Run holdout eval/ })).toBeNull();
  expect(within(row).getByText("Keep waits for the eval.")).toBeTruthy();
});

test("Cancel in the eval dialog starts nothing", async () => {
  await renderMemory();

  const dialog = await openEval();
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await settle();

  expect(calls).toEqual([]);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(within(card("add")).getByRole("button", { name: "Run holdout eval..." })).toBeTruthy();
});

test("under the quota floor the eval does not start, and the dialog says when the week resets", async () => {
  stubDaemon("quota-red");
  await renderMemory();

  const dialog = await openEval();
  expect(fact(dialog, "Left after")).toMatchObject({ value: "about 0%", bad: true });
  expect(within(dialog).getByRole("alert").textContent).toMatch(
    /^Not started: it would leave 0 percent of the week, at or under the 5 percent floor\. The week resets \w{3} \d{1,2} \w{3}, \d{2}:\d{2}\.$/,
  );
  const run = within(dialog).getByRole("button", { name: "Run eval" }) as HTMLButtonElement;
  expect(run.disabled).toBe(true);
  fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
  expect(calls).toEqual([]);
});

test("a stale quota reading fails closed: the eval does not start", async () => {
  stubDaemon("quota-grey");
  await renderMemory();

  const dialog = await openEval();
  expect(within(dialog).getByRole("alert").textContent).toMatch(
    /^Not started: a weekly quota reading is stale, and the quota floor fails closed\.$/,
  );
  expect(
    (within(dialog).getByRole("button", { name: "Run eval" }) as HTMLButtonElement).disabled,
  ).toBe(true);
});

test("a suggestion on a text that moved offers Re-propose on current text, and the new one waits for its eval", async () => {
  await renderMemory("/memory?doc=beta");

  const before = card("retire");
  expect(before.querySelector(".gate.stale")?.textContent).toMatch(
    /^The document changed on \w{3} \d{1,2} \w{3}, after this was proposed\. Re-propose it on the current text, then run the eval again\.$/,
  );
  expect(within(before).queryByRole("button", { name: /Run holdout eval/ })).toBeNull();
  expect((within(before).getByRole("button", { name: "Keep" }) as HTMLButtonElement).disabled).toBe(
    true,
  );
  const blob = view("beta").doc.blob;
  fireEvent.click(within(before).getByRole("button", { name: "Re-propose on current text" }));
  await settle();

  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toMatch(/^\/api\/v1\/memory\/proposals\/[^/]+\/repropose$/);
  expect(calls[0]?.body).toMatchObject({ base_blob: blob });
  const after = card("retire");
  expect(after.querySelector(".gate.stale")).toBeNull();
  expect(after.querySelector(".gate")?.textContent).toMatch(/in no change set/);
  expect(within(after).queryByRole("button", { name: "Re-propose on current text" })).toBeNull();
  expect(within(after).getByRole("button", { name: "Run holdout eval..." })).toBeTruthy();
});

test("an eval that recorded nothing says why on its suggestion, and can run again", async () => {
  await renderMemory("/memory?doc=beta");

  const row = card("add");
  expect(row.querySelector(".gate")?.textContent).toMatch(
    /The eval started \w{3} \d{1,2} \w{3}, \d{2}:\d{2} recorded nothing: .*run\.sh exited with code 1/,
  );
  expect(within(row).getByRole("button", { name: "Run holdout eval again..." })).toBeTruthy();
});

const history = () => within(screen.getByRole("region", { name: "Change history" }));
const firstChange = () => history().getAllByRole("listitem")[0] as HTMLElement;

test("the change history lists each change, and an undone one says so with no button", async () => {
  await renderMemory("/memory?doc=alpha");

  const rows = history().getAllByRole("listitem");
  expect(rows.map((row) => row.querySelector(".lab")?.textContent)).toEqual([
    "Kept",
    "Edited",
    "Undone",
  ]);
  expect(rows[0]?.querySelector(".m")?.textContent).toMatch(
    /^How we work here · the alpha orchestrator from alpha-43 · kept by you, \w{3} \d{1,2} \w{3}, \d{2}:\d{2}$/,
  );
  expect(rows[1]?.querySelector(".m")?.textContent).toMatch(/^Preferences · you, /);
  const undone = rows[2] as HTMLElement;
  expect(within(undone).getByText("Already undone")).toBeTruthy();
  expect(within(undone).queryByRole("button")).toBeNull();
  expect(within(rows[0] as HTMLElement).getByRole("button").textContent).toBe("Undo...");
});

test("Undo... asks first, Keep the change sends nothing, and Undo reverses it where it is read", async () => {
  await renderMemory("/memory?doc=alpha");

  const kept = view("alpha").doc.changes[0];
  if (!kept?.text) throw new Error("no kept change");
  expect(page().getByText(kept.text)).toBeTruthy();
  fireEvent.click(within(firstChange()).getByRole("button", { name: /^Undo: Kept/ }));
  let dialog = screen.getByRole("dialog", { name: "Undo this change?" });
  expect(document.activeElement?.textContent).toBe("Keep the change");
  const buttons = within(dialog).getAllByRole("button");
  expect(buttons.map((button) => button.textContent)).toEqual(["Keep the change", "Undo"]);
  expect(buttons[1]?.className).toContain("danger");
  fireEvent.click(within(dialog).getByRole("button", { name: "Keep the change" }));
  expect(calls).toEqual([]);

  const blob = view("alpha").doc.blob;
  fireEvent.click(within(firstChange()).getByRole("button", { name: /^Undo: Kept/ }));
  dialog = screen.getByRole("dialog", { name: "Undo this change?" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Undo" }));
  await settle();

  expect(calls).toEqual([
    {
      path: `/api/v1/memory/changes/${kept.proposal_id}/undo`,
      body: expect.objectContaining({ base_blob: blob }),
    },
  ]);
  expect(within(firstChange()).getByText("Already undone")).toBeTruthy();
  expect(page().queryByText(kept.text)).toBeNull();
});

test("undo of a discard brings the suggestion back for review", async () => {
  await renderMemory();

  const text = "Never push to main without asking me.";
  expect(page().queryByText(text)).toBeNull();
  const row = history()
    .getAllByRole("listitem")
    .find((each) => each.textContent?.includes(text));
  if (!row) throw new Error("no discard row");
  expect(row.querySelector(".lab")?.textContent).toBe("Discarded");
  fireEvent.click(within(row).getByRole("button", { name: /^Undo: Discarded/ }));
  const dialog = screen.getByRole("dialog", { name: "Undo this discard?" });
  expect(within(dialog).getByText(/Nothing in the file changes until you keep it\./)).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Undo" }));
  await settle();

  expect(calls[0]?.path).toMatch(/\/undo$/);
  const back = page().getByText(text).closest<HTMLElement>(".a-sug");
  if (!back) throw new Error("the suggestion did not come back");
  // It is a new proposal, so Keep waits for its own holdout run.
  expect(within(back).getByRole("button", { name: "Run holdout eval..." })).toBeTruthy();
});
