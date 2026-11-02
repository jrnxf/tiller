import type { QueryClient } from "@tanstack/react-query";
import type { RouterHistory } from "@tanstack/react-router";
import { createRouter } from "@tanstack/react-router";

import { routeTree } from "../routeTree.gen";

export function createAppRouter(queryClient: QueryClient, history?: RouterHistory) {
  return createRouter({
    routeTree,
    history,
    context: { queryClient },
    // Preload on hover or focus; the Query cache owns freshness, so a preload
    // must not be cached a second time by the router.
    defaultPreload: "intent",
    defaultPreloadStaleTime: 0,
    // A local fetch must never show pending UI: the previous screen stays up
    // until the next one is ready.
    defaultPendingMs: Number.POSITIVE_INFINITY,
    defaultStructuralSharing: true,
  });
}

type AppRouter = ReturnType<typeof createAppRouter>;

declare module "@tanstack/react-router" {
  interface Register {
    router: AppRouter;
  }
}
