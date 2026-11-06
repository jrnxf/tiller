import type { AgentRow, BoardRow } from "../../lib/api-types";
import { clockStore } from "../../lib/clock";
import { secondsSince } from "./format";

// The dot kinds in the mocks: live (green, pulsing), waiting (orange), idle
// (grey), stale (yellow), unknown (hollow) and off (red).
export type DotKind = "live" | "waiting" | "idle" | "stale" | "unknown" | "off";

export interface ActivityView {
  dot: DotKind;
  label: string;
  // The state is a verdict that has gone quiet, so a person should look.
  old: boolean;
}

const DEFAULT_STALE_MINUTES = { orchestrator: 30, worker: 15 };

function thresholdMinutes(role: string): number {
  const thresholds = clockStore.get().thresholds;
  const key = role === "orchestrator" ? "orchestrator_stale_minutes" : "worker_stale_minutes";
  const value = thresholds[key];
  if (typeof value === "number" && value > 0) return value;
  return DEFAULT_STALE_MINUTES[role === "orchestrator" ? "orchestrator" : "worker"];
}

// A working agent with no output for this long reads as quiet: a long tool
// call, or a stuck one.
const QUIET_AFTER = 600;

// How long a working agent has gone without output once that passes
// QUIET_AFTER, counted from its last output or from when it started working,
// whichever is later, so a fresh prompt is not quiet. Null before then, and
// when the daemon reports no output.
export function quietFor(
  agent: Pick<AgentRow, "last_output_at" | "state_since">,
  now?: number,
): number | null {
  const output = secondsSince(agent.last_output_at ?? null, now);
  if (output === null) return null;
  const state = secondsSince(agent.state_since ?? null, now);
  const age = state === null ? output : Math.min(output, state);
  return age > QUIET_AFTER ? age : null;
}

// An observation older than the role's threshold reads as stale, as the
// `session_activity` view says; a prompt, `unknown` and `ended` do not age. A
// dated pause also silences staleness there, but the API does not carry the
// pause yet, so the daemon's snapshot is the only place that applies it.
export function agentActivity(
  agent: Pick<AgentRow, "activity" | "activity_observed_at" | "role">,
  now?: number,
): ActivityView {
  const age = secondsSince(agent.activity_observed_at, now);
  const stale = age !== null && age > thresholdMinutes(agent.role) * 60;
  switch (agent.activity) {
    case "working":
      return stale
        ? { dot: "stale", label: "Stale, was working", old: true }
        : { dot: "live", label: "Working", old: false };
    case "idle":
      return stale
        ? { dot: "stale", label: "Stale, was idle", old: true }
        : { dot: "idle", label: "Idle", old: false };
    case "waiting_permission":
      return { dot: "waiting", label: "Waiting on a permission prompt", old: stale };
    case "waiting_input":
      return { dot: "waiting", label: "Waiting for your answer", old: stale };
    case "errored":
      return stale
        ? { dot: "stale", label: "Stale, was errored", old: true }
        : { dot: "off", label: "Errored", old: false };
    case "ended":
      return { dot: "idle", label: "Ended", old: false };
    case "stale":
      return { dot: "stale", label: "Stale", old: true };
    default:
      return { dot: "unknown", label: "Unknown", old: false };
  }
}

// The live dot on a card comes from the badge the `card_column` view computed.
export function cardDot(row: BoardRow): DotKind | null {
  if (row.needs_you) return "waiting";
  if (row.column === "working" || row.column === "landing") {
    switch (row.badge) {
      case "working":
      case "checks":
      case "merging":
      case "deploying":
        return "live";
      case "idle":
      case "paused":
      case "ready":
      case "checks_next":
      case "between_runs":
      case "sent_back":
        return "idle";
      case "stale":
        return "stale";
      case "errored":
      case "checks_failed":
        return "off";
      default:
        return "unknown";
    }
  }
  return null;
}
