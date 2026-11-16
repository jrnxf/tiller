import { expect, test } from "vitest";

import { cardWait, spawnCommand } from "./wait";
import type { WaitInput } from "./wait";

const base: WaitInput = {
  queued: true,
  gated: false,
  projectName: "Lantern",
  peer: null,
  reachable: true,
  refusal: null,
  orchestrator: { activity: "idle" },
  health: { kind: "ok" },
};

test("a queued card waits for its project's orchestrator, and the owner may start it", () => {
  const idle = cardWait(base);
  expect(idle?.title).toBe("The Lantern orchestrator");
  expect(idle?.detail).toContain("next turn");
  expect(idle).toMatchObject({ stuck: false, startable: false, direct: true });
  const busy = cardWait({ ...base, orchestrator: { activity: "working" } });
  expect(busy?.detail).toContain("working now");
});

test("a project with no orchestrator says so and offers the start", () => {
  const wait = cardWait({ ...base, orchestrator: null });
  expect(wait?.title).toBe("Lantern has no orchestrator");
  expect(wait).toMatchObject({ stuck: true, startable: true, direct: false });
});

test("nothing is claimed before the agents read arrives", () => {
  expect(cardWait({ ...base, orchestrator: undefined })).toBeNull();
});

test("a silent or unreachable orchestrator is a wait that does not end by itself", () => {
  const at = "2026-11-16T05:00:00Z";
  const silent = cardWait({ ...base, health: { kind: "stale", since: null, ago: "40 min", at } });
  expect(silent?.title).toBe("The Lantern orchestrator, silent for 40 min");
  expect(silent).toMatchObject({ stuck: true, startable: false, direct: false });
  const gone = cardWait({
    ...base,
    health: { kind: "disconnected", since: null, ago: "2 h", at },
  });
  expect(gone?.detail).toContain("last seen 2 h ago");
  expect(gone?.stuck).toBe(true);
});

test("a peer that does not answer is named, and no start is offered", () => {
  const wait = cardWait({ ...base, peer: "peer", reachable: false, orchestrator: undefined });
  expect(wait?.title).toBe("peer to answer");
  expect(wait).toMatchObject({ stuck: true, startable: false, direct: false });
});

test("a held or blocked card, a started card and a closed one have no wait row", () => {
  expect(cardWait({ ...base, gated: true })).toBeNull();
  expect(cardWait({ ...base, queued: false })).toBeNull();
});

test("the shell command names the first role of the card's kind", () => {
  expect(spawnCommand("lantern-4", "ship")).toBe("tiller spawn lantern-4 builder");
  expect(spawnCommand("lantern-5", "scout")).toBe("tiller spawn lantern-5 scout");
  expect(spawnCommand("lantern-6", "question")).toBeNull();
});

test("a host that refuses every new run is the wait, before whose turn it is", () => {
  const refusal = {
    host: "peer",
    why: "4.6 GB of disk free, under the 40 GB floor",
    step: "Make room on peer, or lower the floor in Settings > Hosts on its own board.",
  };
  const wait = cardWait({ ...base, refusal });
  expect(wait?.title).toBe("peer to accept new runs");
  expect(wait?.detail).toBe(
    "peer refuses every new run: 4.6 GB of disk free, under the 40 GB floor. Make room on peer, or lower the floor in Settings > Hosts on its own board.",
  );
  // No spawn command is printed: the floor refuses that too.
  expect(wait).toMatchObject({ stuck: true, startable: false, direct: false });
  // With no orchestrator either, the floor is still the first thing in the way.
  expect(cardWait({ ...base, refusal, orchestrator: null })?.title).toBe("peer to accept new runs");
  // A held or blocked card keeps its own reason, and an unreachable host says nothing current.
  expect(cardWait({ ...base, refusal, gated: true })).toBeNull();
  expect(cardWait({ ...base, refusal, peer: "peer", reachable: false })?.title).toBe(
    "peer to answer",
  );
});
