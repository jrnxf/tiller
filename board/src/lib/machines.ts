import type { Machine, MachineHealth, StaleReason } from "./api-types";
import { formatLisbon, minutesSince } from "./clock";
import { linkReason, peerMachines, refusalLabel, VIEW_ONLY_REASON } from "./peers";

type DotTone = "green" | "amber" | "red" | "grey";

const HEALTH_TONE: Record<MachineHealth, DotTone> = {
  ok: "green",
  degraded: "amber",
  stale: "amber",
  down: "red",
};

const STALE_REASON: Record<StaleReason, string> = {
  holder_down: "The terminal holder on this host does not answer",
  never_seen: "This host has never answered",
  peer_link_down: "The peer link has no frame from it",
  peer_refused: "The peer refused the link",
  peer_version: "The peer runs a version this host cannot talk to",
};

// The words the Machines row uses for a link that is not `live`.
const LINK_WORD = {
  lagging: "lagging",
  down: "unreachable",
  refused: "refused",
  never: "never connected",
} as const;

export type VersionMark = "older" | "newer" | "different";

// `0.1.0` or `0.1.0-pre.17`; a `-dev+<rev>` build has no order against either.
const RELEASE = /^(\d+)\.(\d+)\.(\d+)(?:-pre\.(\d+))?$/;

function releaseKey(version: string): number[] | null {
  const parts = RELEASE.exec(version);
  if (!parts) return null;
  // A release outranks every pre-release of the same number.
  const [, major, minor, patch, pre] = parts;
  return [Number(major), Number(minor), Number(patch), pre === undefined ? Infinity : Number(pre)];
}

// How a host's version stands against the board host's: `older` or `newer`
// when both are release or pre-release versions, `different` when they differ
// but cannot be ordered, null when they match or either is unknown.
export function versionMark(
  version: string | null | undefined,
  boardVersion: string | null | undefined,
): VersionMark | null {
  if (!version || !boardVersion || version === boardVersion) return null;
  const mine = releaseKey(version);
  const board = releaseKey(boardVersion);
  if (!mine || !board) return "different";
  for (const [index, part] of mine.entries()) {
    const other = board[index] ?? 0;
    if (part !== other) return part < other ? "older" : "newer";
  }
  return null;
}

export const VERSION_MARK_WORD: Record<VersionMark, string> = {
  older: "Older than this host",
  newer: "Newer than this host",
  different: "Differs from this host",
};

export interface MachineRowView {
  tone: DotTone;
  summary: string | null;
  stale: boolean;
  staleText: string | null;
  tooltip: string | null;
  viewOnly: boolean;
  // A paired peer: the link state and the sentence that explains it.
  peer: boolean;
  linkReason: string | null;
  // Set when the host runs another version than the board host.
  versionMark: VersionMark | null;
}

function peerStaleText(machine: Machine, link: "lagging" | "down" | "refused" | "never"): string {
  if (link === "refused") {
    const why = refusalLabel(machine.link_reason);
    return why ? `refused: ${why}` : "refused";
  }
  if (link === "never") return LINK_WORD.never;
  const when = formatLisbon(machine.last_seen_at);
  return when ? `${LINK_WORD[link]} since ${when}` : LINK_WORD[link];
}

export function machineRowView(
  machine: Machine,
  now?: number,
  boardVersion?: string | null,
): MachineRowView {
  const stale = machine.health === "stale";
  let summary: string | null = null;
  if (!stale && machine.running_agents !== null) {
    summary = `${machine.running_agents} running`;
    if (machine.free_slots !== null) summary += `, ${machine.free_slots} free`;
  }
  const peer = peerMachines([machine]).length > 0;
  const link = machine.link ?? null;
  let staleText: string | null = null;
  if (peer && link && link !== "live") {
    staleText = peerStaleText(machine, link);
  } else if (stale) {
    const minutes = minutesSince(machine.last_seen_at, now);
    staleText = minutes === null ? "never seen" : `last seen ${minutes} min ago`;
  }
  return {
    tone: link === "refused" ? "red" : HEALTH_TONE[machine.health],
    summary,
    stale: stale || staleText !== null,
    staleText,
    tooltip: machine.stale_reason ? STALE_REASON[machine.stale_reason] : null,
    // A `read` pairing shows the peer live with its controls off.
    viewOnly: peer ? machine.peer_scope === "read" : machine.view_only,
    peer,
    linkReason:
      peer && link
        ? link === "live"
          ? machine.peer_scope === "read"
            ? VIEW_ONLY_REASON
            : null
          : linkReason(machine.label, link, machine.last_seen_at, machine.link_reason)
        : null,
    versionMark: versionMark(machine.version, boardVersion),
  };
}
