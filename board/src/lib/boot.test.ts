import { QueryClient } from "@tanstack/react-query";
import { expect, test } from "vitest";

import { BOOT_ELEMENT_ID, hydrateFromSnapshot, loadBoot, readBootSnapshot } from "./boot";
import { clockStore } from "./clock";
import { bootScript, fixtureSnapshot } from "./fixtures";
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

test("the inlined script survives text that would end a script element", () => {
  const snapshot = fixtureSnapshot("default", NOW);
  snapshot.queries["/api/v1/note"] = { text: "</script><script>alert(1)</script> &    " };

  document.body.innerHTML = bootScript(snapshot);
  const element = document.getElementById(BOOT_ELEMENT_ID);
  expect(element?.textContent).not.toContain("</script>");
  expect(element?.textContent).not.toContain("<");

  const read = readBootSnapshot(document);
  expect(read?.queries["/api/v1/note"]).toEqual(snapshot.queries["/api/v1/note"]);
});

test("a missing or malformed boot script reads as no snapshot", () => {
  document.body.innerHTML = "";
  expect(readBootSnapshot(document)).toBeNull();

  document.body.innerHTML = `<script type="application/json" id="${BOOT_ELEMENT_ID}">{"v":2}</script>`;
  expect(readBootSnapshot(document)).toBeNull();
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
  document.body.innerHTML = "";
  expect(await loadBoot(document, refusedSignedOut)).toEqual({ kind: "signed-out" });
  expect(await loadBoot(document, unreachable)).toEqual({ kind: "board", snapshot: null });
});
