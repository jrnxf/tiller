import type { BoardRow } from "../../lib/api-types";
import type { StatusMark } from "./icons";

const LABELS: Record<string, string> = {
  decision: "Decision",
  credential: "Credential",
  merge: "Merge approval",
  review: "Review",
  recovery: "Recovery",
  report: "Report",
  permission_prompt: "Permission prompt",
  checks: "Checks running",
  merging: "Merging",
  deploying: "Deploying",
  errored: "Errored",
  working: "Working",
  paused: "Paused",
  idle: "Idle",
  stale: "Stale",
  unknown: "Unknown",
  filed: "Filed",
  landed: "Landed",
  reported: "Reported",
  answered: "Answered",
  completed: "Completed",
  cancelled: "Cancelled",
  failed: "Failed",
  abandoned: "Abandoned",
  superseded: "Superseded",
};

// The chip text for a card's `badge` (state.md, section 6).
export function badgeText(row: BoardRow): string {
  if (row.badge === "parked") return row.parked_reason ? `Parked: ${row.parked_reason}` : "Parked";
  if (row.badge === "blocked") return row.blocked_by ? `Blocked by ${row.blocked_by}` : "Blocked";
  return LABELS[row.badge] ?? row.badge.replaceAll("_", " ");
}

// How the chip is tinted: a person must act (warn), something looks wrong
// (yel for quiet or unknown, bad for errors), or it is just a fact (ghost).
export type BadgeTone = "warn" | "yel" | "bad" | "ghost" | "ok";

export function badgeTone(row: BoardRow): BadgeTone {
  if (row.column === "needs_you") return "warn";
  switch (row.badge) {
    case "errored":
    case "failed":
      return "bad";
    case "stale":
    case "unknown":
      return "yel";
    case "landed":
    case "reported":
    case "answered":
    case "completed":
      return "ok";
    default:
      return "ghost";
  }
}

export function statusMark(row: BoardRow): StatusMark {
  if (row.column === "done") {
    if (row.badge === "failed" || row.badge === "abandoned") return "fail";
    if (row.badge === "cancelled" || row.badge === "superseded") return "retired";
    return "done";
  }
  if (row.column === "queued" && (row.badge === "parked" || row.badge === "blocked")) {
    return "parked";
  }
  return row.column;
}
