import { useQuery } from "@tanstack/react-query";

import type { AgentRow, AgentsResponse } from "../../lib/api-types";
import { agentsQuery } from "../../lib/queries";
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

export function reportRole(report: Report): string {
  return ROLE[report.role] ?? "Worker";
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
