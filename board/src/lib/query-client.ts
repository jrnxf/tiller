import { QueryClient } from "@tanstack/react-query";

// The SSE stream keeps the cache current, so nothing goes stale on a timer and
// nothing refetches on focus or reconnect behind the UI's back.
export function createQueryClient(): QueryClient {
  return new QueryClient({
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
