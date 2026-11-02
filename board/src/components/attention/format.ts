import { formatLisbon, minutesSince } from "../../lib/clock";

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

export interface EvidenceLine {
  text: string;
  href: string | null;
}

const URL_PATTERN = /^https?:\/\/\S+$/;

// Evidence is text, or a JSON list of pointers (state.md, Attention). Either
// way it shows as one line per fact, and a bare URL becomes a link.
export function evidenceLines(evidence: string | null): EvidenceLine[] {
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
