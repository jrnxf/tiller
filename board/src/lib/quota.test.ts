import { expect, test } from "vitest";

import { formatLisbon } from "./clock";
import { SCENARIOS, fixtureSnapshot, quotaDot } from "./fixtures";
import { quotaPath } from "./queries";
import { quotaFooterView } from "./quota";
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

test("the Lisbon text does not depend on the browser's time zone", () => {
  // 14:00 UTC is 14:00 in Lisbon in winter and 15:00 in summer (WEST).
  expect(formatLisbon("2026-11-03T14:00:00.000Z")).toBe("Tue 14:00");
  expect(formatLisbon("2026-07-07T14:00:00.000Z")).toBe("Tue 15:00");
  expect(formatLisbon(null)).toBeNull();
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
