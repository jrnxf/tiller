import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { BoardRow } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureResource, fixtureSnapshot } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { choose } from "../../test-select";
import { closeOutcome, doneNeeds, doneRefusal, outcomeLabel } from "./card-lifecycle";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

const SHIP = "01JCARD0000000000000000041";
const DONE = "01JCARD0000000000000000009";
// beta-8: a scout card in Queued with no run yet.
const QUEUED = "01JCARD0000000000000000008";

interface Posted {
  path: string;
  method: string;
  body: Record<string, unknown>;
}

let posted: Posted[] = [];

// The one write a test expects.
function sent(): Posted {
  const [first] = posted;
  if (!first) throw new Error("no write was sent");
  return first;
}

// The board API from the fixtures, with one card's row changed in its detail,
// and every write recorded.
function stubApi(patch: Partial<BoardRow> = {}, cardId = SHIP, holds: unknown[] = []) {
  posted = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const path = String(input);
      const method = init?.method ?? "GET";
      if (method !== "GET") {
        posted.push({ path, method, body: JSON.parse(String(init?.body ?? "{}")) });
        const body = { ok: true, replayed: false, events: [], receipt: null, data: {} };
        return new Response(JSON.stringify(body), { status: 200 });
      }
      const body = fixtureResource(path, "default") as Record<string, unknown> | undefined;
      if (body === undefined) return new Response("{}", { status: 404 });
      if (path === `/api/v1/cards/${cardId}`) {
        const detail = body as { column: BoardRow };
        return new Response(
          JSON.stringify({ ...body, holds, column: { ...detail.column, ...patch } }),
        );
      }
      return new Response(JSON.stringify(body), { status: 200 });
    }),
  );
}

async function renderCard(cardId = SHIP) {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default"));
  const router = createAppRouter(
    queryClient,
    createMemoryHistory({ initialEntries: [`/cards/${cardId}`] }),
  );
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  return screen.findByTestId("card-actions");
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  receiptsStore.set({ byKey: {}, progress: {} });
  connectionStore.set("live");
  window.scrollTo = () => {};
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const openMenu = async (actions: HTMLElement) => {
  fireEvent.click(within(actions).getByRole("button", { name: "More card actions" }));
  return screen.findByRole("menu", { name: "More card actions" });
};

test("the outcomes map to the card kind's own outcome", () => {
  expect(closeOutcome("ship", "done")).toBe("landed");
  expect(closeOutcome("scout", "done")).toBe("reported");
  expect(closeOutcome("question", "done")).toBe("answered");
  expect(closeOutcome("ops", "done")).toBe("completed");
  expect(closeOutcome("ship", "wont")).toBe("abandoned");
  expect(closeOutcome("ship", "duplicate")).toBe("superseded");
  expect(outcomeLabel("abandoned")).toBe("won't do");
  expect(outcomeLabel("superseded")).toBe("duplicate");
});

test("an open card has Close rightmost and the rest under More", async () => {
  stubApi();
  const actions = await renderCard();
  const buttons = within(actions).getAllByRole("button");
  expect(buttons.at(-1)?.textContent).toBe("Close");
  const menu = await openMenu(actions);
  const items = within(menu)
    .getAllByRole("menuitem")
    .map((item) => item.textContent);
  expect(items).toEqual(["Hold...", "Edit", "Cancel card..."]);
});

test("a card that has not started has no Close button, says what it waits for and keeps Close under More", async () => {
  stubApi({}, QUEUED);
  const actions = await renderCard(QUEUED);
  expect(within(actions).queryByRole("button", { name: "Close" })).toBeNull();
  // Whose move is next, and the way to start it now.
  const wait = await screen.findByTestId("card-wait");
  expect(wait.textContent).toContain("The beta orchestrator");
  expect(screen.getByTestId("card-page").textContent).toContain("tiller spawn beta-8 scout");
  const menu = await openMenu(actions);
  const items = within(menu)
    .getAllByRole("menuitem")
    .map((item) => item.textContent);
  expect(items).toEqual(["Hold...", "Close card...", "Edit", "Cancel card..."]);
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Close card..." }));
  expect(await screen.findByRole("dialog", { name: "Close beta-8" })).toBeTruthy();
});

// UA-20: Start on a queued card runs `tiller spawn` for it, as the board.
test("a queued card has Start, which sends the run once, and a card with a run has none", async () => {
  stubApi({}, QUEUED);
  const actions = await renderCard(QUEUED);
  const start = within(actions).getByRole("button", { name: "Start" });
  fireEvent.click(start);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(sent()).toMatchObject({
    path: `/api/v1/cards/${QUEUED}/runs`,
    method: "POST",
    body: { idempotency_key: expect.any(String) },
  });
  fireEvent.click(start);
  expect(posted).toHaveLength(1);
});

test("a refused Start shows the reason under the buttons", async () => {
  stubApi({}, QUEUED);
  const served = globalThis.fetch;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) =>
      String(input).endsWith("/runs")
        ? new Response(
            JSON.stringify({
              ok: false,
              error: { code: "refused", message: "beta refuses new runs: 31 GB of disk free" },
            }),
            { status: 422 },
          )
        : served(input, init),
    ),
  );
  const actions = await renderCard(QUEUED);
  fireEvent.click(within(actions).getByRole("button", { name: "Start" }));
  const reason = await screen.findByRole("alert");
  expect(reason.textContent).toBe("beta refuses new runs: 31 GB of disk free");
  // The control stays on, so a retry after making room is one click.
  expect(
    (within(actions).getByRole("button", { name: "Start" }) as HTMLButtonElement).disabled,
  ).toBe(false);
});

test("a card that is working or closed has no Start", async () => {
  stubApi();
  const actions = await renderCard();
  expect(within(actions).queryByRole("button", { name: "Start" })).toBeNull();
  cleanup();
  stubApi({ closed_at: "2026-11-10T10:00:00Z" }, QUEUED);
  const closed = await renderCard(QUEUED);
  expect(within(closed).queryByRole("button", { name: "Start" })).toBeNull();
});

test("a card with a run says nothing about a wait", async () => {
  stubApi();
  await renderCard();
  await screen.findByTestId("card-page");
  expect(screen.queryByTestId("card-wait")).toBeNull();
});

test("Close needs an outcome and a reason, then sends both", async () => {
  stubApi();
  const actions = await renderCard();
  fireEvent.click(within(actions).getByRole("button", { name: "Close" }));
  const dialog = await screen.findByRole("dialog");
  const submit = within(dialog).getByRole("button", { name: "Close card" });
  expect((submit as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(within(dialog).getByRole("button", { name: "Won't do" }));
  expect((submit as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(within(dialog).getByRole("textbox"), { target: { value: "not needed now" } });
  expect((submit as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(submit);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(sent().method).toBe("POST");
  expect(sent().path).toBe(`/api/v1/cards/${SHIP}/close`);
  expect(sent().body).toMatchObject({ outcome: "abandoned", reason: "not needed now" });
});

test("Close says what Done needs on a ship card, and why the button is off", async () => {
  stubApi();
  const actions = await renderCard();
  fireEvent.click(within(actions).getByRole("button", { name: "Close" }));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByTestId("close-missing").textContent).toContain("Pick an outcome");
  expect(within(dialog).queryByTestId("close-done-needs")).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(within(dialog).getByTestId("close-done-needs").textContent).toContain("needs a landing");
  expect(within(dialog).getByTestId("close-missing").textContent).toBe(
    "Give a reason to close the card.",
  );
  fireEvent.click(within(dialog).getByRole("button", { name: "Won't do" }));
  expect(within(dialog).queryByTestId("close-done-needs")).toBeNull();
  fireEvent.change(within(dialog).getByRole("textbox"), { target: { value: "not needed now" } });
  expect(within(dialog).queryByTestId("close-missing")).toBeNull();
});

test("the store's refusal of a Done close reads as a sentence with the next step", () => {
  expect(
    doneRefusal(
      'card beta-8: landed needs a succeeded merge or land approval, or a recovery item answered "Mark landed"',
    ),
  ).toContain("has not landed");
  expect(doneRefusal("card beta-9: no report artifact is recorded on the card")).toContain(
    "no scout report",
  );
  expect(doneRefusal("the daemon is not reachable")).toBeNull();
  expect(doneNeeds("ops")).toBeNull();
});

test("Cancel asks for a reason and confirms", async () => {
  stubApi();
  const actions = await renderCard();
  const menu = await openMenu(actions);
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Cancel card..." }));
  const dialog = await screen.findByRole("dialog");
  const submit = within(dialog).getByRole("button", { name: "Cancel card" });
  expect((submit as HTMLButtonElement).disabled).toBe(true);
  expect(posted).toHaveLength(0);
  fireEvent.change(within(dialog).getByRole("textbox"), { target: { value: "wrong repo" } });
  fireEvent.click(submit);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(sent().path).toBe(`/api/v1/cards/${SHIP}/cancel`);
  expect(sent().body).toMatchObject({ reason: "wrong repo" });
});

test("a dialog opened from the More menu gives focus back to the More button", async () => {
  stubApi();
  const actions = await renderCard();
  const more = within(actions).getByRole("button", { name: "More card actions" });
  more.focus();
  const menu = await openMenu(actions);
  const item = within(menu).getByRole("menuitem", { name: "Cancel card..." });
  item.focus();
  fireEvent.click(item);
  const dialog = await screen.findByRole("dialog");
  await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));

  fireEvent.keyDown(document.activeElement as HTMLElement, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(document.activeElement).toBe(more);
});

test("Hold sends a Lisbon time as an instant, with a reason", async () => {
  stubApi();
  const actions = await renderCard();
  const menu = await openMenu(actions);
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Hold..." }));
  const dialog = await screen.findByRole("dialog");
  fireEvent.change(within(dialog).getByLabelText("Reason"), {
    target: { value: "wait for infra" },
  });
  fireEvent.click(within(dialog).getByRole("button", { name: "Hold card" }));
  await waitFor(() => expect(posted).toHaveLength(2));
  expect(sent().path).toBe(`/api/v1/cards/${SHIP}/hold`);
  expect(sent().body.reason).toBe("wait for infra");
  expect(String(sent().body.until)).toMatch(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?Z$/);
  // No rule refuses a start on a held card, so the orchestrator is told.
  expect(posted[1]?.path).toBe(`/api/v1/cards/${SHIP}/comment`);
  expect(String(posted[1]?.body.text)).toMatch(/^Hold: start no new run .* wait for infra$/);
  expect(posted[1]?.body.idempotency_key).toBe(`${String(sent().body.idempotency_key)}:told`);
});

test("a held card shows its hold with Release as the primary", async () => {
  stubApi(
    {
      parked_until: "2026-12-01T09:00:00.000Z",
      badge: "parked",
      parked_reason: "waiting on infra",
    },
    SHIP,
    [
      {
        id: "01JHOLD0000000000000000001",
        until_at: "2026-12-01T09:00:00.000Z",
        reason: "waiting on infra",
      },
    ],
  );
  const actions = await renderCard();
  const hold = await screen.findByTestId("card-hold");
  expect(hold.textContent).toContain("waiting on infra");
  // The banner owns the reason and the date. The Status row names the state only.
  const page = screen.getByTestId("card-page");
  const status = [...page.querySelectorAll(".props dt")].find((dt) => dt.textContent === "Status");
  expect(status?.nextElementSibling?.textContent).toBe("Held");
  expect(page.textContent?.match(/waiting on infra/g)).toHaveLength(1);
  const buttons = within(actions).getAllByRole("button");
  expect(buttons.at(-1)?.textContent).toBe("Release");
  fireEvent.click(buttons.at(-1) as HTMLElement);
  await waitFor(() => expect(posted).toHaveLength(2));
  expect(sent().method).toBe("DELETE");
  expect(sent().path).toBe(`/api/v1/cards/${SHIP}/hold`);
  // The orchestrator hears of the release too.
  expect(posted[1]?.path).toBe(`/api/v1/cards/${SHIP}/comment`);
  expect(posted[1]?.body.text).toBe("The hold is released: this card may start again.");
});

test("a closed card has only Reopen", async () => {
  stubApi({}, DONE);
  const actions = await renderCard(DONE);
  expect(await screen.findByTestId("card-closed")).toBeTruthy();
  const buttons = within(actions).getAllByRole("button");
  expect(buttons.map((button) => button.textContent)).toEqual(["Reopen"]);
  fireEvent.click(buttons[0] as HTMLElement);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(sent().path).toBe(`/api/v1/cards/${DONE}/reopen`);
  expect(String(sent().body.reason).length).toBeGreaterThan(0);
});

test("Edit turns the title and body into fields, Cancel left and Save right", async () => {
  stubApi();
  const actions = await renderCard();
  const menu = await openMenu(actions);
  fireEvent.click(within(menu).getByRole("menuitem", { name: "Edit" }));
  const form = await screen.findByLabelText("Edit card");
  const buttons = within(form).getAllByRole("button");
  expect(buttons.map((button) => button.textContent)).toEqual(["Cancel", "Save"]);
  const title = within(form).getByLabelText("Title");
  fireEvent.change(title, { target: { value: "A new title" } });
  fireEvent.click(within(form).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(sent().method).toBe("PATCH");
  expect(sent().path).toBe(`/api/v1/cards/${SHIP}`);
  expect(sent().body).toMatchObject({ title: "A new title" });
});

test("the priority saves as soon as one is picked", async () => {
  stubApi();
  const actions = await renderCard();
  const header = actions.closest(".ch-card") as HTMLElement;
  await choose(within(header).getByRole("combobox"), /P1/);
  await waitFor(() => expect(posted).toHaveLength(1));
  expect(sent().method).toBe("PATCH");
  expect(sent().body).toMatchObject({ priority: 1 });
});

test("an Inbox row has the same set in its overflow menu", async () => {
  stubApi();
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default"));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: ["/inbox"] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
  const [trigger] = (await screen.findAllByRole("button", { name: "More card actions" })) as [
    HTMLElement,
  ];
  fireEvent.click(trigger);
  const menu = await screen.findByRole("menu", { name: "More card actions" });
  const items = within(menu)
    .getAllByRole("menuitem")
    .map((item) => item.textContent);
  expect(items).toEqual(["Hold...", "Close card...", "Edit", "Cancel card..."]);
  // The menu is not part of the row's link.
  expect(trigger.closest("a")).toBeNull();
});
