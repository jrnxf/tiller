// Fixture counts for the Health page, shaped like `GET /api/v1/signals`. The
// eight weeks follow the approved mock (docs/design/mocks.html); the weeks
// themselves are the Lisbon weeks ending with the one that holds `now`.
import type { SignalsResponse, SignalsWeek } from "./api-types.ts";

const WEEK_MS = 7 * 24 * 3_600_000;

const lisbon = new Intl.DateTimeFormat("en-GB", {
  timeZone: "Europe/Lisbon",
  weekday: "short",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

// Monday 00:00 Lisbon time of the week holding `now`, as an instant. Lisbon is
// a whole number of hours from UTC, so stepping back by the hour finds it.
export function lisbonWeekStart(now: number): number {
  let guess = Math.floor(now / 3_600_000) * 3_600_000;
  for (let step = 0; step <= 7 * 24; step += 1) {
    const parts = Object.fromEntries(
      lisbon.formatToParts(new Date(guess)).map((part) => [part.type, part.value]),
    );
    if (parts.weekday === "Mon" && parts.hour === "00") return guess;
    guess -= 3_600_000;
  }
  return guess;
}

const ACCEPTED = [5, 7, 6, 7, 8, 9, 10, 12];
const REPORTED = [1, 2, 1, 2, 2, 2, 3, 3];
const NEEDED = [10, 13, 14, 13, 10, 9, 8, 8];
const REWORK = [3, 4, 2, 4, 3, 3, 2, 2];
const REVERTS = [0, 1, 0, 0, 0, 0, 0, 1];
const ESCAPED = [1, 2, 1, 1, 1, 0, 1, 0];
const ANSWER = [16, 14, 15, 13, 11, 9, 7, 5.2];

// This week's share of each project, as in the mock's table.
const BY_PROJECT = [
  { project_id: "alpha", accepted: 4, needed: 3, escaped: 0, median_answer_minutes: 6.1 },
  { project_id: "beta", accepted: 5, needed: 3, escaped: 0, median_answer_minutes: 4.4 },
  { project_id: "gamma", accepted: 3, needed: 2, escaped: 0, median_answer_minutes: 3.2 },
];

export function signalsFor(unsafe: boolean, now: number): SignalsResponse {
  const first = lisbonWeekStart(now) - 7 * WEEK_MS;
  const weeks: SignalsWeek[] = ACCEPTED.map((accepted, index) => {
    const start = first + index * WEEK_MS;
    const reported = REPORTED[index] ?? 0;
    const last = index === ACCEPTED.length - 1;
    return {
      // A week never gains or loses an hour here: a changeover week is a
      // fixture detail the page does not depend on.
      start: new Date(start).toISOString(),
      end: new Date(start + WEEK_MS).toISOString(),
      landed: accepted - reported,
      reported,
      merged_prs: accepted - reported,
      needed: NEEDED[index] ?? 0,
      median_answer_minutes: ANSWER[index] ?? null,
      reverts: REVERTS[index] ?? 0,
      escaped: ESCAPED[index] ?? 0,
      rework: REWORK[index] ?? 0,
      lost_work: unsafe && last ? 1 : 0,
      duplicate_dispatches: 0,
      unauthorized: 0,
    };
  });
  return { weeks, by_project: BY_PROJECT, machines_live: unsafe ? 1 : 2, machines_total: 2 };
}
