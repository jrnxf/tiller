import type { QuotaResponse, QuotaWindow, SignalsResponse, SignalsWeek } from "../../lib/api-types";
import { formatLisbon } from "../../lib/clock";
import { lisbonDay, lisbonWeekLabel } from "./lisbon";

// Everything the Health page says is computed here from `GET /api/v1/signals`
// and `GET /api/v1/quota`, so a test can pin each number to its source. The
// status rules are plain thresholds; T7.1 deepens the counts, not the look.

type SignalId = "shipping" | "time" | "quality" | "safety";

export interface Signal {
  id: SignalId;
  title: string;
  state: "good" | "watch";
  big: number;
  unit: string;
  read: string;
  facts: string[];
  // Why the signal is on watch, as the end of "One thing to watch: x, ...".
  reason: string | null;
  // The big number for each of the weeks, oldest first.
  values: number[];
  last: string;
}

export interface Verdict {
  tone: "good" | "bad";
  head: string;
  text: string;
  watch: string;
}

// A median answer slower than this is worth watching, and so is a week that
// needed the developer more than twice as often as the weeks before it.
export const SLOW_ANSWER_MINUTES = 30;
export const BUSY_WEEK_FACTOR = 2;
const BUSY_WEEK_MINIMUM = 5;

export const accepted = (week: SignalsWeek) => week.landed + week.reported;
const problems = (week: SignalsWeek) => week.reverts + week.escaped;
const incidents = (week: SignalsWeek) =>
  week.lost_work + week.duplicate_dispatches + week.unauthorized;

const count = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function minutes(value: number): string {
  return String(value < 10 ? Number(value.toFixed(1)) : Math.round(value));
}

function lastWeek(
  response: SignalsResponse,
): { now: SignalsWeek; before: SignalsWeek | null } | null {
  const now = response.weeks.at(-1);
  if (!now) return null;
  return { now, before: response.weeks.at(-2) ?? null };
}

function inProgress(week: SignalsWeek, now: number): boolean {
  return now < Date.parse(week.end);
}

function shipping(response: SignalsResponse, now: number): Signal | null {
  const weeks = lastWeek(response);
  if (!weeks) return null;
  const cur = accepted(weeks.now);
  const prev = weeks.before ? accepted(weeks.before) : null;
  const quiet = cur === 0 && prev === 0;
  let read: string;
  if (quiet) read = "Nothing has landed for two weeks.";
  else if (prev === null) read = `${cur} so far this week.`;
  else if (inProgress(weeks.now, now)) {
    read = `${cur} so far this week, against ${prev} in all of last week.`;
  } else if (cur > prev) read = `Up from ${prev} last week. Cards are landing steadily.`;
  else if (cur === prev) read = `The same as last week, ${prev}.`;
  else read = `Down from ${prev} last week.`;
  const facts = [];
  if (weeks.now.merged_prs > 0) facts.push(count(weeks.now.merged_prs, "merged PR"));
  if (weeks.now.reported > 0) facts.push(count(weeks.now.reported, "accepted scout report"));
  if (facts.length === 0) facts.push("nothing landed yet");
  return {
    id: "shipping",
    title: "Shipping",
    state: quiet ? "watch" : "good",
    big: cur,
    unit: cur === 1 ? "card landed this week" : "cards landed this week",
    read,
    facts,
    reason: quiet ? "nothing has landed for two weeks" : null,
    values: response.weeks.map(accepted),
    last: prev === null ? "no earlier week" : `last week: ${prev}`,
  };
}

function yourTime(response: SignalsResponse): Signal | null {
  const weeks = lastWeek(response);
  if (!weeks) return null;
  const { now } = weeks;
  const median = now.median_answer_minutes;
  let earlierMedian: number | null = null;
  for (const week of response.weeks.slice(0, -1)) {
    if (week.median_answer_minutes !== null) earlierMedian = week.median_answer_minutes;
  }
  let read: string;
  if (median === null) {
    read = now.needed === 0 ? "You have not been needed yet." : "Nothing has been answered yet.";
  } else if (earlierMedian === null || Math.abs(median - earlierMedian) < 1) {
    read = `You answered in about ${minutes(median)} minutes.`;
  } else if (median < earlierMedian) {
    read = `You answered faster: about ${minutes(median)} minutes, down from ${minutes(earlierMedian)}.`;
  } else {
    read = `You answered more slowly: about ${minutes(median)} minutes, up from ${minutes(earlierMedian)}.`;
  }
  const earlier = response.weeks.slice(0, -1);
  const average = earlier.length
    ? earlier.reduce((total, week) => total + week.needed, 0) / earlier.length
    : null;
  let reason: string | null = null;
  if (median !== null && median > SLOW_ANSWER_MINUTES) {
    reason = `answers are taking about ${minutes(median)} minutes`;
  } else if (
    average !== null &&
    now.needed >= BUSY_WEEK_MINIMUM &&
    now.needed > BUSY_WEEK_FACTOR * average
  ) {
    reason = `you were needed ${now.needed} times, well above the usual ${Math.round(average)}`;
  }
  return {
    id: "time",
    title: "Your time",
    state: reason ? "watch" : "good",
    big: now.needed,
    unit: now.needed === 1 ? "time you were needed" : "times you were needed",
    read,
    facts: median === null ? [] : [`median ${minutes(median)} min to answer`],
    reason,
    values: response.weeks.map((week) => week.needed),
    last: weeks.before ? `last week: ${weeks.before.needed}` : "no earlier week",
  };
}

function quality(response: SignalsResponse): Signal | null {
  const weeks = lastWeek(response);
  if (!weeks) return null;
  const { now } = weeks;
  const total = problems(now);
  const done = accepted(now);
  const read = [
    now.reverts > 0 ? `${count(now.reverts, "revert")} slipped in.` : "No reverts.",
    now.escaped > 0 ? `${count(now.escaped, "escaped bug")}.` : "No escaped bugs.",
    total > 0 ? "Worth a look." : "",
  ]
    .filter(Boolean)
    .join(" ");
  const listed = [
    now.reverts > 0 ? count(now.reverts, "revert") : null,
    now.escaped > 0 ? count(now.escaped, "escaped bug") : null,
  ].filter(Boolean);
  return {
    id: "quality",
    title: "Quality",
    state: total > 0 ? "watch" : "good",
    big: total,
    unit: total === 1 ? "problem this week" : "problems this week",
    read,
    facts: [
      done > 0 ? `rework ${(now.rework / done).toFixed(2)} per card` : "no rework rate yet",
      count(now.reverts, "revert"),
      count(now.escaped, "escaped bug"),
    ],
    reason: total > 0 ? `${listed.join(" and ")} this week` : null,
    values: response.weeks.map(problems),
    last: weeks.before ? `last week: ${problems(weeks.before)}` : "no earlier week",
  };
}

function machinesSentence(live: number, total: number): string {
  if (live === total) {
    if (total === 1) return "The machine is live.";
    return total === 2 ? "Both machines are live." : `All ${total} machines are live.`;
  }
  return `Only ${live} of ${total} machines are live.`;
}

function safety(response: SignalsResponse): Signal | null {
  const weeks = lastWeek(response);
  if (!weeks) return null;
  const { now } = weeks;
  const total = incidents(now);
  const { machines_live: live, machines_total: machines } = response;
  const named = [
    now.lost_work > 0 ? `${count(now.lost_work, "piece")} of work lost` : null,
    now.duplicate_dispatches > 0
      ? count(now.duplicate_dispatches, "duplicate dispatch", "duplicate dispatches")
      : null,
    now.unauthorized > 0 ? count(now.unauthorized, "unauthorized action") : null,
  ].filter((part): part is string => part !== null);
  const offline = live < machines;
  return {
    id: "safety",
    title: "Safety",
    state: total > 0 || offline ? "watch" : "good",
    big: total,
    unit: total === 1 ? "incident this week" : "incidents this week",
    read:
      total === 0
        ? `Nothing lost, nothing sent twice, nothing unauthorized. ${machinesSentence(live, machines)}`
        : `${named.join(", ")}. ${machinesSentence(live, machines)}`,
    facts: [
      `lost work ${now.lost_work}`,
      `duplicate dispatches ${now.duplicate_dispatches}`,
      `unauthorized actions ${now.unauthorized}`,
      `machines live ${live} of ${machines}`,
    ],
    reason:
      total > 0
        ? `${named.join(" and ")} this week`
        : offline
          ? `only ${live} of ${machines} machines are live`
          : null,
    values: response.weeks.map(incidents),
    last: weeks.before ? `last week: ${incidents(weeks.before)}` : "no earlier week",
  };
}

export function signalsOf(response: SignalsResponse, now: number): Signal[] {
  return [shipping(response, now), yourTime(response), quality(response), safety(response)].filter(
    (signal): signal is Signal => signal !== null,
  );
}

// Safety first, then the rest in the order of the page.
const WATCH_ORDER: SignalId[] = ["safety", "quality", "shipping", "time"];

export function verdictOf(
  signals: Signal[],
  response: SignalsResponse,
  now: number,
): Verdict | null {
  const latest = response.weeks.at(-1);
  if (!latest) return null;
  const range = weekRange(latest, now);
  const unsafe = incidents(latest) > 0;
  const watched = WATCH_ORDER.map((id) => signals.find((signal) => signal.id === id)).find(
    (signal) => signal?.state === "watch",
  );
  const watch = watched
    ? `One thing to watch: ${watched.title.toLowerCase()}, ${watched.reason}. ${range}`
    : `Nothing to watch. ${range}`;
  if (unsafe) {
    return {
      tone: "bad",
      head: "Not working.",
      text: " Safety needs your attention first.",
      watch,
    };
  }
  const landed = accepted(latest);
  const text = ` ${landed === 0 ? "Nothing has landed yet" : `${count(landed, "card")} landed`}, you were needed ${count(latest.needed, "time")}, and nothing was lost.`;
  return { tone: "good", head: "Working.", text, watch };
}

// "Week 44, Monday 2026-10-26 to Sunday 2026-11-01." in Lisbon time.
export function weekRange(week: SignalsWeek, now: number): string {
  const first = lisbonDay(week.start);
  const last = lisbonDay(new Date(Date.parse(week.end) - 1).toISOString());
  const label = lisbonWeekLabel(week.start);
  return `Week ${label.slice(1)}, ${first.weekday} ${first.date} to ${last.weekday} ${last.date}${inProgress(week, now) ? ", still in progress" : ""}.`;
}

interface QuotaOther {
  label: string;
  percentLeft: number | null;
}

export interface QuotaBlock {
  tone: "ok" | "warn" | "bad" | "unk";
  word: string;
  percentLeft: number | null;
  percentUsed: number | null;
  elapsedPercent: number | null;
  resetsShort: string | null;
  resetsFull: string | null;
  verdict: string;
  reserve: string | null;
  burn: string | null;
  others: QuotaOther[];
}

const clamp = (value: number) => Math.min(100, Math.max(0, value));
const trim = (value: number) => String(Number(value.toFixed(2)));

export function quotaBlockOf(quota: QuotaResponse): QuotaBlock | null {
  const week = quota.windows.find((window) => window.kind === "weekly");
  if (!week) return null;
  const left = week.percent_remaining === null ? null : clamp(week.percent_remaining);
  const tone = { green: "ok", amber: "warn", red: "bad", grey: "unk" } as const;
  const word = {
    green: "Under pace",
    amber: "Ahead of pace",
    red: "Almost out",
    grey: "Out of date",
  } as const;
  const verdict = {
    green: "On pace to last until reset.",
    amber: "Spending faster than the clock; it may run out before reset.",
    red: "Almost out. It will not last until reset.",
    grey: "This reading is out of date.",
  } as const;
  const reserve = week.reserve;
  return {
    tone: tone[week.dot],
    word: word[week.dot],
    percentLeft: left === null ? null : Math.round(left),
    percentUsed: left === null ? null : 100 - left,
    elapsedPercent: week.elapsed_percent === null ? null : clamp(week.elapsed_percent),
    resetsShort: formatLisbon(week.resets_at),
    resetsFull: week.resets_at ? lisbonLong(week.resets_at) : null,
    verdict: verdict[week.dot],
    reserve:
      reserve === null
        ? null
        : reserve >= 0
          ? `reserve ${trim(reserve)} points`
          : `${trim(-reserve)} points short`,
    burn: week.burn_multiple === null ? null : `burn ${trim(week.burn_multiple)}x the even pace`,
    others: quota.windows.filter((window) => window !== week).map(otherOf),
  };
}

function otherOf(window: QuotaWindow): QuotaOther {
  return {
    label: window.title,
    percentLeft:
      window.percent_remaining === null ? null : Math.round(clamp(window.percent_remaining)),
  };
}

// "Tuesday 2026-11-03, 14:00 Lisbon time".
function lisbonLong(iso: string): string {
  const day = lisbonDay(iso);
  return `${day.weekday} ${day.date}, ${day.time} Lisbon time`;
}

export interface ProjectRow {
  project_id: string;
  accepted: number;
  perOutcome: number | null;
  escaped: number;
  median: number | null;
}

export function projectRows(response: SignalsResponse, shown: Set<string>): ProjectRow[] {
  return response.by_project
    .filter((row) => shown.has(row.project_id))
    .map((row) => ({
      project_id: row.project_id,
      accepted: row.accepted,
      perOutcome: row.accepted > 0 ? row.needed / row.accepted : null,
      escaped: row.escaped,
      median: row.median_answer_minutes,
    }));
}
