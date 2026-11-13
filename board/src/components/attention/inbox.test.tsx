import { readFileSync } from "node:fs";
import { resolve as resolvePath } from "node:path";

import { QueryClientProvider } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type {
  AgentRow,
  AgentsResponse,
  AttentionResponse,
  BoardResponse,
  MachinesResponse,
  PromptDialog,
} from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import {
  FIXTURE_HEAD_SHA,
  fixtureResource,
  fixtureSnapshot,
  orchestratorSession,
  serverResponses,
} from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { loadPrefs, prefsStore } from "../../lib/prefs";
import {
  agentsQuery,
  attentionQuery,
  boardPath,
  machinesQuery,
  queryKeyFor,
} from "../../lib/queries";
import { receiptsStore } from "../../lib/receipts";
import { createAppRouter } from "../../lib/router";
import { applyChange } from "../../lib/sse";
import { answeredStore, sentStore, withdrawnStore } from "./answered";
import { choose } from "../../test-select";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

const RETRY = "01JATTN0000000000000000000";
const RETRY_CARD = "01JCARD0000000000000000041";
const MERGE = "01JATTN0000000000000000001";
const QUIET = "01JATTN0000000000000000002";
const CAPACITY = "01JATTN0000000000000000003";
const SESSION_C = "01JSESSWGAMMA1200000000000";
const PEER_SESSION = "01JSESSPEERD000000000000G";

interface Call {
  method: string;
  path: string;
  body: Record<string, unknown>;
}

type Reply = { status: number; body: unknown } | "unreachable";

let calls: Call[] = [];
let replies: Reply[] = [];
let release: (() => void) | null = null;
// Reads of a waiting agent's dialog (`POST .../prompt`): kept apart from the
// mutations, and answered from `screens` in turn, the last one repeating.
let reads: string[] = [];
let screens: PromptDialog[] = [];
// A tail read finds the terminal holder down.
let tailFails = false;
// A prompt read is refused: the pane is gone.
let promptRefused = false;
// The `host` each prompt read carried, in order.
let readHosts: (string | null)[] = [];
// The agents list never answers, so the board does not know who runs where.
let agentsHang = false;
// The agents list fails.
let agentsFail = false;

const DIALOG: PromptDialog = {
  layout: "permission",
  tool: "Bash",
  title: "Bash command",
  command: "rm -rf target/debug/incremental && cargo build -p gamma-cert",
  folder: null,
  question: "Do you want to proceed?",
  options: [
    { n: 1, text: "Yes" },
    { n: 2, text: "Yes, and don't ask again for rm and cargo build commands in /home/user/gamma" },
    { n: 3, text: "No" },
  ],
  screen: "s1",
};

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
      if (agentsHang && path === "/api/v1/agents") return new Promise<Response>(() => {});
      if (agentsFail && path === "/api/v1/agents") {
        const error = { code: "internal", message: "the store is locked" };
        return new Response(JSON.stringify({ ok: false, error }), { status: 500 });
      }
      const data =
        (serverResponses(scenario) as Record<string, unknown>)[path] ??
        fixtureResource(path, scenario);
      return new Response(JSON.stringify(data), { status: 200 });
    }
    // A tail read: the screen of a menu the board has no layout for.
    if (path.endsWith("/terminal/tail")) {
      if (tailFails) {
        const error = {
          code: "unavailable",
          message: "the terminal holder is unreachable: no socket",
        };
        return new Response(JSON.stringify({ ok: false, error }), { status: 503 });
      }
      const text = " Select a model for this session:\n ❯ 1. Default (recommended)\n";
      return new Response(JSON.stringify({ session_id: "s", cols: 100, rows: 28, text }));
    }
    // A live terminal's stream: it never answers, so it stays connecting.
    if (/\/terminal(\/\w+)?$/.test(path)) return new Promise<Response>(() => {});
    const read = /^\/api\/v1\/agents\/([^/]+)\/prompt$/.exec(path);
    if (read?.[1]) {
      reads.push(read[1]);
      const sent = init?.body ? (JSON.parse(String(init.body)) as { host?: string }) : {};
      readHosts.push(sent.host ?? null);
      if (promptRefused) {
        const error = {
          code: "refused",
          message: "the pane is gone; nothing was read",
        };
        return new Response(JSON.stringify({ ok: false, error }), { status: 422 });
      }
      const prompt = (screens.length > 1 ? screens.shift() : screens[0]) ?? null;
      const body = {
        session_id: read[1],
        read_at: "2026-11-02T11:59:58.000Z",
        prompt,
        reason: prompt ? null : "no_dialog",
      };
      return new Response(JSON.stringify(body), { status: 200 });
    }
    calls.push({ method, path, body: JSON.parse(String(init?.body)) as Record<string, unknown> });
    if (hold) await new Promise<void>((resolve) => (release = resolve));
    const reply = replies.shift() ?? ok();
    if (reply === "unreachable") throw new TypeError("Failed to fetch");
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
}

async function renderInbox(
  scenario: Scenario = "default",
  item?: string,
  prepare?: (queryClient: QueryClient) => void,
) {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario, Date.parse("2026-11-02T12:00:00Z")));
  const path = item ? `/inbox?item=${item}` : "/inbox";
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: [path] }));
  await router.load();
  // After the load, so what it changes is not read again first.
  prepare?.(queryClient);
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
  sentStore.set({});
  calls = [];
  replies = [];
  release = null;
  reads = [];
  readHosts = [];
  screens = [DIALOG];
  tailFails = false;
  promptRefused = false;
  agentsHang = false;
  agentsFail = false;
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    // xterm still listens the old way, for the device pixel ratio.
    addListener: () => {},
    removeListener: () => {},
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

test("a recommendation says who made it, never always the orchestrator", async () => {
  stubDaemon("default");
  // Opened by Tiller itself (the reconciler, a gate, the nightly backup): no
  // session behind it. The owner never opens an item with a recommendation.
  await renderInbox("default", CAPACITY);
  expect(screen.getByText("Tiller recommended")).toBeTruthy();
  expect(screen.queryByText("You recommended")).toBeNull();
  expect(screen.queryByText(/The orchestrator recommend/)).toBeNull();
  cleanup();

  // Opened by a session that is no longer listed: its role is not known.
  const { queryClient } = await renderInbox("default", RETRY);
  expect(screen.getByText("A session recommended")).toBeTruthy();

  // Opened by a live session: its role says who it is.
  await flush();
  const key = agentsQuery().queryKey;
  const agents = queryClient.getQueryData<AgentsResponse>(key);
  const first = agents?.orchestrators[0];
  if (!agents || !first) throw new Error("no fixture agents");
  await act(async () => {
    queryClient.setQueryData<AgentsResponse>(key, {
      ...agents,
      orchestrators: [{ ...first, session_id: "01JSESS000000000000000000A", role: "builder" }],
    });
    // Query notifies its observers on a timer, so stay inside act through it.
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
  expect(screen.getByText("The builder recommended")).toBeTruthy();
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

test("keys 1-3 answer nothing while a dialog is open", async () => {
  stubDaemon("default");
  await renderInbox("default", RETRY);
  fireEvent.click(screen.getByRole("button", { name: "Add project" }));
  const dialog = screen.getByRole("dialog", { name: "Add project" });
  fireEvent.keyDown(within(dialog).getByRole("button", { name: "Cancel" }), { key: "2" });
  await flush();
  expect(calls).toHaveLength(0);
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

test("an option that changes capacity says so, and the answer says it was applied", async () => {
  stubDaemon("default");
  replies = [ok({ outbox_id: "01JOUTB0000000000000000001" })];
  await renderInbox("default", CAPACITY);
  const raise = screen.getByRole("button", { name: /Raise to 2/ });
  expect(raise.textContent).toContain("Sets gamma capacity from 1 to 2");
  expect(screen.getByRole("button", { name: /Let it wait/ }).textContent).not.toContain(
    "Sets gamma",
  );
  fireEvent.click(raise);
  await flush();
  expect(calls).toHaveLength(1);
  expect(calls[0]?.body.answer).toBe("Raise to 2");
  expect(screen.getByText(/Applied gamma capacity from 1 to 2\./)).toBeTruthy();
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

  await choose(screen.getByLabelText("Priority"), /^P0 /);
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await flush();

  expect(calls[0]).toMatchObject({
    method: "PATCH",
    path: "/api/v1/cards/01JCARD0000000000000000041",
    body: { priority: 0, rev: 3 },
  });
  expect(screen.getByRole("alert").textContent).toContain("is at rev 4, you sent 3");
});

test("comment, add card and open in terminal each reach their endpoint with a receipt", async () => {
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

  fireEvent.click(within(actions).getByRole("button", { name: /Open in terminal/ }));
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
  expect(within(actions).getByText(/Switched the terminal to the agent/)).toBeTruthy();
});

// Opens the detail's ··· menu and picks Open in terminal.
async function openTerminal() {
  const detail = document.querySelector<HTMLElement>(".in-detail");
  if (!detail) throw new Error("no detail pane");
  fireEvent.click(within(detail).getByRole("button", { name: "More actions" }));
  const menu = await screen.findByRole("menu", { name: "More actions" });
  fireEvent.click(within(menu).getByRole("menuitem", { name: /Open in terminal/ }));
}

test("a permission prompt shows the pane's own options, in pane order, as answer buttons", async () => {
  stubDaemon("default");
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");
  expect(screen.getByText("Permission prompt")).toBeTruthy();
  const yes = await screen.findByRole("button", { name: "Answer 1: Yes" });
  const answers = screen.getAllByRole("button", { name: /^Answer \d/ });
  expect(answers.map((button) => button.getAttribute("data-n"))).toEqual(["1", "2", "3"]);
  expect(yes.className).toContain("first");
  expect(reads).toEqual([SESSION_C]);
  expect(screen.getByText(/Press an answer, and the board types it in the pane\./)).toBeTruthy();
  expect(screen.queryByText(/answer it in the pane/)).toBeNull();
  // The hook's record shows only when the screen gives nothing to press.
  expect(screen.queryByText("What the pane is asking")).toBeNull();
  expect(calls).toHaveLength(0);
});

test("pressing an answer types its digit, with the screen it was drawn from", async () => {
  stubDaemon("default");
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toBe(`/api/v1/agents/${SESSION_C}/prompt/answer`);
  expect(calls[0]?.body).toEqual({ option: 1, screen: "s1" });
  expect(screen.getByRole("status").textContent).toMatch(/Sent 1 · Yes\./);
});

test("a changed screen types nothing, and the new options show", async () => {
  stubDaemon("default");
  const redrawn = {
    ...DIALOG,
    command: "rm -rf target && cargo build -p gamma-cert",
    screen: "s2",
  };
  screens = [DIALOG, redrawn];
  replies.push({
    status: 409,
    body: { error: "screen_changed", detail: "the prompt changed after the buttons were drawn" },
  });
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  expect(screen.getByRole("alert").textContent).toMatch(/^Not sent\. The prompt changed/);
  expect(screen.getByText("rm -rf target && cargo build -p gamma-cert")).toBeTruthy();
  expect(calls).toHaveLength(1);
});

test("a later prompt of the same session answered elsewhere never shows the earlier receipt", async () => {
  stubDaemon("default");
  const { queryClient } = await renderInbox("default", `perm-${SESSION_C}`);
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  const key = attentionQuery().queryKey;
  const first = queryClient.getQueryData<AttentionResponse>(key);
  const prompt = first?.permission_prompts.find((row) => row.session_id === SESSION_C);
  if (!first || !prompt) throw new Error("no prompt in the fixture");
  const setPrompts = (permission_prompts: AttentionResponse["permission_prompts"]) =>
    act(() => {
      queryClient.setQueryData<AttentionResponse>(key, { ...first, permission_prompts });
    });
  // Prompt A leaves the inbox: this tab answered it.
  setPrompts([]);
  expect(await screen.findByText("Answered")).toBeTruthy();
  // The same session asks prompt B, and the owner answers it in the pane.
  setPrompts([{ ...prompt, since: "2026-11-02T12:01:00.000Z" }]);
  expect(await screen.findByRole("button", { name: "Answer 1: Yes" })).toBeTruthy();
  setPrompts([]);
  expect(await screen.findByText("Not in the inbox any more")).toBeTruthy();
  expect(screen.getByText("It was answered or withdrawn.")).toBeTruthy();
  expect(screen.queryByText(/Sent 1 · Yes/)).toBeNull();
});

test("a prompt answered here and gone while the Inbox was closed claims no answer later", async () => {
  stubDaemon("default");
  const { router, queryClient } = await renderInbox("default", `perm-${SESSION_C}`);
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  // The owner leaves the Inbox. Meanwhile the prompt leaves, a later one of
  // the same session comes, and it is answered in the pane.
  await act(() => router.navigate({ to: "/" }));
  act(() => {
    queryClient.setQueryData<AttentionResponse>(attentionQuery().queryKey, (attention) =>
      attention ? { ...attention, permission_prompts: [] } : attention,
    );
  });
  await act(() => router.navigate({ to: "/inbox", search: { item: `perm-${SESSION_C}` } }));
  expect(await screen.findByText("Not in the inbox any more")).toBeTruthy();
  expect(screen.queryByText(/Sent 1 · Yes/)).toBeNull();
});

test("a permission prompt reads the screen only once the board knows the agent's machine", async () => {
  stubDaemon("default");
  agentsHang = true;
  await renderInbox("default", `perm-${SESSION_C}`, (queryClient) =>
    queryClient.removeQueries({ queryKey: agentsQuery().queryKey }),
  );
  expect(await screen.findByText("What the pane is asking")).toBeTruthy();
  await flush();
  expect(reads).toEqual([]);
  expect(screen.queryByRole("button", { name: /^Answer \d/ })).toBeNull();
});

test("an answer that lands after the Inbox closed claims no answer later", async () => {
  stubDaemon("default", true);
  const { router, queryClient } = await renderInbox("default", `perm-${SESSION_C}`);
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  // The owner leaves while the answer is on its way, and it lands after.
  await act(() => router.navigate({ to: "/" }));
  await act(async () => release?.());
  act(() => {
    queryClient.setQueryData<AttentionResponse>(attentionQuery().queryKey, (attention) =>
      attention ? { ...attention, permission_prompts: [] } : attention,
    );
  });
  await act(() => router.navigate({ to: "/inbox", search: { item: `perm-${SESSION_C}` } }));
  expect(await screen.findByText("Not in the inbox any more")).toBeTruthy();
  expect(screen.queryByText(/Sent 1 · Yes/)).toBeNull();
});

test("a permission prompt still gets its buttons when the agents list fails", async () => {
  stubDaemon("default");
  agentsFail = true;
  await renderInbox("default", `perm-${SESSION_C}`, (queryClient) =>
    queryClient.removeQueries({ queryKey: agentsQuery().queryKey }),
  );
  expect(
    await screen.findByRole("button", { name: "Answer 1: Yes" }, { timeout: 4000 }),
  ).toBeTruthy();
});

test("with no dialog on the screen, the detail shows what the hook recorded", async () => {
  stubDaemon("default");
  screens = [];
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");
  expect(await screen.findByText("What the pane is asking", {}, { timeout: 4000 })).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Answer \d/ })).toBeNull();
});

const SESSION_TRUST = "01JSESSWALPHA4200000000000";
const SESSION_MENU = "01JSESSWBETA70000000000000";

test("an agent waiting on a dialog no hook asked for is a row in Permission prompts", async () => {
  stubDaemon("dialog");
  await renderInbox("dialog");
  const prompts = screen.getByRole("region", { name: "Permission prompts" });
  expect(within(prompts).getByText("alpha-42-build waits on a dialog")).toBeTruthy();
  expect(within(prompts).getByText("beta-7-build waits on a dialog")).toBeTruthy();
  // beta-7's worker waits on a menu, so the scenario has no stale "went
  // quiet" decision for it beside the dialog row.
  expect(screen.queryByText(/went quiet/)).toBeNull();
});

test("the folder trust dialog is answered from its detail, with no digits", async () => {
  stubDaemon("dialog");
  screens = [
    {
      layout: "trust",
      tool: "",
      title: "Accessing workspace:",
      command: null,
      folder: "/home/user/project",
      question: "Is this a project you created or one you trust?",
      options: [
        { n: 1, text: "No, exit" },
        { n: 2, text: "Yes, I trust this folder" },
      ],
      screen: "t1",
    },
  ];
  await renderInbox("dialog", `dialog-${SESSION_TRUST}`);
  expect(await screen.findByRole("heading", { name: "Trust this folder?" })).toBeTruthy();
  expect(screen.getByText("Dialog")).toBeTruthy();
  expect(
    screen.getByText(
      /Tiller records trust before each launch, so this means that step did not take\./,
    ),
  ).toBeTruthy();
  expect(screen.getByText("/home/user/project")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Answer \d/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Yes, I trust this folder" }));
  await flush();
  expect(reads).toEqual([SESSION_TRUST]);
  expect(calls[0]?.path).toBe(`/api/v1/agents/${SESSION_TRUST}/prompt/answer`);
  expect(calls[0]?.body).toEqual({ option: 2, screen: "t1" });
});

test("an answered dialog that left the inbox says it was sent from here", async () => {
  stubDaemon("dialog");
  screens = [{ ...DIALOG, layout: "trust", tool: "", command: null, screen: "t1" }];
  const { queryClient } = await renderInbox("dialog", `dialog-${SESSION_TRUST}`);
  fireEvent.click(await screen.findByRole("button", { name: "Yes" }));
  await flush();
  // The agent goes on working, so it waits on nothing.
  act(() => {
    queryClient.setQueryData<AgentsResponse>(agentsQuery().queryKey, (agents) =>
      agents
        ? {
            ...agents,
            orchestrators: agents.orchestrators.map((orchestrator) => ({
              ...orchestrator,
              workers: orchestrator.workers.map((worker) =>
                worker.session_id === SESSION_TRUST ? { ...worker, activity: "working" } : worker,
              ),
            })),
          }
        : agents,
    );
  });
  expect(await screen.findByText("Answered")).toBeTruthy();
  expect(screen.getByText("Sent Yes. The agent went on, so it left the inbox.")).toBeTruthy();
  expect(screen.queryByText("It was answered or withdrawn.")).toBeNull();
});

test("a later dialog with no other verdict between never shows the earlier receipt", async () => {
  stubDaemon("dialog");
  screens = [{ ...DIALOG, layout: "trust", tool: "", command: null, screen: "t1" }];
  const { queryClient } = await renderInbox("dialog", `dialog-${SESSION_TRUST}`);
  fireEvent.click(await screen.findByRole("button", { name: "Yes" }));
  await flush();
  // A read in flight would put the fixture's agents back over a change.
  const settled = () => waitFor(() => expect(queryClient.isFetching()).toBe(0));
  await settled();
  const worker = (change: Partial<AgentRow>) =>
    act(() => {
      queryClient.setQueryData<AgentsResponse>(agentsQuery().queryKey, (agents) =>
        agents
          ? {
              ...agents,
              orchestrators: agents.orchestrators.map((orchestrator) => ({
                ...orchestrator,
                workers: orchestrator.workers.map((row) =>
                  row.session_id === SESSION_TRUST ? { ...row, ...change } : row,
                ),
              })),
            }
          : agents,
      );
    });
  // The holder sees the next dialog before it ever sees the agent work, so the
  // run of waits keeps its start, but the observation is a later one.
  worker({ activity_observed_at: "2026-11-02T12:01:00.000Z" });
  await settled();
  // The owner answers that one in the pane, and the agent goes on.
  worker({ activity: "working" });
  expect(await screen.findByText("Not in the inbox any more")).toBeTruthy();
  expect(screen.queryByText(/Sent Yes/)).toBeNull();
});

test("a dialog the board cannot read shows no buttons, only the pane and View live", async () => {
  stubDaemon("dialog");
  screens = [];
  await renderInbox("dialog", `dialog-${SESSION_MENU}`);
  expect(
    await screen.findByText(
      "The board cannot read this dialog, so it shows no buttons. Look at the live terminal and answer there.",
    ),
  ).toBeTruthy();
  expect(screen.getByRole("heading", { name: "beta-7-build waits for an answer" })).toBeTruthy();
  expect(screen.getByText("What the pane shows")).toBeTruthy();
  expect(await screen.findByRole("button", { name: /View live/ })).toBeTruthy();
  expect(screen.getByTestId("pane-tail").textContent).toContain("Select a model for this session:");
  expect(screen.queryByRole("button", { name: /^Answer \d/ })).toBeNull();
  expect(screen.queryByText("Trust this folder?")).toBeNull();
});

test("a pane tail keeps each terminal row whole and scrolls inside its box", () => {
  // jsdom lays nothing out, so this holds the rules. A wrapped row breaks a
  // dialog's box on a phone; the tail scrolls sideways, never the page.
  const css = readFileSync(resolvePath(import.meta.dirname, "inbox.css"), "utf8");
  const term = css.match(/\n\.term\s*\{([^}]*)\}/)?.[1] ?? "";
  expect(term).toMatch(/white-space:\s*pre;/);
  expect(term).toMatch(/overflow-x:\s*auto/);
  expect(term).not.toMatch(/overflow-wrap/);
});

test("a pane that cannot be read still offers View live", async () => {
  stubDaemon("dialog");
  screens = [];
  tailFails = true;
  await renderInbox("dialog", `dialog-${SESSION_MENU}`);
  expect(
    await screen.findByText(/The pane could not be read: the terminal holder is unreachable/),
  ).toBeTruthy();
  expect(screen.getByRole("button", { name: /View live/ })).toBeTruthy();
});

test("Open in terminal moves into the detail's ··· menu", async () => {
  stubDaemon("default");
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");
  expect(screen.queryByRole("button", { name: /Open in terminal/ })).toBeNull();
  await openTerminal();
  await flush();
  expect(calls[0]?.path).toBe(`/api/v1/agents/${SESSION_C}/focus`);
});

test("the pane button says what the daemon did, and gives the attach command when no terminal here shows the agent", async () => {
  stubDaemon("default");
  const launched = ok();
  launched.body.data = { opened: "launched", terminal: "Ghostty" };
  const unseen = ok();
  unseen.body.data = { opened: "unseen", terminal: null };
  replies.push(launched, unseen);
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");

  await openTerminal();
  await flush();
  expect(screen.getByText(/Opened the agent in Ghostty/)).toBeTruthy();

  await openTerminal();
  await flush();
  const alert = screen.getByRole("alert");
  expect(alert.textContent).toContain("Run this in a terminal on the daemon's host:");
  expect(alert.textContent).toContain(`tiller attach ${SESSION_C}`);
  expect(screen.queryByText(/Opened the agent in Ghostty/)).toBeNull();
});

const PEER_PROMPT = `perm-${PEER_SESSION}`;

// Turns host-d's pairing read-only.
function readOnlyPeer(queryClient: QueryClient) {
  queryClient.setQueryData<MachinesResponse>(machinesQuery().queryKey, (machines) =>
    machines
      ? {
          ...machines,
          machines: machines.machines.map((machine) =>
            machine.label === "host-d"
              ? { ...machine, view_only: true, peer_scope: "read" }
              : machine,
          ),
        }
      : machines,
  );
}

test("a peer's permission prompt is read and answered through the peer, with a key", async () => {
  stubDaemon("federated");
  await renderInbox("federated", PEER_PROMPT);
  const yes = await screen.findByRole("button", { name: "Answer 1: Yes" });
  expect(readHosts).toEqual(["host-d"]);
  expect(
    screen.getByText(/Press an answer, and the board types it in the pane on host-d\./),
  ).toBeTruthy();
  fireEvent.click(yes);
  await flush();
  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toBe(`/api/v1/agents/${PEER_SESSION}/prompt/answer`);
  expect(calls[0]?.body).toMatchObject({ option: 1, screen: "s1", host: "host-d" });
  expect(typeof calls[0]?.body.idempotency_key).toBe("string");
});

test("a peer answer the peer did not confirm says so, and a retry carries the same key", async () => {
  stubDaemon("federated");
  replies.push({
    status: 504,
    body: {
      ok: false,
      error: { code: "forward_uncertain", message: "host-d did not answer within 60 seconds" },
    },
  });
  await renderInbox("federated", PEER_PROMPT);
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  expect(await screen.findByText(/Sent, not confirmed\./)).toBeTruthy();
  expect(screen.getByText(/did not answer within 60 seconds/)).toBeTruthy();
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  expect(calls).toHaveLength(2);
  expect(calls[1]?.body.idempotency_key).toBe(calls[0]?.body.idempotency_key);
});

// Sets host-d's link, as a peer that stops answering goes lagging.
function peerLink(queryClient: QueryClient, link: "live" | "lagging") {
  queryClient.setQueryData<MachinesResponse>(machinesQuery().queryKey, (machines) =>
    machines
      ? {
          ...machines,
          machines: machines.machines.map((machine) =>
            machine.label === "host-d" ? { ...machine, link } : machine,
          ),
        }
      : machines,
  );
}

test("a peer that lags while an answer is out keeps the strip, says the answer is unconfirmed, and a retry carries the same key", async () => {
  stubDaemon("federated", true);
  replies.push({
    status: 504,
    body: {
      ok: false,
      error: { code: "forward_uncertain", message: "host-d did not answer within 60 seconds" },
    },
  });
  const { queryClient } = await renderInbox("federated", PEER_PROMPT);
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  act(() => peerLink(queryClient, "lagging"));
  await flush();
  expect(screen.queryByText(/The board cannot answer it here/)).toBeNull();
  await act(async () => release?.());
  expect(await screen.findByText(/Sent, not confirmed\./)).toBeTruthy();
  act(() => peerLink(queryClient, "live"));
  fireEvent.click(await screen.findByRole("button", { name: "Answer 1: Yes" }));
  await flush();
  await act(async () => release?.());
  expect(calls).toHaveLength(2);
  expect(calls[1]?.body.idempotency_key).toBe(calls[0]?.body.idempotency_key);
});

test("a permission prompt on a peer paired read-only says to answer it from that host", async () => {
  stubDaemon("federated");
  await renderInbox("federated", PEER_PROMPT, readOnlyPeer);
  expect(
    screen.getByText(
      /is stopped on a permission dialog on host-d\. The board cannot answer it here \(view only pairing\), so answer it from that host\./,
    ),
  ).toBeTruthy();
  expect(screen.getByText("What the pane is asking")).toBeTruthy();
  await flush();
  expect(reads).toEqual([]);
  expect(screen.queryByRole("button", { name: /^Answer \d/ })).toBeNull();
});

test("a peer's pane opens in a terminal only on its own host", async () => {
  stubDaemon("federated");
  await renderInbox("federated", PEER_PROMPT);
  const detail = document.querySelector<HTMLElement>(".in-detail");
  if (!detail) throw new Error("no detail pane");
  fireEvent.click(within(detail).getByRole("button", { name: "More actions" }));
  const menu = await screen.findByRole("menu", { name: "More actions" });
  const item = within(menu).getByRole("menuitem", { name: /Open in terminal/ });
  expect(item.getAttribute("aria-disabled")).toBe("true");
  expect(item.getAttribute("title")).toBe("host-d owns this pane: open it from that host");
});

test("pressing O twice while the first focus is in flight focuses once", async () => {
  stubDaemon("default", true);
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");
  fireEvent.keyDown(window, { key: "o" });
  fireEvent.keyDown(window, { key: "o" });
  release?.();
  await flush();
  expect(calls).toHaveLength(1);
});

test("O does nothing while the detail pane is hidden or the key is held", async () => {
  stubDaemon("default");
  await renderInbox("default", "perm-01JSESSWGAMMA1200000000000");
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

const GAMMA_ORCH = orchestratorSession("gamma");

// The pane tail block of the selected decision's evidence.
function paneTail(): HTMLElement {
  const detail = document.querySelector<HTMLElement>(".in-detail");
  const head = detail?.querySelector<HTMLElement>(".ev-acts")?.parentElement;
  if (!head) throw new Error("no pane tail head");
  return head;
}

test("a current decision's pane tail has View live, with Open in terminal in its ··· menu", async () => {
  stubDaemon("default");
  await renderInbox("default", CAPACITY);
  const head = paneTail();
  const live = within(head).getByRole("button", { name: "View live" });
  const more = within(head).getByRole("button", { name: "More actions" });
  // The primary is rightmost, the menu on its left.
  expect(more.compareDocumentPosition(live) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(live.className).toContain("primary");
  expect(within(head).queryByRole("button", { name: /Open in terminal/ })).toBeNull();
  fireEvent.click(more);
  const menu = await screen.findByRole("menu", { name: "More actions" });
  fireEvent.click(within(menu).getByRole("menuitem", { name: /Open in terminal/ }));
  await flush();
  expect(calls.map((call) => call.path)).toEqual([`/api/v1/agents/${GAMMA_ORCH}/focus`]);
});

test("View live opens the agent's live terminal over the Inbox, and closing it returns to the decision", async () => {
  stubDaemon("default");
  const { router } = await renderInbox("default", CAPACITY);
  expect(screen.queryByRole("dialog")).toBeNull();
  fireEvent.click(within(paneTail()).getByRole("button", { name: "View live" }));
  const dialog = await screen.findByRole("dialog", { name: "Terminal for gamma-orch" });
  expect(within(dialog).getByText("gamma-orch")).toBeTruthy();
  expect(within(dialog).getByRole("group", { name: "Live terminal for gamma-orch" })).toBeTruthy();
  const agents = within(dialog).getByRole("link", { name: "View in Agents" });
  expect(agents.getAttribute("href")).toContain(`agent=${GAMMA_ORCH}`);
  // The decision stays under it, and is where closing lands.
  expect(router.state.location.search).toEqual({ item: CAPACITY });
  fireEvent.click(within(dialog).getByRole("button", { name: "Close the expanded terminal" }));
  await flush();
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(router.state.location.search).toEqual({ item: CAPACITY });
  expect(screen.getByRole("heading", { name: /Raise gamma capacity to 2/ })).toBeTruthy();
  expect(calls).toHaveLength(0);
});

test("a decision that is no longer current has no View live", async () => {
  stubDaemon("default");
  const { queryClient } = await renderInbox("default", CAPACITY);
  expect(within(paneTail()).getByRole("button", { name: "View live" })).toBeTruthy();
  act(() => {
    queryClient.setQueryData<AttentionResponse>(attentionQuery().queryKey, (attention) =>
      attention
        ? {
            ...attention,
            items: attention.items.map((item) =>
              item.id === CAPACITY ? { ...item, still_current: false } : item,
            ),
          }
        : attention,
    );
  });
  // The detail says why, and only then the pane tail has no way in.
  await screen.findByTestId("not-current");
  expect(screen.queryByRole("button", { name: "View live" })).toBeNull();
  expect(document.querySelector(".in-detail .ev-acts")).toBeNull();
});

const COUNTS = ["Landed", "New decisions", "Failed runs", "Memory suggestions"];

test("the recap card counts what happened since the owner left", async () => {
  stubDaemon("default");
  await renderInbox();
  const card = await screen.findByTestId("recap");
  expect(card.textContent).toMatch(/Since \w{3} \d\d:\d\d/);
  for (const [label, count] of [
    ["Landed", "3"],
    ["New decisions", "2"],
    ["Failed runs", "1"],
    ["Memory suggestions", "4"],
  ] as const) {
    const row = within(card).getByText(label).closest("a");
    expect(row?.textContent).toBe(`${label}${count}`);
  }
  expect(
    COUNTS.map((label) => within(card).getByText(label).closest("a")?.getAttribute("href")),
  ).toEqual(["/", "/inbox", "/?filter=needs", "/memory"]);
  // The first decision still opens in the detail.
  expect(screen.getByTestId("inbox").getAttribute("data-pane")).toBe("list");
});

test("a quiet recap says nothing is new and has no Mark seen", async () => {
  stubDaemon("empty");
  await renderInbox("empty");
  const card = await screen.findByTestId("recap-quiet");
  expect(card.textContent).toMatch(/Nothing new since \w{3} \d\d:\d\d/);
  expect(screen.queryByRole("button", { name: "Mark seen" })).toBeNull();
});

test("Mark seen posts once and the card turns quiet", async () => {
  stubDaemon("default", true);
  replies = [
    {
      status: 200,
      body: {
        ...ok().body,
        data: {
          seen_at: "2026-11-02T14:05:09.000Z",
          since: "2026-11-02T14:05:09.000Z",
          landed: 0,
          decisions: 0,
          failed_runs: 0,
          suggestions: 0,
        },
      },
    },
  ];
  await renderInbox();
  const mark = await screen.findByRole("button", { name: "Mark seen" });
  fireEvent.click(mark);
  fireEvent.click(mark);
  release?.();
  await flush();
  expect(calls).toHaveLength(1);
  expect(calls[0]).toMatchObject({ method: "POST", path: "/api/v1/recap/seen" });
  expect(Object.keys(calls[0]?.body ?? {})).toEqual(["idempotency_key"]);
  expect(screen.getByTestId("recap-quiet").textContent).toContain("Nothing new since");
});

test("a refused Mark seen shows why and keeps the card", async () => {
  stubDaemon("default");
  replies = [
    {
      status: 403,
      body: {
        ok: false,
        error: { code: "forbidden", message: "Only the owner can mark this seen." },
      },
    },
  ];
  await renderInbox();
  fireEvent.click(await screen.findByRole("button", { name: "Mark seen" }));
  await flush();
  expect((await screen.findByRole("alert")).textContent).toContain("Only the owner");
  expect(screen.getByTestId("recap")).toBeTruthy();
});

test("a recap event refetches the recap", async () => {
  stubDaemon("default");
  const { queryClient } = await renderInbox();
  await screen.findByTestId("recap");
  const seen = vi.spyOn(queryClient, "invalidateQueries");
  act(() => {
    applyChange(queryClient, {
      seq: 9001,
      at: "2026-11-02T14:06:00.000Z",
      kind: "proposal.created",
      txn: "proposal.create",
      entity: "proposal",
      entity_id: "01JPROP0000000000000000001",
      rev_after: null,
      card_id: null,
      entity_state: null,
    });
  });
  expect(seen).toHaveBeenCalledWith(
    expect.objectContaining({ queryKey: ["api", "/api/v1/recap"] }),
  );
});

test("an item's back link is a bordered button, not a ghost", async () => {
  await renderInbox("default", RETRY);
  const back = screen.getByRole("link", { name: "Back to the inbox" });
  expect([...back.classList]).toContain("btn");
  expect([...back.classList]).not.toContain("ghost");
});

// P31: an undated wait is an open decision, not a hold. A month after it
// opened it still sits under Needs you and in Decisions as current, and the
// board shows no alarm for its age: no stale mark, no amber age, nothing set
// apart. The snapshot's server clock reads 2026-11-02 12:00.
test("an undated hold a month old shows under Needs you and never alarms", async () => {
  stubDaemon("default");
  const { router, queryClient } = await renderInbox();
  act(() => {
    queryClient.setQueryData<AttentionResponse>(attentionQuery().queryKey, (attention) =>
      attention
        ? {
            ...attention,
            items: attention.items.map((item) =>
              item.id === RETRY ? { ...item, opened_at: "2026-10-03T12:00:00.000Z" } : item,
            ),
          }
        : attention,
    );
    // The waiting session is still observed on every pass.
    queryClient.setQueryData<BoardResponse>(queryKeyFor(boardPath()), (board) =>
      board
        ? {
            ...board,
            rows: board.rows.map((row) =>
              row.id === RETRY_CARD ? { ...row, observed_at: "2026-11-02T11:59:50.000Z" } : row,
            ),
          }
        : board,
    );
  });

  const decisions = screen.getByRole("region", { name: "Decisions" });
  const row = within(decisions).getByTestId(`inbox-row-${RETRY}`);
  expect(row.querySelector(".old, .warn")).toBeNull();
  const gone = screen.queryByRole("region", { name: "No longer current" });
  expect(gone ? within(gone).queryByTestId(`inbox-row-${RETRY}`) : null).toBeNull();

  await act(async () => {
    await router.navigate({ to: "/inbox", search: { item: RETRY } });
  });
  expect((await screen.findByTestId("current")).textContent).toContain("Still waiting");
  expect(screen.getByText(/30d 0h ago/)).toBeTruthy();

  await act(async () => {
    await router.navigate({ to: "/", search: { filter: "needs" } });
  });
  const tile = (await screen.findByText("Pick the retry policy for the export job")).closest("a");
  if (!tile) throw new Error("no tile for the held card");
  expect(tile.dataset.attn).toBe("true");
  expect(tile.querySelector(".obs")).toBeTruthy();
  expect(tile.querySelector(".obs.old")).toBeNull();
  expect(tile.textContent).toContain("Decision");
  expect(tile.textContent).not.toMatch(/stale/i);
});

test("a decision opened under a minute ago says just now, with no ago after it", async () => {
  stubDaemon("default");
  const { router, queryClient } = await renderInbox();
  act(() => {
    queryClient.setQueryData<AttentionResponse>(attentionQuery().queryKey, (attention) =>
      attention
        ? {
            ...attention,
            items: attention.items.map((item) =>
              item.id === RETRY ? { ...item, opened_at: "2026-11-02T11:59:40.000Z" } : item,
            ),
          }
        : attention,
    );
  });

  await act(async () => {
    await router.navigate({ to: "/inbox", search: { item: RETRY } });
  });
  await screen.findByTestId("current");
  expect(screen.getByText(/asked .* - just now$/)).toBeTruthy();
  expect(screen.queryByText(/just now ago/)).toBeNull();
});
