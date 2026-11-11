import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";

import { expect, test } from "vitest";

// Before `tiller init`, every refusal names both setup steps: the owner hands
// the host to Tiller first, since `tiller init` refuses while the owner marker
// names another system. The words come from crates/core/src/lib.rs.
const root = resolve(import.meta.dirname, "../../..");
const core = readFileSync(join(root, "crates/core/src/lib.rs"), "utf8");

function rustText(name: string): string {
  const found = new RegExp(`pub const ${name}: &str =\\s*"([^"]*)";`).exec(core);
  if (!found?.[1]) throw new Error(`${name} is not in lib.rs`);
  return found[1];
}

function sources(dir: string): string[] {
  return readdirSync(join(root, dir), { recursive: true, encoding: "utf8" })
    .filter((path) => /\.(rs|ts|tsx)$/.test(path) && !path.includes("node_modules"))
    .map((path) => join(dir, path));
}

test("the fixture's refusal is the core's", () => {
  const fixture = readFileSync(join(root, "board/src/lib/fixture-mutations.ts"), "utf8");
  expect(fixture).toContain(`"state.db is not initialized; ${rustText("RUN_INIT")}"`);
});

test("no refusal names tiller init alone", () => {
  // Split so this file does not match itself.
  const bare = ["run", "tiller init"].join(" ");
  const found = [...sources("crates"), ...sources("board/src")].filter((path) =>
    readFileSync(join(root, path), "utf8").includes(bare),
  );
  expect(found).toEqual([]);
});
