import type { HostConfig, ProjectSetup, SettingProblem, StartSettings } from "./api-types.ts";
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

// The refusal `POST /api/v1/config` sends: plain words, and for the stop hook
// switching rule the numbers behind it.
export interface ConfigRefusal {
  ok: false;
  message: string;
  status: number;
  code: string;
  current?: object;
}

const refuse = (message: string): ConfigRefusal => ({
  ok: false,
  message,
  status: 400,
  code: "usage",
});

// The other host keys, as this host's config.toml holds them
// (crates/core/src/host_config.rs checks each one).
const hostKeys: HostConfig = {
  herdr_terminal: "ghostty",
  stop_hook_mode: "observe",
  accept_peers: true,
  memory_remote: "git@example.com:team/memory.git",
  boundary: "personal",
  learning: {
    sample_every: 5,
    window_cards: 10,
    curator_model: "sonnet",
    curator_timeout_minutes: 20,
    evals_dir: `${HOME}/evals`,
  },
  config_error: null,
};

export function hostConfigFor(): HostConfig {
  return { ...hostKeys, learning: { ...hostKeys.learning } };
}

const DAY_MS = 24 * 60 * 60 * 1000;

// The Stop hook's last seven days, as `tiller intake stats` counts them: too
// few stops and a false-block rate over the limit, so block-once is refused.
// 1 false block in 29 would-block stops is 3.4%.
function switchingRule(now: number): ConfigRefusal {
  return {
    ok: false,
    status: 422,
    code: "refused",
    message:
      "block-once needs 200 orchestrator stops in 7 days with a false-block rate under 2%; there are 146 stops and a rate of 3.4%",
    current: {
      days: 7,
      stops: 146,
      would_block: 29,
      false_blocks: 1,
      false_block_rate: 1 / 29,
      min_stops: 200,
      max_false_block_rate: 0.02,
      rule_met: false,
      since: new Date(now - 7 * DAY_MS).toISOString(),
      until: new Date(now).toISOString(),
    },
  };
}

function whole(key: string, value: string, min: number, max: number): number | ConfigRefusal {
  const number = /^-?\d+$/.test(value) ? Number(value) : Number.NaN;
  if (Number.isNaN(number)) return refuse(`${key} is a whole number, not "${value}"`);
  if (number < min || number > max) return refuse(`${key} is a whole number from ${min} to ${max}`);
  return number;
}

type Saved = { ok: true; data: { key: string; value: string; changed: boolean } };

// A host key, checked as the daemon checks it.
function setHostKey(key: string, value: string, now: number): Saved | ConfigRefusal {
  const keys = hostKeys;
  const done = (changed: boolean): Saved => ({ ok: true, data: { key, value, changed } });
  switch (key) {
    case "herdr.terminal": {
      if (value === "") return refuse(`${key} names the terminal app herdr opens`);
      const changed = keys.herdr_terminal !== value;
      keys.herdr_terminal = value;
      return done(changed);
    }
    case "stop_hook_mode": {
      if (!["off", "observe", "block-once"].includes(value)) {
        return refuse(`${key} is off, observe or block-once, not "${value}"`);
      }
      if (value === "block-once") return switchingRule(now);
      const changed = keys.stop_hook_mode !== value;
      keys.stop_hook_mode = value;
      return done(changed);
    }
    case "federation.accept": {
      if (value !== "true" && value !== "false") {
        return refuse(`${key} is true or false (ADR 0003)`);
      }
      const accept = value === "true";
      const changed = keys.accept_peers !== accept;
      keys.accept_peers = accept;
      return done(changed);
    }
    case "memory.remote": {
      if (value !== "" && keys.boundary === "work") {
        return refuse(
          `${key} stays empty on a work host: its memory repo is never pushed off the machine`,
        );
      }
      if (value !== "" && !/^(ssh:\/\/|https:\/\/|\/|[\w.-]+@[\w.-]+:)/.test(value)) {
        return refuse(
          `${key} is an ssh or https git address such as git@example.com:team/memory.git, or a full path; not ${value}`,
        );
      }
      const changed = (keys.memory_remote ?? "") !== value;
      keys.memory_remote = value === "" ? null : value;
      return done(changed);
    }
    case "learning.sample_every": {
      const number = whole(key, value, 1, 1_000_000);
      if (typeof number !== "number") return number;
      const changed = keys.learning.sample_every !== number;
      keys.learning.sample_every = number;
      return done(changed);
    }
    case "learning.window_cards": {
      const number = whole(key, value, 1, 10_000);
      if (typeof number !== "number") return number;
      const changed = keys.learning.window_cards !== number;
      keys.learning.window_cards = number;
      return done(changed);
    }
    case "learning.curator_timeout_minutes": {
      const number = whole(key, value, 1, 1_440);
      if (typeof number !== "number") return number;
      const changed = keys.learning.curator_timeout_minutes !== number;
      keys.learning.curator_timeout_minutes = number;
      return done(changed);
    }
    case "learning.curator_model": {
      if (!/^[A-Za-z0-9._:[\]][A-Za-z0-9._:[\]-]{0,63}$/.test(value)) {
        return refuse(`${key} is a model name such as sonnet or opus, not "${value}"`);
      }
      const changed = keys.learning.curator_model !== value;
      keys.learning.curator_model = value;
      return done(changed);
    }
    default: {
      if (!value.startsWith("/") || value.split("/").includes("..")) {
        return refuse(`${key} must be an absolute path without .., not ${value}`);
      }
      const path = value.replace(/\/+$/, "");
      const changed = keys.learning.evals_dir !== path;
      keys.learning.evals_dir = path;
      return done(changed);
    }
  }
}

const HOST_KEYS = new Set([
  "herdr.terminal",
  "stop_hook_mode",
  "federation.accept",
  "memory.remote",
  "learning.sample_every",
  "learning.window_cards",
  "learning.curator_model",
  "learning.curator_timeout_minutes",
  "learning.evals_dir",
]);

// `POST /api/v1/config`: the refusal in plain words, or the saved value.
export function setConfigFixture(
  body: Record<string, unknown>,
  scenario: Scenario,
): Saved | ConfigRefusal {
  const key = String(body.key ?? "");
  const value = String(body.value ?? "").trim();
  if (HOST_KEYS.has(key)) return setHostKey(key, value, Date.now());
  const now = config(scenario);
  const project = key.startsWith("checkouts.") ? key.slice("checkouts.".length) : null;
  if (key !== "spawn.pool_root" && key !== "spawn.checkouts_root" && !project) {
    return refuse(`${key} is not a setting the board can change`);
  }
  if (!value.startsWith("/")) {
    return refuse(`${key} must be an absolute path without ..., not ${value}`);
  }
  if (value.split("/").includes("..")) {
    return refuse(`${key} must be an absolute path without ..., not ${value}`);
  }
  const path = value.replace(/\/+$/, "");
  if (project) {
    if (now.checkouts_root && !path.startsWith(`${now.checkouts_root}/`)) {
      return refuse(`${key} (${path}) is not under spawn.checkouts_root (${now.checkouts_root})`);
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
      return refuse(`${what} ${inside} overlaps ${key} (${path})`);
    }
  }
  const field = key === "spawn.pool_root" ? "pool_root" : "checkouts_root";
  const changed = now[field] !== path;
  now[field] = path;
  return { ok: true, data: { key, value: path, changed } };
}
