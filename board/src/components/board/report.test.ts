import { describe, expect, it } from "vitest";

import type { AgentRow, AgentsResponse, AttentionItem, TimelineEntry } from "../../lib/api-types";
import {
  answeredSince,
  cardReport,
  parseReport,
  reportLabel,
  reportRole,
  reportTone,
} from "./report";

function agent(over: Partial<AgentRow>): AgentRow {
  return {
    session_id: "s",
    role: "build",
    model: "sonnet",
    activity: "idle",
    activity_observed_at: null,
    last_status_line: null,
    last_status_at: null,
    name: null,
    card_id: null,
    host_id: "h1",
    workers: [],
    ...over,
  };
}

function roster(workers: AgentRow[]): AgentsResponse {
  return {
    orchestrators: [agent({ session_id: "o", role: "orchestrator", workers })],
  } as AgentsResponse;
}

describe("a worker's report", () => {
  it("splits the stored line into its state and its words", () => {
    expect(parseReport("blocked: The mail server refuses the login", "build")).toEqual({
      state: "blocked",
      text: "The mail server refuses the login",
      role: "build",
    });
    expect(parseReport("needs-decision: Keep the v1 route: yes or no?", "scout")?.text).toBe(
      "Keep the v1 route: yes or no?",
    );
  });

  it("is absent for no line, an unknown state or no words", () => {
    expect(parseReport(null, "build")).toBeNull();
    expect(parseReport("Waiting on alpha-1", "build")).toBeNull();
    expect(parseReport("napping: until noon", "build")).toBeNull();
    expect(parseReport("blocked:  ", "build")).toBeNull();
  });

  it("reads as the worker's word, never as a verdict", () => {
    const done = parseReport("done: All 20 tests pass", "build");
    expect(done && reportLabel(done)).toBe("Reports done");
    expect(done && reportTone(done)).toBe("ok");
    expect(done && reportRole(done)).toBe("Builder");
    const blocked = parseReport("blocked: No key", "verify");
    expect(blocked && reportLabel(blocked)).toBe("Reports blocked");
    expect(blocked && reportTone(blocked)).toBe("warn");
    expect(blocked && reportRole(blocked)).toBe("Verifier");
  });

  it("is the report of that card's worker only", () => {
    const data = roster([
      agent({ card_id: "c1", last_status_line: "working: Wrote the parser" }),
      agent({ card_id: "c2", last_status_line: "blocked: No key" }),
    ]);
    expect(cardReport(data, "c1")?.text).toBe("Wrote the parser");
    expect(cardReport(data, "c2")?.state).toBe("blocked");
    expect(cardReport(data, "c3")).toBeNull();
    expect(cardReport(undefined, "c1")).toBeNull();
  });

  it("shows the worker that stopped when two workers of a card reported", () => {
    const data = roster([
      agent({ card_id: "c1", role: "review", last_status_line: "working: Reading the diff" }),
      agent({
        card_id: "c1",
        last_status_line: "blocked: The build needs a token",
        last_status_at: "2026-11-16T14:00:00Z",
      }),
    ]);
    // With its time, so the tile can say how old it is (UA-19).
    expect(cardReport(data, "c1")).toEqual({
      state: "blocked",
      text: "The build needs a token",
      role: "build",
      at: "2026-11-16T14:00:00Z",
    });
  });
});

function item(over: Partial<AttentionItem>): AttentionItem {
  return {
    id: "a1",
    card_id: "c1",
    kind: "decision",
    question: "Overlap or cut over?",
    evidence: null,
    recommendation: null,
    consequence: null,
    options: null,
    approval: null,
    opened_by_session_id: "o",
    opened_at: "2026-11-16T13:41:04.000Z",
    still_current: true,
    answered_at: null,
    withdrawn_at: null,
    rev: 1,
    ...over,
  };
}

function line(at: string, actor: TimelineEntry["actor"]): TimelineEntry {
  return { seq: 1, at, actor, text: "" };
}

function timeline(entries: TimelineEntry[], more = false) {
  return { entries, next_before: more ? 1 : null };
}

describe("a stop report the owner answered since", () => {
  const asks = parseReport("needs-decision: Two rotation plans; need the owner's pick", "build");
  const reported = line("2026-11-16T13:41:04.000Z", "worker");
  const answeredAt = "2026-11-16T13:44:51.000Z";

  it("is the answer's time when the owner answered after the worker's report", () => {
    const answered = [item({ answered_at: answeredAt })];
    const lines = timeline([line(answeredAt, "you"), reported]);
    expect(answeredSince(asks, answered, lines)).toBe(answeredAt);
    const blocked = parseReport("blocked: No key", "build");
    expect(answeredSince(blocked, answered, lines)).toBe(answeredAt);
  });

  it("is null while a question of the card is still open", () => {
    const items = [item({ answered_at: answeredAt }), item({ id: "a2" })];
    expect(answeredSince(asks, items, timeline([reported]))).toBeNull();
  });

  it("is null when the worker spoke after the answer", () => {
    const items = [item({ answered_at: answeredAt })];
    const later = line("2026-11-16T13:50:00.000Z", "worker");
    expect(answeredSince(asks, items, timeline([later, line(answeredAt, "you"), reported]))).toBe(
      null,
    );
  });

  it("is null with no answer, no reads yet, or a report that asks nothing", () => {
    expect(answeredSince(asks, [item({})], timeline([reported]))).toBeNull();
    expect(answeredSince(asks, [], timeline([reported]))).toBeNull();
    expect(answeredSince(asks, undefined, timeline([reported]))).toBeNull();
    expect(answeredSince(asks, [item({ answered_at: answeredAt })], undefined)).toBeNull();
    const done = parseReport("done: All tests pass", "build");
    expect(answeredSince(done, [item({ answered_at: answeredAt })], timeline([reported]))).toBe(
      null,
    );
    expect(answeredSince(null, [item({ answered_at: answeredAt })], timeline([]))).toBeNull();
  });

  it("bounds the report by the oldest line of a full page that has no worker line", () => {
    const items = [item({ answered_at: answeredAt })];
    const page = [line(answeredAt, "you"), line("2026-11-16T13:42:00.000Z", "system")];
    expect(answeredSince(asks, items, timeline(page, true))).toBe(answeredAt);
    // A short page with no worker line holds no report to compare with.
    expect(answeredSince(asks, items, timeline(page, false))).toBeNull();
  });

  it("takes the newest answer of the card", () => {
    const items = [
      item({ answered_at: "2026-11-16T13:40:00.000Z" }),
      item({ id: "a2", answered_at: answeredAt }),
    ];
    expect(answeredSince(asks, items, timeline([reported]))).toBe(answeredAt);
    // Only an older answer: the report came after it and stands.
    expect(
      answeredSince(
        asks,
        [item({ answered_at: "2026-11-16T13:40:00.000Z" })],
        timeline([reported]),
      ),
    ).toBeNull();
  });
});
