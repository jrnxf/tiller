import type { QueryClient } from "@tanstack/react-query";
import { Outlet, createRootRouteWithContext } from "@tanstack/react-router";

import { Shell } from "../components/shell/shell";
import {
  agentsQuery,
  attentionQuery,
  boardQuery,
  machinesQuery,
  projectsQuery,
  quotaQuery,
} from "../lib/queries";

export interface RouterContext {
  queryClient: QueryClient;
}

export interface RootSearch {
  // Machine filter: set by a click on a Machines row, shown as a header chip.
  host?: string;
}

export const Route = createRootRouteWithContext<RouterContext>()({
  validateSearch: (search): RootSearch => ({
    host: typeof search.host === "string" && search.host !== "" ? search.host : undefined,
  }),
  loader: async ({ context }) => {
    // Everything the shell draws is read before the first render, so no
    // pending state is ever visible (it is in the boot snapshot anyway).
    await Promise.all([
      context.queryClient.ensureQueryData(projectsQuery()),
      context.queryClient.ensureQueryData(agentsQuery()),
      context.queryClient.ensureQueryData(machinesQuery()),
      context.queryClient.ensureQueryData(quotaQuery()),
      context.queryClient.ensureQueryData(attentionQuery()),
      context.queryClient.ensureQueryData(boardQuery()),
    ]);
  },
  component: RootLayout,
});

function RootLayout() {
  const { host } = Route.useSearch();
  return (
    <Shell activeHost={host}>
      <Outlet />
    </Shell>
  );
}
