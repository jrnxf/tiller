import { expect, test } from "vitest";

import type { AgentRow, AgentsResponse, BoardRow, Project } from "../../lib/api-types";
import { agentGroups, findLine, nextNeeding, stepLine } from "./agents-model";

const project = (id: string): Project => ({
  id,
  name: id,
  delivery: "x",
  capacity: 1,
  rev: 1,
  stage: "launched",
  effective_delivery: "x",
  reviewed_through_sha: null,
  reviewed_at: null,
  stage_changed_at: null,
  edited_at: null,
  next_review_due: null,
  review_overdue: false,
  review_open: null,
});

function agent(id: string, role: string, host: string, extra: Partial<AgentRow> = {}): AgentRow {
  return {
    session_id: id,
    role,
    model: "opus",
    activity: "working",
    activity_observed_at: "2026-11-02T12:00:00.000Z",
    last_status_line: null,
    herdr_name: null,
    card_id: null,
    host_id: host,
    workers: [],
    project_id: "alpha",
    ...extra,
  };
}

const rows = [
  { id: "c1", display_id: "alpha-1", title: "one" },
  { id: "c2", display_id: "alpha-2", title: "two" },
  { id: "c3", display_id: "alpha-3", title: "three" },
] as BoardRow[];

const roster: AgentsResponse = {
  orchestrators: [
    agent("o", "orchestrator", "h1", {
      workers: [
        agent("w1", "build", "h1", { card_id: "c1" }),
        agent("w2", "review", "h2", { card_id: "c2" }),
        agent("w3", "scout", "h1", { card_id: "c3" }),
      ],
    }),
    agent("o2", "orchestrator", "h1", { project_id: "beta" }),
  ],
};

test("workers nest under their orchestrator, the last one marked closed", () => {
  const [group] = agentGroups(roster, [project("alpha")], rows, undefined);
  expect(group?.lines.map((line) => line.label)).toEqual([
    "alpha",
    "├ alpha-1 build",
    "├ alpha-2 review",
    "└ alpha-3 scout",
  ]);
});

test("only the projects the view shows get a block", () => {
  const groups = agentGroups(roster, [project("beta")], rows, undefined);
  expect(groups.map((group) => group.project.id)).toEqual(["beta"]);
  expect(groups[0]?.lines).toHaveLength(1);
});

test("a machine filter keeps that machine's agents and redraws the tree glyphs", () => {
  const [group] = agentGroups(roster, [project("alpha")], rows, "h1");
  expect(group?.lines.map((line) => line.label)).toEqual([
    "alpha",
    "├ alpha-1 build",
    "└ alpha-3 scout",
  ]);
});

test("a project whose agents are all on another machine drops out of the filter", () => {
  const groups = agentGroups(roster, [project("alpha"), project("beta")], rows, "h2");
  expect(groups.map((group) => group.project.id)).toEqual(["alpha"]);
  expect(groups[0]?.lines.map((line) => line.agent.session_id)).toEqual(["w2"]);
});

test("a selection that is gone falls back to the first agent", () => {
  const groups = agentGroups(roster, [project("alpha")], rows, undefined);
  expect(findLine(groups, "w2")?.line.agent.session_id).toBe("w2");
  expect(findLine(groups, "missing")?.line.agent.session_id).toBe("o");
  expect(findLine([], "w2")).toBeNull();
});

test("J and K walk the rail across projects and wrap at the ends", () => {
  const groups = agentGroups(roster, [project("alpha"), project("beta")], rows, undefined);
  const id = (from: string, by: 1 | -1) => stepLine(groups, from, by)?.line.agent.session_id;
  expect(id("w3", 1)).toBe("o2");
  expect(id("o2", 1)).toBe("o");
  expect(id("o", -1)).toBe("o2");
  expect(id("missing", 1)).toBe("o");
  expect(stepLine([], "o", 1)).toBeNull();
});

test("N finds the next agent that needs you after the current one, wrapping", () => {
  const groups = agentGroups(roster, [project("alpha"), project("beta")], rows, undefined);
  const waiting = new Set(["w1", "o2"]);
  const next = (from: string | undefined, set = waiting) =>
    nextNeeding(groups, from, (row) => set.has(row.session_id))?.line.agent.session_id;
  expect(next("w1")).toBe("o2");
  expect(next("o2")).toBe("w1");
  expect(next(undefined)).toBe("w1");
  expect(next("w1", new Set(["w1"]))).toBe("w1");
  expect(next("w1", new Set())).toBeUndefined();
});

test("a worker whose orchestrator has ended is listed on its own, in its project's block", () => {
  const orphaned: AgentsResponse = {
    orchestrators: [
      agent("w4", "build", "h1", { card_id: "c1" }),
      agent("o2", "orchestrator", "h1", { project_id: "beta" }),
    ],
  };
  const groups = agentGroups(orphaned, [project("alpha"), project("beta")], rows, undefined);
  expect(groups.map((group) => group.project.id)).toEqual(["alpha", "beta"]);
  expect(groups[0]?.lines).toMatchObject([
    { label: "└ alpha-1 build", orchestrator: false, name: "alpha-1" },
  ]);
  expect(agentGroups(orphaned, [project("alpha")], rows, "h2")).toEqual([]);
});

test("workers listed on their own close the tree only once, at the block's end", () => {
  const orphaned: AgentsResponse = {
    orchestrators: [
      agent("w4", "build", "h1", { card_id: "c1" }),
      agent("w5", "review", "h1", { card_id: "c2" }),
    ],
  };
  const [group] = agentGroups(orphaned, [project("alpha")], rows, undefined);
  expect(group?.lines.map((line) => line.label)).toEqual(["├ alpha-1 build", "└ alpha-2 review"]);
});
