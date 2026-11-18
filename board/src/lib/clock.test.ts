import { expect, test } from "vitest";

import { formatLisbonNear } from "./clock";

test("a near time says only as much as it needs, in Lisbon time", () => {
  // 2026-07-02 10:00 UTC is 11:00 on Thursday 2 July in Lisbon (summer time).
  const now = Date.parse("2026-07-02T10:00:00.000Z");
  // 23:30 UTC on 1 July is already 2 July in Lisbon.
  expect(formatLisbonNear("2026-07-01T23:30:00.000Z", now)).toBe("00:30");
  expect(formatLisbonNear("2026-07-01T08:15:00.000Z", now)).toBe("Yesterday 09:15");
  expect(formatLisbonNear("2026-06-29T08:15:00.000Z", now)).toBe("Mon 09:15");
  expect(formatLisbonNear("2026-06-20T08:15:00.000Z", now)).toBe("Sat 20 Jun, 09:15");
  expect(formatLisbonNear(null, now)).toBeNull();
  expect(formatLisbonNear("not a date", now)).toBeNull();
});

test("yesterday is the calendar day before, across the change of clocks", () => {
  // Lisbon goes back an hour on Sunday 25 October 2026.
  const now = Date.parse("2026-10-26T00:30:00.000Z");
  expect(formatLisbonNear("2026-10-25T00:10:00.000Z", now)).toBe("Yesterday 01:10");
  expect(formatLisbonNear("2026-10-24T22:50:00.000Z", now)).toBe("Sat 23:50");
});
