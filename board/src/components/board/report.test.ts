import { describe, expect, it } from "vitest";

import type { AgentRow, AgentsResponse } from "../../lib/api-types";
import { cardReport, parseReport, reportLabel, reportRole, reportTone } from "./report";

function agent(over: Partial<AgentRow>): AgentRow {
  return {
    session_id: "s",
    role: "build",
    model: "sonnet",
    activity: "idle",
    activity_observed_at: null,
    last_status_line: null,
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
      agent({ card_id: "c1", last_status_line: "blocked: The build needs a token" }),
    ]);
    expect(cardReport(data, "c1")).toEqual({
      state: "blocked",
      text: "The build needs a token",
      role: "build",
    });
  });
});
