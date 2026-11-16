import type { AwayBrief, AwayMerge, Project } from "./api-types";
import { HOST_PROJECT, ownerOf } from "./peers";
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

// Away mode is one host's contract (authority.md section 6): the spend cap,
// the merge rule and the orders bind this host, and the board host cannot
// start it on a paired host. So the board names what a contract leaves out.
export interface Uncovered {
  host: string;
  projects: number;
}

// The paired hosts with projects of their own, each with its count. The
// built-in host project is not one an owner is away from.
export function uncoveredHosts(projects: Project[]): Uncovered[] {
  const counts = new Map<string, number>();
  for (const project of projects) {
    const host = ownerOf(project);
    if (host && project.id !== HOST_PROJECT) counts.set(host, (counts.get(host) ?? 0) + 1);
  }
  return [...counts].map(([host, count]) => ({ host, projects: count }));
}

// "peer's 6 projects", or "peer's 6 projects and spare's 1 project".
export function uncoveredNames(hosts: Uncovered[]): string {
  return hosts
    .map(({ host, projects }) => `${host}'s ${projects} project${projects === 1 ? "" : "s"}`)
    .join(" and ");
}

// "peer's own board", or "the own boards of peer and spare".
export function ownBoards(hosts: Uncovered[]): string {
  return hosts.length === 1
    ? `${hosts[0]?.host}'s own board`
    : `the own boards of ${hosts.map(({ host }) => host).join(" and ")}`;
}
