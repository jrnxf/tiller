import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { briefReadStore } from "../../lib/away";
import { hydrateFromSnapshot } from "../../lib/boot";
import { fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { queryKeyFor, awayBriefPath } from "../../lib/queries";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { applyChange } from "../../lib/sse";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

async function renderInbox(scenario: Scenario, path = "/inbox") {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario, Date.parse("2026-11-04T09:00:00Z")));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return { router, queryClient };
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  briefReadStore.set([]);
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("an unread brief leads the Inbox and opens as one card", async () => {
  await renderInbox("away-back");
  expect(screen.getByTestId("inbox-row-away-brief")).toBeTruthy();
  const card = screen.getByTestId("away-brief");
  expect(screen.getByRole("heading", { name: "While you were away" })).toBeTruthy();
  expect(screen.getByText("Away brief")).toBeTruthy();
  expect(card.textContent).toContain("Keep going on alpha-42");
  expect(card.textContent).toContain("4 of 6 new runs");
  expect(card.textContent).toContain("Existing grants");
  for (const name of ["Landed", "Runs started", "Still waiting for you"]) {
    expect(within(card).getByRole("group", { name })).toBeTruthy();
  }
});

test("the footer has Open waiting items, then Mark read as the rightmost primary", async () => {
  await renderInbox("away-back");
  const footer = screen.getByTestId("away-brief").querySelector(".ab-f") as HTMLElement;
  const actions = [...footer.querySelectorAll(".btn")];
  expect(actions.map((action) => action.textContent)).toEqual(["Open waiting items", "Mark read"]);
  expect(actions[0]?.className).not.toContain("primary");
  expect(actions[1]?.className).toContain("primary");
});

test("Mark read closes the brief for good", async () => {
  const { router } = await renderInbox("away-back");
  fireEvent.click(screen.getByRole("button", { name: "Mark read" }));
  await vi.waitFor(() => expect(screen.queryByTestId("away-brief")).toBeNull());
  expect(router.state.location.pathname).toBe("/inbox");
  expect(briefReadStore.get()).toContain("01JAWAY0000000000000000000");
  expect(localStorage.getItem("tiller:away-read")).toContain("01JAWAY0000000000000000000");
});

test("a read brief leaves the Inbox", async () => {
  briefReadStore.set(["01JAWAY0000000000000000000"]);
  await renderInbox("away-back");
  expect(screen.queryByTestId("inbox-row-away-brief")).toBeNull();
  expect(screen.queryByTestId("away-brief")).toBeNull();
});

test("Open waiting items goes to the first waiting decision", async () => {
  const { router } = await renderInbox("away-back");
  fireEvent.click(screen.getByRole("link", { name: "Open waiting items" }));
  await vi.waitFor(() =>
    expect(router.state.location.search).toMatchObject({ item: expect.any(String) }),
  );
  expect(router.state.location.search).not.toMatchObject({ item: "away-brief" });
});

test("while away there is no brief card to read", async () => {
  await renderInbox("away");
  expect(screen.queryByTestId("inbox-row-away-brief")).toBeNull();
  expect(screen.queryByTestId("away-brief")).toBeNull();
});

test("an approval event refreshes the brief", async () => {
  const { queryClient } = await renderInbox("away");
  applyChange(queryClient, {
    seq: 1,
    at: "2026-11-04T09:00:01.000Z",
    kind: "approval.changed",
    txn: "x",
    entity: "approval",
    entity_id: "01JAWAY0000000000000000000",
    rev_after: null,
    card_id: null,
    entity_state: null,
  });
  expect(queryClient.getQueryState(queryKeyFor(awayBriefPath))?.isInvalidated).toBe(true);
});
