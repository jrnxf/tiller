import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { prefsStore, loadPrefs } from "../../lib/prefs";
import { createQueryClient } from "../../lib/query-client";
import { createAppRouter } from "../../lib/router";

function stubMedia(dark: boolean) {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: dark && query.includes("dark"),
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
  return router;
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  connectionStore.set("live");
  stubMedia(false);
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.removeAttribute("data-density");
});

test("the first render is the whole shell: no pending or loading UI, nothing to await", async () => {
  await renderApp();

  // Synchronous queries only: if anything were still loading, these would throw.
  expect(screen.getByRole("navigation", { name: "Screens" })).toBeTruthy();
  expect(screen.getByRole("heading", { level: 1, name: "Board" })).toBeTruthy();
  expect(screen.getByTestId("quota-footer").textContent).toContain("27% week left");
  expect(screen.getByTestId("machine-host-a").textContent).toContain("3 running, 2 free");
  expect(screen.queryByRole("progressbar")).toBeNull();
  expect(screen.queryByText(/loading/i)).toBeNull();
});

test.each([
  ["default", "green", "27% week left"],
  ["quota-amber", "amber", "41% week left"],
  ["quota-red", "red", "2% week left"],
  ["quota-grey", "grey", "27% week left"],
] as const)("the quota dot for %s is %s", async (scenario, dot, label) => {
  await renderApp(scenario);
  const footer = screen.getByTestId("quota-footer");
  expect(footer.querySelector("[data-dot]")?.getAttribute("data-dot")).toBe(dot);
  expect(footer.textContent).toContain(label);
  expect(footer.textContent).toContain("resets Tue 14:00");
});

test("nothing on screen is an email or account id", async () => {
  for (const scenario of ["default", "quota-red", "stale-machine"] as const) {
    await renderApp(scenario);
    expect(document.body.textContent ?? "", scenario).not.toMatch(/@|account/i);
    cleanup();
  }
});

test("the quota footer opens Health", async () => {
  const router = await renderApp();
  fireEvent.click(screen.getByTestId("quota-footer"));
  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/health"));
});

test("a Machines row filters by host, shows a chip, and the chip removes the filter", async () => {
  const router = await renderApp();

  fireEvent.click(screen.getByTestId("machine-host-b"));
  await vi.waitFor(() => expect(screen.getByTestId("host-chip").textContent).toContain("host-b"));
  expect(router.state.location.search).toEqual({ host: "01JHOSTB00000000000000000B" });
  expect(screen.getByTestId("machine-host-b").getAttribute("aria-pressed")).toBe("true");

  fireEvent.click(screen.getByTestId("host-chip"));
  await vi.waitFor(() => expect(screen.queryByTestId("host-chip")).toBeNull());
  expect(router.state.location.search).toEqual({});
});

test("the filter survives moving between screens", async () => {
  const router = await renderApp("default", "/?host=01JHOSTB00000000000000000B");
  expect(screen.getByTestId("host-chip").textContent).toContain("host-b");
  fireEvent.click(within(screen.getByRole("navigation", { name: "Screens" })).getByText("Agents"));
  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/agents"));
  expect(router.state.location.search).toEqual({ host: "01JHOSTB00000000000000000B" });
});

test("a stale remote host shows how long ago it answered and why", async () => {
  await renderApp("stale-machine");
  const row = screen.getByTestId("machine-host-c");
  expect(row.textContent).toContain("last seen 17 min ago");
  expect(row.textContent).toContain("view only");
  expect(within(row).getByRole("tooltip").textContent).toContain("does not answer");
});

test("Cmd+K opens the palette, filters, and Enter runs the command", async () => {
  await renderApp();
  expect(screen.queryByRole("dialog")).toBeNull();

  fireEvent.keyDown(window, { key: "k", metaKey: true });
  const input = screen.getByRole("combobox");
  fireEvent.change(input, { target: { value: "theme dark" } });
  expect(screen.getAllByRole("option")).toHaveLength(1);
  fireEvent.keyDown(input, { key: "Enter" });

  expect(screen.queryByRole("dialog")).toBeNull();
  await vi.waitFor(() => expect(document.documentElement.dataset.theme).toBe("dark"));
  expect(localStorage.getItem("tiller-theme")).toBe("dark");
});

test("Escape closes the palette", async () => {
  await renderApp();
  fireEvent.keyDown(window, { key: "k", ctrlKey: true });
  fireEvent.keyDown(screen.getByRole("combobox"), { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("density switches the attribute on <html>", async () => {
  await renderApp();
  fireEvent.click(screen.getByRole("button", { name: "Compact" }));
  await vi.waitFor(() => expect(document.documentElement.dataset.density).toBe("compact"));
});

test("the system theme is followed until a theme is chosen", async () => {
  stubMedia(true);
  await renderApp();
  await vi.waitFor(() => expect(document.documentElement.dataset.theme).toBe("dark"));
  fireEvent.click(screen.getByRole("button", { name: "Light" }));
  await vi.waitFor(() => expect(document.documentElement.dataset.theme).toBe("light"));
});

test("a project toggle hides it from the visible set and survives a reload of the prefs", async () => {
  await renderApp();
  fireEvent.click(screen.getByRole("button", { name: "Hide beta" }));
  expect(screen.getByRole("button", { name: "Show beta" })).toBeTruthy();
  expect(loadPrefs().hiddenProjects).toEqual(["beta"]);

  fireEvent.click(screen.getByRole("button", { name: "Focus" }));
  expect(
    screen
      .getAllByRole("button", { pressed: true })
      .some((b) => b.textContent?.startsWith("alpha")),
  ).toBe(true);
});

test("the degraded pill shows only while the stream is reconnecting", async () => {
  await renderApp();
  const pill = screen.getByTestId("degraded-pill");
  expect(pill.hidden).toBe(true);

  act(() => connectionStore.set("reconnecting"));
  expect(pill.hidden).toBe(false);
  expect(screen.getByTestId("quota-footer").getAttribute("title")).toContain("not answering");

  act(() => connectionStore.set("live"));
  expect(pill.hidden).toBe(true);
});
