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

// The page as the daemon serves it, naming this entry script.
function page(entry: string): Response {
  return new Response(
    `<!doctype html><html><head><script type="module" crossorigin src="${entry}"></script></head><body><div id="root"></div></body></html>`,
    { status: 200, headers: { "content-type": "text/html" } },
  );
}

// This tab's own entry script.
function loadedFrom(entry: string): () => void {
  const script = document.createElement("script");
  script.type = "module";
  script.src = entry;
  document.head.append(script);
  return () => script.remove();
}

async function failToLoad() {
  await act(async () => {
    window.dispatchEvent(new Event("vite:preloadError", { cancelable: true }));
  });
}

test("a part that fails to load while the daemon serves the same board shows no banner", async () => {
  // A dropped connection fails the same way, and a reload would not help.
  const unload = loadedFrom("/assets/index-aaaa1111.js");
  const stop = watchForStaleBoard(window);
  const served = vi.fn(async () => page("/assets/index-aaaa1111.js"));
  vi.stubGlobal("fetch", served);
  setup("0.1.0-pre.21");
  await failToLoad();
  await vi.waitFor(() => expect(served).toHaveBeenCalledWith("/", { cache: "no-store" }));
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(screen.queryByTestId("reload-banner")).toBeNull();

  // Nor when the daemon does not answer at all.
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => {
      throw new TypeError("Failed to fetch");
    }),
  );
  await failToLoad();
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(screen.queryByTestId("reload-banner")).toBeNull();
  stop();
  unload();
});

test("offers a reload when a part of this board is gone from the daemon", async () => {
  // An upgrade replaced the files this tab would load its next screen from.
  const unload = loadedFrom("/assets/index-aaaa1111.js");
  const stop = watchForStaleBoard(window);
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => page("/assets/index-bbbb2222.js")),
  );
  setup("0.1.0-pre.21");
  expect(screen.queryByTestId("reload-banner")).toBeNull();
  await failToLoad();
  const banner = await screen.findByTestId("reload-banner");
  expect(banner.textContent).toContain(
    "Tiller was updated since this tab opened. Part of the board could not load. Reload to get the new board.",
  );
  stop();
  unload();
});
