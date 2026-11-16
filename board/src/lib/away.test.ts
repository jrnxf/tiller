import { expect, test } from "vitest";

import type { Project } from "./api-types";
import { ownBoards, uncoveredHosts, uncoveredNames } from "./away";

function project(id: string, host?: string): Project {
  return {
    id,
    name: id,
    ...(host ? { host, link: "live", as_of: "2026-11-02T12:00:00.000Z" } : {}),
  } as Project;
}

test("only a paired host's own projects are left out, counted per host", () => {
  const hosts = uncoveredHosts([
    project("alpha"),
    project("fleet"),
    project("pylon", "host-b"),
    project("orbit", "host-b"),
    project("fleet", "host-b"),
    project("delta", "host-c"),
  ]);
  expect(hosts).toEqual([
    { host: "host-b", projects: 2 },
    { host: "host-c", projects: 1 },
  ]);
  expect(uncoveredNames(hosts)).toBe("host-b's 2 projects and host-c's 1 project");
  expect(ownBoards(hosts)).toBe("the own boards of host-b and host-c");
  expect(ownBoards(hosts.slice(0, 1))).toBe("host-b's own board");
});

test("a host with no paired host leaves nothing out", () => {
  expect(uncoveredHosts([project("alpha"), project("fleet")])).toEqual([]);
});
