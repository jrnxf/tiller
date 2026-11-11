import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { Snapshot } from "../../lib/api-types";
import { hydrateFromSnapshot, loadBoot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureSnapshot } from "../../lib/fixtures";
import { prefsStore, loadPrefs } from "../../lib/prefs";
import { ReadError } from "../../lib/queries";
import { createAppRouter } from "../../lib/router";
import { SETUP_POLL_MS, SetupScreen } from "./setup-screen";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

const UNINITIALIZED = {
  ok: false,
  error: {
    code: "unavailable",
    message:
      "unavailable: state.db is not initialized; run tiller owner set tiller, then tiller init",
    hint: "tiller owner set tiller\ntiller init --host-label <label> --lane-capacity <n>",
  },
};

// A daemon that answers from `answers` and 503s every other read, the way
// it does before `tiller init`.
function stubDaemon(answers: Record<string, unknown>) {
  const fetch = vi.fn((path: string) => {
    const body = answers[path];
    return Promise.resolve(
      body === undefined
        ? new Response(JSON.stringify(UNINITIALIZED), { status: 503 })
        : new Response(JSON.stringify(body), { status: 200 }),
    );
  });
  vi.stubGlobal("fetch", fetch);
  return fetch;
}

// The boot snapshot without the reads in `missing`, so the board has to
// fetch them.
function snapshotWithout(...missing: string[]): Snapshot {
  const snapshot = fixtureSnapshot("default");
  const queries = { ...snapshot.queries };
  for (const path of missing) delete queries[path];
  return { ...snapshot, queries };
}

async function renderApp(snapshot: Snapshot, path = "/") {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, snapshot);
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return router;
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  connectionStore.set("live");
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
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

test("a 503 from the agents read degrades what needs agents and nothing else", async () => {
  stubDaemon({});
  await renderApp(snapshotWithout("/api/v1/agents"));

  expect(screen.queryByText("Something went wrong!")).toBeNull();
  // The shell is whole: screens, projects, machines, quota.
  const nav = screen.getByRole("navigation", { name: "Screens" });
  expect(within(nav).getByText("Board").parentElement?.querySelector(".count")).not.toBeNull();
  expect(within(nav).getByText("Agents").parentElement?.querySelector(".count")).toBeNull();
  expect(screen.getByTestId("machine-host-a").textContent).toContain("3 running, 2 free");
  expect(screen.getByTestId("quota-footer").textContent).toContain("27% week left");
  expect(screen.getByRole("heading", { level: 1, name: "Board" })).toBeTruthy();
  // The board keeps its cards and says what is missing and why.
  expect(screen.getByTestId("board")).toBeTruthy();
  // The board asks once more, as any read does, before it gives up.
  const note = await screen.findByTestId("section-unavailable", {}, { timeout: 3000 });
  expect(note.textContent).toContain("Agents unavailable.");
  expect(note.textContent).toContain("state.db is not initialized");
});

test("a screen that cannot load says so in the shell and loads by itself once it can", async () => {
  const answers: Record<string, unknown> = {};
  stubDaemon(answers);
  await renderApp(snapshotWithout("/api/v1/agents"), "/agents");
  const note = await screen.findByTestId("screen-unavailable");
  expect(note.textContent).toContain("state.db is not initialized");
  expect(screen.getByRole("navigation", { name: "Screens" })).toBeTruthy();

  answers["/api/v1/agents"] = fixtureSnapshot("default").queries["/api/v1/agents"];
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));

  // The screen's loader also reads each agent's run, which still answer 503
  // and are asked once more before the screen shows without them.
  await vi.waitFor(() => expect(screen.queryByTestId("screen-unavailable")).toBeNull(), {
    timeout: 3000,
  });
  expect(screen.getByRole("heading", { level: 1, name: "Agents" })).toBeTruthy();
  // The sidebar count, which failed on the same read, heals with it.
  const nav = screen.getByRole("navigation", { name: "Screens" });
  await vi.waitFor(() =>
    expect(within(nav).getByText("Agents").parentElement?.querySelector(".count")).not.toBeNull(),
  );
});

test("a screen that does not read the failing endpoint is untouched", async () => {
  stubDaemon({});
  await renderApp(snapshotWithout("/api/v1/agents"), "/inbox");
  expect(screen.queryByTestId("screen-unavailable")).toBeNull();
  expect(screen.getByRole("heading", { level: 1, name: "Inbox" })).toBeTruthy();
});

test("a failed quota read keeps the footer's place and the machine rows", async () => {
  stubDaemon({});
  await renderApp(snapshotWithout("/api/v1/quota"));
  const footer = await screen.findByTestId("quota-footer");
  await vi.waitFor(() => expect(footer.textContent).toContain("Quota unavailable"));
  expect(footer.getAttribute("title")).toContain("state.db is not initialized");
  expect(screen.getByTestId("machine-host-a").textContent).toContain("3 running, 2 free");
  expect(screen.getByTestId("board")).toBeTruthy();
});

test("a failed projects read shows a note in the sidebar, not the error screen", async () => {
  stubDaemon({});
  await renderApp(snapshotWithout("/api/v1/projects"), "/inbox");
  const notes = await screen.findAllByTestId("section-unavailable");
  expect(notes.some((note) => note.textContent?.includes("Projects unavailable."))).toBe(true);
  expect(screen.getByRole("navigation", { name: "Screens" })).toBeTruthy();
  expect(screen.getByTestId("machine-host-a")).toBeTruthy();
});

test("a failed projects read leaves the title alone, with no dangling separator", async () => {
  stubDaemon({});
  await renderApp(snapshotWithout("/api/v1/projects"), "/inbox");
  await screen.findAllByTestId("section-unavailable");
  const crumb = screen.getByRole("heading", { level: 1, name: "Inbox" }).parentElement;
  expect(crumb?.className).toBe("crumb");
  expect(crumb?.children).toHaveLength(1);
});

test("the Retry on a failed section is a bordered button like the other secondary actions", async () => {
  stubDaemon({});
  await renderApp(snapshotWithout("/api/v1/projects"), "/inbox");
  const notes = await screen.findAllByTestId("section-unavailable");
  const retry = within(notes[0] as HTMLElement).getByRole("button", { name: "Retry" });
  expect(retry.classList.contains("btn")).toBe(true);
  expect(retry.classList.contains("lnk")).toBe(false);
});

test("a machine nobody has set up boots to the setup screen with the daemon's command", async () => {
  stubDaemon({ "/api/v1/health": { ok: false, version: "0.1.0", initialized: false } });
  const boot = await loadBoot(document.implementation.createHTMLDocument());
  expect(boot).toEqual({
    kind: "setup",
    command: "tiller owner set tiller\ntiller init --host-label <label> --lane-capacity <n>",
  });
});

test("a 503 from a set-up daemon boots the board, not the setup screen", async () => {
  stubDaemon({ "/api/v1/health": { ok: false, version: "0.1.0", initialized: true } });
  const boot = await loadBoot(document.implementation.createHTMLDocument());
  expect(boot).toEqual({ kind: "board", snapshot: null });
});

const SETUP_COMMANDS =
  "tiller owner set tiller\ntiller init --host-label <label> --lane-capacity <n>";

test("the setup screen opens the board once tiller init has run", async () => {
  vi.useFakeTimers();
  const answers: Record<string, unknown> = {
    "/api/v1/health": { ok: false, version: "0.1.0", initialized: true },
  };
  stubDaemon(answers);
  const onReady = vi.fn();
  render(<SetupScreen command={SETUP_COMMANDS} onReady={onReady} />);
  // Each command is its own numbered, copyable block, owner set first, and
  // `--boundary work` is named on the init step, not after both.
  const steps = screen.getAllByRole("listitem");
  expect(steps.map((step) => step.querySelector("pre")?.textContent)).toEqual([
    "tiller owner set tiller",
    "tiller init --host-label <label> --lane-capacity <n>",
  ]);
  expect(steps.map((step) => step.querySelector(".step-n")?.textContent)).toEqual(["1.", "2."]);
  expect(steps[0]?.querySelector(".step-note")?.textContent).toContain("Hand this machine");
  expect(steps[1]?.querySelector(".step-note")?.textContent).toContain(
    "add --boundary work to this command",
  );
  expect(screen.getByText(/one at a time, in this order/)).toBeTruthy();

  await act(() => vi.advanceTimersByTimeAsync(SETUP_POLL_MS));
  expect(onReady).not.toHaveBeenCalled();

  // Health says initialized once state.db exists; the board opens only once
  // the daemon serves it.
  answers["/api/v1/snapshot"] = fixtureSnapshot("default");
  await act(() => vi.advanceTimersByTimeAsync(SETUP_POLL_MS * 3));
  expect(onReady).toHaveBeenCalledTimes(1);
});

test("a one-command hint shows one block with no number and no order to follow", () => {
  stubDaemon({});
  render(
    <SetupScreen
      command="tiller init --host-label <label> --lane-capacity <n>"
      onReady={() => {}}
    />,
  );
  const steps = screen.getAllByRole("listitem");
  expect(steps).toHaveLength(1);
  expect(steps[0]?.querySelector(".step-n")).toBeNull();
  expect(steps[0]?.querySelector(".step-note")?.textContent).toContain("--boundary work");
  expect(screen.getByText(/Run this in a terminal:/)).toBeTruthy();
  expect(screen.queryByText(/in this order/)).toBeNull();
});

test("a refused read carries the daemon's reason and hint", async () => {
  stubDaemon({});
  const { fetchJson } = await import("../../lib/queries");
  const error = await fetchJson("/api/v1/board").catch((caught: unknown) => caught);
  expect(error).toBeInstanceOf(ReadError);
  expect(error).toMatchObject({
    status: 503,
    reason: "state.db is not initialized; run tiller owner set tiller, then tiller init",
    hint: "tiller owner set tiller\ntiller init --host-label <label> --lane-capacity <n>",
  });
});
