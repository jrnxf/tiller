import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { AgentRow, Machine, MachinesResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { machinesQuery } from "../../lib/queries";
import { createAppRouter } from "../../lib/router";
import { resetBoardLook } from "../board/board-prefs";
import { hostAwareActivity, laneHealth } from "./degraded";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

async function renderApp(scenario: Scenario, path = "/") {
  vi.stubGlobal("fetch", async (input: RequestInfo | URL) => {
    const body = fixtureResource(String(input), scenario);
    return new Response(JSON.stringify(body ?? {}), { status: body === undefined ? 404 : 200 });
  });
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { queryClient };
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  resetBoardLook();
  connectionStore.set("live");
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function lane(project: string) {
  return screen.getByTestId(`lane-${project}`);
}

test("a lane on a host whose terminal holder is down says so, with the last time it was seen", async () => {
  await renderApp("stale-machine");
  const alpha = lane("alpha");
  expect(alpha.dataset.state).toBe("disconnected");
  const note = within(alpha).getByTestId("lane-note-disconnected");
  expect(note.textContent).toContain("The terminal holder is not answering on the host-a host.");
  expect(note.textContent).toMatch(/last known state from .+ \(17 min ago\)/);
  expect(note.textContent).toContain("Answers and Open in terminal pause until it is back.");
  // The cards keep their last known state, dimmed, never "observed".
  const cards = [...alpha.querySelectorAll("a.dim")];
  expect(cards.length).toBeGreaterThan(0);
  for (const card of cards) {
    expect(card.textContent).toContain("last known");
    expect(card.textContent).not.toMatch(/\bobserved\b/);
  }
});

test("a lane whose orchestrator went quiet reads as stale, and a healthy lane has no note", async () => {
  await renderApp("degraded");
  const beta = lane("beta");
  expect(beta.dataset.state).toBe("stale");
  const note = within(beta).getByTestId("lane-note-stale");
  expect(note.textContent).toContain("No events from the beta orchestrator since");
  expect(note.textContent).toContain("41 min ago");
  expect(within(beta).getByText("orchestrator silent for 41 min")).toBeTruthy();
  expect(lane("alpha").dataset.state).toBeUndefined();
  expect(within(lane("alpha")).queryByTestId("lane-note-stale")).toBeNull();
  expect(within(lane("alpha")).queryByTestId("lane-note-disconnected")).toBeNull();
});

test("a card whose agent state is unknown says Unknown, not working", async () => {
  await renderApp("degraded");
  const card = within(lane("gamma")).getByText("Rotate the staging credentials").closest("a");
  expect(card?.textContent).toContain("Unknown");
  expect(card?.textContent).toContain("unknown, seen");
  expect(card?.textContent).not.toContain("Working");
});

test("a registered project with no cards shows the empty lane, not a blank one", async () => {
  await renderApp("degraded");
  const kiln = lane("kiln");
  expect(within(kiln).getByText("No cards yet")).toBeTruthy();
  expect(kiln.dataset.state).toBeUndefined();
});

test("the reconnecting banner shows while the daemon is unreachable and clears when it is back", async () => {
  await renderApp("default");
  expect(screen.getByTestId("degraded-pill").hidden).toBe(true);
  act(() => connectionStore.set("reconnecting"));
  const banner = screen.getByTestId("degraded-pill");
  expect(banner.hidden).toBe(false);
  expect(banner.textContent).toContain("Daemon reconnecting.");
  act(() => connectionStore.set("live"));
  expect(screen.getByTestId("degraded-pill").hidden).toBe(true);
});

test("the banner also shows while the local terminal holder is down, even with the daemon live", async () => {
  const { queryClient } = await renderApp("default");
  const machines = fixtureResource("/api/v1/machines", "default") as MachinesResponse;
  act(() =>
    queryClient.setQueryData<MachinesResponse>(machinesQuery().queryKey, {
      machines: machines.machines.map((entry) =>
        entry.kind === "local"
          ? { ...entry, health: "stale" as const, stale_reason: "holder_down" as const }
          : entry,
      ),
    }),
  );
  await waitFor(() => expect(screen.getByTestId("degraded-pill").hidden).toBe(false));
  expect(screen.getByTestId("degraded-pill").textContent).toContain(
    "Terminal holder reconnecting.",
  );
});

test("a host whose terminal holder is down shows last seen with its reason in the sidebar", async () => {
  await renderApp("stale-machine");
  const row = screen.getByTestId("machine-host-a");
  expect(row.textContent).toContain("last seen 17 min ago");
  expect(row.textContent).toContain("The terminal holder on this host does not answer");
});

function machine(over: Partial<Machine>): Machine {
  return {
    id: "m",
    label: "host-x",
    kind: "remote",
    view_only: true,
    health: "ok",
    running_agents: 1,
    capacity: null,
    free_slots: null,
    last_seen_at: new Date(Date.now() - 5 * 60_000).toISOString(),
    stale_reason: null,
    rev: 1,
    ...over,
  };
}

function orchestrator(minutesAgo: number): AgentRow {
  return {
    session_id: "s",
    role: "orchestrator",
    model: "opus",
    activity: "working",
    activity_observed_at: new Date(Date.now() - minutesAgo * 60_000).toISOString(),
    last_status_line: null,
    name: null,
    card_id: null,
    host_id: "m",
    workers: [],
  };
}

test("an unreachable machine outranks a quiet orchestrator, and a fresh one is ok", () => {
  const down = machine({ health: "stale" });
  expect(laneHealth(orchestrator(45), down).kind).toBe("disconnected");
  expect(laneHealth(orchestrator(45), machine({})).kind).toBe("stale");
  expect(laneHealth(orchestrator(1), machine({})).kind).toBe("ok");
  expect(laneHealth(undefined, machine({})).kind).toBe("ok");
});

test("agents on an unreachable host read unknown whatever they last reported", () => {
  const worker = { ...orchestrator(1), role: "build" };
  expect(hostAwareActivity(worker, machine({ health: "stale" })).dot).toBe("unknown");
  expect(hostAwareActivity(worker, machine({ health: "down" })).dot).toBe("unknown");
  expect(hostAwareActivity(worker, machine({})).dot).toBe("live");
  expect(hostAwareActivity(worker, undefined).dot).toBe("live");
});
