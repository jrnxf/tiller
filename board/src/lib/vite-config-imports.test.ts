import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { describe, expect, it } from "vitest";

// Vite loads vite.config.ts with its native loader, which cannot resolve a
// relative import without a file extension and prints a build warning. This
// walks every local file the config reaches and keeps those imports honest.
const RELATIVE = /(?:from|import)\s*\(?\s*["'](\.{1,2}\/[^"']*)["']/g;
const ROOT = resolve(import.meta.dirname, "../..");

function reachable(entry: string): Map<string, string[]> {
  const seen = new Map<string, string[]>();
  const queue = [entry];
  while (queue.length > 0) {
    const file = queue.pop() as string;
    if (seen.has(file)) continue;
    const specs = [...readFileSync(file, "utf8").matchAll(RELATIVE)].map((m) => m[1] as string);
    seen.set(file, specs);
    for (const spec of specs) {
      if (/\.(ts|tsx)$/.test(spec)) queue.push(resolve(dirname(file), spec));
    }
  }
  return seen;
}

describe("vite config imports", () => {
  it("gives every relative import in the config graph a file extension", () => {
    const graph = reachable(resolve(ROOT, "vite.config.ts"));
    expect(graph.size).toBeGreaterThan(3);
    const bad = [...graph].flatMap(([file, specs]) =>
      specs.filter((s) => !/\.[cm]?[jt]sx?$|\.json$/.test(s)).map((s) => `${file}: ${s}`),
    );
    expect(bad).toEqual([]);
  });
});
