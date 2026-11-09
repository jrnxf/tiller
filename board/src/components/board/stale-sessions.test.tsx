import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureMutation } from "../../lib/fixture-mutations";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { createAppRouter } from "../../lib/router";
import { resetBoardLook } from "./board-prefs";

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
  vi.stubGlobal("fetch", async (input: RequestInfo | URL, init?: RequestInit) => {
    const path = String(input);
    if (init?.method === "POST") {
      const body = JSON.parse(String(init.body)) as Record<string, unknown>;
      const { reply } = fixtureMutation("POST", path, body, "stale-sessions");
      return new Response(JSON.stringify(reply.body), { status: reply.status });
    }
    const body = fixtureResource(path, "stale-sessions");
    return new Response(JSON.stringify(body ?? {}), { status: body === undefined ? 404 : 200 });
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("older sessions get a banner, chips and a restart dialog that queues the working one", async () => {
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("stale-sessions"));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: ["/"] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );

  const banner = await screen.findByTestId("stale-banner");
  expect(banner.textContent).toContain("2 sessions run an older tiller.");
  expect(banner.textContent).toContain("Tiller is now 0.1.0-pre.21.");

  const chips = screen.getAllByText("older tiller · pre.20");
  expect(chips.length).toBeGreaterThanOrEqual(2);
  expect(
    chips.some((chip) => chip.title.includes("Current is 0.1.0-pre.21. Role prompt changed.")),
  ).toBe(true);

  fireEvent.click(within(banner).getByRole("button", { name: "Review and restart" }));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByRole("heading").textContent).toBe("Restart 2 older sessions");
  expect(within(dialog).getByText("idle · restarts now")).toBeTruthy();
  expect(within(dialog).getByText("working · restarts when idle")).toBeTruthy();

  fireEvent.click(within(dialog).getByRole("button", { name: "Restart" }));
  expect(await within(dialog).findByText("restarted ✓")).toBeTruthy();
  expect(within(dialog).getByText("queued · restarts when idle")).toBeTruthy();

  fireEvent.click(within(dialog).getByRole("button", { name: "Done" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  await waitFor(() =>
    expect(screen.getByTestId("stale-banner").textContent).toContain(
      "1 session will restart when it goes idle.",
    ),
  );
  expect(within(screen.getByTestId("stale-banner")).queryByRole("button")).toBeNull();
});
