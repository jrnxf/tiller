import { describe, expect, it } from "vitest";

import type { AgentRow, AgentsResponse } from "../../lib/api-types";
import { agentStopped, relaunchCommand, runStopped, stoppedNote } from "./stopped";

function agent(over: Partial<AgentRow>): AgentRow {
  return {
    session_id: "s1",
    role: "orchestrator",
    model: "opus",
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

function roster(...orchestrators: AgentRow[]): AgentsResponse {
  return { orchestrators, tiller_version: "0.1.0" };
}

const card = { id: "c1", badge: "unknown", closed_at: null };
const builder = agent({ session_id: "s2", role: "build", card_id: "c1", run_id: "r1" });

describe("agentStopped", () => {
  it("is true for an active run with no live agent", () => {
    expect(agentStopped(roster(agent({})), card, true)).toBe(true);
    expect(agentStopped(roster(agent({})), { ...card, badge: "idle" }, true)).toBe(true);
  });

  it("is false while a live agent works on the card", () => {
    expect(agentStopped(roster(agent({ workers: [builder] })), card, true)).toBe(false);
  });

  it("counts a worker whose orchestrator is gone, listed at the top level", () => {
    expect(agentStopped(roster(builder), card, true)).toBe(false);
  });

  it("says nothing before the agents read arrives or while the host is silent", () => {
    expect(agentStopped(undefined, card, true)).toBe(false);
    expect(agentStopped(roster(agent({})), card, false)).toBe(false);
  });

  it("is false for a card with no active run", () => {
    for (const badge of ["filed", "between_runs", "decision", "checks", "parked"]) {
      expect(agentStopped(roster(agent({})), { ...card, badge }, true)).toBe(false);
    }
    expect(
      agentStopped(roster(agent({})), { ...card, closed_at: "2026-11-16T07:00:00Z" }, true),
    ).toBe(false);
  });
});

describe("runStopped", () => {
  const run = { id: "r1", ended_at: null };

  it("names the run with no live session, not its live neighbour", () => {
    const reviewer = agent({ session_id: "s3", role: "review", card_id: "c1", run_id: "r2" });
    const live = roster(agent({ workers: [reviewer] }));
    expect(runStopped(live, run, true)).toBe(true);
    expect(runStopped(live, { id: "r2", ended_at: null }, true)).toBe(false);
  });

  it("is false for an ended run, an unread roster and a silent host", () => {
    const empty = roster(agent({}));
    expect(runStopped(empty, { id: "r1", ended_at: "2026-11-16T07:00:00Z" }, true)).toBe(false);
    expect(runStopped(undefined, run, true)).toBe(false);
    expect(runStopped(empty, run, false)).toBe(false);
  });
});

describe("stoppedNote", () => {
  it("names the orchestrator's move and when the owner hears of it", () => {
    const note = stoppedNote("Harbor", true);
    expect(note.detail).toContain("The Harbor orchestrator starts the agent again or ends the run");
    expect(note.detail).toContain("30 minutes");
    expect(note.startable).toBe(false);
  });

  it("offers the orchestrator start when none runs", () => {
    const note = stoppedNote("Harbor", false);
    expect(note.detail).toContain("Harbor has no orchestrator");
    expect(note.startable).toBe(true);
  });
});

it("prints the relaunch command with the run's name", () => {
  expect(relaunchCommand("harbor-1", "build")).toBe(
    'tiller agent relaunch harbor-1-build --note "The agent stopped. Continue from the worktree."',
  );
});
