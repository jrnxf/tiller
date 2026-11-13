import { expect, test } from "vitest";

import { fixtureSnapshot } from "./fixtures";
import { agentsPath, attentionPath, boardPath, projectsPath } from "./queries";
import { attentionCount, dialogWaits, projectsOnHost, scopedProjects } from "./scope";
import type {
  AgentsResponse,
  AttentionResponse,
  BoardResponse,
  ProjectsResponse,
} from "./api-types";
import type { Prefs } from "./prefs";

const queries = fixtureSnapshot("default").queries;
const board = queries[boardPath()] as BoardResponse;
const attention = queries[attentionPath] as AttentionResponse;
const projects = (queries[projectsPath] as ProjectsResponse).projects;

const prefs: Prefs = {
  theme: "system",
  density: "comfortable",
  view: "all",
  hiddenProjects: [],
  focusProject: null,
};

test("without a machine filter every project is in scope", () => {
  expect(projectsOnHost(board.rows, undefined)).toBeNull();
  expect(scopedProjects(prefs, projects, board.rows, undefined)).toEqual(projects);
});

test("a machine filter keeps the projects that have a card there", () => {
  const host = board.rows.find((row) => row.host_id)?.host_id ?? undefined;
  const onHost = projectsOnHost(board.rows, host);
  expect(onHost?.size).toBeGreaterThan(0);
  const scoped = scopedProjects(prefs, projects, board.rows, host);
  expect(scoped.every((project) => onHost?.has(project.id))).toBe(true);
  expect(scopedProjects(prefs, projects, board.rows, "no-such-host")).toEqual([]);
});

test("the view knob and the machine filter narrow together", () => {
  const hidden = { ...prefs, hiddenProjects: [projects[0]?.id ?? ""] };
  expect(scopedProjects(hidden, projects, board.rows, undefined)).toHaveLength(projects.length - 1);
});

test("the Inbox badge counts only what is in scope", () => {
  const all = new Set(projects.map((project) => project.id));
  const total = attention.items.length + attention.permission_prompts.length;
  expect(attentionCount(attention, board.rows, all, undefined)).toBe(total);
  expect(attentionCount(attention, board.rows, new Set(), undefined)).toBeLessThan(total);
  expect(attentionCount(attention, board.rows, all, "no-such-host")).toBeLessThan(total);
});

test("the Inbox badge counts an agent that waits on a dialog, when it is in scope", () => {
  const dialog = fixtureSnapshot("dialog").queries;
  const rows = (dialog[boardPath()] as BoardResponse).rows;
  const waits = dialogWaits(dialog[agentsPath()] as AgentsResponse);
  expect(new Set(waits.map((agent) => agent.name))).toEqual(
    new Set(["alpha-42-build", "beta-7-build"]),
  );
  const all = new Set(projects.map((project) => project.id));
  const base = attentionCount(attention, rows, all, undefined);
  expect(attentionCount(attention, rows, all, undefined, waits)).toBe(base + 2);
  expect(attentionCount(attention, rows, new Set(["beta"]), undefined, waits)).toBe(
    attentionCount(attention, rows, new Set(["beta"]), undefined) + 1,
  );
});
