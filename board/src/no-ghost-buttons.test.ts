import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

import { expect, test } from "vitest";

// The house rule: every button has a border or a fill. A borderless,
// fill-less "ghost" button reads as plain text, so the style is gone and no
// component may bring it back.
const src = import.meta.dirname;

const components = readdirSync(src, { recursive: true, encoding: "utf8" }).filter(
  (path) => path.endsWith(".tsx") && !path.endsWith(".test.tsx"),
);

test("no component draws a ghost button", () => {
  const ghosts = components.filter((path) =>
    /className=\{?[`"][^`"]*\bbtn\b[^`"]*\bghost\b/.test(readFileSync(join(src, path), "utf8")),
  );
  expect(ghosts).toEqual([]);
});

test("the stylesheet has no ghost button style", () => {
  expect(readFileSync(join(src, "styles.css"), "utf8")).not.toMatch(/\.btn\.ghost/);
});
