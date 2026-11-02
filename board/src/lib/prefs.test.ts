import { beforeEach, expect, test } from "vitest";

import type { Project } from "./api-types";
import {
  DENSITY_KEY,
  THEME_KEY,
  VIEW_KEY,
  applyAppearance,
  loadPrefs,
  resolveTheme,
  visibleProjects,
} from "./prefs";
import type { Prefs } from "./prefs";

const projects: Project[] = ["alpha", "beta", "gamma"].map((id) => ({
  id,
  name: id,
  delivery: "direct-pr",
  capacity: 1,
  rev: 1,
}));

const base: Prefs = {
  theme: "system",
  density: "comfortable",
  view: "all",
  hiddenProjects: [],
  focusProject: null,
};

beforeEach(() => localStorage.clear());

test("all projects hides only the toggled-off ones", () => {
  expect(visibleProjects({ ...base, hiddenProjects: ["beta"] }, projects).map((p) => p.id)).toEqual(
    ["alpha", "gamma"],
  );
});

test("focus shows the focused project, or the first one before a pick", () => {
  expect(
    visibleProjects({ ...base, view: "focus", focusProject: "gamma" }, projects).map((p) => p.id),
  ).toEqual(["gamma"]);
  expect(visibleProjects({ ...base, view: "focus" }, projects).map((p) => p.id)).toEqual(["alpha"]);
  expect(
    visibleProjects({ ...base, view: "focus", focusProject: "gone" }, projects).map((p) => p.id),
  ).toEqual(["alpha"]);
  expect(visibleProjects({ ...base, view: "focus" }, [])).toEqual([]);
});

test("the system theme follows the OS and an explicit choice wins", () => {
  expect(resolveTheme("system", true)).toBe("dark");
  expect(resolveTheme("system", false)).toBe("light");
  expect(resolveTheme("light", true)).toBe("light");
  expect(resolveTheme("dark", false)).toBe("dark");
});

test("stored values load, and damaged ones fall back to the defaults", () => {
  localStorage.setItem(THEME_KEY, "dark");
  localStorage.setItem(DENSITY_KEY, "compact");
  localStorage.setItem(
    VIEW_KEY,
    JSON.stringify({ view: "focus", hiddenProjects: ["beta", 7], focusProject: "gamma" }),
  );
  expect(loadPrefs()).toEqual({
    theme: "dark",
    density: "compact",
    view: "focus",
    hiddenProjects: ["beta"],
    focusProject: "gamma",
  });

  localStorage.setItem(THEME_KEY, "purple");
  localStorage.setItem(DENSITY_KEY, "");
  localStorage.setItem(VIEW_KEY, "{nope");
  expect(loadPrefs()).toEqual(base);
});

test("applying appearance sets the theme, its background and the density on <html>", () => {
  const root = document.createElement("html");
  applyAppearance({ ...base, density: "compact" }, true, root);
  expect(root.dataset.theme).toBe("dark");
  expect(root.dataset.density).toBe("compact");
  expect(root.style.backgroundColor).not.toBe("");
});
