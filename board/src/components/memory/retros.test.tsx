import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { RetrosResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import { resetMemoryFixture } from "../../lib/fixtures-memory";
import { RUN_MS, resetRetrosFixture } from "../../lib/fixtures-retros";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";

// Wed 11 Nov 2026, 15:40 in Lisbon (UTC this time of year). The seeded retro
// of alpha-14 was cut at its 20 min limit at 04:20 the same day.
const NOW = Date.parse("2026-11-11T15:40:00Z");

let posts: { path: string; body: Record<string, unknown> }[] = [];
// A read answered here instead of by the fixture, by path.
let override: Record<string, unknown> = {};
// A POST answered here instead of by the fixture.
let refuse: { status: number; code: string; message: string } | null = null;

function stubDaemon() {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      const data = path in override ? override[path] : fixtureResource(path, "default", Date.now());
      if (data === undefined) return new Response("{}", { status: 404 });
      return new Response(JSON.stringify(data), { status: 200 });
    }
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    posts.push({ path, body });
    if (refuse) {
      const { status, code, message } = refuse;
      return new Response(JSON.stringify({ ok: false, error: { code, message } }), { status });
    }
    const { reply } = fixtureMutation(method, path, body);
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
}

async function renderAt(path: string) {
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

const retrosList = async () =>
  within(await screen.findByRole("region", { name: "Retros" }, { timeout: 3000 }));

const rowOf = (list: ReturnType<typeof within>, card: string, status: string) => {
  const row = list
    .getAllByRole("listitem")
    .find((li: HTMLElement) => li.textContent?.includes(card) && li.dataset.status === status);
  if (!row) throw new Error(`no ${status} row for ${card}`);
  return within(row);
};

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW);
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  resetMemoryFixture(NOW);
  resetRetrosFixture(NOW);
  posts = [];
  override = {};
  refuse = null;
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
  vi.useRealTimers();
});

test("a project's document lists its retros, and the one that did not finish says why", async () => {
  await renderAt("/memory?doc=alpha");
  const list = await retrosList();

  expect(list.getByText("alpha · last 30 days")).toBeTruthy();
  const cut = rowOf(list, "alpha-14", "unfinished");
  expect(cut.getByText("Did not finish")).toBeTruthy();
  expect(cut.getByText("Split the export worker")).toBeTruthy();
  expect(
    cut.getByText(
      "Curator was cut at the 20 min limit, Wed 11 Nov, 04:20. The sweep does not run it again.",
    ),
  ).toBeTruthy();
  expect(cut.getByRole("button", { name: "Run the retro of alpha-14 again" })).toBeTruthy();

  const done = rowOf(list, "alpha-9", "done");
  expect(done.getByText("No suggestions · Fri 6 Nov, 13:52")).toBeTruthy();
  expect(done.getByText("Fri 6 Nov")).toBeTruthy();
  expect(done.queryByRole("button")).toBeNull();
  expect(rowOf(list, "alpha-12", "done").getByText(/^1 suggestion, kept · /)).toBeTruthy();
  // Only alpha's retros: beta's sits on beta's document.
  expect(list.queryByText("beta-28")).toBeNull();
});

test("the shared document has no Retros list", async () => {
  await renderAt("/memory");
  await screen.findByRole("article");
  expect(screen.queryByRole("region", { name: "Retros" })).toBeNull();
});

test("Run again starts the curator, shows it running, then its result", async () => {
  const { queryClient } = await renderAt("/memory?doc=alpha");
  const list = await retrosList();

  fireEvent.click(
    rowOf(list, "alpha-14", "unfinished").getByRole("button", {
      name: "Run the retro of alpha-14 again",
    }),
  );
  await waitFor(() => expect(posts).toHaveLength(1));
  expect(posts[0]?.path).toBe("/api/v1/retros/01JRETRO000000000000000014/run");
  expect(typeof posts[0]?.body.idempotency_key).toBe("string");

  const running = await waitFor(() => rowOf(list, "alpha-14", "running"));
  expect(running.getByText("Running")).toBeTruthy();
  expect(running.getByText("Started by you, Wed 11 Nov, 15:40 · limit 20 min")).toBeTruthy();
  expect(running.getByText("0:00")).toBeTruthy();
  // Nothing to run again while it runs.
  expect(running.queryByRole("button")).toBeNull();

  await act(async () => {
    vi.setSystemTime(NOW + RUN_MS + 1000);
    await queryClient.invalidateQueries();
  });
  const done = await waitFor(() => rowOf(list, "alpha-14", "done"));
  expect(done.getByText("1 suggestion, waiting in Watch out for · Wed 11 Nov, 15:40")).toBeTruthy();
  expect(done.getByRole("button", { name: "Review the suggestion from alpha-14" })).toBeTruthy();
  // The suggestion it filed waits in the document above.
  await waitFor(() =>
    expect(
      within(screen.getByRole("article")).getByText(/Drain the export queue before a worker/),
    ).toBeTruthy(),
  );
  expect(posts).toHaveLength(1);
});

test("Run again is off while another retro of the card runs, and a refusal shows by the row", async () => {
  const listed = fixtureResource("/api/v1/retros?project=alpha", "default", NOW) as RetrosResponse;
  const busy =
    "another retro of alpha-14 is running (retro 01JRETRO2); one card has one curator at a time";
  override["/api/v1/retros?project=alpha"] = {
    ...listed,
    retros: listed.retros.map((retro) =>
      retro.card === "alpha-14" ? { ...retro, run_refusal: busy } : retro,
    ),
  };
  await renderAt("/memory?doc=alpha");
  const list = await retrosList();
  const cut = rowOf(list, "alpha-14", "unfinished");
  const button = cut.getByRole("button", { name: "Run the retro of alpha-14 again" });
  expect(button.hasAttribute("disabled")).toBe(true);
  expect(button.getAttribute("title")).toBe(busy);
  expect(cut.getByText(/Run again is off: Another retro of alpha-14 is running/)).toBeTruthy();

  // The daemon refuses a start that raced another: the reason shows by the row.
  delete override["/api/v1/retros?project=alpha"];
  refuse = { status: 422, code: "refused", message: `retro 01JRETRO000000000000000014: ${busy}` };
  cleanup();
  await renderAt("/memory?doc=alpha");
  const again = await retrosList();
  fireEvent.click(
    rowOf(again, "alpha-14", "unfinished").getByRole("button", {
      name: "Run the retro of alpha-14 again",
    }),
  );
  expect((await again.findByRole("alert")).textContent).toContain(busy);
  expect(rowOf(again, "alpha-14", "unfinished").getByText("Did not finish")).toBeTruthy();
});

test("Health reports the retro that did not finish, with Open in Memory and Run again", async () => {
  await renderAt("/health");
  const panel = within(await screen.findByRole("region", { name: "Learning" }, { timeout: 3000 }));

  expect(panel.getByText("Retros and the curator, this week.")).toBeTruthy();
  expect(panel.getByText("1 retro did not finish")).toBeTruthy();
  expect(panel.getByText("alpha-14 · cut at the 20 min limit, Wed 11 Nov, 04:20")).toBeTruthy();
  // Since Monday 9 Nov: beta-28 (waiting), alpha-12 (kept) and gamma-31.
  expect(panel.getByText("3 retros done")).toBeTruthy();
  expect(panel.getByText("2 suggestions, 1 kept")).toBeTruthy();

  const open = panel.getByRole("link", { name: "Open in Memory" });
  expect(open.getAttribute("href")).toBe("/memory?doc=alpha");
  const actions = open.parentElement;
  // Run again is the primary action, on the right.
  expect(actions?.lastElementChild?.textContent).toBe("Run again");

  fireEvent.click(panel.getByRole("button", { name: "Run the retro of alpha-14 again" }));
  await waitFor(() => expect(panel.getByText("The retro of alpha-14 runs")).toBeTruthy());
  expect(panel.queryByText("1 retro did not finish")).toBeNull();
  expect(posts.map((post) => post.path)).toEqual(["/api/v1/retros/01JRETRO000000000000000014/run"]);
});
