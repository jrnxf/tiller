import { QueryCache, QueryClient } from "@tanstack/react-query";

import { isSignedOut, signedOutStore } from "./session";

// The SSE stream keeps the cache current, so nothing goes stale on a timer and
// nothing refetches on focus or reconnect behind the UI's back. A read refused
// as `signed_out` (the cookie was revoked) swaps the board for the signed-out
// screen.
export function createQueryClient(): QueryClient {
  return new QueryClient({
    queryCache: new QueryCache({
      onError: (error) => {
        if (isSignedOut(error)) signedOutStore.set(true);
      },
    }),
    defaultOptions: {
      queries: {
        staleTime: Number.POSITIVE_INFINITY,
        gcTime: 10 * 60_000,
        refetchOnWindowFocus: false,
        refetchOnReconnect: false,
        retry: 1,
      },
    },
  });
}
