import { expect, test } from "vitest";

import type { Machine, MachinesResponse } from "./api-types";
import { fixtureSnapshot } from "./fixtures";
import {
  ambiguousIds,
  asOfText,
  focusReason,
  hostField,
  isFederated,
  linkReason,
  ownerKey,
  ownerOf,
  projectGroups,
  projectIndex,
  projectKey,
  refusalLabel,
  shownId,
  standingOf,
} from "./peers";
import { machinesPath } from "./queries";

// Tue 3 Nov 2026, 12:00 in Lisbon (WET, UTC+0).
const NOW = Date.parse("2026-11-03T12:00:00.000Z");

function machinesOf(scenario: "default" | "federated"): Machine[] {
  return (fixtureSnapshot(scenario, NOW).queries[machinesPath] as MachinesResponse).machines;
}

const FIVE_MIN_AGO = "2026-11-03T11:55:00.000Z";

test("a host with no peers is not federated and every row stands as live and local", () => {
  const machines = machinesOf("default");
  expect(isFederated(machines)).toBe(false);
  const standing = standingOf({}, machines);
  expect(standing).toMatchObject({ peer: false, live: true, dim: false, canAct: true });
  expect(standing.reason).toBeNull();
});

test("a row of a live act peer acts like a local one", () => {
  const standing = standingOf(
    { host: "host-d", link: "live", as_of: FIVE_MIN_AGO },
    machinesOf("federated"),
  );
  expect(standing).toMatchObject({ peer: true, live: true, dim: false, canAct: true });
  expect(standing.reason).toBeNull();
});

test("a row of a read pairing is live and says it is view only", () => {
  const standing = standingOf({ host: "host-f", link: "live" }, machinesOf("federated"));
  expect(standing).toMatchObject({ live: true, dim: false, canAct: false });
  expect(standing.reason).toBe("view only pairing");
});

test("a row of a down peer is dimmed, cannot act and says since when, in Lisbon time", () => {
  const standing = standingOf(
    { host: "host-e", link: "live", as_of: "2026-11-03T11:54:00.000Z" },
    machinesOf("federated"),
  );
  // The Machines row says the link is down even though the row was tagged live.
  expect(standing).toMatchObject({ live: false, dim: true, canAct: false });
  expect(standing.reason).toBe("host-e is unreachable since Tue 11:54");
});

test("a refused peer names the refusal", () => {
  const standing = standingOf({ host: "host-g", link: "refused" }, machinesOf("federated"));
  expect(standing.reason).toBe("host-g refused the link: key revoked");
});

test("a peer the Machines list does not know yet stands as never connected", () => {
  const standing = standingOf({ host: "host-z", link: null as never }, []);
  expect(standing.peer).toBe(false);
  const tagged = standingOf({ host: "host-z", link: "never" }, []);
  expect(tagged).toMatchObject({ peer: true, dim: true, canAct: false });
  expect(tagged.reason).toBe("host-z has not connected yet");
});

test("link reasons read as one sentence each", () => {
  expect(linkReason("host-b", "live", null)).toBe("");
  expect(linkReason("host-b", "lagging", FIVE_MIN_AGO)).toBe(
    "host-b is not answering since Tue 11:55",
  );
  expect(linkReason("host-b", "down", null)).toBe("host-b is unreachable");
  expect(linkReason("host-b", "refused", null, "bad_sig")).toBe(
    "host-b refused the link: signature rejected",
  );
  expect(linkReason("host-b", "refused", null, "something_new")).toBe(
    "host-b refused the link: something new",
  );
  expect(refusalLabel(null)).toBeNull();
});

test("'as of' carries the Lisbon weekday and time and the age", () => {
  expect(asOfText(null, NOW)).toBe("as of never");
  expect(asOfText("2026-11-03T11:59:40.000Z", NOW)).toBe("as of Tue 11:59, just now");
  expect(asOfText("2026-11-03T11:54:00.000Z", NOW)).toBe("as of Tue 11:54, 6 min ago");
  expect(asOfText("2026-11-03T09:00:00.000Z", NOW)).toBe("as of Tue 09:00, 3 h ago");
  expect(asOfText("2026-10-31T12:00:00.000Z", NOW)).toBe("as of Sat 12:00, 3 d ago");
});

test("the time is Lisbon's whatever the browser zone: summer time is an hour ahead", () => {
  const july = Date.parse("2026-07-07T12:00:00.000Z");
  expect(asOfText("2026-07-07T11:00:00.000Z", july)).toBe("as of Tue 12:00, 1 h ago");
});

test("only a display id more than one host uses is host-qualified", () => {
  const rows = [
    { display_id: "gamma-12" },
    { display_id: "gamma-12", host: "host-d" },
    { display_id: "fleet-3", host: "host-d" },
    { display_id: "fleet-3", host: "host-d" },
  ];
  const ambiguous = ambiguousIds(rows);
  expect([...ambiguous]).toEqual(["gamma-12"]);
  expect(shownId({ display_id: "gamma-12", host: "host-d" }, ambiguous)).toBe("host-d · gamma-12");
  expect(shownId({ display_id: "fleet-3", host: "host-d" }, ambiguous)).toBe("fleet-3");
  // This host's own row has no host named, so it keeps the plain id.
  expect(shownId({ display_id: "gamma-12" }, ambiguous)).toBe("gamma-12");
});

test("a peer's project and a local one with the same slug get different keys", () => {
  expect(ownerKey("gamma", {})).toBe("gamma");
  expect(ownerKey("gamma", { host: "host-d", link: "live" })).toBe("host-d/gamma");
  expect(projectKey({ id: "gamma", host: "host-d", link: "down" })).toBe("host-d/gamma");
  const projects = [{ id: "gamma" }, { id: "gamma", host: "host-d", link: "live" as const }];
  expect(projectIndex(projects, { project_id: "gamma", host: "host-d", link: "live" })).toBe(1);
  expect(projectIndex(projects, { project_id: "gamma" })).toBe(0);
});

test("a mutation names the owning host only for a peer's row", () => {
  expect(hostField({})).toEqual({});
  expect(hostField({ host: "host-d", link: "live" })).toEqual({ host: "host-d" });
  expect(ownerOf({ host: "host-d" })).toBeUndefined();
  expect(focusReason(undefined)).toBeNull();
  expect(focusReason("host-d")).toBe("host-d owns this pane: open it from that host");
});

test("the picker groups projects by host, this host first, and turns off a group that cannot act", () => {
  const machines = machinesOf("federated");
  const projects = (
    fixtureSnapshot("federated", NOW).queries["/api/v1/projects"] as {
      projects: { id: string; host?: string; link?: Machine["link"] }[];
    }
  ).projects;
  const groups = projectGroups(projects, machines, "host-a");
  expect(groups.map((group) => [group.host, group.peer, group.standing.canAct])).toEqual([
    ["host-a", false, true],
    ["host-d", true, true],
    ["host-e", true, false],
    ["host-f", true, false],
  ]);
  expect(groups[2]?.standing.reason).toContain("host-e is unreachable");
  expect(groups[3]?.standing.reason).toBe("view only pairing");
});
