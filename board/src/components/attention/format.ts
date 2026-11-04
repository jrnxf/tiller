import { formatLisbon, minutesSince } from "../../lib/clock";
import type { StaleInfo } from "../../lib/api-types";

// "4m", "3h 57m", "2d 5h": how long something has waited.
export function formatAge(iso: string | null): string {
  const minutes = minutesSince(iso);
  if (minutes === null) return "unknown";
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ${minutes % 60}m`;
  return `${Math.floor(hours / 24)}d ${hours % 24}h`;
}

export function formatWhen(iso: string | null): string {
  return formatLisbon(iso) ?? "unknown";
}

export function shortSha(sha: string): string {
  return sha.slice(0, 7);
}

interface EvidenceLine {
  text: string;
  href: string | null;
}

const URL_PATTERN = /^https?:\/\/\S+$/;

// Evidence is text, or a JSON list of pointers (state.md, Attention). Either
// way it shows as one line per fact, and a bare URL becomes a link.
function evidenceLines(evidence: string | null): EvidenceLine[] {
  if (!evidence || evidence.trim() === "") return [];
  let parts: string[];
  try {
    const parsed: unknown = JSON.parse(evidence);
    parts = Array.isArray(parsed) ? parsed.map((part) => String(part)) : [evidence];
  } catch {
    parts = evidence.split("\n");
  }
  return parts
    .map((part) => part.trim())
    .filter((part) => part !== "")
    .map((text) => ({ text, href: URL_PATTERN.test(text) ? text : null }));
}

export const PRIORITIES = ["Urgent", "High", "Medium", "Low", "Someday"] as const;

export const KIND_LABELS: Record<string, string> = {
  decision: "Decision",
  credential: "Credential",
  merge: "Merge approval",
  review: "Review",
  recovery: "Recovery",
  report: "Scout report",
};

// Evidence a recovery item carries as a JSON object: the pane's last output,
// and the facts around it.
export interface ParsedEvidence {
  paneTail: string | null;
  sessionId: string | null;
  lastOutputAt: string | null;
  lines: EvidenceLine[];
}

const POINTED = new Set(["pane_tail", "session_id", "last_observation_at"]);

export function parseEvidence(evidence: string | null): ParsedEvidence {
  const none: ParsedEvidence = { paneTail: null, sessionId: null, lastOutputAt: null, lines: [] };
  if (!evidence || evidence.trim() === "") return none;
  let parsed: unknown;
  try {
    parsed = JSON.parse(evidence);
  } catch {
    return { ...none, lines: evidenceLines(evidence) };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { ...none, lines: evidenceLines(evidence) };
  }
  const object = parsed as Record<string, unknown>;
  const text = (key: string) => (typeof object[key] === "string" ? (object[key] as string) : null);
  return {
    paneTail: text("pane_tail"),
    sessionId: text("session_id"),
    lastOutputAt: text("last_observation_at"),
    lines: Object.entries(object)
      .filter(([key, value]) => !POINTED.has(key) && value !== null && value !== "")
      .map(([key, value]) => ({
        text: `${key}: ${typeof value === "string" ? value : JSON.stringify(value)}`,
        href: null,
      })),
  };
}

// What the stale notice says, from the reason the daemon gives.
export function staleText(stale: StaleInfo | null | undefined): { head: string; sub: string } {
  const at = stale?.at ? ` ${formatWhen(stale.at)}` : "";
  const subject = stale?.subject ?? "";
  switch (stale?.reason) {
    case "project_deleted":
      return {
        head: `Project ${subject} was deleted${at}.`,
        sub: "Whatever it was asking about is gone. Nobody is waiting on this answer.",
      };
    case "project_retired":
      return {
        head: `Project ${subject} was retired${at}.`,
        sub: "Nobody is waiting on this answer.",
      };
    case "session_ended":
      return {
        head: `The session this asks about has ended${at}.`,
        sub: "Nobody is waiting on this answer.",
      };
    case "run_ended":
      return {
        head: `The run that asked this has ended${at}.`,
        sub: "Nobody is waiting on this answer.",
      };
    case "card_closed":
      return {
        head: `The card moved on${at}.`,
        sub: "Nobody is waiting on this answer.",
      };
    default:
      return {
        head: "This question no longer applies.",
        sub: "Nobody is waiting on this answer.",
      };
  }
}
