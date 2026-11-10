import { expect, test } from "vitest";

import { lisbonDate, lisbonInstant } from "./lisbon-input";

test("a winter Lisbon time is the same UTC time", () => {
  expect(lisbonInstant("2026-11-12", "18:00")).toBe("2026-11-12T18:00:00.000Z");
});

test("a summer Lisbon time is an hour behind in UTC", () => {
  expect(lisbonInstant("2026-07-15", "09:30")).toBe("2026-07-15T08:30:00.000Z");
});

test("a pair that is not a date and a time gives nothing", () => {
  expect(lisbonInstant("", "18:00")).toBeNull();
  expect(lisbonInstant("2026-11-12", "6pm")).toBeNull();
});

test("the date is Lisbon's, not the browser's", () => {
  // 23:30 UTC on 14 July is 00:30 on 15 July in Lisbon.
  const now = Date.parse("2026-07-14T23:30:00Z");
  expect(lisbonDate(now)).toBe("2026-07-15");
  expect(lisbonDate(now, 2)).toBe("2026-07-17");
});
