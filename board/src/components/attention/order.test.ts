import { expect, test } from "vitest";

import { mostUrgentFirst } from "./order";
import type { Waits } from "./order";

const wait = (name: string, priority: number, since: string, answered = false) =>
  ({ name, priority, since, answered }) as Waits & { name: string };

test("the most urgent card leads, then the longest wait, and an answered row is last", () => {
  const rows = [
    wait("new-medium", 2, "2026-11-16T09:00:00Z"),
    wait("answered-urgent", 0, "2026-11-16T07:00:00Z", true),
    wait("old-medium", 2, "2026-11-16T08:00:00Z"),
    wait("urgent", 0, "2026-11-16T09:30:00Z"),
    wait("high", 1, "2026-11-16T09:45:00Z"),
  ];
  rows.sort(mostUrgentFirst);
  expect(rows.map((row) => row.name)).toEqual([
    "urgent",
    "high",
    "old-medium",
    "new-medium",
    "answered-urgent",
  ]);
});
