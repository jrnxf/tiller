import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { briefReadStore } from "../../lib/away";
import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { awayBrief } from "../../lib/fixtures-away";
import { fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

interface Call {
  method: string;
  path: string;
  body: Record<string, unknown>;
}

let calls: Call[] = [];
let refusal: { code: string; message: string } | null = null;

function stubDaemon(ended = awayBrief(false)) {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      return new Response(JSON.stringify({ brief: ended }), { status: 200 });
    }
    calls.push({ method, path, body: JSON.parse(String(init?.body)) as Record<string, unknown> });
    if (refusal) {
      return new Response(JSON.stringify({ ok: false, error: refusal }), { status: 409 });
    }
    return new Response(
      JSON.stringify({
        ok: true,
        replayed: false,
        events: [],
        receipt: { recorded_at: "2026-11-02T14:05:09.000Z", outbox_id: null, approval_id: null },
        data: method === "DELETE" ? { away: null, brief: ended } : { id: ended.approval_id },
      }),
      { status: 200 },
    );
  });
}

async function renderApp(scenario: Scenario, path = "/") {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario, Date.parse("2026-11-02T12:00:00Z")));
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
  receiptsStore.set({ byKey: {}, progress: {} });
  briefReadStore.set([]);
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
  calls = [];
  refusal = null;
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const field = (label: string) => screen.getByLabelText(label) as HTMLInputElement;
const posts = (method: string) => calls.filter((call) => call.method === method);

test("idle, the Away button sits left of Ask and opens the Start dialog", async () => {
  await renderApp("default");
  const away = screen.getByRole("button", { name: "Away" });
  const ask = screen.getByRole("button", { name: /^ask/i });
  expect(away.compareDocumentPosition(ask) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

  fireEvent.click(away);
  const dialog = await screen.findByRole("dialog", { name: "Start away mode" });
  expect(dialog.textContent).toContain(
    "Orchestrators keep working inside these limits. Away never answers a decision for you.",
  );
  expect(dialog.textContent).toContain("Lisbon time. Away ends by itself then.");
  expect(dialog.textContent).toContain("Only the grants you already have may merge.");
  expect(within(dialog).getByRole("radio", { name: "Existing grants" }).ariaChecked).toBe("true");
});

test("Start sends the scope, as typed, to POST /api/v1/away", async () => {
  stubDaemon();
  await renderApp("default");
  fireEvent.click(screen.getByRole("button", { name: "Away" }));
  await screen.findByRole("dialog", { name: "Start away mode" });

  fireEvent.change(field("Until"), { target: { value: "2099-01-02T18:00" } });
  fireEvent.change(screen.getByLabelText(/^Orders/), { target: { value: "Keep to alpha." } });
  fireEvent.change(field("New runs, at most"), { target: { value: "3" } });
  fireEvent.change(field("Quota floor, %"), { target: { value: "15" } });
  fireEvent.click(screen.getByRole("radio", { name: "None" }));
  fireEvent.click(screen.getByRole("button", { name: "Start away mode" }));

  await waitFor(() => expect(posts("POST")).toHaveLength(1));
  const [call] = posts("POST");
  expect(call?.path).toBe("/api/v1/away");
  expect(call?.body).toMatchObject({
    until: "2099-01-02T18:00",
    orders: "Keep to alpha.",
    max_new_runs: 3,
    quota_floor_percent: 15,
    merge: "none",
  });
  expect(call?.body.projects).toBeInstanceOf(Array);
  expect(call?.body.idempotency_key).toBeTruthy();
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Start away mode" })).toBeNull());
});

test("Conditions adds a grant body with the chosen actions and checks", async () => {
  stubDaemon();
  await renderApp("default");
  fireEvent.click(screen.getByRole("button", { name: "Away" }));
  await screen.findByRole("dialog", { name: "Start away mode" });

  fireEvent.click(screen.getByRole("radio", { name: "Conditions..." }));
  const group = screen.getByRole("group", { name: "Grant conditions" });
  fireEvent.click(within(group).getByLabelText("squash"));
  fireEvent.change(within(group).getByLabelText(/^Base branch/), { target: { value: "main" } });
  fireEvent.click(screen.getByRole("button", { name: "Start away mode" }));

  await waitFor(() => expect(posts("POST")).toHaveLength(1));
  expect(posts("POST")[0]?.body.merge).toEqual({
    v: 1,
    actions: ["merge", "squash"],
    checks: "success",
    base_ref: "main",
  });
});

test("a form that cannot go says why and sends nothing", async () => {
  stubDaemon();
  await renderApp("default");
  fireEvent.click(screen.getByRole("button", { name: "Away" }));
  await screen.findByRole("dialog", { name: "Start away mode" });

  fireEvent.change(field("Until"), { target: { value: "2020-01-01T09:00" } });
  fireEvent.click(screen.getByRole("button", { name: "Start away mode" }));
  expect((await screen.findByRole("alert")).textContent).toBe(
    "That time has passed. Choose a later one.",
  );

  fireEvent.change(field("Until"), { target: { value: "2099-01-02T18:00" } });
  fireEvent.change(field("Quota floor, %"), { target: { value: "150" } });
  expect((await screen.findByRole("alert")).textContent).toContain("Quota floor");

  fireEvent.change(field("Quota floor, %"), { target: { value: "" } });
  fireEvent.click(screen.getByRole("radio", { name: "Conditions..." }));
  const group = screen.getByRole("group", { name: "Grant conditions" });
  fireEvent.click(within(group).getByLabelText("merge"));
  expect((await screen.findByRole("alert")).textContent).toContain("at least one way");
  expect(calls).toHaveLength(0);
});

test("a refusal from the daemon shows in the dialog and keeps it open", async () => {
  stubDaemon();
  refusal = { code: "conflict", message: "Away mode is already running." };
  await renderApp("default");
  fireEvent.click(screen.getByRole("button", { name: "Away" }));
  await screen.findByRole("dialog", { name: "Start away mode" });
  fireEvent.click(screen.getByRole("button", { name: "Start away mode" }));
  expect((await screen.findByRole("alert")).textContent).toContain("already running");
  expect(screen.getByRole("dialog", { name: "Start away mode" })).toBeTruthy();
});

test("while away the button is a status pill and the popover holds the summary", async () => {
  await renderApp("away");
  expect(screen.queryByRole("button", { name: "Away" })).toBeNull();
  const pill = screen.getByRole("button", { name: /^Away until Thu 12 Nov, 18:00/ });
  expect(pill.querySelector(".ld.live")).toBeTruthy();

  fireEvent.click(pill);
  const pop = screen.getByRole("dialog", { name: "Away mode" });
  expect(pop.textContent).toContain("alpha");
  expect(pop.textContent).toContain("2 of 6");
  expect(pop.textContent).toContain("Existing grants");
  const buttons = within(pop).getAllByRole("button");
  expect(buttons.map((button) => button.textContent)).toEqual(["Edit", "End"]);
  expect(buttons[1]?.className).toContain("primary");

  fireEvent.keyDown(document, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Away mode" })).toBeNull();
});

test("Edit opens the dialog filled and saves with PUT", async () => {
  stubDaemon(awayBrief(true));
  await renderApp("away");
  fireEvent.click(screen.getByRole("button", { name: /^Away until/ }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Away mode" })).getByText("Edit"));

  await screen.findByRole("dialog", { name: "Edit away mode" });
  expect(field("Until").value).toBe("2026-11-12T18:00");
  expect(field("New runs, at most").value).toBe("6");
  fireEvent.change(field("New runs, at most"), { target: { value: "8" } });
  fireEvent.click(screen.getByRole("button", { name: "Save changes" }));

  await waitFor(() => expect(posts("PUT")).toHaveLength(1));
  expect(posts("PUT")[0]?.path).toBe("/api/v1/away");
  expect(posts("PUT")[0]?.body).toMatchObject({ max_new_runs: 8, merge: "grants" });
});

test("End asks nothing, sends DELETE and opens the brief", async () => {
  stubDaemon();
  const router = await renderApp("away");
  fireEvent.click(screen.getByRole("button", { name: /^Away until/ }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "Away mode" })).getByText("End"));

  await waitFor(() => expect(posts("DELETE")).toHaveLength(1));
  expect(posts("DELETE")[0]?.path).toBe("/api/v1/away");
  await waitFor(() => expect(router.state.location.pathname).toBe("/inbox"));
  expect(router.state.location.search).toEqual({ item: "away-brief" });
  expect(await screen.findByTestId("away-brief")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Away until/ })).toBeNull();
  expect(screen.getByRole("button", { name: "Away" })).toBeTruthy();
});
