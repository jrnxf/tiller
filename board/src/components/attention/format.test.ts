import { expect, test } from "vitest";

import { parseEvidence } from "./format";

test("a failed landing's evidence shows no raw record of the approval", () => {
  const evidence = JSON.stringify({
    approval_id: "01JAPPR0000000000000000000",
    state: "failed",
    result: { error: "gh: Pull Request is not mergeable (HTTP 405)" },
  });
  expect(parseEvidence(evidence).lines).toEqual([]);
});

test("evidence with no approval keeps its state line", () => {
  const evidence = JSON.stringify({ state: "blocked", note: "the disk is full" });
  expect(parseEvidence(evidence).lines.map((line) => line.text)).toEqual([
    "state: blocked",
    "note: the disk is full",
  ]);
});
