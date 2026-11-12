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
  fontReadout,
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
  for (const herdr of [
    "w2:p1",
    "p1",
    "p",
    "",
    "pane-alpha",
    "w1:p01ARZ3NDEKTSV4RRFFQ69G5FAV",
    // The `p` is lower case only, as in `tiller_core::ids::is_holder_pane`.
    "P01ARZ3NDEKTSV4RRFFQ69G5FAV",
    // A ULID has no I, L, O or U, and starts at 7 at most.
    "p01ARZ3NDEKTSV4RRFFQ69G5FAI",
    "p81ARZ3NDEKTSV4RRFFQ69G5FAV",
  ]) {
    expect(isHolderPane(herdr), herdr).toBe(false);
  }
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

test("the font readout shows the size on screen, and names the choice when they differ", () => {
  const room = (shown: number, next = true) => ({ shown, next });
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
function holdStreams() {
  const api = globalThis.fetch;
  const encode = (line: unknown) => new TextEncoder().encode(`${JSON.stringify(line)}\n`);
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

test("full screen that starts while a carry waits leaves focus to the carry", async () => {
  stubFullScreen();
  await renderStage("holder");
  const streams = await switchFromTyping();
  fireEvent.click(footer().getByRole("button", { name: "Full screen" }));
  await footer().findByRole("button", { name: "Full screen", pressed: true });
  expect(document.activeElement).toBe(document.body);
  streams.goLive();
  await waitFor(() => expect(document.activeElement).toBe(typing()));
});

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

test("on a phone the stage has no font or full screen buttons, which the sheet stands for", async () => {
  stubFullScreen();
  stubMedia(true);
  const bar = await renderStage("holder", true);
  expect(bar.queryByRole("button", { name: "Full screen" })).toBeNull();
  expect(bar.queryByRole("button", { name: "Larger text" })).toBeNull();
  expect(bar.getByRole("button", { name: "Expand the terminal" })).toBeTruthy();
});
