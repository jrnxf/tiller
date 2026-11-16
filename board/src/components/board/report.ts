import { useQuery } from "@tanstack/react-query";

import type { AgentRow, AgentsResponse, AttentionItem, TimelineEntry } from "../../lib/api-types";
import { agentsQuery, cardQuery, timelineQuery } from "../../lib/queries";
import type { BadgeTone } from "./badge";

// What a worker last said about its own work with `tiller status`: the state
// word and the line. The card's badge is the agent's terminal activity, which
// reads "Idle" for a worker that stopped because it is blocked, so the tile
// and the card page show this beside it.
type ReportState = "working" | "needs-decision" | "blocked" | "paused" | "done" | "failed";

export interface Report {
  state: ReportState;
  text: string;
  // The worker's short role: `build`, `scout`, `review` or `verify`.
  role: string;
}

// The most urgent first: with two workers on one card, the one that stopped
// is the one the owner must see.
const URGENCY: ReportState[] = ["blocked", "needs-decision", "failed", "paused", "done", "working"];

// A worker's word, never a verdict: "done" is a claim until the work lands.
const LABEL: Record<ReportState, string> = {
  working: "Working",
  "needs-decision": "Asks for a decision",
  blocked: "Reports blocked",
  paused: "Reports paused",
  done: "Reports done",
  failed: "Reports failed",
};

const TONE: Record<ReportState, BadgeTone> = {
  working: "ghost",
  "needs-decision": "warn",
  blocked: "warn",
  paused: "ghost",
  done: "ok",
  failed: "bad",
};

const ROLE: Record<string, string> = {
  build: "Builder",
  scout: "Scout",
  review: "Reviewer",
  verify: "Verifier",
};

export function reportLabel(report: Report): string {
  return LABEL[report.state];
}

export function reportTone(report: Report): BadgeTone {
  return TONE[report.state];
}

// A worker role as the owner reads it: "Builder" for `build`.
export function roleName(role: string): string {
  return ROLE[role] ?? "Worker";
}

export function reportRole(report: Report): string {
  return roleName(report.role);
}

// A run's status line is stored as `<state>: <line>` (state.md, `run.status`).
export function parseReport(line: string | null | undefined, role: string): Report | null {
  if (!line) return null;
  const cut = line.indexOf(": ");
  if (cut < 0) return null;
  const state = line.slice(0, cut);
  const text = line.slice(cut + 2).trim();
  if (!URGENCY.includes(state as ReportState) || text === "") return null;
  return { state: state as ReportState, text, role };
}

// The report of a card's live workers, the most urgent one when several
// reported. Null when no live worker of the card has said anything.
export function cardReport(roster: AgentsResponse | undefined, cardId: string): Report | null {
  if (!roster) return null;
  const workers: AgentRow[] = roster.orchestrators.flatMap((top) => top.workers);
  let best: Report | null = null;
  for (const worker of workers) {
    if (worker.card_id !== cardId) continue;
    const report = parseReport(worker.last_status_line, worker.role);
    if (!report) continue;
    if (!best || URGENCY.indexOf(report.state) < URGENCY.indexOf(best.state)) best = report;
  }
  return best;
}

// A report that says the work has stopped or claims an end. It takes the
// place of a badge that only says what the agent's terminal does.
export function overridesActivity(report: Report): boolean {
  return report.state !== "working";
}

// What an idle worker's row says on the Agents screen in place of "Idle":
// the worker's own word when it stopped for a reason ("Reports blocked: The
// staging bucket denies list access"). Null for an orchestrator, a worker
// that said nothing, or one whose last word was "working", which an idle
// terminal has outlived.
export function idleReport(agent: AgentRow): Report | null {
  if (agent.role === "orchestrator") return null;
  const report = parseReport(agent.last_status_line, agent.role);
  return report && overridesActivity(report) ? report : null;
}

// The badges that are the terminal activity of a run (state.md, section 6).
// Every other badge says more than the worker's word does.
const ACTIVITY_BADGES = new Set(["working", "idle", "stale", "unknown"]);

export function isActivityBadge(badge: string): boolean {
  return ACTIVITY_BADGES.has(badge);
}

// The agents read is the board's own, so this adds no request.
export function useReport(cardId: string): Report | null {
  return cardReport(useQuery(agentsQuery()).data, cardId);
}

// A worker that reported a stop ("blocked", "needs-decision") waits for an
// answer that reaches it through the orchestrator. Once the owner has answered
// a question on the card after that report, "Asks for a decision" in the
// needs-you colour is no longer true: the owner already did their part. This
// gives the time of that answer, or null while the report still stands.
//
// The report's own time is not on the agents read, so it is bounded by the
// card's timeline (newest first): the report is a worker line, and it is no
// newer than the newest worker line on the first page. With no worker line on
// a full page, it is older than every line there. Any worker line after the
// answer means the worker spoke again, so the report stands.
export function answeredSince(
  report: Report | null,
  attention: AttentionItem[] | undefined,
  timeline: { entries: TimelineEntry[]; next_before: number | null } | undefined,
): string | null {
  if (!report || !STOPS.has(report.state) || !attention || !timeline) return null;
  // An open question means the card still waits on the owner.
  if (attention.some((item) => item.answered_at === null && item.withdrawn_at === null)) {
    return null;
  }
  const answered = attention.reduce<string | null>(
    (newest, item) =>
      item.answered_at !== null &&
      (newest === null || Date.parse(item.answered_at) > Date.parse(newest))
        ? item.answered_at
        : newest,
    null,
  );
  if (!answered) return null;
  const worker = timeline.entries.find((entry) => entry.actor === "worker");
  const bound = worker
    ? worker.at
    : timeline.next_before !== null
      ? timeline.entries.at(-1)?.at
      : undefined;
  if (!bound) return null;
  return Date.parse(answered) > Date.parse(bound) ? answered : null;
}

// The stops that ask something of someone; a "failed", "paused" or "done"
// report is not a question an answer settles.
const STOPS = new Set<ReportState>(["blocked", "needs-decision"]);

// The words and tone for a stop report that the owner has answered since.
export const ANSWERED_LABEL = "Answered";
export const ANSWERED_TONE: BadgeTone = "ghost";

// The same, read for one card. The two reads run only for a card whose worker
// reported a stop and that waits on no one: a handful at most. A card page has
// both reads already, and the event stream marks both stale on an answer.
export function useAnsweredSince(
  cardId: string,
  owner: string | undefined,
  report: Report | null,
  needsYou: boolean,
): string | null {
  const enabled = Boolean(report && STOPS.has(report.state) && !needsYou);
  const card = useQuery({ ...cardQuery(cardId, owner), enabled });
  const timeline = useQuery({ ...timelineQuery(cardId, owner), enabled });
  if (!enabled) return null;
  return answeredSince(report, card.data?.attention, timeline.data);
}
