import { afterEach, expect, test, vi } from "vitest";

import { attachCommand, focusAgent, focusOutcome, focusPath } from "./terminal-open";

function answer(status: number, body: unknown) {
  const request = vi.fn(async () => new Response(JSON.stringify(body), { status }));
  vi.stubGlobal("fetch", request);
  return request;
}

const ok = { ok: true, replayed: false, events: [], receipt: null, data: null };
const refused = (code: string, message: string) => ({ ok: false, error: { code, message } });

afterEach(() => vi.unstubAllGlobals());

test("Open in terminal posts to the agent's focus endpoint as JSON with an idempotency key", async () => {
  const request = answer(200, ok);
  const result = await focusAgent("01JSESS");
  expect(result).toEqual({ ok: true, data: null });
  expect(request).toHaveBeenCalledTimes(1);
  const [path, init] = request.mock.calls[0] as unknown as [string, RequestInit];
  expect(path).toBe(focusPath("01JSESS"));
  expect(path).toBe("/api/v1/agents/01JSESS/focus");
  expect(init.method).toBe("POST");
  expect(new Headers(init.headers).get("content-type")).toBe("application/json");
  expect(JSON.parse(String(init.body))).toEqual({
    idempotency_key: expect.stringMatching(/^[0-9A-Z]{26}$/),
  });
});

test("each click is a new jump, so each one carries a new key", async () => {
  const request = answer(200, ok);
  await focusAgent("s");
  await focusAgent("s");
  const bodies = request.mock.calls.map((call) =>
    String((call as unknown as [string, RequestInit])[1].body),
  );
  const keys = bodies.map(
    (body) => (JSON.parse(body) as { idempotency_key: string }).idempotency_key,
  );
  expect(new Set(keys).size).toBe(2);
});

test.each([
  [403, refused("forbidden", "no owner cookie"), /not signed in as the owner/],
  [
    503,
    refused("unavailable", "the terminal holder is unreachable: no socket"),
    /^The terminal holder did not switch to the pane, so nothing was opened: the terminal holder is unreachable: no socket$/,
  ],
  [404, refused("not_found", "No such agent."), /^No such agent\.$/],
  [
    502,
    refused("peer_refused", "host-d is paired with a read scope"),
    /^host-d is paired with a read scope$/,
  ],
])("a %i answer is shown as a readable message", async (status, body, message) => {
  answer(status, body);
  const result = await focusAgent("s");
  expect(result.ok).toBe(false);
  expect(result.ok ? "" : result.message).toMatch(message);
});

test("a daemon that does not answer is reported, not thrown", async () => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => {
      throw new TypeError("network");
    }),
  );
  const result = await focusAgent("s");
  expect(result.ok ? "" : result.message).toBe("The daemon did not answer, so nothing was opened.");
});

test("the command shown attaches to the agent's session", () => {
  expect(attachCommand({ session_id: "01JSESS0000000000000000001" })).toBe(
    "tiller attach 01JSESS0000000000000000001",
  );
});

test("the words say what the daemon did with the terminal", () => {
  expect(focusOutcome({ opened: "focused", terminal: null }, "s")).toEqual({
    tone: "ok",
    title: "Switched the terminal to the agent",
    detail: "A tiller attach window on this host shows the agent's pane.",
    command: null,
  });
  expect(focusOutcome({ opened: "raised", terminal: "Ghostty" }, "s")).toEqual({
    tone: "ok",
    title: "Brought Ghostty to the front",
    detail: "Its tiller attach window shows the agent's pane.",
    command: null,
  });
  expect(focusOutcome({ opened: "launched", terminal: "Ghostty" }, "s")).toEqual({
    tone: "ok",
    title: "Opened the agent in Ghostty",
    detail: "The new window runs tiller attach on the agent's pane.",
    command: null,
  });
  // A daemon that says nothing of the window only switched.
  expect(focusOutcome(null, "s").title).toBe("Switched the terminal to the agent");
});

test("nothing here that shows the pane gives the command to run", () => {
  expect(focusOutcome({ session_id: "01JSESS", opened: "unseen", terminal: null }, "s")).toEqual({
    tone: "bad",
    title: "No terminal here shows the agent",
    detail: "Run this in a terminal on the daemon's host:",
    command: "tiller attach 01JSESS",
  });
  // The daemon's reason, when it gives one, goes first.
  expect(
    focusOutcome({ opened: "unseen", terminal: null, problem: "no screen here" }, "01JS").detail,
  ).toBe("no screen here. Run this in a terminal on the daemon's host:");
});

test("an app that would not open shows the daemon's problem", () => {
  expect(
    focusOutcome(
      { opened: "failed", terminal: "Ghostty", problem: "no attach client came up within 10s" },
      "s",
    ),
  ).toEqual({
    tone: "bad",
    title: "Ghostty did not open",
    detail: "no attach client came up within 10s",
    command: null,
  });
  expect(focusOutcome({ opened: "failed", terminal: null }, "s").detail).toBe(
    "The terminal app did not open on the daemon's host.",
  );
});
