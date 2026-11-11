import { readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";

import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { QuotaResponse, SignalsResponse, SignalsWeek } from "../../lib/api-types";
import { hydrateFromSnapshot } from "../../lib/boot";
import { connectionStore } from "../../lib/connection";
import { fixtureResponses, fixtureSnapshot } from "../../lib/fixtures";
import type { Scenario } from "../../lib/fixtures";
import { lisbonWeekStart, signalsFor } from "../../lib/fixtures-signals";
import { loadPrefs, prefsStore, updatePrefs } from "../../lib/prefs";
import { createAppRouter } from "../../lib/router";
import { niceAxis } from "./charts";
import { mutationPhrase } from "./mutation-phrases";
import { lisbonDay, lisbonWeekLabel } from "./lisbon";
import { BUSY_WEEK_FACTOR, SLOW_ANSWER_MINUTES, quotaBlockOf, signalsOf } from "./model";
import { choose } from "../../test-select";
import { createTestQueryClient, loadRouteChunks } from "../../test-app";

await loadRouteChunks();

// Tue 2026-11-03 14:00 Lisbon is the fixture quota reset; this Sunday evening
// is inside the last fixture week, which therefore is in progress.
const NOW = Date.parse("2026-11-01T18:00:00.000Z");

function week(over: Partial<SignalsWeek> = {}): SignalsWeek {
  return {
    start: "2026-10-26T00:00:00.000Z",
    end: "2026-11-02T00:00:00.000Z",
    landed: 3,
    reported: 1,
    merged_prs: 3,
    needed: 4,
    answered: 3,
    median_answer_minutes: 6,
    interventions: { correction: 1, unplanned_escalation: 0, rework: 1, planned: 2 },
    reverts: 0,
    escaped: 0,
    rework: 1,
    lost_work: 0,
    duplicate_dispatches: 0,
    unauthorized: 0,
    cli_decisions: 0,
    ...over,
  };
}

// Two weeks, the newest last, with the fields of `over` on the newest.
function counts(
  over: Partial<SignalsWeek> = {},
  before: Partial<SignalsWeek> = {},
): SignalsResponse {
  return {
    weeks: [
      week({ start: "2026-10-19T00:00:00.000Z", end: "2026-10-26T00:00:00.000Z", ...before }),
      week(over),
    ],
    by_project: [],
    interventions: [],
    cli_decisions: [],
    machines_live: 2,
    machines_total: 2,
    first_event_at: "2026-01-05T09:00:00.000Z",
  };
}

const done = Date.parse("2026-11-05T00:00:00.000Z");

function signal(response: SignalsResponse, id: string, now = done) {
  const found = signalsOf(response, now).find((candidate) => candidate.id === id);
  if (!found) throw new Error(`no ${id} signal`);
  return found;
}

test("shipping counts landed cards from state", () => {
  const response = counts({ landed: 9, reported: 3, merged_prs: 9 }, { landed: 8, reported: 2 });
  const shipping = signal(response, "shipping");
  expect(shipping.big).toBe(12);
  expect(shipping.values).toEqual([10, 12]);
  expect(shipping.facts).toEqual(["9 merged PRs", "3 accepted scout reports"]);
  expect(shipping.read).toBe("Up from 10 last week. Cards are landing steadily.");
  expect(shipping.state).toBe("good");
});

test("a week that is still running is not compared with a whole one", () => {
  const response = counts({ landed: 1, reported: 0 }, { landed: 9, reported: 1 });
  expect(signal(response, "shipping", NOW).read).toBe(
    "1 so far this week, against 10 in all of last week.",
  );
});

test("shipping is on watch only after two weeks with nothing landed", () => {
  const empty = { landed: 0, reported: 0, merged_prs: 0 };
  expect(signal(counts(empty, { landed: 2 }), "shipping").state).toBe("good");
  const quiet = signal(counts(empty, empty), "shipping");
  expect(quiet.state).toBe("watch");
});

// Every count of a week at zero.
const NOTHING: Partial<SignalsWeek> = {
  landed: 0,
  reported: 0,
  merged_prs: 0,
  needed: 0,
  answered: 0,
  median_answer_minutes: null,
  interventions: { correction: 0, unplanned_escalation: 0, rework: 0, planned: 0 },
  rework: 0,
};

test("a new install says nothing has landed yet, not that two weeks went by", () => {
  // Tiller started this week; the current week has only the first ask.
  const response = counts({ ...NOTHING, needed: 1 }, NOTHING);
  response.first_event_at = "2026-10-28T09:00:00.000Z";
  const fresh = signal(response, "shipping");
  expect(fresh.read).toBe("Nothing has landed yet.");
  // Nothing to judge yet is neither good nor on watch.
  expect(fresh.state).toBe("none");
  // A store with no event at all is new too.
  response.first_event_at = null;
  expect(signal(response, "shipping").read).toBe("Nothing has landed yet.");
  expect(signal(response, "shipping").state).toBe("none");
});

test("a store with no event at all has nothing to judge, so no signal reads Good", () => {
  const response = counts(NOTHING, NOTHING);
  response.first_event_at = null;
  const states = signalsOf(response, done).map((each) => [each.id, each.state]);
  expect(states).toEqual([
    ["shipping", "none"],
    ["time", "none"],
    ["quality", "none"],
    ["safety", "none"],
  ]);
  // Their sentences do not claim a clean record either.
  const reads = signalsOf(response, done).map((each) => each.read);
  expect(reads.join(" ")).not.toMatch(/No reverts|Nothing lost|Both machines/);
});

test("the same empty weeks on an install that ran long before them are judged", () => {
  const states = signalsOf(counts(NOTHING, NOTHING), done).map((each) => [each.id, each.state]);
  expect(states).toEqual([
    ["shipping", "watch"],
    ["time", "good"],
    ["quality", "good"],
    ["safety", "good"],
  ]);
});

test("an install that started during last week, busy but with nothing landed, is new", () => {
  const busy = { ...NOTHING, needed: 3, answered: 2, median_answer_minutes: 4 };
  const response = counts(busy, {
    ...busy,
    interventions: { correction: 1, unplanned_escalation: 1, rework: 0, planned: 2 },
  });
  response.first_event_at = "2026-10-22T15:00:00.000Z";
  const fresh = signal(response, "shipping");
  expect(fresh.read).toBe("Nothing has landed yet.");
  expect(fresh.state).toBe("none");
});

test("an old install with two empty weeks says nothing has landed for two weeks", () => {
  // Every week shown is empty, but Tiller ran long before them: a real quiet
  // spell, not a new install.
  const response = counts(NOTHING, NOTHING);
  const quiet = signal(response, "shipping");
  expect(quiet.read).toBe("Nothing has landed for two weeks.");
  expect(quiet.state).toBe("watch");
  // The same when Tiller started the moment last week began.
  response.first_event_at = "2026-10-19T00:00:00.000Z";
  expect(signal(response, "shipping").read).toBe("Nothing has landed for two weeks.");
});

test("your time reads the median answer time against the last one", () => {
  const faster = signal(
    counts({ needed: 8, median_answer_minutes: 5.2 }, { median_answer_minutes: 7 }),
    "time",
  );
  expect(faster.read).toBe("You answered faster: about 5.2 minutes, down from 7.");
  expect(faster.facts).toEqual(["median 5.2 min to answer", "0.5 avoidable per outcome (2 of 4)"]);
  expect(faster.big).toBe(8);
  const nobody = { correction: 0, unplanned_escalation: 0, rework: 0, planned: 0 };
  const none = signal(
    counts({ needed: 0, median_answer_minutes: null, interventions: nobody }),
    "time",
  );
  expect(none.read).toBe("You have not been needed yet.");
  expect(none.facts).toEqual([]);
});

test("your time is on watch for slow answers or a week far busier than usual", () => {
  const slow = signal(counts({ median_answer_minutes: SLOW_ANSWER_MINUTES + 1 }), "time");
  expect(slow.state).toBe("watch");
  const busy = signal(counts({ needed: 4 * BUSY_WEEK_FACTOR + 1 }, { needed: 4 }), "time");
  expect(busy.state).toBe("watch");
  // Few asks are never a busy week, however far above a quiet average.
  expect(signal(counts({ needed: 4 }, { needed: 1 }), "time").state).toBe("good");
});

test("quality counts reverts and escaped bugs as problems and shows rework per landed card", () => {
  const fine = signal(counts({ landed: 8, reported: 2, rework: 2 }), "quality");
  expect(fine.state).toBe("good");
  expect(fine.read).toBe("No reverts. No escaped bugs.");
  expect(fine.facts).toEqual([
    "rework 0.20 per card",
    "0 reverts",
    "0 escaped bugs",
    "0.2 avoidable per outcome (2 of 10)",
  ]);

  const slipped = signal(counts({ reverts: 1, escaped: 0 }), "quality");
  expect(slipped.state).toBe("watch");
  expect(slipped.big).toBe(1);
  expect(slipped.unit).toBe("problem this week");
  expect(slipped.read).toBe("1 revert slipped in. No escaped bugs. Worth a look.");
});

test("safety sums lost work, duplicate dispatches and unauthorized actions", () => {
  const clear = signal(counts(), "safety");
  expect(clear.state).toBe("good");
  expect(clear.read).toBe("Nothing lost, sent twice or unauthorized. Both machines are live.");
  expect(clear.facts).toContain("machines live 2 of 2");
  expect(clear.facts).toContain("CLI decisions 0");

  const hit = signal(counts({ lost_work: 1, duplicate_dispatches: 2, unauthorized: 0 }), "safety");
  expect(hit.big).toBe(3);
  expect(hit.state).toBe("watch");
});

const decision = (at: string, txn: string, display_id: string | null) => ({
  at,
  txn,
  display_id,
  tty: "/dev/pts/3",
  parent: "zsh",
  parent_pid: 48213,
});

// authority.md G23, Detected: every owner decision made through the CLI
// records its terminal and parent process, and Safety names the newest one
// this week, so the owner can spot one they did not make.
test("safety lists the owner's CLI decisions without counting them as incidents", () => {
  const response: SignalsResponse = {
    ...counts({ cli_decisions: 2 }),
    cli_decisions: [
      // Wed 2026-10-28 14:02 Lisbon (WET, UTC+0).
      decision("2026-10-28T14:02:00.000Z", "approval.decide", "alpha-31"),
      decision("2026-10-27T09:10:00.000Z", "card.close", "beta-16"),
      // Last week's: not this week's newest.
      decision("2026-10-20T08:00:00.000Z", "attention.answer", null),
    ],
  };
  const safety = signal(response, "safety");
  expect(safety.state).toBe("good");
  expect(safety.big).toBe(0);
  expect(safety.facts).toContain("CLI decisions 2");
  expect(safety.read).toBe(
    "Nothing lost, sent twice or unauthorized. Both machines are live. " +
      "Last from a terminal: decided an approval on alpha-31, Wed 14:02, zsh on pts/3.",
  );

  // Only last week's: nothing is named for this week.
  const old = signal({ ...response, cli_decisions: response.cli_decisions.slice(2) }, "safety");
  expect(old.read).not.toMatch(/terminal/);

  // A decision with no card and no terminal device still reads plainly.
  const bare = signal(
    {
      ...response,
      cli_decisions: [{ ...decision("2026-10-29T10:00:00.000Z", "project.add", null), tty: null }],
    },
    "safety",
  );
  expect(bare.read).toMatch(/Last from a terminal: added a project, Thu 10:00, zsh\.$/);

  // An empty parent name says nothing, so the terminal is named alone.
  const nameless = signal(
    {
      ...response,
      cli_decisions: [{ ...decision("2026-10-29T10:00:00.000Z", "project.add", null), parent: "" }],
    },
    "safety",
  );
  expect(nameless.read).toMatch(/Last from a terminal: added a project, Thu 10:00, pts\/3\.$/);
});

function readOfOne(txn: string, card: string | null) {
  const response = {
    ...counts({ cli_decisions: 1 }),
    cli_decisions: [decision("2026-10-28T14:02:00.000Z", txn, card)],
  };
  return signal(response, "safety").read;
}

test("the Safety sentence never shows an internal mutation name", () => {
  expect(readOfOne("card.close", "beta-16")).toMatch(
    /Last from a terminal: closed beta-16, Wed 14:02,/,
  );
  expect(readOfOne("card.close", null)).toMatch(/Last from a terminal: closed a card, Wed 14:02,/);
  // A name the board has no phrase for reads generically, never raw.
  const unknown = readOfOne("widget.frobnicate", "beta-16");
  expect(unknown).toMatch(/Last from a terminal: made a change on beta-16, Wed 14:02,/);
  expect(unknown).not.toMatch(/frobnicate|widget/);
  // A name that is only a property of every object is still unknown.
  expect(readOfOne("constructor", null)).toMatch(/Last from a terminal: made a change, Wed 14:02,/);
});

// The names come from the Rust sources: every mutation that can write events
// is `store.write(ctx, "<name>", ...)` or `write_as(ctx, "<name>", ...)`. A
// new one needs a plain phrase, or an owner CLI decision of that kind would
// read as "made a change".
test("every mutation the daemon records has a plain phrase", () => {
  const root = resolve(import.meta.dirname, "../../../../crates");
  const names = new Set<string>();
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = resolve(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.name.endsWith(".rs")) {
        const text = readFileSync(path, "utf8");
        for (const [, name] of text.matchAll(/\.write(?:_as)?\(\s*&?[\w.()]+\s*,\s*"([\w.]+)"/g)) {
          if (name) names.add(name);
        }
      }
    }
  };
  walk(root);
  // Guards the scan itself: it must find the mutations, not match nothing.
  expect(names.size).toBeGreaterThan(100);
  expect(names).toContain("approval.decide");
  const bare = [...names].filter((name) => mutationPhrase(name, null) === "made a change");
  expect(bare).toEqual([]);
  for (const name of names) expect(mutationPhrase(name, "alpha-31"), name).not.toContain(name);
});

test("a machine that is down puts Safety on watch without calling the system broken", () => {
  const response = { ...counts(), machines_live: 1 };
  const safety = signal(response, "safety");
  expect(safety.state).toBe("watch");
});

test("weeks are Lisbon weeks: the ISO number and the dates follow Lisbon, summer or winter", () => {
  const winter = week();
  expect(lisbonWeekLabel(winter.start)).toBe("W44");
  // Lisbon is an hour ahead of UTC in summer, so Monday starts at 23:00 UTC on Sunday.
  const summer = week({ start: "2026-06-14T23:00:00.000Z", end: "2026-06-21T23:00:00.000Z" });
  expect(lisbonDay(summer.start)).toMatchObject({ weekday: "Monday", date: "2026-06-15" });
  // The first days of January can belong to the last week of the year before.
  expect(lisbonWeekLabel("2027-01-01T12:00:00.000Z")).toBe("W53");
});

test("the quota block says it in a sentence, with the weekday in Lisbon time", () => {
  const quota = fixtureResponses("default", NOW)["/api/v1/quota"] as QuotaResponse;
  const block = quotaBlockOf(quota);
  expect(block).toMatchObject({
    tone: "ok",
    percentLeft: 27,
    paceMarker: 29,
    resetsShort: "Tue 14:00",
    resetsFull: "Tuesday 2026-11-03, 14:00 Lisbon time",
    verdict: "On pace to last until reset.",
    reserve: "reserve 8 points",
    burn: "burn 0.8x the even pace",
  });
  expect(block?.others).toEqual([
    { label: "Session", percentLeft: 64 },
    { label: "Fable week", percentLeft: 94 },
  ]);
});

test("the quota block warns when spending runs ahead or the feed is old, and copes with no weekly window", () => {
  const ahead = fixtureResponses("quota-amber", NOW)["/api/v1/quota"] as QuotaResponse;
  expect(quotaBlockOf(ahead)).toMatchObject({
    tone: "warn",
    verdict: "Spending faster than the clock; it may run out before reset.",
    reserve: "9 points short",
  });
  const red = fixtureResponses("quota-red", NOW)["/api/v1/quota"] as QuotaResponse;
  expect(quotaBlockOf(red)?.tone).toBe("bad");
  const old = fixtureResponses("quota-grey", NOW)["/api/v1/quota"] as QuotaResponse;
  expect(quotaBlockOf(old)).toMatchObject({ tone: "unk", verdict: "This reading is out of date." });
  expect(quotaBlockOf({ windows: [] })).toBeNull();
});

test("chart axes use round numbers", () => {
  expect(niceAxis(12)).toEqual({ top: 15, step: 5 });
  expect(niceAxis(0)).toEqual({ top: 1, step: 1 });
  expect(niceAxis(2.4)).toEqual({ top: 3, step: 1 });
  expect(niceAxis(0.6)).toEqual({ top: 0.6, step: 0.2 });
});

test("the fixture weeks start on Monday 00:00 Lisbon time, in winter and in summer", () => {
  expect(new Date(lisbonWeekStart(NOW)).toISOString()).toBe("2026-10-26T00:00:00.000Z");
  const summer = Date.parse("2026-06-20T12:00:00.000Z");
  expect(new Date(lisbonWeekStart(summer)).toISOString()).toBe("2026-06-14T23:00:00.000Z");
  const weeks = signalsFor(false, NOW).weeks;
  expect(weeks).toHaveLength(8);
  expect(weeks.at(-1)?.start).toBe("2026-10-26T00:00:00.000Z");
  expect(weeks.at(0)?.start).toBe("2026-09-07T00:00:00.000Z");
});

test("the fixture has exactly the fields the daemon's signals query returns", () => {
  const [first] = signalsFor(false, NOW).weeks;
  expect(new Set(Object.keys(first ?? {}))).toEqual(
    new Set([
      "start",
      "end",
      "landed",
      "reported",
      "merged_prs",
      "needed",
      "answered",
      "median_answer_minutes",
      "interventions",
      "reverts",
      "escaped",
      "rework",
      "lost_work",
      "duplicate_dispatches",
      "unauthorized",
      "cli_decisions",
    ]),
  );
});

// ---- The page ----

function stubMedia() {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
}

async function renderHealth(scenario: Scenario = "default") {
  const queryClient = createTestQueryClient();
  hydrateFromSnapshot(queryClient, fixtureSnapshot(scenario));
  const router = createAppRouter(queryClient, createMemoryHistory({ initialEntries: ["/health"] }));
  await router.load();
  render(
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  localStorage.clear();
  prefsStore.set(loadPrefs());
  connectionStore.set("live");
  // Only the clock is faked, so the fixture weeks end with W44.
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW);
  stubMedia();
  window.scrollTo = () => {};
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

test("Health shows four signals and the quota, with the numbers collapsed and no summary card", async () => {
  await renderHealth();
  const page = screen.getByTestId("health");

  expect(within(page).queryByLabelText("Verdict")).toBeNull();
  for (const title of ["Shipping", "Your time", "Quality", "Safety"]) {
    expect(within(page).getByRole("article", { name: new RegExp(`^${title}: `) })).toBeTruthy();
  }
  const quality = within(page).getByRole("article", { name: "Quality: Watch" });
  expect(quality.textContent).toContain("1 revert slipped in.");
  expect(within(page).getByRole("article", { name: "Safety: Good" })).toBeTruthy();

  const quota = within(page).getByLabelText("Claude quota");
  expect(quota.textContent).toContain(
    "27% of this week left. Resets Tue 14:00. On pace to last until reset.",
  );
  expect(quota.textContent).toContain("27% left");
  expect(quota.textContent).toContain("even pace would have 29% left");
  expect(quota.querySelector(".qmeter.big i")?.getAttribute("style")).toContain("width: 27%");
  expect(quota.querySelector(".qmeter.big u")?.getAttribute("style")).toContain("left: 29%");
  expect(quota.textContent).toContain("reserve 8 points");
  expect(quota.textContent).toContain("burn 0.8x the even pace");

  const toggle = within(page).getByRole("button", { name: /Show the numbers/ });
  expect(toggle.getAttribute("aria-expanded")).toBe("false");
  expect(page.querySelector(".charts")).toBeNull();
});

test("an install with no data says so on every signal card instead of Good", async () => {
  await renderHealth("empty");
  const page = screen.getByTestId("health");

  for (const title of ["Shipping", "Your time", "Quality", "Safety"]) {
    const card = within(page).getByRole("article", { name: `${title}: No data yet` });
    expect(card.getAttribute("data-state")).toBe("none");
    expect(card.querySelector(".state")?.textContent).toBe("No data yet");
    // The trend line is as muted as the badge, not the green of a good week.
    expect(card.querySelector(".spark")?.classList.contains("muted"), title).toBe(true);
  }
  expect(within(page).queryByText("Good")).toBeNull();
});

test("the sparkline is muted only on a card with no data, and the muted tone has rules", async () => {
  await renderHealth("default");
  const page = screen.getByTestId("health");
  for (const card of within(page).getAllByRole("article")) {
    const state = card.getAttribute("data-state");
    if (!card.querySelector(".spark")) continue;
    expect(card.querySelector(".spark")?.classList.contains("muted"), `${state} card`).toBe(
      state === "none",
    );
  }
  // jsdom lays nothing out, so this holds the rules: the line, the week
  // dots and the newest dot all leave the accent for the badge's tone.
  const css = readFileSync(resolve(import.meta.dirname, "health.css"), "utf8");
  for (const selector of [
    ".spark.muted .sp-now",
    ".spark.muted .sp-pt",
    ".spark.muted .sp-pt.cur",
  ]) {
    expect(css, selector).toContain(`${selector} {`);
  }
});

test("every tone and state the Health page can show has a style", () => {
  // jsdom lays nothing out, so this holds the rules: a class with no rule
  // falls back to the green of the first, which is how "Out of date" read.
  const css = readFileSync(resolve(import.meta.dirname, "health.css"), "utf8");
  // A selector alone before its rule, or one in a list of selectors.
  const has = (selector: string) =>
    css.split("\n").some((line) => line === `${selector} {` || line === `${selector},`);
  for (const state of ["good", "watch", "none"]) expect(has(`.state.${state}`), state).toBe(true);
  // The quota's tones: ok is the base rule; the others are classes.
  for (const tone of ["warn", "bad", "unk"]) expect(has(`.q-state.${tone}`), tone).toBe(true);
  expect(has(".q-state.unk i")).toBe(true);
});

test("the quota says Out of date in the muted tone, not green", async () => {
  await renderHealth("quota-grey");
  const quota = screen.getByLabelText("Claude quota");
  const state = quota.querySelector(".q-state");
  expect(state?.textContent).toBe("Out of date");
  expect(state?.className).toBe("q-state unk");
});

test("Show the numbers opens the weekly charts and the per-project table", async () => {
  await renderHealth();
  const page = screen.getByTestId("health");
  fireEvent.click(within(page).getByRole("button", { name: /Show the numbers/ }));

  expect(page.querySelectorAll(".chart svg")).toHaveLength(4);
  const accepted = within(page).getByRole("img", {
    name: /^Accepted outcomes per week, W37 to W44/,
  });
  expect(accepted.getAttribute("aria-label")).toContain("Latest week: 12 accepted");
  const table = page.querySelector(".tbl-wrap table");
  if (!(table instanceof HTMLElement)) throw new Error("no table");
  const rows = within(table).getAllByRole("row").slice(1);
  // Each figure says what it rests on under it.
  const cells = rows.map((row) =>
    within(row)
      .getAllByRole("cell")
      .map((cell) => [
        [...cell.childNodes]
          .filter((node) => node.nodeName !== "SMALL")
          .map((node) => node.textContent)
          .join(""),
        cell.querySelector("small")?.textContent,
      ]),
  );
  expect(cells).toEqual([
    [
      ["alpha", undefined],
      ["4", undefined],
      ["0.25", "1 of 4 outcomes"],
      ["0", "of 4 outcomes"],
      ["6.1 min", "3 answered"],
    ],
    [
      ["beta", undefined],
      ["5", undefined],
      ["0.40", "2 of 5 outcomes"],
      ["0", "of 5 outcomes"],
      ["4.4 min", "2 answered"],
    ],
    [
      ["gamma", undefined],
      ["3", undefined],
      ["0.33", "1 of 3 outcomes"],
      ["0", "of 3 outcomes"],
      ["3.2 min", "2 answered"],
    ],
  ]);
});

test("every chart under Show the numbers says how many outcomes, items or weeks it rests on", async () => {
  await renderHealth();
  const page = screen.getByTestId("health");
  fireEvent.click(within(page).getByRole("button", { name: /Show the numbers/ }));
  const notes = [...page.querySelectorAll(".charts .cpanel .n")].map((note) => note.textContent);
  expect(notes).toEqual([
    "12 accepted outcomes this week, 64 in the 8 weeks shown.",
    "This week 4 avoidable interventions over 12 accepted outcomes; 55 over 64 in 8 weeks.",
    "This week 0 escaped defects over 12 accepted outcomes; 7 over 64 in 8 weeks.",
    "This week's median is over 7 answered items; 77 answered in 8 weeks.",
  ]);
  const stacked = within(page).getByRole("img", {
    name: /^Avoidable interventions per accepted outcome, W37 to W44/,
  });
  expect(stacked.getAttribute("aria-label")).toContain("Latest week: 0.33 per outcome");
  expect(page.querySelectorAll(".legend span")).toHaveLength(3);
});

test("a time you were needed can be relabeled from the list, and Reset gives it back to the rules", async () => {
  const sent: { url: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((url: string, init?: RequestInit) => {
      if (init?.method === "POST") {
        sent.push({ url, body: JSON.parse(String(init.body)) as Record<string, unknown> });
        return Promise.resolve(
          Response.json({
            ok: true,
            replayed: false,
            events: [],
            receipt: {
              recorded_at: "2026-11-01T18:00:00.000Z",
              outbox_id: null,
              approval_id: null,
            },
            data: {},
          }),
        );
      }
      return Promise.reject(new Error(`no read of ${url}`));
    }),
  );
  await renderHealth();
  const page = screen.getByTestId("health");
  fireEvent.click(within(page).getByRole("button", { name: /Show the numbers/ }));
  const list = within(page).getByRole("region", { name: "Times you were needed" });
  expect(within(list).getAllByRole("listitem")).toHaveLength(8);
  const select = within(list).getByLabelText(
    "Why you were needed: Use the existing date helper, not a new one",
  );
  expect(select.textContent).toContain("Correction");
  await choose(select, "Planned");
  await vi.waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]?.url).toBe("/api/v1/interventions/01JNOTE0000000000000000B18/label");
  expect(sent[0]?.body).toMatchObject({ label: "planned" });
  expect(String(sent[0]?.body.idempotency_key)).toMatch(/^[0-9A-Z]{26}$/);
});

test("the per-project table follows the view knob", async () => {
  updatePrefs({ view: "focus", focusProject: "beta" });
  await renderHealth();
  const page = screen.getByTestId("health");
  fireEvent.click(within(page).getByRole("button", { name: /Show the numbers/ }));
  const rows = within(page.querySelector(".tbl-wrap") as HTMLElement)
    .getAllByRole("row")
    .slice(1);
  expect(rows).toHaveLength(1);
  expect(rows[0]?.textContent).toContain("beta");
});

test("an incident puts Safety on watch, and Health never compares with Firstmate or names an account", async () => {
  await renderHealth("health-bad");
  const page = screen.getByTestId("health");
  expect(within(page).getByRole("article", { name: "Safety: Watch" })).toBeTruthy();
  fireEvent.click(within(page).getByRole("button", { name: /Show the numbers/ }));
  expect(page.textContent).not.toMatch(/firstmate|baseline|compared with|@|account|email/i);
});

test("a reply with no first event time at all reads as a new install too", () => {
  // The contract does not require the field, so a client built from it can
  // leave it out.
  const response: Partial<SignalsResponse> = counts(NOTHING, NOTHING);
  delete response.first_event_at;
  const states = signalsOf(response as SignalsResponse, done).map((each) => each.state);
  expect(states).toEqual(["none", "none", "none", "none"]);
});
