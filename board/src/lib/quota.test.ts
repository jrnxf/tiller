import { expect, test } from "vitest";

import { formatLisbonRounded } from "./clock";
import { SCENARIOS, fixtureSnapshot, quotaDot } from "./fixtures";
import { quotaPath } from "./queries";
import { formatSpan, quotaFooterView } from "./quota";
import type { QuotaResponse } from "./api-types";

const NOW = Date.parse("2026-11-02T12:00:00.000Z");

function quotaFor(scenario: (typeof SCENARIOS)[number]): QuotaResponse {
  return fixtureSnapshot(scenario, NOW).queries[quotaPath] as QuotaResponse;
}

test("the footer shows the dot the weekly window carries, per scenario", () => {
  expect(quotaFooterView(quotaFor("default"))?.dot).toBe("green");
  expect(quotaFooterView(quotaFor("quota-amber"))?.dot).toBe("amber");
  expect(quotaFooterView(quotaFor("quota-red"))?.dot).toBe("red");
  expect(quotaFooterView(quotaFor("quota-grey"))?.dot).toBe("grey");
});

test("the footer reads '27% week left' and the reset in Lisbon time with the weekday", () => {
  const view = quotaFooterView(quotaFor("default"));
  expect(view?.label).toBe("27% week left");
  expect(view?.resets).toBe("resets Tue 14:00");
  expect(view?.percentLeft).toBe(27);
});

test("the bar fills with what is left and the tick sits at the time still to come", () => {
  const view = quotaFooterView(quotaFor("default"), NOW);
  expect(view?.percentLeft).toBe(27);
  // 71% of the window is gone, so an even spend would have 29% left.
  expect(view?.paceMarker).toBe(29);
});

test("the live claude week: 81% left, 87% of the week to come, empty in 3d 7h, fable below", () => {
  const now = Date.parse("2026-11-04T11:06:00.000Z");
  const week = {
    label: "claude:seven_day",
    title: "week",
    kind: "weekly",
    percent_remaining: 81,
    resets_at: "2026-11-10T13:59:59.647Z",
    pace: "ahead",
    reserve: -8.4,
    burn_multiple: 1.67,
    elapsed_percent: 12.56,
    projected_exhaustion_at: "2026-11-07T18:28:31.447Z",
    fetched_at: "2026-11-04T11:05:00.000Z",
    stale: false,
    dot: "amber",
  } as const;
  const fable = {
    ...week,
    label: "claude:model:fable",
    title: "Fable week",
    kind: "model",
    percent_remaining: 94,
    pace: "behind",
    projected_exhaustion_at: "2026-11-18T05:39:50.947Z",
    dot: "amber",
  } as const;
  const view = quotaFooterView({ windows: [week, fable] }, now);
  expect(view?.percentLeft).toBe(81);
  expect(view?.paceMarker).toBeCloseTo(87.44, 2);
  expect(view?.runway).toBe("empty in 3d 7h");
  expect(view?.reason).toBe("Spending ahead of pace, empty in 3d 7h");
  expect(view?.resets).toBe("resets Tue 14:00");
  expect(view?.rows.map((row) => [row.name, row.percentLeft, row.toReset])).toEqual([
    ["week", 81, "6d 2h"],
    ["Fable", 94, "6d 2h"],
  ]);
});

test("no runway when the pace lasts to the reset, and no model rows without models", () => {
  const view = quotaFooterView(
    { windows: quotaFor("default").windows.filter((window) => window.kind === "weekly") },
    NOW,
  );
  expect(view?.runway).toBeNull();
  expect(view?.reason).toBe("On pace");
  expect(view?.rows).toEqual([]);
});

test("the dot colour is explained on the card", () => {
  expect(quotaFooterView(quotaFor("quota-amber"), NOW)?.reason).toBe("Spending ahead of pace");
  expect(quotaFooterView(quotaFor("quota-red"), NOW)?.reason).toMatch(/^Near the floor/);
  expect(quotaFooterView(quotaFor("quota-grey"), NOW)?.reason).toBe("Reading is out of date");
});

test("a stale reading shows no runway countdown", () => {
  const stale = quotaFor("quota-grey").windows.map((window) => ({
    ...window,
    projected_exhaustion_at: "2026-11-02T20:00:00.000Z",
  }));
  expect(quotaFooterView({ windows: stale }, NOW)?.runway).toBeNull();
});

test("spans read like quota-axi's: two units", () => {
  expect(formatSpan(285_752_000)).toBe("3d 7h");
  expect(formatSpan(5 * 3_600_000 + 12 * 60_000)).toBe("5h 12m");
  expect(formatSpan(42 * 60_000)).toBe("42m");
});

test("the reset label rounds to the minute instead of truncating", () => {
  expect(formatLisbonRounded("2026-11-10T13:59:59.647Z")).toBe("Tue 14:00");
  expect(formatLisbonRounded("2026-11-10T14:00:00.000Z")).toBe("Tue 14:00");
  expect(formatLisbonRounded("2026-11-10T14:00:29.000Z")).toBe("Tue 14:00");
});

test("the Lisbon text does not depend on the browser's time zone", () => {
  // 14:00 UTC is 14:00 in Lisbon in winter and 15:00 in summer (WEST).
  expect(formatLisbonRounded("2026-11-03T14:00:00.000Z")).toBe("Tue 14:00");
  expect(formatLisbonRounded("2026-07-07T14:00:00.000Z")).toBe("Tue 15:00");
  expect(formatLisbonRounded(null)).toBeNull();
});

test("a window without a weekly kind gives no footer reading", () => {
  const quota = quotaFor("default");
  expect(
    quotaFooterView({ windows: quota.windows.filter((window) => window.kind !== "weekly") }),
  ).toBeNull();
});

test("the dot table: only the floor is red, ahead of pace is amber, stale is grey", () => {
  const base = {
    percentRemaining: 50,
    resetsAt: "2026-11-03T14:00:00.000Z",
    projectedExhaustionAt: null,
    pace: "behind",
    stale: false,
  };
  expect(quotaDot(base)).toBe("green");
  expect(quotaDot({ ...base, pace: "ahead" })).toBe("amber");
  expect(quotaDot({ ...base, pace: "mystery" })).toBe("green");
  expect(quotaDot({ ...base, percentRemaining: 3 })).toBe("red");
  expect(quotaDot({ ...base, percentRemaining: 4 })).toBe("green");
  // Plenty left with a projected early exhaustion is not red: only nearly empty is.
  expect(
    quotaDot({ ...base, percentRemaining: 96, projectedExhaustionAt: "2026-11-03T10:00:00.000Z" }),
  ).toBe("green");
  expect(quotaDot({ ...base, stale: true, pace: "ahead" })).toBe("grey");
  expect(quotaDot({ ...base, percentRemaining: null })).toBe("grey");
});

test("no fixture or snapshot holds an email, an account id or a credential", () => {
  for (const scenario of SCENARIOS) {
    const text = JSON.stringify(fixtureSnapshot(scenario, NOW));
    expect(text, scenario).not.toMatch(/[\w.+-]+@[\w-]+\.[\w.]+/);
    expect(text, scenario).not.toMatch(/account|email|token|secret|password/i);
  }
});
