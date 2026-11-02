import { createFileRoute } from "@tanstack/react-router";

import { AgentsView } from "../components/board/agents-view";
import { agentsQuery, runQuery } from "../lib/queries";

export interface AgentsSearch {
  // The selected agent's session; without it the first agent is selected.
  agent?: string;
}

export const Route = createFileRoute("/agents")({
  validateSearch: (search): AgentsSearch => ({
    agent: typeof search.agent === "string" && search.agent !== "" ? search.agent : undefined,
  }),
  loader: async ({ context }) => {
    // Every listed agent's run is read before the screen shows, so choosing a
    // row never waits for its events.
    const roster = await context.queryClient.fetchQuery(agentsQuery());
    const runs = roster.orchestrators
      .flatMap((orchestrator) => [orchestrator, ...orchestrator.workers])
      .flatMap((agent) => (agent.run_id ? [agent.run_id] : []));
    await Promise.all(runs.map((id) => context.queryClient.prefetchQuery(runQuery(id))));
  },
  component: AgentsRoute,
});

function AgentsRoute() {
  const { agent, host } = Route.useSearch();
  return <AgentsView host={host} selected={agent} />;
}
