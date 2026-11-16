import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { prefsStore, loadPrefs, updatePrefs } from "../../lib/prefs";
import { createAppRouter } from "../../lib/router";
import { newCardStore } from "../board/new-card";
import { askPrefillStore } from "./ask-prefill";
import { createTestQueryClient, loadAskEditor, loadRouteChunks } from "../../test-app";

await Promise.all([loadRouteChunks(), loadAskEditor()]);

function stubMedia(dark: boolean) {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: dark && query.includes("dark"),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    // xterm still listens the old way, for the device pixel ratio.
    addListener: () => {},
    removeListener: () => {},
  }));
}

async function renderApp(scenario: Scenario = "default", path = "/", now = Date.now()) {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario, now));
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
  // What the palette's jobs set, so one test's open dialog is not the next's.
  askPrefillStore.set(null);
  newCardStore.set(null);
  // jsdom lays nothing out, so a Range has no rects. A prefilled Ask box puts
  // the caret at the end and scrolls to it, which reads them.
  Range.prototype.getClientRects = () => [] as unknown as DOMRectList;
  Range.prototype.getBoundingClientRect = () => new DOMRect();
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
  ["default", "green", "27% week left", "resets Tue 14:00"],
  ["quota-amber", "amber", "41% week left", "resets Tue 14:00"],
  // The pace runs the week out before it resets.
  ["quota-red", "red", "2% week left", "empty in 8h"],
  ["quota-grey", "grey", "27% week left", "out of date"],
] as const)("the quota dot for %s is %s", async (scenario, dot, label, note) => {
  // A fixed day, so the runway does not depend on today's date.
  await renderApp(scenario, "/", Date.parse("2026-11-02T12:00:00Z"));
  const footer = screen.getByTestId("quota-footer");
  expect(footer.querySelector("[data-dot]")?.getAttribute("data-dot")).toBe(dot);
  expect(footer.textContent).toContain(label);
  expect(footer.textContent).toContain(note);
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
  const router = await renderApp("federated");

  fireEvent.click(screen.getByTestId("machine-host-d"));
  const filter = { name: "Remove the host-d filter" };
  await vi.waitFor(() => expect(screen.getByRole("link", filter).textContent).toContain("host-d"));
  expect(router.state.location.search).toEqual({ host: "01JPEERD0000000000000000D" });
  expect(screen.getByTestId("machine-host-d").getAttribute("aria-pressed")).toBe("true");

  fireEvent.click(screen.getByRole("link", filter));
  await vi.waitFor(() => expect(screen.queryByRole("link", filter)).toBeNull());
  expect(router.state.location.search).toEqual({});
});

test("the filter survives moving between screens", async () => {
  const router = await renderApp("federated", "/?host=01JPEERD0000000000000000D");
  expect(screen.getByRole("link", { name: "Remove the host-d filter" })).toBeTruthy();
  fireEvent.click(within(screen.getByRole("navigation", { name: "Screens" })).getByText("Agents"));
  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/agents"));
  expect(router.state.location.search).toEqual({ host: "01JPEERD0000000000000000D" });
});

test("a host whose terminal holder is down shows how long ago it answered and why", async () => {
  await renderApp("stale-machine");
  const row = screen.getByTestId("machine-host-a");
  expect(row.textContent).toContain("last seen 17 min ago");
  expect(within(row).getByRole("tooltip").textContent).toContain(
    "The terminal holder on this host does not answer",
  );
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
    // xterm still listens the old way, for the device pixel ratio.
    addListener: () => {},
    removeListener: () => {},
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

test("the quota card is the week only: percent left and when it resets", async () => {
  await renderApp("quota-amber");
  const footer = screen.getByTestId("quota-footer");
  expect(footer.textContent).toMatch(/% week left/);
  expect(footer.textContent).toMatch(/resets /);
  expect(footer.textContent).not.toContain("Fable");
  expect(within(footer).queryByTestId("quota-row")).toBeNull();
});

test("the project list has no note while every project shows", async () => {
  await renderApp();
  expect(screen.queryByText(/projects at once/)).toBeNull();
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

// Opens the palette and types a query.
function search(query: string): HTMLElement {
  fireEvent.keyDown(window, { key: "k", metaKey: true });
  const input = screen.getByRole("combobox");
  fireEvent.change(input, { target: { value: query } });
  return input;
}

test("the palette finds a card by a word of its title and opens it", async () => {
  const router = await renderApp();
  // No card is listed until something is typed.
  fireEvent.keyDown(window, { key: "k", metaKey: true });
  expect(screen.queryByRole("option", { name: /alpha-42/ })).toBeNull();
  fireEvent.keyDown(window, { key: "k", metaKey: true });

  const input = search("machines summary");
  const found = screen.getByRole("option", {
    name: /^alpha-42 Add the machines summary to the sidebar/,
  });
  // The row says where the card is: its project and its column.
  expect(found.textContent).toContain("alpha / Working");
  expect(screen.getAllByRole("option")[0]).toBe(found);
  // The agent on that card is found by the card's words too.
  expect(screen.getByRole("option", { name: /on alpha-42 Add the machines/ })).toBeTruthy();
  fireEvent.keyDown(input, { key: "Enter" });

  await vi.waitFor(() => expect(router.state.location.search.card).toBeTruthy());
  expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
  await screen.findByTestId("card-drawer");
});

test("the palette finds a card by its id, and says when nothing matches", async () => {
  await renderApp();
  search("beta-8");
  const found = screen.getByRole("option", { name: /beta-8 Document the cutover steps/ });
  expect(found.textContent).toContain("beta / Queued");
  // A card that waits on the owner says so in place of its column.
  fireEvent.change(screen.getByRole("combobox"), { target: { value: "alpha-41" } });
  expect(screen.getByRole("option", { name: /^alpha-41/ }).textContent).toContain(
    "alpha / Needs you",
  );
  fireEvent.change(screen.getByRole("combobox"), { target: { value: "zzzz" } });
  expect(screen.getByText("No card, agent or command matches")).toBeTruthy();
});

test("the palette asks one project: the Ask box opens with it picked", async () => {
  await renderApp();
  const input = search("ask alpha");
  expect(screen.getAllByRole("option")[0]?.textContent).toBe("Ask alpha");
  fireEvent.keyDown(input, { key: "Enter" });
  const dialog = await screen.findByRole("dialog", { name: "Ask orchestrators" });
  await vi.waitFor(() =>
    expect(dialog.querySelector(".ProseMirror")?.textContent).toContain("alpha"),
  );
});

test("C with every project in view opens the palette on New card, and a pick opens the dialog", async () => {
  await renderApp();
  // The header has no New card: each lane has its own.
  expect(document.querySelector("header.top .new-card-top")).toBeNull();
  press("c");
  const input = screen.getByRole("combobox") as HTMLInputElement;
  expect(input.value).toBe("New card in ");
  const options = screen.getAllByRole("option").map((option) => option.textContent);
  expect(options).toContain("New card in alpha");
  expect(options.every((label) => label?.startsWith("New card"))).toBe(true);

  fireEvent.change(input, { target: { value: "New card in alpha" } });
  fireEvent.keyDown(input, { key: "Enter" });
  const dialog = await screen.findByRole("dialog", { name: "New card in alpha" });
  expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

test("with one project in view the header has New card, and C opens its dialog", async () => {
  await renderApp();
  act(() => updatePrefs({ view: "focus", focusProject: "alpha" }));
  const header = document.querySelector("header.top") as HTMLElement;
  const button = await within(header).findByRole("button", { name: "New card" });
  fireEvent.click(button);
  const dialog = await screen.findByRole("dialog", { name: "New card in alpha" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

  press("c");
  await screen.findByRole("dialog", { name: "New card in alpha" });
  // The project is known, so the palette has nothing to ask.
  expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
});

test("New card in the palette opens it again on the projects", async () => {
  await renderApp();
  const input = search("new card");
  expect(screen.getAllByRole("option")[0]?.textContent).toBe("New cardC");
  fireEvent.keyDown(input, { key: "Enter" });
  await vi.waitFor(() =>
    expect((screen.getByRole("combobox") as HTMLInputElement).value).toBe("New card in "),
  );
});

test("the top-bar Ask button and the A key open the ask dialog with the text focused", async () => {
  await renderApp();
  expect(screen.queryByRole("dialog", { name: "Ask orchestrators" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^Ask/ }));
  expect(screen.getByRole("dialog", { name: "Ask orchestrators" })).toBeTruthy();
  await vi.waitFor(() =>
    expect(document.activeElement?.classList.contains("ProseMirror")).toBe(true),
  );
  fireEvent.keyDown(document.activeElement as HTMLElement, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Ask orchestrators" })).toBeNull();

  fireEvent.keyDown(document.body, { key: "a" });
  expect(screen.getByRole("dialog", { name: "Ask orchestrators" })).toBeTruthy();
});

// A plain letter key on the page, outside any field.
function press(key: string) {
  fireEvent.keyDown(document.body, { key, code: `Key${key.toUpperCase()}` });
}

test("G then a screen's key opens that screen, and the second key does nothing else", async () => {
  const router = await renderApp();
  press("g");
  press("i");
  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/inbox"));
  expect(screen.getByRole("heading", { level: 1, name: "Inbox" })).toBeTruthy();

  // A alone opens Ask. After G it opens the Agents screen only.
  press("g");
  press("a");
  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/agents"));
  expect(screen.queryByRole("dialog", { name: "Ask orchestrators" })).toBeNull();

  press("g");
  press("b");
  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/"));
});

test("the screen chords leave a field and an open dialog alone", async () => {
  const router = await renderApp();
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  const dialog = screen.getByRole("dialog", { name: "Add project" });
  const cancel = within(dialog).getByRole("button", { name: "Cancel" });
  fireEvent.keyDown(cancel, { key: "g", code: "KeyG" });
  fireEvent.keyDown(cancel, { key: "i", code: "KeyI" });
  expect(router.state.location.pathname).toBe("/");

  fireEvent.click(cancel);
  const field = document.createElement("input");
  document.body.append(field);
  field.focus();
  fireEvent.keyDown(field, { key: "g", code: "KeyG" });
  fireEvent.keyDown(field, { key: "i", code: "KeyI" });
  expect(router.state.location.pathname).toBe("/");
  field.remove();
});

test("the palette shows each screen's chord", async () => {
  await renderApp();
  fireEvent.keyDown(document.body, { key: "k", metaKey: true });
  const palette = screen.getByRole("dialog", { name: /command/i });
  const inbox = within(palette).getByRole("option", { name: /Inbox/ });
  expect([...inbox.querySelectorAll("kbd")].map((key) => key.textContent)).toEqual(["G", "I"]);
});

test("no global shortcut fires while a dialog is open", async () => {
  await renderApp();
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  const dialog = screen.getByRole("dialog", { name: "Add project" });
  const cancel = within(dialog).getByRole("button", { name: "Cancel" });
  cancel.focus();

  // A on a dialog's button, and Cmd+K, would each open a second dialog.
  fireEvent.keyDown(cancel, { key: "a" });
  fireEvent.keyDown(cancel, { key: "k", metaKey: true });
  expect(screen.queryByRole("dialog", { name: "Ask orchestrators" })).toBeNull();
  expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
  expect(screen.getAllByRole("dialog")).toEqual([dialog]);
});

test("Escape closes the palette with focus on the page body", async () => {
  await renderApp();
  fireEvent.keyDown(window, { key: "k", ctrlKey: true });
  expect(screen.getByRole("dialog", { name: "Command palette" })).toBeTruthy();
  (document.activeElement as HTMLElement).blur();
  expect(document.activeElement).toBe(document.body);

  fireEvent.keyDown(document.body, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
});

test("the screen behind the palette is inert while it is open", async () => {
  await renderApp();
  const nav = screen.getByRole("navigation", { name: "Screens" });
  fireEvent.keyDown(window, { key: "k", ctrlKey: true });
  expect(nav.closest("[inert]")).not.toBeNull();
  expect(screen.getByRole("dialog", { name: "Command palette" }).closest("[inert]")).toBeNull();

  fireEvent.keyDown(screen.getByRole("combobox"), { key: "Escape" });
  expect(nav.closest("[inert]")).toBeNull();
});

test("no shortcut acts on the screen behind the open phone drawer", async () => {
  await renderApp();
  fireEvent.click(screen.getByRole("button", { name: "Open the sidebar" }));
  await vi.waitFor(() => expect(document.activeElement?.className).toBe("search"));

  fireEvent.keyDown(document.body, { key: "a" });
  expect(screen.queryByRole("dialog", { name: "Ask orchestrators" })).toBeNull();
  // Escape still closes the drawer, and the keys work again.
  fireEvent.keyDown(document.body, { key: "Escape" });
  expect(document.querySelector(".main")?.hasAttribute("inert")).toBe(false);
  fireEvent.keyDown(document.body, { key: "a" });
  expect(screen.getByRole("dialog", { name: "Ask orchestrators" })).toBeTruthy();
});

test("a dialog opened from the drawer keeps the screen inert when widening closes the drawer", async () => {
  const listeners: ((event: { matches: boolean }) => void)[] = [];
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: (_type: string, listener: (event: { matches: boolean }) => void) => {
      if (query.includes("max-width")) listeners.push(listener);
    },
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
  await renderApp();
  fireEvent.click(screen.getByRole("button", { name: "Open the sidebar" }));
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  const dialog = screen.getByRole("dialog", { name: "Add project" });

  act(() => {
    for (const listener of listeners) listener({ matches: false });
  });
  expect(document.querySelector(".main")?.hasAttribute("inert")).toBe(true);

  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(document.querySelector(".main")?.hasAttribute("inert")).toBe(false);
});

test("the palette turns on away mode: the Start dialog opens", async () => {
  await renderApp();
  const input = search("away");
  expect(screen.getAllByRole("option")[0]?.textContent).toBe("Turn on away mode");
  fireEvent.keyDown(input, { key: "Enter" });
  await screen.findByRole("dialog", { name: /away/i });
});
