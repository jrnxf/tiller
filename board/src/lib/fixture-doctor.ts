import type {
  DoctorCheck,
  DoctorGate,
  DoctorReport,
  MergeAppHealth,
  PollerHealth,
} from "./api-types.ts";
import type { Scenario } from "./fixtures.ts";

// The fixture board host's checks, as `GET /api/v1/doctor` runs them, and its
// pollers, as `GET /api/v1/health` lists them. The default host has a poller
// that keeps failing and no merge App; the federated one lacks a tool, which
// only gets a hint; the `merge-ready` one has its merge App ready. Run now on
// a failing poller makes its next poll succeed.

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

// A fixture project runs as detect while its host has no merge App. With
// the App ready (`merge-ready`, and the peer host-d), the ruleset check has
// moved some to enforced.
const gate = (project: string): DoctorGate => ({
  project,
  merge_gate: "detect",
  status: "warn",
  detail: "merge gate: detection only; merges are detected, not prevented",
});

const enforced = (project: string, now: number): DoctorGate => ({
  project,
  merge_gate: "enforced",
  status: "ok",
  last_changed_at: at(now, -26 * 60),
  detail: "the ruleset check holds",
});

const APP_ID = 1234567;

const MERGE_APP_READY: DoctorCheck = {
  name: "merge app",
  status: "ok",
  required: false,
  detail: `App ${APP_ID} with its key in /home/user/.tiller/forge-app`,
};

// The merge App key as `GET /api/v1/health` reads it on the fixture host.
export function mergeAppFor(scenario: Scenario): MergeAppHealth {
  return scenario === "merge-ready"
    ? { status: "ok", app_id: APP_ID, detail: MERGE_APP_READY.detail }
    : {
        status: "missing",
        app_id: null,
        detail:
          "Create the merge App and put its app-id and private-key.pem in /home/user/.tiller/forge-app (docs/plan/merge-app-setup.md). Until then every project runs as detect.",
      };
}

const ok = (name: string, detail: string, version?: string): DoctorCheck => ({
  name,
  status: "ok",
  required: true,
  ...(version ? { version } : {}),
  detail,
});

export function doctorFor(scenario: Scenario, now: number): DoctorReport {
  const missingTool = scenario === "federated";
  const ready = scenario === "merge-ready";
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
    ready
      ? MERGE_APP_READY
      : {
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
    merge_gates: ready
      ? [enforced("alpha", now), enforced("beta", now), gate("gamma")]
      : [gate("alpha"), gate("beta"), gate("gamma")],
  };
}

// A paired peer's checks, as it answers `GET /api/v1/doctor?host=<label>`
// over a live link. host-d has the merge App ready and its repos enforced,
// and runs another no-mistakes daemon than its CLI; host-f has no merge
// App, so its repo runs as detect. `undefined` for a peer whose link is not
// live.
export function peerDoctorFor(label: string, now: number): DoctorReport | undefined {
  if (label !== "host-d" && label !== "host-f") return undefined;
  const local = doctorFor("default", now);
  const checks = local.checks.map((check): DoctorCheck => {
    if (check.name === "merge app") {
      return label === "host-d"
        ? MERGE_APP_READY
        : {
            ...check,
            detail:
              "no merge App key at /home/user/.tiller/forge-app/private-key.pem, so every project runs as detect",
          };
    }
    if (check.name === "no-mistakes daemon" && label === "host-d") {
      return {
        ...check,
        status: "warn",
        detail:
          "daemon runs no-mistakes version v1.104.0 (pid 2210), CLI is v1.106.1; restart the daemon",
      };
    }
    return check;
  });
  return {
    checked_at: at(now, -0.1),
    ok: true,
    checks,
    merge_gates:
      label === "host-d" ? [enforced("gamma", now), enforced("fleet", now)] : [gate("atlas")],
  };
}
