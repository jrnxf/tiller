import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import { changeSession, resetLifecycleFixture } from "../../lib/fixtures-lifecycle";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { ActionError } from "../attention/api";
import { resetBoardLook } from "./board-prefs";
import { stopRefusal } from "./run-menu";
import { NO_RUN } from "./steer-bar";

// The `lifecycle` fixture: beta-7's run is paused, alpha-44's pane is gone,
// alpha-42 works on its run, and gamma's orchestrator has gone silent.
const BETA_7 = "01JSESSWBETA70000000000000";
const ALPHA_44 = "01JSESSWALPHA4400000000000";
const ALPHA_42 = "01JSESSWALPHA4200000000000";
const GAMMA = "01JSESSOGAMMA0000000000000";

type Posted = { path: string; body: Record<string, unknown> };

// The dev server's fixture daemon, as the board's fetch sees it.
function stubDaemon(): Posted[] {
  const posted: Posted[] = [];
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (/\/terminal(\/tail)?$/.test(path)) return new Response("{}", { status: 404 });
    if (init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      posted.push({ path, body });
      const { reply } = fixtureMutation("POST", path, body, "lifecycle");
      return new Response(JSON.stringify(reply.body), { status: reply.status });
    }
    const body = fixtureResource(path, "lifecycle");
    return new Response(JSON.stringify(body ?? {}), { status: body === undefined ? 404 : 200 });
  });
  return posted;
}

async function renderApp(path: string) {
  const queryClient = createQueryClient();
  queryClient.setDefaultOptions({
    queries: { ...queryClient.getDefaultOptions().queries, retryDelay: 0 },
  });
  hydrateFromSnapshot(queryClient, fixtureSnapshot("lifecycle"));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

// The Stage of one agent, once its header shows.
async function stageOf(session: string, name: string) {
  await renderApp(`/agents?agent=${session}`);
  await screen.findByRole("form", { name: `Message ${name}` });
  const acts = document.querySelector<HTMLElement>(".av-acts");
  if (!acts) throw new Error("no agent on the stage");
  return acts;
}

async function openRunMenu(scope: HTMLElement) {
  fireEvent.click(within(scope).getByRole("button", { name: "Run actions" }));
  return screen.findByRole("menu", { name: "Run actions" });
}

function itemsOf(menu: HTMLElement): string[] {
  return within(menu)
    .getAllByRole("menuitem")
    .map((item) => item.textContent ?? "");
}

function toast(): string {
  return screen.queryByTestId("toast")?.textContent ?? "";
}

// The names in the agents list, top to bottom.
function rows(): (string | null)[] {
  return [...document.querySelectorAll(".av-row .nm .l")].map((name) => name.textContent);
}

// A stop refusal with the daemon's message, in the board's words.
function refusal(message: string): string {
  return stopRefusal(new ActionError(422, "refused", message));
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  resetBoardLook();
  resetLifecycleFixture();
  receiptsStore.set({ byKey: {}, progress: {} });
  connectionStore.set("live");
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

test("a silent orchestrator restarts from its lane behind a confirmation, and the lane stays open", async () => {
  const posted = stubDaemon();
  await renderApp("/");
  const lane = await screen.findByTestId("lane-gamma");
  const header = lane.querySelector(".lane-h") as HTMLElement;
  fireEvent.click(within(header).getByRole("button", { name: "Restart..." }));
  const dialog = await screen.findByRole("dialog", { name: "Restart the gamma orchestrator?" });
  expect(dialog.textContent).toContain("The silent session is stopped");
  expect(posted).toEqual([]);
  // A click inside the dialog never reaches the lane bar, which would fold it.
  fireEvent.click(within(dialog).getByRole("button", { name: "Restart" }));
  await waitFor(() => expect(toast()).toContain("Restarted the gamma orchestrator"));
  expect(lane.getAttribute("data-open")).toBe("true");
  expect(posted).toHaveLength(1);
  expect(posted[0]?.path).toBe("/api/v1/projects/gamma/orchestrator");
  expect(posted[0]?.body.restart).toBe(true);
  // The new orchestrator is live, so the lane no longer offers a restart.
  await waitFor(() =>
    expect(within(header).queryByRole("button", { name: "Restart..." })).toBeNull(),
  );
});

test("Cancel on the lane restart sends nothing and leaves the lane open", async () => {
  const posted = stubDaemon();
  await renderApp("/");
  const lane = await screen.findByTestId("lane-gamma");
  const header = lane.querySelector(".lane-h") as HTMLElement;
  fireEvent.click(within(header).getByRole("button", { name: "Restart..." }));
  const dialog = await screen.findByRole("dialog", { name: "Restart the gamma orchestrator?" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(lane.getAttribute("data-open")).toBe("true");
  expect(posted).toEqual([]);
});

test("a paused run's primary is Resume, which sends one go on line", async () => {
  const posted = stubDaemon();
  const acts = await stageOf(BETA_7, "beta-7");
  const note = screen.getByText(/^Paused until /).closest<HTMLElement>(".run-paused");
  expect(note?.textContent).toContain("Waiting for the vendor's new bucket.");
  expect(note?.textContent).toContain("Paused by you");
  const buttons = within(acts).getAllByRole("button");
  const resume = buttons.at(-1) as HTMLButtonElement;
  expect(resume.textContent).toBe("Resume");
  expect(itemsOf(await openRunMenu(acts))).toEqual([
    "Pause until...",
    "Relaunch...",
    "Open in herdr",
    "Stop agent...",
  ]);
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  fireEvent.click(resume);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(posted[0]?.path).toBe(`/api/v1/agents/${BETA_7}/steer`);
  expect(posted[0]?.body.text).toBe("go on");
  await waitFor(() => expect(resume.disabled).toBe(true));
});

test("one agent's Resume does not hold the next paused agent's Resume", async () => {
  changeSession(ALPHA_42, {
    activity: "idle",
    pause: {
      until: "2099-01-01T09:00:00Z",
      at: "2026-11-11T10:00:00Z",
      by: "board",
      reason: "Wait.",
    },
  });
  const posted = stubDaemon();
  const acts = await stageOf(BETA_7, "beta-7");
  const first = within(acts).getAllByRole("button").at(-1) as HTMLButtonElement;
  fireEvent.click(first);
  await waitFor(() => expect(first.disabled).toBe(true));
  const row = [...document.querySelectorAll<HTMLElement>(".av-row")].find(
    (item) => item.querySelector(".nm .l")?.textContent === "alpha-42",
  );
  if (!row) throw new Error("no alpha-42 row");
  fireEvent.click(row);
  await screen.findByRole("form", { name: "Message alpha-42" });
  const next = within(document.querySelector<HTMLElement>(".av-acts") as HTMLElement)
    .getAllByRole("button")
    .at(-1) as HTMLButtonElement;
  expect(next.textContent).toBe("Resume");
  expect(next.disabled).toBe(false);
  fireEvent.click(next);
  await waitFor(() => expect(posted).toHaveLength(2));
  expect(posted[1]?.path).toBe(`/api/v1/agents/${ALPHA_42}/steer`);
});

test("Pause until asks for a reason and pauses the run", async () => {
  const posted = stubDaemon();
  const acts = await stageOf(ALPHA_42, "alpha-42");
  const menu = await openRunMenu(acts);
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Pause until..." }));
  const dialog = await screen.findByRole("dialog", { name: "Pause alpha-42" });
  const pause = within(dialog).getByRole("button", { name: "Pause" }) as HTMLButtonElement;
  expect(pause.disabled).toBe(true);
  fireEvent.change(within(dialog).getByLabelText("Reason"), {
    target: { value: "Waiting on the release." },
  });
  expect(pause.disabled).toBe(false);
  fireEvent.click(pause);
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(posted[0]?.path).toBe(`/api/v1/agents/${ALPHA_42}/pause`);
  expect(posted[0]?.body.reason).toBe("Waiting on the release.");
  expect(Date.parse(String(posted[0]?.body.until))).toBeGreaterThan(Date.now());
});

test("a dead worker's primary is Relaunch, which needs a note first", async () => {
  const posted = stubDaemon();
  const acts = await stageOf(ALPHA_44, "alpha-44");
  const relaunch = within(acts).getAllByRole("button").at(-1) as HTMLButtonElement;
  expect(relaunch.textContent).toBe("Relaunch...");
  expect(itemsOf(await openRunMenu(acts))).toContain("Resume session");
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  fireEvent.click(relaunch);
  const dialog = await screen.findByRole("dialog", { name: "Relaunch alpha-44" });
  const go = within(dialog).getByRole("button", { name: "Relaunch" }) as HTMLButtonElement;
  expect(go.disabled).toBe(true);
  fireEvent.change(within(dialog).getByLabelText("Note"), {
    target: { value: "The tests pass; open the PR next." },
  });
  fireEvent.click(go);
  await waitFor(() => expect(toast()).toContain("Relaunched alpha-44"));
  expect(posted[0]).toMatchObject({
    path: `/api/v1/agents/${ALPHA_44}/relaunch`,
    body: { note: "The tests pass; open the PR next.", new_attempt: false },
  });
  await waitFor(() =>
    expect(within(acts).getAllByRole("button").at(-1)?.textContent).toMatch(/^Open in herdr/),
  );
});

test("Stop on a worker whose run is open says why and sends nothing", async () => {
  const posted = stubDaemon();
  const acts = await stageOf(ALPHA_42, "alpha-42");
  const menu = await openRunMenu(acts);
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Stop agent..." }));
  const dialog = await screen.findByRole("dialog", { name: "alpha-42 is still on its run" });
  expect(dialog.textContent).toContain("Cancel card on the card page");
  expect(within(dialog).queryByRole("button", { name: "Stop agent" })).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Close" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(posted).toHaveLength(0);
});

test("an orchestrator has no run: its menu holds Restart and Stop, and Interrupt is off", async () => {
  const posted = stubDaemon();
  const acts = await stageOf(GAMMA, "gamma");
  const interrupt = screen.getByRole("button", { name: "Interrupt..." }) as HTMLButtonElement;
  expect(interrupt.disabled).toBe(true);
  expect(interrupt.title).toBe(NO_RUN);
  const menu = await openRunMenu(acts);
  expect(itemsOf(menu)).toEqual(["Restart...", "Stop agent..."]);
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Stop agent..." }));
  const dialog = await screen.findByRole("dialog", { name: "Stop gamma?" });
  const stop = within(dialog).getByRole("button", { name: "Stop agent" });
  fireEvent.click(stop);
  expect((await within(dialog).findByRole("alert")).textContent).toMatch(
    /^Not stopped: the pane has an unsent draft\./,
  );
  fireEvent.click(stop);
  await waitFor(() => expect(toast()).toContain("Stopped gamma"));
  // One press is one key, kept for the retry after the refusal.
  expect(posted.map(({ path }) => path)).toEqual([
    `/api/v1/agents/${GAMMA}/stop`,
    `/api/v1/agents/${GAMMA}/stop`,
  ]);
  expect(posted[0]?.body.idempotency_key).toBe(posted[1]?.body.idempotency_key);
  // Its builder keeps working, listed on its own.
  await waitFor(() => expect(rows()).not.toContain("gamma"));
  expect(rows()).toContain("gamma-12");
});

test("a refused stop says what to do next where the board can, and the daemon's words otherwise", () => {
  expect(
    refusal('session s: the composer of pane p1 holds a draft ("x"); send or clear it first'),
  ).toMatch(/^Not stopped: the pane has an unsent draft\./);
  expect(refusal("session s: pane p1 shows a dialog; answer it first")).toBe(
    "Not stopped: the agent is waiting on a dialog. Answer it first.",
  );
  expect(
    refusal(
      "session s: run r1 is active; end it first with tiller done r1 --outcome <outcome>, then stop its agent; nothing was sent",
    ),
  ).toMatch(/^Not stopped: its run is still open\./);
  expect(refusal("herdr is down")).toBe("herdr is down");
});
