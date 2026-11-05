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
  await renderApp("default", "/settings");
  fireEvent.click(within(screen.getByRole("group", { name: "Density" })).getByText("Compact"));
  await vi.waitFor(() => expect(document.documentElement.dataset.density).toBe("compact"));
  expect(localStorage.getItem("tiller-density")).toBe("compact");
});

test("the system theme is followed until a theme is chosen", async () => {
  stubMedia(true);
  await renderApp("default", "/settings");
  await vi.waitFor(() => expect(document.documentElement.dataset.theme).toBe("dark"));
  fireEvent.click(within(screen.getByRole("group", { name: "Theme" })).getByText("Light"));
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

test("the palette keeps focus in its input and hands it back to what opened it", async () => {
  await renderApp();
  const opener = screen.getByRole("button", { name: "Open command palette" });
  opener.focus();
  fireEvent.click(opener);

  const input = screen.getByRole("combobox");
  expect(document.activeElement).toBe(input);
  // Tab must not walk out of a modal dialog into the screen behind it.
  expect(fireEvent.keyDown(input, { key: "Tab" })).toBe(false);

  fireEvent.keyDown(input, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(document.activeElement).toBe(opener);
});

test("the phone drawer takes focus when it opens, makes the screen inert and gives focus back", async () => {
  await renderApp();
  const menuButton = screen.getByRole("button", { name: "Open the sidebar" });
  const main = document.querySelector(".main");
  expect(menuButton.getAttribute("aria-expanded")).toBe("false");
  expect(main?.hasAttribute("inert")).toBe(false);

  menuButton.focus();
  fireEvent.click(menuButton);
  await vi.waitFor(() => expect(document.activeElement?.className).toBe("search"));
  expect(menuButton.getAttribute("aria-expanded")).toBe("true");
  expect(main?.hasAttribute("inert")).toBe(true);

  fireEvent.keyDown(window, { key: "Escape" });
  await vi.waitFor(() => expect(document.activeElement).toBe(menuButton));
  expect(main?.hasAttribute("inert")).toBe(false);
});

test("widening past the phone layout closes the drawer so the screen is not left inert", async () => {
  const listeners: ((event: { matches: boolean }) => void)[] = [];
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: (_type: string, listener: (event: { matches: boolean }) => void) => {
      if (query.includes("max-width")) listeners.push(listener);
    },
    removeEventListener: () => {},
  }));
  await renderApp();
  fireEvent.click(screen.getByRole("button", { name: "Open the sidebar" }));
  expect(document.querySelector(".main")?.hasAttribute("inert")).toBe(true);

  act(() => {
    for (const listener of listeners) listener({ matches: false });
  });
  expect(document.querySelector(".main")?.hasAttribute("inert")).toBe(false);
});

test("the top bar no longer carries the theme and density controls", async () => {
  await renderApp();
  expect(screen.queryByRole("group", { name: "Theme" })).toBeNull();
  expect(screen.queryByRole("group", { name: "Density" })).toBeNull();
});

test("the quota card shows the runway reason and a row per model", async () => {
  await renderApp("quota-amber");
  const footer = screen.getByTestId("quota-footer");
  expect(footer.textContent).toContain("Spending ahead of pace");
  expect(
    within(footer)
      .getAllByTestId("quota-row")
      .map((row) => row.textContent),
  ).toEqual([expect.stringContaining("week"), expect.stringContaining("Fable")]);
});

test("Settings is a screen item right under Health and opens the page", async () => {
  await renderApp();
  const nav = screen.getByRole("navigation", { name: "Screens" });
  const names = within(nav)
    .getAllByRole("link")
    .map((link) => link.textContent);
  expect(names.indexOf("Settings")).toBe(names.findIndex((name) => name?.startsWith("Health")) + 1);

  fireEvent.click(within(nav).getByRole("link", { name: "Settings" }));
  await vi.waitFor(() => expect(screen.getByTestId("settings")).toBeTruthy());
  expect(screen.getByRole("heading", { level: 1, name: "Settings" })).toBeTruthy();
  expect(within(nav).getByRole("link", { name: "Settings" }).getAttribute("aria-current")).toBe(
    "page",
  );
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("the Settings page shows the saved choices and saves a new one at once", async () => {
  localStorage.setItem("tiller-theme", "dark");
  prefsStore.set(loadPrefs());
  await renderApp("default", "/settings");
  const theme = screen.getByRole("group", { name: "Theme" });
  expect(within(theme).getByText("Dark").getAttribute("aria-pressed")).toBe("true");
  expect(
    within(screen.getByRole("group", { name: "Density" }))
      .getByText("Comfortable")
      .getAttribute("aria-pressed"),
  ).toBe("true");

  fireEvent.click(within(theme).getByText("System"));
  expect(localStorage.getItem("tiller-theme")).toBe("system");
  expect(within(theme).getByText("System").getAttribute("aria-pressed")).toBe("true");
});

test("the palette goes to Settings", async () => {
  await renderApp();
  fireEvent.keyDown(window, { key: "k", metaKey: true });
  const input = screen.getByRole("combobox");
  fireEvent.change(input, { target: { value: "settings" } });
  fireEvent.keyDown(input, { key: "Enter" });
  await vi.waitFor(() => expect(screen.getByTestId("settings")).toBeTruthy());
});

test("the top-bar Ask button and the A key open the ask dialog with the picker open", async () => {
  await renderApp();
  expect(screen.queryByRole("dialog", { name: "Ask an orchestrator" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^Ask/ }));
  expect(screen.getByRole("dialog", { name: "Ask an orchestrator" })).toBeTruthy();
  await vi.waitFor(() =>
    expect(document.activeElement?.classList.contains("ask-combo-in")).toBe(true),
  );
  // Escape closes the open list first, then the dialog.
  fireEvent.keyDown(document.activeElement as HTMLElement, { key: "Escape" });
  fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Ask an orchestrator" })).toBeNull();

  fireEvent.keyDown(document.body, { key: "a" });
  expect(screen.getByRole("dialog", { name: "Ask an orchestrator" })).toBeTruthy();
});
