import type { Machine, MachineHealth, StaleReason } from "./api-types";
import { minutesSince } from "./clock";

type DotTone = "green" | "amber" | "red" | "grey";

const HEALTH_TONE: Record<MachineHealth, DotTone> = {
  ok: "green",
  degraded: "amber",
  stale: "amber",
  down: "red",
};

const STALE_REASON: Record<StaleReason, string> = {
  herdr_link_down: "The saved herdr machine does not answer",
  host_asleep: "herdr reports the remote server is suspended",
  poll_failed: "The last answer could not be read",
  never_seen: "This host has never answered",
};

export interface MachineRowView {
  tone: DotTone;
  summary: string | null;
  stale: boolean;
  staleText: string | null;
  tooltip: string | null;
  viewOnly: boolean;
}

export function machineRowView(machine: Machine, now?: number): MachineRowView {
  const stale = machine.health === "stale";
  let summary: string | null = null;
  if (!stale && machine.running_agents !== null) {
    summary = `${machine.running_agents} running`;
    if (machine.free_slots !== null) summary += `, ${machine.free_slots} free`;
  }
  let staleText: string | null = null;
  if (stale) {
    const minutes = minutesSince(machine.last_seen_at, now);
    staleText = minutes === null ? "never seen" : `last seen ${minutes} min ago`;
  }
  return {
    tone: HEALTH_TONE[machine.health],
    summary,
    stale,
    staleText,
    tooltip: machine.stale_reason ? STALE_REASON[machine.stale_reason] : null,
    viewOnly: machine.view_only,
  };
}
