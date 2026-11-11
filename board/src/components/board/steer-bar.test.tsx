import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { Interrupted } from "../../lib/api-types";
import { receiptsStore, recordProgress } from "../../lib/receipts";
import { NOT_WORKING, SteerBar } from "./steer-bar";

const STEER = "/api/v1/agents/s1/steer";
const INTERRUPT = "/api/v1/agents/s1/interrupt";

type Reply = { status: number; body: unknown };

function recorded(data: unknown, outbox: string | null): Reply {
  const receipt = { recorded_at: "2026-11-11T10:00:00Z", outbox_id: outbox, approval_id: null };
  return { status: 200, body: { ok: true, replayed: false, events: [1], receipt, data } };
}

function interrupted(cancel: Interrupted["cancel"]): Reply {
  const evidence = cancel === "confirmed" ? "observation Stop after the Escape" : "no Stop in 10 s";
  return recorded({ run: "r1", session: "s1", pane_id: "p1", cancel, evidence }, null);
}

// The daemon's two control routes: each call takes the next reply.
function stubDaemon(replies: Reply[]) {
  const calls: { path: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push({ path: String(input), body: JSON.parse(String(init?.body ?? "{}")) });
      const reply = replies.shift() ?? { status: 500, body: {} };
      return new Response(JSON.stringify(reply.body), { status: reply.status });
    }),
  );
  return calls;
}

function bar(off: string | null = null) {
  return render(<SteerBar sessionId="s1" name="alpha-42" off={off} />);
}

function interruptButton() {
  return screen.getByRole("button", { name: "Interrupt..." });
}

beforeEach(() => receiptsStore.set({ byKey: {}, progress: {} }));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("a sent line is queued until the pane is idle, then typed", async () => {
  const calls = stubDaemon([
    recorded(
      { id: "o1", target_session_id: "s1", kind: "steer", state: "pending", body: "Stop at 16 s." },
      "o1",
    ),
  ]);
  bar();
  expect(screen.getByText("Typed when the pane is idle. One line per send.")).toBeTruthy();
  const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
  expect(send.disabled).toBe(true);
  const input = screen.getByRole("textbox", { name: "Message" }) as HTMLInputElement;
  fireEvent.change(input, { target: { value: "  Stop at 16 s.  " } });
  fireEvent.click(send);
  const queued = await screen.findByRole("status");
  expect(queued.textContent).toBe("Queued.Stop at 16 s.Typed when the pane is idle.");
  expect(input.value).toBe("");
  expect(calls).toHaveLength(1);
  expect(calls[0]?.path).toBe(STEER);
  expect(calls[0]?.body.text).toBe("Stop at 16 s.");
  expect(typeof calls[0]?.body.idempotency_key).toBe("string");
  act(() => recordProgress("o1", "delivered", "2026-11-11T10:00:07Z"));
  expect(screen.getByRole("status").textContent).toMatch(/^Typed\.Stop at 16 s\.Into the pane at /);
});

test("a refused line stays in the bar with the reason", async () => {
  stubDaemon([
    {
      status: 400,
      body: { ok: false, error: { code: "invalid", message: "the message is empty" } },
    },
  ]);
  bar();
  const input = screen.getByRole("textbox", { name: "Message" }) as HTMLInputElement;
  fireEvent.change(input, { target: { value: "hello" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  expect((await screen.findByRole("alert")).textContent).toBe("the message is empty");
  expect(input.value).toBe("hello");
});

test("Interrupt is off while the agent is not working, and says why", () => {
  const calls = stubDaemon([]);
  bar(NOT_WORKING);
  const button = interruptButton() as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  expect(button.title).toBe("Only while the agent is working");
  fireEvent.click(button);
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(calls).toEqual([]);
});

test("Interrupt asks first, and keep working sends nothing", () => {
  const calls = stubDaemon([]);
  bar();
  fireEvent.click(interruptButton());
  const dialog = screen.getByRole("dialog", { name: "Interrupt alpha-42?" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Keep working" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(calls).toEqual([]);
});

test("a confirmed interrupt says the pane stopped", async () => {
  const calls = stubDaemon([interrupted("confirmed")]);
  bar();
  fireEvent.click(interruptButton());
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Interrupt" }));
  const result = await screen.findByRole("status");
  expect(result.textContent).toMatch(/^Interrupt confirmed\. The pane stopped at /);
  expect(result.title).toBe("observation Stop after the Escape");
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(calls.map(({ path }) => path)).toEqual([INTERRUPT]);
});

test("an unconfirmed interrupt says so, and a second press is a new key", async () => {
  const calls = stubDaemon([interrupted("unconfirmed"), interrupted("confirmed")]);
  bar();
  fireEvent.click(interruptButton());
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Interrupt" }));
  const warning = await screen.findByRole("alert");
  expect(warning.textContent).toMatch(/^Not confirmed after 10 s\. The pane still shows work\./);
  fireEvent.click(interruptButton());
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Interrupt" }));
  expect((await screen.findByRole("status")).textContent).toMatch(/^Interrupt confirmed\./);
  expect(calls[0]?.body.idempotency_key).not.toBe(calls[1]?.body.idempotency_key);
});

test("a pending interrupt counts down the daemon's watch of the pane", async () => {
  vi.useFakeTimers();
  try {
    let answer: ((reply: Response) => void) | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(() => new Promise<Response>((resolve) => (answer = resolve))),
    );
    bar();
    fireEvent.click(interruptButton());
    const dialog = screen.getByRole("dialog");
    expect(dialog.textContent).toContain("The board watches the pane for 10 s");
    fireEvent.click(within(dialog).getByRole("button", { name: "Interrupt" }));
    await act(async () => {});
    expect(dialog.textContent).toContain("Watching the pane: 10 s left");
    await act(async () => vi.advanceTimersByTime(1_000));
    expect(dialog.textContent).toContain("Watching the pane: 9 s left");
    await act(async () => vi.advanceTimersByTime(8_500));
    expect(dialog.textContent).toContain("Watching the pane: 1 s left");
    await act(async () => vi.advanceTimersByTime(1_000));
    expect(dialog.textContent).toContain("Watching the pane: reading the result");
    const reply = interrupted("confirmed");
    await act(async () => answer?.(new Response(JSON.stringify(reply.body), { status: 200 })));
    expect(screen.queryByRole("dialog")).toBeNull();
  } finally {
    vi.useRealTimers();
  }
});

test("a refused interrupt keeps the dialog open with the reason", async () => {
  const message = "alpha-42 is not working; nothing was sent";
  stubDaemon([{ status: 422, body: { ok: false, error: { code: "refused", message } } }]);
  bar();
  fireEvent.click(interruptButton());
  const dialog = screen.getByRole("dialog");
  fireEvent.click(within(dialog).getByRole("button", { name: "Interrupt" }));
  expect((await within(dialog).findByRole("alert")).textContent).toBe(message);
  expect(screen.getByRole("dialog")).toBeTruthy();
});
