import { readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";

import { expect, test } from "vitest";

import { stepOf } from "../components/states/setup-screen";

// Before `tiller init`, every refusal names both setup steps: the owner hands
// the host to Tiller first, since `tiller init` refuses while the owner marker
// names another system. The words come from crates/core/src/lib.rs.
const root = resolve(import.meta.dirname, "../../..");
const core = readFileSync(join(root, "crates/core/src/lib.rs"), "utf8");

// A constant there is `concat!` of string literals and `flip_back!()`, the
// one text of the flip-back command.
function rustText(name: string): string {
  const flipBack = /macro_rules! flip_back \{\s*\(\) => \{\s*"([^"]*)"\s*\};\s*\}/.exec(core)?.[1];
  if (!flipBack) throw new Error("flip_back! is not in lib.rs");
  const found = new RegExp(`pub const ${name}: &str = concat!\\(([\\s\\S]*?)\\);\\n`).exec(
    core,
  )?.[1];
  if (!found) throw new Error(`${name} is not in lib.rs`);
  return [...found.matchAll(/flip_back!\(\)|"((?:[^"\\]|\\.)*)"/g)]
    .map(([part, text]) =>
      part === "flip_back!()" ? flipBack : (text ?? "").replaceAll("\\n", "\n"),
    )
    .join("");
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

test("the setup screen knows every step of the core's hint", () => {
  expect(rustText("INIT_HINT").split("\n").map(stepOf)).toEqual(["owner", "init"]);
});
