import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { createAppRouter } from "../../lib/router";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";
import {
  FIT_KEY,
  FONT_KEY,
  HOLDER_FIT_KEY,
  STAGE_FONT,
  STAGE_FONT_MAX,
  STAGE_FONT_MIN,
  clampStageFont,
  isHolderPane,
  reloadTerminalPrefs,
} from "./live-terminal";
import { showToast } from "./toast";

await loadRouteChunks();

// The board API from the fixtures. The terminal stream never answers, so the
// stage stays on its first screen, Connecting, with its controls shown.
function stubApi(scenario: Scenario) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const path = String(input);
      if (path.endsWith("/terminal")) return new Promise<Response>(() => {});
      const body = fixtureResource(path, scenario);
      return body === undefined
        ? new Response("{}", { status: 404 })
        : new Response(JSON.stringify(body), { status: 200 });
    }),
  );
}

function stubMedia(phone = false) {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: phone && query.includes("max-width"),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
}

// The Agents view with its first agent on the stage, once the stage shows.
// A phone's list shows alone, so there the first row opens the stage.
async function renderStage(scenario: Scenario, phone = false) {
  stubApi(scenario);
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: ["/agents"] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  await waitFor(() => expect(router.state.location.pathname).toBe("/agents"));
  if (phone) {
    const rows = await screen.findAllByTestId(/^agent-/);
    fireEvent.click(rows[0] as HTMLElement);
  }
  // The route's chunk is split: wait on the screen, not only the location.
  await screen.findByRole("group", { name: /^Live terminal for / });
  return footer();
}

function footer() {
  const found = document.querySelector<HTMLElement>(".mir.stage .mir-f");
  if (!found) throw new Error("no stage footer");
  return within(found);
}

// The Fullscreen API, which jsdom lacks: `requestFullscreen` puts the element
// in full screen and `exitFullscreen` takes it out, each with its event.
function stubFullScreen() {
  let current: Element | null = null;
  const change = (next: Element | null) => {
    current = next;
    document.dispatchEvent(new Event("fullscreenchange"));
  };
  Object.defineProperty(document, "fullscreenEnabled", { configurable: true, value: true });
  Object.defineProperty(document, "fullscreenElement", {
    configurable: true,
    get: () => current,
  });
  const request = vi.fn(function (this: Element) {
    change(this);
    return Promise.resolve();
  });
  const exit = vi.fn(() => {
    change(null);
    return Promise.resolve();
  });
  Object.defineProperty(Element.prototype, "requestFullscreen", {
    configurable: true,
    value: request,
  });
  Object.defineProperty(document, "exitFullscreen", { configurable: true, value: exit });
  // The browser's own Esc.
  const leave = () => act(() => change(null));
  return { request, exit, leave };
}

function unstubFullScreen() {
  for (const name of ["fullscreenEnabled", "fullscreenElement", "exitFullscreen"]) {
    Reflect.deleteProperty(document, name);
  }
  Reflect.deleteProperty(Element.prototype, "requestFullscreen");
}

beforeEach(() => {
  localStorage.clear();
  reloadTerminalPrefs();
  prefsStore.set(loadPrefs());
  connectionStore.set("live");
  stubMedia();
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  unstubFullScreen();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

test("a holder pane id is p and a ULID, and a herdr pane id is not", () => {
  expect(isHolderPane("p01ARZ3NDEKTSV4RRFFQ69G5FAV")).toBe(true);
  expect(isHolderPane("p01arz3ndektsv4rrffq69g5fav")).toBe(true);
  for (const herdr of ["w2:p1", "p1", "p", "", "pane-alpha", "w1:p01ARZ3NDEKTSV4RRFFQ69G5FAV"]) {
    expect(isHolderPane(herdr), herdr).toBe(false);
  }
  // A ULID's first digit is 7 at most, and it has no I, L, O or U.
  expect(isHolderPane("p81ARZ3NDEKTSV4RRFFQ69G5FAV")).toBe(false);
  expect(isHolderPane("p01ARZ3NDEKTSV4RRFFQ69G5FAI")).toBe(false);
  expect(isHolderPane(null)).toBe(false);
  expect(isHolderPane(undefined)).toBe(false);
});

test("a holder pane fits the stage by default, and the owner's choice holds", async () => {
  const bar = await renderStage("holder");
  const fit = bar.getByRole("button", { name: "Fit pane" });
  expect(fit.getAttribute("aria-pressed")).toBe("true");
  expect(fit.getAttribute("title")).toBe("Stop sizing the pane to the stage");

  fireEvent.click(fit);
  await waitFor(() => expect(fit.getAttribute("aria-pressed")).toBe("false"));
  expect(fit.getAttribute("title")).toBe("Size the pane to fill the stage");
  expect(localStorage.getItem(HOLDER_FIT_KEY)).toBe("0");
  // The herdr choice is its own.
  expect(localStorage.getItem(FIT_KEY)).toBeNull();

  // A new tab reads the stored choice.
  reloadTerminalPrefs();
  cleanup();
  const again = await renderStage("holder");
  expect(again.getByRole("button", { name: "Fit pane" }).getAttribute("aria-pressed")).toBe(
    "false",
  );
});

test("a herdr pane keeps its own size until the owner fits it", async () => {
  const bar = await renderStage("default");
  const fit = bar.getByRole("button", { name: "Fit pane" });
  expect(fit.getAttribute("aria-pressed")).toBe("false");
  expect(fit.getAttribute("title")).toBe(
    "Size the pane in herdr to fill the stage, until you leave it",
  );
  fireEvent.click(fit);
  await waitFor(() => expect(fit.getAttribute("aria-pressed")).toBe("true"));
  expect(localStorage.getItem(FIT_KEY)).toBe("1");
  expect(localStorage.getItem(HOLDER_FIT_KEY)).toBeNull();
});

test("the font size steps by one pixel, clamps, persists, and goes back to the default", async () => {
  const bar = await renderStage("holder");
  const size = () => bar.getByRole("button", { name: /^Font size / });
  expect(size().textContent).toBe(String(STAGE_FONT));
  expect(size().getAttribute("aria-label")).toBe(`Font size ${STAGE_FONT} px, the default`);

  fireEvent.click(bar.getByRole("button", { name: "Larger text" }));
  fireEvent.click(bar.getByRole("button", { name: "Larger text" }));
  await waitFor(() => expect(size().textContent).toBe(String(STAGE_FONT + 2)));
  expect(localStorage.getItem(FONT_KEY)).toBe(String(STAGE_FONT + 2));
  fireEvent.click(bar.getByRole("button", { name: "Smaller text" }));
  await waitFor(() => expect(size().textContent).toBe(String(STAGE_FONT + 1)));

  // A click on the size goes back to the default, and forgets the choice.
  expect(size().getAttribute("title")).toBe(
    `Font size ${STAGE_FONT + 1} px. Click for the default, ${STAGE_FONT} px`,
  );
  fireEvent.click(size());
  await waitFor(() => expect(size().textContent).toBe(String(STAGE_FONT)));
  expect(localStorage.getItem(FONT_KEY)).toBeNull();

  // A stored size past either end is read clamped, and that end's button is off.
  localStorage.setItem(FONT_KEY, "40");
  act(() => reloadTerminalPrefs());
  await waitFor(() => expect(size().textContent).toBe(String(STAGE_FONT_MAX)));
  expect(bar.getByRole("button", { name: "Larger text" })).toHaveProperty("disabled", true);
  localStorage.setItem(FONT_KEY, "3");
  act(() => reloadTerminalPrefs());
  await waitFor(() => expect(size().textContent).toBe(String(STAGE_FONT_MIN)));
  expect(bar.getByRole("button", { name: "Smaller text" })).toHaveProperty("disabled", true);
  fireEvent.click(bar.getByRole("button", { name: "Smaller text" }));
  expect(size().textContent).toBe(String(STAGE_FONT_MIN));
});

test("an empty stored font size reads as the default", async () => {
  localStorage.setItem(FONT_KEY, " ");
  reloadTerminalPrefs();
  const bar = await renderStage("holder");
  expect(bar.getByRole("button", { name: /^Font size / }).textContent).toBe(String(STAGE_FONT));
});

test("the stage font clamps to 9 to 24 px in whole pixels", () => {
  expect(clampStageFont(14)).toBe(14);
  expect(clampStageFont(8)).toBe(STAGE_FONT_MIN);
  expect(clampStageFont(25)).toBe(STAGE_FONT_MAX);
  expect(clampStageFont(15.6)).toBe(16);
  expect(clampStageFont(Number.NaN)).toBe(STAGE_FONT);
  expect([STAGE_FONT_MIN, STAGE_FONT_MAX]).toEqual([9, 24]);
});

test("full screen pins the agent's card over the page, and a second press or Esc leaves it", async () => {
  const { request, exit, leave } = stubFullScreen();
  const bar = await renderStage("holder");
  const card = document.querySelector(".av-card");
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();

  fireEvent.click(bar.getByRole("button", { name: "Full screen" }));
  expect(request).toHaveBeenCalledTimes(1);
  // The page goes full screen, so dialogs, menus and toasts still show.
  expect(request.mock.contexts[0]).toBe(document.documentElement);
  const leaveButton = await bar.findByRole("button", { name: "Leave full screen" });
  expect(leaveButton.getAttribute("aria-pressed")).toBe("true");
  expect(leaveButton.getAttribute("title")).toBe("Leave full screen (Esc)");
  expect(card?.classList.contains("full")).toBe(true);
  // Full screen is already the larger view.
  expect(bar.queryByRole("button", { name: "Expand the terminal" })).toBeNull();

  fireEvent.click(leaveButton);
  expect(exit).toHaveBeenCalledTimes(1);
  await bar.findByRole("button", { name: "Full screen" });
  expect(card?.classList.contains("full")).toBe(false);

  fireEvent.click(bar.getByRole("button", { name: "Full screen" }));
  await bar.findByRole("button", { name: "Leave full screen" });
  leave();
  await bar.findByRole("button", { name: "Full screen" });
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();
});

test("a switch to another agent stays in full screen, and a toast shows in it", async () => {
  const { request } = stubFullScreen();
  await renderStage("holder");
  fireEvent.click(footer().getByRole("button", { name: "Full screen" }));
  await footer().findByRole("button", { name: "Leave full screen" });
  const first = screen
    .getByRole("group", { name: /^Live terminal for / })
    .getAttribute("aria-label");

  const rows = screen.getAllByRole("option");
  fireEvent.click(rows[1] as HTMLElement);
  await waitFor(() =>
    expect(
      screen.getByRole("group", { name: /^Live terminal for / }).getAttribute("aria-label"),
    ).not.toBe(first),
  );
  expect(await footer().findByRole("button", { name: "Leave full screen" })).toBeTruthy();
  expect(request).toHaveBeenCalledTimes(1);
  expect(document.fullscreenElement).toBe(document.documentElement);
  expect(document.querySelector(".av-card")?.classList.contains("full")).toBe(true);

  // The page is in full screen, so a toast shows over the card.
  act(() => showToast("bad", "The pane kept its own size"));
  expect(screen.getByTestId("toast").textContent).toContain("The pane kept its own size");
});

test("with no Fullscreen API the button is not there, and Expand still is", async () => {
  const bar = await renderStage("holder");
  expect(bar.queryByRole("button", { name: "Full screen" })).toBeNull();
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();
  // The font size is the stage's own, so it stays.
  expect(bar.getByRole("button", { name: "Larger text" })).toBeTruthy();
});

test("on a phone the stage has no font or full screen buttons, which the sheet stands for", async () => {
  stubFullScreen();
  stubMedia(true);
  const bar = await renderStage("holder", true);
  expect(bar.queryByRole("button", { name: "Full screen" })).toBeNull();
  expect(bar.queryByRole("button", { name: "Larger text" })).toBeNull();
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();
});
