import type { DoctorCheck, DoctorGate, DoctorReport, PollerHealth } from "./api-types.ts";
import type { Scenario } from "./fixtures.ts";

// The fixture board host's checks, as `GET /api/v1/doctor` runs them, and its
// pollers, as `GET /api/v1/health` lists them. The default host has a poller
// that keeps failing and no merge App; the federated one lacks a tool, which
// only gets a hint. Run now on a failing poller makes its next poll succeed.

const at = (now: number, minutes: number) => new Date(now + minutes * 60_000).toISOString();

const SOURCES = [
  "delivery",
  "herdr:local",
  "land",
  "machines",
  "merge-gate",
  "notify",
  "pages",
  "publish",
  "quota",
  "reconcile",
  "stop",
  "transcript",
] as const;

// The pollers Run now has polled since the server started, with when.
const ran = new Map<string, number>();

function failing(scenario: Scenario): string | null {
  return scenario === "default" ? "reconcile" : null;
}

export function pollersFor(scenario: Scenario, now: number): PollerHealth[] {
  const broken = failing(scenario);
  return SOURCES.map((source) => {
    const polled = ran.get(source);
    if (source === broken && polled === undefined) {
      return {
        source,
        last_attempt_at: at(now, -2),
        last_success_at: at(now, -47),
        consecutive_failures: 5,
        last_error: "gh api repos/example/alpha/pulls: HTTP 502 Bad Gateway",
      };
    }
    const read = polled === undefined ? at(now, 0) : new Date(polled).toISOString();
    return {
      source,
      last_attempt_at: read,
      last_success_at: read,
      consecutive_failures: 0,
      last_error: null,
    };
  });
}

// Run now: the poller polls at once, and in the stand-in server that poll
// succeeds. `undefined` for a source no poller holds.
export function runPollerFixture(source: string, now: number): PollerHealth | undefined {
  if (!(SOURCES as readonly string[]).includes(source)) return undefined;
  ran.set(source, now);
  return pollersFor("default", now).find((poller) => poller.source === source);
}

// Every fixture project runs as detect: the stand-in host has no merge App.
const gate = (project: string): DoctorGate => ({
  project,
  merge_gate: "detect",
  status: "warn",
  detail: "merge gate: detection only; merges are detected, not prevented",
});

const ok = (name: string, detail: string, version?: string): DoctorCheck => ({
  name,
  status: "ok",
  required: true,
  ...(version ? { version } : {}),
  detail,
});

export function doctorFor(scenario: Scenario, now: number): DoctorReport {
  const missingTool = scenario === "federated";
  const checks: DoctorCheck[] = [
    ok("git", "answers --version", "git version 2.47.1"),
    ok("treehouse", "answers --version", "treehouse 0.9.2"),
    ok("herdr", "reachable, 4 workspaces and 9 panes", "0.7.3"),
    ok("claude", "answers --version", "2.1.312 (Claude Code)"),
    ok("gh", "answers --version", "gh version 2.63.0 (2026-10-14)"),
    missingTool
      ? {
          name: "gh-axi",
          status: "warn",
          required: false,
          detail: "cannot run gh-axi: No such file or directory (os error 2)",
        }
      : { ...ok("gh-axi", "answers --version", "gh-axi 0.4.1"), required: false },
    { ...ok("quota-axi", "answers --version", "quota-axi 0.6.0"), required: false },
    { ...ok("no-mistakes", "answers --version", "no-mistakes version v1.106.1"), required: false },
    {
      ...ok(
        "no-mistakes daemon",
        "daemon running (pid 3121); same version as its CLI",
        "no-mistakes version v1.106.1",
      ),
      required: false,
    },
    ok("hook registration", "6 roles register 11 events; run scope only"),
    { ...ok("owner marker", "tiller owns this host"), code: "tiller" },
    {
      name: "firstmate",
      status: "ok",
      required: false,
      detail: "no other process is running",
      code: "not_running",
    },
    {
      name: "merge app",
      status: "warn",
      required: false,
      detail:
        "no merge App key at /home/user/.tiller/forge-app/private-key.pem, so every project runs as detect (docs/plan/merge-app-setup.md)",
    },
    {
      name: "config drift",
      status: "ok",
      required: false,
      detail: "config.toml matches its last event",
    },
  ];
  return {
    checked_at: new Date(now).toISOString(),
    ok: true,
    checks,
    merge_gates: [gate("alpha"), gate("beta"), gate("gamma")],
  };
}
