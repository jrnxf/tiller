import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { expect, test } from "vitest";

// The first load carries the board and nothing else. These keep the heavy
// parts out of the main chunk (a build warns above 500 kB), so the editor is
// reached only through a dynamic import and the routes split on their own.
const read = (file: string) => readFileSync(resolve(import.meta.dirname, file), "utf8");

test("the ask dialog reaches the editor only through a dynamic import", () => {
  const dialog = read("components/shell/ask-dialog.tsx");
  expect(dialog).toMatch(/import\("\.\/ask-editor"\)/);
  expect(dialog).not.toMatch(/^import [^;]*from "\.\/ask-editor";/m);
  expect(dialog).not.toMatch(/from "@tiptap\/(?!core")/);
});

test("no module the board loads first imports the editor", () => {
  for (const file of ["components/shell/shell.tsx", "components/shell/ask-mentions.ts"]) {
    expect(read(file)).not.toMatch(/from "\.\/ask-editor"/);
    expect(read(file)).not.toMatch(/from "@tiptap\/(?!core")/);
  }
});

test("the build splits each route into its own chunk", () => {
  expect(read("../vite.config.ts")).toMatch(/autoCodeSplitting:\s*true/);
});
