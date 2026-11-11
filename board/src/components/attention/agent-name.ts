import { useQuery } from "@tanstack/react-query";

import type { AgentRow } from "../../lib/api-types";
import { agentsQuery } from "../../lib/queries";
import { useMachineRows } from "../board/peer-state";

type Named = Pick<AgentRow, "herdr_name" | "role">;

// What the board calls an agent in a sentence: its herdr name, else its role,
// so the Inbox and View live name it the same way.
export function agentName(agent: Named | undefined): string {
  if (agent?.herdr_name) return agent.herdr_name;
  if (!agent) return "An agent";
  return agent.role === "orchestrator" ? "The orchestrator" : `The ${agent.role} worker`;
}

// The same name as a title: its herdr name, else its role.
export function agentTitle(agent: Named | undefined): string {
  return agent?.herdr_name ?? agent?.role ?? "Agent";
}

// The saved machine a host id names, if any. herdr streams and takes keys only
// for this machine's panes, so a saved machine's agent is answered in herdr on
// that machine.
export function useSavedMachine(hostId: string | null | undefined): string | undefined {
  const machines = useMachineRows();
  const machine = hostId ? machines.find((row) => row.id === hostId) : undefined;
  return machine?.kind === "remote" ? machine.label : undefined;
}

// The agent a session runs, from the agents list, and the saved machine it
// runs on, if any. `known` is false until the agents list has loaded, as only
// then does a missing agent mean this machine's.
export function useSessionAgent(sessionId: string): {
  agent: AgentRow | undefined;
  saved: string | undefined;
  known: boolean;
} {
  const data = useQuery(agentsQuery()).data;
  const agent = (data?.orchestrators ?? [])
    .flatMap((row) => [row, ...row.workers])
    .find((row) => row.session_id === sessionId);
  const saved = useSavedMachine(agent?.host_id);
  return { agent, saved, known: data !== undefined };
}
