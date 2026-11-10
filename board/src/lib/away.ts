import type { AwayBrief, AwayMerge } from "./api-types";
import { createStore } from "./store";

// Away briefs the owner marked read in this browser. The brief itself is read
// from the records and stays the same; only the Inbox card goes away.
const READ_KEY = "tiller:away-read";

function load(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(READ_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
  } catch {
    return [];
  }
}

export const briefReadStore = createStore<string[]>(load());

export function markBriefRead(approvalId: string) {
  briefReadStore.update((current) =>
    current.includes(approvalId) ? current : [...current, approvalId],
  );
  try {
    localStorage.setItem(READ_KEY, JSON.stringify(briefReadStore.get().slice(-20)));
  } catch {
    // Without storage the card comes back on reload, which is harmless.
  }
}

// The id of the Inbox card that shows the away brief.
export const AWAY_BRIEF_ID = "away-brief";

// The brief belongs in the Inbox once the contract ended and is unread.
export function briefIsUnread(
  brief: AwayBrief | null | undefined,
  read: string[],
): brief is AwayBrief {
  return !!brief && !brief.away && !read.includes(brief.approval_id);
}

// "Existing grants", "No merges" or "Own grant: merge, squash".
export function mergeLabel(merge: AwayMerge): string {
  if (merge === "none") return "None";
  if (merge === "grants") return "Existing grants";
  const actions = Array.isArray(merge.actions) ? merge.actions.join(", ") : "";
  return actions ? `Own grant: ${actions}` : "Own grant";
}

export const MERGE_ACTIONS = ["merge", "squash", "rebase"] as const;
