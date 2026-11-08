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

const card = (op: "add" | "rewrite" | "retire") => {
  const found = document.querySelector<HTMLElement>(`.a-sug[data-op="${op}"]`);
  if (!found) throw new Error(`no ${op} suggestion on the page`);
  return found;
};
const rewrite = () => card("rewrite");

const rail = () => screen.getByRole("complementary", { name: "Recent changes" });

// The line row that shows `text`.
const lineOf = (text: string | RegExp) => {
  const row = page().getByText(text, { selector: ".a-ln .tx" }).closest<HTMLElement>(".a-ln");
  if (!row) throw new Error("no line row");
  return row;
};

const type = (box: HTMLElement, value: string) => fireEvent.change(box, { target: { value } });

test("each document is a tab, with the suggestions waiting on it or its line count", async () => {
  await renderMemory();

  const tabs = within(screen.getByRole("navigation", { name: "Memory documents" }));
  const fleet = tabs.getByRole("link", { name: /^All projects \(shared\)/ });
  expect(fleet.getAttribute("aria-current")).toBe("page");
  expect(fleet.textContent).toBe("All projects (shared)2");
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

  expect(screen.getByRole("heading", { level: 1, name: "All projects (shared)" })).toBeTruthy();
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
  expect(within(rail()).getByText("Edited a line")).toBeTruthy();
  expect(screen.getByRole("button", { name: "1 suggestion to review" })).toBeTruthy();
  expect(screen.getByTestId("toast").textContent).toBe("Kept. Undo it from Recent changes.");
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
  expect(within(rail()).getAllByText("Discarded a suggestion")).toHaveLength(2);
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
  await settle();

  expect(calls[0]?.body).toMatchObject({ op: "remove", index: 0, section: "How we work here" });
  expect(page().queryByText(/^Before asking me to merge a sizeable PR/)).toBeNull();
  expect(within(rail()).getByText("Removed a line")).toBeTruthy();
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

  expect(screen.getByRole("alert").textContent).toBe(
    "fleet.md changed since you opened it; here is the current text",
  );
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

test("each Recent changes entry is one Undo button, and a click anywhere on it only undoes", async () => {
  await renderMemory();

  const entries = rail().querySelectorAll(".rc");
  expect(entries.length).toBeGreaterThan(0);
  // Nothing in the rail acts but the entries, and nothing inside an entry acts
  // on its own.
  const controls = rail().querySelectorAll("button, a, input, textarea, [tabindex]");
  expect([...controls]).toEqual([...entries]);
  for (const entry of entries) {
    expect(entry.tagName).toBe("BUTTON");
    expect(entry.getAttribute("title")).toBe("Undo this change");
    expect(entry.querySelector("button, a, [tabindex], [role='button']")).toBeNull();
  }
  const first = entries[0] as HTMLElement;
  expect(within(rail()).getAllByRole("button", { name: /^Undo / })[0]).toBe(first);

  // A click on the label of a fresh change undoes it, the same as a click on Undo.
  fireEvent.click(within(rewrite()).getByRole("button", { name: "Keep" }));
  await settle();
  const kept = rail().querySelector<HTMLElement>(".rc");
  if (!kept) throw new Error("expected the kept change");
  const before = view().doc.blob;
  const proposal = kept.dataset.proposal;
  fireEvent.click(kept.querySelector(".rc-t") as HTMLElement);
  await settle();
  expect(calls.slice(1)).toEqual([
    {
      path: `/api/v1/memory/changes/${proposal}/undo`,
      body: { idempotency_key: expect.any(String), base_blob: before },
    },
  ]);
  // The entry is gone, the rail is still Recent changes and no other view opened.
  expect(rail().querySelector(`[data-proposal="${proposal}"]`)).toBeNull();
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("Undo reverts the newest change; an older one that overlaps is refused with the reason", async () => {
  await renderMemory();

  fireEvent.click(within(rewrite()).getByRole("button", { name: "Keep" }));
  await settle();

  const [newest] = within(rail()).getAllByRole("button", { name: /^Undo / });
  if (!newest) throw new Error("expected the kept change");
  expect(newest.getAttribute("aria-label")).toMatch(
    /^Undo edited a line: Stop starting .* at 8%, from the curator, [A-Z][a-z]{2} \d{1,2} [A-Z][a-z]{2}, \d{2}:\d{2}$/,
  );
  const kept = view().doc.blob;
  fireEvent.click(newest);
  await settle();

  expect(calls[1]?.path).toMatch(/^\/api\/v1\/memory\/changes\/[^/]+\/undo$/);
  expect(calls[1]?.body.base_blob).toBe(kept);
  expect(
    page().getByText("Stop new worker dispatch when the weekly quota reaches 3%."),
  ).toBeTruthy();
  expect(within(rail()).queryByText("Edited a line")).toBeNull();
  expect(screen.getByTestId("toast").textContent).toBe("Undone: edited a line.");

  const [seeded] = within(rail()).getAllByRole("button", { name: /^Undo / });
  if (!seeded) throw new Error("expected the seeded change");
  fireEvent.click(seeded);
  await settle();
  expect(screen.getByRole("alert").textContent).toBe(
    "undoing it conflicts with a later edit to the same lines; change the line by hand",
  );
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
  // Recent changes stays as it was behind it.
  expect(rail()).toBeTruthy();

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

test("Keep waits for the holdout gate, with the reason and the command beside it", async () => {
  await renderMemory();

  const row = card("add");
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
