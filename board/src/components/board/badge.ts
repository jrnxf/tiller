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
  checks_next: "Checks next",
  checks_failed: "Checks failed",
  ready: "Approval next",
  merging: "Merging",
  deploying: "Deploying",
  errored: "Errored",
  working: "Working",
  between_runs: "Between runs",
  paused: "Paused",
  idle: "Idle",
  stale: "Stale",
  unknown: "Unknown",
  filed: "Filed",
  sent_back: "Sent back",
  landed: "Landed",
  reported: "Reported",
  answered: "Answered",
  completed: "Completed",
  cancelled: "Cancelled",
  failed: "Failed",
  abandoned: "Abandoned",
  superseded: "Superseded",
};

// The chip text for a card's `badge` (state.md, section 6). The board's verbs
// are Hold, Held and Release, so a parked card reads "Held" everywhere.
export function badgeText(row: BoardRow): string {
  if (row.badge === "parked") return row.parked_reason ? `Held: ${row.parked_reason}` : "Held";
  if (row.badge === "blocked") return row.blocked_by ? `Blocked by ${row.blocked_by}` : "Blocked";
  return LABELS[row.badge] ?? row.badge.replaceAll("_", " ");
}

// The state alone, for a card page whose banner already says why a card is held.
// A blocked card says what blocks it here, from the open dependencies the page
// read, so the page names the blockers once.
export function stateText(row: BoardRow, blockers: string[] = []): string {
  if (row.badge === "parked") return "Held";
  if (row.badge === "blocked") {
    const names = blockers.length > 0 ? blockers : row.blocked_by ? [row.blocked_by] : [];
    return names.length > 0 ? `Blocked by ${names.join(", ")}` : "Blocked";
  }
  return badgeText(row);
}

// How the chip is tinted: a person must act (warn), something looks wrong
// (yel for quiet or unknown, bad for errors), or it is just a fact (ghost).
// A card between the steps of its work (pend) keeps its place and says what
// comes next.
export type BadgeTone = "warn" | "yel" | "bad" | "ghost" | "ok" | "pend";

// The badges of a card between steps: no run is active and the next step has
// not started (state.md, section 6).
const PENDING = new Set(["between_runs", "checks_next", "ready", "sent_back"]);

export function isPending(row: BoardRow): boolean {
  return !row.needs_you && PENDING.has(row.badge);
}

export function badgeTone(row: BoardRow): BadgeTone {
  if (row.needs_you) return "warn";
  if (isPending(row)) return "pend";
  switch (row.badge) {
    case "errored":
    case "failed":
    case "checks_failed":
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
  // A held card keeps its column, so it is parked wherever it is.
  if (row.badge === "parked" || row.badge === "blocked") {
    return "parked";
  }
  return row.column;
}
