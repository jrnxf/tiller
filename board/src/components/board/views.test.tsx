import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type {
  BoardResponse,
  BoardRow,
  ChangeEvent,
  Project,
  ProjectRetirePreview,
  ProjectsResponse,
  PromptDialog,
} from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { FIXTURE_HEAD_SHA, fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore, updatePrefs } from "../../lib/prefs";
import { receiptsStore } from "../../lib/receipts";
import { createQueryClient } from "../../lib/query-client";
import { boardPath, projectsPath, queryKeyFor } from "../../lib/queries";
import { createAppRouter } from "../../lib/router";
import { applyChange } from "../../lib/sse";
import { ArchiveProject } from "../shell/project-archive";
import { ProjectDetail } from "../shell/project-stage";
import { DeleteProject } from "../shell/project-delete";
import { resetBoardLook } from "./board-prefs";

const HOST_B = "01JHOSTB00000000000000000B";
const CARD_43 = "01JCARD0000000000000000043";
const CARD_44 = "01JCARD0000000000000000044";
const CARD_42 = "01JCARD0000000000000000042";
const CARD_41 = "01JCARD0000000000000000041";

interface Posted {
  path: string;
  method: string;
  body: Record<string, unknown>;
}

// The dialog a waiting agent's screen shows.
const PROMPT: PromptDialog = {
  tool: "Bash",
  title: "Bash command",
  command: "rm -f old.txt",
  question: "Do you want to proceed?",
  options: [
    { n: 1, text: "Yes" },
    { n: 2, text: "No" },
  ],
  screen: "hash",
};

// Answers the board API from the fixtures and records every write.
function stubApi(scenario: Scenario, focusStatus = 200, focused: unknown = null) {
  const posted: Posted[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const method = init?.method ?? "GET";
      // The stage mirrors the selected agent's pane, or tails a remote one;
      // these tests never read it.
      if (/\/terminal(\/tail)?$/.test(path)) return new Response("{}", { status: 404 });
      if (method !== "GET") {
        posted.push({
          path,
          method,
          body: JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>,
        });
        if (path.endsWith("/prompt")) {
          const read = { session_id: "s", read_at: new Date().toISOString(), reason: null };
          return new Response(JSON.stringify({ ...read, prompt: PROMPT }), { status: 200 });
        }
        return focusStatus === 200
          ? new Response(
              JSON.stringify({
                ok: true,
                replayed: false,
                events: [],
                receipt: null,
                data: focused,
              }),
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
    // xterm still listens the old way, for the device pixel ratio.
    addListener: () => {},
    removeListener: () => {},
  }));
}

// `unread` leaves those paths out of the boot snapshot, so they are fetched.
async function renderApp(scenario: Scenario = "default", path = "/", unread: string[] = []) {
  const queryClient = createQueryClient();
  // The board's one retry, without the real second of backoff before it,
  // so a loaded test run cannot push these tests past their timeout.
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  const snapshot = fixtureSnapshot(scenario);
  snapshot.queries = Object.fromEntries(
    Object.entries(snapshot.queries).filter(([key]) => !unread.includes(key)),
  );
  hydrateFromSnapshot(queryClient, snapshot);
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
  receiptsStore.set({ byKey: {}, progress: {} });
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

function tileOf(text: string): HTMLElement {
  const tile = screen.getByText(text).closest<HTMLElement>(".card");
  if (!tile) throw new Error(`no tile holds "${text}"`);
  return tile;
}

// ---- Board ----

test("every project is a lane and every card sits in the column its state puts it in", async () => {
  stubApi("default");
  await renderApp();

  expect(screen.getByTestId("lane-alpha")).toBeTruthy();
  expect(screen.getByTestId("lane-beta")).toBeTruthy();
  expect(screen.getByTestId("lane-gamma")).toBeTruthy();
  expect(cellOf("Add the machines summary to the sidebar")?.dataset.column).toBe("working");
  // A card that waits on the owner stays where its work is, marked by its bar.
  const waiting = screen.getByText("Pick the retry policy for the export job");
  expect(cellOf("Pick the retry policy for the export job")?.dataset.column).toBe("working");
  expect(waiting.closest<HTMLElement>(".card")?.dataset.attn).toBe("true");
  expect(cellOf("Retry failed exports with backoff")?.dataset.column).toBe("landing");
  expect(cellOf("Move the poller behind a trait")?.dataset.column).toBe("landing");
  expect(cellOf("Document the cutover steps")?.dataset.column).toBe("queued");
  expect(cellOf("Pin the toolchain version")?.dataset.column).toBe("done");
  expect(cellOf("Can the card fade on exit instead of sliding?")?.dataset.column).toBe("asked");
});

test("an untagged ask shows its source and its first line, and says it waits to be tagged", async () => {
  stubApi("default");
  await renderApp();

  const chat = tileOf("Can the card fade on exit instead of sliding?");
  expect(within(chat).getByText("chat")).toBeTruthy();
  expect(within(chat).getByText("Waiting for the orchestrator to tag it")).toBeTruthy();
  const board = tileOf(
    "Move the export settings into their own page, keep the old links working, and say in the release notes where each one went",
  );
  expect(within(board).getByText("board")).toBeTruthy();
  const script = tileOf("Nightly check: the staging certificate expires in 6 days");
  expect(within(script).getByText("script")).toBeTruthy();
  // No ask is shown without its text any more.
  expect(screen.queryByText("Waiting for the orchestrator to tag this ask")).toBeNull();
});

test("a peer's ask, which carries no text, still says it waits to be tagged", async () => {
  stubApi("default");
  const { queryClient } = await renderApp();
  const board = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  if (!board) throw new Error("no board");
  act(() => {
    queryClient.setQueryData<BoardResponse>(queryKeyFor(boardPath()), {
      ...board,
      asked: [
        {
          ...(board.asked[0] as BoardResponse["asked"][number]),
          summary: undefined,
          source: undefined,
        },
      ],
    });
  });
  await screen.findByText("Waiting for the orchestrator to tag this ask");
  const tile = tileOf("Waiting for the orchestrator to tag this ask");
  expect(tile.querySelector(".c-from")).toBeNull();
  expect(within(tile).queryByText("chat")).toBeNull();
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

  // 9 cards and 3 asks: 11 are not done, 3 need the owner.
  expect(within(segment).getByRole("button", { name: /^All/ }).textContent).toBe("All12");
  expect(within(segment).getByRole("button", { name: /^Active/ }).textContent).toBe("Active11");
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

test("with no agents running the Agents screen says so, not that projects are hidden", async () => {
  stubApi("empty");
  await renderApp("empty", "/agents");
  const empty = screen.getByTestId("agents-empty");
  expect(empty.textContent).toContain("No agents running");
  expect(screen.queryByText("No projects shown")).toBeNull();
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
  // TanStack Query hands a write to React on a timer, and the glide applies
  // the move as a transition. Outside act, a loaded machine renders that
  // transition in slices between timers, past the wait below. Staying in act
  // until the timer has fired makes React render the move to its end.
  await act(async () => {
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
    await new Promise((resolve) => setTimeout(resolve, 0));
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

function columnHead(name: string) {
  const head = [...document.querySelectorAll<HTMLElement>(".bhead .colh")].find(
    (colh) => colh.textContent?.includes(name) || colh.querySelector(`[aria-label^="${name} "]`),
  );
  if (!head) throw new Error(`no column head for ${name}`);
  return head;
}

test("columns with cards share the board's width and an empty column folds to its icon", async () => {
  stubApi("default");
  await renderApp("default", "/?filter=needs");

  const tracks = screen.getByTestId("board").style.getPropertyValue("--cols").split(" ");
  // Asked, Queued and Done hold no card that needs you; Working and Landing do.
  expect(tracks).toEqual(["52px", "52px", "minmax(168px,1fr)", "minmax(168px,1fr)", "52px"]);
  for (const name of ["Asked", "Queued", "Done"]) {
    const head = columnHead(name);
    expect(head.dataset.empty).toBe("true");
    expect(head.querySelector(".n")).toBeNull();
  }
  expect(columnHead("Working").dataset.empty).toBeUndefined();
});

function sidebarCount() {
  const nav = screen.getByRole("navigation", { name: "Screens" });
  return within(nav).getByText("Board").parentElement?.querySelector(".count")?.textContent;
}
function activeTab() {
  const [segment] = screen.getAllByRole("group", { name: "Cards shown" });
  if (!segment) throw new Error("no filter");
  return within(segment)
    .getByRole("button", { name: /^Active/ })
    .querySelector(".tc")?.textContent;
}

test("the sidebar's Board count is the header's Active count, with and without a machine filter", async () => {
  stubApi("default");
  const { router } = await renderApp();
  // 8 open cards and 3 asks: the asks count in both.
  expect(activeTab()).toBe("11");
  expect(sidebarCount()).toBe("11");

  await act(async () => {
    await router.navigate({ to: "/", search: { host: "01JHOSTA00000000000000000A" } });
  });
  await waitFor(() => expect(sidebarCount()).toBe(activeTab()));
});

test("an empty column's icon names the column and its count on hover and on focus", async () => {
  stubApi("default");
  await renderApp("default", "/?filter=needs");

  const icon = screen.getByLabelText("Queued 0");
  expect(icon.tabIndex).toBe(0);
  act(() => icon.focus());
  await waitFor(() => expect(document.querySelector(".colh-tip")?.textContent).toBe("Queued 0"));
  act(() => icon.blur());
  await waitFor(() => expect(document.querySelector(".colh-tip")).toBeNull());

  fireEvent.mouseEnter(screen.getByLabelText("Done 0"));
  fireEvent.pointerEnter(screen.getByLabelText("Done 0"), { pointerType: "mouse" });
  fireEvent.mouseMove(screen.getByLabelText("Done 0"));
  await waitFor(() => expect(document.querySelector(".colh-tip")?.textContent).toBe("Done 0"));
});

test("a column that gains a card unfolds", async () => {
  stubApi("default");
  const { queryClient } = await renderApp("default", "/?filter=needs");
  expect(columnHead("Queued").dataset.empty).toBe("true");

  const board = queryClient.getQueryData<BoardResponse>(queryKeyFor(boardPath()));
  const row = board?.rows.find((candidate) => candidate.display_id === "alpha-41") as BoardRow;
  act(() => {
    applyChange(queryClient, {
      seq: 5001,
      at: "2026-11-02T12:00:00.000Z",
      kind: "card.changed",
      txn: "x",
      entity: "card",
      entity_id: row.id,
      rev_after: 2,
      card_id: row.id,
      entity_state: null,
      card: { ...row, column: "queued" },
    } satisfies ChangeEvent);
  });

  await waitFor(() => expect(columnHead("Queued").dataset.empty).toBeUndefined());
  expect(screen.getByTestId("board").style.getPropertyValue("--cols").split(" ")[1]).toBe(
    "minmax(168px,1fr)",
  );
});

test("with no card on the board no column folds", async () => {
  stubApi("empty");
  await renderApp("empty");
  expect(screen.getByTestId("board").querySelector("[data-empty]")).toBeNull();
  expect(screen.getByTestId("board").style.getPropertyValue("--cols")).not.toContain("52px");
});

// ---- Agents ----

function agentList() {
  return screen.getByRole("listbox", { name: "Agents" });
}

function agentOptions() {
  return within(agentList()).getAllByRole("option");
}

// A row on the rail by the name it shows: the project for an orchestrator,
// the card for a worker.
function agentRow(name: string) {
  const row = agentOptions().find((option) => option.querySelector(".nm .l")?.textContent === name);
  if (!row) throw new Error(`no row for ${name}`);
  return row;
}

// The selected agent's Open in herdr, the rightmost action on the stage.
function stageOpen() {
  const acts = document.querySelector<HTMLElement>(".av-acts");
  if (!acts) throw new Error("no agent on the stage");
  return within(acts).getByRole("button", { name: /Open in herdr/ }) as HTMLButtonElement;
}

async function selectAgent(name: string) {
  fireEvent.click(agentRow(name));
  await waitFor(() => expect(agentRow(name).getAttribute("aria-selected")).toBe("true"));
}

test("each orchestrator heads its workers, drawn as a tree with role, status and last line", async () => {
  stubApi("default");
  await renderApp("default", "/agents");

  const group = within(agentList()).getByRole("group", { name: "alpha" });
  const rows = within(group).getAllByRole("option");
  expect(rows.map((row) => row.querySelector(".nm .l")?.textContent)).toEqual([
    "alpha",
    "alpha-41",
    "alpha-42",
    "alpha-43",
    "alpha-44",
  ]);
  expect(rows.map((row) => row.className.match(/\b(orch|wk)\b/)?.[0])).toEqual([
    "orch",
    "wk",
    "wk",
    "wk",
    "wk",
  ]);
  // Only the last worker's elbow ends the tree's stem.
  expect(rows.map((row) => row.classList.contains("last"))).toEqual([
    false,
    false,
    false,
    false,
    true,
  ]);

  const worker = agentRow("alpha-43");
  expect(worker.textContent).toContain("build");
  expect(worker.textContent).toContain("Working");
  expect(worker.textContent).toContain("Running tests for alpha-43");
  await selectAgent("alpha-43");
  expect(document.querySelector(".av-who-1")?.textContent).toContain("opus");
});

test("a working agent with no output for 10 minutes says how long it has been quiet", async () => {
  stubApi("default");
  await renderApp("default", "/agents");
  expect(agentRow("alpha-42").querySelector(".tm")?.textContent).toBe("quiet 11 min");
  expect(agentRow("alpha-43").querySelector(".tm")?.textContent).toBe("14 min ago");
});

test("a worker that went quiet reads as stale, not working", async () => {
  stubApi("default");
  await renderApp("default", "/agents");
  expect(agentRow("beta-7").textContent).toContain("Stale, was working");
});

test("J and K walk the rail, N jumps to the next agent that needs you, and a field keeps its keys", async () => {
  stubApi("default");
  const { router } = await renderApp("default", "/agents");
  const selected = () =>
    agentOptions().findIndex((row) => row.getAttribute("aria-selected") === "true");
  expect(selected()).toBe(0);

  fireEvent.keyDown(window, { key: "j" });
  await waitFor(() => expect(selected()).toBe(1));
  fireEvent.keyDown(window, { key: "k" });
  await waitFor(() => expect(selected()).toBe(0));
  fireEvent.keyDown(window, { key: "k" });
  await waitFor(() => expect(selected()).toBe(agentOptions().length - 1));
  expect(router.state.location.search).toHaveProperty("agent");

  fireEvent.keyDown(window, { key: "j" });
  await waitFor(() => expect(selected()).toBe(0));
  const needing = agentOptions().filter((row) => row.dataset.s === "needs");
  expect(needing.length).toBeGreaterThan(0);
  fireEvent.keyDown(window, { key: "n" });
  await waitFor(() => expect(needing[0]?.getAttribute("aria-selected")).toBe("true"));

  fireEvent.keyDown(window, { key: "k", metaKey: true });
  const before = selected();
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "j" });
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "n" });
  expect(selected()).toBe(before);
});

test("a digit taps the waiting agent's answer button, and a field keeps its digits", async () => {
  const posted = stubApi("default");
  // The screen stays connecting, so the stage keeps its place for the buttons.
  const api = globalThis.fetch;
  vi.stubGlobal("fetch", (input: RequestInfo | URL, init?: RequestInit) =>
    String(input).endsWith("/terminal") ? new Promise(() => {}) : api(input, init),
  );
  await renderApp("default", "/agents");
  await selectAgent("gamma-12");
  await screen.findByRole("button", { name: "Answer 2: No" });
  const answers = () => posted.filter(({ path }) => path.endsWith("/prompt/answer"));

  fireEvent.keyDown(window, { key: "k", metaKey: true });
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "2" });
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("combobox")).toBeNull());
  fireEvent.keyDown(window, { key: "3" });
  expect(answers()).toEqual([]);
  fireEvent.keyDown(window, { key: "2" });
  await waitFor(() =>
    expect(answers().map(({ body }) => body)).toEqual([{ option: 2, screen: "hash" }]),
  );
});

test("the button says it opens herdr on the remote machine first only for a remote agent", async () => {
  stubApi("default");
  await renderApp("default", "/agents");

  await selectAgent("alpha-42");
  expect(stageOpen().getAttribute("title")).toBe("Focus this agent's terminal in herdr");
  await selectAgent("alpha-43");
  expect(stageOpen().getAttribute("title")).toBe(
    "Focuses this agent's terminal on the remote machine; select that machine in herdr's sidebar to see it",
  );
});

test("Open in herdr asks the daemon to focus that agent, and says where to find the remote one", async () => {
  const posted = stubApi("default", 200, { opened: "raised", terminal: "Ghostty" });
  await renderApp("default", "/agents");

  await selectAgent("alpha-43");
  fireEvent.click(stageOpen());

  await waitFor(() => expect(posted).toHaveLength(1));
  const sessionId = agentRow("alpha-43").dataset.testid?.replace("agent-", "");
  expect(posted[0]).toMatchObject({ path: `/api/v1/agents/${sessionId}/focus`, method: "POST" });
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain("Brought herdr to the front"),
  );
  expect(screen.getByTestId("toast").textContent).toContain(
    "Select host-b in herdr's sidebar to see the agent's terminal.",
  );
});

function laneToggle(header: HTMLElement) {
  return within(header).getByRole("button", { expanded: true });
}

test("the lane header folds through one button around the chevron and name, and its actions stay beside it", async () => {
  stubApi("degraded");
  await renderApp("degraded");

  const lane = screen.getByTestId("lane-kiln");
  const header = lane.querySelector(".lane-h") as HTMLElement;
  // The bar is not itself a button: nothing interactive nests in another.
  expect(header.getAttribute("role")).toBeNull();
  expect(header.getAttribute("tabindex")).toBeNull();
  const toggle = within(header).getByRole("button", { name: "kiln" });
  expect(toggle.tagName).toBe("BUTTON");
  expect(toggle.getAttribute("aria-expanded")).toBe("true");
  expect(toggle.querySelector("button")).toBeNull();
  for (const name of ["New card", "Start orchestrator"]) {
    const action = within(header).getByRole("button", { name });
    expect(toggle.contains(action)).toBe(false);
  }

  // The button folds and unfolds the lane, once per click, and the mouse can
  // also click the bar around it.
  fireEvent.click(toggle);
  await waitFor(() => expect(lane.getAttribute("data-open")).toBe("false"));
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  fireEvent.click(toggle);
  await waitFor(() => expect(lane.getAttribute("data-open")).toBe("true"));
  fireEvent.click(header.querySelector(".c") as HTMLElement);
  await waitFor(() => expect(lane.getAttribute("data-open")).toBe("false"));
  // An action in the bar never folds it.
  fireEvent.click(within(header).getByRole("button", { name: "New card" }));
  await screen.findByRole("dialog", { name: "New card in kiln" });
  expect(lane.getAttribute("data-open")).toBe("false");
});

test("Start orchestrator on a lane with none posts the start and leaves the lane open", async () => {
  const posted = stubApi("degraded");
  await renderApp("degraded");

  const kiln = screen.getByTestId("lane-kiln");
  const header = kiln.querySelector(".lane-h") as HTMLElement;
  fireEvent.click(within(header).getByRole("button", { name: "Start orchestrator" }));

  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]).toMatchObject({ path: "/api/v1/projects/kiln/orchestrator", method: "POST" });
  expect(typeof posted[0]?.body.idempotency_key).toBe("string");
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain("Started the kiln orchestrator"),
  );
  expect(laneToggle(header).getAttribute("aria-expanded")).toBe("true");
  // A lane with a live orchestrator offers no start.
  const alpha = screen.getByTestId("lane-alpha").querySelector(".lane-h") as HTMLElement;
  expect(within(alpha).queryByRole("button", { name: "Start orchestrator" })).toBeNull();
});

test("New card files a card for its lane's project and never folds the lane", async () => {
  const posted = stubApi("degraded");
  await renderApp("degraded");

  const header = screen.getByTestId("lane-kiln").querySelector(".lane-h") as HTMLElement;
  fireEvent.click(within(header).getByRole("button", { name: "New card" }));
  const dialog = await screen.findByRole("dialog", { name: "New card in kiln" });
  const title = within(dialog).getByLabelText("Title");
  // The dialog sits outside the header's toggle: a space typed into it stays text.
  fireEvent.keyDown(title, { key: " " });
  fireEvent.change(title, { target: { value: "Fix the flaky upload" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "File card" }));

  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]).toMatchObject({
    path: "/api/v1/cards",
    method: "POST",
    body: { project: "kiln", kind: "ship", title: "Fix the flaky upload", priority: 2, body: null },
  });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(laneToggle(header).getAttribute("aria-expanded")).toBe("true");
});

test("no lane offers Start orchestrator while the agents cannot be read", async () => {
  stubApi("degraded");
  const served = globalThis.fetch;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
      String(input).startsWith("/api/v1/agents")
        ? new Response(JSON.stringify({ ok: false, error: { code: "internal", message: "x" } }), {
            status: 500,
          })
        : served(input, init),
    ),
  );
  await renderApp("degraded", "/", ["/api/v1/agents"]);

  await waitFor(() =>
    expect(screen.getByText(/Lanes show no orchestrator until it answers/)).toBeTruthy(),
  );
  expect(screen.queryByRole("button", { name: "Start orchestrator" })).toBeNull();
});

test("Open in herdr says when herdr opened, and when no herdr window here can show it", async () => {
  stubApi("default", 200, { opened: "launched", terminal: "Ghostty" });
  await renderApp("default", "/agents");

  await selectAgent("alpha-42");
  fireEvent.click(stageOpen());
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain("Opened herdr in Ghostty"),
  );

  stubApi("default", 200, { opened: "unseen", terminal: null });
  fireEvent.click(stageOpen());
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain("No herdr window here"),
  );
  expect(screen.getByTestId("toast").textContent).toContain(
    "Start herdr there, or open the board on the machine you are at.",
  );
  expect(screen.getByTestId("toast").textContent).not.toContain("Focused in herdr");
});

test("the O key opens the selected agent, and a letter typed into a field does not", async () => {
  const posted = stubApi("default");
  await renderApp("default", "/agents");

  await selectAgent("alpha-42");
  const sessionId = agentRow("alpha-42").dataset.testid?.replace("agent-", "");

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
  await selectAgent("alpha-42");
  fireEvent.click(stageOpen());
  await waitFor(() =>
    expect(screen.getByTestId("toast").textContent).toContain("herdr did not answer"),
  );
});

test("the I key opens the details with the focus command, branch, worktree, label and recent events", async () => {
  stubApi("default");
  await renderApp("default", "/agents");
  await selectAgent("alpha-42");

  const panel = screen.getByRole("complementary", { name: "Agent details" });
  expect(panel.dataset.open).toBe("false");
  fireEvent.keyDown(window, { key: "i" });
  await waitFor(() => expect(panel.dataset.open).toBe("true"));
  expect(panel.textContent).toContain("herdr agent focus alpha-42-build");
  expect(panel.textContent).toContain("ship/fixture");
  expect(panel.textContent).toContain("/home/user/work/fixture");
  expect(panel.textContent).toContain("alpha-42 build");
  expect(panel.textContent).toContain("pane.state working");
  fireEvent.keyDown(window, { key: "Escape" });
  await waitFor(() => expect(panel.dataset.open).toBe("false"));

  const card = within(document.querySelector(".av-acts") as HTMLElement).getByRole("link", {
    name: "Open card",
  });
  expect(card.getAttribute("href")).toBe(`/cards/${CARD_42}`);
});

test("a machine filter keeps only that machine's agents and redraws the tree", async () => {
  stubApi("default");
  await renderApp("default", `/agents?host=${HOST_B}`);
  expect(agentRow("alpha-43").classList.contains("last")).toBe(true);
  expect(within(agentList()).queryByText("alpha-42")).toBeNull();
  expect(within(agentList()).queryByRole("group", { name: "beta" })).toBeNull();
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
  expect(agentList()).toBeTruthy();
});

// ---- Card detail ----

test("a card on the board opens a docked drawer and the lanes stay on screen", async () => {
  stubApi("default");
  const { router } = await renderApp();
  fireEvent.click(screen.getByText("Move the poller behind a trait"));
  await waitFor(() => expect(router.state.location.search).toEqual({ card: CARD_43 }));
  expect(router.state.location.pathname).toBe("/");
  const drawer = await screen.findByTestId("card-drawer");
  expect(within(drawer).getByTestId("card-page")).toBeTruthy();
  expect(screen.getByTestId("board")).toBeTruthy();
  expect(document.querySelector(".board-split")?.getAttribute("data-docked")).toBe("true");
  // Another card swaps the drawer's card in place.
  fireEvent.click(screen.getByText("Add the machines summary to the sidebar"));
  await waitFor(() => expect(router.state.location.search).toEqual({ card: CARD_42 }));
});

test("the drawer expands to the full page, and Back returns to the board with the drawer open", async () => {
  stubApi("default");
  const { router } = await renderApp("default", `/?card=${CARD_43}`);
  const drawer = await screen.findByTestId("card-drawer");
  fireEvent.click(within(drawer).getByRole("link", { name: "Open the full page" }));
  await waitFor(() => expect(router.state.location.pathname).toBe(`/cards/${CARD_43}`));
  expect(screen.queryByTestId("card-drawer")).toBeNull();
  fireEvent.click(within(screen.getByTestId("card-page")).getByRole("link", { name: /Board/ }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/"));
  expect(router.state.location.search).toEqual({ card: CARD_43 });
  expect(await screen.findByTestId("card-drawer")).toBeTruthy();
});

test("Close and Escape shut the drawer", async () => {
  stubApi("default");
  const { router } = await renderApp("default", `/?card=${CARD_43}`);
  const drawer = await screen.findByTestId("card-drawer");
  fireEvent.click(within(drawer).getByRole("link", { name: "Close" }));
  await waitFor(() => expect(screen.queryByTestId("card-drawer")).toBeNull());
  expect(router.state.location.search).toEqual({});

  await act(async () => {
    await router.navigate({ to: "/", search: { card: CARD_41 } });
  });
  await screen.findByTestId("card-drawer");
  fireEvent.keyDown(window, { key: "Escape" });
  await waitFor(() => expect(screen.queryByTestId("card-drawer")).toBeNull());
});

test("a card that cannot be read leaves the board up and the drawer says so", async () => {
  stubApi("default");
  await renderApp("default", "/?card=does-not-exist");
  expect(await screen.findByText("This card could not be loaded")).toBeTruthy();
  expect(screen.getByTestId("board")).toBeTruthy();
});

test("a stale approval is called out with the commits that came after it", async () => {
  stubApi("default");
  await renderApp("default", `/cards/${CARD_43}`);

  const page = screen.getByTestId("card-page");
  expect(within(page).getByText(/Approval is stale/)).toBeTruthy();
  // The pull request and its approval are one card.
  expect(page.querySelector(".appr")).toBeNull();
  const box = page.querySelector<HTMLElement>(".prc");
  expect(box?.dataset.standing).toBe("stale");
  expect(box?.textContent).toContain("stale");
  expect(box?.textContent).toContain("undo: coalesce on pointerup");
  // The bound commit is struck through; the head it moved to is highlighted.
  expect(page.querySelector(".prc .sha.old")?.textContent).toHaveLength(7);
  expect(page.querySelectorAll(".prc .sha.new").length).toBeGreaterThan(0);
  expect(page.textContent).toContain("+2 commits since approval");
  // Nothing to decide on a stale approval.
  expect(within(box as HTMLElement).queryByRole("button", { name: "Approve" })).toBeNull();
});

test("the page lists runs, the PR with its head, events and how fresh each signal is", async () => {
  stubApi("default");
  await renderApp("default", `/cards/${CARD_43}`);
  const page = screen.getByTestId("card-page");

  expect(page.textContent).toContain("Attempt 1");
  expect(page.textContent).toContain("Attempt 2");
  expect(page.textContent).toContain("failed");
  expect(within(page).getByRole("link", { name: "example/alpha#52" })).toHaveProperty(
    "href",
    "https://github.com/example/alpha/pull/52",
  );
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

test("a blocked card page names its blockers once and a held card says Held", async () => {
  stubApi("blocked");
  await renderApp("blocked", "/cards/01JCARD0000000000000000014");
  const blocked = screen.getByTestId("card-page");
  expect(blocked.textContent?.match(/blocked by gamma-12/gi)).toHaveLength(1);
  expect(blocked.textContent).toContain("Blocked by gamma-12");
  cleanup();

  await renderApp("blocked", "/cards/01JCARD0000000000000000013");
  const held = await screen.findByTestId("card-page");
  expect(held.textContent).toContain("Held");
  expect(held.textContent).not.toMatch(/parked/i);
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

test("the slug shows whole words, never a title cut mid-word", async () => {
  stubApi("default");
  await renderApp("default", `/cards/${CARD_43}`);
  const page = screen.getByTestId("card-page");
  const dt = [...page.querySelectorAll(".props dt")].find((node) => node.textContent === "Slug");
  const slug = dt?.nextElementSibling?.textContent ?? "";
  const title = page.querySelector("h4")?.textContent ?? "";
  const words = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  expect(slug.length).toBeGreaterThan(0);
  const tail = slug.slice(slug.indexOf("-", slug.indexOf("-") + 1) + 1);
  expect(`${words}-`.startsWith(`${tail}-`)).toBe(true);
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

// ---- Project stage ----

function projectsRegion() {
  return screen.getByRole("region", { name: "Projects" });
}

test("a pre-launch project shows its chip and, opened, its review facts", async () => {
  stubApi("default");
  const { queryClient } = await renderApp();

  const sidebar = projectsRegion();
  expect(within(sidebar).getAllByText("pre-launch")).toHaveLength(1);
  expect(within(sidebar).queryByText("final review")).toBeNull();

  fireEvent.click(within(sidebar).getByRole("button", { name: "Stage of alpha" }));
  const panel = within(sidebar).getByRole("group", { name: "alpha stage" });
  expect(panel.textContent).toContain("9c1e4f7a02b8");
  expect(panel.textContent).not.toContain("9c1e4f7a02b86");
  expect(panel.textContent).toContain("Sat 31 Oct 07:12");
  expect(panel.textContent).toContain("Sat 7 Nov 06:00");
  expect(panel.textContent).toContain("direct-pr");
  expect(panel.textContent).not.toContain("review overdue");
  expect(within(panel).getByRole("button", { name: "Switch to launched" })).toBeTruthy();

  const projects = queryClient.getQueryData<ProjectsResponse>(queryKeyFor(projectsPath));
  const alpha = projects?.projects.find((project) => project.id === "alpha") as Project;
  act(() => {
    applyChange(queryClient, {
      seq: 5001,
      at: "2026-11-08T12:00:00.000Z",
      kind: "project.changed",
      txn: "x",
      entity: "project",
      entity_id: "alpha",
      rev_after: 2,
      card_id: null,
      entity_state: { ...alpha, rev: 2, review_overdue: true },
    } satisfies ChangeEvent);
  });
  await waitFor(() => expect(panel.textContent).toContain("review overdue"));
});

test("a launched project shows no chip, except while its final review runs", async () => {
  stubApi("busy");
  await renderApp("busy");

  const sidebar = projectsRegion();
  const rowOf = (name: string) => within(sidebar).getByTitle(`Focus ${name}`);
  expect(within(rowOf("beta")).queryByText(/pre-launch|final review/)).toBeNull();
  expect(within(rowOf("delta")).getByText("final review")).toBeTruthy();

  fireEvent.click(within(sidebar).getByRole("button", { name: "Stage of delta" }));
  const panel = within(sidebar).getByRole("group", { name: "delta stage" });
  expect(panel.textContent).toContain("final review running");
  expect(within(panel).getByRole("button", { name: "Switch to pre-launch" })).toBeTruthy();
});

test("switching stage asks first, says what changes, and sends the rev it read", async () => {
  const posted = stubApi("default");
  await renderApp();

  fireEvent.click(within(projectsRegion()).getByRole("button", { name: "Stage of alpha" }));
  fireEvent.click(screen.getByRole("button", { name: "Switch to launched" }));
  const dialog = screen.getByRole("dialog", { name: "Switch alpha to launched?" });
  expect(dialog.textContent).toContain(
    "Validation is back on for every push from now on. A final review of everything since the last reviewed commit starts next. Open PRs with no pipeline run must go back through the gate before they merge.",
  );
  expect(posted).toHaveLength(0);

  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(posted).toHaveLength(0);

  fireEvent.click(screen.getByRole("button", { name: "Switch to launched" }));
  fireEvent.click(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Switch to launched" }),
  );
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe("/api/v1/projects/alpha/stage");
  expect(posted[0]?.method).toBe("POST");
  expect(posted[0]?.body).toMatchObject({ rev: 1, stage: "launched" });
  expect(typeof posted[0]?.body.idempotency_key).toBe("string");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

// ---- Project delete ----

test("deleting a project shows what goes and arms only once the id is typed", async () => {
  const posted = stubApi("default");
  await renderApp();

  fireEvent.click(within(projectsRegion()).getByRole("button", { name: "Stage of gamma" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  const dialog = screen.getByRole("dialog", { name: "Delete gamma?" });
  expect(dialog.textContent).toContain(
    "The checkout, its worktrees and the GitHub repository are left as they are.",
  );
  const counts = await within(dialog).findByRole("list", { name: "Rows removed" });
  expect(counts.textContent).toContain("cards2");
  expect(counts.textContent).toContain("events24");
  expect(counts.textContent).toContain("memory revisions4");
  expect(dialog.textContent).toContain("1 link with another project's records is cleared.");

  const confirm = within(dialog).getByRole("button", { name: /^Stop \d+ agents? and delete$/ });
  const typed = within(dialog).getByLabelText("Type gamma to confirm");
  expect(confirm).toHaveProperty("disabled", true);
  fireEvent.change(typed, { target: { value: "gam" } });
  expect(confirm).toHaveProperty("disabled", true);
  fireEvent.click(confirm);
  expect(posted).toHaveLength(0);

  fireEvent.change(typed, { target: { value: "gamma" } });
  expect(confirm).toHaveProperty("disabled", false);
  fireEvent.click(confirm);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe("/api/v1/projects/gamma/delete");
  expect(posted[0]?.method).toBe("POST");
  expect(posted[0]?.body).toMatchObject({ confirm: "gamma" });
  expect(typeof posted[0]?.body.idempotency_key).toBe("string");
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

test("the fleet project has no archive or delete control", () => {
  const projects = fixtureResource(projectsPath) as ProjectsResponse;
  const gamma = projects.projects.find((project) => project.id === "gamma");
  if (!gamma) throw new Error("the fixtures lost gamma");
  const fleet = { ...gamma, id: "fleet", name: "Fleet" };
  render(
    <>
      <ArchiveProject project={fleet} owner={undefined} />
      <DeleteProject project={fleet} owner={undefined} />
    </>,
  );
  expect(screen.queryByRole("button")).toBeNull();
});

test("the fleet project offers no stage switch and no checkout setting", () => {
  const projects = fixtureResource(projectsPath) as ProjectsResponse;
  const gamma = projects.projects.find((project) => project.id === "gamma");
  if (!gamma) throw new Error("the fixtures lost gamma");
  const fleet = { ...gamma, id: "fleet", name: "Fleet" };
  const { container } = render(<ProjectDetail project={fleet} id="fleet-detail" />);
  expect(screen.queryByRole("button")).toBeNull();
  expect(container.textContent).not.toContain("Checkout");
  expect(container.textContent).toContain("Builders follow");
});

test("deleting a project with a working card lists the agents it stops and the work it leaves", async () => {
  const posted = stubApi("default");
  await renderApp();

  fireEvent.click(within(projectsRegion()).getByRole("button", { name: "Stage of alpha" }));
  fireEvent.click(screen.getByRole("button", { name: "Delete" }));
  const dialog = screen.getByRole("dialog", { name: "Delete alpha?" });
  const stops = await within(dialog).findByRole("region", { name: "Stopped first" });
  const agents = within(stops)
    .getAllByRole("listitem")
    .map((item) => item.textContent);
  expect(agents[0]).toBe("orchestrator");
  expect(agents.slice(1).every((agent) => /^builder on alpha-\d+$/.test(agent ?? ""))).toBe(true);
  const unlanded = within(dialog).getByRole("region", { name: "Not landed" });
  expect(unlanded.textContent).toMatch(/\/home\/user\/worktrees\/alpha-\d+/);
  expect(within(dialog).getByRole("region", { name: "Open pull requests" })).toBeTruthy();
  expect(within(dialog).queryByRole("alert")).toBeNull();

  const confirm = within(dialog).getByRole("button", {
    name: `Stop ${agents.length} agents and delete`,
  });
  expect(confirm).toHaveProperty("disabled", true);
  fireEvent.change(within(dialog).getByLabelText("Type alpha to confirm"), {
    target: { value: "alpha" },
  });
  fireEvent.click(confirm);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe("/api/v1/projects/alpha/delete");
});

// ---- Project add ----

test("adding a project closes the dialog, or keeps it open to show the add's warning", async () => {
  const warning =
    "cards continue at alpha-9: the deleted project's run branches are left (tiller/alpha-8-build)";
  const posted = stubApi("default");
  const fixtures = globalThis.fetch;
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    const reply = await fixtures(input, init);
    if (String(input) !== "/api/v1/projects" || init?.method !== "POST") return reply;
    const body = (await reply.json()) as Record<string, unknown>;
    const id = posted.at(-1)?.body.id;
    return new Response(JSON.stringify(id === "alpha" ? { ...body, warning } : body), {
      status: 200,
    });
  });
  await renderApp();

  // delta's checkout is set later; alpha uses the checkout already there.
  const add = async (id: string, mode: "Later" | "Existing folder") => {
    fireEvent.click(screen.getByRole("button", { name: "Add project" }));
    const dialog = screen.getByRole("dialog", { name: "Add project" });
    expect(within(dialog).queryByLabelText("Name")).toBeNull();
    expect(dialog.textContent).not.toContain("config set checkouts.<id> before");
    fireEvent.change(within(dialog).getByLabelText("Id"), { target: { value: id } });
    fireEvent.click(within(dialog).getByRole("button", { name: mode }));
    const submit = within(dialog).getByRole("button", { name: "Add project" });
    await waitFor(() => expect((submit as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(submit);
    await waitFor(() => expect(posted.at(-1)?.body.id).toBe(id));
  };

  await add("delta", "Later");
  expect(posted.at(-1)?.path).toBe("/api/v1/projects");
  expect(posted.at(-1)?.body).not.toHaveProperty("name");
  expect(posted.at(-1)?.body.checkout).toBeNull();
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

  await add("alpha", "Existing folder");
  expect(posted.at(-1)?.body.checkout).toEqual({ path: "", clone: false });
  const added = await screen.findByRole("dialog", { name: "Project added" });
  expect(within(added).getByRole("status").textContent).toBe(warning);
  const done = within(added).getByRole("button", { name: "Done" });
  await waitFor(() => expect(document.activeElement).toBe(done));
  fireEvent.click(done);
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("Add project checks the local checkout on the daemon and says what it will do", async () => {
  const posted = stubApi("default");
  await renderApp();

  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  const dialog = screen.getByRole("dialog", { name: "Add project" });
  const submit = within(dialog).getByRole("button", { name: "Add project" }) as HTMLButtonElement;
  const folder = within(dialog).getByLabelText("Local checkout");
  const verdict = () => document.getElementById(folder.getAttribute("aria-describedby") ?? "");
  fireEvent.change(within(dialog).getByLabelText("Id"), { target: { value: "delta" } });

  // No checkout at the default folder yet: the daemon says to clone.
  await waitFor(() =>
    expect(verdict()?.textContent).toBe(
      "nothing is at /home/user/src/delta; choose Clone to make it",
    ),
  );
  expect(verdict()?.dataset.problem).toBe("true");
  expect(submit.disabled).toBe(true);

  // A clone needs the repo, and a folder under the checkouts root.
  fireEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  await waitFor(() =>
    expect(verdict()?.textContent).toBe(
      "a clone needs the project's repo; fill in Repo as owner/name",
    ),
  );
  fireEvent.change(within(dialog).getByLabelText(/^Repo/), { target: { value: "example/delta" } });
  fireEvent.change(folder, { target: { value: "/srv/delta" } });
  await waitFor(() => expect(verdict()?.textContent).toMatch(/^\/srv\/delta is not under/));
  expect(submit.disabled).toBe(true);
  expect(folder.getAttribute("aria-invalid")).toBe("true");

  fireEvent.change(folder, { target: { value: "/home/user/src/delta" } });
  await waitFor(() =>
    expect(verdict()?.textContent).toBe(
      "Clones https://github.com/example/delta.git into /home/user/src/delta.",
    ),
  );
  expect(verdict()?.dataset.problem).toBeUndefined();
  await waitFor(() => expect(submit.disabled).toBe(false));
  fireEvent.click(submit);
  await waitFor(() => expect(posted.at(-1)?.path).toBe("/api/v1/projects"));
  expect(posted.at(-1)?.body).toMatchObject({
    id: "delta",
    repo: "example/delta",
    checkout: { path: "/home/user/src/delta", clone: true },
  });
});

test("Add project locks the Local checkout choice while the add is sent", async () => {
  const posted = stubApi("default");
  const fixtures = globalThis.fetch;
  let answer: (() => void) | undefined;
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    if (String(input) === "/api/v1/projects" && init?.method === "POST") {
      await new Promise<void>((resolve) => {
        answer = resolve;
      });
    }
    return fixtures(input, init);
  });
  await renderApp();

  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  const dialog = screen.getByRole("dialog", { name: "Add project" });
  fireEvent.change(within(dialog).getByLabelText("Id"), { target: { value: "delta" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Later" }));
  const submit = within(dialog).getByRole("button", { name: "Add project" }) as HTMLButtonElement;
  await waitFor(() => expect(submit.disabled).toBe(false));
  fireEvent.click(submit);

  const modes = within(within(dialog).getByRole("group", { name: "Where the checkout comes from" }))
    .getAllByRole("button")
    .map((button) => button as HTMLButtonElement);
  await waitFor(() => expect(modes.map((button) => button.disabled)).toEqual([true, true, true]));
  fireEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  expect(within(dialog).getByRole("button", { name: "Later" }).getAttribute("aria-pressed")).toBe(
    "true",
  );
  answer?.();
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(posted.at(-1)?.body.checkout).toBeNull();
});

// ---- Project archive ----

test("archiving a project with an open card says why and offers the delete", async () => {
  const posted = stubApi("default");
  await renderApp();

  fireEvent.click(within(projectsRegion()).getByRole("button", { name: "Stage of alpha" }));
  fireEvent.click(screen.getByRole("button", { name: "Archive" }));
  const dialog = screen.getByRole("dialog", { name: "Archive alpha?" });
  const why = await within(dialog).findByRole("alert");
  expect(why.textContent).toMatch(
    /^Archiving needs its open cards finished or cancelled first: card alpha-\d+ is open\.$/,
  );
  // No archive or stop to press while a card is open.
  expect(within(dialog).queryByRole("button", { name: /Archive|Stop/ })).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Delete instead" }));
  expect(screen.queryByRole("dialog", { name: "Archive alpha?" })).toBeNull();
  const deleting = screen.getByRole("dialog", { name: "Delete alpha?" });
  fireEvent.keyDown(deleting, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(posted).toHaveLength(0);
});

// kiln's archive preview with its orchestrator running, here or on another
// host. kiln has no open card, so only the agent stands in the way.
function kilnRunning(local: boolean) {
  const fixtures = globalThis.fetch;
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    if (String(input) !== "/api/v1/projects/kiln/retire" || init?.method) {
      return fixtures(input, init);
    }
    const preview = (await (await fixtures(input, init)).json()) as ProjectRetirePreview;
    const agents = [{ role: "orchestrator", session: "s-kiln", card: null, run: null, local }];
    return new Response(JSON.stringify({ ...preview, agents }), { status: 200 });
  });
}

test("archiving a project with a running agent asks whether to stop it", async () => {
  const posted = stubApi("degraded");
  kilnRunning(true);
  await renderApp("degraded");

  fireEvent.click(within(projectsRegion()).getByRole("button", { name: "Stage of kiln" }));
  fireEvent.click(screen.getByRole("button", { name: "Archive" }));
  const dialog = screen.getByRole("dialog", { name: "Archive kiln?" });
  const running = await within(dialog).findByRole("region", { name: "Still running" });
  expect(running.textContent).toBe("Still runningorchestrator");
  expect(within(dialog).getByRole("button", { name: "Archive, keep running" })).toBeTruthy();
  expect(within(dialog).getByText(/^Stop the orchestrator now/)).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Stop and archive" }));
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe("/api/v1/projects/kiln/retire");
  expect(posted[0]?.body).toMatchObject({ stop_agents: true });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

test("archiving with an agent on another host offers no stop", async () => {
  const posted = stubApi("degraded");
  kilnRunning(false);
  await renderApp("degraded");

  fireEvent.click(within(projectsRegion()).getByRole("button", { name: "Stage of kiln" }));
  fireEvent.click(screen.getByRole("button", { name: "Archive" }));
  const dialog = screen.getByRole("dialog", { name: "Archive kiln?" });
  await within(dialog).findByRole("region", { name: "Still running" });
  expect(within(dialog).queryByRole("button", { name: /^Stop / })).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Archive, keep running" }));
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.body).toMatchObject({ stop_agents: false });
});

test("the switch to pre-launch says validation is skipped and a Saturday review runs", async () => {
  stubApi("default");
  await renderApp();

  fireEvent.click(within(projectsRegion()).getByRole("button", { name: "Stage of beta" }));
  fireEvent.click(screen.getByRole("button", { name: "Switch to pre-launch" }));
  expect(screen.getByRole("dialog").textContent).toContain(
    "Per-change validation (no-mistakes) is skipped and a batched review runs every Saturday at 06:00 Lisbon time. Grants skip their pipeline condition for a PR with no pipeline run while pre-launch; every other condition still applies.",
  );
});

test("a PR waiting on the owner is decided in the PR card, Reject then Approve on the right", async () => {
  const posted = stubApi("default");
  await renderApp("default", `/cards/${CARD_44}`);
  const page = screen.getByTestId("card-page");
  const box = page.querySelector<HTMLElement>(".prc");
  expect(box?.dataset.standing).toBe("pending");
  expect(page.querySelector(".appr")).toBeNull();
  expect(
    within(box as HTMLElement)
      .getAllByRole("button")
      .map((button) => button.textContent),
  ).toEqual(["Reject", `Approve ${FIXTURE_HEAD_SHA.slice(0, 7)}`]);
  fireEvent.click(
    within(page).getByRole("button", { name: `Approve ${FIXTURE_HEAD_SHA.slice(0, 7)}` }),
  );
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]).toMatchObject({
    path: "/api/v1/approvals/01JAPPR0000000000000000000/decide",
    body: { decision: "accept", head_sha: FIXTURE_HEAD_SHA },
  });
});

test("Reject needs no reason and is bound to the head the page shows", async () => {
  const posted = stubApi("default");
  await renderApp("default", `/cards/${CARD_44}`);
  fireEvent.click(screen.getByRole("button", { name: "Reject" }));
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.body).toMatchObject({ decision: "reject", head_sha: FIXTURE_HEAD_SHA });
  expect(Object.keys(posted[0]?.body ?? {})).not.toContain("reason");
});
