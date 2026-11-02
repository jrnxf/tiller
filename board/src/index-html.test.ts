import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { THEME_BACKGROUND } from "./lib/prefs";

// The inline script in index.html runs before any CSS, so the theme is right on
// the first paint. These run it the way the browser does.
const html = readFileSync(resolve(import.meta.dirname, "../index.html"), "utf8");
const script = /<script>([\s\S]*?)<\/script>/.exec(html)?.[1] ?? "";

function run({ stored, dark }: { stored: Record<string, string>; dark: boolean }) {
  localStorage.clear();
  for (const [key, value] of Object.entries(stored)) localStorage.setItem(key, value);
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: dark && query.includes("dark") }));
  new Function(script)();
  return document.documentElement;
}

beforeEach(() => {
  document.documentElement.removeAttribute("data-theme");
  document.documentElement.removeAttribute("style");
});

afterEach(() => vi.unstubAllGlobals());

test("the page ships exactly one inline script, in the head, before the app", () => {
  expect(script).not.toBe("");
  expect(html.indexOf(script)).toBeLessThan(html.indexOf('type="module"'));
});

test("the system theme decides when nothing is stored", () => {
  expect(run({ stored: {}, dark: true }).dataset.theme).toBe("dark");
  expect(run({ stored: {}, dark: false }).dataset.theme).toBe("light");
});

test("a stored choice beats the system theme", () => {
  expect(run({ stored: { "tiller-theme": "light" }, dark: true }).dataset.theme).toBe("light");
  expect(run({ stored: { "tiller-theme": "dark" }, dark: false }).dataset.theme).toBe("dark");
});

test("the background matches the CSS tokens, so there is no flash", () => {
  const dark = run({ stored: { "tiller-theme": "dark" }, dark: false });
  expect(dark.style.backgroundColor).toBe(hex(THEME_BACKGROUND.dark));
  const light = run({ stored: {}, dark: false });
  expect(light.style.backgroundColor).toBe(hex(THEME_BACKGROUND.light));
});

test("density comes from storage, defaulting to comfortable", () => {
  expect(run({ stored: { "tiller-density": "compact" }, dark: false }).dataset.density).toBe(
    "compact",
  );
  expect(run({ stored: {}, dark: false }).dataset.density).toBe("comfortable");
});

test("storage that throws does not stop the script", () => {
  vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("blocked");
  });
  expect(run({ stored: {}, dark: true }).dataset.theme).toBe("dark");
  vi.restoreAllMocks();
});

function hex(value: string): string {
  // jsdom normalizes colors to rgb().
  const n = Number.parseInt(value.slice(1), 16);
  return `rgb(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255})`;
}
