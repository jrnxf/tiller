import { expect, test } from "vitest";

import type { Machine } from "../../lib/api-types";
import { hostChoices } from "./project-add";

function machine(label: string, kind: Machine["kind"]): Machine {
  const peer: Partial<Machine> = kind === "remote" ? { link: "live", peer_scope: "act" } : {};
  return {
    id: label,
    label,
    kind,
    view_only: false,
    health: "ok",
    running_agents: 0,
    capacity: 2,
    free_slots: 2,
    last_seen_at: null,
    stale_reason: null,
    ...peer,
    rev: 1,
  };
}

test("the hosts are this host, then each paired peer", () => {
  const choices = hostChoices([machine("dev", "local"), machine("host-d", "remote")], "here");
  expect(choices.map(({ value, label, reason }) => ({ value, label, reason }))).toEqual([
    { value: "here", label: "dev (this host)", reason: null },
    { value: "peer:host-d", label: "host-d", reason: null },
  ]);
});

test("a peer unpaired while it is chosen stays chosen, and off", () => {
  const choices = hostChoices([machine("dev", "local")], "peer:host-d");
  expect(choices.at(-1)).toEqual({
    value: "peer:host-d",
    host: "host-d",
    label: "host-d",
    reason: "no longer paired",
  });
});
