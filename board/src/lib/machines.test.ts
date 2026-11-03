import { expect, test } from "vitest";

import type { Machine, MachinesResponse } from "./api-types";
import { fixtureSnapshot } from "./fixtures";
import { machineRowView } from "./machines";
import { machinesPath } from "./queries";

const NOW = Date.parse("2026-11-02T12:00:00.000Z");

function machines(scenario: "default" | "stale-machine" | "federated"): Machine[] {
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

function peer(label: string): Machine {
  const found = machines("federated").find((machine) => machine.label === label);
  if (!found) throw new Error(`no fixture peer ${label}`);
  return found;
}

test("a live act peer is tagged as a peer and shows its running agents", () => {
  const view = machineRowView(peer("host-d"), NOW);
  expect(view).toMatchObject({ peer: true, viewOnly: false, stale: false, linkReason: null });
  expect(view.summary).toBe("2 running");
});

test("a read pairing is live, view only, and gives the reason its controls are off", () => {
  const view = machineRowView(peer("host-f"), NOW);
  expect(view).toMatchObject({ peer: true, viewOnly: true, stale: false });
  expect(view.linkReason).toBe("view only pairing");
});

test("a down peer says unreachable since its last frame, in Lisbon time, amber", () => {
  const view = machineRowView(peer("host-e"), NOW);
  expect(view.staleText).toBe("unreachable since Mon 11:54");
  expect(view.linkReason).toBe("host-e is unreachable since Mon 11:54");
  expect(view.tone).toBe("amber");
});

test("a refused peer is red and names why the link was refused", () => {
  const view = machineRowView(peer("host-g"), NOW);
  expect(view.staleText).toBe("refused: key revoked");
  expect(view.linkReason).toBe("host-g refused the link: key revoked");
  expect(view.tone).toBe("red");
});

test("a host with no peers keeps the rows it always had", () => {
  for (const machine of machines("default")) {
    const view = machineRowView(machine, NOW);
    expect(view.peer).toBe(false);
    expect(view.linkReason).toBeNull();
  }
});
