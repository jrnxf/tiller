import type { Project } from "./api-types";
import { projectKey } from "./peers";
import { createStore } from "./store";

export type ThemePreference = "system" | "light" | "dark";
export type Theme = "light" | "dark";
export type Density = "comfortable" | "compact";
export type ViewKnob = "all" | "focus";

export interface Prefs {
  theme: ThemePreference;
  density: Density;
  view: ViewKnob;
  hiddenProjects: string[];
  focusProject: string | null;
}

export const THEME_KEY = "tiller-theme";
export const DENSITY_KEY = "tiller-density";
export const VIEW_KEY = "tiller-view";

// Keep in step with the inline script in index.html, which paints these before
// any CSS loads (tests check both).
export const THEME_BACKGROUND: Record<Theme, string> = {
  light: "#f8f8f8",
  dark: "#0f1011",
};

const DEFAULTS: Prefs = {
  theme: "system",
  density: "comfortable",
  view: "all",
  hiddenProjects: [],
  focusProject: null,
};

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private mode or blocked storage: the preference lasts for this tab.
  }
}

function isTheme(value: unknown): value is ThemePreference {
  return value === "system" || value === "light" || value === "dark";
}

function isDensity(value: unknown): value is Density {
  return value === "comfortable" || value === "compact";
}

export function loadPrefs(): Prefs {
  const prefs = { ...DEFAULTS };
  const theme = read(THEME_KEY);
  if (isTheme(theme)) prefs.theme = theme;
  const density = read(DENSITY_KEY);
  if (isDensity(density)) prefs.density = density;
  const view = read(VIEW_KEY);
  if (view) {
    try {
      const parsed = JSON.parse(view) as Partial<Prefs>;
      if (parsed.view === "all" || parsed.view === "focus") prefs.view = parsed.view;
      if (Array.isArray(parsed.hiddenProjects)) {
        prefs.hiddenProjects = parsed.hiddenProjects.filter((id) => typeof id === "string");
      }
      if (typeof parsed.focusProject === "string") prefs.focusProject = parsed.focusProject;
    } catch {
      // A damaged value falls back to the defaults.
    }
  }
  return prefs;
}

export const prefsStore = createStore<Prefs>(loadPrefs());

function persist(prefs: Prefs) {
  write(THEME_KEY, prefs.theme);
  write(DENSITY_KEY, prefs.density);
  write(
    VIEW_KEY,
    JSON.stringify({
      view: prefs.view,
      hiddenProjects: prefs.hiddenProjects,
      focusProject: prefs.focusProject,
    }),
  );
}

export function updatePrefs(change: Partial<Prefs>) {
  prefsStore.update((current) => {
    const next = { ...current, ...change };
    persist(next);
    return next;
  });
}

export function toggleProjectHidden(projectId: string) {
  const { hiddenProjects } = prefsStore.get();
  updatePrefs({
    hiddenProjects: hiddenProjects.includes(projectId)
      ? hiddenProjects.filter((id) => id !== projectId)
      : [...hiddenProjects, projectId],
  });
}

// "All projects" shows every project that is not toggled off. "Focus" shows
// the one focused project, or the first project when none was picked yet.
export function visibleProjects(prefs: Prefs, projects: Project[]): Project[] {
  if (prefs.view === "focus") {
    const focused =
      projects.find((project) => projectKey(project) === prefs.focusProject) ?? projects[0];
    return focused ? [focused] : [];
  }
  return projects.filter((project) => !prefs.hiddenProjects.includes(projectKey(project)));
}

export function resolveTheme(preference: ThemePreference, systemDark: boolean): Theme {
  if (preference === "system") return systemDark ? "dark" : "light";
  return preference;
}

export function applyAppearance(prefs: Prefs, systemDark: boolean, root: HTMLElement) {
  const theme = resolveTheme(prefs.theme, systemDark);
  root.dataset.theme = theme;
  root.style.colorScheme = theme;
  root.style.backgroundColor = THEME_BACKGROUND[theme];
  root.dataset.density = prefs.density;
}
