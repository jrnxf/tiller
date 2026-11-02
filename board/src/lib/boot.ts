import type { QueryClient } from "@tanstack/react-query";

import type { Snapshot } from "./api-types";
import { syncClock } from "./clock";
import { API_KEY_PREFIX } from "./queries";

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
