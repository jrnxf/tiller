import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { expect, test } from "vitest";

import { LIMITS } from "./limits";

// The store checks the caps in crates/core/src/limits.rs for every writer. The
// board must refuse exactly what they refuse, so its numbers are those.
const rust = readFileSync(
  resolve(import.meta.dirname, "../../../crates/core/src/limits.rs"),
  "utf8",
);

function rustCap(name: string): number {
  const found = new RegExp(`pub const ${name}: usize = ([0-9_]+);`).exec(rust);
  if (!found?.[1]) throw new Error(`${name} is not in limits.rs`);
  return Number(found[1].replaceAll("_", ""));
}

test("the board's caps are the core's", () => {
  expect(LIMITS).toEqual({
    cardTitle: rustCap("TITLE_MAX_CHARS"),
    cardBody: rustCap("BODY_MAX_CHARS"),
    comment: rustCap("COMMENT_MAX_CHARS"),
    answer: rustCap("ANSWER_MAX_CHARS"),
    steer: rustCap("STEER_MAX_CHARS"),
    askNote: rustCap("ASK_NOTE_MAX_CHARS"),
  });
});
