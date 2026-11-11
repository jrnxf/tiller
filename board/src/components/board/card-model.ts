import type { CardApproval, CardPr, CardReview, TimelineEntry } from "../../lib/api-types";

// The forge and a reviewer may write a SHA in different cases; the grant
// matcher compares them in lower case, and so does the board.
export const sameSha = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

// The review a PR shows. Reviews belong to the card's runs, not to one PR, so
// a PR shows the newest review of its own head. An open PR with none there
// shows the card's newest review, as one of an older head. A merged or closed
// PR shows only a review of its own head. `reviews` is oldest first.
export function reviewFor(reviews: CardReview[], pr: CardPr): CardReview | undefined {
  for (let i = reviews.length - 1; i >= 0; i -= 1) {
    const covered = reviews[i]?.head_sha;
    if (covered && sameSha(covered, pr.head_sha)) return reviews[i];
  }
  return pr.state === "open" ? reviews.at(-1) : undefined;
}

// How an approval stands against its PR's head commit.
//   current  - bound to the head the PR has now
//   stale    - the head moved after it was bound (or the daemon marked it stale)
//   pending  - requested, waiting for a decision
//   spent    - it was used or turned down; the PR no longer depends on it
export type ApprovalStanding = "current" | "stale" | "pending" | "spent";

const OPEN_STATES = new Set(["accepted", "executing"]);

export function approvalStanding(approval: CardApproval, pr: CardPr | null): ApprovalStanding {
  if (approval.state === "stale") return "stale";
  if (approval.state === "requested") return "pending";
  if (!OPEN_STATES.has(approval.state)) return "spent";
  if (pr && pr.state === "open" && approval.head_sha && approval.head_sha !== pr.head_sha) {
    return "stale";
  }
  return "current";
}

// The approval that binds a PR: one that can still be acted on beats a stale or
// spent one, whatever order the list came in.
const RANK: Record<ApprovalStanding, number> = { current: 0, pending: 1, stale: 2, spent: 3 };

export function approvalFor(approvals: CardApproval[], pr: CardPr): CardApproval | null {
  let best: CardApproval | null = null;
  for (const approval of approvals) {
    if (approval.pr_id !== pr.id) continue;
    if (!best || RANK[approvalStanding(approval, pr)] < RANK[approvalStanding(best, pr)]) {
      best = approval;
    }
  }
  return best;
}

// Merge the first page and the pages loaded later. Every page is newest first and
// each later page is older than the one before, so the order holds; an event that
// appears twice (a refetched first page overlaps) is kept once.
export function mergeTimeline(...pages: TimelineEntry[][]): TimelineEntry[] {
  const seen = new Set<number>();
  const merged: TimelineEntry[] = [];
  for (const entry of pages.flat()) {
    if (seen.has(entry.seq)) continue;
    seen.add(entry.seq);
    merged.push(entry);
  }
  return merged;
}
