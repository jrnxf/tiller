import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { BoardResponse, BoardRow, ChangeEvent } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore, updatePrefs } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { boardPath, queryKeyFor } from "../../lib/queries";
import { createAppRouter } from "../../lib/router";
import { applyChange } from "../../lib/sse";
import { resetBoardLook } from "./board-prefs";

const HOST_B = "01JHOSTB00000000000000000B";
const CARD_43 = "01JCARD0000000000000000043";
const CARD_41 = "01JCARD0000000000000000041";

interface Posted {
  path: string;
  method: string;
}

// Answers the board API from the fixtures and records every write.
function stubApi(scenario: Scenario, focusStatus = 200) {
  const posted: Posted[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const method = init?.method ?? "GET";
      if (method !== "GET") {
        posted.push({ path, method });
        return focusStatus === 200
          ? new Response(
              JSON.stringify({ ok: true, replayed: false, events: [], receipt: null, data: null }),
              { status: 200 },
            )
          : new Response(
              JSON.stringify({
                ok: false,
                error: { code: "unavailable", message: "herdr is down" },
              }),
              { status: focusStatus },
            );
      }
      const body = fixtureResource(path, scenario);
      return body === undefined
        ? new Response("{}", { status: 404 })
        : new Response(JSON.stringify(body), { status: 200 });
    }),
  );
  return posted;
}

function stubMedia(matching: (query: string) => boolean = () => false) {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: matching(query),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
}

async function renderApp(scenario: Scenario = "default", path = "/") {
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario));
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
  resetBoardLook();
  connectionStore.set("live");
  stubMedia();
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.removeAttribute("data-density");
});

function cellOf(text: string) {
  return screen.getByText(text).closest<HTMLElement>(".cell");
}

// ---- Board ----

test("every project is a lane and every card sits in the column its state puts it in", async () => {
  stubApi("default");
  await renderApp();

  expect(screen.getByTestId("lane-alpha")).toBeTruthy();
  expect(screen.getByTestId("lane-beta")).toBeTruthy();
  expect(screen.getByTestId("lane-gamma")).toBeTruthy();
  expect(cellOf("Add the machines summary to the sidebar")?.dataset.column).toBe("working");
  expect(cellOf("Pick the retry policy for the export job")?.dataset.column).toBe("needs_you");
  expect(cellOf("Move the poller behind a trait")?.dataset.column).toBe("landing");
  expect(cellOf("Document the cutover steps")?.dataset.column).toBe("queued");
  expect(cellOf("Pin the toolchain version")?.dataset.column).toBe("done");
  expect(cellOf("Waiting for the orchestrator to tag this ask")?.dataset.column).toBe("asked");
});

test("the board is on screen at once: no spinner, no empty frame", async () => {
  stubApi("default");
  await renderApp();
  expect(screen.getByTestId("board")).toBeTruthy();
  expect(screen.queryByRole("progressbar")).toBeNull();
  expect(screen.queryByText(/loading/i)).toBeNull();
});

test("the All / Active / Needs you filter narrows the cards, lives in the URL and shows its counts", async () => {
  stubApi("default");
  const { router } = await renderApp();
  const [segment] = screen.getAllByRole("group", { name: "Cards shown" });
  if (!segment) throw new Error("no filter");

  // 9 cards and 1 ask: 9 are not done, 3 need the owner.
  expect(within(segment).getByRole("button", { name: /^All/ }).textContent).toBe("All10");
  expect(within(segment).getByRole("button", { name: /^Active/ }).textContent).toBe("Active9");
  expect(within(segment).getByRole("button", { name: /^Needs you/ }).textContent).toBe(
    "Needs you3",
  );

  fireEvent.click(within(segment).getByRole("button", { name: /^Needs you/ }));
  await waitFor(() => expect(router.state.location.search).toEqual({ filter: "needs" }));
  expect(screen.getByText("Pick the retry policy for the export job")).toBeTruthy();
  expect(screen.getByText("Renew the deploy certificate")).toBeTruthy();
  expect(screen.queryByText("Add the machines summary to the sidebar")).toBeNull();
  // A lane with nothing to show says why, instead of vanishing.
  expect(within(screen.getByTestId("lane-beta")).getByText("Nothing in this tab")).toBeTruthy();

  fireEvent.click(within(segment).getByRole("button", { name: /^Active/ }));
  await waitFor(() => expect(router.state.location.search).toEqual({ filter: "active" }));
  expect(screen.queryByText("Pin the toolchain version")).toBeNull();
  expect(screen.getByText("Add the machines summary to the sidebar")).toBeTruthy();

  fireEvent.click(within(segment).getByRole("button", { name: /^All/ }));
  await waitFor(() => expect(router.state.location.search).toEqual({}));
  expect(screen.getByText("Pin the toolchain version")).toBeTruthy();
});

test("a filter in the URL is applied on the first render", async () => {
  stubApi("default");
  await renderApp("default", "/?filter=needs");
  expect(screen.queryByText("Add the machines summary to the sidebar")).toBeNull();
  expect(screen.getByText("Pick the retry policy for the export job")).toBeTruthy();
});

test("a machine filter keeps only the lanes that have cards there", async () => {
  stubApi("default");
  await renderApp("default", `/?host=${HOST_B}`);
  expect(screen.getByTestId("lane-alpha")).toBeTruthy();
  expect(screen.queryByTestId("lane-beta")).toBeNull();
  expect(screen.getByText("Move the poller behind a trait")).toBeTruthy();
  expect(screen.queryByText("Add the machines summary to the sidebar")).toBeNull();
});

test("a project with no cards says so", async () => {
  stubApi("default");
  await renderApp("empty");
  expect(screen.getAllByText("No cards yet")).toHaveLength(3);
});

test("with every project hidden the board says why and one click brings them back", async () => {
  stubApi("default");
  updatePrefs({ hiddenProjects: ["alpha", "beta", "gamma"] });
  await renderApp();
  expect(screen.getByText("No projects shown")).toBeTruthy();
  fireEvent.click(within(screen.getByTestId("board-empty")).getByRole("button"));
  await waitFor(() => expect(screen.getByTestId("board")).toBeTruthy());
  expect(screen.getByTestId("lane-alpha")).toBeTruthy();
});

test("a card event moves the card to its new column without remounting the others", async () => {
  stubApi("default");
  const { queryClient } = await renderApp();
  const untouched = screen.getByText("Pick the retry policy for the export job").closest("a");
  const moving = screen.getByText("Add the machines summary to the sidebar").closest("a");
  expect(untouched).toBeTruthy();
  expect(moving?.closest<HTMLElement>(".cell")?.dataset.column).toBe("working");

  const board = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  const row = board?.rows.find((candidate) => candidate.display_id === "alpha-42") as BoardRow;
  act(() => {
    applyChange(queryClient, {
      seq: 5000,
      at: "2026-11-02T12:00:00.000Z",
      kind: "card.changed",
      txn: "x",
      entity: "card",
      entity_id: row.id,
      rev_after: 2,
      card_id: row.id,
      entity_state: null,
      card: { ...row, column: "landing", badge: "checks" },
    } satisfies ChangeEvent);
  });

  await waitFor(() =>
    expect(
      screen.getByText("Add the machines summary to the sidebar").closest<HTMLElement>(".cell")
        ?.dataset.column,
    ).toBe("landing"),
  );
  // Same element before and after: React updated it, nothing was rebuilt.
  expect(screen.getByText("Pick the retry policy for the export job").closest("a")).toBe(untouched);
});

// ---- Fit ----

type Measure = "scrollHeight" | "clientHeight" | "clientWidth";

// jsdom has no layout: give the board's frame a size and a content height that
// depends on how much detail the cards show.
function defineMeasure(name: Measure, value: (frame: Element) => number) {
  Object.defineProperty(Element.prototype, name, {
    configurable: true,
    get(this: Element) {
      return this.classList.contains("board-view") ? value(this) : 0;
    },
  });
}

function stubFrame(contentHeight: (fit: number) => number) {
  defineMeasure("clientHeight", () => 700);
  defineMeasure("clientWidth", () => 1200);
  defineMeasure("scrollHeight", (el) =>
    contentHeight(Number(el.querySelector(".board")?.getAttribute("data-fit") ?? 0)),
  );
  return () => {
    for (const name of ["scrollHeight", "clientHeight", "clientWidth"]) {
      Reflect.deleteProperty(Element.prototype, name);
    }
  };
}

test("lanes that overflow the frame drop card detail one step at a time, and stop once they fit", async () => {
  stubApi("busy");
  const restore = stubFrame((fit) => 1000 - 150 * fit);
  try {
    await renderApp("busy");
    // 1000, 850, 700: the third step fits the 700 px frame, so there is no fourth.
    expect(screen.getByTestId("board").getAttribute("data-fit")).toBe("2");
  } finally {
    restore();
  }
});

test("a board that cannot fit stops at the last step instead of looping", async () => {
  stubApi("busy");
  const restore = stubFrame(() => 5000);
  try {
    await renderApp("busy");
    expect(screen.getByTestId("board").getAttribute("data-fit")).toBe("3");
  } finally {
    restore();
  }
});

test("a board that already fits keeps the full card", async () => {
  stubApi("busy");
  const restore = stubFrame(() => 600);
  try {
    await renderApp("busy");
    expect(screen.getByTestId("board").getAttribute("data-fit")).toBe("0");
  } finally {
    restore();
  }
});

test("on a phone the columns stack and scroll, so nothing is dropped to fit", async () => {
  stubApi("busy");
  stubMedia((query) => query.includes("max-width"));
  const restore = stubFrame(() => 5000);
  try {
    await renderApp("busy");
    expect(screen.getByTestId("board").getAttribute("data-fit")).toBe("0");
  } finally {
    restore();
  }
});

// ---- Agents ----

function agentRow(label: string) {
  // The list only: the detail panel repeats the selected agent's label.
  const list = document.querySelector<HTMLElement>(".ag-list");
  if (!list) throw new Error("no agent list");
  const row = within(list).getByText(label).closest<HTMLElement>("[role=button]");
  if (!row) throw new Error(`no row for ${label}`);
  return row;
}

test("each orchestrator heads its workers, drawn as a tree with role, model, status and last line", async () => {
  stubApi("default");
  await renderApp("default", "/agents");

  const group = screen.getByRole("region", { name: "alpha" });
  const labels = [...group.querySelectorAll(".wsl")].map((node) => node.textContent);
  expect(labels).toEqual([
    "alpha",
    "├ alpha-41 build",
    "├ alpha-42 build",
    "├ alpha-43 build",
    "└ alpha-44 build",
  ]);

  const worker = agentRow("├ alpha-43 build");
  expect(worker.textContent).toContain("build");
  expect(worker.textContent).toContain("opus");
  expect(worker.textContent).toContain("Working");
  expect(worker.textContent).toContain("Running tests for alpha-43");
  expect(worker.className).toContain("nest");
  expect(group.querySelector("[role=button]")?.className).not.toContain("nest");
});

test("a worker that went quiet reads as stale, not working", async () => {
  stubApi("default");
  await renderApp("default", "/agents");
  expect(agentRow("└ beta-7 build").textContent).toContain("Stale, was working");
});

test("the button says it opens herdr on the remote machine first only for a remote agent", async () => {
  stubApi("default");
  await renderApp("default", "/agents");

  const local = within(agentRow("├ alpha-42 build")).getByRole("button", { name: /Open in herdr/ });
  const remote = within(agentRow("├ alpha-43 build")).getByRole("button", {
    name: /Open in herdr/,
  });
  expect(local.getAttribute("title")).toBe("Focus this agent's terminal in herdr");
  expect(remote.getAttribute("title")).toBe(
    "Opens herdr on the remote machine first, then focuses this agent's terminal",
  );
});

test("Open in herdr asks the daemon to focus that agent, and says the remote machine was opened", async () => {
  const posted = stubApi("default");
  await renderApp("default", "/agents");

  const row = agentRow("├ alpha-43 build");
  fireEvent.click(within(row).getByRole("button", { name: /Open in herdr/ }));

  await waitFor(() => expect(posted).toHaveLength(1));
  const sessionId = row.dataset.testid?.replace("agent-", "");
  expect(posted[0]).toEqual({ path: `/api/v1/agents/${sessionId}/focus`, method: "POST" });
  await waitFor(() =>
    expect(screen.getByRole("status").textContent).toContain("Opened herdr on host-b"),
  );
  // Clicking the button did not also change which agent is selected.
  expect(row.getAttribute("aria-selected")).toBe("false");
});

test("the O key opens the selected agent, and a letter typed into a field does not", async () => {
  const posted = stubApi("default");
  await renderApp("default", "/agents");

  fireEvent.click(agentRow("├ alpha-42 build"));
  await waitFor(() =>
    expect(agentRow("├ alpha-42 build").getAttribute("aria-selected")).toBe("true"),
  );
  const sessionId = agentRow("├ alpha-42 build").dataset.testid?.replace("agent-", "");

  fireEvent.keyDown(window, { key: "k", metaKey: true });
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "o" });
  expect(posted).toHaveLength(0);
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "Escape" });

  fireEvent.keyDown(window, { key: "o" });
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe(`/api/v1/agents/${sessionId}/focus`);

  fireEvent.keyDown(window, { key: "o", metaKey: true });
  fireEvent.keyDown(window, { key: "o", ctrlKey: true });
  expect(posted).toHaveLength(1);
});

test("when herdr cannot be reached the board says so and changes nothing", async () => {
  stubApi("default", 503);
  await renderApp("default", "/agents");
  fireEvent.click(
    within(agentRow("├ alpha-42 build")).getByRole("button", { name: /Open in herdr/ }),
  );
  await waitFor(() =>
    expect(screen.getByRole("status").textContent).toContain("herdr did not answer"),
  );
});

test("the detail panel shows the focus command, branch, worktree, label and recent events", async () => {
  stubApi("default");
  const { router } = await renderApp("default", "/agents");
  fireEvent.click(agentRow("├ alpha-42 build"));
  await waitFor(() => expect(router.state.location.search).toHaveProperty("agent"));

  const panel = screen.getByRole("complementary", { name: "Agent details" });
  expect(panel.textContent).toContain("herdr agent focus alpha-42-build");
  expect(panel.textContent).toContain("ship/fixture");
  expect(panel.textContent).toContain("/home/user/work/fixture");
  expect(panel.textContent).toContain("├ alpha-42 build");
  expect(panel.textContent).toContain("pane.state working");
  expect(within(panel).getByRole("link", { name: /Open card alpha-42/ })).toBeTruthy();
});

test("a machine filter keeps only that machine's agents and redraws the tree", async () => {
  stubApi("default");
  await renderApp("default", `/agents?host=${HOST_B}`);
  expect(agentRow("└ alpha-43 build")).toBeTruthy();
  expect(screen.queryByText("├ alpha-42 build")).toBeNull();
  expect(screen.queryByRole("region", { name: "beta" })).toBeNull();
});

test("moving between views keeps the screen you were on until the next one is ready", async () => {
  stubApi("default");
  const { router } = await renderApp();
  fireEvent.click(within(screen.getByRole("navigation", { name: "Screens" })).getByText("Agents"));
  // The loader is still fetching runs: the board stays, nothing pending shows.
  expect(screen.getByTestId("board")).toBeTruthy();
  expect(screen.queryByRole("progressbar")).toBeNull();
  await waitFor(() => expect(router.state.location.pathname).toBe("/agents"));
  expect(screen.queryByTestId("board")).toBeNull();
  expect(screen.getByRole("complementary", { name: "Agent details" })).toBeTruthy();
});

// ---- Card detail ----

test("a card on the board opens its detail page", async () => {
  stubApi("default");
  const { router } = await renderApp();
  fireEvent.click(screen.getByText("Move the poller behind a trait"));
  await waitFor(() => expect(router.state.location.pathname).toBe(`/cards/${CARD_43}`));
  expect(screen.getByTestId("card-page")).toBeTruthy();
});

test("a stale approval is called out with the commits that came after it", async () => {
  stubApi("default");
  await renderApp("default", `/cards/${CARD_43}`);

  const page = screen.getByTestId("card-page");
  expect(within(page).getByText(/Approval is stale/)).toBeTruthy();
  const box = page.querySelector<HTMLElement>(".appr");
  expect(box?.dataset.standing).toBe("stale");
  expect(box?.textContent).toContain("Approval bound to an older commit");
  expect(box?.textContent).toContain("undo: coalesce on pointerup");
  // The bound commit is struck through; the head it moved to is highlighted.
  expect(page.querySelector(".appr .sha.old")?.textContent).toHaveLength(7);
  expect(page.querySelector(".appr .sha.new")?.textContent).toHaveLength(7);
  expect(page.textContent).toContain("+2 commits since approval");
});

test("the page lists runs, the PR with its head, events and how fresh each signal is", async () => {
  stubApi("default");
  await renderApp("default", `/cards/${CARD_43}`);
  const page = screen.getByTestId("card-page");

  expect(page.textContent).toContain("Attempt 1");
  expect(page.textContent).toContain("Attempt 2");
  expect(page.textContent).toContain("failed");
  expect(page.textContent).toContain("example/alpha#52");
  expect(page.textContent).toContain("ci.pass 4/4");
  expect(page.textContent).toContain("Observation freshness");
  expect(page.textContent).toContain("Pane (herdr)");
  // Only the running attempt can be opened in herdr.
  expect(within(page).getAllByRole("button", { name: /Open in herdr/ })).toHaveLength(1);
});

test("a card without a PR says an approval comes with one", async () => {
  stubApi("default");
  await renderApp("default", `/cards/${CARD_41}`);
  const page = screen.getByTestId("card-page");
  expect(page.textContent).toContain("No PR yet");
  expect(within(page).queryByText(/Approval is stale/)).toBeNull();
});

test("earlier timeline events load on request and the button goes away at the start", async () => {
  stubApi("default");
  await renderApp("default", `/cards/${CARD_43}`);
  const page = screen.getByTestId("card-page");

  expect(page.textContent).not.toContain("asked in the orchestrator pane");
  fireEvent.click(within(page).getByRole("button", { name: "Show earlier" }));
  await waitFor(() => expect(page.textContent).toContain("asked in the orchestrator pane"));
  expect(within(page).queryByRole("button", { name: "Show earlier" })).toBeNull();
  // Newest first: the earlier events come after the ones already shown.
  const times = [...page.querySelectorAll(".tl .it .tx")].map((node) => node.textContent ?? "");
  expect(times[0]).toContain("logged progress");
  expect(times.at(-1)).toContain("asked in the orchestrator pane");
});

test("a card that cannot be loaded says so and links back to the board", async () => {
  stubApi("default");
  await renderApp("default", "/cards/does-not-exist");
  expect(screen.getByText("This card could not be loaded")).toBeTruthy();
  expect(screen.getByRole("link", { name: "Back to the board" })).toBeTruthy();
});

test("an event that refetches the timeline drops the earlier pages, so no event is skipped", async () => {
  stubApi("default");
  const { queryClient } = await renderApp("default", `/cards/${CARD_43}`);
  const page = screen.getByTestId("card-page");
  fireEvent.click(within(page).getByRole("button", { name: "Show earlier" }));
  await waitFor(() => expect(page.textContent).toContain("asked in the orchestrator pane"));

  // A new event arrives: the first page is refetched and no longer joins the older one.
  await act(async () => {
    await queryClient.invalidateQueries({ queryKey: ["api", `/api/v1/cards/${CARD_43}/timeline`] });
  });
  await waitFor(() => expect(page.textContent).not.toContain("asked in the orchestrator pane"));
  expect(within(page).getByRole("button", { name: "Show earlier" })).toBeTruthy();
});

test("moving from one card to another does not carry over the first card's earlier events", async () => {
  stubApi("default");
  const { router } = await renderApp("default", `/cards/${CARD_43}`);
  const page = screen.getByTestId("card-page");
  fireEvent.click(within(page).getByRole("button", { name: "Show earlier" }));
  await waitFor(() => expect(page.textContent).toContain("asked in the orchestrator pane"));

  await act(async () => {
    await router.navigate({ to: "/cards/$cardId", params: { cardId: CARD_41 } });
  });
  await waitFor(() => expect(screen.getByTestId("card-page").textContent).toContain("alpha-41"));
  expect(screen.getByTestId("card-page").textContent).not.toContain(
    "asked in the orchestrator pane",
  );
});

test("on a phone with no agent chosen the O key does nothing", async () => {
  const posted = stubApi("default");
  stubMedia((query) => query.includes("max-width"));
  await renderApp("default", "/agents");
  fireEvent.keyDown(window, { key: "o" });
  await new Promise((done) => setTimeout(done, 50));
  expect(posted).toHaveLength(0);
});
