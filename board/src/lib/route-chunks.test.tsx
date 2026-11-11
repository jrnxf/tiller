import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { createTestQueryClient } from "../test-app";
import { hydrateFromSnapshot } from "./boot";
import { connectionStore } from "./connection";
import { fixtureResource, fixtureSnapshot } from "./fixtures";
import { loadPrefs, prefsStore } from "./prefs";
import { createQueryClient } from "./query-client";
import { createAppRouter } from "./router";

// This file does not call loadRouteChunks on purpose: its test covers the
// lazy load of a route's split chunk, which every other app test preloads.
// The import phase, which has no timeout, still pays for the slow part: it
// loads the first screen's chunk, and transforms the Health page that the
// Health route's chunk imports. The chunk itself stays unloaded until the
// test navigates to it.
const warm = createAppRouter(createQueryClient());
await warm.loadRouteChunk(warm.routesById["/contributions"]);
await import("../components/health/health-page");

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  connectionStore.set("live");
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const body = fixtureResource(String(input));
      return body === undefined
        ? new Response("{}", { status: 404 })
        : new Response(JSON.stringify(body), { status: 200 });
    }),
  );
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("a navigation loads the next screen's split chunk and shows that screen", async () => {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot());
  const router = createAppRouter(
    queryClient,
    createMemoryHistory({ initialEntries: ["/contributions"] }),
  );
  // The Health screen is a lazy component in a split chunk, and nothing in
  // this file loads that chunk before the navigation.
  const health = router.routesById["/health"];
  const component = health.options.component as { preload?: unknown } | undefined;
  expect(typeof component?.preload).toBe("function");

  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  expect(screen.getAllByTestId("contribution").length).toBeGreaterThan(0);
  expect(screen.queryByLabelText("Claude quota")).toBeNull();

  await act(() => router.navigate({ to: "/health" }));
  expect(router.state.location.pathname).toBe("/health");
  expect(await screen.findByLabelText("Claude quota")).toBeTruthy();
  expect(screen.queryAllByTestId("contribution")).toHaveLength(0);
});
