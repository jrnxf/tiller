import { createStore } from "./store.ts";

// Staleness is derived from age, so it moves without an event. The snapshot's
// `server_time` gives the offset between this clock and the daemon's, and its
// thresholds say when something counts as stale (protocol.md section 7.5).
interface ClockState {
  offsetMs: number;
  thresholds: Record<string, unknown>;
}

export const clockStore = createStore<ClockState>({ offsetMs: 0, thresholds: {} });

export function syncClock(serverTime: string, thresholds: Record<string, unknown>) {
  const server = Date.parse(serverTime);
  clockStore.set({
    offsetMs: Number.isNaN(server) ? 0 : server - Date.now(),
    thresholds,
  });
}

export function serverNow(): number {
  return Date.now() + clockStore.get().offsetMs;
}

export function minutesSince(iso: string | null, now = serverNow()): number | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;
  return Math.max(0, Math.floor((now - then) / 60_000));
}

const lisbonParts = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  weekday: "short",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

// "Tue 14:00" in Lisbon time, with the weekday, whatever zone the browser is in.
export function formatLisbon(iso: string | null): string | null {
  if (!iso) return null;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return null;
  const parts = Object.fromEntries(lisbonParts.formatToParts(date).map((p) => [p.type, p.value]));
  return `${parts.weekday} ${parts.hour}:${parts.minute}`;
}
