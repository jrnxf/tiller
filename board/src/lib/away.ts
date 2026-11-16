import type { AwayBrief, AwayBriefResponse, AwayMerge, Project } from "./api-types";
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

// `keys` are `readKey`s: a peer's brief is kept under its label.
export function markBriefRead(keys: string[]) {
  briefReadStore.update((current) => [...current, ...keys.filter((key) => !current.includes(key))]);
  try {
    localStorage.setItem(READ_KEY, JSON.stringify(briefReadStore.get().slice(-20)));
  } catch {
    // Without storage the card comes back on reload, which is harmless.
  }
}

// The id of the Inbox card that shows the away brief.
export const AWAY_BRIEF_ID = "away-brief";

// "Existing grants", "No merges" or "Own grant: merge, squash".
export function mergeLabel(merge: AwayMerge): string {
  if (merge === "none") return "None";
  if (merge === "grants") return "Existing grants";
  const actions = Array.isArray(merge.actions) ? merge.actions.join(", ") : "";
  return actions ? `Own grant: ${actions}` : "Own grant";
}

export const MERGE_ACTIONS = ["merge", "squash", "rebase"] as const;

// Away mode is each host's own contract (authority.md section 6). The board
// host forwards a start, an edit and an end to a peer paired with the `act`
// scope, and reads each such peer's contract over its link (UA-30). So one
// header status and one brief cover every host.
export interface HostContract {
  // The peer's label; undefined for this host.
  host?: string;
  brief: AwayBrief;
}

// This host's contract first, then each act-scope peer's, as the read lists
// them. A host that was never away has none.
export function contractsOf(response: AwayBriefResponse | undefined): HostContract[] {
  if (!response) return [];
  const peers = (response.hosts ?? []).flatMap((entry) =>
    entry.brief ? [{ host: entry.host, brief: entry.brief }] : [],
  );
  return [...(response.brief ? [{ brief: response.brief }] : []), ...peers];
}

// The contracts that still bind.
export function runningOf(response: AwayBriefResponse | undefined): HostContract[] {
  return contractsOf(response).filter((contract) => contract.brief.away);
}

// The earliest end among the running contracts: when the first host is back.
export function earliestUntil(running: HostContract[]): string | null {
  let earliest: string | null = null;
  for (const { brief } of running) {
    if (earliest === null || Date.parse(brief.until_at) < Date.parse(earliest)) {
      earliest = brief.until_at;
    }
  }
  return earliest;
}

export interface HostAwayRead {
  host: string;
  message: string;
}

// The peers whose contract the board could not read, each with a sentence
// that names the host. The daemon's own words for a down link are a
// mutation's ("nothing was sent"), so the board says it for a read.
export function unreadHosts(response: AwayBriefResponse | undefined): HostAwayRead[] {
  return (response?.hosts ?? []).flatMap(({ host, error }) => {
    if (!error) return [];
    const message =
      error.code === "peer_unreachable"
        ? `${host} is unreachable, so its away mode is not known`
        : `${host}: ${error.message.replace(/\.$/, "")}`;
    return [{ host, message }];
  });
}

// The key a read brief is kept under. A peer's is qualified by its label, so
// this host's briefs keep the keys they always had.
export function readKey(contract: HostContract): string {
  return contract.host
    ? `${contract.host}:${contract.brief.approval_id}`
    : contract.brief.approval_id;
}

// The briefs that belong in the Inbox: ended and not marked read here.
export function unreadBriefs(
  response: AwayBriefResponse | undefined,
  read: string[],
): HostContract[] {
  return contractsOf(response).filter(
    (contract) => !contract.brief.away && !read.includes(readKey(contract)),
  );
}

// The projects the away form offers: each host's built-in project is not one
// an owner is away from.
export function awayProjects<T extends Project>(projects: T[]): T[] {
  return projects.filter((project) => project.id !== HOST_PROJECT);
}

// The keys of the projects a contract covers, as `projectKey` keys a project:
// a peer's is qualified by its label.
export function contractKeys(contract: HostContract): string[] {
  return contract.brief.projects.map((id) => (contract.host ? `${contract.host}/${id}` : id));
}

// The name of a project a contract covers, looked up on its own host.
export function projectName(id: string, host: string | undefined, projects: Project[]): string {
  return projects.find((project) => project.id === id && ownerOf(project) === host)?.name ?? id;
}
