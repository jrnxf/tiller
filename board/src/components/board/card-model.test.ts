import { expect, test } from "vitest";

import type { CardApproval, CardPr, TimelineEntry } from "../../lib/api-types";
import { approvalFor, approvalStanding, mergeTimeline } from "./card-model";

const pr = (head: string, state: CardPr["state"] = "open"): CardPr => ({
  id: "pr1",
  number: 52,
  repo: "example/alpha",
  url: "https://github.com/example/alpha/pull/52",
  title: "t",
  state,
  head_sha: head,
  base: "main",
  checks: "passing",
  checks_summary: null,
  diff_summary: null,
});

const approval = (state: string, bound: string | null): CardApproval => ({
  id: "ap1",
  kind: "merge",
  state,
  pr_id: "pr1",
  head_sha: bound,
  decided_at: null,
  decided_by: null,
  newer_commits: [],
});

test("an approval bound to the head is current", () => {
  expect(approvalStanding(approval("accepted", "aaa"), pr("aaa"))).toBe("current");
});

test("a push after approval makes it stale even when the daemon has not marked it yet", () => {
  expect(approvalStanding(approval("accepted", "aaa"), pr("bbb"))).toBe("stale");
  expect(approvalStanding(approval("executing", "aaa"), pr("bbb"))).toBe("stale");
});

test("a state of stale is stale whatever the head says", () => {
  expect(approvalStanding(approval("stale", "aaa"), pr("aaa"))).toBe("stale");
});

test("a spent approval is not called stale after the PR has merged", () => {
  expect(approvalStanding(approval("succeeded", "aaa"), pr("bbb", "merged"))).toBe("spent");
  expect(approvalStanding(approval("rejected", "aaa"), pr("bbb"))).toBe("spent");
});

test("a request waiting for a decision is pending", () => {
  expect(approvalStanding(approval("requested", null), pr("aaa"))).toBe("pending");
});

test("a replaced stale approval does not hide the current one, in either order", () => {
  const stale = { ...approval("stale", "old"), id: "old" };
  const current = { ...approval("accepted", "aaa"), id: "new" };
  expect(approvalFor([stale, current], pr("aaa"))?.id).toBe("new");
  expect(approvalFor([current, stale], pr("aaa"))?.id).toBe("new");
});

test("the approval for a PR is the one bound to it", () => {
  const other = { ...approval("accepted", "aaa"), id: "ap2", pr_id: "pr9" };
  expect(approvalFor([other, approval("accepted", "aaa")], pr("aaa"))?.id).toBe("ap1");
  expect(approvalFor([other], pr("aaa"))).toBeNull();
});

const entry = (seq: number): TimelineEntry => ({
  seq,
  at: "x",
  actor: "system",
  text: `e${seq}`,
});

test("an earlier page joins the timeline without repeating an event the first page also holds", () => {
  const merged = mergeTimeline([entry(5), entry(4), entry(3)], [entry(3), entry(2), entry(1)]);
  expect(merged.map((e) => e.seq)).toEqual([5, 4, 3, 2, 1]);
});
