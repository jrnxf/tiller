import { expect, test } from "vitest";

import type { AwayBriefResponse } from "./api-types";
import {
  contractKeys,
  contractsOf,
  earliestUntil,
  readKey,
  runningOf,
  unreadBriefs,
  unreadHosts,
} from "./away";
import { awayBrief } from "./fixtures-away";

function brief(running: boolean, until: string, id = "01JAWAY0000000000000000000") {
  return { ...awayBrief(running), approval_id: id, until_at: until };
}

const both: AwayBriefResponse = {
  brief: brief(true, "2026-11-12T18:00:00.000Z"),
  hosts: [
    {
      host: "host-d",
      brief: brief(true, "2026-11-11T09:00:00.000Z", "01JAWAYD000000000000000000"),
    },
    { host: "host-g", brief: null },
    {
      host: "host-e",
      brief: null,
      error: { code: "peer_unreachable", message: "host-e is unreachable", host: "host-e" },
    },
  ],
};

test("one status covers this host's contract and each paired host's", () => {
  expect(contractsOf(both).map((contract) => contract.host)).toEqual([undefined, "host-d"]);
  const running = runningOf(both);
  expect(running).toHaveLength(2);
  // The header says when the first host is back.
  expect(earliestUntil(running)).toBe("2026-11-11T09:00:00.000Z");
  expect(unreadHosts(both)).toEqual([
    { host: "host-e", message: "host-e is unreachable, so its away mode is not known" },
  ]);
  expect(contractKeys(running[1]!)).toEqual(["host-d/alpha", "host-d/beta"]);
});

test("a peer's brief is read under its label, and this host's as before", () => {
  const back: AwayBriefResponse = {
    brief: brief(false, "2026-11-03T18:00:00.000Z"),
    hosts: [
      {
        host: "host-d",
        brief: brief(false, "2026-11-03T18:00:00.000Z", "01JAWAYD000000000000000000"),
      },
    ],
  };
  const [here, there] = contractsOf(back);
  expect(readKey(here!)).toBe("01JAWAY0000000000000000000");
  expect(readKey(there!)).toBe("host-d:01JAWAYD000000000000000000");
  expect(unreadBriefs(back, [])).toHaveLength(2);
  expect(unreadBriefs(back, ["host-d:01JAWAYD000000000000000000"])).toEqual([here]);
  // A running contract has no brief to read yet.
  expect(unreadBriefs(both, [])).toEqual([]);
});

test("an answer with no peers reads as this host's alone", () => {
  expect(contractsOf({ brief: null })).toEqual([]);
  expect(runningOf({ brief: brief(true, "2026-11-12T18:00:00.000Z") })).toHaveLength(1);
  expect(earliestUntil([])).toBeNull();
  // Another refusal keeps the peer's own words, after its label.
  const refused: AwayBriefResponse = {
    brief: null,
    hosts: [
      {
        host: "host-d",
        brief: null,
        error: { code: "peer_refused", message: "the key was revoked.", host: "host-d" },
      },
    ],
  };
  expect(unreadHosts(refused)).toEqual([
    { host: "host-d", message: "host-d: the key was revoked" },
  ]);
});
