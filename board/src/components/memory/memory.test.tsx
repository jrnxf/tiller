import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { resetMemoryFixture } from "../../lib/fixtures-memory";
import { resetRetrosFixture } from "../../lib/fixtures-retros";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

// Wed 11 Nov 2026, 15:40 in Lisbon.
const NOW = Date.parse("2026-11-11T15:40:00Z");
const ROUTE = "A new owner-only route also needs an authority test";
const ROUTE_ID = "01JPEND0000000000000000001";
const TOKEN = "Read the deploy token from the runner's store";

let scenario: Scenario = "default";
let writes: { method: string; path: string; body: Record<string, unknown> }[] = [];

function stubDaemon() {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      const data = fixtureResource(path, scenario, NOW);
      if (data === undefined) return new Response("{}", { status: 404 });
      return new Response(JSON.stringify(data), { status: 200 });
    }
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    writes.push({ method, path, body });
    const { reply } = fixtureMutation(method, path, body, scenario);
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
}

async function renderAt(path: string, seeded: Scenario = "default") {
  scenario = seeded;
  resetMemoryFixture(NOW, seeded);
  stubDaemon();
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(seeded, NOW));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  await screen.findByRole("tablist", { name: "Memory" }, { timeout: 3000 });
  return router;
}

const tab = (name: RegExp) => screen.getByRole("tab", { name });
const cardOf = (title: string) => {
  const card = screen.getByText(title).closest<HTMLElement>(".mem-card");
  if (!card) throw new Error(`no card for ${title}`);
  return within(card);
};

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  resetRetrosFixture(NOW);
  writes = [];
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("Pending opens first when notes wait, Live is one click away, and each shows its count", async () => {
  await renderAt("/memory?project=alpha");

  expect(tab(/^Live/).textContent).toBe("Live7");
  expect(tab(/^Pending/).textContent).toBe("Pending2");
  expect(tab(/^Pending/).getAttribute("aria-selected")).toBe("true");
  expect(screen.getByRole("tabpanel").id).toBe("mem-panel-pending");
  expect(screen.getByText(ROUTE)).toBeTruthy();

  fireEvent.click(tab(/^Live/));
  await waitFor(() => expect(tab(/^Live/).getAttribute("aria-selected")).toBe("true"));
  expect(screen.getByRole("tabpanel").id).toBe("mem-panel-live");
  expect(screen.getByRole("meter", { name: "Startup budget" })).toBeTruthy();
});

test("Live opens first when nothing waits", async () => {
  await renderAt("/memory?project=alpha", "empty");

  expect(tab(/^Live/).getAttribute("aria-selected")).toBe("true");
  expect(screen.getByRole("tabpanel").id).toBe("mem-panel-live");
});

test("a pending card shows its kind, source, time, and the card and run it came from", async () => {
  await renderAt("/memory?project=alpha");
  const card = cardOf(ROUTE);

  expect(card.getByText("gotcha")).toBeTruthy();
  expect(card.getByText("by builder", { exact: false }).textContent).toBe(
    "by builder · Wed 11 Nov, 13:35",
  );
  const links = card.getAllByRole("link");
  expect(links.map((link) => link.textContent)).toEqual(["card 0004", "run 0041"]);
  expect(links[0]?.getAttribute("href")).toBe("/cards/01JCARD0000000000000000004");
  // The actions sit on the right, Keep last.
  const buttons = card.getAllByRole("button").map((button) => button.textContent);
  expect(buttons).toEqual(["Discard", "Edit", "Keep"]);
});

test("Keep posts to the note's keep path with its rev, and the note leaves Pending", async () => {
  await renderAt("/memory?project=alpha");

  fireEvent.click(cardOf(ROUTE).getByRole("button", { name: /^Keep / }));

  await waitFor(() => expect(screen.queryByText(ROUTE)).toBeNull());
  expect(writes).toHaveLength(1);
  expect(writes[0]).toMatchObject({
    method: "POST",
    path: `/api/v1/memory/pending/${ROUTE_ID}/keep`,
    body: { rev: 1 },
  });
  expect(typeof writes[0]?.body.idempotency_key).toBe("string");
});

test("Discard posts to the note's discard path with its rev", async () => {
  await renderAt("/memory?project=alpha");

  fireEvent.click(cardOf(ROUTE).getByRole("button", { name: /^Discard / }));

  await waitFor(() => expect(screen.queryByText(ROUTE)).toBeNull());
  expect(writes).toEqual([
    {
      method: "POST",
      path: `/api/v1/memory/pending/${ROUTE_ID}/discard`,
      body: { idempotency_key: expect.any(String), rev: 1 },
    },
  ]);
});

test("Edit saves only the changed fields with PATCH and the rev, and the card shows them", async () => {
  await renderAt("/memory?project=alpha");

  fireEvent.click(cardOf(ROUTE).getByRole("button", { name: /^Edit / }));
  const form = within(screen.getByRole("form"));
  fireEvent.change(form.getByRole("textbox", { name: "Title" }), {
    target: { value: "Owner-only routes need an authority test" },
  });
  fireEvent.click(form.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("Owner-only routes need an authority test")).toBeTruthy();
  expect(writes).toEqual([
    {
      method: "PATCH",
      path: `/api/v1/memory/pending/${ROUTE_ID}`,
      body: {
        idempotency_key: expect.any(String),
        rev: 1,
        title: "Owner-only routes need an authority test",
      },
    },
  ]);
  // The next write carries the new rev.
  fireEvent.click(
    cardOf("Owner-only routes need an authority test").getByRole("button", { name: /^Keep / }),
  );
  await waitFor(() => expect(writes).toHaveLength(2));
  expect(writes[1]?.body.rev).toBe(2);
});

test("Edit changes the kind and body too, and Cancel sends nothing", async () => {
  await renderAt("/memory?project=alpha");

  fireEvent.click(cardOf(ROUTE).getByRole("button", { name: /^Edit / }));
  fireEvent.click(within(screen.getByRole("form")).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("form")).toBeNull();
  expect(writes).toHaveLength(0);

  fireEvent.click(cardOf(ROUTE).getByRole("button", { name: /^Edit / }));
  const form = within(screen.getByRole("form"));
  fireEvent.change(form.getByRole("textbox", { name: "Body" }), {
    target: { value: "Add the route to the owner list." },
  });
  fireEvent.click(form.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("Add the route to the owner list.")).toBeTruthy();
  expect(writes[0]?.body).toEqual({
    idempotency_key: expect.any(String),
    rev: 1,
    body: "Add the route to the owner list.",
  });
});

test("a Keep kb refuses shows kb's own message on the card, and the note stays", async () => {
  await renderAt("/memory?project=gamma");

  fireEvent.click(cardOf(TOKEN).getByRole("button", { name: /^Keep / }));

  const alert = await cardOf(TOKEN).findByRole("alert");
  expect(alert.textContent).toBe(
    "kb add: the note looks like it holds a secret (it names a token). Reword it and try again.",
  );
  expect(writes[0]?.body.rev).toBe(2);
  expect(screen.getByText(TOKEN)).toBeTruthy();
});

test("a stale rev comes back as a conflict, shown inline on the card", async () => {
  await renderAt("/memory?project=alpha");
  // Another board edits the note first.
  fixtureMutation("PATCH", `/api/v1/memory/pending/${ROUTE_ID}`, {
    idempotency_key: "elsewhere",
    rev: 1,
    body: "Changed elsewhere.",
  });

  fireEvent.click(cardOf(ROUTE).getByRole("button", { name: /^Discard / }));

  const alert = await cardOf(ROUTE).findByRole("alert");
  expect(alert.textContent).toBe(
    "This note changed since you opened it. Cancel to see the new text.",
  );
});

test("Live lists each note with its scope and tokens, and a skipped note says why", async () => {
  await renderAt("/memory?project=alpha&tab=live");
  const rows = within(screen.getByRole("list", { name: "Notes" })).getAllByRole("listitem");

  const runbook = rows.find((row) => row.textContent?.includes("full export"));
  expect(runbook?.dataset.loaded).toBe("false");
  expect(runbook?.textContent).toContain(
    "github.com/example/alpha · 5,200 tokens · Skipped: over budget",
  );
  const broken = rows.find((row) => row.textContent?.includes("old-format.md"));
  expect(broken?.textContent).toContain("Skipped: unreadable");
  const loaded = rows.find((row) => row.textContent?.includes("one worker until"));
  expect(loaded?.dataset.loaded).toBe("true");
  expect(loaded?.textContent).not.toContain("Skipped");
  // Open work never shows.
  expect(screen.queryByText(/retry path is half done/)).toBeNull();

  const meter = screen.getByRole("meter", { name: "Startup budget" });
  expect(meter.getAttribute("aria-valuenow")).toBe("3250");
  expect(screen.getByText("3,250 / 7,500 tokens")).toBeTruthy();
  expect(screen.getByText("Builder startup")).toBeTruthy();
  expect(screen.getByText(/^2 notes skipped\./)).toBeTruthy();
});

test("the role in the search picks whose launch Live shows", async () => {
  await renderAt("/memory?project=alpha&tab=live&role=reviewer");

  expect(screen.getByText("Reviewer startup")).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Role" }).textContent).toContain("Reviewer");
});

test("when kb is missing, Live warns that the fleet loads no notes on this host", async () => {
  await renderAt("/memory?project=alpha&tab=live", "memory-limits");

  const warning = screen.getByRole("alert");
  expect(warning.textContent).toContain("tiller cannot find kb on this host");
  expect(warning.textContent).toContain("The fleet loads no notes on this host");
  expect(warning.textContent).toContain("/home/user/.kb");
  expect(screen.getByText("No notes load while kb is down.")).toBeTruthy();
});

test("a Keep while kb is missing shows the daemon's message", async () => {
  await renderAt("/memory?project=alpha", "memory-limits");

  fireEvent.click(cardOf(ROUTE).getByRole("button", { name: /^Keep / }));

  const alert = await cardOf(ROUTE).findByRole("alert");
  expect(alert.textContent).toBe("tiller cannot find the kb command: set memory.kb_command");
});

test("each tab has an empty state", async () => {
  await renderAt("/memory?project=alpha&tab=pending", "empty");
  expect(screen.getByText("Nothing waits for you")).toBeTruthy();
  cleanup();

  await renderAt("/memory?tab=live");
  expect(screen.getByText("Pick a project")).toBeTruthy();
  cleanup();

  await renderAt("/memory?project=alpha&tab=live", "empty");
  expect(screen.getByText("kb has no notes for this project yet.")).toBeTruthy();
});

test("with no project, Pending lists every project's notes and names each one's project", async () => {
  await renderAt("/memory");

  expect(tab(/^Pending/).textContent).toBe("Pending4");
  expect(cardOf(TOKEN).getByText("gamma")).toBeTruthy();
  expect(
    cardOf("The bench forge stand-in has no checks command").getByText(/by the curator/),
  ).toBeTruthy();
});
