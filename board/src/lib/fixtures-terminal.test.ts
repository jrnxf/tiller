import { expect, test } from "vitest";

import {
  fullFrame,
  historyText,
  promptFrame,
  screenText,
  tickFrame,
  type PaneSize,
} from "./fixtures-terminal";

// oxlint-disable-next-line no-control-regex
const plain = (text: string) => text.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "");

// What a frame writes, without its escape sequences.
function written(frame: string): string {
  const bytes = Uint8Array.from(atob(frame), (char) => char.charCodeAt(0));
  return plain(new TextDecoder().decode(bytes));
}

const widths = (text: string, split: string) =>
  plain(text)
    .split(split)
    .map((row) => [...row].length);

// A phone's Fit (390 px wide) is 43 columns: the stand-in pane must draw for
// it as an agent would, so no row wraps. A wrapped row pushes the screen up
// (the top row shows cut) and leaves the end of the thinking line behind when
// a tick clears its one row (the spinner line shows twice).
const PHONE: PaneSize = { cols: 43, rows: 28 };

test("a full screen at a phone's size has no row wider than the pane", () => {
  const screen = written(fullFrame("", 5, PHONE));
  const rows = widths(screen, "\r\n");
  expect(rows.length).toBeGreaterThan(20);
  expect(rows.length).toBeLessThanOrEqual(PHONE.rows);
  expect(Math.max(...rows)).toBeLessThanOrEqual(PHONE.cols);
});

test("a tick at a phone's size writes the thinking line on one row", () => {
  for (const seconds of [5, 95, 1234]) {
    const row = written(tickFrame("", seconds, PHONE));
    expect([...row].length).toBeLessThanOrEqual(PHONE.cols);
  }
});

test("the prompt and the reads at a phone's size stay inside the pane", () => {
  const typed = written(promptFrame("x".repeat(80), PHONE));
  expect([...typed].length).toBeLessThanOrEqual(PHONE.cols);
  expect(Math.max(...widths(screenText("", 5, PHONE), "\n"))).toBeLessThanOrEqual(PHONE.cols);
  expect(Math.max(...widths(historyText("", 5, PHONE), "\n"))).toBeLessThanOrEqual(PHONE.cols);
});

test("a wide pane keeps its rows whole", () => {
  const screen = written(fullFrame("", 5, { cols: 100, rows: 28 }));
  expect(screen).toContain("(5s · esc to interrupt)");
  expect(screen).toContain("PASS importer::dry_run::prints_each_row");
});
