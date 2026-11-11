import { useQuery } from "@tanstack/react-query";

import type { AgentRow } from "../../lib/api-types";
import { agentsQuery } from "../../lib/queries";
import { useMachineRows } from "../board/peer-state";

// What the board calls an agent in a sentence: its herdr name, else its role,
// so the Inbox and View live name it the same way.
export function agentName(agent: Pick<AgentRow, "herdr_name" | "role"> | undefined): string {
  if (agent?.herdr_name) return agent.herdr_name;
  if (!agent) return "A worker";
  return agent.role === "orchestrator" ? "The orchestrator" : `The ${agent.role} worker`;
}

// The agent a session runs, from the agents list, and the saved machine it
// runs on, if any. herdr streams and takes keys only for this machine's panes,
// so a saved machine's agent is answered in herdr on that machine.
export function useSessionAgent(sessionId: string): {
  agent: AgentRow | undefined;
  saved: string | undefined;
} {
  const agents = useQuery(agentsQuery()).data?.orchestrators ?? [];
  const machines = useMachineRows();
  const agent = agents
    .flatMap((row) => [row, ...row.workers])
    .find((row) => row.session_id === sessionId);
  const machine = agent ? machines.find((row) => row.id === agent.host_id) : undefined;
  return { agent, saved: machine?.kind === "remote" ? machine.label : undefined };
}
