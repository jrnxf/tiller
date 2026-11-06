import { afterEach, expect, test } from "vitest";

import type { BoardRow } from "../../lib/api-types";
import { clockStore, syncClock } from "../../lib/clock";
import { agentActivity, cardDot, quietFor } from "./activity";

const NOW = Date.parse("2026-11-02T12:00:00.000Z");
const minutesAgo = (minutes: number) => new Date(NOW - minutes * 60_000).toISOString();

afterEach(() => clockStore.set({ offsetMs: 0, thresholds: {} }));

function agent(activity: string, observedMinutesAgo: number, role = "build") {
  return { activity, activity_observed_at: minutesAgo(observedMinutesAgo), role };
}

test("a worker turns stale after the snapshot's worker threshold, an orchestrator after its own", () => {
  syncClock(new Date(NOW).toISOString(), {
    worker_stale_minutes: 15,
    orchestrator_stale_minutes: 30,
  });
  expect(agentActivity(agent("working", 14), NOW).dot).toBe("live");
  expect(agentActivity(agent("working", 16), NOW)).toMatchObject({ dot: "stale", old: true });
  expect(agentActivity(agent("working", 16, "orchestrator"), NOW).dot).toBe("live");
  expect(agentActivity(agent("working", 31, "orchestrator"), NOW).dot).toBe("stale");
});

test("a quiet verdict says what it was, so stale is never mistaken for working", () => {
  syncClock(new Date(NOW).toISOString(), { worker_stale_minutes: 15 });
  expect(agentActivity(agent("idle", 20), NOW).label).toBe("Stale, was idle");
  expect(agentActivity(agent("working", 20), NOW).label).toBe("Stale, was working");
});

test("a permission prompt stays what it is however old it is, and an old error reads stale like the view says", () => {
  syncClock(new Date(NOW).toISOString(), { worker_stale_minutes: 15 });
  expect(agentActivity(agent("waiting_permission", 120), NOW).dot).toBe("waiting");
  expect(agentActivity(agent("waiting_input", 120), NOW).dot).toBe("waiting");
  expect(agentActivity(agent("errored", 5), NOW).dot).toBe("off");
  expect(agentActivity(agent("errored", 120), NOW).label).toBe("Stale, was errored");
  expect(agentActivity(agent("ended", 120), NOW).dot).toBe("idle");
});

test("an activity the board does not know reads as unknown, not as working", () => {
  expect(agentActivity(agent("something_new", 1), NOW).dot).toBe("unknown");
  expect(agentActivity(agent("unknown", 1), NOW).label).toBe("Unknown");
});

test("without thresholds in the snapshot the defaults apply", () => {
  expect(agentActivity(agent("working", 16), NOW).dot).toBe("stale");
});

const row = (column: BoardRow["column"], badge: string, needs_you = false) =>
  ({ column, badge, needs_you }) as BoardRow;

test("a card's dot follows its badge, and only live columns have one", () => {
  expect(cardDot(row("working", "working"))).toBe("live");
  expect(cardDot(row("working", "stale"))).toBe("stale");
  expect(cardDot(row("working", "errored"))).toBe("off");
  expect(cardDot(row("landing", "checks"))).toBe("live");
  expect(cardDot(row("working", "decision", true))).toBe("waiting");
  expect(cardDot(row("landing", "checks_next"))).toBe("idle");
  expect(cardDot(row("working", "parked"))).toBe("idle");
  expect(cardDot(row("landing", "blocked"))).toBe("idle");
  expect(cardDot(row("working", "filed"))).toBe("idle");
  expect(cardDot(row("queued", "filed"))).toBeNull();
  expect(cardDot(row("done", "landed"))).toBeNull();
});

// The quiet age for output and a state that began so many minutes ago.
function quiet(output: number | null, state: number) {
  return quietFor(
    { last_output_at: output === null ? null : minutesAgo(output), state_since: minutesAgo(state) },
    NOW,
  );
}

test("a working agent is quiet after 10 minutes with no output, counted from a newer prompt", () => {
  expect(quiet(12, 30)).toBe(12 * 60);
  expect(quiet(9, 30)).toBeNull();
  // Idle for hours, then prompted a minute ago: working, and not yet quiet.
  expect(quiet(180, 1)).toBeNull();
  // A host with hooks blocked reports no output: never quiet.
  expect(quiet(null, 30)).toBeNull();
});
