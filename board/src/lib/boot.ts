import type { QueryClient } from "@tanstack/react-query";

import type { DaemonHealth, Snapshot } from "./api-types";
import { syncClock } from "./clock";
import { API_KEY_PREFIX, ReadError, fetchJson } from "./queries";
import { isSignedOut } from "./session";

export const BOOT_ELEMENT_ID = "tiller-boot";

// The daemon inlines the dehydrated query state in index.html (protocol.md
// section 7.6). Reading it is synchronous, so the first render is complete.
export function readBootSnapshot(doc: Document = document): Snapshot | null {
  const element = doc.getElementById(BOOT_ELEMENT_ID);
  if (!element?.textContent) return null;
  const parsed: unknown = JSON.parse(element.textContent);
  return isSnapshot(parsed) ? parsed : null;
}

function isSnapshot(value: unknown): value is Snapshot {
  if (typeof value !== "object" || value === null) return false;
  const snapshot = value as Partial<Snapshot>;
  return (
    snapshot.v === 1 &&
    typeof snapshot.server_time === "string" &&
    typeof snapshot.last_event_seq === "number" &&
    typeof snapshot.queries === "object" &&
    snapshot.queries !== null
  );
}

// Each `queries` entry is exactly the response of that GET, keyed by its path
// and query string, which is also how the query option factories key it.
export function hydrateFromSnapshot(queryClient: QueryClient, snapshot: Snapshot): number {
  for (const [path, data] of Object.entries(snapshot.queries)) {
    queryClient.setQueryData([...API_KEY_PREFIX, path], data);
  }
  syncClock(snapshot.server_time, snapshot.thresholds);
  return snapshot.last_event_seq;
}

// The page lost its place in the event log (a `reset`, or a stream that began
// with no boot snapshot). The snapshot covers six reads; every other read in
// the cache (an open card, its timeline, memory, Settings, Health) is only
// marked stale, so the ones on screen fetch again and the rest fetch when they
// next mount. The UI stays put: nothing is cleared first. With `all`, the six
// are marked stale too: a change newer than the snapshot was applied while it
// was in flight, and the snapshot wrote the older rows back. Returns the
// snapshot's seq.
export function refreshFromSnapshot(
  queryClient: QueryClient,
  snapshot: Snapshot,
  all = false,
): number {
  const seq = hydrateFromSnapshot(queryClient, snapshot);
  const hydrated = new Set(Object.keys(snapshot.queries));
  void queryClient.invalidateQueries({
    queryKey: API_KEY_PREFIX,
    predicate: (query) => all || !hydrated.has(String(query.queryKey[1] ?? "")),
    refetchType: "active",
  });
  return seq;
}

export const SNAPSHOT_PATH = "/api/v1/snapshot";
const HEALTH_PATH = "/api/v1/health";

// What the page opens with: the board, with the snapshot when there is one;
// the setup screen when the daemon runs on a machine nobody has set up yet
// (no state.db: every read answers 503, with the command to run, until
// `tiller init`); or the signed-out screen when, at an `--allow-host` name,
// this browser has no owner cookie (every read answers 403 `signed_out`).
export type Boot =
  | { kind: "board"; snapshot: Snapshot | null }
  | { kind: "setup"; command: string }
  | { kind: "signed-out" };

type Fetch = <T>(path: string) => Promise<T>;

export async function loadBoot(
  doc: Document = document,
  fetch: Fetch = (path) => fetchJson(path),
): Promise<Boot> {
  const inlined = readBootSnapshot(doc);
  if (inlined) return { kind: "board", snapshot: inlined };
  // A dev server that is not the daemon has no inlined snapshot either.
  try {
    return { kind: "board", snapshot: await fetch<Snapshot>(SNAPSHOT_PATH) };
  } catch (error) {
    if (isSignedOut(error)) return { kind: "signed-out" };
    if (error instanceof ReadError && error.status === 503 && error.hint) {
      const health = await fetch<DaemonHealth>(HEALTH_PATH).catch(() => null);
      if (health?.initialized === false) return { kind: "setup", command: error.hint };
    }
    return { kind: "board", snapshot: null };
  }
}
