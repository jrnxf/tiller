import { expect, test } from "vitest";

import { inlineDiff } from "./inline-diff";

test("a rewrite strikes the old words and tints the new ones in place", () => {
  expect(
    inlineDiff(
      "Stop new worker dispatch at 3% of the week.",
      "Stop new small-model worker dispatch at 3% and large-model at 8% of the week.",
    ),
  ).toEqual([
    { kind: "same", text: "Stop new" },
    { kind: "ins", text: "small-model" },
    { kind: "same", text: "worker dispatch at 3%" },
    { kind: "ins", text: "and large-model at 8%" },
    { kind: "same", text: "of the week." },
  ]);
});

test("a replaced word shows the old one struck before the new one", () => {
  expect(inlineDiff("Cap at 3 workers.", "Cap at 4 workers.")).toEqual([
    { kind: "same", text: "Cap at" },
    { kind: "del", text: "3" },
    { kind: "ins", text: "4" },
    { kind: "same", text: "workers." },
  ]);
});

test("every word of both texts appears exactly once", () => {
  const before = "one two three four five";
  const after = "zero two four six";
  const parts = inlineDiff(before, after);
  const of = (kinds: string[]) =>
    parts
      .filter((part) => kinds.includes(part.kind))
      .map((part) => part.text)
      .join(" ");
  expect(of(["same", "del"])).toBe(before);
  expect(of(["same", "ins"])).toBe(after);
});
