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
  FONT_KEY,
  STAGE_FONT,
  STAGE_FONT_MAX,
  STAGE_FONT_MIN,
  clampStageFont,
  fontReadout,
  gridCell,
  holdsFont,
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

// The window's width: a phone's or a computer's. `turn` changes it, as a
// tablet turned on its side does.
function stubMedia(phone = false) {
  let narrow = phone;
  const changes = new Map<(event: { matches: boolean }) => void, string>();
  const matches = (query: string) => narrow && query.includes("max-width");
  vi.stubGlobal("matchMedia", (query: string) => ({
    get matches() {
      return matches(query);
    },
    media: query,
    addEventListener: (_: string, change: (event: { matches: boolean }) => void) =>
      changes.set(change, query),
    removeEventListener: (_: string, change: (event: { matches: boolean }) => void) =>
      changes.delete(change),
    addListener: () => {},
    removeListener: () => {},
  }));
  return {
    turn(toPhone: boolean) {
      narrow = toPhone;
      act(() => {
        for (const [change, query] of changes) change({ matches: matches(query) });
      });
    },
  };
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

// The Keyboard Lock API, which only Chromium has.
function stubKeyboardLock(refuse = false) {
  const lock = vi.fn((_keys?: string[]) =>
    refuse ? Promise.reject(new Error("refused")) : Promise.resolve(),
  );
  const unlock = vi.fn();
  Object.defineProperty(navigator, "keyboard", { configurable: true, value: { lock, unlock } });
  return { lock, unlock };
}

function unstubFullScreen() {
  Reflect.deleteProperty(navigator, "keyboard");
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

test("every pane fits the stage, so it has no Fit pane switch", async () => {
  const bar = await renderStage("default");
  expect(bar.queryByRole("button", { name: "Fit pane" })).toBeNull();
  // The font steps from the choice, which the Fit keeps on screen.
  expect(bar.getByRole("button", { name: /^Font size / }).textContent).toBe(String(STAGE_FONT));
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

// The stage's room, as the layout measures it.
const room = (shown: number, next = true) => ({ shown, next });

test("the font readout shows the size on screen, and names the choice when they differ", () => {
  // Before the first measure, the choice.
  expect(fontReadout(16, false, room(0))).toMatchObject({ shown: 16, from: 16, full: false });
  // Without the Fit: the size that shows, and Smaller steps down from it.
  expect(fontReadout(20, false, room(13, false))).toEqual({
    shown: 13,
    from: 13,
    full: true,
    hint: "Font size 13 px of the chosen 20, as large as the stage holds the pane. Click for the default, 14 px",
  });
  expect(fontReadout(STAGE_FONT, false, room(12, false)).hint).toBe(
    "Font size 12 px of the default 14, as large as the stage holds the pane",
  );
  expect(fontReadout(STAGE_FONT, false, room(STAGE_FONT)).hint).toBe(
    "Font size 14 px, the default",
  );
  expect(fontReadout(18, false, room(18)).hint).toBe(
    "Font size 18 px. Click for the default, 14 px",
  );
  // With the Fit, the steps go from the choice while the pane takes its size.
  expect(fontReadout(17, true, room(14, false))).toEqual({
    shown: 14,
    from: 17,
    full: false,
    hint: "Font size 14 px of the chosen 17, as large as the stage holds the pane. Click for the default, 14 px",
  });
  // A Fit at its smallest pane that still cannot hold the font.
  expect(fontReadout(24, true, room(19, false))).toEqual({
    shown: 19,
    from: 24,
    full: false,
    hint: "Font size 19 px of the chosen 24, as large as the stage holds the pane. Click for the default, 14 px",
  });
});

// How high a grid's rows are at a cell, on a 2x screen with rows 1.2 cells high.
const rowsHigh = (rows: number) => (its: number) => (its / 2) * 1.2 * rows;

test("under the Fit the font size stays fixed across a stage resize", () => {
  // A 14 px font on a 2x screen: a 28 px cell, and rows 17 CSS px high.
  const cell = 14 * 2;
  const old = { cols: 150, rows: 40 };
  const target = { cols: 75, rows: 27 };
  // The window goes narrower and lower. The grid is still the wide stage's,
  // and the pane's new size is on its way.
  const narrow = { cell, least: 12, width: 75 * cell, room: 27 * 17, fit: true };
  const stage = { held: true, sizing: true, lent: 0, grid: old, target };
  const pinned = holdsFont(stage);
  expect(pinned).toBe(true);
  // The cell stays at the chosen font: the stage clips the old grid.
  expect(gridCell({ ...narrow, cols: old.cols, rowsHigh: rowsHigh(old.rows), pinned })).toBe(cell);

  // The new size lands: the grid fills the stage at the same font.
  const landed = holdsFont({ ...stage, sizing: false, grid: target });
  expect(landed).toBe(false);
  expect(
    gridCell({ ...narrow, cols: target.cols, rowsHigh: rowsHigh(target.rows), pinned: landed }),
  ).toBe(cell);
});

test("only a new size on its way holds the font; a grid that stays another size scales", () => {
  const old = { cols: 150, rows: 40 };
  const target = { cols: 75, rows: 27 };
  const stage = { held: true, sizing: true, lent: 0, grid: old, target };
  // Without the Fit, or once the wait ran out (another client kept its own
  // size), the old grid shows whole, scaled down.
  expect(holdsFont({ ...stage, held: false })).toBe(false);
  expect(holdsFont({ ...stage, sizing: false })).toBe(false);
  // The answer strip's loan keeps the dialog under it in view.
  expect(holdsFont({ ...stage, lent: 40 })).toBe(false);
  // A grid already at the target needs no hold.
  expect(holdsFont({ ...stage, grid: target })).toBe(false);

  const cell = 14 * 2;
  const narrow = { cell, least: 12, width: 75 * cell, room: 27 * 17, fit: true };
  const scaled = gridCell({
    ...narrow,
    cols: old.cols,
    rowsHigh: rowsHigh(old.rows),
    pinned: false,
  });
  expect(scaled).toBeLessThanOrEqual(Math.floor((75 * cell) / old.cols));
  expect(rowsHigh(old.rows)(scaled)).toBeLessThanOrEqual(narrow.room);
});

test("full screen pins the agent's card over the page, and a second press or Esc leaves it", async () => {
  const { request, exit, leave } = stubFullScreen();
  const bar = await renderStage("holder");
  const card = document.querySelector(".av-card");
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();

  // Focus on the rail, which the card is about to cover.
  const list = screen.getByRole("listbox");
  list.focus();
  fireEvent.click(bar.getByRole("button", { name: "Full screen" }));
  expect(request).toHaveBeenCalledTimes(1);
  // The page goes full screen, so dialogs, menus and toasts still show.
  expect(request.mock.contexts[0]).toBe(document.documentElement);
  const leaveButton = await bar.findByRole("button", { name: "Full screen", pressed: true });
  expect(leaveButton.getAttribute("aria-pressed")).toBe("true");
  // With no Keyboard Lock API the browser's own Esc leaves.
  expect(leaveButton.getAttribute("title")).toBe("Leave full screen (Esc)");
  // Focus moves to the way out, off the covered rail.
  expect(document.activeElement).toBe(leaveButton);
  expect(card?.classList.contains("full")).toBe(true);
  // The page under the card is inert, so Tab stays on the card.
  expect(screen.getAllByRole("option", { hidden: true })[0]?.closest("[inert]")).toBeTruthy();
  expect(card?.closest("[inert]")).toBeNull();
  // Full screen is already the larger view.
  expect(bar.queryByRole("button", { name: "Expand the terminal" })).toBeNull();

  fireEvent.click(leaveButton);
  expect(exit).toHaveBeenCalledTimes(1);
  await bar.findByRole("button", { name: "Full screen", pressed: false });
  expect(card?.classList.contains("full")).toBe(false);
  expect(screen.getAllByRole("option")[0]?.closest("[inert]")).toBeNull();
  expect(document.activeElement).toBe(list);

  // From nothing focused, as after a mouse click, focus goes to the way out.
  (document.activeElement as HTMLElement | null)?.blur();
  fireEvent.click(bar.getByRole("button", { name: "Full screen" }));
  const again = await bar.findByRole("button", { name: "Full screen", pressed: true });
  expect(document.activeElement).toBe(again);
  leave();
  await bar.findByRole("button", { name: "Full screen", pressed: false });
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();
});

test("where the browser locks Esc, a tap goes to the pane and a held Esc leaves", async () => {
  const { exit, leave } = stubFullScreen();
  const { lock, unlock } = stubKeyboardLock();
  const bar = await renderStage("holder");
  fireEvent.click(bar.getByRole("button", { name: "Full screen" }));
  const leaveButton = await bar.findByRole("button", { name: "Full screen", pressed: true });
  expect(lock).toHaveBeenCalledWith(["Escape"]);
  await waitFor(() =>
    expect(leaveButton.getAttribute("title")).toBe("Hold Esc to leave full screen"),
  );

  // A tap of Esc reaches the page, and the board does not leave on it.
  fireEvent.keyDown(window, { key: "Escape" });
  fireEvent.keyUp(window, { key: "Escape" });
  expect(exit).not.toHaveBeenCalled();
  expect(document.fullscreenElement).toBe(document.documentElement);

  // The browser's held Esc leaves, and the lock goes with full screen.
  leave();
  const out = await bar.findByRole("button", { name: "Full screen", pressed: false });
  expect(unlock).toHaveBeenCalledTimes(1);
  expect(out.getAttribute("title")).toBe("Full screen");

  // The way out leaves too, and unlocks.
  fireEvent.click(out);
  const again = await bar.findByRole("button", { name: "Full screen", pressed: true });
  await waitFor(() => expect(again.getAttribute("title")).toBe("Hold Esc to leave full screen"));
  fireEvent.click(again);
  expect(exit).toHaveBeenCalledTimes(1);
  expect(unlock).toHaveBeenCalledTimes(2);
  await bar.findByRole("button", { name: "Full screen", pressed: false });
});

test("a lock the browser refuses keeps the browser's own Esc, and says why in the console", async () => {
  stubFullScreen();
  const { lock, unlock } = stubKeyboardLock(true);
  const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
  const bar = await renderStage("holder");
  fireEvent.click(bar.getByRole("button", { name: "Full screen" }));
  const leaveButton = await bar.findByRole("button", { name: "Full screen", pressed: true });
  await waitFor(() => expect(lock).toHaveBeenCalledTimes(1));
  await waitFor(() =>
    expect(debug).toHaveBeenCalledWith(
      "tiller: Esc is not held in full screen: the browser refused the Keyboard Lock",
      expect.anything(),
    ),
  );
  expect(leaveButton.getAttribute("title")).toBe("Leave full screen (Esc)");
  // No toast: full screen opened.
  expect(screen.queryByText("Full screen did not open")).toBeNull();
  fireEvent.click(leaveButton);
  await bar.findByRole("button", { name: "Full screen", pressed: false });
  expect(unlock).not.toHaveBeenCalled();
});

test("a switch to another agent stays in full screen, and a toast shows in it", async () => {
  const { request } = stubFullScreen();
  await renderStage("holder");
  fireEvent.click(footer().getByRole("button", { name: "Full screen" }));
  (await footer().findByRole("button", { name: "Full screen", pressed: true })).focus();
  const first = screen
    .getByRole("group", { name: /^Live terminal for / })
    .getAttribute("aria-label");

  // The rail is under the card and inert, so the view's keys switch.
  fireEvent.keyDown(window, { key: "j" });
  await waitFor(() =>
    expect(
      screen.getByRole("group", { name: /^Live terminal for / }).getAttribute("aria-label"),
    ).not.toBe(first),
  );
  // The old terminal's way out is gone, so focus goes to the new one's.
  const leaveButton = await footer().findByRole("button", { name: "Full screen", pressed: true });
  await waitFor(() => expect(document.activeElement).toBe(leaveButton));
  expect(request).toHaveBeenCalledTimes(1);
  expect(document.fullscreenElement).toBe(document.documentElement);
  expect(document.querySelector(".av-card")?.classList.contains("full")).toBe(true);

  // The page is in full screen, so a toast shows over the card.
  act(() => showToast("bad", "The pane kept its own size"));
  expect(screen.getByTestId("toast").textContent).toContain("The pane kept its own size");
});

// Terminal streams that go live only when the test says: a carry gives the
// new terminal focus then. A stream the mirror stops is gone, as a real
// fetch's is.
// One NDJSON line of a terminal stream.
const encode = (line: unknown) => new TextEncoder().encode(`${JSON.stringify(line)}\n`);

function holdStreams() {
  const api = globalThis.fetch;
  const streams = new Set<ReadableStreamDefaultController<Uint8Array>>();
  vi.stubGlobal("fetch", (input: RequestInfo | URL, init?: RequestInit) => {
    if (!String(input).endsWith("/terminal")) return api(input, init);
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        if (init?.signal?.aborted) return;
        streams.add(controller);
        init?.signal?.addEventListener(
          "abort",
          () => {
            streams.delete(controller);
            controller.error(init.signal?.reason);
          },
          { once: true },
        );
      },
    });
    return Promise.resolve(new Response(body, { status: 200 }));
  });
  return {
    goLive() {
      act(() => {
        for (const controller of streams) {
          controller.enqueue(encode({ type: "size", cols: 80, rows: 24 }));
          controller.enqueue(encode({ type: "frame", seq: 0, full: true, bytes: btoa("$ ") }));
        }
      });
    },
  };
}

const typing = () => screen.getByRole("textbox", { name: /^Type to / });

// Types in the stage's terminal and switches with Option-J, as a carry does.
async function switchFromTyping() {
  const first = typing();
  first.focus();
  const streams = holdStreams();
  fireEvent.keyDown(first, { key: "∆", code: "KeyJ", altKey: true });
  await waitFor(() => expect(typing()).not.toBe(first));
  return streams;
}

test("a switch from typing in full screen leaves focus to the new terminal, not the way out", async () => {
  stubFullScreen();
  await renderStage("holder");
  fireEvent.click(footer().getByRole("button", { name: "Full screen" }));
  await footer().findByRole("button", { name: "Full screen", pressed: true });
  const streams = await switchFromTyping();
  // Before it is live, focus waits on the page, so a key typed on does not
  // press the way out.
  expect(document.activeElement).toBe(document.body);
  streams.goLive();
  await waitFor(() => expect(document.activeElement).toBe(typing()));
  expect(document.fullscreenElement).toBe(document.documentElement);
});

test.each([false, true])(
  "full screen that starts while a carry waits leaves focus to the carry (focus on the rail: %s)",
  async (onRail) => {
    stubFullScreen();
    await renderStage("holder");
    const streams = await switchFromTyping();
    // Focus under the card leaves it, as it turns inert.
    if (onRail) screen.getByRole("listbox").focus();
    fireEvent.click(footer().getByRole("button", { name: "Full screen" }));
    await footer().findByRole("button", { name: "Full screen", pressed: true });
    expect(document.activeElement).toBe(document.body);
    streams.goLive();
    await waitFor(() => expect(document.activeElement).toBe(typing()));
  },
);

test("a turn to the phone layout, or leaving the Agents view, leaves full screen", async () => {
  const { exit } = stubFullScreen();
  const media = stubMedia();
  await renderStage("holder");
  fireEvent.click(footer().getByRole("button", { name: "Full screen" }));
  await footer().findByRole("button", { name: "Full screen", pressed: true });

  media.turn(true);
  await waitFor(() => expect(exit).toHaveBeenCalledTimes(1));
  expect(document.fullscreenElement).toBeNull();
  expect(document.querySelector(".av-card.full")).toBeNull();
  expect(document.querySelector("[inert]:not(.av-det)")).toBeNull();

  media.turn(false);
  fireEvent.click(await footer().findByRole("button", { name: "Full screen" }));
  await footer().findByRole("button", { name: "Full screen", pressed: true });
  cleanup();
  expect(exit).toHaveBeenCalledTimes(2);
  expect(document.fullscreenElement).toBeNull();
});

test("with no Fullscreen API the button is not there, and Expand still is", async () => {
  const bar = await renderStage("holder");
  expect(bar.queryByRole("button", { name: "Full screen" })).toBeNull();
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();
  // The font size is the stage's own, so it stays.
  expect(bar.getByRole("button", { name: "Larger text" })).toBeTruthy();
});

test("on a phone the stage has the font buttons, and no full screen, which the sheet stands for", async () => {
  stubFullScreen();
  stubMedia(true);
  const bar = await renderStage("holder", true);
  expect(bar.queryByRole("button", { name: "Full screen" })).toBeNull();
  expect(bar.queryByRole("button", { name: "Fit pane" })).toBeNull();
  const size = () => bar.getByRole("button", { name: /^Font size / });
  expect(size().textContent).toBe(String(STAGE_FONT));
  fireEvent.click(bar.getByRole("button", { name: "Smaller text" }));
  await waitFor(() => expect(size().textContent).toBe(String(STAGE_FONT - 1)));
  expect(localStorage.getItem(FONT_KEY)).toBe(String(STAGE_FONT - 1));
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();
});

test("a holder pane the daemon has not read yet says so, with no 'observed never'", async () => {
  await renderStage("holder");
  const rows = await screen.findAllByTestId(/^agent-/);
  const fresh = rows.filter((row) => row.textContent?.includes("Not observed yet"));
  expect(fresh).toHaveLength(1);
  expect(document.body.textContent).not.toMatch(/observed never|Unknown/);
  fireEvent.click(fresh[0] as HTMLElement);
  await waitFor(() =>
    expect(document.querySelector(".av-card-h .av-who-2")?.textContent).toBe("Not observed yet"),
  );
});

test("the stage opens its agent's stream once, and a resize or a new font does not open it again", async () => {
  const controls = await renderStage("holder");
  const opens = () =>
    vi
      .mocked(globalThis.fetch)
      .mock.calls.filter(([input]) => String(input).endsWith("/terminal")).length;
  await waitFor(() => expect(opens()).toBe(1));
  act(() => window.dispatchEvent(new Event("resize")));
  fireEvent.click(controls.getByRole("button", { name: /larger/i }));
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(opens()).toBe(1);
});
