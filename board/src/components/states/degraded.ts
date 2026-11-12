import type { AgentRow, Machine } from "../../lib/api-types";
import { agentActivity } from "../board/activity";
import type { ActivityView } from "../board/activity";
import { formatAge, formatLisbonMoment, secondsSince } from "../board/format";

// How a lane is doing, from the orchestrator that serves it and the machine
// that orchestrator runs on (protocol.md section 7.4, recovery.md R05 and R18).
//   disconnected: the machine's herdr link is down, so nothing there is fresh.
//   stale: the machine answers but the orchestrator has been silent too long.
export type LaneHealth =
  | { kind: "ok" }
  | { kind: "stale"; since: string | null; ago: string; at: string }
  | { kind: "disconnected"; since: string | null; ago: string; at: string };

export function machineUnreachable(machine: Machine | undefined): boolean {
  return machine?.health === "stale" || machine?.health === "down";
}

function agoText(iso: string | null, now?: number): string {
  const seconds = secondsSince(iso, now);
  return seconds === null ? "an unknown time" : formatAge(seconds);
}

export function laneHealth(
  orchestrator: AgentRow | undefined,
  machine: Machine | undefined,
  now?: number,
): LaneHealth {
  if (machineUnreachable(machine) && machine) {
    return {
      kind: "disconnected",
      since: formatLisbonMoment(machine.last_seen_at, now),
      ago: agoText(machine.last_seen_at, now),
      at: machine.last_seen_at ?? "",
    };
  }
  if (orchestrator && agentActivity(orchestrator, now).dot === "stale") {
    return {
      kind: "stale",
      since: formatLisbonMoment(orchestrator.activity_observed_at, now),
      ago: agoText(orchestrator.activity_observed_at, now),
      at: orchestrator.activity_observed_at ?? "",
    };
  }
  return { kind: "ok" };
}

// An agent on a host that does not answer has no live state: the last verdict
// is an old one, so it reads as unknown rather than working or idle.
export function hostAwareActivity(
  agent: Pick<AgentRow, "activity" | "activity_observed_at" | "role" | "pause">,
  machine: Machine | undefined,
  now?: number,
): ActivityView {
  if (machineUnreachable(machine)) {
    return { dot: "unknown", label: "Unknown, host not answering", old: true };
  }
  return agentActivity(agent, now);
}
