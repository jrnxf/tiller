import { useQuery } from "@tanstack/react-query";

import type { AgentRow } from "../../lib/api-types";
import { agentsQuery } from "../../lib/queries";

type Named = Pick<AgentRow, "name" | "role">;

// What the board calls an agent in a sentence: its name, else its role,
// so the Inbox and View live name it the same way.
export function agentName(agent: Named | undefined): string {
  if (agent?.name) return agent.name;
  if (!agent) return "An agent";
  return agent.role === "orchestrator" ? "The orchestrator" : `The ${agent.role} worker`;
}

// The same name as a title: its name, else its role.
export function agentTitle(agent: Named | undefined): string {
  return agent?.name || agent?.role || "Agent";
}

// The agent a session runs, from the agents list. `known` is false until the
// agents list has loaded, as only then does a missing agent mean an ended
// one. A list that failed to load counts as known.
export function useSessionAgent(sessionId: string): {
  agent: AgentRow | undefined;
  known: boolean;
} {
  const { data, isError } = useQuery(agentsQuery());
  const agent = (data?.orchestrators ?? [])
    .flatMap((row) => [row, ...row.workers])
    .find((row) => row.session_id === sessionId);
  return { agent, known: data !== undefined || isError };
}
