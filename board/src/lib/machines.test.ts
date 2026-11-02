import { expect, test } from "vitest";

import type { Machine, MachinesResponse } from "./api-types";
import { fixtureSnapshot } from "./fixtures";
import { machineRowView } from "./machines";
import { machinesPath } from "./queries";

const NOW = Date.parse("2026-11-02T12:00:00.000Z");

function machines(scenario: "default" | "stale-machine"): Machine[] {
  return (fixtureSnapshot(scenario, NOW).queries[machinesPath] as MachinesResponse).machines;
}

test("a local host reads '3 running, 2 free' with no view-only tag", () => {
  const view = machineRowView(machines("default")[0] as Machine, NOW);
  expect(view.summary).toBe("3 running, 2 free");
  expect(view.viewOnly).toBe(false);
  expect(view.tone).toBe("green");
});

test("a remote host is view only and shows running agents without a free count", () => {
  const view = machineRowView(machines("default")[1] as Machine, NOW);
  expect(view.viewOnly).toBe(true);
  expect(view.summary).toBe("2 running");
});

test("a stale host shows how long ago it answered, amber, with the reason in the tooltip", () => {
  const view = machineRowView(machines("stale-machine")[2] as Machine, NOW);
  expect(view.stale).toBe(true);
  expect(view.staleText).toBe("last seen 17 min ago");
  expect(view.tone).toBe("amber");
  expect(view.tooltip).toContain("does not answer");
  expect(view.summary).toBeNull();
});

test("a host that was never seen says so", () => {
  const view = machineRowView(
    {
      ...(machines("default")[1] as Machine),
      health: "stale",
      last_seen_at: null,
      stale_reason: "never_seen",
    },
    NOW,
  );
  expect(view.staleText).toBe("never seen");
});
