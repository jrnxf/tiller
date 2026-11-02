import { afterEach, expect, test, vi } from "vitest";

import { focusAgent, focusCommand, focusPath } from "./herdr";

function answer(status: number, body: unknown) {
  const request = vi.fn(async () => new Response(JSON.stringify(body), { status }));
  vi.stubGlobal("fetch", request);
  return request;
}

const ok = { ok: true, replayed: false, events: [], receipt: null, data: null };
const refused = (code: string, message: string) => ({ ok: false, error: { code, message } });

afterEach(() => vi.unstubAllGlobals());

test("Open in herdr posts to the agent's focus endpoint as JSON with an idempotency key", async () => {
  const request = answer(200, ok);
  const result = await focusAgent("01JSESS");
  expect(result).toEqual({ ok: true });
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
  [503, refused("unavailable", "herdr is down"), /herdr did not answer/],
  [404, refused("not_found", "No such agent."), /No such agent\./],
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
  expect(result.ok ? "" : result.message).toMatch(/herdr did not answer/);
});

test("the command shown is the one herdr knows the agent by", () => {
  expect(focusCommand({ herdr_name: "alpha-42-build", role: "build" }, "alpha")).toBe(
    "herdr agent focus alpha-42-build",
  );
  expect(focusCommand({ herdr_name: null, role: "orchestrator" }, "alpha")).toBe(
    "herdr agent focus alpha-orch",
  );
  // Without a name from herdr the worker is named for its card, not its project.
  expect(focusCommand({ herdr_name: null, role: "build" }, "alpha", "alpha-42")).toBe(
    "herdr agent focus alpha-42-build",
  );
});
