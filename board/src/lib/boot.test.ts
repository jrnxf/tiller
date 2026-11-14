import { QueryClient } from "@tanstack/react-query";
import { expect, test } from "vitest";

import { hydrateFromSnapshot, loadBoot } from "./boot";
import { clockStore } from "./clock";
import { fixtureSnapshot } from "./fixtures";
import {
  agentsQuery,
  attentionQuery,
  boardQuery,
  machinesQuery,
  projectsQuery,
  quotaQuery,
  ReadError,
} from "./queries";

const NOW = Date.parse("2026-11-02T12:00:00.000Z");

test("every snapshot entry lands under the key its query option factory reads", () => {
  const queryClient = new QueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot("default", NOW));

  for (const options of [
    boardQuery(),
    attentionQuery(),
    agentsQuery(),
    machinesQuery(),
    quotaQuery(),
    projectsQuery(),
  ]) {
    expect(queryClient.getQueryData(options.queryKey), String(options.queryKey[1])).toBeDefined();
  }
});

test("the filtered board paths are distinct keys, in the snapshot's own spelling", () => {
  expect(boardQuery().queryKey).toEqual(["api", "/api/v1/board?filter=all"]);
  expect(boardQuery({ filter: "needs", project: "alpha", host: "h1" }).queryKey).toEqual([
    "api",
    "/api/v1/board?project=alpha&filter=needs&host=h1",
  ]);
  expect(agentsQuery().queryKey).toEqual(["api", "/api/v1/agents"]);
  expect(agentsQuery("h1").queryKey).toEqual(["api", "/api/v1/agents?host=h1"]);
});

test("hydrating returns the stream position and sets the server clock offset", () => {
  const queryClient = new QueryClient();
  const snapshot = fixtureSnapshot("default", Date.now() + 60_000);
  expect(hydrateFromSnapshot(queryClient, snapshot)).toBe(4181);
  expect(clockStore.get().offsetMs).toBeGreaterThan(55_000);
  expect(clockStore.get().offsetMs).toBeLessThan(65_000);
});

function refusedSignedOut(path: string) {
  return Promise.reject(new ReadError(path, 403, "not signed in", null, "signed_out"));
}

function unreachable(path: string) {
  return Promise.reject(new ReadError(path, 502, null));
}

test("a snapshot refused as signed out boots the signed-out screen", async () => {
  expect(await loadBoot(refusedSignedOut)).toEqual({ kind: "signed-out" });
  expect(await loadBoot(unreachable)).toEqual({ kind: "board", snapshot: null });
});

// A machine before `tiller init`: every read answers 503 with the command,
// and health answers `health`.
function unset(health: unknown) {
  return (path: string) =>
    path === "/api/v1/health"
      ? Promise.resolve(health)
      : Promise.reject(new ReadError(path, 503, "no state.db", "tiller init"));
}

test("a 503 opens the setup screen when health says init has not run", async () => {
  const full = { ok: false, version: "0.1.0", initialized: false, pollers: [] };
  expect(await loadBoot(unset(full) as never)).toEqual({
    kind: "setup",
    command: "tiller init",
  });
  // Signed out at a local Host, health is small but still says it.
  const small = unset({ ok: false, version: "0.1.0", initialized: false }) as never;
  expect(await loadBoot(small)).toEqual({ kind: "setup", command: "tiller init" });
  // A health answer that does not say it never claims an unset machine.
  const unsaid = unset({ ok: false, version: "0.1.0" }) as never;
  expect(await loadBoot(unsaid)).toEqual({ kind: "board", snapshot: null });
});
