// Fixture counts for the Health page, shaped like `GET /api/v1/signals`. The
// eight weeks follow the approved mock (docs/design/mocks.html); the weeks
// themselves are the Lisbon weeks ending with the one that holds `now`.
import type {
  InterventionCounts,
  InterventionKind,
  SignalsCliDecision,
  SignalsIntervention,
  SignalsResponse,
  SignalsWeek,
} from "./api-types.ts";

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
const ANSWERED = [9, 12, 13, 12, 9, 8, 7, 7];
const REVERTS = [0, 1, 0, 0, 0, 0, 0, 1];
const ESCAPED = [1, 2, 1, 1, 1, 0, 1, 0];
const ANSWER = [16, 14, 15, 13, 11, 9, 7, 5.2];
// Why the developer was needed in each week. The newest week is the tally of
// `INTERVENTIONS` below, so the list and the counts always agree.
const CORRECTIONS = [2, 3, 3, 2, 2, 1, 1];
const ESCALATIONS = [2, 3, 3, 3, 2, 2, 1];
const REWORK = [3, 4, 2, 4, 3, 3, 2];
const PLANNED = [3, 3, 6, 4, 3, 3, 4];

// The newest week's interventions, newest first, with what the rules said.
const INTERVENTIONS: Omit<SignalsIntervention, "at" | "kind" | "relabeled">[] = [
  {
    id: "01JATT00000000000000000A01",
    source: "attention",
    card_id: "01JCARD0000000000000000A31",
    display_id: "alpha-31",
    project_id: "alpha",
    summary: "Approve merging the settings page",
    classified: "planned",
  },
  {
    id: "01JNOTE0000000000000000B18",
    source: "note",
    card_id: "01JCARD0000000000000000B18",
    display_id: "beta-18",
    project_id: "beta",
    summary: "Use the existing date helper, not a new one",
    classified: "correction",
  },
  {
    id: "01JATT00000000000000000C09",
    source: "attention",
    card_id: "01JCARD0000000000000000C09",
    display_id: "gamma-9",
    project_id: "gamma",
    summary: "Read the weekly scout report",
    classified: "planned",
  },
  {
    id: "01JATT00000000000000000A30",
    source: "attention",
    card_id: "01JCARD0000000000000000A30",
    display_id: "alpha-30",
    project_id: "alpha",
    summary: "Second attempt at the export fix needs a look",
    classified: "rework",
  },
  {
    id: "01JATT00000000000000000B17",
    source: "attention",
    card_id: "01JCARD0000000000000000B17",
    display_id: "beta-17",
    project_id: "beta",
    summary: "Which retry policy should the sync use?",
    classified: "unplanned_escalation",
  },
  {
    id: "01JATT00000000000000000C08",
    source: "attention",
    card_id: "01JCARD0000000000000000C08",
    display_id: "gamma-8",
    project_id: "gamma",
    summary: "Approve merging the report template",
    classified: "planned",
  },
  {
    id: "01JATT00000000000000000A29",
    source: "attention",
    card_id: "01JCARD0000000000000000A29",
    display_id: "alpha-29",
    project_id: "alpha",
    summary: "Approve merging the import fix",
    classified: "planned",
  },
  {
    id: "01JATT00000000000000000C07",
    source: "attention",
    card_id: "01JCARD0000000000000000C07",
    display_id: "gamma-7",
    project_id: "gamma",
    summary: "The first attempt was reverted; check the retry",
    classified: "rework",
  },
];

// The decisions the owner made at a terminal through the CLI in each week; the
// newest week's are `CLI_DECISIONS` below.
const CLI_DECISION_COUNTS = [2, 4, 3, 1, 2, 3, 2];

// The newest week's CLI decisions, newest first.
const CLI_DECISIONS: Omit<SignalsCliDecision, "at">[] = [
  {
    txn: "approval.decide",
    display_id: "alpha-31",
    tty: "/dev/pts/3",
    parent: "zsh",
    parent_pid: 48213,
  },
  {
    txn: "card.close",
    display_id: "beta-16",
    tty: "/dev/pts/3",
    parent: "zsh",
    parent_pid: 48213,
  },
  {
    txn: "attention.answer",
    display_id: "gamma-9",
    tty: "/dev/pts/1",
    parent: "bash",
    parent_pid: 47120,
  },
];

// What the developer has relabeled since the server started: the dev and
// preview servers keep it so a relabel shows up on the next read.
const relabels = new Map<string, InterventionKind | null>();

export function relabelFixture(
  id: string,
  label: InterventionKind | null,
): { id: string; kind: InterventionKind; classified: InterventionKind; relabeled: boolean } | null {
  const found = INTERVENTIONS.find((item) => item.id === id);
  if (!found) return null;
  relabels.set(id, label);
  const kind = label ?? found.classified;
  return { id, kind, classified: found.classified, relabeled: kind !== found.classified };
}

const tally = (items: { kind: InterventionKind }[]): InterventionCounts => {
  const counts = { correction: 0, unplanned_escalation: 0, rework: 0, planned: 0 };
  for (const item of items) counts[item.kind] += 1;
  return counts;
};

// This week's share of each project, as in the mock's table. `avoidable` is
// filled from the interventions.
const BY_PROJECT = [
  { project_id: "alpha", accepted: 4, needed: 3, answered: 3, median_answer_minutes: 6.1 },
  { project_id: "beta", accepted: 5, needed: 2, answered: 2, median_answer_minutes: 4.4 },
  { project_id: "gamma", accepted: 3, needed: 3, answered: 2, median_answer_minutes: 3.2 },
];

// A new install: eight Lisbon weeks, all empty but the current one, which
// holds the first ask and nothing landed.
function freshSignals(now: number): SignalsResponse {
  // Tiller started this week: an hour in, never later than now.
  const started = Math.min(now, lisbonWeekStart(now) + 3_600_000);
  return emptyWeeks(now, 1, new Date(started).toISOString());
}

// An install that has not seen one event: eight empty weeks, no first event.
function noEventSignals(now: number): SignalsResponse {
  return emptyWeeks(now, 0, null);
}

function emptyWeeks(now: number, needed: number, firstEventAt: string | null): SignalsResponse {
  const first = lisbonWeekStart(now) - 7 * WEEK_MS;
  const weeks: SignalsWeek[] = Array.from({ length: 8 }, (_, index) => {
    const start = first + index * WEEK_MS;
    return {
      start: new Date(start).toISOString(),
      end: new Date(start + WEEK_MS).toISOString(),
      landed: 0,
      reported: 0,
      merged_prs: 0,
      needed: index === 7 ? needed : 0,
      answered: 0,
      median_answer_minutes: null,
      interventions: { correction: 0, unplanned_escalation: 0, rework: 0, planned: 0 },
      reverts: 0,
      escaped: 0,
      rework: 0,
      lost_work: 0,
      duplicate_dispatches: 0,
      unauthorized: 0,
      cli_decisions: 0,
    };
  });
  return {
    weeks,
    by_project: [],
    interventions: [],
    cli_decisions: [],
    machines_live: 1,
    machines_total: 1,
    first_event_at: firstEventAt,
  };
}

// What the fixture's store has seen: a long run of weeks, a new install whose
// first ask came this week, or no event at all.
export type SignalsShape = "full" | "new-install" | "no-events";

// The time of item `index` of `count`, newest first: `gap` apart going back
// from now, but never before the week began nor later than now, and no two
// alike, even in the week's first minutes.
function spreadAt(now: number, weekStart: number, index: number, count: number, gap: number) {
  const share = weekStart + ((now - weekStart) * (count - index)) / (count + 1);
  return new Date(Math.max(now - (index + 1) * gap, share)).toISOString();
}

export function signalsFor(
  unsafe: boolean,
  now: number,
  shape: SignalsShape = "full",
): SignalsResponse {
  if (shape === "new-install") return freshSignals(now);
  if (shape === "no-events") return noEventSignals(now);
  const first = lisbonWeekStart(now) - 7 * WEEK_MS;
  const newest = first + 7 * WEEK_MS;
  const interventions: SignalsIntervention[] = INTERVENTIONS.map((item, index) => {
    const label = relabels.get(item.id) ?? null;
    const kind = label ?? item.classified;
    return {
      ...item,
      at: spreadAt(now, newest, index, INTERVENTIONS.length, 2.5 * 3_600_000),
      kind,
      relabeled: kind !== item.classified,
    };
  });
  const cliDecisions: SignalsCliDecision[] = CLI_DECISIONS.map((item, index) => ({
    ...item,
    at: spreadAt(now, newest, index, CLI_DECISIONS.length, 3.5 * 3_600_000),
  }));
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
      answered: ANSWERED[index] ?? 0,
      median_answer_minutes: ANSWER[index] ?? null,
      interventions: last
        ? tally(interventions)
        : {
            correction: CORRECTIONS[index] ?? 0,
            unplanned_escalation: ESCALATIONS[index] ?? 0,
            rework: REWORK[index] ?? 0,
            planned: PLANNED[index] ?? 0,
          },
      reverts: REVERTS[index] ?? 0,
      escaped: ESCAPED[index] ?? 0,
      rework: last ? tally(interventions).rework : (REWORK[index] ?? 0),
      lost_work: unsafe && last ? 1 : 0,
      duplicate_dispatches: 0,
      unauthorized: 0,
      cli_decisions: last ? cliDecisions.length : (CLI_DECISION_COUNTS[index] ?? 0),
    };
  });
  const by_project = BY_PROJECT.map((row) => ({
    ...row,
    avoidable: interventions.filter(
      (item) => item.project_id === row.project_id && item.kind !== "planned",
    ).length,
    escaped: 0,
  }));
  return {
    weeks,
    by_project,
    interventions,
    cli_decisions: cliDecisions,
    machines_live: unsafe ? 1 : 2,
    machines_total: 2,
    // Tiller has run since well before the oldest week shown.
    first_event_at: new Date(first - 4 * WEEK_MS).toISOString(),
  };
}
