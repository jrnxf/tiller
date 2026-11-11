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
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

// Wed 11 Nov 2026, 15:40 in Lisbon (UTC this time of year). The seeded retro
// of alpha-14 was cut at its 20 min limit at 04:20 the same day.
const NOW = Date.parse("2026-11-11T15:40:00Z");

let posts: { path: string; body: Record<string, unknown> }[] = [];
// A read answered here instead of by the fixture, by path.
let override: Record<string, unknown> = {};
// A POST answered here instead of by the fixture.
let refuse: { status: number; code: string; message: string } | null = null;
// A POST whose answer is lost on the way: the daemon may have acted on it.
let lose = false;
// A POST answered with this body, as the daemon's replay of a key does.
let answer: Record<string, unknown> | null = null;

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
    if (lose) throw new TypeError("Failed to fetch");
    if (answer) return new Response(JSON.stringify(answer), { status: 200 });
    if (refuse) {
      const { status, code, message } = refuse;
      return new Response(JSON.stringify({ ok: false, error: { code, message } }), { status });
    }
    const { reply } = fixtureMutation(method, path, body);
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
}

async function renderAt(path: string) {
  const queryClient = createTestQueryClient();
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
  lose = false;
  answer = null;
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
      "Curator was cut at the 20 min limit, Wed 11 Nov, 04:20. Nothing runs it again by itself.",
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

test("a waiting retro says that nothing starts its curator, and how to start it", async () => {
  await renderAt("/memory?doc=alpha");
  const waiting = rowOf(await retrosList(), "alpha-15", "pending");
  expect(waiting.getByText("Waiting")).toBeTruthy();
  const meta = waiting.getByText(/^Waiting since /);
  expect(meta.textContent).toBe(
    "Waiting since Wed 11 Nov, 13:30. Nothing starts its curator by itself: run tiller retro run 0000015 in a terminal.",
  );
  expect(within(meta).getByText("tiller retro run 0000015").tagName).toBe("CODE");
  expect(waiting.queryByRole("button")).toBeNull();
});

test("the header says so when an older retro that did not finish is listed", async () => {
  const listed = fixtureResource("/api/v1/retros?project=alpha", "default", NOW) as RetrosResponse;
  const old = "2026-09-01T10:00:00Z";
  override["/api/v1/retros?project=alpha"] = {
    ...listed,
    retros: listed.retros.map((retro) =>
      retro.card === "alpha-14"
        ? { ...retro, created_at: old, curator_started_at: old, finished_at: old }
        : retro,
    ),
  };
  await renderAt("/memory?doc=alpha");
  const list = await retrosList();
  expect(list.getByText("alpha · last 30 days and every unfinished one")).toBeTruthy();
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

test("a retry after a lost answer sends the same key, so the daemon runs it once", async () => {
  await renderAt("/memory?doc=alpha");
  const list = await retrosList();
  const button = () =>
    rowOf(list, "alpha-14", "unfinished").getByRole("button", {
      name: "Run the retro of alpha-14 again",
    });

  lose = true;
  fireEvent.click(button());
  expect((await list.findByRole("alert")).textContent).toBe(
    "The daemon did not answer, so this may not be saved yet. Trying again is safe: the same click is never recorded twice.",
  );
  await waitFor(() => expect(button().hasAttribute("disabled")).toBe(false));

  lose = false;
  fireEvent.click(button());
  await waitFor(() => rowOf(list, "alpha-14", "running"));
  expect(posts).toHaveLength(2);
  expect(posts[1]?.body.idempotency_key).toBe(posts[0]?.body.idempotency_key);
});

test("a replayed click that had already run and ended says so, and Run again is a new click", async () => {
  const listed = fixtureResource("/api/v1/retros?project=alpha", "default", NOW) as RetrosResponse;
  // The first click's curator failed fast, at 15:39, but its answer was lost
  // and the list was not read again.
  const ended = "2026-11-11T15:39:00Z";
  await renderAt("/memory?doc=alpha");
  const list = await retrosList();
  answer = {
    ok: true,
    replayed: true,
    events: [],
    receipt: { recorded_at: "2026-11-11T15:40:00Z", outbox_id: null, approval_id: null },
    data: {
      id: "01JRETRO000000000000000014",
      state: "done",
      outcome: "unfinished",
      finished_at: ended,
    },
  };
  override["/api/v1/retros?project=alpha"] = {
    ...listed,
    retros: listed.retros.map((retro) =>
      retro.card === "alpha-14"
        ? {
            ...retro,
            learning: "curator exited 3 without finishing the retro",
            started_by: "board",
            curator_started_at: "2026-11-11T15:38:00Z",
            finished_at: ended,
          }
        : retro,
    ),
  };
  fireEvent.click(
    rowOf(list, "alpha-14", "unfinished").getByRole("button", {
      name: "Run the retro of alpha-14 again",
    }),
  );
  const row = await waitFor(() => {
    const found = rowOf(list, "alpha-14", "unfinished");
    found.getByText(/^Curator exited 3/);
    return found;
  });
  expect(row.getByRole("status").textContent).toBe(
    "Your earlier Run again had already run, and it did not finish either.",
  );
  // The run it ended is a new one to run again, with a new key.
  const again = row.getByRole("button", { name: "Run the retro of alpha-14 again" });
  await waitFor(() => expect(again.hasAttribute("disabled")).toBe(false));
  answer = null;
  delete override["/api/v1/retros?project=alpha"];
  fireEvent.click(again);
  await waitFor(() => expect(posts).toHaveLength(2));
  expect(posts[1]?.body.idempotency_key).not.toBe(posts[0]?.body.idempotency_key);
});

test("a refusal shows as a sentence, and goes once the row changes", async () => {
  const { queryClient } = await renderAt("/memory?doc=alpha");
  const list = await retrosList();
  refuse = {
    status: 422,
    code: "refused",
    message: "retro 01JRETRO000000000000000014: its curator still runs",
  };
  fireEvent.click(
    rowOf(list, "alpha-14", "unfinished").getByRole("button", {
      name: "Run the retro of alpha-14 again",
    }),
  );
  expect((await list.findByRole("alert")).textContent).toBe(
    "Retro 01JRETRO000000000000000014: its curator still runs.",
  );

  // The list now shows the run it met: the refusal is about a row that is gone.
  const listed = fixtureResource("/api/v1/retros?project=alpha", "default", NOW) as RetrosResponse;
  override["/api/v1/retros?project=alpha"] = {
    ...listed,
    retros: listed.retros.map((retro) =>
      retro.card === "alpha-14"
        ? { ...retro, status: "running", curator_started_at: "2026-11-11T15:39:00Z" }
        : retro,
    ),
  };
  await act(async () => {
    await queryClient.invalidateQueries();
  });
  await waitFor(() => rowOf(list, "alpha-14", "running"));
  expect(list.queryByRole("alert")).toBeNull();
});

test("Health reports the retro that did not finish, with Open in Memory and Run again", async () => {
  await renderAt("/health");
  const panel = within(await screen.findByRole("region", { name: "Learning" }, { timeout: 3000 }));

  expect(panel.getByText("Retros and the curator, this week.")).toBeTruthy();
  expect(panel.getByText("1 retro did not finish")).toBeTruthy();
  expect(
    panel.getByText(
      (_, node) =>
        node?.className === "m" &&
        node.textContent === "alpha-14 · cut at the 20 min limit, Wed 11 Nov, 04:20",
    ),
  ).toBeTruthy();
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

test("Health says why Run again is off", async () => {
  const listed = fixtureResource("/api/v1/retros", "default", NOW) as RetrosResponse;
  override["/api/v1/retros"] = {
    ...listed,
    retros: listed.retros.map((retro) =>
      retro.card === "alpha-14"
        ? { ...retro, run_refusal: "config.toml cannot be used: no such table" }
        : retro,
    ),
  };
  await renderAt("/health");
  const panel = within(await screen.findByRole("region", { name: "Learning" }, { timeout: 3000 }));
  expect(
    panel.getByText(
      (_, node) =>
        node?.className === "m" &&
        node.textContent ===
          "alpha-14 · cut at the 20 min limit, Wed 11 Nov, 04:20. Run again is off: Config.toml cannot be used: no such table.",
    ),
  ).toBeTruthy();
  expect(
    panel.getByRole("button", { name: "Run the retro of alpha-14 again" }).hasAttribute("disabled"),
  ).toBe(true);
});
