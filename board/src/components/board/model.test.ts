import { expect, test } from "vitest";

import type { BoardResponse, BoardRow } from "../../lib/api-types";
import { badgeText, badgeTone, statusMark } from "./badge";
import { countsByFilter, onHost, openCount, parseFilter, passesFilter, tilesOf } from "./model";

function row(
  id: string,
  column: BoardRow["column"],
  badge: string,
  host = "h1",
  needsYou = false,
): BoardRow {
  return {
    id,
    display_id: id,
    project_id: "alpha",
    host_id: host,
    kind: "ship",
    title: id,
    column,
    badge,
    needs_you: needsYou,
    parked_until: null,
    parked_reason: null,
    blocked_by: null,
    closed_at: null,
    priority: 2,
    rev: 1,
  };
}

const board: BoardResponse = {
  rows: [
    row("a-1", "queued", "filed"),
    row("a-2", "working", "working"),
    row("a-3", "working", "decision", "h2", true),
    row("a-4", "done", "landed"),
  ],
  asked: [{ id: "ask-1", project_id: "alpha", session_id: null, received_at: "x", age_seconds: 5 }],
};

test("an ask is a tile in the Asked column and counts as active, never as needing you", () => {
  const tiles = tilesOf(board);
  const ask = tiles.find((tile) => tile.key === "ask-1");
  expect(ask?.column).toBe("asked");
  expect(ask?.row).toBeNull();
  expect(countsByFilter(tiles)).toEqual({ all: 5, active: 4, needs: 1 });
});

test("the filters keep the cards the tabs name", () => {
  const tiles = tilesOf(board);
  const keys = (filter: "all" | "active" | "needs") =>
    tiles.filter((tile) => passesFilter(tile, filter)).map((tile) => tile.key);
  expect(keys("all")).toHaveLength(5);
  expect(keys("active")).not.toContain("a-4");
  expect(keys("needs")).toEqual(["a-3"]);
});

test("the machine filter keeps cards that run there and drops asks", () => {
  const tiles = tilesOf(board);
  expect(tiles.filter((tile) => onHost(tile, "h2")).map((tile) => tile.key)).toEqual(["a-3"]);
  expect(tiles.filter((tile) => onHost(tile, undefined))).toHaveLength(5);
});

test("an unknown filter in the URL means All", () => {
  expect(parseFilter("needs")).toBe("needs");
  expect(parseFilter("active")).toBe("active");
  expect(parseFilter("bogus")).toBe("all");
  expect(parseFilter(undefined)).toBe("all");
});

test("the lane's open count leaves out done cards and asks", () => {
  expect(openCount(tilesOf(board))).toBe(3);
});

test("badges say what the card_column view computed, and parked and blocked say why", () => {
  expect(
    badgeText({ ...row("x", "queued", "parked"), parked_reason: "waiting for the release" }),
  ).toBe("Parked: waiting for the release");
  expect(badgeText({ ...row("x", "queued", "blocked"), blocked_by: "alpha-3" })).toBe(
    "Blocked by alpha-3",
  );
  expect(badgeText(row("x", "working", "permission_prompt", "h1", true))).toBe("Permission prompt");
  expect(badgeText(row("x", "working", "brand_new_state"))).toBe("brand new state");
});

test("the mark and tone follow the column, with failures marked as such", () => {
  expect(statusMark(row("x", "done", "failed"))).toBe("fail");
  expect(statusMark(row("x", "done", "superseded"))).toBe("retired");
  expect(statusMark(row("x", "done", "landed"))).toBe("done");
  expect(statusMark(row("x", "queued", "parked"))).toBe("parked");
  expect(statusMark(row("x", "queued", "filed"))).toBe("queued");
  expect(badgeTone(row("x", "landing", "merge", "h1", true))).toBe("warn");
  expect(badgeTone(row("x", "landing", "ready"))).toBe("pend");
  expect(badgeTone(row("x", "working", "between_runs"))).toBe("pend");
  expect(badgeTone(row("x", "working", "errored"))).toBe("bad");
  expect(badgeTone(row("x", "working", "stale"))).toBe("yel");
});
