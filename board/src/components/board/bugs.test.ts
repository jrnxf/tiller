import { expect, test } from "vitest";

import type { BoardRow, Machine, Project } from "../../lib/api-types";
import {
  bugProjectOf,
  cardName,
  isUntriaged,
  machineOf,
  moveKeys,
  moveTargets,
  movedBody,
} from "./bugs";

const machine = (label: string, extra: Partial<Machine> = {}): Machine => ({
  id: `m-${label}`,
  label,
  kind: "remote",
  view_only: false,
  health: "ok",
  running_agents: 0,
  capacity: 2,
  free_slots: 2,
  last_seen_at: null,
  stale_reason: null,
  rev: 3,
  ...extra,
});

const project = (id: string, host?: string): Project => ({
  id,
  name: id,
  delivery: "direct-pr",
  capacity: 2,
  rev: 1,
  stage: "launched",
  effective_delivery: "direct-pr",
  reviewed_through_sha: null,
  reviewed_at: null,
  stage_changed_at: null,
  edited_at: null,
  next_review_due: null,
  review_overdue: false,
  review_open: null,
  ...(host ? { host, link: "live" as const } : {}),
});

const bug: Pick<BoardRow, "category" | "project_id" | "column" | "display_id"> = {
  category: "tiller-bug",
  project_id: "fleet",
  column: "queued",
  display_id: "fleet-4",
};

test("only an open Tiller bug in fleet is untriaged, and never the Host health card", () => {
  expect(isUntriaged(bug)).toBe(true);
  expect(isUntriaged({ ...bug, category: null })).toBe(false);
  expect(isUntriaged({ ...bug, project_id: "tiller" })).toBe(false);
  expect(isUntriaged({ ...bug, column: "done" })).toBe(false);
  expect(isUntriaged({ ...bug, display_id: "fleet-1" })).toBe(false);
});

test("a bug project counts only while the projects read lists it on the same host", () => {
  const local = machine("host-a", { kind: "local", bug_project: "tiller" });
  const peer = machine("host-d", { link: "live", peer_scope: "act", bug_project: "tiller" });
  const projects = [project("tiller"), project("alpha", "host-d")];
  expect(bugProjectOf(local, projects)?.id).toBe("tiller");
  // host-d names tiller, but the only tiller the board knows is this host's.
  expect(bugProjectOf(peer, projects)).toBeUndefined();
  expect(bugProjectOf(machine("host-a", { kind: "local" }), projects)).toBeUndefined();
  expect(bugProjectOf(undefined, projects)).toBeUndefined();
});

test("Move offers this host first, then each peer the owner can act on", () => {
  const machines = [
    machine("host-d", { link: "live", peer_scope: "act", bug_project: "tiller" }),
    machine("host-e", { link: "down", peer_scope: "act", bug_project: "tiller" }),
    machine("host-f", { link: "live", peer_scope: "read", bug_project: "tiller" }),
    machine("host-a", { kind: "local", bug_project: "tiller" }),
  ];
  const projects = [
    project("tiller"),
    project("tiller", "host-d"),
    project("tiller", "host-e"),
    project("tiller", "host-f"),
  ];
  expect(moveTargets(machines, projects).map((target) => target.label)).toEqual([
    "host-a · tiller",
    "host-d · tiller",
  ]);
  expect(machineOf({ host: "host-d", link: "live" }, machines)?.label).toBe("host-d");
  expect(machineOf({}, machines)?.label).toBe("host-a");
});

test("a Move's keys hold for the card and target, and the new card links back", () => {
  const [target] = moveTargets(
    [machine("host-a", { kind: "local", bug_project: "tiller" })],
    [project("tiller")],
  );
  if (!target) throw new Error("no target");
  const keys = moveKeys({ id: "c1" }, target);
  expect(moveKeys({ id: "c1" }, target)).toEqual(keys);
  expect(moveKeys({ id: "c2" }, target).create).not.toBe(keys.create);
  expect(keys.create).not.toBe(keys.close);

  const from = cardName({ display_id: "fleet-4" }, "host-d");
  expect(from).toBe("host-d · fleet-4");
  expect(cardName({ display_id: "fleet-4" }, null)).toBe("fleet-4");
  expect(movedBody("It breaks.", from)).toBe("It breaks.\n\nMoved from host-d · fleet-4.");
  expect(movedBody(null, from)).toBe("Moved from host-d · fleet-4.");
  expect(movedBody("It breaks.", from, "alpha")).toBe(
    "It breaks.\n\nMoved from host-d · fleet-4, filed from alpha.",
  );
});
