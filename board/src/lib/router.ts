import type { QueryClient } from "@tanstack/react-query";
import type { RouterHistory } from "@tanstack/react-router";
import { createRouter } from "@tanstack/react-router";

import { ScreenUnavailable } from "../components/states/screen-unavailable";
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
    // A screen that cannot read its data says so inside the shell; the
    // sidebar and header keep whatever they could read. The root route's
    // loader never fails (__root.tsx), so the root shows this only when the
    // shell itself cannot render, and then without the shell.
    defaultErrorComponent: ScreenUnavailable,
  });
}

type AppRouter = ReturnType<typeof createAppRouter>;

declare module "@tanstack/react-router" {
  interface Register {
    router: AppRouter;
  }
}
