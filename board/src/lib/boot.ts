import type { QueryClient } from "@tanstack/react-query";

import type { HealthAnswer, Snapshot } from "./api-types";
import { syncClock } from "./clock";
import { API_KEY_PREFIX, ReadError, fetchJson } from "./queries";
import { isSignedOut } from "./session";

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

// What the page opens with, from `GET /api/v1/snapshot` (the page itself
// carries no snapshot: a navigation sends the cookie alone, which reads
// nothing, protocol.md section 7.6): the board, with the snapshot when the
// read worked; the setup screen when the daemon runs on a machine nobody has
// set up yet (no state.db: every read answers 503, with the command to run,
// until `tiller init`, signed in or not at a local Host); or the signed-out
// screen when this browser has no owner session (every read answers 403
// `signed_out`).
export type Boot =
  | { kind: "board"; snapshot: Snapshot | null }
  | { kind: "setup"; command: string }
  | { kind: "signed-out" };

type Fetch = <T>(path: string) => Promise<T>;

export async function loadBoot(fetch: Fetch = (path) => fetchJson(path)): Promise<Boot> {
  try {
    return { kind: "board", snapshot: await fetch<Snapshot>(SNAPSHOT_PATH) };
  } catch (error) {
    if (isSignedOut(error)) return { kind: "signed-out" };
    if (error instanceof ReadError && error.status === 503 && error.hint) {
      // Signed in or not, health says whether `tiller init` has run.
      const health = await fetch<HealthAnswer>(HEALTH_PATH).catch(() => null);
      if (health?.initialized === false) return { kind: "setup", command: error.hint };
    }
    return { kind: "board", snapshot: null };
  }
}
