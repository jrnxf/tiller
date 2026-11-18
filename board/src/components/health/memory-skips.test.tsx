import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { resetMemoryFixture } from "../../lib/fixtures-memory";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createAppRouter } from "../../lib/router";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

const NOW = Date.parse("2026-11-11T15:40:00Z");

// The reads the page made.
let reads: string[] = [];

function stubDaemon(scenario: Scenario) {
  vi.stubGlobal("fetch", async (path: string) => {
    reads.push(path);
    const data = fixtureResource(path, scenario, NOW);
    if (data === undefined) return new Response("{}", { status: 404 });
    return new Response(JSON.stringify(data), { status: 200 });
  });
}

async function renderHealth(scenario: Scenario) {
  stubDaemon(scenario);
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario, NOW));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: ["/health"] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  resetMemoryFixture(NOW);
  reads = [];
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

test("Health warns when kb is missing on this host, and links to Memory", async () => {
  await renderHealth("memory-limits");
  const panel = within(await screen.findByRole("region", { name: "Memory" }, { timeout: 3000 }));

  const rows = panel.getAllByRole("listitem");
  expect(rows).toHaveLength(1);
  const row = within(rows[0] as HTMLElement);
  expect(row.getByText("tiller cannot find kb on this host")).toBeTruthy();
  expect(row.getByText("The fleet loads no notes on this host until kb works again.")).toBeTruthy();
  const link = row.getByRole("link", { name: "Open in Memory" });
  expect(link.getAttribute("href")).toBe("/memory?tab=live");
});

test("Health shows no Memory panel while kb works", async () => {
  await renderHealth("default");
  await screen.findByTestId("health");
  await waitFor(() => expect(reads).toContain("/api/v1/memory"));
  for (let i = 0; i < 4; i += 1) await act(async () => {});
  expect(screen.queryByRole("region", { name: "Memory" })).toBeNull();
});
