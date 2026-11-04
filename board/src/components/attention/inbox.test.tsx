import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { AgentRow, AgentsResponse } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { FIXTURE_HEAD_SHA, fixtureSnapshot, serverResponses } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import { agentsQuery } from "../../lib/queries";
import { createQueryClient } from "../../lib/query-client";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { applyChange } from "../../lib/sse";
import { answeredStore, withdrawnStore } from "./answered";

const RETRY = "01JATTN0000000000000000000";
const MERGE = "01JATTN0000000000000000001";
const QUIET = "01JATTN0000000000000000002";
const SESSION_C = "01JSESS000000000000000000C";
const HOST_B = "01JHOSTB00000000000000000B";

interface Call {
  method: string;
  path: string;
  body: Record<string, unknown>;
}

type Reply = { status: number; body: unknown } | "unreachable";

let calls: Call[] = [];
let replies: Reply[] = [];
let release: (() => void) | null = null;

const ok = (receipt: Partial<Record<string, unknown>> = {}) => ({
  status: 200,
  body: {
    ok: true,
    replayed: false,
    events: [5001],
    receipt: {
      recorded_at: "2026-11-02T14:05:09.000Z",
      outbox_id: null,
      approval_id: null,
      ...receipt,
    },
    data: {},
  },
});

// A daemon stand-in: records every mutation and answers from `replies`. When
// `hold` is set, the answer waits until `release()` so a second click lands
// while the first is still in flight.
function stubDaemon(scenario: Scenario, hold = false) {
  vi.stubGlobal("fetch", async (path: string, init?: RequestInit) => {
    const method = init?.method ?? "GET";
    if (method === "GET") {
      const data = (serverResponses(scenario) as Record<string, unknown>)[path];
      return new Response(JSON.stringify(data), { status: 200 });
    }
    calls.push({ method, path, body: JSON.parse(String(init?.body)) as Record<string, unknown> });
    if (hold) await new Promise<void>((resolve) => (release = resolve));
    const reply = replies.shift() ?? ok();
    if (reply === "unreachable") throw new TypeError("Failed to fetch");
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
}

async function renderInbox(scenario: Scenario = "default", item?: string) {
  const queryClient = createQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario, Date.parse("2026-11-02T12:00:00Z")));
  const path = item ? `/inbox?item=${item}` : "/inbox";
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
  answeredStore.set({});
  withdrawnStore.set({});
  calls = [];
  replies = [];
  release = null;
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const flush = () => act(async () => {});

test("decisions and permission prompts are separate lists, and a stale item is set apart", async () => {
  stubDaemon("default");
  await renderInbox();

  const decisions = screen.getByRole("region", { name: "Decisions" });
  const prompts = screen.getByRole("region", { name: "Permission prompts" });
  const gone = screen.getByRole("region", { name: "No longer current" });
  expect(within(decisions).getByTestId(`inbox-row-${RETRY}`)).toBeTruthy();
  expect(within(decisions).getByTestId(`inbox-row-${MERGE}`)).toBeTruthy();
  expect(within(prompts).getByText(/rm -rf target\/debug\/incremental/)).toBeTruthy();
  expect(within(prompts).queryByTestId(`inbox-row-${RETRY}`)).toBeNull();
  expect(within(gone).getByTestId(`inbox-row-${QUIET}`)).toBeTruthy();
  expect(within(decisions).queryByTestId(`inbox-row-${QUIET}`)).toBeNull();
});

test("an item shows its question, evidence, recommendation, consequence and that it is current", async () => {
  stubDaemon("default");
  await renderInbox("default", RETRY);

  expect(
    screen.getByRole("heading", { name: "Which retry policy should the export job use?" }),
  ).toBeTruthy();
  expect(screen.getByText(/3 of the last 40 exports failed/)).toBeTruthy();
  expect(
    screen.getByRole("link", { name: "https://example.com/alpha/exports/runbook" }),
  ).toBeTruthy();
  expect(screen.getByText(/It covers every failure seen this month/)).toBeTruthy();
  expect(screen.getByText(/The builder implements the policy you pick/)).toBeTruthy();
  expect(screen.getByTestId("current").textContent).toContain("Still waiting");
});

test("an item that no longer applies offers Withdraw and turns its options off", async () => {
  stubDaemon("default");
  await renderInbox("default", QUIET);
  expect(screen.getByTestId("not-current").textContent).toContain("No longer applies.");
  expect(screen.getByRole("button", { name: "Withdraw" })).toBeTruthy();
  for (const option of within(screen.getByRole("group", { name: "Answer" })).getAllByRole(
    "button",
  )) {
    expect((option as HTMLButtonElement).disabled).toBe(true);
  }
});

test("Withdraw sends the withdraw once", async () => {
  stubDaemon("default", true);
  replies = [ok({})];
  await renderInbox("default", QUIET);
  const withdraw = screen.getByRole("button", { name: "Withdraw" });
  fireEvent.click(withdraw);
  fireEvent.click(withdraw);
  release?.();
  await flush();
  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({
    method: "POST",
    path: `/api/v1/attention/${QUIET}/withdraw`,
  });
  // The pane keeps the item, with a receipt, rather than going blank.
  expect(screen.getByTestId("withdrawn").textContent).toContain("Withdrawn.");
  expect(screen.getByTestId(`inbox-row-${QUIET}`).textContent).toContain("withdrawn");
});

test("keys 1-3 answer, but not while typing in the note", async () => {
  stubDaemon("default");
  replies = [ok({ outbox_id: "01JOUTB0000000000000000001" })];
  await renderInbox("default", RETRY);
  fireEvent.keyDown(screen.getByRole("textbox", { name: "Note" }), { key: "2" });
  await flush();
  expect(calls).toHaveLength(0);
  fireEvent.keyDown(window, { key: "2" });
  await flush();
  expect(calls).toHaveLength(1);
  expect(calls[0]?.body.answer).toBe("Fixed delay, five attempts");
});

function pickRecommended() {
  return screen.getByRole("button", { name: /Exponential backoff, three attempts/ });
}

test("a second click on an option repeats nothing", async () => {
  stubDaemon("default", true);
  replies = [ok({ outbox_id: "01JOUTB0000000000000000001" })];
  await renderInbox("default", RETRY);

  const answer = pickRecommended();
  fireEvent.click(answer);
  fireEvent.click(answer);
  fireEvent.click(answer);
  release?.();
  await flush();

  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({
    method: "POST",
    path: `/api/v1/attention/${RETRY}/answer`,
    body: { answer: "Exponential backoff, three attempts", rev: 1 },
  });
  expect(String(calls[0]?.body.idempotency_key)).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);

  // Recorded: the control is gone, so there is nothing left to click.
  expect(screen.queryByRole("group", { name: "Answer" })).toBeNull();
  expect(screen.getByRole("status")).toBeTruthy();
  expect(calls).toHaveLength(1);
});

test("the receipt moves to delivered and done as the outbox row does", async () => {
  stubDaemon("default");
  replies = [ok({ outbox_id: "01JOUTB0000000000000000001" })];
  const { queryClient } = await renderInbox("default", RETRY);
  fireEvent.click(pickRecommended());
  await flush();

  const outbox = (state: string, seq: number) =>
    applyChange(queryClient, {
      seq,
      at: "2026-11-02T14:05:12.000Z",
      kind: `outbox.${state}`,
      txn: "outbox.deliver",
      entity: "outbox",
      entity_id: "01JOUTB0000000000000000001",
      rev_after: null,
      card_id: null,
      entity_state: { id: "01JOUTB0000000000000000001", state },
    });
  act(() => outbox("delivered", 5002));
  expect(screen.getByRole("status").getAttribute("data-stage")).toBe("delivered");
  act(() => outbox("acked", 5003));
  const receipt = screen.getByRole("status");
  expect(receipt.getAttribute("data-stage")).toBe("done");
  expect(receipt.querySelectorAll(".rs.done")).toHaveLength(3);
});

test("a retry after the daemon did not answer reuses the same key", async () => {
  stubDaemon("default");
  replies = ["unreachable", ok({ outbox_id: "01JOUTB0000000000000000001" })];
  await renderInbox("default", RETRY);

  fireEvent.click(pickRecommended());
  await flush();
  expect(screen.getByRole("alert").textContent).toContain("Trying again is safe");
  fireEvent.click(pickRecommended());
  await flush();

  expect(calls).toHaveLength(2);
  expect(calls[1]?.body.idempotency_key).toBe(calls[0]?.body.idempotency_key);
  expect(screen.getByRole("status").getAttribute("data-stage")).toBe("recorded");
});

test("the answered item stays in the inbox with its receipt after it leaves the queue", async () => {
  stubDaemon("default");
  replies = [ok({ outbox_id: "01JOUTB0000000000000000001" })];
  const { queryClient } = await renderInbox("default", RETRY);
  fireEvent.click(pickRecommended());
  await flush();

  act(() =>
    applyChange(queryClient, {
      seq: 5002,
      at: "2026-11-02T14:05:10.000Z",
      kind: "attention.answered",
      txn: "attention.answer",
      entity: "attention",
      entity_id: RETRY,
      rev_after: 2,
      card_id: null,
      entity_state: { id: RETRY, answered_at: "2026-11-02T14:05:10.000Z", withdrawn_at: null },
    }),
  );
  expect(screen.getByTestId(`inbox-row-${RETRY}`).textContent).toContain("answered");
  expect(screen.getByRole("status")).toBeTruthy();
});

test("approve sends the head SHA the board shows, once", async () => {
  stubDaemon("default", true);
  replies = [ok({ approval_id: "01JAPPR0000000000000000000" })];
  await renderInbox("default", MERGE);

  const approve = screen.getByRole("button", { name: `Approve ${FIXTURE_HEAD_SHA.slice(0, 7)}` });
  fireEvent.click(approve);
  fireEvent.click(approve);
  release?.();
  await flush();

  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({
    path: "/api/v1/approvals/01JAPPR0000000000000000000/decide",
    body: { decision: "accept", head_sha: FIXTURE_HEAD_SHA },
  });
  expect(screen.getByRole("status").textContent).toContain(
    `Saved an approval bound to ${FIXTURE_HEAD_SHA.slice(0, 7)}`,
  );
});

test("approving a stale SHA is refused with a readable message", async () => {
  stubDaemon("stale-approval");
  replies = [
    {
      status: 422,
      body: {
        ok: false,
        error: {
          code: "refused",
          message: "approval 01JAPPR: head moved",
          current: { head_sha: FIXTURE_HEAD_SHA },
        },
      },
    },
  ];
  await renderInbox("stale-approval", MERGE);

  fireEvent.click(screen.getByRole("button", { name: /^Approve 7c3e9a1/ }));
  await flush();

  const refusal = screen.getByTestId("stale-refusal");
  expect(refusal.textContent).toBe(
    "Not approved. PR #52 moved from 7c3e9a1 to d82f405 after this page showed it. An approval covers one exact head, so nothing was approved and nothing will merge. Look at the new commits, then approve the new head.",
  );
  expect(screen.queryByRole("status")).toBeNull();
  expect(receiptsStore.get().byKey).toEqual({});

  // The refusal refetches the item, the new head arrives, and approving it is
  // a new decision with its own key.
  const approveNew = await screen.findByRole("button", { name: /^Approve d82f405/ });
  expect(approveNew).toHaveProperty("disabled", false);
  fireEvent.click(approveNew);
  await flush();
  expect(calls).toHaveLength(2);
  expect(calls[1]?.body.head_sha).toBe(FIXTURE_HEAD_SHA);
  expect(calls[1]?.body.idempotency_key).not.toBe(calls[0]?.body.idempotency_key);
});

test("reprioritize sends the card's rev, and a conflict is shown as the server words it", async () => {
  stubDaemon("default");
  replies = [
    {
      status: 409,
      body: {
        ok: false,
        error: {
          code: "conflict",
          message: 'conflict: card 01JCARD is at rev 4, you sent 3; title is now "Retry exports"',
          current: { rev: 4 },
        },
      },
    },
  ];
  await renderInbox("default", RETRY);

  fireEvent.change(screen.getByLabelText("Priority"), { target: { value: "0" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await flush();

  expect(calls[0]).toMatchObject({
    method: "PATCH",
    path: "/api/v1/cards/01JCARD0000000000000000041",
    body: { priority: 0, rev: 3 },
  });
  expect(screen.getByRole("alert").textContent).toContain("is at rev 4, you sent 3");
});

test("comment, add card and open in herdr each reach their endpoint with a receipt", async () => {
  stubDaemon("default");
  replies = [
    ok({ outbox_id: "01JOUTB0000000000000000002" }),
    ok({ outbox_id: "01JOUTB0000000000000000003" }),
    ok(),
  ];
  await renderInbox("default", RETRY);
  const actions = screen.getByTestId("card-actions");

  fireEvent.change(within(actions).getByLabelText("Comment to the orchestrator"), {
    target: { value: "Check the storage client first" },
  });
  fireEvent.click(within(actions).getByRole("button", { name: /Send/ }));
  await flush();

  fireEvent.click(within(actions).getByRole("button", { name: "Add card" }));
  fireEvent.change(within(actions).getByPlaceholderText("Title"), {
    target: { value: "Alert on repeated export failures" },
  });
  fireEvent.click(within(actions).getByRole("button", { name: "Add card" }));
  await flush();

  fireEvent.click(within(actions).getByRole("button", { name: /Open in herdr/ }));
  await flush();

  expect(calls.map((call) => `${call.method} ${call.path}`)).toEqual([
    "POST /api/v1/cards/01JCARD0000000000000000041/comment",
    "POST /api/v1/cards",
    "POST /api/v1/agents/01JSESS000000000000000000A/focus",
  ]);
  expect(calls[0]?.body.text).toBe("Check the storage client first");
  expect(calls[1]?.body).toMatchObject({
    project: "alpha",
    kind: "ship",
    title: "Alert on repeated export failures",
  });
  const keys = new Set(calls.map((call) => call.body.idempotency_key));
  expect(keys.size).toBe(3);
  expect(within(actions).getAllByRole("status")).toHaveLength(3);
  expect(within(actions).getByText(/Focused in herdr/)).toBeTruthy();
});

test("a permission prompt is answered in its pane, not with a decision", async () => {
  stubDaemon("default");
  await renderInbox("default", "perm-01JSESS000000000000000000C");
  expect(screen.getByText("Permission prompt")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Answer/ })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: /Open the pane in herdr/ }));
  await flush();
  expect(calls[0]?.path).toBe("/api/v1/agents/01JSESS000000000000000000C/focus");
});

test("the pane button says what herdr did, and when no herdr window here shows it", async () => {
  stubDaemon("default");
  const launched = ok();
  launched.body.data = { opened: "launched", terminal: "Ghostty" };
  const unseen = ok();
  unseen.body.data = { opened: "unseen", terminal: null };
  replies.push(launched, unseen);
  await renderInbox("default", "perm-01JSESS000000000000000000C");

  fireEvent.click(screen.getByRole("button", { name: /Open the pane in herdr/ }));
  await flush();
  expect(screen.getByText(/Opened herdr in Ghostty/)).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: /Open the pane in herdr/ }));
  await flush();
  expect(screen.getByRole("alert").textContent).toMatch(
    /no herdr window is open on the daemon's machine\. Start herdr there, or open the board on the machine you are at\./,
  );
  expect(screen.queryByText(/Opened herdr in Ghostty/)).toBeNull();
});

test("the pane button says where to find a remote agent's pane", async () => {
  stubDaemon("default");
  const { queryClient } = await renderInbox("default", "perm-01JSESS000000000000000000C");
  // The prompt's agent runs on host-b, a saved remote machine.
  act(() => {
    queryClient.setQueryData<AgentsResponse>(agentsQuery().queryKey, (agents) => {
      const [first, ...rest] = agents?.orchestrators ?? [];
      if (!first) throw new Error("no orchestrator in the fixture");
      const worker: AgentRow = { ...first, session_id: SESSION_C, host_id: HOST_B, workers: [] };
      return { orchestrators: [{ ...first, workers: [...first.workers, worker] }, ...rest] };
    });
  });

  fireEvent.click(screen.getByRole("button", { name: /Open the pane in herdr/ }));
  await flush();
  expect(screen.getByRole("status").textContent).toMatch(
    /^Focused on host-b - select host-b in herdr's sidebar,/,
  );
});

test("pressing O twice while the first focus is in flight focuses once", async () => {
  stubDaemon("default", true);
  await renderInbox("default", "perm-01JSESS000000000000000000C");
  fireEvent.keyDown(window, { key: "o" });
  fireEvent.keyDown(window, { key: "o" });
  release?.();
  await flush();
  expect(calls).toHaveLength(1);
});

test("O does nothing while the detail pane is hidden or the key is held", async () => {
  stubDaemon("default");
  await renderInbox("default", "perm-01JSESS000000000000000000C");
  fireEvent.keyDown(window, { key: "o", repeat: true });
  const detail = document.querySelector<HTMLElement>(".in-detail");
  if (!detail) throw new Error("no detail pane");
  // What the narrow layout does with the list showing.
  detail.style.display = "none";
  fireEvent.keyDown(window, { key: "o" });
  await flush();
  expect(calls).toHaveLength(0);
  detail.style.display = "";
  fireEvent.keyDown(window, { key: "o" });
  await flush();
  expect(calls).toHaveLength(1);
});
