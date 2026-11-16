import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";

import type { AgentRow, AgentsResponse, BoardRow } from "../../lib/api-types";
import { agentsQuery } from "../../lib/queries";
import { isActivityBadge } from "./report";

// A run whose agent died stays active until its orchestrator relaunches it or
// ends it (recovery.md, R03), and the card's badge falls to `unknown`
// (state.md, section 6). "Unknown" hides what the daemon does know: the
// session ended and no agent works on the card. The board says so from the
// agents read, which lists every live session.

// Every live session of the roster: orchestrators, their workers, and the
// workers with no live orchestrator, which the read lists at the top level.
function liveAgents(roster: AgentsResponse): AgentRow[] {
  return roster.orchestrators.flatMap((top) => [top, ...top.workers]);
}

// Whether a card with an active run has no live agent. False until the agents
// read arrives, and false while the card's host does not answer: then nothing
// about it is fresh.
export function agentStopped(
  roster: AgentsResponse | undefined,
  row: Pick<BoardRow, "id" | "badge" | "closed_at">,
  reachable: boolean,
): boolean {
  if (!roster || !reachable) return false;
  // An activity badge is the badge of a card with an active run.
  if (row.closed_at !== null || !isActivityBadge(row.badge)) return false;
  return !liveAgents(roster).some((agent) => agent.card_id === row.id);
}

// The same for one run of a card page, so a card with a live reviewer and a
// dead builder names the dead one.
export function runStopped(
  roster: AgentsResponse | undefined,
  run: { id: string; ended_at: string | null },
  reachable: boolean,
): boolean {
  if (!roster || !reachable || run.ended_at !== null) return false;
  return !liveAgents(roster).some((agent) => agent.run_id === run.id);
}

export const STOPPED_LABEL = "Agent stopped";

export interface StoppedNote {
  title: string;
  detail: string;
  // The owner can start the project's orchestrator, which then gets the run.
  startable: boolean;
}

// What the card page says under the status: whose move is next, and when the
// owner hears of it (R03: the orchestrator first, the owner after 30 minutes).
export function stoppedNote(projectName: string, hasOrchestrator: boolean): StoppedNote {
  if (!hasOrchestrator) {
    return {
      title: "No agent works on this card",
      detail: `Its agent stopped before the work ended, and ${projectName} has no orchestrator to start it again. The work so far is kept. Start the orchestrator, and it takes the run.`,
      startable: true,
    };
  }
  return {
    title: "No agent works on this card",
    detail: `Its agent stopped before the work ended. The work so far is kept. The ${projectName} orchestrator starts the agent again or ends the run. If it does neither in 30 minutes, the question comes to your Inbox.`,
    startable: false,
  };
}

// The shell command that starts a new agent on the run, in its worktree.
// `tiller agent resume` refuses here: it needs a live session.
export function relaunchCommand(displayId: string, role: string): string {
  return `tiller agent relaunch ${displayId}-${role} --note "The agent stopped. Continue from the worktree."`;
}

// The board and the agents reads refresh one after the other, so a run that
// started a moment ago can be on the card before its session is on the
// roster. The verdict must hold this long before the board says it.
export const SETTLE_MS = 3000;

function useSettled(value: boolean, ms: number): boolean {
  const [settled, setSettled] = useState(false);
  useEffect(() => {
    if (!value) return;
    const timer = setTimeout(() => setSettled(true), ms);
    // The next time the verdict comes true, it must hold the full time again.
    return () => {
      clearTimeout(timer);
      setSettled(false);
    };
  }, [value, ms]);
  return value && settled;
}

// The agents read is the board's own, so these add no request.
export function useAgentStopped(
  row: Pick<BoardRow, "id" | "badge" | "closed_at">,
  reachable: boolean,
): boolean {
  const roster = useQuery(agentsQuery()).data;
  return useSettled(agentStopped(roster, row, reachable), SETTLE_MS);
}

export function useRunStopped(
  run: { id: string; ended_at: string | null },
  reachable: boolean,
): boolean {
  const roster = useQuery(agentsQuery()).data;
  return useSettled(runStopped(roster, run, reachable), SETTLE_MS);
}
