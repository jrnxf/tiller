import { QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import type { AgentsResponse } from "../../lib/api-types";
import { agentsPath, queryKeyFor } from "../../lib/queries";
import { ReloadBanner, watchForStaleBoard } from "./reload-banner";
import { createTestQueryClient } from "../../test-app";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function setup(version: string) {
  const queryClient = createTestQueryClient();
  const set = (tiller_version: string | undefined) =>
    queryClient.setQueryData<AgentsResponse>(queryKeyFor(agentsPath()), {
      orchestrators: [],
      tiller_version,
    });
  set(version);
  render(
    <QueryClientProvider client={queryClient}>
      <ReloadBanner />
    </QueryClientProvider>,
  );
  return { set };
}

test("stays quiet when a reconnect reports the same version", async () => {
  const { set } = setup("0.1.0-pre.21");
  await act(async () => set("0.1.0-pre.21"));
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(screen.queryByTestId("reload-banner")).toBeNull();
});

test("offers a reload when the daemon comes back on a new version", async () => {
  const { set } = setup("0.1.0-pre.21");
  await act(async () => set("0.1.0-pre.22"));
  const banner = await screen.findByTestId("reload-banner");
  expect(banner.textContent).toContain("0.1.0-pre.22");
  expect(banner.textContent).toContain("0.1.0-pre.21");
  const reload = vi.fn();
  vi.stubGlobal("location", { ...window.location, reload });
  screen.getByRole("button", { name: "Reload" }).click();
  expect(reload).toHaveBeenCalled();
});

test("offers a reload when a part of this board is gone from the daemon", async () => {
  // An upgrade replaced the files this tab would load its next screen from.
  const stop = watchForStaleBoard(window);
  setup("0.1.0-pre.21");
  expect(screen.queryByTestId("reload-banner")).toBeNull();
  await act(async () => {
    window.dispatchEvent(new Event("vite:preloadError", { cancelable: true }));
  });
  const banner = await screen.findByTestId("reload-banner");
  expect(banner.textContent).toContain(
    "Part of the board could not load. Tiller may have been updated since this tab opened. Reload to get the current board.",
  );
  stop();
});
