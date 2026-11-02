import { serverNow } from "../../lib/clock";

// Relative ages and Lisbon times for the board views. Every time on the board is
// shown in Europe/Lisbon, with the weekday once it is not today.

export function secondsSince(iso: string | null, now = serverNow()): number | null {
  if (!iso) return null;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return null;
  return Math.max(0, Math.floor((now - then) / 1000));
}

// "12 s", "3 min", "2 h", "4 d".
export function formatAge(seconds: number): string {
  if (seconds < 60) return `${seconds} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return `${hours} h`;
  return `${Math.floor(hours / 24)} d`;
}

const dayOf = new Intl.DateTimeFormat("en-CA", {
  timeZone: "Europe/Lisbon",
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
});

const clock = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hourCycle: "h23",
});

const dated = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  weekday: "short",
  day: "numeric",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

// "17:58:11" for today, "Sun 1 Nov, 17:58" for any other day.
export function formatLisbonMoment(iso: string | null, now = serverNow()): string | null {
  if (!iso) return null;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return null;
  if (dayOf.format(date) === dayOf.format(new Date(now))) return clock.format(date);
  return dated.format(date);
}

// The first seven characters of a commit hash, as the PR chip and the approval
// binding show it.
export function shortSha(sha: string | null): string {
  return sha ? sha.slice(0, 7) : "";
}

// How long something ran: to `to`, or to now while it is still running.
export function durationSeconds(from: string, to: string | null, now = serverNow()): number | null {
  const start = Date.parse(from);
  const end = to ? Date.parse(to) : now;
  if (Number.isNaN(start) || Number.isNaN(end)) return null;
  return Math.max(0, Math.floor((end - start) / 1000));
}
