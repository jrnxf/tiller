import type { ProjectSetup, SettingProblem, StartSettings } from "./api-types.ts";
import type { Scenario } from "./fixtures.ts";

// The host-level start settings the stand-in server holds, by the daemon's
// rules for them (crates/agents/src/spawn/settings.rs): absolute, no `..`, a
// project's checkout under the checkouts folder. The `setup-needed` scenario
// starts with none of them set, as a fresh host does.
interface Held {
  pool_root: string | null;
  checkouts_root: string | null;
  checkouts: Map<string, string>;
}

const HOME = "/home/user";
const HOMES = `${HOME}/.tiller/orchestrators`;
const SCRATCH = `${HOME}/.tiller/scratch`;

let held: Held | undefined;

function config(scenario: Scenario): Held {
  held ??=
    scenario === "setup-needed"
      ? { pool_root: null, checkouts_root: null, checkouts: new Map() }
      : {
          pool_root: `${HOME}/pool`,
          checkouts_root: `${HOME}/src`,
          checkouts: new Map(),
        };
  return held;
}

const missing = (key: string): SettingProblem => ({
  key,
  kind: "missing",
  problem: `${key} is not set; tiller orchestrator start needs it`,
});

function hostProblems(now: Held): SettingProblem[] {
  return [
    ...(now.pool_root ? [] : [missing("spawn.pool_root")]),
    ...(now.checkouts_root ? [] : [missing("spawn.checkouts_root")]),
  ];
}

export function startSettingsFor(scenario: Scenario): StartSettings {
  const now = config(scenario);
  return {
    pool_root: now.pool_root,
    checkouts_root: now.checkouts_root,
    orchestrator_home: HOMES,
    scratch_root: SCRATCH,
    problems: hostProblems(now),
    config_error: null,
  };
}

export function projectSetupFor(project: string, scenario: Scenario): ProjectSetup {
  return {
    project,
    repo: `example/${project}`,
    merge_gate: "detect",
    merge_gate_changed_at: null,
    checkout: config(scenario).checkouts.get(project) ?? null,
    config_error: null,
  };
}

// Every setting a start of this project would refuse, as the start reports
// them in `current.settings`.
export function startProblems(project: string, scenario: Scenario): SettingProblem[] {
  const now = config(scenario);
  return [
    ...(now.checkouts.has(project) ? [] : [missing(`checkouts.${project}`)]),
    ...hostProblems(now),
  ];
}

// `POST /api/v1/config`: the refusal in plain words, or the saved value.
export function setConfigFixture(
  body: Record<string, unknown>,
  scenario: Scenario,
):
  | { ok: true; data: { key: string; value: string; changed: boolean } }
  | { ok: false; message: string } {
  const key = String(body.key ?? "");
  const value = String(body.value ?? "").trim();
  const now = config(scenario);
  const project = key.startsWith("checkouts.") ? key.slice("checkouts.".length) : null;
  if (key !== "spawn.pool_root" && key !== "spawn.checkouts_root" && !project) {
    return { ok: false, message: `${key} is not a setting the board can change` };
  }
  if (!value.startsWith("/")) {
    return { ok: false, message: `${key} must be an absolute path without ..., not ${value}` };
  }
  if (value.split("/").includes("..")) {
    return { ok: false, message: `${key} must be an absolute path without ..., not ${value}` };
  }
  const path = value.replace(/\/+$/, "");
  if (project) {
    if (now.checkouts_root && !path.startsWith(`${now.checkouts_root}/`)) {
      return {
        ok: false,
        message: `${key} (${path}) is not under spawn.checkouts_root (${now.checkouts_root})`,
      };
    }
    const changed = now.checkouts.get(project) !== path;
    now.checkouts.set(project, path);
    return { ok: true, data: { key, value: path, changed } };
  }
  for (const [what, inside] of [
    ["the orchestrator homes folder", HOMES],
    ["spawn.scratch_root", SCRATCH],
  ] as const) {
    if (inside.startsWith(path) || path.startsWith(inside)) {
      return { ok: false, message: `${what} ${inside} overlaps ${key} (${path})` };
    }
  }
  const field = key === "spawn.pool_root" ? "pool_root" : "checkouts_root";
  const changed = now[field] !== path;
  now[field] = path;
  return { ok: true, data: { key, value: path, changed } };
}
