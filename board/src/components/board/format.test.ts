import { expect, test } from "vitest";

import { durationSeconds, formatAge, formatLisbonMoment, shortSha } from "./format";

test("ages read in the coarsest unit that is not zero", () => {
  expect(formatAge(7)).toBe("7 s");
  expect(formatAge(59)).toBe("59 s");
  expect(formatAge(60)).toBe("1 min");
  expect(formatAge(3599)).toBe("59 min");
  expect(formatAge(3600)).toBe("1 h");
  expect(formatAge(47 * 3600)).toBe("47 h");
  expect(formatAge(48 * 3600)).toBe("2 d");
});

test("times are Lisbon time, with the weekday once the day is not today", () => {
  // 2026-07-01 23:30 UTC is 00:30 on Thursday 2 July in Lisbon (summer time).
  const now = Date.parse("2026-07-02T10:00:00.000Z");
  expect(formatLisbonMoment("2026-07-01T23:30:05.000Z", now)).toBe("00:30:05");
  expect(formatLisbonMoment("2026-07-01T08:15:00.000Z", now)).toBe("Wed 1 Jul, 09:15");
  expect(formatLisbonMoment(null, now)).toBeNull();
  expect(formatLisbonMoment("not a date", now)).toBeNull();
});

test("a run that is still going lasts until now", () => {
  const now = Date.parse("2026-11-02T12:10:00.000Z");
  expect(durationSeconds("2026-11-02T12:00:00.000Z", null, now)).toBe(600);
  expect(durationSeconds("2026-11-02T12:00:00.000Z", "2026-11-02T12:05:00.000Z", now)).toBe(300);
  expect(durationSeconds("garbage", null, now)).toBeNull();
});

test("a commit shows its first seven characters", () => {
  expect(shortSha("65b9a53c0ffee1234567890abcdef1234567890a")).toBe("65b9a53");
  expect(shortSha(null)).toBe("");
});
